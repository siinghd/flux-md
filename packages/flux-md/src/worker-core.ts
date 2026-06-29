import type { FromWorker, ParserConfig, ToWorker } from "./types-core";

/** The slice of `FluxParser` the worker drives — narrowed to an interface so the
 *  message/readiness state machine is unit-testable with a fake parser, no WASM.
 *  (Same testability move as {@link FluxPool} taking an injected worker factory.)
 *
 *  `append`/`finalize` return the patch as a **JSON string**, not an object: the
 *  worker forwards it verbatim to the main thread (a string structuredClones far
 *  cheaper than an object graph) where it is `JSON.parse`d exactly once. */
export interface ParserLike {
  append(chunk: string): string;
  finalize(): string;
  free(): void;
  retainedBytes(): number;
}

/** Dependencies injected into {@link WorkerCore}, isolating it from the worker
 *  globals (`self`, `queueMicrotask`) and the WASM module so it can be tested. */
export interface WorkerCoreDeps {
  /** Create + configure a parser for a stream (prod: `new FluxParser()` + setX). */
  makeParser(config: ParserConfig | undefined): ParserLike;
  /** Post a message to the main thread (prod: `self.postMessage`). */
  post(msg: FromWorker): void;
  /** Current WASM heap size in bytes, reported on each patch (0 if unknown). */
  memBytes(): number;
  /** Defer a flush to a future microtask (prod: `queueMicrotask`). */
  schedule(fn: () => void): void;
}

/**
 * The worker's message reducer + WASM-readiness gate, extracted from the worker
 * shell so its trickiest invariant is testable without a real Worker or WASM.
 *
 * **The invariant:** WASM `init()` is async, and the client does NOT wait for
 * readiness before appending — so chunks can arrive first. A parser must never
 * be constructed before init resolves (`new FluxParser()` against an
 * uninitialized module throws `fluxparser_new of undefined` and silently drops
 * that chunk). So while `ready` is false, appends only accumulate in `pending`
 * (scheduleFlush is a no-op) and `finalize` is deferred; {@link markReady}
 * drains both — appends first (creating each parser + applying buffered text),
 * then any deferred finalize — once init has completed.
 */
export class WorkerCore {
  // One parser per stream id; WASM is loaded once and shared by all of them.
  private parsers = new Map<number, ParserLike>();
  private config = new Map<number, ParserConfig>();
  private pending = new Map<number, string>();
  private totalAppended = new Map<number, number>();
  private finalizePending = new Set<number>();
  // Per-stream generation, echoed on emitted patches so the client can drop a
  // patch produced before a reset (see FromWorker.epoch). A microtask flush
  // drains `pending` between messages, so the epoch at emit time is always the
  // epoch the buffered input was appended under.
  private epochs = new Map<number, number>();
  private flushScheduled = false;
  private ready = false;

  constructor(private deps: WorkerCoreDeps) {}

  /** Handle one message from the main thread (append/finalize/reset/dispose). */
  handle(msg: ToWorker): void {
    const id = msg.streamId;
    // Stash any per-stream config carried on the first message (FIFO guarantees
    // it arrives before the parser is created in flush/finalize).
    if ((msg.type === "append" || msg.type === "finalize") && msg.config) {
      this.config.set(id, msg.config);
    }
    // Track the stream generation carried on append/finalize/reset.
    if (msg.type !== "dispose" && msg.epoch !== undefined) {
      this.epochs.set(id, msg.epoch);
    }
    switch (msg.type) {
      case "append":
        this.pending.set(id, (this.pending.get(id) ?? "") + msg.chunk);
        this.scheduleFlush();
        break;
      case "finalize":
        // Before WASM is ready, defer: markReady() finalizes it after init (the
        // buffered input is drained first). Otherwise finalize now.
        if (!this.ready) this.finalizePending.add(id);
        else this.doFinalize(id);
        break;
      case "reset":
        // Free and recreate lazily on the next append — same stream id, **same
        // config** (kept). The client resets its local state synchronously.
        // free() is a WASM call: if the instance was poisoned by an earlier trap
        // it can throw, so guard it — a reset must never throw out of the loop.
        this.safeFree(id);
        this.parsers.delete(id);
        this.pending.delete(id);
        this.finalizePending.delete(id); // a reset cancels a not-yet-run early finalize
        this.totalAppended.delete(id);
        break;
      case "dispose":
        this.dispose(id);
        break;
    }
  }

  /** Called once WASM init resolves: open the gate and drain what was buffered. */
  markReady(): void {
    this.ready = true;
    this.deps.post({ type: "ready" });
    // Order matters: flush appends first (creating each parser + applying
    // buffered text), then finalize any stream that already requested it.
    if (this.pending.size > 0) this.flush();
    if (this.finalizePending.size > 0) {
      for (const id of this.finalizePending) this.doFinalize(id);
      this.finalizePending.clear();
    }
  }

  private getOrCreate(streamId: number): ParserLike {
    let p = this.parsers.get(streamId);
    if (!p) {
      p = this.deps.makeParser(this.config.get(streamId));
      this.parsers.set(streamId, p);
    }
    return p;
  }

  private dispose(streamId: number): void {
    this.safeFree(streamId); // guarded: a poisoned instance can throw on free()
    this.parsers.delete(streamId);
    this.config.delete(streamId);
    this.pending.delete(streamId);
    this.finalizePending.delete(streamId);
    this.totalAppended.delete(streamId);
    this.epochs.delete(streamId);
  }

  /** free() a stream's parser, swallowing a throw from a poisoned WASM instance
   *  so teardown can never escape the message loop. */
  private safeFree(streamId: number): void {
    try {
      this.parsers.get(streamId)?.free();
    } catch {
      /* instance already trapped/poisoned — nothing to reclaim */
    }
  }

  private emitPatch(
    streamId: number,
    patch: string,
    parser: ParserLike,
    parseMicros: number,
    final: boolean,
  ): void {
    this.deps.post({
      type: "patch",
      streamId,
      patch,
      appendedBytes: this.totalAppended.get(streamId) ?? 0,
      parseMicros,
      retainedBytes: parser.retainedBytes(),
      wasmMemoryBytes: this.deps.memBytes(),
      final,
      epoch: this.epochs.get(streamId),
    });
  }

  private scheduleFlush(): void {
    if (this.flushScheduled || !this.ready) return; // before ready, input just accumulates in `pending`
    this.flushScheduled = true;
    this.deps.schedule(() => this.flush());
  }

  private flush(): void {
    this.flushScheduled = false;
    if (!this.ready || this.pending.size === 0) return; // buffer stays put until WASM is ready
    // Process every stream with buffered input this microtask.
    for (const [streamId, chunk] of this.pending) {
      this.pending.delete(streamId);
      if (chunk.length === 0) continue;
      const t0 = performance.now();
      try {
        // getOrCreate (→ makeParser) is inside the try: with `ready` gating it
        // can't hit the init race, but any other construction failure becomes a
        // posted error rather than an uncaught exception that kills the worker.
        const parser = this.getOrCreate(streamId);
        const patch = parser.append(chunk);
        const dt = (performance.now() - t0) * 1000;
        this.totalAppended.set(streamId, (this.totalAppended.get(streamId) ?? 0) + chunk.length);
        this.emitPatch(streamId, patch, parser, dt, false);
      } catch (e: unknown) {
        this.postParseError(streamId, e);
      }
    }
  }

  // Drain a stream's buffered input (if any), then finalize its parser. Shared by
  // the `finalize` message path and markReady()'s post-ready drain.
  private doFinalize(streamId: number): void {
    const buffered = this.pending.get(streamId);
    this.pending.delete(streamId);
    try {
      const parser = this.getOrCreate(streamId);
      if (buffered && buffered.length > 0) {
        parser.append(buffered);
        this.totalAppended.set(streamId, (this.totalAppended.get(streamId) ?? 0) + buffered.length);
      }
      const patch = parser.finalize();
      this.emitPatch(streamId, patch, parser, 0, true);
    } catch (e: unknown) {
      this.postParseError(streamId, e);
    }
  }

  // A WASM trap (stack overflow / unreachable / OOM) throws a
  // WebAssembly.RuntimeError and POISONS the singleton instance shared by every
  // stream on this worker — so it is escalated to `fatal`, which the pool treats
  // as a worker-level failure (eviction + terminate). A plain Error (e.g. a
  // parser-construction failure) stays a recoverable per-stream error.
  private postParseError(streamId: number, e: unknown): void {
    const fatal =
      typeof WebAssembly !== "undefined" &&
      typeof WebAssembly.RuntimeError === "function" &&
      e instanceof WebAssembly.RuntimeError;
    this.deps.post({
      type: "error",
      streamId,
      message: e instanceof Error ? e.message : String(e),
      fatal,
    });
  }
}

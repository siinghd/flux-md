import type { FromWorker, ParserConfig, Patch, ToWorker } from "./types-core";

/** The slice of `FluxParser` the worker drives — narrowed to an interface so the
 *  message/readiness state machine is unit-testable with a fake parser, no WASM.
 *  (Same testability move as {@link FluxPool} taking an injected worker factory.) */
export interface ParserLike {
  append(chunk: string): Patch;
  finalize(): Patch;
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
        this.parsers.get(id)?.free();
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
    this.parsers.get(streamId)?.free();
    this.parsers.delete(streamId);
    this.config.delete(streamId);
    this.pending.delete(streamId);
    this.finalizePending.delete(streamId);
    this.totalAppended.delete(streamId);
  }

  private emitPatch(streamId: number, patch: Patch, parser: ParserLike, parseMicros: number): void {
    this.deps.post({
      type: "patch",
      streamId,
      patch,
      appendedBytes: this.totalAppended.get(streamId) ?? 0,
      parseMicros,
      retainedBytes: parser.retainedBytes(),
      wasmMemoryBytes: this.deps.memBytes(),
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
        const patch = parser.append(chunk) as Patch;
        const dt = (performance.now() - t0) * 1000;
        this.totalAppended.set(streamId, (this.totalAppended.get(streamId) ?? 0) + chunk.length);
        this.emitPatch(streamId, patch, parser, dt);
      } catch (e: unknown) {
        this.deps.post({ type: "error", streamId, message: e instanceof Error ? e.message : String(e) });
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
      const patch = parser.finalize() as Patch;
      this.emitPatch(streamId, patch, parser, 0);
    } catch (e: unknown) {
      this.deps.post({ type: "error", streamId, message: e instanceof Error ? e.message : String(e) });
    }
  }
}

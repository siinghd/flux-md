import type { Block, FromWorker, ParserConfig, Patch, ToWorker, WorkerLike } from "./types-core";

/**
 * The ordered-block store backing a stream, extracted as a pure function so
 * its reference-stability contract is testable without a Worker.
 *
 * **The contract that prevents extra React re-renders:** a block, once
 * committed, is never re-sent by the parser, so `applyPatch` never replaces it
 * in the map. Its object reference stays identical across every later patch —
 * which is exactly what `blocksEqual` (the BlockView memo) checks, so committed
 * blocks never re-render (and never re-parse) as the stream grows. Only the
 * `active` tail gets fresh references each patch, and only it re-renders.
 */
export interface BlockStore {
  committed: Map<number, Block>;
  committedOrder: number[];
  active: Block[];
  snapshot: Block[];
}

export function emptyBlockStore(): BlockStore {
  return { committed: new Map(), committedOrder: [], active: [], snapshot: [] };
}

/** A heading entry for building a table of contents — see {@link FluxClient.outline}. */
export interface OutlineEntry {
  /** Heading level 1–6. */
  level: number;
  /** Plain-text heading content (tags stripped, entities decoded). */
  text: string;
  /** Stable block id — usable as a scroll target / React key. */
  id: number;
}

/** Strip tags (→ space) and decode the small entity set the core emits, then
 *  collapse whitespace. INVARIANT: the simple `<[^>]*>` strip is only safe
 *  because every input here is HTML the Rust core produced via escape_html /
 *  escape_attr — which escape `>` inside attribute values, so no `>` ever
 *  appears except as a real tag close. This must NOT be fed externally-authored
 *  HTML. `&amp;` decodes last so `&amp;lt;` → `&lt;`, not `<`. */
function htmlToText(html: string): string {
  return html
    .replace(/<[^>]*>/g, " ")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, "&")
    .replace(/\s+/g, " ")
    .trim();
}

export function applyPatch(store: BlockStore, patch: Patch): void {
  for (const b of patch.newly_committed) {
    if (!store.committed.has(b.id)) store.committedOrder.push(b.id);
    store.committed.set(b.id, b);
  }
  store.active = patch.active;
  // Fresh array each patch (immutable for React reference checks), but the
  // committed entries inside it are the same object references as before.
  const next: Block[] = new Array(store.committedOrder.length + store.active.length);
  for (let i = 0; i < store.committedOrder.length; i++) {
    next[i] = store.committed.get(store.committedOrder[i])!;
  }
  for (let i = 0; i < store.active.length; i++) {
    next[store.committedOrder.length + i] = store.active[i];
  }
  store.snapshot = next;
}

// --------------------------------------------------------------------------
// Worker pool
// --------------------------------------------------------------------------

interface PoolWorker {
  worker: WorkerLike;
  ready: boolean;
  /** Set once WASM init fails; whenWorkerReady rejects with this thereafter. */
  failed: Error | null;
  streamCount: number;
  /** Live stream ids on this worker — so a fatal failure can notify each one. */
  streamIds: Set<number>;
  readyWaiters: Array<{ resolve: () => void; reject: (e: Error) => void }>;
}

/**
 * A pool of Web Workers, each multiplexing many `FluxParser`s keyed by stream
 * id. This is what lets flux-md scale past `hardwareConcurrency` concurrent
 * streams without oversubscribing OS threads: 50 streams share (at most) the
 * cap's worth of workers instead of spawning 50.
 *
 * Worker creation is **lazy and load-aware**: while under the cap, each new
 * stream gets its own worker (so 1 stream = 1 worker, identical to the old
 * behavior); once at the cap, new streams attach to the least-loaded worker.
 *
 * The constructor injects a `WorkerLike` factory so the routing and lifecycle
 * logic is unit-testable with a fake worker — no real Worker or WASM needed.
 */
export class FluxPool {
  private workers: PoolWorker[] = [];
  private handlers = new Map<number, (msg: FromWorker) => void>();
  private nextStreamId = 1;

  constructor(
    private factory: () => WorkerLike,
    private cap: number,
  ) {}

  /** Reserve a stream id and assign a worker, registering its message handler. */
  acquire(handler: (msg: FromWorker) => void): { streamId: number; pw: PoolWorker } {
    const streamId = this.nextStreamId++;
    const pw = this.pick();
    pw.streamCount++;
    pw.streamIds.add(streamId);
    this.handlers.set(streamId, handler);
    return { streamId, pw };
  }

  /** Free a stream's parser in its worker; keep the worker warm for siblings. */
  release(streamId: number, pw: PoolWorker): void {
    this.handlers.delete(streamId);
    pw.streamIds.delete(streamId);
    pw.streamCount = Math.max(0, pw.streamCount - 1);
    try {
      pw.worker.postMessage({ type: "dispose", streamId });
    } catch {
      /* worker already gone */
    }
  }

  send(pw: PoolWorker, msg: ToWorker): void {
    pw.worker.postMessage(msg);
  }

  /** Resolves when the given worker has finished WASM init; rejects if it failed. */
  whenWorkerReady(pw: PoolWorker): Promise<void> {
    if (pw.ready) return Promise.resolve();
    if (pw.failed) return Promise.reject(pw.failed);
    return new Promise((resolve, reject) => pw.readyWaiters.push({ resolve, reject }));
  }

  /** Terminate every worker (test teardown / full shutdown). */
  disposeAll(): void {
    for (const pw of this.workers) {
      try {
        pw.worker.terminate();
      } catch {
        /* ignore */
      }
    }
    this.workers = [];
    this.handlers.clear();
  }

  get workerCount(): number {
    return this.workers.length;
  }

  // Create a new worker while under cap and every live worker is busy; otherwise
  // attach to the least-loaded LIVE worker. A fatally-failed worker is never
  // handed out (a stream on it would post into a dead worker and hang) — it is
  // retained only to reject outstanding whenWorkerReady waiters.
  private pick(): PoolWorker {
    const live = this.workers.filter((w) => !w.failed);
    if (this.workers.length < this.cap && live.every((w) => w.streamCount > 0)) {
      return this.create();
    }
    if (live.length === 0) return this.create();
    return live.reduce((a, b) => (b.streamCount < a.streamCount ? b : a));
  }

  private create(): PoolWorker {
    const pw: PoolWorker = {
      worker: this.factory(),
      ready: false,
      failed: null,
      streamCount: 0,
      streamIds: new Set(),
      readyWaiters: [],
    };
    pw.worker.addEventListener("message", (ev) => this.onMessage(pw, ev.data));
    this.workers.push(pw);
    return pw;
  }

  private onMessage(pw: PoolWorker, msg: FromWorker): void {
    if (msg.type === "ready") {
      pw.ready = true;
      const waiters = pw.readyWaiters;
      pw.readyWaiters = [];
      for (const w of waiters) w.resolve();
      return;
    }
    if (msg.type === "error" && msg.fatal) {
      // A fatal (WASM-init) failure dooms every stream on this worker. Reject
      // anyone awaiting readiness, then notify each live stream's client so its
      // onError fires — the message carries no real streamId to route by. The
      // worker is kept only to reject those waiters; pick() never reuses it.
      const err = new Error(msg.message);
      pw.failed = err;
      const waiters = pw.readyWaiters;
      pw.readyWaiters = [];
      for (const w of waiters) {
        try {
          w.reject(err);
        } catch {
          /* a waiter's rejection handler is the caller's problem, not ours */
        }
      }
      for (const sid of pw.streamIds) this.dispatch(sid, msg);
      return;
    }
    this.dispatch(msg.streamId, msg);
  }

  // Route a message to a stream's handler, isolating a throwing client callback
  // (e.g. a user-supplied onError) so it can neither break the worker message
  // loop nor starve sibling streams sharing this worker.
  private dispatch(streamId: number, msg: FromWorker): void {
    try {
      this.handlers.get(streamId)?.(msg);
    } catch (e) {
      // eslint-disable-next-line no-console
      console.error("flux: stream message handler threw", e);
    }
  }
}

function poolCap(): number {
  const hc = typeof navigator !== "undefined" ? navigator.hardwareConcurrency : 0;
  return Math.min(hc || 4, 8);
}

let defaultPool: FluxPool | null = null;

/** The process-wide default pool every `FluxClient` shares unless given one. */
export function getDefaultPool(): FluxPool {
  if (!defaultPool) {
    defaultPool = new FluxPool(
      () => new Worker(new URL("./worker.ts", import.meta.url), { type: "module" }) as unknown as WorkerLike,
      poolCap(),
    );
  }
  return defaultPool;
}

// --------------------------------------------------------------------------
// Client
// --------------------------------------------------------------------------

/**
 * Subscriber-driven store backing a single streaming parser. Each client owns
 * one stream within a shared {@link FluxPool}; many clients multiplex over a
 * small set of workers (see the pool for the scaling story).
 *
 * The store exposes:
 *   - subscribe(listener): for React's useSyncExternalStore
 *   - getSnapshot(): the current ordered list of blocks
 *   - getMetrics(): per-stream perf metrics
 *
 * Mutation methods:
 *   - append(chunk): forward to the worker
 *   - finalize(): mark the stream done
 *   - reset(): start fresh
 */
export class FluxClient {
  private pool: FluxPool;
  private pw: PoolWorker;
  private streamId: number;
  private config?: ParserConfig;
  private configSent = false;
  private listeners = new Set<() => void>();
  private store: BlockStore = emptyBlockStore();
  private onError?: (err: { message: string; fatal?: boolean }) => void;
  private onBlock?: (block: Block) => void;

  // Perf
  private appendedBytes = 0;
  private patchCount = 0;
  private totalParseMicros = 0;
  private lastPatchMs = 0;
  private firstAppendMs = 0;
  private retainedBytes = 0;
  private wasmMemoryBytes = 0;

  /**
   * @param options.pool   worker pool to join (defaults to the shared
   *   process-wide pool — pass a dedicated `FluxPool` only for isolation).
   * @param options.config per-stream parser flags (see {@link ParserConfig});
   *   omitted fields use library defaults. Applied once, immutable thereafter.
   * @param options.onError invoked on a worker/parse error or a fatal WASM-init
   *   failure (`fatal: true`). Without it, errors are only `console.error`d and
   *   a load failure surfaces solely as a rejected {@link FluxClient.whenReady}.
   * @param options.onBlock invoked once per block as it commits (in document
   *   order, after the store updates) — for side effects like lazily
   *   highlighting a finished code block or analytics. A committed block never
   *   re-fires; the streaming tail does not (subscribe for live tail updates).
   */
  constructor(
    options: {
      pool?: FluxPool;
      config?: ParserConfig;
      onError?: (err: { message: string; fatal?: boolean }) => void;
      onBlock?: (block: Block) => void;
    } = {},
  ) {
    this.pool = options.pool ?? getDefaultPool();
    this.config = options.config;
    this.onError = options.onError;
    this.onBlock = options.onBlock;
    const { streamId, pw } = this.pool.acquire((msg) => this.onMessage(msg));
    this.streamId = streamId;
    this.pw = pw;
  }

  get ready(): boolean {
    return this.pw.ready;
  }

  whenReady(): Promise<void> {
    return this.pool.whenWorkerReady(this.pw);
  }

  // The config rides on the first message a stream sends; the worker applies it
  // when it creates the parser. postMessage is FIFO per worker, so it always
  // lands before any append is processed. Returns undefined after the first use.
  private firstConfig(): ParserConfig | undefined {
    if (this.configSent || !this.config) return undefined;
    this.configSent = true;
    return this.config;
  }

  append(chunk: string) {
    if (this.firstAppendMs === 0) this.firstAppendMs = performance.now();
    this.pool.send(this.pw, { type: "append", streamId: this.streamId, chunk, config: this.firstConfig() });
  }

  finalize() {
    this.pool.send(this.pw, { type: "finalize", streamId: this.streamId, config: this.firstConfig() });
  }

  /**
   * Pipe a source straight in: read it to completion, `append()` each chunk,
   * then `finalize()`. The LLM-native path — e.g.
   * `await client.pipeFrom(await fetch("/api/chat"))`. Accepts:
   *   - a `Response` or its `ReadableStream<Uint8Array>` body (bytes; decoded
   *     with `TextDecoder({ stream: true })` so a multibyte sequence straddling
   *     a chunk boundary carries into the next read), or
   *   - an `AsyncIterable<string>` (e.g. an SSE delta generator) — string chunks
   *     appended verbatim.
   *
   * Pass `opts.signal` to supersede/cancel: the signal is checked on every
   * iteration, so once aborted no further chunk is appended and **finalize is
   * skipped** (a superseded stream must not finalize). For a byte source the
   * reader is also `cancel()`'d to tear down the upstream. Resolves once
   * finalized (or cleanly on abort); rejects if the source itself errors.
   * Browser-only for byte sources (uses `TextDecoder`).
   */
  async pipeFrom(
    source: ReadableStream<Uint8Array> | Response | AsyncIterable<string>,
    opts?: { signal?: AbortSignal },
  ): Promise<void> {
    const signal = opts?.signal;

    if (signal?.aborted) return; // already superseded before we started

    // AsyncIterable<string> (SSE deltas, generators). Detected by elimination:
    // a ReadableStream has `getReader`, a Response has `body` — neither here.
    if (!("getReader" in source) && !("body" in source)) {
      for await (const chunk of source as AsyncIterable<string>) {
        if (signal?.aborted) return; // superseded/unmounted: drop late chunks, no finalize
        this.append(chunk);
      }
      if (!signal?.aborted) this.finalize();
      return;
    }

    // Byte source: a Response (use its body) or a ReadableStream directly.
    const body = "body" in source ? source.body : source;
    if (!body) {
      // An empty Response body (e.g. 204) is a completed, empty stream.
      this.finalize();
      return;
    }
    const reader = body.getReader();
    // A pending read() can't observe `aborted` until the next chunk; cancel()
    // on abort tears down the upstream and resolves the pending read so the
    // loop's post-read check fires and bails without finalizing.
    const onAbort = () => {
      reader.cancel().catch(() => {});
    };
    signal?.addEventListener("abort", onAbort, { once: true });
    const decoder = new TextDecoder();
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (signal?.aborted) return; // superseded: no finalize (cancel already fired)
        if (done) break;
        if (value) this.append(decoder.decode(value, { stream: true }));
      }
      this.append(decoder.decode()); // flush any trailing partial sequence
      this.finalize();
    } finally {
      signal?.removeEventListener("abort", onAbort);
      try {
        reader.releaseLock();
      } catch {
        /* already released (e.g. by cancel) */
      }
    }
  }

  reset() {
    this.store = emptyBlockStore();
    this.appendedBytes = 0;
    this.patchCount = 0;
    this.totalParseMicros = 0;
    this.lastPatchMs = 0;
    this.firstAppendMs = 0;
    this.retainedBytes = 0;
    this.wasmMemoryBytes = 0;
    // Same streamId + worker — the worker frees and lazily recreates the parser.
    this.pool.send(this.pw, { type: "reset", streamId: this.streamId });
    this.emit();
  }

  destroy() {
    // Free this stream's parser; the shared worker stays warm for siblings.
    this.pool.release(this.streamId, this.pw);
    this.listeners.clear();
  }

  subscribe = (fn: () => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  getSnapshot = (): Block[] => this.store.snapshot;

  getMetrics() {
    const elapsed = this.firstAppendMs ? Math.max(1, performance.now() - this.firstAppendMs) : 1;
    return {
      bytes: this.appendedBytes,
      patches: this.patchCount,
      meanParseMicros: this.patchCount > 0 ? this.totalParseMicros / this.patchCount : 0,
      totalParseMs: this.totalParseMicros / 1000,
      throughputKBs: (this.appendedBytes / 1024) / (elapsed / 1000),
      committedBlocks: this.store.committed.size,
      activeBlocks: this.store.active.length,
      lastPatchAgoMs: this.lastPatchMs === 0 ? 0 : performance.now() - this.lastPatchMs,
      retainedBytes: this.retainedBytes,
      // NOTE: with the worker pool, this is the *shared* worker's WASM heap —
      // clients on the same worker report the same number. Use Math.max (not
      // sum) when aggregating across clients; summing double-counts.
      wasmMemoryBytes: this.wasmMemoryBytes,
    };
  }

  /**
   * A heading outline of the current snapshot (committed + active), in document
   * order — for a table of contents. Works mid-stream; entries appear as their
   * headings stream in. The `id` is stable, so a built ToC won't re-key.
   */
  outline(): OutlineEntry[] {
    const out: OutlineEntry[] = [];
    for (const b of this.store.snapshot) {
      if (b.kind.type === "Heading") {
        out.push({ level: (b.kind.data as number) ?? 1, text: htmlToText(b.html), id: b.id });
      }
    }
    return out;
  }

  /**
   * The rendered document as plain text — tags stripped, entities decoded,
   * blocks separated by blank lines. Derived from the rendered HTML (the source
   * markdown is parsed away in WASM and not retained client-side), so it is a
   * readable approximation for search indexing / summaries, not a round-trip of
   * the original source.
   */
  toPlaintext(): string {
    const parts: string[] = [];
    for (const b of this.store.snapshot) {
      const t = htmlToText(b.html);
      if (t) parts.push(t);
    }
    return parts.join("\n\n");
  }

  private onMessage(msg: FromWorker) {
    switch (msg.type) {
      case "patch":
        applyPatch(this.store, msg.patch);
        this.appendedBytes = msg.appendedBytes;
        this.totalParseMicros += msg.parseMicros;
        this.retainedBytes = msg.retainedBytes;
        this.wasmMemoryBytes = msg.wasmMemoryBytes;
        this.patchCount += 1;
        this.lastPatchMs = performance.now();
        this.emit();
        // After subscribers see the new snapshot, fire the per-block hook for
        // anything that just committed (document order). A throw here is
        // isolated by the pool's dispatch boundary and won't skip emit().
        if (this.onBlock) {
          for (const b of msg.patch.newly_committed) this.onBlock(b);
        }
        break;
      case "error":
        if (this.onError) {
          this.onError({ message: msg.message, fatal: msg.fatal });
        } else {
          // eslint-disable-next-line no-console
          console.error("flux worker error:", msg.message);
        }
        break;
    }
  }

  private emit() {
    for (const fn of this.listeners) fn();
  }
}

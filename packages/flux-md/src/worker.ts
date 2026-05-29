/// <reference lib="webworker" />
import init, { FluxParser } from "./wasm/flux_md_core.js";
import type { FromWorker, ParserConfig, Patch, ToWorker } from "./types";

// Resolve the WASM asset with the *web-standard* `new URL(asset,
// import.meta.url)` pattern (not Vite's `?url` suffix), so the package works in
// any bundler with asset-module support — Vite, webpack 5, Rollup, Parcel.
// wasm-bindgen's init() fetches a URL instance directly.
const wasmUrl = new URL("./wasm/flux_md_core_bg.wasm", import.meta.url);

// One worker multiplexes many streams: a parser per stream id (the worker
// pool). WASM is loaded once for the whole worker, shared by every parser.
const parsers = new Map<number, FluxParser>();
const config = new Map<number, ParserConfig>();
const pending = new Map<number, string>();
const totalAppended = new Map<number, number>();
let flushScheduled = false;
let wasmExports: any = null;

const ctx: DedicatedWorkerGlobalScope = self as unknown as DedicatedWorkerGlobalScope;

function post(msg: FromWorker) {
  ctx.postMessage(msg);
}

async function setup() {
  // init() returns the wasm-bindgen instance; capture its `.memory` export so
  // we can report WASM-side memory usage on every patch. No parser yet — they
  // are created per stream, on demand.
  try {
    wasmExports = await init({ module_or_path: wasmUrl });
    post({ type: "ready" });
  } catch (e: unknown) {
    // WASM failed to load/instantiate: this worker can never parse anything.
    // Report it so the pool rejects whenReady() (rather than hanging forever)
    // and clients' onError fires. streamId is irrelevant for a worker-level
    // failure — the pool routes a fatal error to every stream it hosts.
    post({ type: "error", streamId: -1, message: e instanceof Error ? e.message : String(e), fatal: true });
  }
}

function getOrCreate(streamId: number): FluxParser {
  let p = parsers.get(streamId);
  if (!p) {
    p = new FluxParser();
    // Per-stream config (sent on the stream's first message); omitted fields
    // fall back to the library defaults — autolinks + alerts on (LLM output is
    // full of bare URLs and `> [!NOTE]` blocks), raw HTML escaped, footnotes off.
    const c = config.get(streamId);
    p.setGfmAutolinks(c?.gfmAutolinks ?? true);
    p.setGfmAlerts(c?.gfmAlerts ?? true);
    p.setGfmFootnotes(c?.gfmFootnotes ?? false);
    p.setGfmMath(c?.gfmMath ?? false);
    p.setDirAuto(c?.dirAuto ?? false);
    p.setUnsafeHtml(c?.unsafeHtml ?? false);
    p.setComponentTags(c?.componentTags ?? []);
    parsers.set(streamId, p);
  }
  return p;
}

function dispose(streamId: number) {
  parsers.get(streamId)?.free();
  parsers.delete(streamId);
  config.delete(streamId);
  pending.delete(streamId);
  totalAppended.delete(streamId);
}

function wasmMemBytes(): number {
  try {
    return (wasmExports?.memory?.buffer?.byteLength as number) ?? 0;
  } catch {
    return 0;
  }
}

function emitPatch(streamId: number, patch: Patch, parser: FluxParser, parseMicros: number) {
  post({
    type: "patch",
    streamId,
    patch,
    appendedBytes: totalAppended.get(streamId) ?? 0,
    parseMicros,
    retainedBytes: parser.retainedBytes(),
    wasmMemoryBytes: wasmMemBytes(),
  });
}

function flush() {
  flushScheduled = false;
  if (pending.size === 0) return;
  // Process every stream with buffered input this microtask.
  for (const [streamId, chunk] of pending) {
    pending.delete(streamId);
    if (chunk.length === 0) continue;
    const parser = getOrCreate(streamId);
    const t0 = performance.now();
    try {
      const patch = parser.append(chunk) as Patch;
      const dt = (performance.now() - t0) * 1000;
      totalAppended.set(streamId, (totalAppended.get(streamId) ?? 0) + chunk.length);
      emitPatch(streamId, patch, parser, dt);
    } catch (e: unknown) {
      post({ type: "error", streamId, message: e instanceof Error ? e.message : String(e) });
    }
  }
}

function scheduleFlush() {
  if (flushScheduled) return;
  flushScheduled = true;
  queueMicrotask(flush);
}

ctx.addEventListener("message", (ev: MessageEvent<ToWorker>) => {
  const msg = ev.data;
  const id = msg.streamId;
  // Stash any per-stream config carried on the first message (FIFO guarantees
  // it arrives before the parser is created in flush/finalize).
  if ((msg.type === "append" || msg.type === "finalize") && msg.config) {
    config.set(id, msg.config);
  }
  switch (msg.type) {
    case "append":
      pending.set(id, (pending.get(id) ?? "") + msg.chunk);
      scheduleFlush();
      break;
    case "finalize": {
      // Drain any buffered input for this stream, then finalize.
      const buffered = pending.get(id);
      pending.delete(id);
      const parser = getOrCreate(id);
      try {
        if (buffered && buffered.length > 0) {
          parser.append(buffered);
          totalAppended.set(id, (totalAppended.get(id) ?? 0) + buffered.length);
        }
        const patch = parser.finalize() as Patch;
        emitPatch(id, patch, parser, 0);
      } catch (e: unknown) {
        post({ type: "error", streamId: id, message: e instanceof Error ? e.message : String(e) });
      }
      break;
    }
    case "reset":
      // Free and recreate lazily on the next append — same stream id, **same
      // config** (kept). The client resets its local state synchronously.
      parsers.get(id)?.free();
      parsers.delete(id);
      pending.delete(id);
      totalAppended.delete(id);
      break;
    case "dispose":
      dispose(id);
      break;
  }
});

setup();

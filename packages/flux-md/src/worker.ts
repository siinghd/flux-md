/// <reference lib="webworker" />
import init, { FluxParser } from "./wasm/flux_md_core.js";
import type { ParserConfig } from "./types";
import { WorkerCore, type ParserLike } from "./worker-core";

// Resolve the WASM asset with the *web-standard* `new URL(asset,
// import.meta.url)` pattern (not Vite's `?url` suffix), so the package works in
// any bundler with asset-module support — Vite, webpack 5, Rollup, Parcel, and
// Next.js (Turbopack/webpack). wasm-bindgen's init() fetches a URL instance.
const wasmUrl = new URL("./wasm/flux_md_core_bg.wasm", import.meta.url);

const ctx: DedicatedWorkerGlobalScope = self as unknown as DedicatedWorkerGlobalScope;

// Captured from init() so we can report WASM-side memory usage on each patch.
let wasmExports: { memory?: { buffer?: ArrayBufferLike } } | null = null;

// The message/readiness state machine lives in WorkerCore (testable without
// WASM); this shell injects the WASM-specific dependencies.
const core = new WorkerCore({
  // Create + configure a parser for a stream. Omitted config fields fall back to
  // the library defaults — autolinks + alerts on (LLM output is full of bare
  // URLs and `> [!NOTE]` blocks), raw HTML escaped, footnotes/math off.
  makeParser(c: ParserConfig | undefined): ParserLike {
    const p = new FluxParser();
    p.setGfmAutolinks(c?.gfmAutolinks ?? true);
    p.setGfmAlerts(c?.gfmAlerts ?? true);
    p.setGfmFootnotes(c?.gfmFootnotes ?? false);
    p.setGfmMath(c?.gfmMath ?? false);
    p.setDirAuto(c?.dirAuto ?? false);
    p.setA11y(c?.a11y ?? false);
    p.setUnsafeHtml(c?.unsafeHtml ?? false);
    p.setComponentTags(c?.componentTags ?? []);
    p.setInlineComponentTags(c?.inlineComponentTags ?? []);
    // Engage the safe raw-HTML sanitizer when either list is provided (even []).
    p.setHtmlSanitize(
      c?.htmlAllowlist !== undefined || c?.dropHtmlTags !== undefined,
      c?.htmlAllowlist ?? [],
      c?.dropHtmlTags ?? [],
    );
    p.setBlockData(c?.blockData ?? false);
    return p;
  },
  post: (msg) => ctx.postMessage(msg),
  memBytes: () => {
    try {
      return (wasmExports?.memory?.buffer?.byteLength as number) ?? 0;
    } catch {
      return 0;
    }
  },
  schedule: (fn) => queueMicrotask(fn),
});

ctx.addEventListener("message", (ev: MessageEvent) => core.handle(ev.data));

async function setup() {
  // init() returns the wasm-bindgen instance; capture its `.memory` export for
  // the per-patch memory metric. No parser yet — they are created per stream,
  // on demand, only after markReady() opens the gate.
  try {
    wasmExports = await init({ module_or_path: wasmUrl });
    core.markReady();
  } catch (e: unknown) {
    // WASM failed to load/instantiate: this worker can never parse anything.
    // Report it so the pool rejects whenReady() (rather than hanging forever)
    // and clients' onError fires. streamId is irrelevant for a worker-level
    // failure — the pool routes a fatal error to every stream it hosts.
    ctx.postMessage({ type: "error", streamId: -1, message: e instanceof Error ? e.message : String(e), fatal: true });
  }
}

setup();

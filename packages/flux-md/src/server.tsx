import { createElement, type ReactNode } from "react";
import initWasmAsync, { FluxParser, initSync } from "./wasm/flux_md_core.js";
import { htmlToReact } from "./html-to-react";
import { blockKindProps } from "./react";
import type { Block, Components, ParserConfig } from "./types";

/**
 * Synchronous, worker-free server / static rendering for flux-md.
 *
 * The browser path runs the Rust→WASM core in a Web Worker, but the very same
 * `FluxParser` is a plain synchronous class — so on the server (Node, RSC, a
 * build step) you can parse a finished markdown string with no worker and no
 * async ceremony:
 *
 * ```ts
 * import { initFlux, renderToString } from "flux-md/server";
 * await initFlux();                       // once, at startup
 * const html = renderToString(markdown);  // sync, no worker
 * ```
 *
 * For React server rendering (RSC, static generation, SSR) use {@link
 * FluxMarkdownStatic} — a hookless, RSC-safe component with the same `components`
 * overrides. It targets **render-once** contexts; the streaming, interactive
 * `<FluxMarkdown>` (client-side code highlighting, Mermaid, live updates) is a
 * separate component. If you SSR-then-hydrate, use the *same* component on both
 * sides.
 */

let ready = false;

/** Has the sync WASM core been initialized in this process? */
export function isFluxReady(): boolean {
  return ready;
}

/** Initialize the sync core from compiled WASM bytes (or a `WebAssembly.Module`).
 *  Idempotent. Use on runtimes without a filesystem (edge) or to control exactly
 *  when init happens; otherwise {@link initFlux} auto-loads the co-located WASM. */
export function initFluxSync(wasm: BufferSource | WebAssembly.Module): void {
  if (ready) return;
  initSync({ module: wasm });
  ready = true;
}

let initPromise: Promise<void> | null = null;

/** Initialize the sync core once. In Node it reads the package's co-located
 *  `.wasm` off disk (Node's `fetch` can't load `file://`); on the web it fetches
 *  the bundler-resolved asset URL. Pass `{ wasm }` to supply bytes yourself
 *  (edge runtimes). Safe to call repeatedly / concurrently. */
export function initFlux(opts?: { wasm?: BufferSource | WebAssembly.Module }): Promise<void> {
  if (ready) return Promise.resolve();
  if (opts?.wasm) {
    initFluxSync(opts.wasm);
    return Promise.resolve();
  }
  if (!initPromise) {
    initPromise = (async () => {
      const wasmUrl = new URL("./wasm/flux_md_core_bg.wasm", import.meta.url);
      if (wasmUrl.protocol === "file:") {
        // Node: read the bytes (Node's fetch can't load file://). The literal
        // `node:` specifier is externalized by bundlers, so node:fs never reaches
        // a web bundle (this branch is also file:-only, never true in browsers).
        // @ts-ignore — no @types/node in this package; node:fs/promises is a builtin.
        const { readFile } = await import("node:fs/promises");
        initFluxSync(await readFile(wasmUrl));
      } else {
        await initWasmAsync({ module_or_path: wasmUrl });
        ready = true;
      }
    })();
  }
  return initPromise;
}

// Configure a one-shot parser exactly as the worker does, so server output is
// byte-identical to the streamed/browser output (defaults: autolinks + alerts
// on, raw HTML escaped, footnotes/math off).
function makeParser(config?: ParserConfig): FluxParser {
  const p = new FluxParser();
  p.setGfmAutolinks(config?.gfmAutolinks ?? true);
  p.setGfmAlerts(config?.gfmAlerts ?? true);
  p.setGfmFootnotes(config?.gfmFootnotes ?? false);
  p.setGfmMath(config?.gfmMath ?? false);
  p.setDirAuto(config?.dirAuto ?? false);
  p.setA11y(config?.a11y ?? false);
  p.setUnsafeHtml(config?.unsafeHtml ?? false);
  p.setComponentTags(config?.componentTags ?? []);
  p.setInlineComponentTags(config?.inlineComponentTags ?? []);
  // Engage the safe raw-HTML sanitizer when either list is provided (even []).
  p.setHtmlSanitize(
    config?.htmlAllowlist !== undefined || config?.dropHtmlTags !== undefined,
    config?.htmlAllowlist ?? [],
    config?.dropHtmlTags ?? [],
  );
  p.setBlockData(config?.blockData ?? false);
  return p;
}

function requireReady(): void {
  if (!ready) {
    throw new Error(
      "flux-md/server: WASM not initialized. Call `await initFlux()` (or `initFluxSync(bytes)`) once before rendering.",
    );
  }
}

/**
 * Parse a complete markdown string to its block array synchronously (committed +
 * any trailing block, in document order). Requires {@link initFlux} to have run.
 */
export function parseToBlocks(markdown: string, opts?: { config?: ParserConfig }): Block[] {
  requireReady();
  const p = makeParser(opts?.config);
  try {
    p.append(markdown);
    p.finalize();
    return p.allBlocks() as Block[];
  } finally {
    p.free();
  }
}

/**
 * Render a complete markdown string to an HTML string synchronously — no worker,
 * no React. The concatenated per-block HTML (XSS-safe with `unsafeHtml` off).
 * For component dispatch / a `<FluxMarkdown>`-matching React tree, use
 * {@link FluxMarkdownStatic} with your framework's server renderer instead.
 */
export function renderToString(markdown: string, opts?: { config?: ParserConfig }): string {
  return parseToBlocks(markdown, opts)
    .map((b) => b.html)
    .join("");
}

// Hookless block renderer (RSC-safe): mirrors the client renderer's dispatch
// (block-kind overrides, a Component block dispatched by tag, tag-level overrides
// via htmlToReact) but uses no hooks and skips the client-only interactive
// renderers (Mermaid; client-side code highlighting) — those activate on the
// client after hydration. Kept in step with react.tsx's renderBlockContent.
function renderStaticBlock(block: Block, components?: Components): ReactNode {
  const kind = block.kind.type;
  if (components) {
    if (kind === "Component") {
      const tag = (block.kind.data as { tag?: string } | undefined)?.tag;
      const override = (tag && components[tag]) || components.Component;
      if (override) return createElement(override, { key: block.id, ...blockKindProps(block, components) });
    }
    const blockOverride = components[kind];
    if (blockOverride) return createElement(blockOverride, { key: block.id, ...blockKindProps(block, components) });
  }
  const className =
    "flux-block flux-block-" +
    kind.toLowerCase() +
    (block.open ? " flux-open" : "") +
    (block.speculative ? " flux-speculative" : "");
  if (components) {
    return createElement("div", { key: block.id, className }, htmlToReact(block.html, components));
  }
  return createElement("div", { key: block.id, className, dangerouslySetInnerHTML: { __html: block.html } });
}

interface FluxMarkdownStaticProps {
  /** The complete markdown to render (server/static use is for finished content). */
  content: string;
  /** Parser config (same shape as the streaming client's). */
  config?: ParserConfig;
  /** Tag-level / block-kind / component-tag overrides (see {@link Components}). */
  components?: Components;
  /** Appended to the root's `className` (the `flux-md` class is always present). */
  className?: string;
  /** Set on the root element. */
  id?: string;
  /** Set on the root element (e.g. `"article"`). */
  role?: string;
  /** Make the root a live region (parity with `<FluxMarkdown>` if you hydrate). */
  "aria-live"?: "off" | "polite" | "assertive";
  /** Live-region atomicity; pair with `aria-live`. */
  "aria-atomic"?: boolean;
}

/**
 * Synchronous, worker-free React rendering of finished markdown — a React Server
 * Component, or any one-shot SSR / static render. Emits the `flux-md` root +
 * per-block structure with the same `components` overrides (inline/block
 * component tags dispatch here too). Requires {@link initFlux} (or
 * {@link initFluxSync}) to have run. Uses no hooks (RSC-safe). A **render-once**
 * component: for live streaming, client-side code highlighting, or Mermaid use
 * the client `<FluxMarkdown>` instead (and if you SSR-then-hydrate, render the
 * *same* component on both sides).
 */
export function FluxMarkdownStatic({
  content,
  config,
  components,
  className,
  id,
  role,
  "aria-live": ariaLive,
  "aria-atomic": ariaAtomic,
}: FluxMarkdownStaticProps): ReactNode {
  const blocks = parseToBlocks(content, { config });
  const comps = components && Object.keys(components).length > 0 ? components : undefined;
  return createElement(
    "div",
    {
      className: className ? `flux-md ${className}` : "flux-md",
      id,
      role,
      "aria-live": ariaLive,
      "aria-atomic": ariaAtomic,
    },
    blocks.map((b) => renderStaticBlock(b, comps)),
  );
}

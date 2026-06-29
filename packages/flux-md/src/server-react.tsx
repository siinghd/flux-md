import { createElement, type ReactNode } from "react";
import { htmlToReact } from "./html-to-react";
import { blockKindProps } from "./react";
import { parseToBlocks } from "./server";
import type { Block, Components, ParserConfig } from "./types";

/**
 * React server / static rendering for flux-md — the **React-requiring** half of
 * `flux-md/server`, split into its own subpath (`flux-md/server/react`) so the
 * core `flux-md/server` entry (`initFlux` / `renderToString` / `parseToBlocks`)
 * stays importable with no `react` installed.
 *
 * Requires {@link initFlux} (or `initFluxSync`) from `flux-md/server` to have run
 * before rendering.
 *
 * ```tsx
 * import { initFlux } from "flux-md/server";
 * import { FluxMarkdownStatic } from "flux-md/server/react";
 * await initFlux();
 * // <FluxMarkdownStatic content={markdown} />  (RSC / SSR / static)
 * ```
 */

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
 * component tags dispatch here too). Requires `initFlux` (or `initFluxSync`)
 * from `flux-md/server` to have run. Uses no hooks (RSC-safe). A **render-once**
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

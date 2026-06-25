import type { FluxClient } from "./client";
import { highlight } from "./hi";
import type { Block, BlockComponentProps, BlockKindTag } from "./types-core";
import { blockProps, extractLang } from "./block-props";

/**
 * Framework-neutral DOM renderer for a {@link FluxClient}. Mounts the streaming
 * document into a container and keeps it in sync via direct DOM mutation,
 * mirroring the JSX renderer's block model: each block is keyed by its stable
 * parser-assigned id, and a committed block's node is reused untouched on every
 * later patch (the parity analogue of the JSX renderer's block memo). Only the
 * streaming tail is rebuilt.
 *
 * This is the foundation the Web Component / Vue / Svelte / Solid bindings
 * build on; it imports only neutral modules and carries no framework dependency.
 *
 * ## Custom components
 *
 * Pass `components` to override a whole block kind (or a component tag). Keys
 * are capitalized block-kind names (`CodeBlock`, `Table`, `Mermaid`…) or, for
 * `Component` blocks, the tag name (e.g. `Thinking`) with `Component` as the
 * generic fallback. A component receives {@link BlockComponentProps} and returns
 * an `HTMLElement` or an HTML string. There is no tag-level override path (no
 * `table`/`a`/`code` keys) — that requires an HTML→tree pass the DOM renderer
 * doesn't carry.
 */

export interface MountHandle {
  destroy(): void;
  refresh(): void;
  /**
   * The id of the streaming **tail** block — the one block that may re-render on
   * the next patch (a committed block's node is frozen, so its id never appears
   * here). Returns `null` when no block is open (idle / fully committed).
   *
   * Purely derived from the live snapshot; reading it renders nothing and mutates
   * nothing. It exists so a fine-grained framework binding (Solid `createMemo`,
   * Vue `computed`, Svelte `derived`) can narrow a reactive cell to *just the tail*
   * for its own scheduling/diagnostics — the DOM is already updated by the
   * renderer's own subscribe loop, so this never changes what is drawn.
   */
  openBlockId(): number | null;
}

export type DomBlockComponent = (props: BlockComponentProps) => HTMLElement | string;

/** Override map: capitalized block-kind / component-tag keys only. */
export type DomComponents = Record<string, DomBlockComponent>;

export interface MountOptions {
  components?: DomComponents;
  /**
   * Optional HTML sanitizer applied to every generic block's HTML before it is
   * injected via `innerHTML` — **including the streaming (open/speculative)
   * tail**. The built-in code/math/mermaid renderers operate on already-escaped
   * content and are not run through it (same as the JSX renderer). When omitted,
   * rendering is byte-identical and zero-cost.
   */
  sanitize?: (html: string) => string;
  /**
   * Skip layout/paint for off-screen *closed* blocks via CSS
   * `content-visibility: auto` (for very long documents). Off by default.
   */
  virtualize?: boolean;
  /**
   * Keep a bottom snap target so the view follows the streaming tail. CSS-only:
   * emits a sentinel with `scroll-snap-align: end`; you add
   * `scroll-snap-type: y proximity` to your scroll container. Off by default.
   */
  stickToBottom?: boolean;
  /** Use the built-in code highlighter. Default true; suppressed when a
   *  `components.CodeBlock` override is supplied. */
  highlightCode?: boolean;
  /** Coalesce patches into one DOM write per animation frame. Default true. */
  batch?: boolean;
  /** Appended to the root's `className` (the `flux-md` class is always present). */
  className?: string;
  /** Set on the root element. */
  id?: string;
  /** Set on the root element (e.g. `"article"`, `"log"`). */
  role?: string;
  /**
   * Make the root a live region so screen readers announce streamed content.
   * `"polite"` coalesces rapid updates (does not read every token). Off by default.
   */
  ariaLive?: "off" | "polite" | "assertive";
  /** Live-region atomicity; pair with `ariaLive`. Off by default. */
  ariaAtomic?: boolean;
}

// Per-kind off-screen size estimate for `contain-intrinsic-size`. Duplicated
// verbatim from the JSX renderer so per-kind virtualization sizes match.
const INTRINSIC_PX: Record<string, number> = {
  Paragraph: 80, Heading: 44, CodeBlock: 300, MathBlock: 140, Mermaid: 220,
  List: 120, Blockquote: 100, Alert: 120, Table: 200, Rule: 24, Html: 80,
  Component: 120,
};

// The fingerprint that decides whether a block's node may be reused: exactly
// what the JSX renderer's block memo checks, minus `id` (the map key).
interface MountedBlock {
  id: number;
  node: HTMLElement;
  html: string;
  open: boolean;
  speculative: boolean;
  kind: BlockKindTag;
}

export function mountFluxMarkdown(
  client: FluxClient,
  container: HTMLElement,
  options: MountOptions = {},
): MountHandle {
  if (typeof document === "undefined") {
    throw new Error("mountFluxMarkdown is browser-only; call it after the DOM exists.");
  }

  // Normalize "no overrides" to undefined so the fast path doesn't churn.
  const components =
    options.components && Object.keys(options.components).length > 0 ? options.components : undefined;
  const { sanitize, virtualize, stickToBottom } = options;
  const highlightCode = options.highlightCode !== false && !components?.CodeBlock;
  const batch = options.batch !== false && typeof requestAnimationFrame === "function";

  const root = document.createElement("div");
  root.className = options.className ? `flux-md ${options.className}` : "flux-md";
  if (options.id) root.id = options.id;
  if (options.role) root.setAttribute("role", options.role);
  if (options.ariaLive) root.setAttribute("aria-live", options.ariaLive);
  if (options.ariaAtomic !== undefined) root.setAttribute("aria-atomic", String(options.ariaAtomic));
  container.appendChild(root);

  // CSS-only stick-to-bottom: a permanent sentinel pinned as the last child.
  let anchor: HTMLElement | null = null;
  if (stickToBottom) {
    anchor = document.createElement("div");
    anchor.className = "flux-bottom-anchor";
    anchor.setAttribute("aria-hidden", "true");
    anchor.style.scrollSnapAlign = "end";
    root.appendChild(anchor);
  }

  const mounted = new Map<number, MountedBlock>();
  let order: number[] = [];
  let dead = false;
  let frame = 0;

  function sync(): void {
    if (dead) return;
    const snapshot = client.getSnapshot();
    const nextOrder: number[] = new Array(snapshot.length);
    const seen = new Set<number>();

    for (let i = 0; i < snapshot.length; i++) {
      const b = snapshot[i];
      nextOrder[i] = b.id;
      seen.add(b.id);
      const existing = mounted.get(b.id);
      if (!existing) {
        const node = renderBlock(b);
        mounted.set(b.id, {
          id: b.id, node, html: b.html, open: b.open, speculative: b.speculative, kind: b.kind.type,
        });
        continue;
      }
      // Unchanged fingerprint → reuse the node untouched. Committed blocks land
      // here forever: their node is never recreated, so any one-shot work
      // (highlight, copy listener) runs exactly once. This is the whole point.
      if (existing.html === b.html && existing.open === b.open && existing.speculative === b.speculative) {
        continue;
      }
      // Changed → rebuild and swap in place.
      const node = renderBlock(b);
      existing.node.replaceWith(node);
      existing.node = node;
      existing.html = b.html;
      existing.open = b.open;
      existing.speculative = b.speculative;
      existing.kind = b.kind.type;
    }

    // Drop ids no longer present (reset() empties the snapshot; a speculative
    // revision can drop a tail block).
    if (mounted.size > seen.size) {
      for (const [id, mb] of mounted) {
        if (!seen.has(id)) {
          mb.node.remove();
          mounted.delete(id);
        }
      }
    }

    order = nextOrder;
    reconcileChildren();
  }

  // Keyed reconcile with a single forward cursor (O(n), not O(n²)): walk the
  // desired order and the live children in lockstep, inserting/moving only a
  // node whose live position differs. The `.flux-bottom-anchor` is never part of
  // `order`, so it acts as the end-of-list marker — blocks always land before
  // it, keeping it pinned last. The common streaming case touches 1–2 tail nodes.
  function reconcileChildren(): void {
    let cursor = root.firstChild;
    for (let i = 0; i < order.length; i++) {
      const mb = mounted.get(order[i]);
      if (!mb) continue;
      const want = mb.node;
      if (cursor === want) {
        cursor = want.nextSibling; // already in place; advance
        continue;
      }
      // Out of place: move `want` before the cursor. When an anchor exists the
      // cursor never advances past it (the anchor is never a `want`), so blocks
      // always land before it; without one, a tail cursor of `null` appends.
      root.insertBefore(want, cursor);
    }
  }

  function renderBlock(b: Block): HTMLElement {
    const content = renderBlockContent(b);
    // Virtualize only *closed* blocks. Unlike the JSX renderer (which wraps in
    // an extra div) the DOM renderer sets the properties on the block node
    // directly — one of the documented byte-faithfulness divergences.
    if (virtualize && !b.open && !b.speculative) {
      const px = INTRINSIC_PX[b.kind.type] ?? 120;
      content.style.contentVisibility = "auto";
      content.style.containIntrinsicSize = `auto ${px}px`;
    }
    return content;
  }

  function renderBlockContent(b: Block): HTMLElement {
    const kind = b.kind.type;

    // 1. Block-kind override (a Component block dispatches on its tag first).
    if (components) {
      if (kind === "Component") {
        const tag = (b.kind.data as { tag?: string } | undefined)?.tag;
        const override = (tag && components[tag]) || components.Component;
        if (override) return wrapOverrideResult(override(blockProps(b)));
      }
      const blockOverride = components[kind];
      if (blockOverride) return wrapOverrideResult(blockOverride(blockProps(b)));
    }

    // 2. Dedicated default renderers.
    switch (kind) {
      case "CodeBlock":
        if (highlightCode) return renderCodeBlock(b);
        break; // fall through to the generic path
      case "MathBlock":
        return renderMathBlock(b);
      case "Mermaid":
        return renderMermaid(b);
    }

    // 3. Generic fast path.
    const node = document.createElement("div");
    node.className =
      "flux-block flux-block-" +
      kind.toLowerCase() +
      (b.open ? " flux-open" : "") +
      (b.speculative ? " flux-speculative" : "");
    node.innerHTML = sanitize ? sanitize(b.html) : b.html;
    return node;
  }

  // An override may return an element (used directly) or an HTML string (wrapped
  // in a div so the renderer always owns a single block node to track/swap).
  function wrapOverrideResult(result: HTMLElement | string): HTMLElement {
    if (typeof result === "string") {
      const node = document.createElement("div");
      node.innerHTML = result;
      return node;
    }
    return result;
  }

  function renderCodeBlock(b: Block): HTMLElement {
    const lang = extractLang(b.html) || "text";
    // Mirror CodeBlock.tsx: text is "" while open, so the body falls to the raw
    // `<div>` path; a closed block decodes once and highlights once. The node is
    // frozen once closed, so highlight runs exactly once (no re-tokenize).
    const text = b.open ? "" : decodeCodeText(b.html);
    const highlighted = text ? highlight(text, lang) : null;

    const block = document.createElement("div");
    block.className = "flux-code-block" + (b.open ? " flux-streaming" : "");

    const header = document.createElement("div");
    header.className = "flux-code-header";
    const langSpan = document.createElement("span");
    langSpan.className = "flux-code-lang";
    langSpan.textContent = lang;
    header.appendChild(langSpan);

    if (b.open) {
      const pill = document.createElement("span");
      pill.className = "flux-code-streaming-pill";
      pill.textContent = "streaming";
      header.appendChild(pill);
    } else {
      header.appendChild(makeCopyButton(text));
    }
    block.appendChild(header);

    const body = document.createElement("div");
    body.className = "flux-code-body";
    if (highlighted) {
      const pre = document.createElement("pre");
      pre.tabIndex = 0;
      pre.setAttribute("role", "region");
      pre.setAttribute("aria-label", `${lang} code`);
      const code = document.createElement("code");
      code.innerHTML = highlighted;
      pre.appendChild(code);
      body.appendChild(pre);
    } else {
      const div = document.createElement("div");
      div.tabIndex = 0;
      div.setAttribute("role", "region");
      div.setAttribute("aria-label", `${lang} code`);
      div.innerHTML = b.html;
      body.appendChild(div);
    }
    block.appendChild(body);
    return block;
  }

  function renderMathBlock(b: Block): HTMLElement {
    const block = document.createElement("div");
    block.className = "flux-math-block" + (b.open ? " flux-streaming" : "");
    const header = document.createElement("div");
    header.className = "flux-math-header";
    const lang = document.createElement("span");
    lang.className = "flux-math-lang";
    lang.textContent = "math";
    header.appendChild(lang);
    if (b.open) header.appendChild(streamingPill());
    block.appendChild(header);
    const body = document.createElement("div");
    body.className = "flux-math-body";
    body.innerHTML = b.html;
    block.appendChild(body);
    return block;
  }

  function renderMermaid(b: Block): HTMLElement {
    const block = document.createElement("div");
    block.className = "flux-mermaid-block" + (b.open ? " flux-streaming" : "");
    const header = document.createElement("div");
    header.className = "flux-mermaid-header";
    const lang = document.createElement("span");
    lang.className = "flux-mermaid-lang";
    lang.textContent = "mermaid";
    header.appendChild(lang);
    if (b.open) header.appendChild(streamingPill());
    block.appendChild(header);
    const body = document.createElement("div");
    body.className = "flux-mermaid-body";
    body.innerHTML = b.html;
    block.appendChild(body);
    return block;
  }

  function streamingPill(): HTMLElement {
    const pill = document.createElement("span");
    pill.className = "flux-code-streaming-pill";
    pill.textContent = "streaming";
    return pill;
  }

  // SVG markup uses the live-DOM attribute form (hyphenated, e.g. stroke-width).
  const COPY_ICON =
    '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="9" y="9" width="11" height="11" rx="2"></rect><path d="M5 15V5a2 2 0 0 1 2-2h10"></path></svg><span>Copy</span>';
  const COPIED_ICON =
    '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 6 9 17l-5-5"></path></svg><span>Copied</span>';

  function makeCopyButton(text: string): HTMLElement {
    const btn = document.createElement("button");
    btn.type = "button";
    btn.className = "flux-code-copy";
    btn.setAttribute("aria-label", "Copy code");
    btn.setAttribute("aria-live", "polite");
    btn.innerHTML = COPY_ICON;
    // The listener lives as long as the node. A closed block's node is never
    // recreated (frozen fingerprint), so there is no per-patch rebind; it is
    // GC'd when `root` is removed.
    let timer: ReturnType<typeof setTimeout> | null = null;
    btn.addEventListener("click", () => {
      const clip = typeof navigator !== "undefined" ? navigator.clipboard : undefined;
      if (!clip || !clip.writeText || !text) return;
      clip.writeText(text).then(
        () => {
          btn.setAttribute("aria-label", "Copied");
          btn.innerHTML = COPIED_ICON;
          if (timer !== null) clearTimeout(timer);
          timer = setTimeout(() => {
            btn.setAttribute("aria-label", "Copy code");
            btn.innerHTML = COPY_ICON;
          }, 1500);
        },
        // Permission denied / blocked: stay silent, leave button usable.
        () => {},
      );
    });
    return btn;
  }

  const unsubscribe = client.subscribe(() => {
    if (dead) return;
    if (batch) {
      if (frame === 0) frame = requestAnimationFrame(flush);
    } else {
      sync();
    }
  });

  function flush(): void {
    frame = 0;
    sync();
  }

  // Initial render from whatever is already in the snapshot.
  sync();

  return {
    destroy() {
      if (dead) return;
      dead = true;
      if (frame !== 0) {
        cancelAnimationFrame(frame);
        frame = 0;
      }
      unsubscribe();
      // The caller owns the worker/stream — never call client.destroy() here
      // (same contract as the JSX renderer: unmounting never destroys the client).
      root.remove();
    },
    refresh() {
      if (dead) return;
      sync();
    },
    openBlockId() {
      return tailOpenBlockId(client.getSnapshot());
    },
  };
}

/**
 * Derive the streaming tail's block id from an ordered snapshot: the id of the
 * last block when it is open, else `null`. The open block is always the tail by
 * construction (the parser only keeps the final block speculative/open), so this
 * is an O(1) read of the last element — no scan. Shared so the framework
 * adapters expose the same "what may re-render next" signal as the DOM handle.
 */
export function tailOpenBlockId(snapshot: readonly Block[]): number | null {
  const tail = snapshot.length > 0 ? snapshot[snapshot.length - 1] : undefined;
  return tail && tail.open ? tail.id : null;
}

// Local copy of the canonical code-text decoder (kept here so dom.ts depends
// only on neutral modules; block-props.ts keeps its own private copy too).
function decodeCodeText(html: string): string {
  const m = html.match(/<pre><code[^>]*>([\s\S]*?)<\/code><\/pre>/);
  if (!m) return "";
  return m[1]
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, "&");
}

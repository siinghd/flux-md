import type { FluxClient } from "./client";
import { highlight } from "./hi";
import { morph } from "./morph";
import type { Align, Block, BlockComponentProps, BlockKindTag, ListData, RenderMetricsHook, TableData } from "./types-core";
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
  /**
   * Opt-in (default false). When a generic open/streaming block grows, morph its
   * existing DOM subtree **in place** toward the new HTML instead of rebuilding
   * the whole node with `innerHTML`. The browser then only repaints/relayouts
   * the parts that changed, and focus/text-selection inside the streaming tail
   * survive a token append. The default path (full rebuild) is byte-identical
   * and unchanged; this only affects generic blocks rendered via the `innerHTML`
   * fast path (not code/math/mermaid/component overrides). The morphed subtree is
   * equivalent to the rebuilt one. */
  morphOpenBlocks?: boolean;
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
  /**
   * Optional render-churn probe. Fires once per ACTUAL node build/rebuild of a
   * block — never for a committed block whose node is reused untouched on a
   * tail-only patch. The callback gets the block id and a {@link RenderMetrics}
   * sample (per-block `renderCount`/rebuild count, `speculativeToggleCount`,
   * `lastRenderMs`, `kind`). Zero overhead when omitted, and advances
   * `client.getMetrics().rebuildCount`.
   */
  onRenderMetrics?: RenderMetricsHook;
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
  // Render-churn probe state (only maintained when an onRenderMetrics hook is
  // wired; otherwise these stay at their initial values and are never read).
  renderCount: number;
  toggleCount: number;
  // Set only for an OPEN table rendered via the keyed-tbody path (blockData on).
  // Lets a later patch update just the growing trailing row in place instead of
  // rebuilding the whole node.
  table?: KeyedTable;
  // True when `node` is the generic `<div class="flux-block…">` whose entire
  // `innerHTML` is exactly `html` (no special wrapper, no sanitizer transform).
  // Only such a node is eligible for the prefix-extension tail-append fast path.
  generic: boolean;
}

// Incremental keyed-tbody state for one OPEN table. `<tr>` nodes for committed
// rows are appended once and never rebuilt; only the last (open) row's cells are
// re-set each patch — never a whole-`<tbody>` rebuild.
interface KeyedTable {
  table: HTMLTableElement;
  tbody: HTMLTableSectionElement | null;
  scope: boolean;
  // Number of LEADING rows whose `<tr>` is frozen in the DOM (built once and
  // never touched again). The last data row is OPEN — re-rendered each patch —
  // so after every update `committed === rows.length - 1`.
  committed: number;
  // The current open trailing `<tr>` (re-rendered each patch); replaced in place.
  lastRow: HTMLTableRowElement | null;
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
  const { sanitize, virtualize, stickToBottom, onRenderMetrics } = options;
  const hasPerf = typeof performance !== "undefined";
  const highlightCode = options.highlightCode !== false && !components?.CodeBlock;
  const batch = options.batch !== false && typeof requestAnimationFrame === "function";
  const morphOpenBlocks = options.morphOpenBlocks === true;

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
  // Set by `renderBlockContent` for the call in flight: true only when it took
  // the generic `<div class="flux-block…">innerHTML=html` path (no override, no
  // dedicated renderer). Read immediately after the render to tag the mount.
  let lastRenderGeneric = false;

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
        const t0 = onRenderMetrics && hasPerf ? performance.now() : 0;
        const mb: MountedBlock = {
          id: b.id, node: undefined as unknown as HTMLElement,
          html: b.html, open: b.open, speculative: b.speculative, kind: b.kind.type,
          renderCount: 0, toggleCount: 0, generic: false,
        };
        mb.node = renderBlock(b, mb);
        mb.generic = lastRenderGeneric;
        mounted.set(b.id, mb);
        if (onRenderMetrics) noteRender(mb, b, t0);
        continue;
      }
      // Unchanged fingerprint → reuse the node untouched. Committed blocks land
      // here forever: their node is never recreated, so any one-shot work
      // (highlight, copy listener) runs exactly once. This is the whole point.
      if (existing.html === b.html && existing.open === b.open && existing.speculative === b.speculative) {
        continue;
      }
      const t0 = onRenderMetrics && hasPerf ? performance.now() : 0;
      // Keyed-table fast path: an OPEN table that already mounted via the keyed
      // tbody updates only its growing trailing row in place — committed `<tr>`
      // nodes are never rebuilt. Reuses the same block node (no replaceWith).
      // This is still a render of the node, so it feeds the render-churn probe.
      if (existing.table && b.open && b.kind.type === "Table") {
        const data = tableData(b);
        if (data) {
          syncTbody(existing.table, data);
          if (onRenderMetrics) noteRender(existing, b, t0);
          // Keep the wrapper's speculative class in sync (parity with the
          // full-rebuild path) without recreating the node.
          if (existing.speculative !== b.speculative) {
            existing.node.classList.toggle("flux-speculative", b.speculative);
          }
          existing.html = b.html;
          existing.open = b.open;
          existing.speculative = b.speculative;
          continue;
        }
      }
      // Opt-in morph fast path: an open generic block that only grew its HTML
      // (same kind, still routed through the innerHTML path) is morphed in place,
      // preserving the node's identity, focus, and selection. Falls through to a
      // full rebuild for anything not eligible (commit transition, kind change,
      // code/math/mermaid/override blocks). This is still a render of the node, so
      // it feeds the render-churn probe.
      if (
        morphOpenBlocks &&
        b.open &&
        existing.open &&
        existing.generic &&
        existing.kind === b.kind.type &&
        usesGenericPath(b)
      ) {
        morph(existing.node, sanitize ? sanitize(b.html) : b.html);
        if (onRenderMetrics) noteRender(existing, b, t0);
        existing.html = b.html;
        existing.speculative = b.speculative;
        existing.node.className = genericClassName(b);
        // The node stays the generic innerHTML mirror, so it remains eligible for
        // the prefix-append / morph fast paths on later patches.
        existing.generic = !sanitize;
        continue;
      }
      // Prefix-extension tail-append fast path (generic blocks only, no
      // sanitizer). When the new html merely *appends* one or more WHOLE
      // top-level sibling elements to the old html, we can splice the suffix
      // onto the live node instead of rebuilding the whole subtree. The result
      // is byte-identical to a full rebuild because the appended suffix is
      // self-contained markup that begins a new depth-0 sibling — the browser
      // parses it the same way whether appended or rendered whole. This is still
      // a render of the node, so it feeds the render-churn probe.
      if (
        !sanitize &&
        existing.generic &&
        existing.kind === b.kind.type &&
        existing.open === b.open &&
        existing.speculative === b.speculative &&
        b.html.length > existing.html.length &&
        b.html.startsWith(existing.html) &&
        isDepth0Boundary(existing.html, b.html)
      ) {
        existing.node.insertAdjacentHTML("beforeend", b.html.slice(existing.html.length));
        if (onRenderMetrics) noteRender(existing, b, t0);
        existing.html = b.html;
        continue;
      }
      // Changed → rebuild and swap in place. A table that just closed (or whose
      // data vanished) drops its keyed manager and re-renders the full HTML once.
      existing.table = undefined;
      const node = renderBlock(b, existing);
      existing.node.replaceWith(node);
      existing.node = node;
      if (onRenderMetrics) noteRender(existing, b, t0);
      existing.html = b.html;
      existing.open = b.open;
      existing.speculative = b.speculative;
      existing.kind = b.kind.type;
      existing.generic = lastRenderGeneric;
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

  // Fire the render-churn probe for one actual node build/rebuild. `mb` carries
  // the PRE-update fingerprint (its `speculative` is the prior value) so the
  // toggle count is correct; the caller updates the fingerprint afterward. Only
  // called when an onRenderMetrics hook is wired, so it stays zero-cost off.
  function noteRender(mb: MountedBlock, b: Block, t0: number): void {
    mb.renderCount++;
    if (mb.speculative !== b.speculative) mb.toggleCount++;
    client.__noteRebuild();
    onRenderMetrics!(b.id, {
      renderCount: mb.renderCount,
      speculativeToggleCount: mb.toggleCount,
      lastRenderMs: hasPerf ? performance.now() - t0 : 0,
      kind: b.kind.type,
    });
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

  function renderBlock(b: Block, mb: MountedBlock): HTMLElement {
    const content = renderBlockContent(b, mb);
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

  function renderBlockContent(b: Block, mb: MountedBlock): HTMLElement {
    const kind = b.kind.type;
    lastRenderGeneric = false;

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

    // 2b. Keyed-table path for the streaming tail: an OPEN table with `blockData`
    // renders a real `<table>` whose committed `<tr>` nodes are appended once and
    // frozen, so a later patch updates only the growing trailing row. Closed
    // tables (and blockData-off tables) take the generic full-HTML path below
    // (closed nodes are frozen by the fingerprint check, already free).
    if (kind === "Table" && b.open) {
      const data = tableData(b);
      if (data) return buildKeyedTable(b, data, mb);
    }

    // 2c. Keyed list renderer (opt-in: only when `blockData` is on, so
    // `kind.data.items` carries per-item inner HTML). For an OPEN list, stamp one
    // `<li>` per item — each item's inner HTML routed through the SAME sanitize
    // path the generic innerHTML branch uses — so the rebuilt list tracks the
    // structured items instead of re-parsing the whole `<ul>`/`<ol>` HTML. Closed
    // lists fall through (their node is reused untouched, never rebuilt).
    if (b.open && kind === "List") {
      const keyed = renderKeyedList(b);
      if (keyed) return keyed;
    }

    // 3. Generic fast path.
    const node = document.createElement("div");
    node.className = genericClassName(b);
    // Streaming-tail keyed path: an OPEN Blockquote / Alert with structured
    // `nested` data (blockData on) builds its wrapper with one child node per
    // inner sub-block instead of a single full-wrapper `innerHTML`. Each child's
    // `html` is the SAME safe-allowlist-serialized fragment as the corresponding
    // slice of `b.html` (no new innerHTML hole). A `sanitize` hook disables it
    // (it must run over the full wrapper string). Closed blocks fall through —
    // their node fingerprint is stable, so they are never rebuilt anyway.
    if (b.open && !sanitize && (kind === "Blockquote" || kind === "Alert")) {
      const wrapper = renderKeyedContainer(b);
      if (wrapper) {
        node.appendChild(wrapper);
        return node;
      }
    }
    node.innerHTML = sanitize ? sanitize(b.html) : b.html;
    // Eligible for the prefix-append fast path only when no sanitizer rewrote
    // the html (the stored `html` must equal the node's actual innerHTML source).
    lastRenderGeneric = !sanitize;
    return node;
  }

  // Build a `<div class="flux-block flux-block-list flux-open …"><ul|ol>…</ul|ol>`
  // node from the structured `kind.data.items`, one `<li>` per item with its inner
  // HTML sanitized via the shared `sanitize` path. Returns `null` when the items
  // channel is absent (blockData off) so the caller falls back to opaque HTML.
  function renderKeyedList(b: Block): HTMLElement | null {
    const ld = b.kind.data as ListData | undefined;
    const items = ld?.items;
    if (!items || items.length === 0) return null;
    const node = document.createElement("div");
    node.className =
      "flux-block flux-block-list" +
      (b.open ? " flux-open" : "") +
      (b.speculative ? " flux-speculative" : "");
    const list = document.createElement(ld?.ordered ? "ol" : "ul");
    if (ld?.ordered && ld.start !== undefined && ld.start !== 1) {
      list.setAttribute("start", String(ld.start));
    }
    for (const it of items) {
      const li = document.createElement("li");
      li.innerHTML = sanitize ? sanitize(it.html) : it.html;
      list.appendChild(li);
    }
    node.appendChild(list);
    return node;
  }

  // Build a Blockquote / Alert wrapper with KEYED inner sub-block nodes from the
  // structured `nested` channel. The wrapper element + its attributes (`dir`/
  // `class`/`data-alert`/`role`) come from `b.html`'s opening tag so the streamed
  // wrapper is byte-faithful; the alert title `<p>` is kept as the first child
  // (it is the wrapper, not a body block). Returns null when `nested` is absent.
  function renderKeyedContainer(b: Block): HTMLElement | null {
    const nested = (b.kind.data as { nested?: { html: string }[] } | undefined)?.nested;
    if (!Array.isArray(nested)) return null;
    const tagName = b.kind.type === "Alert" ? "div" : "blockquote";
    const wrapper = document.createElement(tagName);
    applyOpenTagAttrs(wrapper, b.html);
    if (b.kind.type === "Alert") {
      const title = alertTitleHtml(b.html);
      if (title) {
        const t = document.createElement("div");
        t.innerHTML = title;
        const titleNode = t.firstElementChild;
        if (titleNode) wrapper.appendChild(titleNode);
      }
    }
    for (let i = 0; i < nested.length; i++) {
      const child = document.createElement("div");
      child.innerHTML = nested[i].html;
      const inner = child.firstElementChild;
      // A nested block is a single root element (`<p>…</p>`, `<ul>…</ul>`, …);
      // unwrap the temp `<div>` so the wrapper holds the real element directly.
      wrapper.appendChild(inner ?? child);
    }
    return wrapper;
  }

  // Build the initial keyed table node + manager. The `<thead>` and all-but-last
  // `<tr>` are emitted once; the manager remembers the committed row count so a
  // later patch (via syncTbody) only re-renders the open trailing row.
  function buildKeyedTable(b: Block, data: TableData, mb: MountedBlock): HTMLElement {
    const node = document.createElement("div");
    node.className = "flux-block flux-block-table flux-open" + (b.speculative ? " flux-speculative" : "");
    const table = document.createElement("table");
    if (b.html.startsWith('<table dir="auto"')) table.setAttribute("dir", "auto");
    const scope = b.html.includes('<th scope="col"');

    const thead = document.createElement("thead");
    const htr = document.createElement("tr");
    for (let j = 0; j < data.headers.length; j++) {
      htr.appendChild(makeCell("th", data.headers[j].html, data.aligns[j] ?? null, scope));
    }
    thead.appendChild(htr);
    table.appendChild(thead);

    const km: KeyedTable = { table, tbody: null, scope, committed: 0, lastRow: null };
    mb.table = km;
    node.appendChild(table);
    syncTbody(km, data);
    return node;
  }

  // Append any newly-committed rows once, then (re)render only the open trailing
  // row. Shared by build (committed===0) and update. The whole `<tbody>` is never
  // rebuilt — committed `<tr>` nodes keep their identity across patches.
  function syncTbody(km: KeyedTable, data: TableData): void {
    const n = data.rows.length;
    if (n === 0) {
      // No body rows yet (header-only streamed table). Tear down any stale tbody.
      if (km.tbody) {
        km.tbody.remove();
        km.tbody = null;
      }
      km.committed = 0;
      km.lastRow = null;
      return;
    }
    if (!km.tbody) {
      km.tbody = document.createElement("tbody");
      km.table.appendChild(km.tbody);
    }
    const tbody = km.tbody;
    // The prior open trailing row is now superseded — drop it before freezing the
    // rows that have since committed and rendering the new trailing row.
    if (km.lastRow) {
      km.lastRow.remove();
      km.lastRow = null;
    }
    // Freeze every row from the first uncommitted up to (but not including) the
    // last: append its `<tr>` once and never touch it again (committed cell html
    // is byte-stable).
    for (let i = km.committed; i < n - 1; i++) {
      tbody.appendChild(makeRow(data.rows[i], data.aligns));
    }
    km.committed = n - 1;
    // Render the still-OPEN last row and remember it so the next patch replaces it.
    const last = makeRow(data.rows[n - 1], data.aligns);
    tbody.appendChild(last);
    km.lastRow = last;
  }

  function makeRow(cells: TableData["rows"][number], aligns: Align[]): HTMLTableRowElement {
    const tr = document.createElement("tr");
    for (let j = 0; j < cells.length; j++) {
      tr.appendChild(makeCell("td", cells[j].html, aligns[j] ?? null, false));
    }
    return tr;
  }

  function makeCell(tag: "th" | "td", html: string, align: Align, scope: boolean): HTMLElement {
    const cell = document.createElement(tag);
    if (tag === "th" && scope) cell.setAttribute("scope", "col");
    if (align) cell.style.textAlign = align;
    // Route cell html through the same sanitize path the generic block uses.
    cell.innerHTML = sanitize ? sanitize(html) : html;
    return cell;
  }

  // The class string for a generic-path block node. Shared by the initial
  // render and the in-place morph branch so a morphed node keeps the exact
  // class string (e.g. dropping `flux-speculative`) a rebuild would have set.
  function genericClassName(b: Block): string {
    return (
      "flux-block flux-block-" +
      b.kind.type.toLowerCase() +
      (b.open ? " flux-open" : "") +
      (b.speculative ? " flux-speculative" : "")
    );
  }

  // True when a block renders through the generic `innerHTML` fast path — the
  // only path the in-place morph applies to. Mirrors the dispatch order in
  // renderBlockContent: an override (block-kind or Component tag) or a dedicated
  // renderer (highlighted code / math / mermaid) all opt OUT of morphing.
  function usesGenericPath(b: Block): boolean {
    const kind = b.kind.type;
    if (components) {
      if (kind === "Component") {
        const tag = (b.kind.data as { tag?: string } | undefined)?.tag;
        if ((tag && components[tag]) || components.Component) return false;
      }
      if (components[kind]) return false;
    }
    if (kind === "CodeBlock") return !highlightCode;
    if (kind === "MathBlock" || kind === "Mermaid") return false;
    return true;
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

// The structured `TableData` (opt-in `blockData`) on a Table block, or
// `undefined` when the flag is off (the keyed path then falls back to full HTML).
function tableData(b: Block): TableData | undefined {
  if (b.kind.type !== "Table") return undefined;
  const data = b.kind.data as TableData | undefined;
  return data && Array.isArray(data.rows) ? data : undefined;
}

// HTML void elements: they self-terminate, so they never push element depth.
const VOID_ELEMENTS = new Set([
  "area", "base", "br", "col", "embed", "hr", "img", "input", "link",
  "meta", "param", "source", "track", "wbr",
]);

/**
 * True when `prefix` is a complete run of balanced top-level markup (element
 * depth returns to 0 at its end and any trailing whitespace/text is harmless)
 * AND the appended suffix `full.slice(prefix.length)` begins a NEW depth-0
 * sibling element (an opening tag, not a close tag / text / mid-tag).
 *
 * When both hold, splicing the suffix onto the live node via
 * `insertAdjacentHTML('beforeend', suffix)` yields the exact same DOM the
 * browser would build from parsing `full` whole — the appended markup is a
 * self-contained sibling appended after the last existing child. Any other
 * shape (an unclosed element at the prefix boundary, a suffix that continues
 * text, closes a tag, or splits a tag) must fall back to a full rebuild.
 *
 * The scan is single-pass over `prefix` (O(prefix length)); it is run only on a
 * confirmed `startsWith` prefix extension, so the amortized streaming cost stays
 * proportional to the bytes seen.
 */
function isDepth0Boundary(prefix: string, full: string): boolean {
  // Suffix must open a new element: '<' immediately followed by an ASCII letter.
  const c0 = full.charCodeAt(prefix.length);
  if (c0 !== 60 /* '<' */) return false;
  const c1 = full.charCodeAt(prefix.length + 1);
  const isLetter = (c1 >= 65 && c1 <= 90) || (c1 >= 97 && c1 <= 122);
  if (!isLetter) return false;

  // Walk `prefix`, tracking element depth. Bail (return false) on anything we
  // cannot cheaply prove balanced: comments, CDATA, processing instructions,
  // or any tag that leaves the cursor inside markup at the end.
  let depth = 0;
  let i = 0;
  const n = prefix.length;
  while (i < n) {
    const lt = prefix.indexOf("<", i);
    if (lt === -1) break; // only text remains; depth unchanged
    i = lt + 1;
    if (i >= n) return false; // trailing '<' with nothing after → mid-tag
    const ch = prefix.charCodeAt(i);
    // Comments / CDATA / declarations / PIs: not handled — fall back.
    if (ch === 33 /* '!' */ || ch === 63 /* '?' */) return false;
    let closing = false;
    if (ch === 47 /* '/' */) {
      closing = true;
      i++;
    }
    // Read the tag name.
    const nameStart = i;
    while (i < n) {
      const t = prefix.charCodeAt(i);
      const nameChar =
        (t >= 65 && t <= 90) || (t >= 97 && t <= 122) || (t >= 48 && t <= 57) || t === 45;
      if (!nameChar) break;
      i++;
    }
    if (i === nameStart) return false; // '<' not followed by a tag name
    const name = prefix.slice(nameStart, i).toLowerCase();
    // Find the tag's '>' (attribute values here never contain a literal '>'
    // because the renderer emits entity-escaped attributes; if we hit EOF first
    // the prefix ends mid-tag → not a boundary).
    const gt = prefix.indexOf(">", i);
    if (gt === -1) return false;
    const selfClosing = prefix.charCodeAt(gt - 1) === 47; /* '/' */
    i = gt + 1;
    if (closing) {
      depth--;
      if (depth < 0) return false; // unbalanced close
    } else if (!selfClosing && !VOID_ELEMENTS.has(name)) {
      depth++;
    }
  }
  return depth === 0;
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

// Attributes the Rust renderer emits on a blockquote / alert wrapper open tag
// (`dir`/`class`/`data-alert`/`role`). Whitelisted (not a generic HTML parser):
// only these names are forwarded onto the keyed wrapper element so it is
// byte-faithful to the full-wrapper innerHTML path.
const CONTAINER_ATTR_RE = /([a-zA-Z][a-zA-Z0-9-]*)="([^"]*)"/g;
function applyOpenTagAttrs(el: HTMLElement, html: string): void {
  const gt = html.indexOf(">");
  const open = gt < 0 ? html : html.slice(0, gt);
  let m: RegExpExecArray | null;
  CONTAINER_ATTR_RE.lastIndex = 0;
  while ((m = CONTAINER_ATTR_RE.exec(open))) {
    const name = m[1].toLowerCase();
    if (name === "class" || name === "dir" || name === "role" || name.startsWith("data-")) {
      el.setAttribute(name, m[2]);
    }
  }
}

// Extract an alert's title `<p class="markdown-alert-title"…>Title</p>` from the
// wrapper HTML so the keyed path keeps it as the first child (never in `nested`).
function alertTitleHtml(html: string): string {
  const m = html.match(/<p class="markdown-alert-title"[^>]*>[\s\S]*?<\/p>/);
  return m ? m[0] : "";
}

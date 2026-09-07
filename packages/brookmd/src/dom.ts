import type { BrookClient } from "./client";
import { highlightDeferred, type DeferredHighlight } from "./hi-defer";
import { createInc, incHighlight, incSeed, type IncState } from "./hi-inc";
import { morph } from "./morph";
import { incView, newIncCode, paintIncCode, spliceHtml, spliceKeep, type IncCode, type TailMode } from "./splice";
import type { Align, Block, BlockComponentProps, BlockKindTag, ContainerData, Decorator, LinkClickInfo, ListData, RenderMetricsHook, TableData, UrlTransform } from "./types-core";
import { blockProps, extractLang } from "./block-props";
import { decorateSegments } from "./decorate";
import { safeUrl } from "./url-safety";

/**
 * Framework-neutral DOM renderer for a {@link BrookClient}. Mounts the streaming
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

export type { LinkClickInfo };

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
  /**
   * Highlight a code fence **while it is still streaming**, instead of showing
   * plain escaped text until it closes. On by default (parity with the React
   * renderer's `streamingHighlight` prop).
   *
   * An open block keeps a frozen prefix and re-tokenizes only its tail on each
   * patch, so this stays linear in the block's size. The settled markup is
   * byte-identical either way — only the tail's colours are provisional, and
   * they may shift as bytes arrive.
   *
   * - `true` / omitted — `"wavefront"`.
   * - `"wavefront"` — the frozen prefix is coloured; the speculative tail (in
   *   practice the line being typed) renders as plain text until its line
   *   completes. The tail is one text node updated through its character data,
   *   which is what keeps the option's cost at the DOM near zero.
   * - `"eager"` — colour the tail on every patch too, by rebuilding its span
   *   markup each time. Sub-line colour latency, ~all of the option's
   *   style/layout cost.
   * - `false` — the pre-0.27 behaviour: plain body until the fence closes.
   */
  streamingHighlight?: boolean | "wavefront" | "eager";
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
  /**
   * Wrap or replace matched inline **text** while streaming (parity with the
   * React `decorators` prop). Each {@link Decorator} runs POST-render over the
   * block's real TEXT nodes via a `TreeWalker` (after `innerHTML`), once per
   * committed block, honoring `skipInside` (default `a`/`code`/`pre`/`kbd`).
   *
   * **Trusted surface.** A decorator's `replace` may return a `Node` or a string;
   * a returned Node is inserted as-is and is NOT sanitized (a `javascript:` href
   * on a user-built `<a>` reaches the DOM). Route hrefs through the exported
   * {@link safeUrl}. Enabling decorators moves a block onto the walk path (off the
   * `innerHTML`/prefix-append/morph fast paths) — still O(n) per block.
   */
  decorators?: Decorator[];
  /**
   * Rewrite `href`/`src`/`poster` URLs as blocks render (parity with the React
   * `urlTransform` prop). The output is re-sanitized
   * (`safeUrl(urlTransform(safeUrl(v)))`) so it can never introduce a dangerous
   * scheme. O(1) per attribute.
   */
  urlTransform?: UrlTransform;
  /**
   * Called when a rendered link is clicked (parity with the React `onLinkClick`
   * prop). Exactly ONE `click` listener is added to the renderer root and the
   * anchor is resolved from the event target — never a listener per anchor, so
   * this costs nothing per block and the streaming path is untouched. The
   * listener is removed by {@link MountHandle.destroy}.
   *
   * `event.preventDefault()` cancels the navigation. A still-streaming anchor
   * (`data-brook-pending`, no href yet) is never reported.
   */
  onLinkClick?: (event: MouseEvent, link: LinkClickInfo) => void;
  /** Appended to the root's `className` (the `brook-md` class is always present). */
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
  /**
   * @internal TEST-ONLY. Turn off the incremental apply fast paths — the open
   * code block's frozen/tail mirror and the delta-driven child splice — so every
   * changed open block rebuilds its whole node, exactly as it did before those
   * existed. The DOM-parity fuzz mounts one renderer with this on and one with it
   * off over the same patch stream and asserts their `innerHTML` matches after
   * every sync: correctness must never depend on a fast path firing. Not part of
   * the supported API and never useful in an app.
   */
  __fullRebuild?: boolean;
}

// How often the keyed list / container syncs were tried and how often they
// proved out. Same pattern (and same negligible cost) as splice.ts's
// `__spliceStats`: the parity fuzz asserts the fast path actually FIRES, so a
// change that quietly turns every patch back into a whole-node rebuild fails
// loudly instead of just getting quadratic again.
let keyedAttempts = 0;
let keyedHits = 0;
/** @internal Test-only. */
export function __keyedStats(): { attempts: number; hits: number } {
  return { attempts: keyedAttempts, hits: keyedHits };
}
/** @internal Test-only. */
export function __resetKeyedStats(): void {
  keyedAttempts = 0;
  keyedHits = 0;
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
  /** The Block object `html` came from. `spliceKeep` is keyed on Block IDENTITY
   *  (that is how it finds the wire delta that produced the new one), so the
   *  renderer has to hold the object, not just its html. Always the same object
   *  the store published — no extra retention. */
  block: Block;
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
  // Set only for an OPEN list rendered via the keyed-items path (blockData on).
  // Lets a later patch rewrite just the items whose html changed.
  list?: KeyedList;
  // Set only for an OPEN Blockquote / Alert rendered via the keyed-nested path
  // (blockData on). Lets a later patch replace just the nested sub-blocks whose
  // html changed.
  container?: KeyedContainer;
  // Set only while a code block too big for one slice is still tokenizing (see
  // hi-defer.ts). Cancelled when the node is rebuilt/dropped or the renderer is
  // destroyed, and used as the token that proves a landed result is not stale.
  highlight?: DeferredHighlight;
  // Set only for an OPEN code block being highlighted as it streams (see
  // hi-inc.ts): the frozen prefix and the cursor it was frozen at. Survives the
  // per-patch node rebuild — that is the whole point — and is consumed once as
  // the seed for the close-time run. Dropped when the block is dropped/destroyed
  // or stops being a code block; a revised (rather than extended) body is caught
  // by the state's own guard, which salvages or restarts it.
  inc?: IncState;
  // Set only while an OPEN code block's `<code>` is being driven by the
  // incremental highlighter: the live element plus how much of `inc.frozenHtml`
  // is already mirrored into it. Lets a patch APPEND the newly-frozen markup and
  // rewrite only the (CAP-bounded) speculative tail, instead of re-setting the
  // whole `innerHTML`. Dropped whenever the node is rebuilt or removed.
  codeInc?: IncCode;
  // Set only for an OPEN code block rendered with the PLAIN escaped body: the
  // `<div>` whose innerHTML is exactly `b.html`, so the generic delta splice
  // applies to it too. Cleared when a deferred highlight replaces that div.
  plainCode?: HTMLElement;
  // True when `node` is the generic `<div class="brook-block…">` whose entire
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

/**
 * Incremental keyed state for one OPEN list (`blockData` on). Each `<li>` is
 * stamped once from `ListData.items[i].html` and afterwards rewritten only when
 * THAT item's html actually changes.
 *
 * ## The identity, and why html-equality is the whole key
 *
 * The items channel carries no id, so the key is the pair (index, html). That is
 * sound because the core only ever appends items and revises the LAST one: an
 * item's html, once a later item exists, is byte-stable for the rest of the
 * stream (it is `Rc`-shared upstream and re-serialized verbatim into every
 * patch). So "same index, same html" really does mean "the same item, unchanged",
 * and comparing the strings is what proves it rather than assuming it — no
 * committed-watermark heuristic that a rewrite could silently invalidate.
 *
 * That matters for the one case a watermark WOULD get wrong: a tight→loose flip.
 * The moment a blank line makes the list loose, every item's html legitimately
 * changes at once (`a` → `<p>a</p>`), and this resyncs all of them. It costs one
 * O(current size) pass, once, when the flip happens — not on every patch, which
 * is what the whole-node rebuild it replaces was doing.
 *
 * Per patch the scan compares `items.length` strings; the ones that are equal
 * cost a memcmp and write NOTHING to the document, which is the term that
 * actually dominates (an `innerHTML` set re-parses HTML; a compare does not).
 */
interface KeyedList {
  /** The live `<ul>` / `<ol>`; its element children are the `<li>`s, in order. */
  list: HTMLElement;
  /** `<ul>` ⇄ `<ol>` is a different element, so a flip forces a rebuild. */
  ordered: boolean;
  /** The `start` currently reflected in the `start="N"` attribute. */
  start?: number;
  /** The html currently rendered into each `<li>`, index-aligned with them. */
  items: string[];
}

/**
 * Incremental keyed state for one OPEN Blockquote / Alert (`blockData` on).
 * Same key as {@link KeyedList} — (index, html) over `ContainerData.nested` —
 * and sound for the same reason: the core appends nested sub-blocks and revises
 * only the last one, so an entry with an earlier index and unchanged html is the
 * same sub-block untouched. A changed entry has its whole child node replaced
 * (a nested entry's html carries its own root tag, unlike a list item's inner
 * html), which is exactly what the full rebuild built for it.
 */
interface KeyedContainer {
  /** The live `<blockquote>` / alert `<div>` inside the block node. */
  wrapper: HTMLElement;
  /** Wrapper children BEFORE the nested ones: 1 for an alert title, else 0. */
  offset: number;
  /** The wrapper's opening tag, whose attributes were applied at build time. */
  openTag: string;
  /** An alert's title markup, kept as the first child (never in `nested`). */
  title: string;
  /** The html currently rendered for each nested entry, index-aligned. */
  nested: string[];
}

/**
 * Resolve the anchor a DELEGATED click landed on, or null when the click was
 * not on a live link. Deliberately duplicated (six lines) by the JSX renderer
 * rather than shared, so importing `brookmd/react` never pulls in the DOM
 * renderer. A `data-brook-pending` anchor — label rendered, URL still streaming
 * — is excluded twice over: it carries no `href`, so `a[href]` already misses
 * it, and the attribute is checked explicitly.
 */
function linkFromEvent(target: EventTarget | null, root: HTMLElement): HTMLAnchorElement | null {
  const el = target as Element | null;
  // The target can be a text node (or, in an odd host, anything): guard the
  // lookup instead of throwing out of a listener the app installed.
  if (!el || typeof el.closest !== "function") return null;
  const a = el.closest("a[href]");
  if (!a || !root.contains(a) || a.hasAttribute("data-brook-pending")) return null;
  return a as HTMLAnchorElement;
}

export function mountBrookMarkdown(
  client: BrookClient,
  container: HTMLElement,
  options: MountOptions = {},
): MountHandle {
  if (typeof document === "undefined") {
    throw new Error("mountBrookMarkdown is browser-only; call it after the DOM exists.");
  }

  // Normalize "no overrides" to undefined so the fast path doesn't churn.
  const components =
    options.components && Object.keys(options.components).length > 0 ? options.components : undefined;
  const { sanitize, virtualize, stickToBottom, onRenderMetrics, onLinkClick } = options;
  // Normalize "no decorators" to undefined so an empty array doesn't take the
  // walk path; both transforms move a block off the innerHTML fast path.
  const decorators =
    options.decorators && options.decorators.length > 0 ? options.decorators : undefined;
  const urlTransform = options.urlTransform;
  const hasInlineTransforms = !!decorators || !!urlTransform;
  const hasPerf = typeof performance !== "undefined";
  const highlightCode = options.highlightCode !== false && !components?.CodeBlock;
  const streamingHighlight = options.streamingHighlight !== false;
  const tailMode: TailMode = options.streamingHighlight === "eager" ? "eager" : "wavefront";
  const batch = options.batch !== false && typeof requestAnimationFrame === "function";
  const morphOpenBlocks = options.morphOpenBlocks === true;
  const fullRebuild = options.__fullRebuild === true;

  const root = document.createElement("div");
  root.className = options.className ? `brook-md ${options.className}` : "brook-md";
  if (options.id) root.id = options.id;
  if (options.role) root.setAttribute("role", options.role);
  if (options.ariaLive) root.setAttribute("aria-live", options.ariaLive);
  if (options.ariaAtomic !== undefined) root.setAttribute("aria-atomic", String(options.ariaAtomic));
  container.appendChild(root);

  // CSS-only stick-to-bottom: a permanent sentinel pinned as the last child.
  let anchor: HTMLElement | null = null;
  if (stickToBottom) {
    anchor = document.createElement("div");
    anchor.className = "brook-bottom-anchor";
    anchor.setAttribute("aria-hidden", "true");
    anchor.style.scrollSnapAlign = "end";
    root.appendChild(anchor);
  }

  // Delegated link clicks: ONE listener on the root for the whole document, so
  // no block render ever touches it and node reuse is unaffected. Held so
  // destroy() can remove it (the root is removed too, but an app may keep a
  // reference to a node inside it).
  const linkClickListener = onLinkClick
    ? (ev: Event): void => {
        const a = linkFromEvent(ev.target, root);
        if (!a) return;
        onLinkClick(ev as MouseEvent, {
          href: a.getAttribute("href") ?? "",
          text: a.textContent ?? "",
          element: a,
        });
      }
    : null;
  if (linkClickListener) root.addEventListener("click", linkClickListener);

  const mounted = new Map<number, MountedBlock>();
  let order: number[] = [];
  let dead = false;
  let frame = 0;
  // Set by `renderBlockContent` for the call in flight: true only when it took
  // the generic `<div class="brook-block…">innerHTML=html` path (no override, no
  // dedicated renderer). Read immediately after the render to tag the mount.
  let lastRenderGeneric = false;

  function sync(): void {
    if (dead) return;
    const snapshot = client.getSnapshot();
    const nextOrder: number[] = new Array(snapshot.length);
    const seen = new Set<number>();

    let w = 0;
    for (let i = 0; i < snapshot.length; i++) {
      const b = snapshot[i];
      // Parity with the React renderer: a malformed entry is skipped, never
      // dereferenced. The store's invariants say this cannot happen; a thrown
      // TypeError here would abort the whole reconcile mid-way and leave the
      // container in a half-updated state.
      if (b == null || b.kind == null) continue;
      nextOrder[w++] = b.id;
      seen.add(b.id);
      const existing = mounted.get(b.id);
      if (!existing) {
        const t0 = onRenderMetrics && hasPerf ? performance.now() : 0;
        const mb: MountedBlock = {
          id: b.id, node: undefined as unknown as HTMLElement,
          html: b.html, block: b, open: b.open, speculative: b.speculative, kind: b.kind.type,
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
      // Incremental-code fast path: an OPEN fence already mounted through hi-inc
      // appends whatever its frozen prefix just settled and rewrites ONLY the
      // speculative tail. That is the single biggest write on the streaming path
      // — a 20 KB fence otherwise re-sets ~7× its own size in highlighted markup
      // on every patch — and keeping the node also keeps the `<pre>`'s scroll
      // offset and any text selection inside the frozen region alive. Reuses the
      // same block node (no replaceWith), so it still feeds the render probe.
      if (
        existing.codeInc &&
        existing.inc &&
        b.open &&
        existing.open &&
        b.kind.type === "CodeBlock" &&
        existing.kind === "CodeBlock" &&
        syncIncCode(existing, b)
      ) {
        if (onRenderMetrics) noteRender(existing, b, t0);
        existing.html = b.html;
        existing.block = b;
        existing.speculative = b.speculative;
        continue;
      }
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
            existing.node.classList.toggle("brook-speculative", b.speculative);
          }
          existing.html = b.html;
          existing.block = b;
          existing.open = b.open;
          existing.speculative = b.speculative;
          continue;
        }
      }
      // Keyed-list fast path: an OPEN list that mounted through the keyed-items
      // path rewrites only the `<li>`s whose item html actually changed (normally
      // just the open last one) and appends the items that have since arrived.
      // Without this the keyed renderer re-stamped EVERY `<li>` on every patch —
      // it routes around the generic splice, so it was the last quadratic DOM
      // writer left. Committed `<li>` nodes are now never touched, so a selection
      // inside one survives. Reuses the same block node (no replaceWith), and is
      // still a render of it, so it feeds the render-churn probe.
      if (
        !fullRebuild &&
        existing.list &&
        b.open &&
        b.kind.type === "List" &&
        existing.kind === "List"
      ) {
        const ld = b.kind.data as ListData | undefined;
        if (ld && syncKeyedList(existing.list, ld)) {
          if (onRenderMetrics) noteRender(existing, b, t0);
          // The node stays the keyed list mirror, so only the class string
          // (which encodes `speculative`) can have moved.
          existing.node.className = genericClassName(b);
          existing.html = b.html;
          existing.block = b;
          existing.open = b.open;
          existing.speculative = b.speculative;
          continue;
        }
      }
      // Keyed-container fast path: the same treatment for an OPEN Blockquote /
      // Alert's `nested` sub-blocks — replace only the entries whose html
      // changed, append the new ones, and leave every settled sub-block's node
      // alone.
      if (
        !fullRebuild &&
        existing.container &&
        b.open &&
        existing.kind === b.kind.type &&
        (b.kind.type === "Blockquote" || b.kind.type === "Alert") &&
        syncKeyedContainer(existing.container, b)
      ) {
        if (onRenderMetrics) noteRender(existing, b, t0);
        existing.node.className = genericClassName(b);
        existing.html = b.html;
        existing.block = b;
        existing.open = b.open;
        existing.speculative = b.speculative;
        continue;
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
        existing.block = b;
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
        existing.block = b;
        continue;
      }
      // Delta-driven child splice (see splice.ts). The prefix-append path above
      // can only fire when the new html *starts with* the old one, which an open
      // block almost never does — the core speculatively closes it, so every
      // patch rewrites the trailing `</em></p>`. The wire already computed the
      // real boundary; this applies it, appending inside the chain of elements
      // still open at that offset and leaving every node before it — including a
      // live text selection — untouched. Bails to the rebuild below whenever it
      // cannot prove the shape.
      if (
        !fullRebuild &&
        !sanitize &&
        !hasInlineTransforms &&
        existing.generic &&
        b.open &&
        existing.open &&
        existing.kind === b.kind.type &&
        usesGenericPath(b)
      ) {
        const keep = spliceKeep(existing.block, b);
        if (keep !== undefined && spliceHtml(existing.node, existing.html, b.html, keep)) {
          if (onRenderMetrics) noteRender(existing, b, t0);
          existing.html = b.html;
          existing.block = b;
          existing.speculative = b.speculative;
          // The node stays the generic innerHTML mirror, so only the class
          // string (which encodes `speculative`) can have moved.
          existing.node.className = genericClassName(b);
          continue;
        }
      }
      // Same splice, applied to an open code block whose body is the PLAIN
      // escaped `<pre><code>` (no highlighter, `streamingHighlight: false`, or a
      // language with no table): that `<div>`'s innerHTML is exactly `b.html`,
      // so the identical reasoning applies one level down.
      if (
        !fullRebuild &&
        existing.plainCode &&
        b.open &&
        existing.open &&
        b.kind.type === "CodeBlock" &&
        existing.kind === "CodeBlock"
      ) {
        const keep = spliceKeep(existing.block, b);
        if (keep !== undefined && spliceHtml(existing.plainCode, existing.html, b.html, keep)) {
          if (onRenderMetrics) noteRender(existing, b, t0);
          existing.html = b.html;
          existing.block = b;
          existing.speculative = b.speculative;
          continue;
        }
      }
      // Changed → rebuild and swap in place. A table that just closed (or whose
      // data vanished) drops its keyed manager and re-renders the full HTML once.
      existing.table = undefined;
      // Same for the keyed list / container managers: both point at nodes inside
      // the subtree about to be discarded, and whichever branch the rebuild takes
      // re-seeds its own. A closed (or kind-changed) block leaves them unset, so
      // it can never be resynced against a node that is no longer on screen.
      existing.list = undefined;
      existing.container = undefined;
      // The frozen/tail mirror and the plain-body handle both point into the
      // node we are about to discard; renderCodeBlock re-seeds whichever the
      // new node turns out to have.
      existing.codeInc = undefined;
      existing.plainCode = undefined;
      // Incremental highlight state belongs to a code block's SOURCE, not to its
      // node, so a per-patch rebuild must keep it (that is what makes the open
      // block linear). It only becomes meaningless when the block stops being a
      // code block; a revised — rather than extended — body is caught by the
      // state's own guard, which keeps whatever the revision left settled.
      if (existing.inc && b.kind.type !== "CodeBlock") existing.inc = undefined;
      // A half-finished highlight belongs to the node about to be replaced.
      if (existing.highlight) {
        existing.highlight.cancel();
        existing.highlight = undefined;
      }
      const node = renderBlock(b, existing);
      existing.node.replaceWith(node);
      existing.node = node;
      if (onRenderMetrics) noteRender(existing, b, t0);
      existing.html = b.html;
      existing.block = b;
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
          if (mb.highlight) mb.highlight.cancel();
          mb.inc = undefined;
          mb.codeInc = undefined;
          mb.plainCode = undefined;
          mb.node.remove();
          mounted.delete(id);
        }
      }
    }

    // Only ever shorter when a malformed entry was skipped above; `order` must
    // not carry trailing holes into reconcileChildren.
    if (w !== nextOrder.length) nextOrder.length = w;
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
  // node whose live position differs. The `.brook-bottom-anchor` is never part of
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
        if (highlightCode) return renderCodeBlock(b, mb);
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
    // Decorators / urlTransform disable the keyed streaming fast paths (they
    // build sub-trees from the data channel, bypassing the post-render text/URL
    // walk) — the block falls through to the generic full-HTML path, which is
    // then walked once. Still O(n) per block (committed blocks stay frozen).
    if (kind === "Table" && b.open && !hasInlineTransforms) {
      const data = tableData(b);
      if (data) return buildKeyedTable(b, data, mb);
    }

    // 2c. Keyed list renderer (opt-in: only when `blockData` is on, so
    // `kind.data.items` carries per-item inner HTML). For an OPEN list, stamp one
    // `<li>` per item — each item's inner HTML routed through the SAME sanitize
    // path the generic innerHTML branch uses — so the rebuilt list tracks the
    // structured items instead of re-parsing the whole `<ul>`/`<ol>` HTML. Closed
    // lists fall through (their node is reused untouched, never rebuilt).
    if (b.open && kind === "List" && !hasInlineTransforms) {
      const keyed = renderKeyedList(b, mb);
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
    if (b.open && !sanitize && !hasInlineTransforms && (kind === "Blockquote" || kind === "Alert")) {
      const wrapper = renderKeyedContainer(b, mb);
      if (wrapper) {
        node.appendChild(wrapper);
        return node;
      }
    }
    node.innerHTML = sanitize ? sanitize(b.html) : b.html;
    // Post-render inline transforms: walk the just-built subtree's TEXT nodes for
    // decorators and rewrite URL attributes for urlTransform. This runs on the
    // trusted core HTML AFTER it is parsed into nodes — the same model as the
    // React walker — so the security posture is unchanged (decorator output is a
    // trusted, un-sanitized surface; urlTransform output is re-sanitized).
    if (decorators) decorateDomSubtree(node, decorators);
    if (urlTransform) applyUrlTransformDom(node, urlTransform);
    // Eligible for the prefix-append fast path only when no sanitizer rewrote the
    // html AND no inline transform mutated the subtree (the stored `html` must
    // equal the node's actual innerHTML source for the byte-faithful append). A
    // transformed block therefore fully rebuilds + re-walks on each open patch.
    lastRenderGeneric = !sanitize && !hasInlineTransforms;
    return node;
  }

  // Build a `<div class="brook-block brook-block-list brook-open …"><ul|ol>…</ul|ol>`
  // node from the structured `kind.data.items`, one `<li>` per item with its inner
  // HTML sanitized via the shared `sanitize` path. Returns `null` when the items
  // channel is absent (blockData off) so the caller falls back to opaque HTML.
  function renderKeyedList(b: Block, mb: MountedBlock): HTMLElement | null {
    const ld = b.kind.data as ListData | undefined;
    const items = ld?.items;
    if (!Array.isArray(items) || items.length === 0) return null;
    const node = document.createElement("div");
    // Byte-identical to `genericClassName(b)` for a List; shared so the keyed
    // sync's in-place class update cannot drift from what a rebuild would set.
    node.className = genericClassName(b);
    const ordered = !!ld?.ordered;
    const list = document.createElement(ordered ? "ol" : "ul");
    if (ordered && ld!.start !== undefined && ld!.start !== 1) {
      list.setAttribute("start", String(ld!.start));
    }
    const rendered: string[] = new Array(items.length);
    for (let i = 0; i < items.length; i++) {
      const li = document.createElement("li");
      li.innerHTML = sanitize ? sanitize(items[i].html) : items[i].html;
      list.appendChild(li);
      rendered[i] = items[i].html;
    }
    node.appendChild(list);
    // Remember what is on screen so the next patch rewrites only what moved.
    mb.list = { list, ordered, start: ld?.start, items: rendered };
    return node;
  }

  /**
   * Bring an already-mounted keyed list up to date against the new `items`.
   * Returns `false` when the shape is not one it can prove, and the caller
   * rebuilds the whole node — which is also why the mutations below need no
   * rollback: a `false` result always discards this subtree. Every bail is
   * therefore taken BEFORE the first write.
   */
  function syncKeyedList(km: KeyedList, ld: ListData): boolean {
    keyedAttempts++;
    const items = ld.items;
    // An empty/absent items channel is what `renderKeyedList` refuses to build
    // from, so it is what the rebuild has to see too.
    if (!Array.isArray(items) || items.length === 0) return false;
    // `<ul>` and `<ol>` are different elements — a flip is a new node.
    if (!!ld.ordered !== km.ordered) return false;
    const cur = km.items;
    const kids = km.list.children;
    // The manager must still describe the live element exactly; anything else
    // (an override or a consumer mutating our subtree) means the index key no
    // longer identifies what we think it does.
    if (kids.length !== cur.length) return false;

    // The `start="N"` attribute, on exactly the condition the builder uses.
    if (ld.start !== km.start) {
      if (km.ordered && ld.start !== undefined && ld.start !== 1) {
        km.list.setAttribute("start", String(ld.start));
      } else {
        km.list.removeAttribute("start");
      }
      km.start = ld.start;
    }

    const n = items.length;
    // A speculative revision withdrew trailing items: drop their `<li>`s. The
    // nested entries are always the trailing children, so this is a pop.
    while (cur.length > n) {
      km.list.removeChild(km.list.lastElementChild!);
      cur.pop();
    }
    // Rewrite only the items whose html actually moved. In the streaming case
    // that is exactly one (the open last item); on a tight→loose flip it is all
    // of them, which is a legitimate one-off O(current size) resync.
    for (let i = 0; i < cur.length; i++) {
      if (cur[i] === items[i].html) continue;
      (kids[i] as HTMLElement).innerHTML = sanitize ? sanitize(items[i].html) : items[i].html;
      cur[i] = items[i].html;
    }
    // Append the items that have arrived since the last patch.
    for (let i = cur.length; i < n; i++) {
      const li = document.createElement("li");
      li.innerHTML = sanitize ? sanitize(items[i].html) : items[i].html;
      km.list.appendChild(li);
      cur.push(items[i].html);
    }
    keyedHits++;
    return true;
  }

  // Build a Blockquote / Alert wrapper with KEYED inner sub-block nodes from the
  // structured `nested` channel. The wrapper element + its attributes (`dir`/
  // `class`/`data-alert`/`role`) come from `b.html`'s opening tag so the streamed
  // wrapper is byte-faithful; the alert title `<p>` is kept as the first child
  // (it is the wrapper, not a body block). Returns null when `nested` is absent.
  function renderKeyedContainer(b: Block, mb: MountedBlock): HTMLElement | null {
    const nested = (b.kind.data as ContainerData | undefined)?.nested;
    if (!Array.isArray(nested)) return null;
    const tagName = b.kind.type === "Alert" ? "div" : "blockquote";
    const wrapper = document.createElement(tagName);
    applyOpenTagAttrs(wrapper, b.html);
    let offset = 0;
    let title = "";
    if (b.kind.type === "Alert") {
      title = alertTitleHtml(b.html);
      if (title) {
        const t = document.createElement("div");
        t.innerHTML = title;
        const titleNode = t.firstElementChild;
        if (titleNode) {
          wrapper.appendChild(titleNode);
          offset = 1;
        }
      }
    }
    const rendered: string[] = new Array(nested.length);
    for (let i = 0; i < nested.length; i++) {
      wrapper.appendChild(nestedChild(nested[i].html));
      rendered[i] = nested[i].html;
    }
    mb.container = { wrapper, offset, openTag: openTagOf(b.html), title, nested: rendered };
    return wrapper;
  }

  // One nested sub-block's node. A nested block is a single root element
  // (`<p>…</p>`, `<ul>…</ul>`, …), so the temp `<div>` is unwrapped and the
  // wrapper holds the real element directly. Shared by the build and the sync so
  // a replaced child is byte-identical to the one a rebuild would have made.
  function nestedChild(html: string): Element {
    const child = document.createElement("div");
    child.innerHTML = html;
    return child.firstElementChild ?? child;
  }

  /**
   * Bring an already-mounted keyed container up to date against the new
   * `nested`. Like {@link syncKeyedList}, `false` means the caller rebuilds the
   * node outright, so every bail is taken before the first write.
   */
  function syncKeyedContainer(kc: KeyedContainer, b: Block): boolean {
    keyedAttempts++;
    const nested = (b.kind.data as ContainerData | undefined)?.nested;
    if (!Array.isArray(nested)) return false;
    // The wrapper's attributes were stamped from the opening tag at build time,
    // so a different opening tag is a different wrapper. This is an O(1) slice
    // (the first `>` is a handful of chars in), and it is also what makes the
    // alert title stable: the alert's kind lives in that tag's class.
    if (openTagOf(b.html) !== kc.openTag) return false;
    // …and belt-and-braces for the title itself, which is the one child the
    // `nested` key does not cover. Alert-only: the regex anchors on the title
    // `<p>`, which is the wrapper's first child, so the match terminates near
    // the start of the html rather than scanning the whole block.
    if (b.kind.type === "Alert" && alertTitleHtml(b.html) !== kc.title) return false;
    const cur = kc.nested;
    const kids = kc.wrapper.children;
    if (kids.length !== kc.offset + cur.length) return false;

    const n = nested.length;
    // A speculative revision withdrew trailing sub-blocks. They are the trailing
    // children (the alert title is always first), so this is a pop.
    while (cur.length > n) {
      kc.wrapper.removeChild(kc.wrapper.lastElementChild!);
      cur.pop();
    }
    // Replace only the sub-blocks whose html moved — normally just the open last
    // one. A nested entry's html carries its own root tag, so the whole child
    // node is swapped rather than its innerHTML rewritten.
    for (let i = 0; i < cur.length; i++) {
      if (cur[i] === nested[i].html) continue;
      kc.wrapper.replaceChild(nestedChild(nested[i].html), kids[kc.offset + i]);
      cur[i] = nested[i].html;
    }
    // Append the sub-blocks that have arrived since the last patch.
    for (let i = cur.length; i < n; i++) {
      kc.wrapper.appendChild(nestedChild(nested[i].html));
      cur.push(nested[i].html);
    }
    keyedHits++;
    return true;
  }

  // Build the initial keyed table node + manager. The `<thead>` and all-but-last
  // `<tr>` are emitted once; the manager remembers the committed row count so a
  // later patch (via syncTbody) only re-renders the open trailing row.
  function buildKeyedTable(b: Block, data: TableData, mb: MountedBlock): HTMLElement {
    const node = document.createElement("div");
    node.className = "brook-block brook-block-table brook-open" + (b.speculative ? " brook-speculative" : "");
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
  // class string (e.g. dropping `brook-speculative`) a rebuild would have set.
  function genericClassName(b: Block): string {
    return (
      "brook-block brook-block-" +
      b.kind.type.toLowerCase() +
      (b.open ? " brook-open" : "") +
      (b.speculative ? " brook-speculative" : "")
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

  function renderCodeBlock(b: Block, mb: MountedBlock): HTMLElement {
    const lang = extractLang(b.html) || "text";
    // A rebuild throws away the nodes both handles pointed at; whichever branch
    // this render takes re-seeds its own.
    mb.codeInc = undefined;
    mb.plainCode = undefined;
    // Mirror CodeBlock.tsx: text is "" while open (the OPEN block's markup comes
    // from the incremental path instead); a closed block decodes once and
    // highlights once. The node is frozen once closed, so highlight runs exactly
    // once (no re-tokenize).
    const text = b.open ? "" : codeText(b);
    // An open fence: extend the frozen prefix and re-tokenize only the tail (see
    // hi-inc.ts). `null` back means the block is past the size guard and gets
    // the plain escaped body, exactly as it always did.
    let openMarkup: string | null = null;
    if (b.open && streamingHighlight) {
      let inc = mb.inc;
      if (inc === undefined || inc.lang !== lang.toLowerCase()) {
        inc = createInc(lang) ?? undefined;
        mb.inc = inc;
      }
      if (inc !== undefined) openMarkup = incHighlight(inc, codeText(b));
    }
    // What that markup LOOKS like under the tail mode — the mirror paints it
    // incrementally, and a rebuild has to write the same thing in one go.
    const openView =
      openMarkup === null || mb.inc === undefined ? openMarkup : incView(mb.inc, openMarkup, tailMode);
    // The first slice runs HERE, before the node is inserted: an ordinary block
    // is highlighted in the same paint as always, and a fence that streamed in
    // resumes from its frozen prefix so only the tail is left to do. Only a fence
    // too big for the budget shows the plain body first and swaps its markup in a
    // few tasks later — same bytes, one extra paint, no frozen main thread.
    const seed = !b.open && mb.inc !== undefined ? incSeed(mb.inc, text, lang) : undefined;
    const run = text ? highlightDeferred(text, lang, seed) : null;
    if (!b.open) mb.inc = undefined; // consumed: the block is settled
    const highlighted = openView ?? (run ? run.html : null);

    const block = document.createElement("div");
    block.className = "brook-code-block" + (b.open ? " brook-streaming" : "");

    const header = document.createElement("div");
    header.className = "brook-code-header";
    const langSpan = document.createElement("span");
    langSpan.className = "brook-code-lang";
    langSpan.textContent = lang;
    header.appendChild(langSpan);

    if (b.open) {
      const pill = document.createElement("span");
      pill.className = "brook-code-streaming-pill";
      pill.textContent = "streaming";
      header.appendChild(pill);
    } else {
      header.appendChild(makeCopyButton(text));
    }
    block.appendChild(header);

    const body = document.createElement("div");
    body.className = "brook-code-body";
    if (highlighted !== null) {
      // An open fence mounts its `<code>` already split into frozen prefix +
      // speculative tail, so the very next patch can splice instead of rebuild.
      body.appendChild(
        openMarkup !== null && mb.inc !== undefined && !fullRebuild
          ? incPre(lang, mb, mb.inc, openMarkup)
          : highlightedPre(lang, highlighted),
      );
    } else {
      const div = document.createElement("div");
      div.tabIndex = 0;
      div.setAttribute("role", "region");
      div.setAttribute("aria-label", `${lang} code`);
      div.innerHTML = b.html;
      body.appendChild(div);
      // Only an OPEN block's plain body ever grows; a closed one is frozen by
      // the fingerprint check and never revisited.
      if (b.open) mb.plainCode = div;
      if (run !== null && run.rest !== null) {
        // Remember the run on the mount so a rebuild/removal can cancel it, and
        // so a landed result can prove it still belongs to what is on screen.
        mb.highlight = run;
        run.rest.then((markup) => {
          if (markup === null || dead) return;
          // Stale guard: the block was rebuilt (new node), dropped, or its run
          // was superseded while we tokenized — never paint the old tokens.
          if (mb.highlight !== run || mb.node !== block) return;
          mb.highlight = undefined;
          mb.plainCode = undefined; // the div the splice targeted is gone
          body.replaceChild(highlightedPre(lang, markup), div);
        });
      }
    }
    block.appendChild(body);
    return block;
  }

  // Advance an already-mounted open fence's incremental state and repaint just
  // its tail. False → the mirror bowed out (language change, past hi-inc's
  // cliff, or the split invariant broke) and the caller rebuilds the node.
  function syncIncCode(mb: MountedBlock, b: Block): boolean {
    const ic = mb.codeInc!;
    if ((extractLang(b.html) || "text") !== ic.lang) return false;
    const markup = incHighlight(mb.inc!, codeText(b));
    if (markup === null) return false;
    return paintIncCode(ic, mb.inc!, markup);
  }

  // `highlightedPre`, but with the `<code>` populated through the frozen/tail
  // mirror and the mirror recorded on the mount, so later patches splice.
  function incPre(lang: string, mb: MountedBlock, st: IncState, markup: string): HTMLElement {
    const pre = document.createElement("pre");
    pre.tabIndex = 0;
    pre.setAttribute("role", "region");
    pre.setAttribute("aria-label", `${lang} code`);
    const code = document.createElement("code");
    const ic = newIncCode(code, lang, st, tailMode);
    if (paintIncCode(ic, st, markup)) mb.codeInc = ic;
    else code.innerHTML = incView(st, markup, tailMode);
    pre.appendChild(code);
    return pre;
  }

  // The highlighted body: `<pre tabindex=0 role=region><code>…</code></pre>`.
  // Shared by the synchronous build and the deferred swap so both produce the
  // identical subtree.
  function highlightedPre(lang: string, markup: string): HTMLElement {
    const pre = document.createElement("pre");
    pre.tabIndex = 0;
    pre.setAttribute("role", "region");
    pre.setAttribute("aria-label", `${lang} code`);
    const code = document.createElement("code");
    code.innerHTML = markup;
    pre.appendChild(code);
    return pre;
  }

  function renderMathBlock(b: Block): HTMLElement {
    const block = document.createElement("div");
    block.className = "brook-math-block" + (b.open ? " brook-streaming" : "");
    const header = document.createElement("div");
    header.className = "brook-math-header";
    const lang = document.createElement("span");
    lang.className = "brook-math-lang";
    lang.textContent = "math";
    header.appendChild(lang);
    if (b.open) header.appendChild(streamingPill());
    block.appendChild(header);
    const body = document.createElement("div");
    body.className = "brook-math-body";
    body.innerHTML = b.html;
    block.appendChild(body);
    return block;
  }

  function renderMermaid(b: Block): HTMLElement {
    const block = document.createElement("div");
    block.className = "brook-mermaid-block" + (b.open ? " brook-streaming" : "");
    const header = document.createElement("div");
    header.className = "brook-mermaid-header";
    const lang = document.createElement("span");
    lang.className = "brook-mermaid-lang";
    lang.textContent = "mermaid";
    header.appendChild(lang);
    if (b.open) header.appendChild(streamingPill());
    block.appendChild(header);
    const body = document.createElement("div");
    body.className = "brook-mermaid-body";
    body.innerHTML = b.html;
    block.appendChild(body);
    return block;
  }

  function streamingPill(): HTMLElement {
    const pill = document.createElement("span");
    pill.className = "brook-code-streaming-pill";
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
    btn.className = "brook-code-copy";
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
      if (linkClickListener) root.removeEventListener("click", linkClickListener);
      // Abandon any code block still tokenizing — its node is going away.
      for (const mb of mounted.values()) {
        if (mb.highlight) {
          mb.highlight.cancel();
          mb.highlight = undefined;
        }
        mb.inc = undefined;
        mb.codeInc = undefined;
        mb.plainCode = undefined;
      }
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
  // Validate the shapes the keyed path indexes (rows/aligns/headers) so a
  // drifted/malformed blockData wire falls back to the full-HTML path instead
  // of crashing makeRow/makeCell on `aligns[j]`/`headers.map` of undefined.
  if (
    !data ||
    !Array.isArray(data.rows) ||
    !Array.isArray(data.aligns) ||
    !Array.isArray(data.headers)
  ) {
    return undefined;
  }
  return data;
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

// A code block's DECODED source. Prefers the structured `kind.data.code` the
// core emits under `blockData` — the same string the HTML decoder rebuilds,
// without the whole-body regex + five entity passes.
function codeText(b: Block): string {
  const data = b.kind.data as { code?: string } | undefined;
  return typeof data?.code === "string" ? data.code : decodeCodeText(b.html);
}

// Local copy of the canonical code-text decoder (kept here so dom.ts depends
// only on neutral modules; block-props.ts keeps its own private copy too).
//
// The body is located by INDEX rather than by matching
// `/<pre><code[^>]*>([\s\S]*?)<\/code><\/pre>/` — the same three landmarks, in
// the same order, taking the same first match, so the same bytes — and the
// entity passes run only when there is an entity to decode. That matters
// because this is the open fence's per-patch source: it runs once for every
// patch, over a body that keeps growing. On a streamed 32 KB fence the regex
// form and its five unconditional passes cost 295 ms, three times the
// highlighting they feed; this form costs 16 ms.
function decodeCodeText(html: string): string {
  const open = html.indexOf("<pre><code");
  if (open < 0) return "";
  const start = html.indexOf(">", open + 10);
  if (start < 0) return "";
  const end = html.indexOf("</code></pre>", start + 1);
  if (end < 0) return "";
  const body = html.slice(start + 1, end);
  // amp last, so `&amp;lt;` decodes to `&lt;` and not to `<`.
  return body.indexOf("&") < 0
    ? body
    : body
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

// The wrapper's opening tag — everything up to the first `>`. The keyed
// container sync compares this to decide whether the attributes it stamped are
// still the right ones, so the two must read the same slice.
function openTagOf(html: string): string {
  const gt = html.indexOf(">");
  return gt < 0 ? html : html.slice(0, gt);
}

function applyOpenTagAttrs(el: HTMLElement, html: string): void {
  const open = openTagOf(html);
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

// The chain of enclosing element tag names for a text node, from its immediate
// parent up to (but excluding) the block-root `root`. Matches the React walker's
// ancestor set (the block's own elements, not the outer brook-block wrapper) so
// `skipInside` behaves identically across the two renderers. Order is irrelevant
// (the skip check is a membership test).
function ancestorTags(node: Node, root: HTMLElement): string[] {
  const out: string[] = [];
  let p = node.parentNode as HTMLElement | null;
  while (p && p !== root) {
    if (p.tagName) out.push(p.tagName.toLowerCase());
    p = p.parentNode as HTMLElement | null;
  }
  return out;
}

// Walk `root`'s real TEXT nodes (a `TreeWalker`) and splice in each decorator's
// replacement. Text nodes are collected FIRST, then mutated — a live TreeWalker
// would be invalidated by replacing the node it is parked on. The matcher feeds
// each ORIGINAL text-node string via the SHARED `decorateSegments` helper
// (identical behavior to the React walker), so a decorator never re-matches
// inside another decorator's replacement.
//
// NOTE: we request `SHOW_ALL` and select TEXT nodes (`nodeType === 3`) ourselves
// rather than relying on `SHOW_TEXT`'s `whatToShow` filtering — some DOM
// implementations (happy-dom) mishandle the bitmask and drop every text node.
// Real browsers behave identically either way; this keeps the walk portable.
const SHOW_ALL = 0xffffffff;
function decorateDomSubtree(root: HTMLElement, decorators: Decorator[]): void {
  const walker = document.createTreeWalker(root, SHOW_ALL);
  const texts: Text[] = [];
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (n.nodeType === 3) texts.push(n as Text);
  }
  for (const t of texts) {
    const segs = decorateSegments(t.data, decorators, ancestorTags(t, root));
    if (segs === null) continue; // nothing matched — leave the text node untouched
    const frag = document.createDocumentFragment();
    for (const s of segs) {
      if (s.type === "text") {
        frag.appendChild(document.createTextNode(s.text));
        continue;
      }
      // Trusted surface: a returned Node is inserted verbatim (NOT sanitized);
      // a string becomes a text node. `replace` is pure, so streamed == one-shot.
      const replacement = s.decorator.replace(s.matchText, s.groups) as Node | string | null | undefined;
      if (replacement == null) continue;
      frag.appendChild(typeof replacement === "string" ? document.createTextNode(replacement) : replacement);
    }
    t.parentNode?.replaceChild(frag, t);
  }
}

// Rewrite every `href`/`src`/`poster` in `root` through `urlTransform`,
// re-sanitizing the OUTPUT so a buggy/hostile transform can't reach the DOM with
// a dangerous scheme: `safeUrl(urlTransform(safeUrl(value)))`. O(1) per attr.
function applyUrlTransformDom(root: HTMLElement, urlTransform: UrlTransform): void {
  const els = root.querySelectorAll("[href],[src],[poster]");
  const attrs: Array<"href" | "src" | "poster"> = ["href", "src", "poster"];
  els.forEach((el) => {
    for (const attr of attrs) {
      if (!el.hasAttribute(attr)) continue;
      const cur = el.getAttribute(attr) ?? "";
      const next = safeUrl(urlTransform(safeUrl(cur), { tag: el.tagName.toLowerCase(), attr }));
      el.setAttribute(attr, next);
    }
  });
}

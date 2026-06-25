import {
  createElement,
  memo,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type CSSProperties,
  type ReactElement,
} from "react";
import type { Block, BlockComponentProps, Components, HeadingData, TableData } from "./types";
import { FluxClient } from "./client";
import type { ParserConfig, RenderMetricsHook } from "./types-core";
import { CodeBlock } from "./renderers/CodeBlock";
import { MathBlock } from "./renderers/Math";
import { Mermaid } from "./renderers/Mermaid";
import { htmlToReact } from "./html-to-react";

/**
 * Render a streaming markdown document from a FluxClient. Each block is its
 * own memoized React node keyed by its stable parser-assigned ID, so React
 * only reconciles the blocks whose HTML actually changed since the last
 * patch. Heavy renderers (Shiki, KaTeX, Mermaid) defer work until a block
 * is closed.
 *
 * ## Custom components
 *
 * Pass `components` to override rendering (see {@link Components}):
 *
 * ```tsx
 * <FluxMarkdown
 *   client={client}
 *   components={{
 *     table: (p) => <table className="my-table" {...p} />, // tag-level
 *     a: (p) => <a target="_blank" rel="noreferrer" {...p} />,
 *     CodeBlock: (p) => <MyCodeBlock {...p} />,             // block-kind
 *   }}
 * />
 * ```
 *
 * Rules:
 *   - **Tag-level** keys (`table`, `a`, `code`, `h1`…) replace that element
 *     wherever it appears inside a block. Applied by converting the block's
 *     trusted HTML to a React tree.
 *   - **Block-kind** keys ({@link BlockKindTag}: `CodeBlock`, `Mermaid`,
 *     `Table`…) replace the whole block; the component gets
 *     {@link BlockComponentProps}.
 *   - **Open / speculative** blocks always render via `innerHTML` (their HTML
 *     is partial); a tag-level override takes effect once the block commits.
 *   - With no `components` prop the renderer takes the original fast
 *     `innerHTML` path — output is byte-identical to before.
 *   - **Memoize `components`** (or hoist it) if you define it inside a
 *     component — a fresh object identity each render busts the block memo and
 *     forces every block to re-parse on every patch.
 *   - For code blocks the built-in highlighter is the default; it is bypassed
 *     (so your override wins) when you provide `components.CodeBlock`,
 *     `components.pre`, or `components.code`.
 */

interface FluxMarkdownProps {
  /**
   * A caller-owned client (you drive `append`/`finalize` and own its lifecycle —
   * the component never destroys it). Mutually exclusive with `stream`; if both
   * are given, `client` wins (a dev warning fires).
   */
  client?: FluxClient;
  /**
   * A stream to render directly — the 1-line common case. Pass a `Response`, a
   * `ReadableStream<Uint8Array>`, or an `AsyncIterable<string>` (e.g. SSE
   * deltas) and the component owns an internal client, pipes the stream, and
   * destroys it on unmount. A new `stream` identity supersedes the old.
   */
  stream?: AsyncIterable<string> | ReadableStream<Uint8Array> | Response;
  /** Parser config for the internally-created client (stream mode only). */
  streamConfig?: ParserConfig;
  /** Called if piping the `stream` rejects (the source errored). Not the worker error channel. */
  onStreamError?: (err: Error) => void;
  components?: Components;
  /**
   * Skip layout/paint for off-screen blocks via CSS `content-visibility: auto`
   * — for very long documents (hundreds+ of blocks). Off by default. Applies
   * only to *closed* blocks (the streaming tail always renders fully). Keeps
   * nodes in the DOM; it cuts rendering cost, not node count.
   */
  virtualize?: boolean;
  /**
   * Render a bottom snap target so the view follows the streaming tail. This is
   * CSS-only: it emits a sentinel with `scroll-snap-align: end`; **you** add
   * `scroll-snap-type: y proximity` to your scroll container. The view then
   * follows the bottom as content streams in and releases when the user scrolls
   * up (and re-locks when they scroll back near the bottom). Off by default.
   */
  stickToBottom?: boolean;
  /**
   * Optional HTML sanitizer applied to every block's HTML before it is injected
   * via `innerHTML` — **including the streaming (open/speculative) tail**, the
   * path that raw `innerHTML` would otherwise expose. Pass a real sanitizer
   * (e.g. DOMPurify's `sanitize`) when rendering untrusted / LLM HTML with
   * `unsafeHtml` on. flux-md stays zero-dep — you bring the sanitizer. The
   * built-in code/math renderers operate on already-escaped content and are not
   * run through it. When omitted, rendering is byte-identical and zero-cost.
   *
   * **Memoize / hoist this** (same trap as `components`): a fresh closure each
   * render busts the per-block memo, so every block re-sanitizes and re-parses
   * on every patch instead of only the streaming tail.
   */
  sanitize?: (html: string) => string;
  /** Appended to the root's `className` (the `flux-md` class is always present). */
  className?: string;
  /** Set on the root element. */
  id?: string;
  /** Set on the root element (e.g. `"article"`, `"log"`). */
  role?: string;
  /**
   * Make the root a live region so screen readers announce streamed content.
   * `"polite"` (recommended) coalesces rapid updates and announces when the
   * reader is idle — it does **not** read every token. Off by default.
   */
  "aria-live"?: "off" | "polite" | "assertive";
  /** Live-region atomicity; pair with `aria-live`. Off by default. */
  "aria-atomic"?: boolean;
  /**
   * Optional render-churn probe. Fires once per ACTUAL render of a block —
   * never for a committed block that memo-skips on a tail-only patch. The
   * callback gets the block id and a {@link RenderMetrics} sample (per-block
   * `renderCount`, `speculativeToggleCount`, `lastRenderMs`, `kind`). Zero
   * overhead when omitted. **Memoize / hoist this** (same trap as `components`):
   * a fresh closure each render busts the per-block memo, forcing every block to
   * re-render on every patch.
   */
  onRenderMetrics?: RenderMetricsHook;
}

// The original render path: subscribe to a (required, caller- or hook-owned)
// client and render its blocks. NEVER creates or destroys a client.
function FluxMarkdownFromClient({
  client,
  components,
  virtualize,
  stickToBottom,
  sanitize,
  className,
  id,
  role,
  "aria-live": ariaLive,
  "aria-atomic": ariaAtomic,
  onRenderMetrics,
}: FluxMarkdownProps & { client: FluxClient }) {
  const blocks = useSyncExternalStore(client.subscribe, client.getSnapshot, client.getSnapshot);
  // Normalize "no overrides" to a stable `undefined` so memo comparisons and
  // the fast path don't churn on an empty object identity.
  const comps = components && Object.keys(components).length > 0 ? components : undefined;
  // Wrap the user hook so each fire also advances the client's aggregate
  // renderCount. Memoized on (client, hook) so its identity stays stable across
  // tail patches — a fresh closure would bust every block's memo. When no hook
  // is supplied this stays `undefined` and the whole probe path is skipped.
  const onMetrics = useMemo<RenderMetricsHook | undefined>(
    () =>
      onRenderMetrics
        ? (id, m) => {
            client.__noteRender();
            onRenderMetrics(id, m);
          }
        : undefined,
    [client, onRenderMetrics],
  );
  return (
    <div
      className={className ? `flux-md ${className}` : "flux-md"}
      id={id}
      role={role}
      aria-live={ariaLive}
      aria-atomic={ariaAtomic}
    >
      {blocks.map((b) => (
        <BlockView
          key={b.id}
          block={b}
          components={comps}
          virtualize={virtualize}
          sanitize={sanitize}
          onRenderMetrics={onMetrics}
        />
      ))}
      {stickToBottom && <div aria-hidden="true" style={{ scrollSnapAlign: "end" }} className="flux-bottom-anchor" />}
    </div>
  );
}

/**
 * Own a {@link FluxClient} for the lifetime of a component and drive it from a
 * `stream` (a `Response`, `ReadableStream<Uint8Array>`, or
 * `AsyncIterable<string>`). Returns the client (read `outline()` / `getMetrics()`
 * off it, or pass it to `<FluxMarkdown client={…} />`). The client is created
 * once and destroyed on unmount; a new `stream` identity supersedes the old
 * (the prior pipe is aborted, the parser is reset, the new stream is piped).
 *
 * Caveat (matches the manual `useEffect` form): a single-use stream — a
 * `Response`/`ReadableStream`, or an async generator — can only be consumed
 * once, so React **StrictMode**'s dev-only double-mount may truncate it in
 * development. Production mounts once and is unaffected. If you need dev-exact
 * streaming, drive a caller-owned client manually.
 */
export function useFluxStream(
  stream: AsyncIterable<string> | ReadableStream<Uint8Array> | Response | null | undefined,
  options?: { config?: ParserConfig; onError?: (err: Error) => void },
): FluxClient {
  // One client per hook instance. (React StrictMode double-invokes this
  // initializer in DEV, constructing a throwaway second client whose worker
  // slot isn't reclaimed — a minor dev-only artifact; production runs it once.
  // The committed client is what's used, and its lifecycle below is correct.)
  const [client] = useState(() => new FluxClient({ config: options?.config }));
  // Read onError through a ref so its identity never re-subscribes the stream.
  const onErrorRef = useRef(options?.onError);
  onErrorRef.current = options?.onError;
  // Track the last stream so we reset() only on a genuine source change — never
  // on a StrictMode replay of the same stream (which would discard its head).
  const prevStream = useRef<typeof stream>(undefined);

  // Own the client's pool attachment. On (re)mount, reattach (StrictMode's
  // dev double-mount destroys on the simulated unmount, then remounts the SAME
  // instance — without reattach its patches would be dropped and it'd render
  // blank); destroy on real unmount.
  useEffect(() => {
    client.reattach();
    return () => client.destroy();
  }, [client]);

  // Consume the current stream; supersede (abort, no finalize) on change/unmount.
  useEffect(() => {
    if (stream == null) return;
    const ac = new AbortController();
    if (prevStream.current !== undefined && prevStream.current !== stream) {
      client.reset(); // a different stream replaced a prior one
    }
    prevStream.current = stream;
    client.pipeFrom(stream, { signal: ac.signal }).catch((e) => {
      if (!ac.signal.aborted) {
        onErrorRef.current?.(e instanceof Error ? e : new Error(String(e)));
      }
    });
    return () => ac.abort();
  }, [stream, client]);

  return client;
}

/**
 * Own a {@link FluxClient} driven by a CONTROLLED full string — the bridge for
 * UIs that hold a streaming message as a single growing string prop (the common
 * React shape) rather than as a stream. Pass the whole document-so-far on each
 * render and {@link FluxClient.setContent} diffs it: a prefix-extension appends
 * only the delta; any divergence (e.g. the finished text swapped for a
 * re-processed final string) resets and reparses. Returns the owned client —
 * pass it to `<FluxMarkdown client={…} />` (and read `outline()` etc.).
 *
 * Pass `streaming: false` once the content is final to finalize the stream and
 * commit its last block (only then does a finished code fence highlight + show
 * its copy button). If `streaming` is omitted or `true` the stream is left OPEN
 * — right for a still-growing string, but a *complete static* string rendered as
 * `useFluxMarkdownString(md)` keeps its last block in the streaming state until
 * you pass `{ streaming: false }`. (Inferring "done" from an absent flag is
 * deliberately avoided: it would re-finalize on every token for callers that
 * grow the string without the flag — an O(n²) reparse trap.) The client is
 * created once and destroyed on unmount; StrictMode's dev double-mount is handled
 * (reattach re-feeds the document). For a true stream source
 * (`Response` / `ReadableStream` / SSE generator) use {@link useFluxStream}
 * instead — it avoids buffering the whole document as a string.
 */
export function useFluxMarkdownString(
  content: string,
  options?: { config?: ParserConfig; streaming?: boolean },
): FluxClient {
  const [client] = useState(() => new FluxClient({ config: options?.config }));

  // Own the client's pool attachment (StrictMode dev double-mount destroys on the
  // simulated unmount then remounts the SAME instance; reattach re-registers and
  // clears setContent's diff baseline so the document is re-fed). Destroy on the
  // real unmount.
  useEffect(() => {
    client.reattach();
    return () => client.destroy();
  }, [client]);

  // Reconcile the parser to the controlled string. setContent diffs internally,
  // so this stays correct whether `content` grows by a token or is swapped wholesale.
  useEffect(() => {
    client.setContent(content, { done: options?.streaming === false });
  }, [client, content, options?.streaming]);

  return client;
}

// Stream mode: own a client via the hook, then render the normal client path.
function FluxMarkdownFromStream(props: FluxMarkdownProps) {
  const client = useFluxStream(props.stream, {
    config: props.streamConfig,
    onError: props.onStreamError,
  });
  return <FluxMarkdownFromClient {...props} client={client} />;
}

// Dispatch by rendering one of two SIBLING components (never a hook in a branch,
// which would violate the Rules of Hooks): `stream` mode owns a client, `client`
// mode uses the caller's. `memo` skips re-render when props are unchanged. If
// both are given `client` wins (it owns the lifecycle); passing neither is a
// usage error and throws (rather than crashing cryptically downstream).
function FluxMarkdownImpl(props: FluxMarkdownProps) {
  if (props.stream != null && props.client == null) {
    return <FluxMarkdownFromStream {...props} />;
  }
  if (props.client == null) {
    throw new Error("<FluxMarkdown>: pass either a `client` or a `stream` prop.");
  }
  return <FluxMarkdownFromClient {...(props as FluxMarkdownProps & { client: FluxClient })} />;
}

export const FluxMarkdown = memo(FluxMarkdownImpl);

function decodeEntities(s: string): string {
  return s
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, "&");
}

function decodeCodeText(html: string): string {
  const m = html.match(/<pre><code[^>]*>([\s\S]*?)<\/code><\/pre>/);
  return m ? decodeEntities(m[1]) : "";
}

/**
 * The LaTeX source for a MathBlock. Display math (`$$…$$` / `\[…\]`) renders as
 * `<div class="math math-display">…</div>`; a fenced ```math block renders as
 * `<pre><code>…</code></pre>`. Either way the body is the HTML-escaped LaTeX —
 * decode it back so a `components.MathBlock` override gets the raw source.
 */
function decodeMathText(html: string): string {
  const d = html.match(/<div class="math math-display">([\s\S]*?)<\/div>/);
  if (d) return decodeEntities(d[1]);
  return decodeCodeText(html);
}

export function blockKindProps(block: Block, components?: Components): BlockComponentProps {
  const props: BlockComponentProps = {
    block,
    html: block.html,
    open: block.open,
    speculative: block.speculative,
  };
  const data = block.kind.data as
    | { lang?: string | null; code?: string; latex?: string; start?: number; ordered?: boolean; tag?: string; attrs?: [string, string][] }
    | undefined;
  if (block.kind.type === "CodeBlock") {
    // Prefer the structured `code` (present when blockData is on) over the HTML
    // regex — the lossless decoded source. Fall back to the regex when off.
    props.text = data?.code ?? decodeCodeText(block.html);
    props.language = data?.lang ?? "";
    if (typeof data?.code === "string") {
      props.code = { lang: data.lang ?? null, code: data.code };
    }
  } else if (block.kind.type === "MathBlock") {
    props.text = data?.latex ?? decodeMathText(block.html);
    if (typeof data?.latex === "string") {
      props.math = { latex: data.latex };
    }
  } else if (block.kind.type === "List") {
    if (data && typeof data.start === "number") {
      props.list = { ordered: !!data.ordered, start: data.start };
    }
  } else if (block.kind.type === "Component") {
    props.tag = data?.tag ?? "";
    // React-form attribute names, so `{...attrs}` spreads cleanly onto an element
    // (HTML `class`/`for` → React `className`/`htmlFor`).
    props.attrs = reactAttrs(data?.attrs ?? []);
    // An override replaces the `<tag>` wrapper, so it gets the *inner* HTML
    // (markdown already rendered) rather than the full wrapped block.
    props.html = componentInnerHtml(block.html, props.tag);
    // Convenience: the inner markdown pre-parsed to a React tree (with nested
    // tag/inline-component overrides applied). Render `{children}` directly
    // instead of dangerouslySetInnerHTML-ing `html` — the easy, correct path.
    props.children = htmlToReact(props.html, components ?? {});
  } else if (block.kind.type === "Table") {
    // Pure structured data (present only when `blockData` is on) — unlike
    // `attrs` there is no React/DOM name-form divergence, so this is the same
    // line as block-props.ts's branch.
    props.table = block.kind.data as TableData | undefined;
  } else if (block.kind.type === "Heading") {
    // When `blockData` is on, `kind.data` is `{ level, text, id }`; off, it is the
    // bare level `number`. Surface the rich object only (mirrors block-props.ts).
    if (typeof block.kind.data === "object" && block.kind.data !== null) {
      props.heading = block.kind.data as HeadingData;
    }
  }
  return props;
}

// Prototype-free so a key like `constructor`/`hasOwnProperty` returns undefined
// (and the `?? k` fallback fires) instead of an inherited Object.prototype member.
const REACT_ATTR_NAME: Record<string, string> = Object.assign(Object.create(null), {
  class: "className",
  for: "htmlFor",
});

// React-meaningful prop names that must never survive into a user override's
// attrs object (dangerouslySetInnerHTML crashes the render tree; ref/key/etc.
// inject internals). Mirrors html-to-react's PROP_DENY.
const ATTR_DENY = new Set([
  "dangerouslysetinnerhtml", "ref", "key", "defaultvalue", "defaultchecked",
  "suppresshydrationwarning", "suppresscontenteditablewarning",
]);

// Forward only plain HTML attribute identifiers (the REACT_ATTR_NAME renames
// pass too), so weird casings / `__proto__` / `constructor` never reach a prop.
const SAFE_ATTR_NAME = /^[a-z][a-z0-9-]*$/i;

/** Convert sanitized HTML attribute pairs into a React-spreadable object,
 *  renaming the two names React requires (`class`→`className`, `for`→`htmlFor`).
 *  Other names (including `data-*` / `aria-*`) pass through unchanged. Drops
 *  inline event handlers and React-meaningful/unsafe names as defense-in-depth
 *  (the Rust `sanitize_attrs` is the primary gate; this keeps the React layer
 *  safe on its own when attrs are handed to user override components). */
function reactAttrs(pairs: [string, string][]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of pairs) {
    const lower = k.toLowerCase();
    if (lower.startsWith("on")) continue;
    if (ATTR_DENY.has(lower)) continue;
    if (!(lower in REACT_ATTR_NAME) && !SAFE_ATTR_NAME.test(k)) continue;
    out[REACT_ATTR_NAME[lower] ?? k] = v;
  }
  return out;
}

/** Strip the `<tag …>` open and trailing `</tag>` from a component block's HTML,
 *  leaving the inner (already-rendered markdown) HTML. Handles open (unclosed)
 *  blocks, where there is no close tag yet. */
function componentInnerHtml(html: string, tag: string): string {
  const gt = html.indexOf(">");
  if (gt < 0) return "";
  let inner = html.slice(gt + 1);
  const close = `</${tag}>`;
  if (inner.endsWith(close)) inner = inner.slice(0, -close.length);
  return inner.replace(/^\n/, "").replace(/\n$/, "");
}

/** Convert a closed block's HTML to a React tree, memoized on html+components. */
function SafeHtml({ html, components }: { html: string; components: Components }) {
  // `ReactElement` (not the global `JSX.Element`) so the source type-checks under
  // both @types/react 18 and 19 — React 19 removed the global `JSX` namespace,
  // and a consumer's `next build` type-checks this shipped source.
  return useMemo(() => htmlToReact(html, components), [html, components]) as ReactElement;
}

// Per-kind off-screen size estimate for `contain-intrinsic-size` — keeps the
// scrollbar stable while a block is layout-skipped. Wrong by 2× is fine; the
// `auto` keyword makes the browser remember the real size once rendered.
const INTRINSIC_PX: Record<string, number> = {
  Paragraph: 80, Heading: 44, CodeBlock: 300, MathBlock: 140, Mermaid: 220,
  List: 120, Blockquote: 100, Alert: 120, Table: 200, Rule: 24, Html: 80,
  Component: 120,
};

function BlockViewImpl(props: {
  block: Block;
  components?: Components;
  virtualize?: boolean;
  sanitize?: (html: string) => string;
  onRenderMetrics?: RenderMetricsHook;
}) {
  const { block, virtualize, onRenderMetrics } = props;
  // Render-churn probe (only when a hook is wired — refs are cheap, but the
  // measurement + hook call below are guarded so the no-hook path is untouched).
  // Reaching this body at all means React did NOT memo-skip, so a committed
  // block fires exactly once and the streaming tail fires per patch — by design.
  const metricsRef = useRef<{ renderCount: number; toggle: number; speculative: boolean } | null>(
    onRenderMetrics ? { renderCount: 0, toggle: 0, speculative: block.speculative } : null,
  );
  const hasPerf = typeof performance !== "undefined";
  const t0 = onRenderMetrics && hasPerf ? performance.now() : 0;

  const content = renderBlockContent(props);

  if (onRenderMetrics) {
    // Lazily init if the hook was added after mount (initial useRef value only
    // applies on first render).
    const m = (metricsRef.current ??= { renderCount: 0, toggle: 0, speculative: block.speculative });
    m.renderCount++;
    if (m.speculative !== block.speculative) {
      m.toggle++;
      m.speculative = block.speculative;
    }
    onRenderMetrics(block.id, {
      renderCount: m.renderCount,
      speculativeToggleCount: m.toggle,
      lastRenderMs: hasPerf ? performance.now() - t0 : 0,
      kind: block.kind.type,
    });
  }
  // Virtualize only *closed* blocks: the streaming tail (open/speculative) is
  // where the user looks and where heights change fastest — deferring it there
  // causes flicker. A uniform wrapper covers every kind, including dedicated
  // renderers and block-kind overrides.
  if (virtualize && !block.open && !block.speculative) {
    const px = INTRINSIC_PX[block.kind.type] ?? 120;
    return (
      <div style={{ contentVisibility: "auto", containIntrinsicSize: `auto ${px}px` } as CSSProperties}>
        {content}
      </div>
    );
  }
  return content;
}

function renderBlockContent({
  block,
  components,
  sanitize,
}: {
  block: Block;
  components?: Components;
  sanitize?: (html: string) => string;
}) {
  const kind = block.kind.type;

  // Block-kind override replaces the entire renderer for this block. A
  // `Component` block also dispatches on its tag name, so `components.Thinking`
  // (the specific tag) wins over `components.Component` (the generic fallback).
  if (components) {
    if (kind === "Component") {
      const tag = (block.kind.data as { tag?: string } | undefined)?.tag;
      const override = (tag && components[tag]) || components.Component;
      if (override) {
        return createElement(override, blockKindProps(block, components));
      }
    }
    const blockOverride = components[kind];
    if (blockOverride) {
      return createElement(blockOverride, blockKindProps(block, components));
    }
  }

  // Dedicated renderers for code / math / mermaid. Code blocks fall through to
  // the generic (override-aware) path if the user supplied a pre/code override.
  switch (kind) {
    case "CodeBlock": {
      const wantsCodeOverride = !!components && (!!components.pre || !!components.code);
      if (!wantsCodeOverride) return <CodeBlock html={block.html} open={block.open} />;
      break; // fall through to generic override-aware rendering
    }
    case "MathBlock":
      return <MathBlock html={block.html} open={block.open} />;
    case "Mermaid":
      return <Mermaid html={block.html} open={block.open} />;
  }

  const className =
    "flux-block flux-block-" +
    kind.toLowerCase() +
    (block.open ? " flux-open" : "") +
    (block.speculative ? " flux-speculative" : "");

  // Tag-level / inline overrides apply to OPEN and speculative blocks too, not
  // just settled ones: the streaming tail's HTML is always well-formed (the
  // parser speculatively closes it), so a design-system renderer (Tailwind
  // classes on p/ul/li, inline <a>/<code> overrides) stays styled mid-stream
  // instead of only after a block commits. A supplied `sanitize` runs FIRST
  // (same as the innerHTML path below), so overrides compose with sanitization on
  // every block — closing the gap where a component-rendered block previously
  // bypassed the user sanitizer. The no-`components` fast path is untouched
  // (byte-identical innerHTML).
  if (components) {
    const safe = sanitize ? sanitize(block.html) : block.html;
    return (
      <div className={className}>
        <SafeHtml html={safe} components={components} />
      </div>
    );
  }

  return (
    <div
      className={className}
      dangerouslySetInnerHTML={{ __html: sanitize ? sanitize(block.html) : block.html }}
    />
  );
}

// A block is the same render when its identity, HTML, open-state, and the
// active components map are all unchanged. Exported for tests: this predicate
// is what stops a committed block from re-rendering (and thus re-parsing) on
// every streaming patch.
export function blocksEqual(
  prev: { block: Block; components?: Components; virtualize?: boolean; sanitize?: (html: string) => string; onRenderMetrics?: RenderMetricsHook },
  next: { block: Block; components?: Components; virtualize?: boolean; sanitize?: (html: string) => string; onRenderMetrics?: RenderMetricsHook },
): boolean {
  return (
    prev.block.id === next.block.id &&
    prev.block.html === next.block.html &&
    prev.block.open === next.block.open &&
    prev.block.speculative === next.block.speculative &&
    prev.components === next.components &&
    prev.virtualize === next.virtualize &&
    prev.sanitize === next.sanitize &&
    prev.onRenderMetrics === next.onRenderMetrics
  );
}

const BlockView = memo(BlockViewImpl, blocksEqual);

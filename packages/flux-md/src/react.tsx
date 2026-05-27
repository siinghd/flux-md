import { createElement, memo, useMemo, useSyncExternalStore, type CSSProperties } from "react";
import type { Block, BlockComponentProps, Components } from "./types";
import type { FluxClient } from "./client";
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
  client: FluxClient;
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
}

function FluxMarkdownImpl({ client, components, virtualize, stickToBottom }: FluxMarkdownProps) {
  const blocks = useSyncExternalStore(client.subscribe, client.getSnapshot, client.getSnapshot);
  // Normalize "no overrides" to a stable `undefined` so memo comparisons and
  // the fast path don't churn on an empty object identity.
  const comps = components && Object.keys(components).length > 0 ? components : undefined;
  return (
    <div className="flux-md">
      {blocks.map((b) => (
        <BlockView key={b.id} block={b} components={comps} virtualize={virtualize} />
      ))}
      {stickToBottom && <div aria-hidden="true" style={{ scrollSnapAlign: "end" }} className="flux-bottom-anchor" />}
    </div>
  );
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

function blockKindProps(block: Block): BlockComponentProps {
  const props: BlockComponentProps = {
    block,
    html: block.html,
    open: block.open,
    speculative: block.speculative,
  };
  const data = block.kind.data as
    | { lang?: string | null; tag?: string; attrs?: [string, string][] }
    | undefined;
  if (block.kind.type === "CodeBlock") {
    props.text = decodeCodeText(block.html);
    props.language = data?.lang ?? "";
  } else if (block.kind.type === "MathBlock") {
    props.text = decodeMathText(block.html);
  } else if (block.kind.type === "Component") {
    props.tag = data?.tag ?? "";
    // React-form attribute names, so `{...attrs}` spreads cleanly onto an element
    // (HTML `class`/`for` → React `className`/`htmlFor`).
    props.attrs = reactAttrs(data?.attrs ?? []);
    // An override replaces the `<tag>` wrapper, so it gets the *inner* HTML
    // (markdown already rendered) rather than the full wrapped block.
    props.html = componentInnerHtml(block.html, props.tag);
  }
  return props;
}

const REACT_ATTR_NAME: Record<string, string> = { class: "className", for: "htmlFor" };

/** Convert sanitized HTML attribute pairs into a React-spreadable object,
 *  renaming the two names React requires (`class`→`className`, `for`→`htmlFor`).
 *  Other names (including `data-*` / `aria-*`) pass through unchanged. */
function reactAttrs(pairs: [string, string][]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of pairs) out[REACT_ATTR_NAME[k] ?? k] = v;
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
  return useMemo(() => htmlToReact(html, components), [html, components]) as JSX.Element;
}

// Per-kind off-screen size estimate for `contain-intrinsic-size` — keeps the
// scrollbar stable while a block is layout-skipped. Wrong by 2× is fine; the
// `auto` keyword makes the browser remember the real size once rendered.
const INTRINSIC_PX: Record<string, number> = {
  Paragraph: 80, Heading: 44, CodeBlock: 300, MathBlock: 140, Mermaid: 220,
  List: 120, Blockquote: 100, Alert: 120, Table: 200, Rule: 24, Html: 80,
  Component: 120,
};

function BlockViewImpl(props: { block: Block; components?: Components; virtualize?: boolean }) {
  const { block, virtualize } = props;
  const content = renderBlockContent(props);
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

function renderBlockContent({ block, components }: { block: Block; components?: Components }) {
  const kind = block.kind.type;

  // Block-kind override replaces the entire renderer for this block. A
  // `Component` block also dispatches on its tag name, so `components.Thinking`
  // (the specific tag) wins over `components.Component` (the generic fallback).
  if (components) {
    if (kind === "Component") {
      const tag = (block.kind.data as { tag?: string } | undefined)?.tag;
      const override = (tag && components[tag]) || components.Component;
      if (override) {
        return createElement(override, blockKindProps(block));
      }
    }
    const blockOverride = components[kind];
    if (blockOverride) {
      return createElement(blockOverride, blockKindProps(block));
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

  // Tag-level overrides only apply to a settled block (open/speculative blocks
  // have partial HTML we must not feed to the parser).
  if (components && !block.open && !block.speculative) {
    return (
      <div className={className}>
        <SafeHtml html={block.html} components={components} />
      </div>
    );
  }

  return <div className={className} dangerouslySetInnerHTML={{ __html: block.html }} />;
}

// A block is the same render when its identity, HTML, open-state, and the
// active components map are all unchanged. Exported for tests: this predicate
// is what stops a committed block from re-rendering (and thus re-parsing) on
// every streaming patch.
export function blocksEqual(
  prev: { block: Block; components?: Components; virtualize?: boolean },
  next: { block: Block; components?: Components; virtualize?: boolean },
): boolean {
  return (
    prev.block.id === next.block.id &&
    prev.block.html === next.block.html &&
    prev.block.open === next.block.open &&
    prev.block.speculative === next.block.speculative &&
    prev.components === next.components &&
    prev.virtualize === next.virtualize
  );
}

const BlockView = memo(BlockViewImpl, blocksEqual);

export type BlockKindTag =
  | "Paragraph"
  | "Heading"
  | "CodeBlock"
  | "MathBlock"
  | "Mermaid"
  | "List"
  | "Blockquote"
  | "Alert"
  | "Table"
  | "Rule"
  | "Html"
  | "Component";

export interface BlockKind {
  type: BlockKindTag;
  data?: unknown;
}

/**
 * The node type a {@link Decorator} (or `wrapLink`) builds. Kept `unknown` here
 * so this framework-neutral types module stays React-free: the React binding
 * treats it as `ReactNode`, the DOM binding (`flux-md/dom`) as `Node | string`.
 */
export type FluxNode = unknown;

/**
 * Wrap or replace matched inline **text** while streaming, in O(n). A decorator
 * runs POST-PARSE on real inline TEXT nodes only (after the core renders a block
 * to HTML and the walker parses it), once per committed block — so it never sees
 * URLs, code, or markup, and a value split by inline markup (e.g.
 * `$2.<em>5</em>B`) is two text nodes and won't match across them.
 *
 * **Trusted surface (read this).** A decorator's `replace` output is spliced
 * directly into the render tree and does **NOT** pass through flux's attribute
 * sanitizer (that only runs on attributes the trusted core emitted). React and
 * the DOM both happily render a `javascript:` href. Treat `decorators` exactly
 * like `components`: only build trusted nodes, and route any link href through
 * the exported `safeUrl` (or use the `wrapLink` helper, which does it for you).
 *
 * **Stability matters (the #1 footgun).** Pass a HOISTED / memoized array — a
 * fresh `decorators` identity every render busts the per-block memo, so every
 * committed block re-parses and re-decorates on every patch (O(n²)). The React
 * binding emits a one-time dev warning if the identity changes.
 */
export interface Decorator {
  /** Tested against each inline TEXT node's string only (never URLs/code/markup). */
  match: RegExp | string;
  /** PURE fn building the replacement for ONE match. Returns framework nodes. */
  replace: (matchText: string, groups: string[]) => FluxNode;
  /** Ancestor tags to skip. Default `['a','code','pre','kbd']`. */
  skipInside?: string[];
}

/**
 * Rewrite a URL attribute (`href`/`src`/`poster`) as a block renders — e.g. to
 * proxy images or add UTM params. Applied O(1) per attribute. The renderer
 * re-sanitizes the OUTPUT (`safeUrl(urlTransform(safeUrl(value)))`), so a buggy
 * or hostile transform can never emit a `javascript:` / `data:text/html` URL
 * that reaches the DOM. Like `decorators`, pass a HOISTED / memoized function so
 * the per-block memo holds.
 */
export type UrlTransform = (
  url: string,
  ctx: { tag: string; attr: "href" | "src" | "poster" },
) => string;

/** Column alignment from the `|:--|:-:|--:|` delimiter row; `null` = unset. */
export type Align = "left" | "center" | "right" | null;

/**
 * One table cell as STRUCTURED DATA (opt-in via {@link ParserConfig.blockData}).
 * `text` is the inline-stripped plaintext — sort/filter/CSV/chart from DATA,
 * with no HTML re-parse. `html` is the inline-rendered display markup, byte-for-
 * byte the inline content inside the matching `<td>`/`<th>` of `block.html`.
 */
export interface TableCell {
  text: string;
  html: string;
}

/**
 * A Table block's `kind.data` when {@link ParserConfig.blockData} is on. Lets a
 * consumer build a sort/filter/transpose/chart/CSV toolbar from DATA alone —
 * no HAST tree, no HTML re-parse. `aligns[i]` is column `i`'s alignment.
 */
export interface TableData {
  headers: TableCell[];
  rows: TableCell[][];
  aligns: Align[];
}

/**
 * A Heading block's `kind.data` when {@link ParserConfig.blockData} is on. Lets a
 * consumer build a table of contents — nested by `level`, anchored by `id` — from
 * DATA alone, with no HTML re-parse. `text` is the inline-stripped plaintext (the
 * heading rendered to plain text, e.g. `## **Bold** & x` → `"Bold & x"`); `id` is
 * a GitHub-style anchor slug of that text (`"bold-x"`) for `#`-links. When
 * `blockData` is off, a Heading's `kind.data` is instead the bare level `number`
 * (byte-identical to before), so consumers reading `kind.data` must accept the
 * `number | HeadingData` union.
 *
 * v1: duplicate heading texts produce identical slugs (no document-wide dedup
 * counter yet) — give same-named headings distinct text if unique anchors matter.
 */
export interface HeadingData {
  level: number;
  text: string;
  id: string;
}

/**
 * A CodeBlock's `kind.data` when {@link ParserConfig.blockData} is on. `lang` is
 * the always-on info-string language (`null` for none); `code` is the opt-in
 * DECODED source inside `<pre><code>…</code></pre>` (only present when `blockData`
 * is on). Build a copy-to-clipboard string / re-highlight from `code` alone — no
 * HTML re-parse, no entity-decode. When `blockData` is off, `code` is absent and
 * `kind.data` is just `{ lang }`, byte-identical to before.
 */
export interface CodeBlockData {
  lang: string | null;
  code?: string;
}

/**
 * A MathBlock's `kind.data` when {@link ParserConfig.blockData} is on. `latex` is
 * the DECODED LaTeX source (the display-math body, entity-decoded). Re-render with
 * KaTeX from `latex` alone — no HTML re-parse. When `blockData` is off, a
 * MathBlock has no `kind.data` at all (byte-identical to before).
 */
export interface MathBlockData {
  latex: string;
}

/**
 * One list item in {@link ListData.items}. `html` is the inline-rendered inner
 * HTML of the item's `<li>` (byte-identical to the content between the matching
 * `<li…>`/`</li>` in `block.html`), so a keyed renderer can stamp one node per
 * item and reuse the unchanged items while the list streams.
 */
export interface ListItemData {
  html: string;
}

/**
 * A List's `kind.data` when {@link ParserConfig.blockData} is on. `ordered` is the
 * always-on flag; `start` is the opt-in ordered-list start number (the `start="N"`
 * HTML attribute; `1` for an unordered list), only present when `blockData` is on.
 * `items` carries each item's inner `<li>` HTML — present (and non-empty) only when
 * `blockData` is on — so a keyed renderer can re-render only the items that changed
 * since the last patch instead of the whole list's HTML. Renumber / continue a
 * split list from `start` alone — no HTML re-parse. When `blockData` is off, `start`
 * and `items` are absent and `kind.data` is just `{ ordered }`, byte-identical.
 */
export interface ListData {
  ordered: boolean;
  start?: number;
  items?: ListItemData[];
}

/**
 * One inner sub-block of a `Blockquote` / `Alert` as STRUCTURED DATA (opt-in via
 * {@link ParserConfig.blockData}). `html` is that sub-block's pre-rendered display
 * markup (e.g. `<p>…</p>`), byte-for-byte the matching fragment inside the
 * container's `block.html` wrapper.
 */
export interface NestedBlock {
  html: string;
}

/**
 * A `Blockquote`'s `kind.data` (and the `nested` carrier inside an `Alert`'s data)
 * when {@link ParserConfig.blockData} is on. `nested` is the ordered list of the
 * container's inner sub-blocks, each as its own pre-rendered HTML. A
 * `components.Blockquote` / `components.Alert` override can render these KEYED (one
 * node per entry) so that while the container streams only its last (open) inner
 * block re-renders each tick — committed inner blocks have stable HTML and memoize.
 * When `blockData` is off, a Blockquote has no `kind.data` and an Alert's is just
 * `{ kind }` (byte-identical to before).
 */
export interface ContainerData {
  nested: NestedBlock[];
}

export interface Block {
  id: number;
  kind: BlockKind;
  start: number;
  end: number;
  html: string;
  open: boolean;
  speculative: boolean;
}

export interface Patch {
  newly_committed: Block[];
  active: Block[];
}

/**
 * Per-block render-churn sample passed to an {@link RenderMetricsHook}. Lets you
 * measure how often each block actually re-renders / rebuilds (committed blocks
 * memo-skip, so they fire exactly once; the streaming tail fires per patch).
 */
export interface RenderMetrics {
  /** How many times THIS block has actually rendered/rebuilt so far (≥ 1). */
  renderCount: number;
  /** How many times this block's `speculative` flag flipped between renders. */
  speculativeToggleCount: number;
  /** Wall-clock duration of this render's body in ms (0 if `performance` absent). */
  lastRenderMs: number;
  /** The block's kind (`"Paragraph"`, `"CodeBlock"`, …). */
  kind: string;
}

/**
 * Optional observability probe. When supplied to the React renderer (the
 * `onRenderMetrics` prop) or the DOM renderer ({@link MountOptions.onRenderMetrics}),
 * it fires once per ACTUAL render/rebuild of a block — never for a committed
 * block that memo-skips. Zero overhead when absent (no counters advance, the hook
 * path is never entered).
 */
export type RenderMetricsHook = (blockId: number, m: RenderMetrics) => void;

/** Props passed to a block-kind override (e.g. `components.CodeBlock`). */
export interface BlockComponentProps {
  /** The full parsed block, including `kind` (with `kind.data`) and offsets. */
  block: Block;
  /**
   * Rendered, XSS-safe HTML for this block. For `Component` blocks this is the
   * **inner** rendered-markdown HTML (not the `<tag>…</tag>` wrapper). NOTE: a
   * `Component` override that ignores both `html` and `children` renders empty —
   * use {@link children} (the easy path) or `dangerouslySetInnerHTML={{__html:
   * html}}`.
   */
  html: string;
  /**
   * React only: this block's inner content already parsed to a React node tree
   * (markdown rendered, nested tag/inline-component overrides applied). For a
   * `Component` block it is the inner markdown — render it directly
   * (`return <Chip {...attrs}>{children}</Chip>`) instead of dangerously setting
   * `html`. Populated by `<FluxMarkdown>` / `<FluxMarkdownStatic>` when a
   * `components` map is supplied; DOM and other bindings leave it `undefined`
   * (they consume `html`). Typed `unknown` to keep this surface framework-neutral
   * — cast to `ReactNode` in a React override.
   */
  children?: unknown;
  /** True while the block is still streaming (its HTML may still change). */
  open: boolean;
  /** True if the block was closed speculatively and may yet be revised. */
  speculative: boolean;
  /** Decoded source text — present for `CodeBlock` / `MathBlock`. */
  text?: string;
  /** Info-string language — present for `CodeBlock` (from `kind.data.lang`). */
  language?: string;
  /** Component tag name — present for `Component` blocks (from `kind.data.tag`). */
  tag?: string;
  /**
   * Sanitized attributes — present for `Component` blocks. The name-form depends
   * on the consumer: the JSX renderer maps `class`→`className`/`for`→`htmlFor`
   * so `{...attrs}` spreads cleanly onto an element; the DOM renderer keeps the
   * literal HTML names (`class`/`for`) because it applies them via
   * `setAttribute`. For `Component` blocks, `html` is the **inner**
   * rendered-markdown HTML (not the `<tag>…</tag>` wrapper), so an override can
   * wrap it itself.
   */
  attrs?: Record<string, string>;
  /**
   * Structured table data — present for `Table` blocks when
   * {@link ParserConfig.blockData} is on (otherwise `undefined`). Equivalent to
   * `block.kind.data`, given a typed, documented name. `{ headers, rows, aligns }`
   * with each cell carrying `text` (plaintext, for sort/filter/CSV/chart) and
   * `html` (display). Build a sort/filter/transpose/chart/CSV toolbar from DATA —
   * no HTML re-parse, no HAST tree.
   */
  table?: TableData;
  /**
   * Structured heading data — present for `Heading` blocks when
   * {@link ParserConfig.blockData} is on (otherwise `undefined`). `{ level, text,
   * id }` with `text` the inline-stripped plaintext and `id` a GitHub-style anchor
   * slug. Build a table of contents (nested by `level`, anchored by `id`) from
   * DATA — no HTML re-parse.
   */
  heading?: HeadingData;
  /**
   * Structured code data — present for `CodeBlock` blocks when
   * {@link ParserConfig.blockData} is on (otherwise `undefined`). `{ lang, code }`
   * with `code` the DECODED source. Build a copy-to-clipboard string / re-highlight
   * from `code` — no HTML re-parse, no entity-decode. (`props.text` / `props.language`
   * carry the same source / lang and stay populated even when off, via the HTML
   * regex fallback.)
   */
  code?: CodeBlockData;
  /**
   * Structured math data — present for `MathBlock` blocks when
   * {@link ParserConfig.blockData} is on (otherwise `undefined`). `{ latex }` — the
   * DECODED LaTeX source. Re-render with KaTeX from `latex` — no HTML re-parse.
   * (`props.text` carries the same source and stays populated even when off, via
   * the HTML regex fallback.)
   */
  math?: MathBlockData;
  /**
   * Structured list data — present for `List` blocks when
   * {@link ParserConfig.blockData} is on (otherwise `undefined`). `{ ordered,
   * start }` — renumber / continue a split list from `start` (the ordered-list
   * start number) without re-parsing the `<ol start=…>` attribute.
   */
  list?: ListData;
  /**
   * Structured container data — present for `Blockquote` / `Alert` blocks when
   * {@link ParserConfig.blockData} is on (otherwise `undefined`). `{ nested }` —
   * the ordered pre-rendered HTML of each inner sub-block. The default renderers
   * use this to render the children KEYED (one node per entry) so that while the
   * container streams, only its open last inner block re-renders each tick.
   */
  container?: ContainerData;
}

/**
 * Per-stream parser configuration. Omitted fields use the library defaults
 * (autolinks + alerts on, raw HTML escaped, footnotes off) — so the default
 * `new FluxClient()` behaves exactly as before. Config is applied when the
 * stream's parser is created and is **immutable** for that stream's lifetime
 * (a `reset()` keeps it; use a new client for different flags).
 */
export interface ParserConfig {
  /** GFM extended autolinks (bare www./http(s)://ftp:// + emails). Default true. */
  gfmAutolinks?: boolean;
  /** GitHub alerts (`> [!NOTE]` → callouts). Default true. */
  gfmAlerts?: boolean;
  /** GFM footnotes (`[^1]` + `[^1]:` → footnote section). Default false. */
  gfmFootnotes?: boolean;
  /**
   * Math: `$…$` / `\(…\)` inline and `$$…$$` / `\[…\]` display. Default false
   * (so `$` in prose / currency stays literal). Emits KaTeX-ready markup
   * (`<span class="math math-inline">` / `<div class="math math-display">`)
   * carrying the LaTeX — bring your own KaTeX pass (flux-md stays zero-dep).
   */
  gfmMath?: boolean;
  /**
   * Emit `dir="auto"` on block-level text elements (`p`, `h1`–`h6`,
   * `blockquote`, `ul`/`ol`/`li`, `table`) so the browser detects each block's
   * direction independently — correct for documents mixing English with
   * Arabic/Hebrew. Default false; code blocks always stay LTR. Recommended for
   * apps that render RTL or mixed-direction content.
   */
  dirAuto?: boolean;
  /**
   * Opt-in accessibility markup that deviates from strict GFM byte-output:
   * wraps a task-list checkbox + its text in a `<label>` (programmatic
   * association for screen readers) and adds `scope="col"` to table header
   * cells. Default false (so CommonMark/GFM conformance output is unchanged).
   */
  a11y?: boolean;
  /** Pass raw HTML through unescaped. Default false. **Never enable for untrusted input.** */
  unsafeHtml?: boolean;
  /**
   * Opt-in allowlist of custom component tag names (e.g. `["Thinking",
   * "Callout"]`). A `<Tag>…</Tag>` whose name is listed renders as a component
   * whose inner content is parsed as **markdown** — safely, without `unsafeHtml`
   * (the tag is allowlisted and its attributes are sanitized: event handlers
   * dropped, dangerous URL schemes neutralized). The block is dispatched by the
   * renderer via `components[tag]` (or `components.Component`). Empty/omitted =
   * off. Names match case-sensitively.
   */
  componentTags?: string[];
  /**
   * Opt-in allowlist of INLINE component tag names (e.g. `["tik", "cite"]`). An
   * allowlisted `<tik>…</tik>` (or self-closing `<tik/>`) anywhere in inline
   * content — paragraphs, headings, table cells, list items — renders as a real
   * custom element with **markdown** inner content and sanitized attributes
   * (event handlers dropped, dangerous URL schemes neutralized) — XSS-safe
   * without `unsafeHtml`. The React renderer dispatches it via `components[tag]`,
   * with the inner markdown as the component's `children` and the sanitized
   * attributes as props. Separate from `componentTags` (block containers): list a
   * tag here for inline chips (tickers, citations, @mentions), or in both lists
   * to allow both positions. Names match **case-sensitively** and dispatch
   * verbatim to `components[tag]` (e.g. `"Cite"` → `components.Cite`), same as
   * `componentTags`. Empty/omitted = off.
   */
  inlineComponentTags?: string[];
  /**
   * Opt-in **safe raw-HTML allowlist**. Setting this (even to `[]`) engages a
   * sanitizer that renders a safe subset of *inline* raw HTML **without**
   * `unsafeHtml`: an **empty** array means "allow all tags except a built-in
   * dangerous set" (`script`, `style`, `iframe`, `object`, `embed`, `form`,
   * `input`, `svg`, …); a **non-empty** array renders only those tags (e.g.
   * `["br","sub","sup"]`) and escapes the rest. Every rendered tag's attributes
   * are sanitized (event handlers dropped, dangerous URL schemes → `#`), and HTML
   * comments are dropped. Block-level raw HTML stays escaped (sanitize is
   * inline-scoped for now). Unset/omitted = off (raw HTML handling unchanged).
   * Matching is case-insensitive. See also {@link dropHtmlTags}.
   */
  htmlAllowlist?: string[];
  /**
   * Tags removed entirely (markup dropped; any text between an open/close pair
   * stays as inert text) — e.g. app marker tags, or belt-and-suspenders
   * `["script","style"]`. Setting this (even to `[]`) also engages the safe
   * raw-HTML sanitizer (see {@link htmlAllowlist}). Case-insensitive.
   */
  dropHtmlTags?: string[];
  /**
   * Opt-in structured table data. When on, a `Table` block's `kind.data` is
   * populated with `{ headers, rows, aligns }` (each cell `{ text, html }`) so a
   * consumer can build a sort/filter/transpose/chart/CSV toolbar from DATA — no
   * HTML re-parse, no HAST tree. Default false (non-users pay zero allocation /
   * serde bytes; output and the `kind` serde shape stay byte-identical when off).
   */
  blockData?: boolean;
}

// Each message carries a `streamId` so one worker can multiplex many parsers
// (the worker pool). `ready` is the exception — it's worker-level (WASM loaded),
// not stream-level. The first message for a stream may carry `config`, applied
// when that stream's parser is created.
// `epoch` is a per-stream generation counter the client bumps on reset(): the
// worker echoes the current epoch on every patch so the client can DROP a patch
// that was produced for content from before the reset (an in-flight patch racing
// a reset() would otherwise repopulate the just-cleared store with ghost blocks).
export type ToWorker =
  | { type: "append"; streamId: number; chunk: string; config?: ParserConfig; epoch?: number }
  | { type: "finalize"; streamId: number; config?: ParserConfig; epoch?: number }
  | { type: "reset"; streamId: number; epoch?: number }
  | { type: "dispose"; streamId: number };

export type FromWorker =
  | { type: "ready" }
  | {
      // `patch` is a JSON-encoded Patch (the worker forwards the WASM string
      // verbatim); the main thread JSON.parses it once. See FluxClient.onMessage.
      type: "patch";
      streamId: number;
      patch: string;
      appendedBytes: number;
      parseMicros: number;
      retainedBytes: number;
      wasmMemoryBytes: number;
      // True only on the terminal patch emitted by finalize(). The client flushes
      // it synchronously even under rAF coalescing, regardless of how many append
      // patches preceded it — `final` rides the message so the sync flush binds to
      // the ACTUAL terminal patch, not whichever patch happens to arrive first.
      final?: boolean;
      // The stream generation this patch belongs to (see ToWorker.epoch).
      epoch?: number;
    }
  // `fatal` marks a worker-level failure (WASM init) that dooms every stream on
  // the worker — not a single parse error. It carries no meaningful streamId.
  | { type: "error"; streamId: number; message: string; fatal?: boolean };

/**
 * Minimal structural interface satisfied by the DOM `Worker`. Injectable so the
 * pool's routing/lifecycle logic can be unit-tested with a fake worker — no
 * real Worker or WASM required.
 */
export interface WorkerLike {
  postMessage(msg: ToWorker): void;
  addEventListener(type: "message", listener: (ev: { data: FromWorker }) => void): void;
  terminate(): void;
}

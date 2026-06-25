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
export type ToWorker =
  | { type: "append"; streamId: number; chunk: string; config?: ParserConfig }
  | { type: "finalize"; streamId: number; config?: ParserConfig }
  | { type: "reset"; streamId: number }
  | { type: "dispose"; streamId: number };

export type FromWorker =
  | { type: "ready" }
  | {
      type: "patch";
      streamId: number;
      patch: Patch;
      appendedBytes: number;
      parseMicros: number;
      retainedBytes: number;
      wasmMemoryBytes: number;
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

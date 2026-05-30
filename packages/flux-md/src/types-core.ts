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
 * A List's `kind.data` when {@link ParserConfig.blockData} is on. `ordered` is the
 * always-on flag; `start` is the opt-in ordered-list start number (the `start="N"`
 * HTML attribute; `1` for an unordered list), only present when `blockData` is on.
 * Renumber / continue a split list from `start` alone — no HTML re-parse. When
 * `blockData` is off, `start` is absent and `kind.data` is just `{ ordered }`,
 * byte-identical to before.
 */
export interface ListData {
  ordered: boolean;
  start?: number;
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
  /** Rendered, XSS-safe HTML for this block. */
  html: string;
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

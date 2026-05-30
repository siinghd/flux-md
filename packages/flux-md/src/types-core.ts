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

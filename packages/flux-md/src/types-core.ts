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
  | { type: "error"; streamId: number; message: string };

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

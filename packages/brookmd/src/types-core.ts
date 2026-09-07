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
 * treats it as `ReactNode`, the DOM binding (`brookmd/dom`) as `Node | string`.
 */
export type BrookNode = unknown;

/**
 * Wrap or replace matched inline **text** while streaming, in O(n). A decorator
 * runs POST-PARSE on real inline TEXT nodes only (after the core renders a block
 * to HTML and the walker parses it), once per committed block — so it never sees
 * URLs, code, or markup, and a value split by inline markup (e.g.
 * `$2.<em>5</em>B`) is two text nodes and won't match across them.
 *
 * **Trusted surface (read this).** A decorator's `replace` output is spliced
 * directly into the render tree and does **NOT** pass through brookmd's attribute
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
  replace: (matchText: string, groups: string[]) => BrookNode;
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

/**
 * The link a delegated click landed on, handed to every renderer's
 * `onLinkClick` hook alongside the raw event.
 *
 * The hook is DELEGATED by design: ONE listener sits on the renderer's
 * `.brook-md` root and resolves the anchor from the event target, so turning it
 * on adds no per-block and no per-anchor work — the streaming path is untouched
 * and no block's memo/node reuse is affected.
 *
 * A still-streaming anchor (`data-brook-pending`: label rendered, URL not yet
 * arrived) is never reported — there is no `href` to hand you yet.
 */
export interface LinkClickInfo {
  /** The anchor's `href` attribute verbatim, as the core emitted it. */
  href: string;
  /** The anchor's rendered text content. */
  text: string;
  /** The live `<a>` element that was clicked. */
  element: HTMLAnchorElement;
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
 * A CodeBlock's `kind.data`. `lang` is the always-on info-string language (`null`
 * for none) — the info string's first word; `meta` is the always-on REMAINDER of
 * that same info string, trimmed (```` ```ts title="src/main.ts" ```` ⇒
 * `lang: "ts"`, `meta: 'title="src/main.ts"'`), absent when the fence carried
 * none. `code` is the opt-in DECODED source inside `<pre><code>…</code></pre>`
 * (only present when `blockData` is on). Build a copy-to-clipboard string /
 * re-highlight from `code` alone — no HTML re-parse, no entity-decode. When
 * `blockData` is off, `code` is absent and `kind.data` is just `{ lang }` (plus
 * `meta` if the fence had one), byte-identical to before.
 *
 * Both halves are the RAW info-string text (backslash escapes / entity references
 * left undecoded), and only `lang` appears in the rendered HTML
 * (`class="language-…" data-lang="…"`) — there is deliberately no `data-meta`
 * attribute, so a filename header needs a `components.CodeBlock` override.
 */
export interface CodeBlockData {
  lang: string | null;
  meta?: string;
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
  /**
   * The item's DOCUMENT-ABSOLUTE source byte offset — the index, in the markdown
   * fed so far, of the byte where this item's marker (`-`, `*`, `1.`, …) begins.
   * Same origin as {@link Block.start}, and stable as the document grows (the
   * parser's buffer is append-only), so `source.slice(item.start)` always begins
   * at this item's marker. Use it to read or rewrite the item in place — e.g.
   * find the task-list checkbox with your own `findTaskListMarkerOffset(source,
   * item.start)` and flip `[ ]` ⇄ `[x]` in the original string.
   *
   * Present only when {@link ParserConfig.blockData} is on.
   *
   * KNOWN LIMITATION — absent for NESTED list items. A nested list is not a
   * separate block: its items live inside the parent item's `html` and never
   * reach the `items` channel, and the nested render runs against a synthesized
   * de-indented string with no document offset. Nested items therefore carry no
   * offset rather than a wrong one. Only top-level list items get a `start`.
   */
  start?: number;
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

/**
 * Wire delta mode (WIRE.md §11): the splice an active block carries in place
 * of `html` when it was already emitted in the previous patch. Reconstruct
 * with `prev.slice(0, keep_units) + append` (JS strings are UTF-16, so
 * `keep_units` is the right offset here; `keep_bytes` is the same prefix for
 * byte-oriented consumers).
 */
export interface WireHtmlDelta {
  keep_bytes: number;
  keep_units: number;
  append: string;
}

/**
 * An `active` array entry as serialized: a full {@link Block}, or (wire delta
 * mode only) every Block field except `html` plus an `html_delta` splice.
 * {@link applyPatch} reconstructs deltas into full Blocks — nothing past the
 * store ever sees this union.
 */
export type WireActiveBlock = Block | (Omit<Block, "html"> & { html_delta: WireHtmlDelta });

export interface Patch {
  newly_committed: Block[];
  active: WireActiveBlock[];
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
   * `html`. Populated by `<BrookMarkdown>` / `<BrookMarkdownStatic>` when a
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
  /**
   * Info-string META — everything after the language word, trimmed (from
   * `kind.data.meta`), e.g. `title="src/main.ts"` or a bare `src/main.ts`.
   * Always-on like `language` (no `blockData` needed); `undefined` when the
   * fence carried none. Deliberately absent from the rendered HTML, so render a
   * filename header from this prop.
   *
   * While streaming it appears once it can no longer change — when the opening
   * fence line is terminated by a newline, or at finalize — so a header never
   * flickers through a half-typed `title="src/ma`.
   */
  meta?: string;
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
   * {@link ParserConfig.blockData} is on (otherwise `undefined`). `{ lang, meta?,
   * code }` with `code` the DECODED source. Build a copy-to-clipboard string /
   * re-highlight from `code` — no HTML re-parse, no entity-decode. (`props.text` /
   * `props.language` carry the same source / lang and stay populated even when off,
   * via the HTML regex fallback; `props.meta` carries the same meta and is
   * always-on, since it has no HTML form to fall back to.)
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
 * `new BrookClient()` behaves exactly as before. Config is applied when the
 * stream's parser is created and is **immutable** for that stream's lifetime
 * (a `reset()` keeps it; use a new client for different flags).
 */
export interface ParserConfig {
  /** GFM extended autolinks (bare www./http(s)://ftp:// + emails). Default true. */
  gfmAutolinks?: boolean;
  /** GitHub alerts (`> [!NOTE]` → callouts). Default true. */
  gfmAlerts?: boolean;
  /**
   * GFM "Disallowed Raw HTML" (tagfilter): with `unsafeHtml` on, the nine
   * disallowed tags (`<title>`, `<textarea>`, `<style>`, `<xmp>`, `<iframe>`,
   * `<noembed>`, `<noframes>`, `<script>`, `<plaintext>`) get their leading
   * `<` escaped so they display as text instead of taking effect. Default
   * false (strict CommonMark passes them through under `unsafeHtml`); no
   * effect while raw HTML is escaped (default) or sanitized — already inert.
   */
  gfmTagfilter?: boolean;
  /** GFM footnotes (`[^1]` + `[^1]:` → footnote section). Default false. */
  gfmFootnotes?: boolean;
  /**
   * Math: `$…$` / `\(…\)` inline and `$$…$$` / `\[…\]` display. Default false
   * (so `$` in prose / currency stays literal). Emits KaTeX-ready markup
   * (`<span class="math math-inline">` / `<div class="math math-display">`)
   * carrying the LaTeX — bring your own KaTeX pass (brookmd stays zero-dep).
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
   * Lenient list indentation: a list marker followed by 6 or more columns of
   * SPACE padding yields the item's text, where strict CommonMark (§5.2) keeps
   * one column and renders the rest as an indented code block. Default false.
   *
   * Aimed at model output, which routinely over-indents after a bullet
   * (`-       const value = 1;`). Four cases stay strictly conformant: exactly
   * 5 columns of padding, a fenced code block opened on the marker line itself,
   * indented code that starts on a line AFTER the marker, and tab-padded
   * markers (`-\t\tfoo`).
   */
  lenientLists?: boolean;
  /**
   * Render a CommonMark SOFT line break (a bare `\n` inside inline content) as
   * a `<br>` — the "GitHub comment" convention, where one
   * Enter is one visual line. Default false (strict CommonMark: a soft break is
   * whitespace). Hard breaks (two trailing spaces, or a trailing `\`) are `<br>`
   * either way, so enabling this only ADDS breaks — it never removes one. Chat
   * UIs streaming model output usually want this on, since models emit single
   * newlines expecting a visible break.
   */
  softBreaks?: boolean;
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
   * comments are dropped. Block-level raw HTML stays escaped unless you also set
   * {@link blockHtml}. Unset/omitted = off (raw HTML handling unchanged).
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
   * Extend the safe raw-HTML sanitizer to **block-level** raw HTML, so a model
   * emitting `<details><summary>…</summary>…</details>` on its own lines renders
   * as real elements instead of an escaped code block. Only takes effect when
   * the sanitizer is engaged ({@link htmlAllowlist} / {@link dropHtmlTags}); on
   * its own it does nothing. Default false — existing sanitizer users keep
   * escaped block HTML until they opt in.
   *
   * Scope is CommonMark HTML block **types 6 and 7**: a known block-level tag
   * (`<details>`, `<div>`, `<table>`, …) or any other complete tag alone on its
   * line. Types 1–5 stay escaped/dropped: type 1 is the raw-text family
   * (`<script>`, `<pre>`, `<style>`, `<textarea>`) — a browser reads everything
   * after such a tag as unparsed text, so a speculative mid-stream close is
   * mXSS-prone — and types 2–5 (comments, PIs, CDATA, declarations) carry no
   * renderable element. The tag allow/drop/dangerous decision and the hardened
   * attribute policy are exactly the inline sanitizer's.
   *
   * While the block streams, still-open elements get **speculative closers**, so
   * what the reader has seen so far is a complete tree at every append; a
   * half-arrived tag stays invisible until it completes. Markdown *inside* the
   * HTML is not parsed (the body is text + tags).
   */
  blockHtml?: boolean;
  /**
   * Opt-in **un-blocklist** for URL schemes that brookmd blocks by default.
   * Bare scheme names, **without** the colon (`["file"]`), matched
   * case-insensitively. Empty/omitted = the built-in policy is unchanged.
   *
   * This never *restricts* anything — it is not a general allowlist. Schemes
   * outside the built-in blocklist (`vscode:`, `ftp:`, `mailto:`, …) already
   * render today and are unaffected. The only tier it can reach is the
   * overridable-blocked one, currently just `file:`.
   *
   * The script-executing tier — `javascript:`, `vbscript:`, `data:text/html`,
   * `data:text/javascript`, and the scriptable `data:` media types
   * (`data:image/svg`, `data:application/xhtml`, …) — is **non-overridable**:
   * listing one here is a silent no-op, exactly as allowlisting `<script>`
   * cannot re-enable it via {@link htmlAllowlist}.
   *
   * Only enable `file:` in a host that intercepts link clicks instead of
   * navigating (an Electron / extension UI that opens the path in an editor);
   * local-resource disclosure then becomes the embedder's responsibility.
   * Applies uniformly to links, URI autolinks, images, and sanitized URL
   * attributes.
   */
  allowSchemes?: string[];
  /**
   * Opt-in structured table data. When on, a `Table` block's `kind.data` is
   * populated with `{ headers, rows, aligns }` (each cell `{ text, html }`) so a
   * consumer can build a sort/filter/transpose/chart/CSV toolbar from DATA — no
   * HTML re-parse, no HAST tree. Default false (non-users pay zero allocation /
   * serde bytes; output and the `kind` serde shape stay byte-identical when off).
   */
  blockData?: boolean;
  /**
   * Keep every committed block's rendered HTML retained **inside the parser**.
   *
   * Defaults to **false on the streaming path** (the worker), which is what you
   * want: the client receives each committed block exactly once, in the patch
   * that commits it, and stores it itself — so a second copy sitting in WASM for
   * the life of the stream serves nobody. Dropping it roughly halves a long
   * stream's retained bytes (`onPatch`'s `retainedBytes`); the wire is
   * byte-identical either way.
   *
   * Set `true` only if you need the parser itself to still hold the whole
   * rendered document. The server one-shot renderers (`renderToString`,
   * `parseToBlocks`) read the document back out of the parser and therefore pin
   * this on regardless of what you pass.
   */
  retainCommittedHtml?: boolean;
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
      // verbatim); the main thread JSON.parses it once. See BrookClient.onMessage.
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
  /**
   * Structural superset of DOM `Worker.addEventListener` for the three channels
   * the pool listens on: `message` (patches / ready / in-band errors — read via
   * `ev.data`) plus the out-of-band failure channels `error` (a script that
   * 404s or throws at load — `ev.message`) and `messageerror` (an
   * undeserializable posted message). One widened signature keeps the unit-test
   * fakes that declare only `"message"` compiling — method parameters are
   * checked bivariantly — while letting the pool attach all three without a
   * structural cast.
   */
  addEventListener(
    type: "message" | "error" | "messageerror",
    listener: (ev: { data: FromWorker; message?: string }) => void,
  ): void;
  terminate(): void;
}

//! brookmd-core: zero-dep streaming markdown parser.
//!
//! No third-party parser — block scanning, inline tokenizing,
//! and HTML rendering are all in-house. The library exposes a `StreamParser`
//! you `append(chunk)` repeatedly. Each call returns a `Patch` describing
//! which blocks just became permanent ("committed") and which blocks are
//! still being built ("active"). Active blocks may flicker as more input
//! arrives; committed blocks never change. Each block carries a stable
//! monotonic ID so a UI layer can reconcile in place.

pub mod blocks;
mod entities;
mod inline;
mod parser;
mod render;
mod scanner;
mod sort;
mod url;
pub mod wire;

pub use blocks::{Block, BlockKind};
pub use parser::{HtmlDelta, Patch, StreamParser};
pub use wire::{WireActive, WirePatch};

/// Deterministic perf instrumentation — feature `perf_counters`, off by default
/// and never compiled into the WASM build (no `[features]` flag is set there).
///
/// Three per-thread work counters, each capturing a different way streaming
/// cost can go quadratic (see `tests/scaling.rs` for the gate that bounds them):
///
/// - `scanned_bytes` — tail bytes the *slow path* re-scans. The incremental
///   caches (fence/paragraph/table/container/list/indented/html) extend an open
///   block in O(new bytes) and never reach this counter; only a cache miss
///   falls through to `scan(tail)`, whose cost is `tail.len()`. One exception:
///   the table cache's speculative partial-row path counts the bytes it
///   scans/re-renders per append itself — it returns before the slow-path
///   counter, and that exact cliff shipped once behind a linear-looking count.
/// - `rendered_bytes` — input bytes fed through the inline renderer. Catches
///   cache-*internal* quadratics the scan counter is blind to: a cache that
///   stays armed but re-inline-renders a growing region on every append (open
///   list item bodies, table partial rows, pinned paragraph cuts inside
///   containers) shows up here even though it never re-scans.
/// - `emitted_bytes` — HTML bytes crossing the public `append`/`finalize`
///   boundary (`patch.newly_committed` + `patch.active`). With wire delta mode
///   off, full re-emission of the open block's HTML each append is the wire
///   contract, so this is inherently O(n²/chunk) for a giant single open block
///   (printed, not gated). With `set_wire_delta(true)` (WIRE.md §11) only each
///   delta's `append` tail counts, and `tests/scaling.rs` GATES linearity.
///
/// All counters count work, not wall-clock time, so a scaling test can gate
/// deterministically in CI without flaking on noisy shared runners.
#[cfg(feature = "perf_counters")]
pub mod perf {
    use std::cell::Cell;

    thread_local! {
        static SCAN_BYTES: Cell<u64> = Cell::new(0);
        static RENDER_BYTES: Cell<u64> = Cell::new(0);
        static EMIT_BYTES: Cell<u64> = Cell::new(0);
    }

    /// Reset all per-thread counters before a measurement.
    pub fn reset() {
        SCAN_BYTES.with(|c| c.set(0));
        RENDER_BYTES.with(|c| c.set(0));
        EMIT_BYTES.with(|c| c.set(0));
    }

    /// Total tail bytes the slow path has scanned since the last [`reset`].
    pub fn scanned_bytes() -> u64 {
        SCAN_BYTES.with(|c| c.get())
    }

    /// Total input bytes fed through the inline renderer since the last
    /// [`reset`] (nested constructs — link text, inline components — count
    /// their sub-slices again; a constant factor, irrelevant to scaling ratios).
    pub fn rendered_bytes() -> u64 {
        RENDER_BYTES.with(|c| c.get())
    }

    /// Total HTML bytes emitted across the `append`/`finalize` boundary
    /// (committed + active) since the last [`reset`].
    pub fn emitted_bytes() -> u64 {
        EMIT_BYTES.with(|c| c.get())
    }

    #[inline]
    pub(crate) fn add_scan(n: usize) {
        SCAN_BYTES.with(|c| c.set(c.get().wrapping_add(n as u64)));
    }

    #[inline]
    pub(crate) fn add_render(n: usize) {
        RENDER_BYTES.with(|c| c.set(c.get().wrapping_add(n as u64)));
    }

    #[inline]
    pub(crate) fn add_emit(n: usize) {
        EMIT_BYTES.with(|c| c.set(c.get().wrapping_add(n as u64)));
    }
}

#[cfg(feature = "wasm")]
use wasm_bindgen::prelude::*;

#[cfg(feature = "wasm")]
#[wasm_bindgen]
pub struct BrookParser {
    inner: StreamParser,
}

#[cfg(feature = "wasm")]
#[wasm_bindgen]
impl BrookParser {
    #[wasm_bindgen(constructor)]
    pub fn new() -> BrookParser {
        BrookParser { inner: StreamParser::new() }
    }

    /// Returns the patch as a **JSON string** (parsed with `JSON.parse` on the
    /// main thread), not a live JS object. This is deliberate: serializing once to
    /// a string in Rust avoids serde-wasm-bindgen's per-node boundary calls, and a
    /// string is far cheaper than an object graph to `structuredClone` across the
    /// worker→main `postMessage` (the worker forwards the string verbatim).
    #[wasm_bindgen]
    pub fn append(&mut self, chunk: &str) -> Result<String, JsValue> {
        let patch = self.inner.append(chunk);
        // Mirrors `wire::patch_to_json`, but keeps the Result-propagating shape
        // the JS boundary expects (and that keeps the shipped WASM byte-stable).
        serde_json::to_string(&WirePatch::from(patch)).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// JSON-string patch — see [`BrookParser::append`].
    #[wasm_bindgen]
    pub fn finalize(&mut self) -> Result<String, JsValue> {
        let patch = self.inner.finalize();
        // Mirrors `wire::patch_to_json`, but keeps the Result-propagating shape
        // the JS boundary expects (and that keeps the shipped WASM byte-stable).
        serde_json::to_string(&WirePatch::from(patch)).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = bufferLen)]
    pub fn buffer_len(&self) -> usize {
        self.inner.buffer().len()
    }

    /// All blocks currently parsed (committed + active), in document order — the
    /// whole rendered document as a **JSON string** of a `Block[]` (parse with
    /// `JSON.parse`). The one-shot / server-side render primitive: feed the full
    /// markdown via `append`, call `finalize`, then read `allBlocks()` (no worker,
    /// no patch accumulation).
    #[wasm_bindgen(js_name = allBlocks)]
    pub fn all_blocks(&self) -> Result<String, JsValue> {
        let blocks: Vec<&Block> = self.inner.all_blocks().collect();
        serde_json::to_string(&blocks).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Total bytes the parser is retaining: source buffer + all rendered
    /// HTML for committed and active blocks. Use to compare per-parser
    /// memory cost against alternatives.
    #[wasm_bindgen(js_name = retainedBytes)]
    pub fn retained_bytes(&self) -> usize {
        self.inner.retained_bytes()
    }

    /// Enable or disable raw-HTML pass-through. Default off. Do not enable
    /// when rendering untrusted input — bypasses XSS protection.
    #[wasm_bindgen(js_name = setUnsafeHtml)]
    pub fn set_unsafe_html(&mut self, on: bool) {
        self.inner.set_unsafe_html(on);
    }

    /// Enable GFM extended autolinks (bare www./http(s)://ftp:// URLs and email
    /// addresses become links). Useful for LLM output, which is full of them.
    #[wasm_bindgen(js_name = setGfmAutolinks)]
    pub fn set_gfm_autolinks(&mut self, on: bool) {
        self.inner.set_gfm_autolinks(on);
    }

    /// Enable GitHub alerts (`> [!NOTE]` blockquotes render as styled callouts
    /// with GitHub-compatible class names). Off by default.
    #[wasm_bindgen(js_name = setGfmAlerts)]
    pub fn set_gfm_alerts(&mut self, on: bool) {
        self.inner.set_gfm_alerts(on);
    }

    /// Enable the GFM "Disallowed Raw HTML" extension (tagfilter): with raw
    /// HTML passing through (`setUnsafeHtml(true)`), the nine disallowed tags
    /// (`<title>`, `<script>`, `<iframe>`, …) get their leading `<` escaped so
    /// they display as text instead of taking effect. Off by default; no
    /// effect while raw HTML is escaped or sanitized (already inert).
    #[wasm_bindgen(js_name = setGfmTagfilter)]
    pub fn set_gfm_tagfilter(&mut self, on: bool) {
        self.inner.set_gfm_tagfilter(on);
    }

    /// Enable GFM footnotes (`[^1]` references + `[^1]:` definitions → a
    /// footnote section emitted at finalize). Off by default.
    #[wasm_bindgen(js_name = setGfmFootnotes)]
    pub fn set_gfm_footnotes(&mut self, on: bool) {
        self.inner.set_gfm_footnotes(on);
    }

    /// Enable math: `$…$` / `\(…\)` inline and `$$…$$` / `\[…\]` display math.
    /// Off by default (so `$` in prose / currency stays literal). The emitted
    /// HTML carries the LaTeX in `<span class="math math-inline">` /
    /// `<div class="math math-display">` for a KaTeX pass on the JS side.
    #[wasm_bindgen(js_name = setGfmMath)]
    pub fn set_gfm_math(&mut self, on: bool) {
        self.inner.set_gfm_math(on);
    }

    /// Lenient list indentation: a list marker followed by 6+ columns of SPACE
    /// padding yields the item's text instead of an indented code block. Off by
    /// default (strict CommonMark §5.2). Useful for model output, which routinely
    /// over-indents after a bullet. Exactly-5-column padding, a fence opened on
    /// the marker line, indented code starting on a later line, and tab-padded
    /// markers all stay strictly conformant.
    #[wasm_bindgen(js_name = setLenientLists)]
    pub fn set_lenient_lists(&mut self, on: bool) {
        self.inner.set_lenient_lists(on);
    }

    /// Emit `dir="auto"` on block-level text elements so the browser detects
    /// each block's direction (LTR/RTL) independently — correct rendering for
    /// documents that mix English with Arabic/Hebrew. Off by default; code
    /// blocks never get it (code is always LTR).
    #[wasm_bindgen(js_name = setDirAuto)]
    pub fn set_dir_auto(&mut self, on: bool) {
        self.inner.set_dir_auto(on);
    }

    /// Render a CommonMark SOFT line break (a bare `\n` in inline content) as a
    /// `<br>` — the "GitHub comment" convention, where one Enter is one visual
    /// line. Off by default (strict CommonMark: a soft break is whitespace).
    /// Hard breaks (two trailing spaces / trailing `\`) are `<br>` either way,
    /// so turning this on only ADDS breaks; it never removes one.
    #[wasm_bindgen(js_name = setSoftBreaks)]
    pub fn set_soft_breaks(&mut self, on: bool) {
        self.inner.set_soft_breaks(on);
    }

    /// Opt-in accessibility markup that deviates from strict GFM byte-output:
    /// `<label>`-wrap a task-list checkbox with its text, and add `scope="col"`
    /// to table header cells. Off by default (conformance output unchanged).
    #[wasm_bindgen(js_name = setA11y)]
    pub fn set_a11y(&mut self, on: bool) {
        self.inner.set_a11y(on);
    }

    /// Opt-in structured `kind.data` channel for Table blocks: a Table then
    /// carries `{ headers, rows, aligns }` (per-cell `{ text, html }`) so a
    /// consumer can build a sort/filter/transpose/chart/CSV toolbar from DATA
    /// without re-parsing the HTML. Off by default — when off, Table serializes
    /// as `{"type":"Table"}` (no `data` key) and output is byte-identical.
    #[wasm_bindgen(js_name = setBlockData)]
    pub fn set_block_data(&mut self, on: bool) {
        self.inner.set_block_data(on);
    }

    /// Opt-in wire delta mode (WIRE.md §11): active blocks re-emitted across
    /// appends serialize as verified `html_delta` splices against their previous
    /// emit instead of full `html`, making total emitted bytes O(n) for a block
    /// that grows across many appends. Off by default (wire byte-identical to
    /// pre-delta releases). A consumer that enables this must reconstruct
    /// active `html` per WIRE.md §11 — the npm package's client does.
    #[wasm_bindgen(js_name = setWireDelta)]
    pub fn set_wire_delta(&mut self, on: bool) {
        self.inner.set_wire_delta(on);
    }

    /// Keep every committed block's rendered HTML retained inside the parser for
    /// `allBlocks()`. ON by default. Turn it OFF in a pure STREAMING consumer —
    /// one that reads each committed block exactly once out of its patch and
    /// never calls `allBlocks()` (the npm worker): the HTML is then released the
    /// moment the block is emitted, so a long stream retains the source buffer
    /// plus the open tail instead of the whole rendered document (`retainedBytes`
    /// reflects it). Patch bytes are identical either way; the only cost is that
    /// `allBlocks()` then reports committed blocks with an EMPTY `html` (id,
    /// kind, start, end, open, speculative all stay exact).
    #[wasm_bindgen(js_name = setRetainCommittedHtml)]
    pub fn set_retain_committed_html(&mut self, on: bool) {
        self.inner.set_retain_committed_html(on);
    }

    /// Set the opt-in component-tag allowlist (e.g. `["Thinking", "Callout"]`).
    /// A `<Tag>…</Tag>` whose name is listed renders as a component whose inner
    /// content is markdown — safely, without unsafe HTML (the tag is allowlisted
    /// and its attributes are sanitized). Empty by default (feature off).
    #[wasm_bindgen(js_name = setComponentTags)]
    pub fn set_component_tags(&mut self, tags: Vec<String>) {
        self.inner.set_component_tags(tags);
    }

    /// Set the opt-in INLINE component-tag allowlist (e.g. `["tik", "cite"]`).
    /// An allowlisted inline `<tik>…</tik>` (or self-closing `<tik/>`) renders as
    /// a custom element (markdown inner, sanitized attributes) so a JSX/DOM layer
    /// can dispatch it via `components[tag]` — in paragraphs, headings, table
    /// cells, and list items. Empty by default (inline output unchanged).
    #[wasm_bindgen(js_name = setInlineComponentTags)]
    pub fn set_inline_component_tags(&mut self, tags: Vec<String>) {
        self.inner.set_inline_component_tags(tags);
    }

    /// Engage the safe raw-HTML sanitizer. When `on`, inline raw HTML renders
    /// sanitized without full unsafe HTML: `allow` empty = allow all tags except
    /// a built-in dangerous set (`script`, `style`, `iframe`, …); `allow`
    /// non-empty = only those render (others escaped); `drop` tags are removed
    /// entirely; HTML comments are dropped; every rendered tag's attributes are
    /// sanitized. Off by default (raw-HTML handling unchanged).
    #[wasm_bindgen(js_name = setHtmlSanitize)]
    pub fn set_html_sanitize(&mut self, on: bool, allow: Vec<String>, drop: Vec<String>) {
        self.inner.set_html_sanitize(on, allow, drop);
    }

    /// Extend the safe raw-HTML sanitizer to BLOCK-level raw HTML — a
    /// `<details><summary>…` block renders as real elements instead of escaping
    /// into a code block. Takes effect ONLY when the sanitizer is engaged
    /// (`setHtmlSanitize`), and only for CommonMark HTML block types 6 and 7.
    /// Types 1–5 (`<script>`/`<pre>`/`<style>`/`<textarea>`, comments, PIs,
    /// CDATA, declarations) stay escaped/dropped. Still-open elements get
    /// speculative closers while the block streams, so the emitted HTML is a
    /// complete tree at every prefix. Off by default (output unchanged).
    #[wasm_bindgen(js_name = setBlockHtml)]
    pub fn set_block_html(&mut self, on: bool) {
        self.inner.set_block_html(on);
    }

    /// Un-block specific URL schemes that are blocked by DEFAULT — bare scheme
    /// names without the colon (`["file"]`), matched case-insensitively. Empty
    /// by default (built-in policy unchanged). This never RESTRICTS anything:
    /// schemes outside the built-in blocklist (`vscode:`, `ftp:`, …) already
    /// pass. The script-executing tier (`javascript:`, `vbscript:`,
    /// `data:text/html`, `data:text/javascript`, scriptable `data:` media types)
    /// is non-overridable — listing one here is a no-op. Only enable `file:` in
    /// a host that intercepts link clicks instead of navigating (Electron,
    /// extensions); local-resource disclosure is then the embedder's call.
    #[wasm_bindgen(js_name = setAllowSchemes)]
    pub fn set_allow_schemes(&mut self, schemes: Vec<String>) {
        self.inner.set_allow_schemes(schemes);
    }
}

#[cfg(feature = "wasm")]
impl Default for BrookParser {
    fn default() -> Self {
        Self::new()
    }
}

//! flux-md-core: zero-dep streaming markdown parser.
//!
//! No `pulldown-cmark`, no other parsers — block scanning, inline tokenizing,
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
mod url;

pub use blocks::{Block, BlockKind};
pub use parser::{Patch, StreamParser};

use serde::Serialize;
use wasm_bindgen::prelude::*;

#[derive(Serialize)]
struct PatchJs {
    newly_committed: Vec<Block>,
    active: Vec<Block>,
}

impl From<Patch> for PatchJs {
    fn from(p: Patch) -> Self {
        Self { newly_committed: p.newly_committed, active: p.active }
    }
}

#[wasm_bindgen]
pub struct FluxParser {
    inner: StreamParser,
}

#[wasm_bindgen]
impl FluxParser {
    #[wasm_bindgen(constructor)]
    pub fn new() -> FluxParser {
        FluxParser { inner: StreamParser::new() }
    }

    #[wasm_bindgen]
    pub fn append(&mut self, chunk: &str) -> Result<JsValue, JsValue> {
        let patch = self.inner.append(chunk);
        serde_wasm_bindgen::to_value(&PatchJs::from(patch))
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen]
    pub fn finalize(&mut self) -> Result<JsValue, JsValue> {
        let patch = self.inner.finalize();
        serde_wasm_bindgen::to_value(&PatchJs::from(patch))
            .map_err(|e| JsValue::from_str(&e.to_string()))
    }

    #[wasm_bindgen(js_name = bufferLen)]
    pub fn buffer_len(&self) -> usize {
        self.inner.buffer().len()
    }

    /// All blocks currently parsed (committed + active), in document order — the
    /// whole rendered document as a JS array of `Block`. The one-shot /
    /// server-side render primitive: feed the full markdown via `append`, call
    /// `finalize`, then read `allBlocks()` (no worker, no patch accumulation).
    #[wasm_bindgen(js_name = allBlocks)]
    pub fn all_blocks(&self) -> Result<JsValue, JsValue> {
        let blocks: Vec<&Block> = self.inner.all_blocks().collect();
        serde_wasm_bindgen::to_value(&blocks).map_err(|e| JsValue::from_str(&e.to_string()))
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

    /// Emit `dir="auto"` on block-level text elements so the browser detects
    /// each block's direction (LTR/RTL) independently — correct rendering for
    /// documents that mix English with Arabic/Hebrew. Off by default; code
    /// blocks never get it (code is always LTR).
    #[wasm_bindgen(js_name = setDirAuto)]
    pub fn set_dir_auto(&mut self, on: bool) {
        self.inner.set_dir_auto(on);
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
}

impl Default for FluxParser {
    fn default() -> Self {
        Self::new()
    }
}

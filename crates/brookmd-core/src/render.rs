//! Block-level renderer. Takes a `RawBlock` (from `scanner`) plus its source
//! slice and emits sanitized HTML for it. Inline content is delegated to
//! `inline::render_inline`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::blocks::{
    AlertKind, BlockKind, ContainerData, HeadingData, ListItemData, MathBlockData, NestedBlock,
    TableCell, TableData,
};
use crate::inline::{
    inline_html_streams_to_eof, match_inline_html, render_inline, render_inline_para,
    sanitize_html_token, SanitizedTag,
};
use crate::scanner::{
    component_inner_range, detect_html_block_open, indent_cols, is_blank_line, line_end,
    line_slice, scan, scan_marker, RawBlock, RawBlockKind, ScanCtx,
};
use crate::url::{escape_attr, escape_html, sanitize_attrs};

#[derive(Clone, Default, Debug)]
pub struct LinkRef {
    pub url: String,
    pub title: Option<String>,
}

/// Rendering context threaded through every block + inline call. Holds the
/// unsafe-HTML flag (whether raw HTML passes through) and the link
/// reference table.
#[derive(Clone, Default, Debug)]
pub struct RenderOpts {
    pub unsafe_html: bool,
    /// Link-reference table, split in two so the streaming parser never clones
    /// the (growing) committed table per append: `committed_refs` is a shared
    /// snapshot of the permanent definitions (cheap `Rc` clone, O(1)); `tail_refs`
    /// holds the definitions in the current uncommitted tail. Lookups check
    /// committed first (first-definition-wins), then the tail.
    pub committed_refs: Rc<HashMap<String, LinkRef>>,
    pub tail_refs: HashMap<String, LinkRef>,
    /// Set by the link/image renderer when recursing into link text. While
    /// true, the inline parser will not recognize nested `[...]` links
    /// (CommonMark disallows nested links).
    pub in_link: bool,
    /// Speculative open-tail link rendering. `true` ONLY when rendering the
    /// still-open block that abuts buffer EOF during streaming (the active tail
    /// the user is currently watching grow). When on, an incomplete inline link
    /// still streaming to EOF — open label `[Earnings Ca`, `]` at EOF, ref name
    /// `[t][re`, destination/title `[label](http…` — renders as an INERT
    /// `<a data-brook-pending="">label</a>` (label inline, destination
    /// suppressed, NO `href`) instead of flashing the raw `[label` / half-typed
    /// URL as literal text; the real `href` lands (and the pending marker
    /// drops) only once the link completes (node-reuse: only attributes swap).
    /// OFF (default) ⇒ byte-identical one-shot CommonMark output: an incomplete
    /// link degrades to literal text. The streaming parser sets this true only
    /// on the genuine abuts-EOF active tail; `finalize()` forces it false
    /// everywhere (literal), so the committed output is byte-parity with a
    /// one-shot complete-literal render.
    pub open_tail: bool,
    /// GFM extended autolinks: recognize bare `www.`, `http(s)://`, `ftp://`
    /// URLs (and turn them into links) in ordinary text. Off by default so
    /// strict CommonMark output is unchanged.
    pub gfm_autolinks: bool,
    /// GitHub alerts: a `> [!NOTE]` blockquote becomes a styled callout
    /// (`<div class="markdown-alert …">`). Off by default so strict CommonMark
    /// output (a plain `<blockquote>`) is unchanged.
    pub gfm_alerts: bool,
    /// GFM "Disallowed Raw HTML" (tagfilter, GFM spec §6.11): when raw HTML is
    /// emitted verbatim (`unsafe_html` pass-through), escape the leading `<` of
    /// the nine disallowed tags (`<title>`, `<script>`, …) so they render as
    /// text instead of taking effect. Off by default — strict CommonMark
    /// expects `<script>` verbatim under `unsafe_html`, so this is NOT implied
    /// by it. Irrelevant when raw HTML is escaped/sanitized (already inert).
    pub gfm_tagfilter: bool,
    /// Math: recognize `$…$` / `\(…\)` inline and `$$…$$` / `\[…\]` display
    /// math. Off by default (so `$` in prose stays literal). The block-level
    /// half is also gated in the scanner via [`ScanCtx::math`].
    pub gfm_math: bool,
    /// Emit `dir="auto"` on block-level text elements for per-block bidi. Off by
    /// default (strict-CommonMark output is unchanged).
    pub dir_auto: bool,
    /// Lenient list indentation: a marker followed by 6+ columns of SPACE padding
    /// yields the item's text instead of an indented code block. Off by default
    /// (strict CommonMark). Purely a scanner rule — see [`ScanCtx::lenient_lists`].
    pub lenient_lists: bool,
    /// Render a CommonMark SOFT line break (a bare `\n` inside inline content)
    /// as a `<br>` instead of a literal newline — the
    /// "GitHub comment" convention, where a single Enter starts a new visual
    /// line. Off by default (strict CommonMark: a soft break is whitespace).
    /// Chat UIs that stream model output usually want this on, since models
    /// emit single newlines expecting a visible break. Hard breaks (two trailing
    /// spaces, or a trailing `\`) are unaffected — they are already `<br>`.
    pub soft_breaks: bool,
    /// Emit extra accessibility markup that deviates from strict GFM byte-output:
    /// wrap a task-list checkbox + its inline text in a `<label>` (programmatic
    /// association), and add `scope="col"` to table header cells. Off by default
    /// so the CommonMark/GFM conformance output is unchanged.
    pub a11y: bool,
    /// Emit the opt-in structured `kind.data` channel for blocks that support it
    /// (currently Table → `{headers,rows,aligns}` with per-cell `{text,html}`).
    /// Off by default so non-users pay zero allocation/serde bytes and the
    /// CommonMark/GFM byte-output is unchanged (Table serializes as
    /// `{"type":"Table"}`, no `data` key).
    pub block_data: bool,
    /// GFM footnotes. Off by default. When on, an inline `[^label]` whose label
    /// appears in `footnotes`/`tail_footnotes` renders as a superscript link.
    pub gfm_footnotes: bool,
    /// label → footnote number, assigned in first-reference order across the
    /// whole document. Split in two layers so the streaming parser never clones
    /// the (growing) committed table per append (mirrors `committed_refs` /
    /// `tail_refs`): `footnotes` is a shared snapshot of the committed numbers
    /// (cheap `Rc` clone, O(1)); `tail_footnotes` holds the numbers of labels
    /// first referenced in the current uncommitted tail. A label appears in at
    /// most one layer with a given number assigned once (first-reference-wins),
    /// so lookups via [`RenderOpts::footnote_num`] check committed first. Both
    /// empty unless `gfm_footnotes` is on.
    pub footnotes: Rc<HashMap<String, usize>>,
    pub tail_footnotes: Rc<HashMap<String, usize>>,
    /// Per-label occurrence counter, mutated as `[^label]` references render
    /// (in document order) so the Kth reference to a label gets a unique id
    /// (`fnref-N`, `fnref-N-2`, …). Interior-mutable because emitting unique
    /// ids is inherently sequential state; the alternative (threading `&mut`
    /// through every render_inline caller) is far more invasive. Seeded from
    /// the committed occurrence counts so ids stay unique across the stream.
    pub footnote_occ: RefCell<HashMap<String, usize>>,
    /// Footnote-ref PLACEHOLDER mode (streaming caches). When on, an inline
    /// `[^label]` whose label is numbered emits the `href="#fn-N"` + visible `N`
    /// exactly as normal, but the occurrence-dependent `id="fnref-…"` value is
    /// emitted as an opaque sentinel token carrying both `N` and the raw label
    /// (`\u{0}F\u{1}{N}\u{1}{label}\u{0}`) and the `footnote_occ` counter is NOT
    /// advanced. A later [`resolve_footnote_ids`] pass replaces every token with
    /// the real `fnref-N`/`fnref-N-K` suffix in document order. This lets the
    /// tail caches freeze occurrence-INDEPENDENT html once (the only per-stream
    /// state — the occurrence index — is resolved on commit). Off (default) =
    /// behavior byte-identical to before.
    pub footnote_placeholder: bool,
    /// Opt-in component-tag allowlist, carried so recursive sub-block scans
    /// (inside lists/quotes/components) recognize nested component tags too.
    pub component_tags: Vec<Box<str>>,
    /// Opt-in INLINE component-tag allowlist. An allowlisted `<tik>…</tik>` (or
    /// self-closing `<tik/>`) in inline content renders as a real custom element
    /// — markdown inner, sanitized attributes — for a JSX/DOM layer to dispatch
    /// via `components[tag]`. Empty (default) = off (inline output unchanged).
    pub inline_component_tags: Vec<Box<str>>,
    /// Safe raw-HTML sanitizer (see `StreamParser::set_html_sanitize`). When
    /// `html_sanitize` is on, inline raw HTML is rendered through the allow/drop
    /// lists (dangerous tags + comments removed, attrs sanitized) instead of
    /// escaped/passed-through. Off (default) = unchanged.
    pub html_sanitize: bool,
    pub html_allowlist: Vec<Box<str>>,
    pub html_drop: Vec<Box<str>>,
    /// Extend the safe raw-HTML sanitizer to BLOCK-level raw HTML (see
    /// `StreamParser::set_block_html`). Only takes effect when `html_sanitize` is
    /// also on, and only for CommonMark HTML block types 6 and 7 — types 1–5
    /// (`<script>`/`<pre>`/`<style>`/`<textarea>`, comments, PIs, CDATA,
    /// declarations) stay escaped. Off (default) = block raw HTML is escaped
    /// exactly as before.
    pub block_html: bool,
    /// Opt-in URL-scheme un-blocklist (see `StreamParser::set_allow_schemes`):
    /// BARE scheme names, no colon, matched case-insensitively. Empty (default)
    /// = every default-blocked scheme stays blocked. This only reaches the
    /// OVERRIDABLE-blocked tier (`file:`) — it can never re-enable a
    /// script-executing scheme, and it never restricts a scheme that already
    /// passes.
    pub allow_schemes: Vec<Box<str>>,
}

/// The three configuration lists the raw-HTML sanitizer actually reads,
/// projected out of [`RenderOpts`] so the streaming block-HTML cache — which
/// holds no `RenderOpts` — can drive the SAME decision path
/// ([`crate::inline::sanitize_html_token`]) from borrowed parser state, with no
/// per-append clone of the lists.
#[derive(Clone, Copy)]
pub(crate) struct HtmlPolicy<'a> {
    pub allowlist: &'a [Box<str>],
    pub drop: &'a [Box<str>],
    pub allow_schemes: &'a [Box<str>],
}

impl RenderOpts {
    /// Borrowed view of the sanitizer's allow/drop/scheme lists.
    pub(crate) fn html_policy(&self) -> HtmlPolicy<'_> {
        HtmlPolicy {
            allowlist: &self.html_allowlist,
            drop: &self.html_drop,
            allow_schemes: &self.allow_schemes,
        }
    }

    pub fn lookup(&self, label: &str) -> Option<&LinkRef> {
        let key = normalize_label(label);
        // Committed (permanent) definitions win over tail ones — first-wins.
        self.committed_refs.get(&key).or_else(|| self.tail_refs.get(&key))
    }

    /// Footnote number for `label`, looked up across both layers (committed
    /// first — a label is only ever numbered once, so the order is cosmetic).
    pub(crate) fn footnote_num(&self, label: &str) -> Option<usize> {
        self.footnotes.get(label).or_else(|| self.tail_footnotes.get(label)).copied()
    }

    /// Scanner feature flags derived from these render options, so sub-blocks
    /// (inside lists, block quotes, alerts, components) scan with the same
    /// feature set as the top level.
    pub(crate) fn scan_ctx(&self) -> ScanCtx<'_> {
        ScanCtx {
            math: self.gfm_math,
            component_tags: &self.component_tags,
            inline_component_tags: &self.inline_component_tags,
            lenient_lists: self.lenient_lists,
        }
    }

    /// The ` dir="auto"` attribute (with a leading space) when bidi is on, else
    /// empty — appended inside block-level opening tags.
    pub(crate) fn dir(&self) -> &'static str {
        if self.dir_auto {
            " dir=\"auto\""
        } else {
            ""
        }
    }
}

/// CommonMark §4.7 label normalization: lowercase, collapse internal
/// whitespace runs to a single space, strip leading/trailing whitespace.
pub fn normalize_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = true;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            // CommonMark normalizes with Unicode *case folding*, not simple
            // lowercasing. The one fold that differs and appears in the spec
            // suite is ß / ẞ → "ss".
            if c == 'ß' || c == 'ẞ' {
                out.push_str("ss");
            } else {
                out.extend(c.to_lowercase());
            }
            in_ws = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

/// CommonMark §6.3 link-label validity: 1–999 characters between the brackets,
/// at least one non-whitespace character, and no unescaped `[` or `]`.
pub fn valid_link_label(s: &str) -> bool {
    if s.chars().count() > 999 {
        return false;
    }
    let bytes = s.as_bytes();
    let mut has_content = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                has_content = true;
                i += 2;
            }
            b'[' | b']' => return false,
            c if !c.is_ascii_whitespace() => {
                has_content = true;
                i += 1;
            }
            _ => i += 1,
        }
    }
    has_content
}

/// `finalizing` is the parser's finalize pass (see `StreamParser::reparse_tail`).
/// It gates exactly one thing: a code fence's `meta` (see the `CodeFence` arm).
pub fn classify(
    raw: &RawBlockKind,
    slice: &str,
    gfm_alerts: bool,
    allow_schemes: &[Box<str>],
    finalizing: bool,
) -> BlockKind {
    if gfm_alerts {
        if let RawBlockKind::Blockquote = raw {
            if let Some(kind) = alert_head(&blockquote_inner(slice)) {
                return BlockKind::Alert { kind, nested: None };
            }
        }
    }
    match raw {
        // `rich` is filled in at the top-level promotion site (parser.rs) from
        // `render_block`'s `Enrichment::Heading` when `block_data` is on; `None`
        // here keeps the off-path (and nested-heading) wire byte-identical.
        RawBlockKind::Heading { level } => BlockKind::Heading { level: *level, rich: None },
        RawBlockKind::SetextHeading { level } => BlockKind::Heading { level: *level, rich: None },
        RawBlockKind::Paragraph => BlockKind::Paragraph,
        RawBlockKind::CodeFence { info, .. } => {
            let (lang, meta) = split_info(info);
            // `meta` is exposed only once it can no longer change: the opener line
            // has its terminating `\n`, or the stream is finalizing (which makes a
            // still-partial opener line final by definition, so nothing is lost at
            // EOF). While that line is still growing, its remainder is a
            // half-arrived value — publishing it would flicker a filename header
            // through every prefix, and would force the streaming `FenceInfoCache`
            // (whose whole contract is FROZEN output, O(new bytes) per append) to
            // re-derive a growing string on every append, which is quadratic in the
            // line length. `lang` needs no such gate: it is the FIRST word, and the
            // cache only arms once that word is settled by following whitespace.
            // Both the full path and every cache classify through here, so
            // streaming and one-shot agree at every prefix (`midstream_parity.rs`).
            // The scan is O(opener line): it stops at the first `\n`, and a slice
            // with none IS the opener line.
            let meta = if finalizing || slice.as_bytes().contains(&b'\n') { meta } else { None };
            match lang {
                "math" | "latex" | "tex" => BlockKind::MathBlock(None),
                "mermaid" => BlockKind::Mermaid,
                "" => BlockKind::CodeBlock { lang: None, meta: None, code: None },
                other => BlockKind::CodeBlock {
                    lang: Some(other.to_string()),
                    meta: meta.map(str::to_string),
                    code: None,
                },
            }
        }
        // An indented code block has no info string at all — never a lang or meta.
        RawBlockKind::IndentedCode => BlockKind::CodeBlock { lang: None, meta: None, code: None },
        RawBlockKind::MathFence { .. } => BlockKind::MathBlock(None),
        RawBlockKind::List { ordered, .. } => {
            BlockKind::List { ordered: *ordered, start: None, items: Vec::new() }
        }
        RawBlockKind::Blockquote => BlockKind::Blockquote(None),
        RawBlockKind::Table => BlockKind::Table(None),
        RawBlockKind::HorizontalRule => BlockKind::Rule,
        RawBlockKind::HtmlBlock { .. } => BlockKind::Html,
        RawBlockKind::ComponentBlock { tag, .. } => {
            // Attributes are parsed + sanitized from the open tag for the JS layer
            // (`components[tag]` receives them); the same sanitizer feeds the HTML
            // wrapper in render_component. PERMISSIVE tier (`sanitize_attrs`):
            // these are props the consumer's component mediates, so the raw-HTML
            // DOM denylist (`id`/`name`, `slot`, `form*`, … — see
            // `RAW_HTML_DROPPED_ATTRS`) is deliberately NOT applied here.
            let open = slice.trim_start_matches([' ', '\t']);
            BlockKind::Component { tag: tag.clone(), attrs: sanitize_attrs(open, allow_schemes) }
        }
        RawBlockKind::LinkRefDefinition => BlockKind::Paragraph, // no output anyway
    }
}

/// The opt-in structured `kind.data` payload a top-level block can carry, in the
/// shape `render_block` returns it. This is the generic enrichment carrier on the
/// render side: each enriched kind gets one variant, and the promotion site
/// (parser.rs) folds it onto the matching `BlockKind` `Option` field. Off (or for
/// any kind without an opt-in payload) `render_block` returns `None`.
pub enum Enrichment {
    /// Top-level GFM table — folds onto `BlockKind::Table(Some(_))`.
    Table(TableData),
    /// ATX or Setext heading — folds onto `BlockKind::Heading { rich: Some(_) }`.
    Heading(HeadingData),
    /// Fenced or indented code block — folds the decoded source onto
    /// `BlockKind::CodeBlock { code: Some(_), .. }` (the classified `lang` is
    /// preserved).
    CodeBlock(String),
    /// Display-math block — folds onto `BlockKind::MathBlock(Some(_))`.
    MathBlock(MathBlockData),
    /// Ordered/unordered list — folds the start number and the per-item inner
    /// HTML onto `BlockKind::List { start: Some(_), items, .. }` (the classified
    /// `ordered` is preserved).
    List(u32, Vec<Rc<ListItemData>>),
    /// Blockquote — folds onto `BlockKind::Blockquote(Some(_))`.
    Blockquote(ContainerData),
    /// GFM alert — folds onto `BlockKind::Alert { nested: Some(_), .. }` (the
    /// classified `kind` is preserved).
    Alert(ContainerData),
}

/// Render one block to HTML. Returns `Some(Enrichment)` only for a top-level
/// Maximum container-nesting depth for the recursive descent below. Blockquotes,
/// list items, alerts, and component blocks all recurse through [`render_block`];
/// each level consumes a WASM shadow-stack frame, and a stack overflow in WASM is
/// an uncatchable **trap** (it poisons the whole instance), not a recoverable
/// error. This cap turns a deeply-nested adversarial input (e.g.
/// `">".repeat(10_000)`) into graceful flat output instead of a crash, and keeps
/// the worst-case stack well under the (now 256 KB) shadow stack. 100 is far above
/// any real document — CommonMark/GFM spec examples nest <10 — so it never affects
/// legitimate content. See also the inline cap `MAX_BRACKET_DEPTH`.
const MAX_RENDER_DEPTH: usize = 100;

thread_local! {
    /// Live nesting depth of the [`render_block`] recursion. WASM is
    /// single-threaded so this is just a module global; on a native multi-threaded
    /// host each thread gets its own counter, which is also correct.
    static RENDER_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Restores [`RENDER_DEPTH`] on *every* exit path of [`render_block`] — which has
/// many early `return`s — so the counter can never leak across calls.
struct DepthGuard(usize);
impl Drop for DepthGuard {
    fn drop(&mut self) {
        RENDER_DEPTH.with(|d| d.set(self.0));
    }
}

/// block whose kind has an opt-in `kind.data` payload (Table, Heading) when
/// `opts.block_data` is on; `None` for every other kind and whenever the flag is
/// off. Nested (recursive) call sites ignore the return — only blocks that
/// appear at the document top level get a `kind.data`.
/// `base` is `source`'s own DOCUMENT-ABSOLUTE start offset, used only to stamp
/// absolute source offsets onto the opt-in structured channel (list items). The
/// top-level caller passes `Some(tail_start)`; the recursive calls below render
/// SYNTHESIZED strings (a `>`-stripped blockquote inner, a de-indented list-item
/// body, an alert/component inner) that have no offset in the document, so they
/// pass `None` — and their `Enrichment` is discarded anyway.
pub fn render_block(source: &str, raw: &RawBlock, base: Option<usize>, opts: &RenderOpts, out: &mut String) -> Option<Enrichment> {
    let slice = &source[raw.range.clone()];
    // Depth guard: every recursive call (blockquote / list-item / alert /
    // component inner blocks) funnels back through here. Past the cap, stop
    // descending and emit the remaining inner source as escaped text — content is
    // preserved, just not further structured — rather than risk a stack-overflow
    // trap. No legitimate document reaches this depth.
    let depth = RENDER_DEPTH.with(|d| d.get());
    if depth >= MAX_RENDER_DEPTH {
        escape_html(slice, out);
        return None;
    }
    RENDER_DEPTH.with(|d| d.set(depth + 1));
    let _depth_guard = DepthGuard(depth);
    match &raw.kind {
        RawBlockKind::Heading { level } => {
            return render_heading(slice, *level, opts, out).map(Enrichment::Heading)
        }
        RawBlockKind::SetextHeading { level } => {
            return render_setext_heading(slice, *level, opts, out).map(Enrichment::Heading)
        }
        RawBlockKind::Paragraph => render_paragraph(slice, opts, out),
        RawBlockKind::CodeFence { info, fence_char, fence_len, terminated } => {
            // A ```math/```latex/```tex fence classifies to MathBlock (a ```mermaid
            // fence to Mermaid, which carries no enrichment); route the decoded
            // source onto the matching carrier so it rides the right `data`.
            let src = render_code_fence(slice, info, *fence_char, *fence_len, *terminated, opts, out);
            if let Some(code) = src {
                let lang = info.split_whitespace().next().unwrap_or("");
                match lang {
                    "math" | "latex" | "tex" => {
                        return Some(Enrichment::MathBlock(MathBlockData { latex: Rc::new(code) }))
                    }
                    // A ```mermaid fence classifies to the unit `Mermaid` kind,
                    // which is intentionally NOT enriched (see report) — drop the
                    // source so it is not mis-routed onto a CodeBlock carrier.
                    "mermaid" => {}
                    _ => return Some(Enrichment::CodeBlock(code)),
                }
            }
        }
        RawBlockKind::IndentedCode => {
            return render_indented_code(slice, opts, out).map(Enrichment::CodeBlock)
        }
        RawBlockKind::MathFence { terminated } => {
            return render_math_block(slice, *terminated, opts, out)
                .map(|latex| Enrichment::MathBlock(MathBlockData { latex: Rc::new(latex) }))
        }
        RawBlockKind::Blockquote => return render_blockquote(slice, opts, out),
        RawBlockKind::List { ordered, start } => {
            let items =
                render_list(slice, *ordered, *start, base.map(|b| b + raw.range.start), opts, out);
            if opts.block_data {
                return Some(Enrichment::List(*start, items));
            }
        }
        RawBlockKind::Table => return render_table(slice, opts, out).map(Enrichment::Table),
        RawBlockKind::HorizontalRule => out.push_str("<hr>"),
        RawBlockKind::HtmlBlock { .. } => render_html_block(slice, opts, out),
        RawBlockKind::ComponentBlock { tag, terminated } => {
            render_component(slice, tag, *terminated, opts, out)
        }
        RawBlockKind::LinkRefDefinition => { /* no output */ }
    }
    None
}

/// GitHub-style anchor slug for a heading's plaintext: lowercase, drop every
/// character that is not ASCII alphanumeric / space / hyphen, then collapse runs
/// of spaces (and surrounding whitespace) into single `-`. A pure function of the
/// heading's own text — no document-wide dedup counter (so it is trivially
/// streaming-consistent: a heading's slug never depends on what came before).
/// v1 limitation: two headings with identical text yield identical slugs.
pub(crate) fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_dash = false;
    let mut started = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_dash && started {
                out.push('-');
            }
            pending_dash = false;
            started = true;
            out.push(c.to_ascii_lowercase());
        } else if c == '-' {
            // A literal hyphen is kept (GitHub keeps `foo-bar` as `foo-bar`),
            // but never doubles a pending separator.
            if pending_dash && started {
                out.push('-');
                pending_dash = false;
            }
            if started {
                out.push('-');
            }
        } else {
            // Any other char (space, punctuation, non-ASCII) becomes a separator
            // boundary; emitted lazily so trailing separators are dropped.
            if started {
                pending_dash = true;
            }
        }
    }
    out
}

/// ATX heading inner content — also strip trailing whitespace from final
/// inline rendering (mirrors render_paragraph). When `opts.block_data` is on it
/// also returns the trimmed inner HTML (the bytes written to `out` between the
/// `<hN>`/`</hN>` tags) so the heading renderers can derive the structured
/// `kind.data` (plaintext + slug) from the SAME inline render that produced the
/// display HTML — no second inline pass. When OFF it returns `None` and does NOT
/// clone the inner span, so the default path pays zero extra allocation
/// (zero-cost-off, not merely byte-identical-off).
fn render_heading_inner_trimmed(content: &str, opts: &RenderOpts, out: &mut String) -> Option<String> {
    // Render directly into `out` then trim in place — no temp String. (block_data
    // off ⇒ still zero extra allocation; on ⇒ one alloc for the captured inner.)
    let inner_start = out.len();
    render_inline(content, opts, out);
    let keep = out[inner_start..]
        .trim_end_matches(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r')
        .len();
    out.truncate(inner_start + keep);
    if opts.block_data {
        Some(out[inner_start..].to_string())
    } else {
        None
    }
}

/// Build the opt-in `HeadingData` from a heading's inner HTML (already gated on
/// `block_data`, so `inner_html` is `Some` only when the flag is on): `text` is
/// the inline-stripped plaintext (the same `strip_inline_html` the client's
/// `outline()`/`htmlToText` mirrors), `id` its anchor `slug`. Returns `None`
/// when `block_data` is off (no inner span captured) so the heading carries no
/// enrichment (off path).
fn heading_data(level: u8, inner_html: Option<String>) -> Option<HeadingData> {
    let text = strip_inline_html(&inner_html?);
    let id = slug(&text);
    Some(HeadingData { level, text, id })
}

fn render_heading(slice: &str, level: u8, opts: &RenderOpts, out: &mut String) -> Option<HeadingData> {
    let bytes = slice.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] == b' ' {
        i += 1;
    }
    let mut hashes = 0;
    while i < bytes.len() && bytes[i] == b'#' {
        i += 1;
        hashes += 1;
    }
    let _ = hashes;
    while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let mut end = bytes.len();
    while end > i && (bytes[end - 1] == b'\n' || bytes[end - 1] == b'\r' || bytes[end - 1] == b' ') {
        end -= 1;
    }
    // Optional trailing #s (closing seq). Per CommonMark, only counts if
    // preceded by space or it's the only content.
    let trim_target = {
        let mut tail = end;
        while tail > i && bytes[tail - 1] == b'#' {
            tail -= 1;
        }
        if tail == i {
            // Heading content is only #s — strip them all.
            i
        } else if tail < end && (bytes[tail - 1] == b' ' || bytes[tail - 1] == b'\t') {
            // Strip closing hashes plus the separator.
            let mut t = tail - 1;
            while t > i && (bytes[t - 1] == b' ' || bytes[t - 1] == b'\t') {
                t -= 1;
            }
            t
        } else {
            end
        }
    };
    let content = std::str::from_utf8(&bytes[i..trim_target]).unwrap_or("");
    out.push('<');
    out.push('h');
    out.push((b'0' + level) as char);
    out.push_str(opts.dir());
    out.push('>');
    let inner = render_heading_inner_trimmed(content, opts, out);
    out.push_str("</h");
    out.push((b'0' + level) as char);
    out.push('>');
    heading_data(level, inner)
}

fn render_paragraph(slice: &str, opts: &RenderOpts, out: &mut String) {
    let trimmed = trim_trailing_newlines(slice);
    out.push_str("<p");
    out.push_str(opts.dir());
    out.push('>');
    // Render inline directly into `out` (no temp String / copy), then strip the
    // trailing whitespace in place. render_inline already targets a non-empty
    // `out` correctly (same idiom as list items / table cells).
    let inner_start = out.len();
    render_inline_para(trimmed, opts, out);
    // CommonMark: trailing whitespace at end of final line is stripped.
    let keep = out[inner_start..]
        .trim_end_matches(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r')
        .len();
    out.truncate(inner_start + keep);
    out.push_str("</p>");
}

/// Render a fenced code block. When `opts.block_data` is on, also returns the
/// DECODED source (the exact `content` it escapes into the `<pre><code>` body) so
/// the enrichment carries the same text `decodeCodeText` re-derives from the HTML;
/// returns `None` when off (zero extra allocation).
fn render_code_fence(
    slice: &str,
    info: &str,
    _fence_char: u8,
    _fence_len: usize,
    _terminated: bool,
    opts: &RenderOpts,
    out: &mut String,
) -> Option<String> {
    let bytes = slice.as_bytes();
    let first_nl = bytes.iter().position(|&b| b == b'\n').map(|i| i + 1).unwrap_or(bytes.len());
    let content_start = first_nl;
    let mut content_end = bytes.len();
    if content_end > content_start {
        while content_end > content_start && (bytes[content_end - 1] == b'\n' || bytes[content_end - 1] == b'\r') {
            content_end -= 1;
        }
        let last_nl = bytes[content_start..content_end].iter().rposition(|&b| b == b'\n');
        let last_line_start = match last_nl {
            Some(p) => content_start + p + 1,
            None => content_start,
        };
        let last_line = &bytes[last_line_start..content_end];
        if is_fence_close_line(last_line) {
            // Cut at the closer's line START, not one byte before it: the `\n`
            // at `last_line_start - 1` terminates the last CONTENT line and
            // belongs to the body. Treating it as the closer's separator ate a
            // trailing blank line (CommonMark example 318). The normal case is
            // unaffected — the body then simply ends with the `\n` that the
            // `!content.ends_with('\n')` guard below used to re-add.
            content_end = last_line_start;
            if content_end < content_start {
                content_end = content_start;
            }
        }
    }
    let raw = if content_end > content_start {
        std::str::from_utf8(&bytes[content_start..content_end]).unwrap_or("")
    } else {
        ""
    };
    // §4.5: an indented opening fence removes up to that many columns of
    // indentation from EACH body line (a line with less loses only what it has).
    // Indent 0 — the overwhelmingly common case — keeps the zero-copy slice.
    let deindented = match fence_indent(bytes) {
        0 => None,
        n => Some(strip_fence_indent(raw, n)),
    };
    let content: &str = deindented.as_deref().unwrap_or(raw);

    push_code_fence_open(info, out);
    escape_html(content, out);
    if !content.is_empty() && !content.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("</code></pre>");
    // Opt-in source: the bytes BETWEEN the tags, decoded — which here is `content`
    // itself plus the SAME trailing-`\n` normalization the HTML body carries, so
    // `data.code` is byte-identical to `decodeCodeText(block.html)`. Off ⇒ no work.
    if opts.block_data {
        Some(code_body_source(content))
    } else {
        None
    }
}

/// Columns of indentation on a fenced block's OPENER line — the width each body
/// line may shed (§4.5). A fence opener carries 0–3 leading SPACES by
/// construction (`scan_fence` rejects 4+, and a leading tab already counts 4), so
/// this looks at no more than three bytes.
pub(crate) fn fence_indent(slice: &[u8]) -> usize {
    slice.iter().take(3).take_while(|&&b| b == b' ').count()
}

/// De-indent a fenced block's body: up to `cols` columns off each line. Shared
/// with the streaming fence cache, which applies it per line as lines arrive.
pub(crate) fn strip_fence_indent(body: &str, cols: usize) -> String {
    let mut out = String::with_capacity(body.len());
    for line in body.split_inclusive('\n') {
        strip_cols_into(line.as_bytes(), cols, &mut out);
    }
    out
}

/// The decoded source the `<pre><code>` body holds for a `content` string: empty
/// stays empty; otherwise a trailing `\n` is guaranteed (mirroring the HTML the
/// code renderers emit), so it equals `decodeCodeText(block.html)` byte-for-byte.
fn code_body_source(content: &str) -> String {
    if content.is_empty() {
        String::new()
    } else if content.ends_with('\n') {
        content.to_string()
    } else {
        let mut s = String::with_capacity(content.len() + 1);
        s.push_str(content);
        s.push('\n');
        s
    }
}

/// Split a code fence's info string into its LANGUAGE (the first
/// whitespace-delimited word, `""` when the info string is empty) and its META
/// (the remainder, trimmed; `None` when nothing follows the language). CommonMark
/// §4.5 makes the info string one opaque run — the "first word is the language"
/// split is convention, and `meta` is simply the rest of the same string, so both
/// halves are the RAW source text: backslash escapes and entity references are
/// left undecoded on the data channel exactly as `lang` already leaves them
/// (`push_code_fence_open` decodes only for the HTML `class`/`data-lang`).
/// Shared by `classify` and the streaming fence-info cache so the two can't drift.
pub(crate) fn split_info(info: &str) -> (&str, Option<&str>) {
    // The scanner already trims `info`; trimming here keeps the helper total for
    // the cache's raw-buffer slice and makes the first word match
    // `split_whitespace().next()` exactly.
    let info = info.trim();
    match info.find(char::is_whitespace) {
        None => (info, None),
        Some(i) => {
            let (lang, rest) = info.split_at(i);
            let rest = rest.trim();
            (lang, if rest.is_empty() { None } else { Some(rest) })
        }
    }
}

/// Emit a code-fence opening tag `<pre><code…>` for the given info string.
/// Shared by the block renderer and the streaming-parser's incremental
/// code-fence cache so their output can't drift. CommonMark §4.5: the info
/// string is processed for backslash escapes and entity references.
pub(crate) fn push_code_fence_open(info: &str, out: &mut String) {
    let lang_raw = info.split_whitespace().next().unwrap_or("");
    let lang = crate::url::decode_text(lang_raw);
    out.push_str("<pre><code");
    if !lang.is_empty() {
        out.push_str(" class=\"language-");
        escape_attr(&lang, out);
        out.push_str("\" data-lang=\"");
        escape_attr(&lang, out);
        out.push('"');
    }
    out.push('>');
}

/// True if `line` (a body line, with or without its trailing newline) reads as
/// a closing code fence: ≤3 leading spaces, then ≥3 `` ` `` or `~`, then only
/// whitespace. The streaming cache bails to the full renderer on any such line
/// — that covers the real closer *and* the rarer "fence-looking but not the
/// closer" case the block renderer trims, so cached output can't diverge.
pub(crate) fn is_fence_close_line(line: &[u8]) -> bool {
    let mut i = 0;
    while i < line.len() && line[i] == b' ' && i < 3 {
        i += 1;
    }
    if i >= line.len() {
        return false;
    }
    let c = line[i];
    if c != b'`' && c != b'~' {
        return false;
    }
    let mut len = 0;
    while i + len < line.len() && line[i + len] == c {
        len += 1;
    }
    if len < 3 {
        return false;
    }
    for &b in &line[i + len..] {
        if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
            continue;
        }
        return false;
    }
    true
}

/// Display-math block (`$$…$$` / `\[…\]`). Emits `<div class="math
/// math-display">` carrying the HTML-escaped LaTeX source — KaTeX auto-render
/// (or a `components.MathBlock` override) consumes that `class` and reads the
/// LaTeX from the element's text content. We never process the body as
/// markdown. An open (still-streaming) block has no closer yet, so its content
/// is everything after the opener.
/// Render a display-math fence (`$$…$$` / `\[…\]`). When `opts.block_data` is on,
/// also returns the decoded LaTeX source (the trimmed `content` it escapes into
/// the `<div class="math math-display">` body), matching `decodeMathText(block
/// .html)`; `None` when off.
fn render_math_block(slice: &str, terminated: bool, opts: &RenderOpts, out: &mut String) -> Option<String> {
    // Leading indent is ≤3 spaces (guaranteed by the scanner); trim it plus any
    // trailing newline so we can match the opener delimiter.
    let s = slice.trim_start_matches([' ', '\t']);
    let (open, close): (&str, &str) = if s.starts_with("$$") {
        ("$$", "$$")
    } else if s.starts_with("\\[") {
        ("\\[", "\\]")
    } else {
        // Defensive: scanner only produces these two openers.
        ("", "")
    };
    let after_open = &s[open.len().min(s.len())..];
    let content = if terminated && !close.is_empty() {
        match after_open.rfind(close) {
            Some(idx) => &after_open[..idx],
            None => after_open,
        }
    } else {
        after_open
    };
    let content = content.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r'));
    out.push_str("<div class=\"math math-display\">");
    escape_html(content, out);
    out.push_str("</div>");
    // The HTML body is exactly `escape_html(content)`, so the decoded LaTeX source
    // is `content` — byte-identical to `decodeMathText(block.html)`. Off ⇒ no work.
    if opts.block_data {
        Some(content.to_string())
    } else {
        None
    }
}

fn render_setext_heading(slice: &str, level: u8, opts: &RenderOpts, out: &mut String) -> Option<HeadingData> {
    let bytes = slice.as_bytes();
    let mut end = bytes.len();
    while end > 0 && matches!(bytes[end - 1], b'\n' | b'\r' | b' ' | b'\t') {
        end -= 1;
    }
    let mut last_nl = end;
    while last_nl > 0 && bytes[last_nl - 1] != b'\n' {
        last_nl -= 1;
    }
    let content_end = if last_nl > 0 { last_nl - 1 } else { 0 };
    let mut start = 0;
    while start < content_end && matches!(bytes[start], b' ' | b'\t') {
        start += 1;
    }
    let mut content_trim = content_end;
    while content_trim > start
        && matches!(bytes[content_trim - 1], b'\n' | b'\r' | b' ' | b'\t')
    {
        content_trim -= 1;
    }
    let content = std::str::from_utf8(&bytes[start..content_trim]).unwrap_or("");
    out.push('<');
    out.push('h');
    out.push((b'0' + level) as char);
    out.push_str(opts.dir());
    out.push('>');
    let inner = render_heading_inner_trimmed(content, opts, out);
    out.push_str("</h");
    out.push((b'0' + level) as char);
    out.push('>');
    heading_data(level, inner)
}

/// Drop an indented code body's trailing BLANK LINES — and only those. The
/// trailing whitespace INSIDE the last content line is significant and must
/// survive (`"    foo  "` → `<code>foo  `, CommonMark example 118), so a plain
/// `trim_end` is wrong here: code is the one place whitespace carries meaning.
///
/// cmark's `chop_trailing_blank_lines`: walk back to the last non-whitespace
/// byte, then cut at the first `\n` at or after it (everything past that point
/// is whitespace-only lines). Both scans stay inside the trailing whitespace
/// run, so this is O(trailing run) — never a rescan of the body.
pub(crate) fn chop_trailing_blank_lines(content: &str) -> &str {
    let bytes = content.as_bytes();
    let last = match bytes.iter().rposition(|&b| !matches!(b, b' ' | b'\t' | b'\r' | b'\n')) {
        Some(i) => i,
        None => return "",
    };
    // `last` is a non-whitespace byte, so any `\n` found sits strictly after it
    // and is a char boundary — the slice is always valid UTF-8.
    match bytes[last..].iter().position(|&b| b == b'\n') {
        Some(off) => &content[..last + off],
        None => content,
    }
}

/// Render an indented code block. When `opts.block_data` is on, also returns the
/// decoded source (the de-indented body + the trailing `\n` the HTML always
/// carries), matching `decodeCodeText(block.html)`; `None` when off.
fn render_indented_code(slice: &str, opts: &RenderOpts, out: &mut String) -> Option<String> {
    let mut content = String::with_capacity(slice.len());
    for line in slice.split_inclusive('\n') {
        let bytes = line.as_bytes();
        let mut i = 0;
        let mut consumed = 0;
        while i < bytes.len() && consumed < 4 {
            match bytes[i] {
                b' ' => {
                    consumed += 1;
                    i += 1;
                }
                b'\t' => {
                    i += 1;
                    break;
                }
                _ => break,
            }
        }
        content.push_str(std::str::from_utf8(&bytes[i..]).unwrap_or(""));
    }
    let trimmed = chop_trailing_blank_lines(&content);
    out.push_str("<pre><code>");
    escape_html(trimmed, out);
    out.push('\n');
    out.push_str("</code></pre>");
    // The HTML body is `trimmed` + an always-present trailing `\n`, so the decoded
    // source is `trimmed + "\n"` — byte-identical to `decodeCodeText(block.html)`.
    if opts.block_data {
        let mut s = String::with_capacity(trimmed.len() + 1);
        s.push_str(trimmed);
        s.push('\n');
        Some(s)
    } else {
        None
    }
}

/// Columns of leading whitespace a blockquote's stripped content keeps when a
/// TAB follows the `>` marker (`rest` is the line past `>`, whose `>` sat at
/// column `indent`). §2.2: the marker's optional trailing space is one COLUMN,
/// so the tab is only PARTIALLY consumed — and every tab after it in the run had
/// its stop measured from the ORIGINAL column, so re-basing the line to column 0
/// would move them. The whole run therefore materializes as spaces.
/// `>\t\tfoo` ⇒ 6 spaces + `foo`, which is indented code (CommonMark example 6).
/// Shared shape with [`strip_container_delta`](crate::parser)'s streaming twin.
fn quote_tab_content(rest: &str, indent: usize) -> String {
    let b = rest.as_bytes();
    // `>` occupies column `indent`, so the run starts at column `indent + 1`.
    let mut col = indent + 1;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b' ' => col += 1,
            b'\t' => col += 4 - (col % 4),
            _ => break,
        }
        i += 1;
    }
    // The marker ate `>` plus one column; the columns past it are content.
    let spaces = col - (indent + 2);
    let mut out = String::with_capacity(spaces + rest.len() - i);
    for _ in 0..spaces {
        out.push(' ');
    }
    out.push_str(&rest[i..]);
    out
}

/// Indentation a lazy continuation line is re-emitted at when a container
/// rebuilds its inner document ([`push_lazy_line`]).
///
/// A lazy line is, by definition, paragraph continuation TEXT — the scanner only
/// keeps it inside the container when it can be nothing else. The re-scan of the
/// rebuilt inner document has lost that context, though, so a line like `===`,
/// `- bar` or `# x` would open a block there (CommonMark examples 93, 238, 312).
/// Four columns is the one indent that is inert to every block start (a setext
/// underline takes at most 3, and indented code cannot interrupt a paragraph)
/// while staying invisible in the output: a paragraph's leading spaces or tabs
/// are stripped at render time (`render_inline_para` + the inline break arms),
/// which is what lets the line keep its own `\n` instead of being glued to the
/// previous one with a space.
pub(crate) const LAZY_INDENT: &str = "    ";

/// Append one lazy continuation line's content at [`LAZY_INDENT`], returning the
/// number of bytes pushed. The caller owns the line terminator (a streaming
/// PARTIAL lazy line has none yet). Shared by `blockquote_inner`, `item_body`
/// and the streaming container cache so all three build byte-identical inner
/// documents.
pub(crate) fn push_lazy_line(out: &mut String, line: &str) -> usize {
    let content = line.trim_start_matches([' ', '\t']);
    out.push_str(LAZY_INDENT);
    out.push_str(content);
    LAZY_INDENT.len() + content.len()
}

/// Strip the blockquote prefix (≤3 spaces, one `>`, one optional space) from
/// each line, yielding the inner document text.
pub(crate) fn blockquote_inner(slice: &str) -> String {
    let mut inner = String::with_capacity(slice.len());
    for line in slice.lines() {
        let mut s = line;
        let mut indent = 0;
        for c in s.chars() {
            if c == ' ' && indent < 3 {
                indent += 1;
            } else {
                break;
            }
        }
        s = &s[indent..];
        if let Some(stripped) = s.strip_prefix('>') {
            let owned;
            s = if stripped.as_bytes().first() == Some(&b'\t') {
                owned = quote_tab_content(stripped, indent);
                &owned
            } else {
                stripped.strip_prefix(' ').unwrap_or(stripped)
            };
            inner.push_str(s);
            inner.push('\n');
        } else {
            // A line without a `>` is a lazy paragraph continuation (the
            // scanner only kept valid ones), so it keeps its OWN line: the soft
            // break between it and the line above is a `\n`, not a space
            // (examples 93, 232, 233, 238, 247, 250, 251). It is re-emitted at
            // [`LAZY_INDENT`] columns so the re-scan still can't reinterpret it
            // as a new block — see that constant.
            push_lazy_line(&mut inner, s);
            inner.push('\n');
        }
    }
    inner
}

/// GitHub alert keyword if `inner` (a blockquote's `>`-stripped content) opens
/// with a line that is exactly `[!NOTE]` (or TIP/IMPORTANT/WARNING/CAUTION).
/// The marker must be the whole first line — trailing text disqualifies it,
/// matching GitHub.
pub(crate) fn alert_head(inner: &str) -> Option<AlertKind> {
    let first = inner.lines().next()?;
    let kw = first.trim().strip_prefix("[!")?.strip_suffix(']')?;
    AlertKind::from_keyword(kw)
}

/// cmark's `cr()`: end the current output line — append `\n` only if `out` is
/// non-empty and does not ALREADY end with one. Every container (`<li>`,
/// `<blockquote>`, an alert body) separates its children with this rather than
/// an unconditional push, because some children serialize their own trailing
/// newline (a raw HTML block — see `render_html_block`) and a second one would
/// be a real byte divergence (CommonMark example 174), while a container with
/// no children still needs the newline its opener implies (examples 218/239).
/// O(1) — a byte test at a push that already happens.
pub(crate) fn cr(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn render_blockquote(slice: &str, opts: &RenderOpts, out: &mut String) -> Option<Enrichment> {
    let inner = blockquote_inner(slice);
    if opts.gfm_alerts {
        if let Some(kind) = alert_head(&inner) {
            return render_alert(&inner, kind, opts, out);
        }
    }
    out.push_str("<blockquote");
    out.push_str(opts.dir());
    out.push('>');
    // Ref defs render to nothing (their content was hoisted into the table).
    let sub: Vec<_> = scan(&inner, opts.scan_ctx())
        .into_iter()
        .filter(|b| !matches!(b.kind, RawBlockKind::LinkRefDefinition))
        .collect();
    // Unconditional (cmark opens a blockquote with `cr()`): an EMPTY blockquote
    // still renders as `<blockquote>\n</blockquote>` (examples 218, 239, 240).
    out.push('\n');
    // Opt-in: capture each inner sub-block's own HTML fragment (byte-identical to
    // what lands in `out`) into the structured `nested` channel so a keyed
    // override can render children one node at a time. Off ⇒ no Vec, no Enrichment.
    let mut nested: Vec<Rc<NestedBlock>> = if opts.block_data { Vec::with_capacity(sub.len()) } else { Vec::new() };
    for b in &sub {
        let frag_start = out.len();
        render_block(&inner, b, None, opts, out);
        if opts.block_data {
            nested.push(Rc::new(NestedBlock { html: out[frag_start..].to_string() }));
        }
        cr(out);
    }
    out.push_str("</blockquote>");
    if opts.block_data {
        Some(Enrichment::Blockquote(ContainerData { nested }))
    } else {
        None
    }
}

/// Render a GitHub alert as `<div class="markdown-alert markdown-alert-TYPE">`
/// (GitHub-compatible class names so existing markdown CSS styles it). The body
/// is everything after the `[!TYPE]` title line, scanned as sub-blocks exactly
/// like a blockquote.
fn render_alert(inner: &str, kind: AlertKind, opts: &RenderOpts, out: &mut String) -> Option<Enrichment> {
    // role="note" (not "alert") for a11y: "alert" forces an immediate
    // screen-reader announcement, which during streaming would be obnoxious.
    out.push_str("<div class=\"markdown-alert markdown-alert-");
    out.push_str(kind.class());
    out.push_str("\" data-alert=\"");
    out.push_str(kind.class());
    out.push_str("\" role=\"note\"");
    out.push_str(opts.dir());
    out.push_str(">\n<p class=\"markdown-alert-title\"");
    out.push_str(opts.dir());
    out.push('>');
    out.push_str(kind.title());
    out.push_str("</p>\n");
    // Body = inner minus its first line (the marker).
    let body = match inner.find('\n') {
        Some(nl) => &inner[nl + 1..],
        None => "",
    };
    let sub: Vec<_> = scan(body, opts.scan_ctx())
        .into_iter()
        .filter(|b| !matches!(b.kind, RawBlockKind::LinkRefDefinition))
        .collect();
    // Opt-in: capture each body sub-block's HTML fragment into `nested` (the
    // title line is the wrapper, not a body block, so it is excluded). Mirrors
    // `render_blockquote`'s nested capture.
    let mut nested: Vec<Rc<NestedBlock>> = if opts.block_data { Vec::with_capacity(sub.len()) } else { Vec::new() };
    for b in &sub {
        let frag_start = out.len();
        render_block(body, b, None, opts, out);
        if opts.block_data {
            nested.push(Rc::new(NestedBlock { html: out[frag_start..].to_string() }));
        }
        cr(out);
    }
    out.push_str("</div>");
    if opts.block_data {
        Some(Enrichment::Alert(ContainerData { nested }))
    } else {
        None
    }
}

// --------------------------------------------------------------------------
// GFM footnotes (gated on opts.gfm_footnotes). v1 limits: single-block
// definitions (soft-wrapped lines joined; no continuation-indent), no nesting,
// one backref per definition. References render speculatively (committed
// blocks freeze), the section is emitted at finalize.
// --------------------------------------------------------------------------

/// A label is footnote-valid if it is non-empty and has no whitespace or `[`.
fn valid_footnote_label(label: &str) -> bool {
    !label.is_empty() && !label.contains(|c: char| c.is_whitespace() || c == '[' || c == ']')
}

/// Parse one `[^label]: content` line, returning (label, content). None if the
/// line isn't a footnote-definition opener.
fn parse_def_line(line: &str) -> Option<(String, String)> {
    let rest = line.trim_start().strip_prefix("[^")?;
    let close = rest.find(']')?;
    let label = &rest[..close];
    if !valid_footnote_label(label) {
        return None;
    }
    let content = rest[close + 1..].strip_prefix(':')?;
    Some((label.to_string(), content.trim().to_string()))
}

/// True if `slice` is a footnote-definition block (its first line opens one).
pub(crate) fn is_footnote_def_block(slice: &str) -> bool {
    slice.lines().next().is_some_and(|l| parse_def_line(l).is_some())
}

/// Extract every footnote definition in a block. Adjacent `[^a]: …` / `[^b]: …`
/// lines are separate definitions (GitHub allows this without blank lines); a
/// line that doesn't open a new definition continues the current one (soft
/// break → space).
pub(crate) fn footnote_defs(slice: &str) -> Vec<(String, String)> {
    let mut defs: Vec<(String, String)> = Vec::new();
    let mut cur: Option<(String, String)> = None;
    for line in slice.lines() {
        if let Some(def) = parse_def_line(line) {
            if let Some(d) = cur.take() {
                defs.push(d);
            }
            cur = Some(def);
        } else if let Some((_, content)) = cur.as_mut() {
            let t = line.trim();
            if !t.is_empty() {
                if !content.is_empty() {
                    content.push(' ');
                }
                content.push_str(t);
            }
        }
    }
    if let Some(d) = cur.take() {
        defs.push(d);
    }
    defs
}

/// Byte offset (within `slice`) of the start of the LAST line that opens a
/// footnote definition, or 0 when only the first line (or none) does. Lines are
/// split exactly like [`footnote_defs`]' `slice.lines()` (a trailing `\r` is
/// stripped before the opener test; the trailing newline-less partial line
/// counts). Everything before that line belongs to earlier, SEALED definitions
/// — a def opener always terminates the previous def — so the streaming parser
/// can commit up to it while only the final def's body can still grow. The
/// opener test is byte-frozen: once a (possibly still-growing) line parses as
/// `[^label]: …`, appended bytes only extend its content, never un-open it.
pub(crate) fn last_footnote_def_opener(slice: &str) -> usize {
    let bytes = slice.as_bytes();
    let mut best = 0;
    let mut ls = 0;
    while ls < bytes.len() {
        let le = bytes[ls..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |r| ls + r);
        let mut ce = le;
        if ce > ls && bytes[ce - 1] == b'\r' {
            ce -= 1;
        }
        if ls > 0 {
            if let Ok(line) = std::str::from_utf8(&bytes[ls..ce]) {
                if parse_def_line(line).is_some() {
                    best = ls;
                }
            }
        }
        ls = le + 1;
    }
    best
}

/// Visit every footnote *reference* `[^label]` in `text`, in document order.
/// Definition lines (`[^x]:`) are skipped.
fn for_each_footnote_ref(text: &str, mut f: impl FnMut(&str)) {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'^' {
            let mut j = i + 2;
            let mut ok = true;
            while j < bytes.len() && bytes[j] != b']' {
                if bytes[j] == b'[' || bytes[j].is_ascii_whitespace() {
                    ok = false;
                    break;
                }
                j += 1;
            }
            if ok && j < bytes.len() && j > i + 2 && bytes.get(j + 1) != Some(&b':') {
                f(&text[i + 2..j]);
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
}

/// Assign each new footnote reference label the next number, in document order.
pub(crate) fn collect_footnote_refs(
    text: &str,
    nums: &mut HashMap<String, usize>,
    next: &mut usize,
) {
    for_each_footnote_ref(text, |label| {
        if !nums.contains_key(label) {
            nums.insert(label.to_string(), *next);
            *next += 1;
        }
    });
}

/// [`collect_footnote_refs`], layered: new labels go into `overlay`, first-wins
/// checked against BOTH `committed` and `overlay` — so the streaming pre-pass
/// numbers a tail without cloning the (growing) committed table per append.
pub(crate) fn collect_footnote_refs_overlay(
    text: &str,
    committed: &HashMap<String, usize>,
    overlay: &mut HashMap<String, usize>,
    next: &mut usize,
) {
    for_each_footnote_ref(text, |label| {
        if !committed.contains_key(label) && !overlay.contains_key(label) {
            overlay.insert(label.to_string(), *next);
            *next += 1;
        }
    });
}

/// Incremental [`collect_footnote_refs_overlay`] over a GROWING region: extends
/// first-occurrence numbering over `text[from..]` and returns the offset up to
/// which classification is SETTLED (future appends resume there instead of
/// re-scanning the whole region — first-occurrence numbering over an
/// append-only region is prefix-stable). Byte-for-byte mirror of
/// [`for_each_footnote_ref`], except at the region's edge, where a candidate's
/// ref-vs-def classification can still depend on unseen bytes:
///   - `[^label` with no `]` yet → nothing after it can be a candidate (the
///     label scan admits no `[`), so stop and re-scan from the `[` next append;
///   - `[^label]` whose `]` is the region's FINAL byte → `for_each_footnote_ref`
///     counts it as a ref NOW (`bytes.get(j+1) != Some(b':')`), but a `:`
///     arriving next append would turn it into a def opener — so it is numbered
///     for this append and reported back as SPECULATIVE; the caller retracts it
///     before the next extension re-classifies from the same `[`.
/// The returned speculative label is `Some` only when this call inserted it.
pub(crate) fn extend_footnote_refs(
    text: &str,
    from: usize,
    committed: &HashMap<String, usize>,
    overlay: &mut HashMap<String, usize>,
    next: &mut usize,
) -> (usize, Option<String>) {
    let bytes = text.as_bytes();
    let mut i = from;
    while i + 2 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'^' {
            let mut j = i + 2;
            let mut ok = true;
            while j < bytes.len() && bytes[j] != b']' {
                if bytes[j] == b'[' || bytes[j].is_ascii_whitespace() {
                    ok = false;
                    break;
                }
                j += 1;
            }
            if ok && j >= bytes.len() {
                // No `]` yet: unclassifiable, and no `[` follows within the
                // label scan — resume from this candidate next append.
                return (i, None);
            }
            if ok && j > i + 2 && bytes.get(j + 1) != Some(&b':') {
                let label = &text[i + 2..j];
                let inserted = if !committed.contains_key(label) && !overlay.contains_key(label) {
                    overlay.insert(label.to_string(), *next);
                    *next += 1;
                    true
                } else {
                    false
                };
                if j + 1 == bytes.len() {
                    // `]` is the region's final byte: counted now, but a `:`
                    // may still arrive — speculative.
                    return (i, if inserted { Some(label.to_string()) } else { None });
                }
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    // The trailing <3 bytes are never examined by the scan above, but a `[`
    // there can begin a future candidate — never settle past it.
    for p in i..bytes.len() {
        if bytes[p] == b'[' {
            return (p, None);
        }
    }
    (bytes.len(), None)
}

// Note: a raw `count_footnote_refs(committed_slice)` used to advance the
// committed occurrence map, but it counted `[^x]` inside code spans / escaped
// text (which emit no ref) → over-count → broken backrefs. The committed advance
// is now derived from the RESOLVED placeholder-token replay of the committed
// blocks (see `resolve_block_footnotes` + `occ_after_block` in `parser.rs`), so
// seed == tokens by construction. `for_each_footnote_ref` stays in use for the
// numbering passes (`collect_footnote_refs`).

/// Collect footnote definitions (label → rendered-inline HTML) from `text`.
/// First definition wins.
pub(crate) fn collect_footnote_defs(text: &str, defs: &mut HashMap<String, String>, opts: &RenderOpts) {
    for raw in scan(text, opts.scan_ctx()) {
        let slice = &text[raw.range.clone()];
        if !is_footnote_def_block(slice) {
            continue;
        }
        for (label, content) in footnote_defs(slice) {
            if !defs.contains_key(&label) {
                let mut html = String::new();
                render_inline(&content, opts, &mut html);
                defs.insert(label, html);
            }
        }
    }
}

// --- Footnote-ref placeholder tokens (streaming-cache path) ---------------
//
// A footnote ref's only occurrence-dependent slot is `id="fnref-{suffix}"`.
// In placeholder mode `try_footnote_ref` emits, in place of the suffix, the
// token `\u{0}F\u{1}{N}\u{1}{label}\u{0}`. NUL/SOH are control chars that the
// HTML escaper passes through verbatim and that never occur in normal rendered
// output, so the linear [`resolve_footnote_ids`] pass can find every token and
// rewrite it to the real `fnref-N`/`fnref-N-K` suffix in document order. If a
// label itself contains NUL or SOH (pathological markdown source) the emitter
// falls back to the normal non-placeholder path, so the tokens stay unambiguous.
pub(crate) const FN_TOK_OPEN: u8 = 0x00; // \u{0}
pub(crate) const FN_TOK_SEP: u8 = 0x01; // \u{1}
pub(crate) const FN_TOK_TAG: u8 = b'F';

/// Replace every footnote-ref placeholder token in `src` with its resolved
/// `fnref` suffix, copying all other bytes verbatim, into `out`. `occ` is the
/// running per-label occurrence map and is advanced exactly once per token (so
/// the token replay is the SOLE source of truth for the occurrence count — it
/// matches what the non-placeholder path would have produced byte-for-byte).
/// O(src.len()).
pub(crate) fn resolve_footnote_ids(src: &str, occ: &mut HashMap<String, usize>, out: &mut String) {
    resolve_footnote_tokens(src, out, |label| {
        let c = occ.entry(label.to_string()).or_insert(0);
        let k = *c;
        *c += 1;
        k
    });
}

/// [`resolve_footnote_ids`], layered: the running count for a label starts from
/// `base` (the Rc-shared committed occurrence map, never mutated) and advances
/// in `overlay` — so a full-tail resolve pass never clones the committed map.
pub(crate) fn resolve_footnote_ids_overlay(
    src: &str,
    base: &HashMap<String, usize>,
    overlay: &mut HashMap<String, usize>,
    out: &mut String,
) {
    resolve_footnote_tokens(src, out, |label| {
        let c = overlay
            .entry(label.to_string())
            .or_insert_with(|| base.get(label).copied().unwrap_or(0));
        let k = *c;
        *c += 1;
        k
    });
}

/// Shared token walk for the `resolve_footnote_ids*` variants. `occurrence_of`
/// returns the 0-based occurrence index for a label and advances its counter.
fn resolve_footnote_tokens(
    src: &str,
    out: &mut String,
    mut occurrence_of: impl FnMut(&str) -> usize,
) {
    let bytes = src.as_bytes();
    let out_start = out.len();
    let mut i = 0;
    while i < bytes.len() {
        // A token starts with `\u{0}F\u{1}`.
        if bytes[i] == FN_TOK_OPEN
            && bytes.get(i + 1) == Some(&FN_TOK_TAG)
            && bytes.get(i + 2) == Some(&FN_TOK_SEP)
        {
            // Parse `{N}` up to the next SOH.
            let n_start = i + 3;
            let mut j = n_start;
            while j < bytes.len() && bytes[j] != FN_TOK_SEP {
                j += 1;
            }
            // Parse `{label}` up to the closing NUL.
            let label_start = j + 1;
            let mut k = label_start;
            while k < bytes.len() && bytes[k] != FN_TOK_OPEN {
                k += 1;
            }
            if j < bytes.len() && k < bytes.len() {
                let n = &src[n_start..j];
                let label = &src[label_start..k];
                let occurrence = occurrence_of(label);
                // The token sits inside `id="fnref-<token>"`, so emit ONLY the
                // occurrence suffix (`N` or `N-K`), not the `fnref-` prefix.
                out.push_str(n);
                if occurrence != 0 {
                    out.push('-');
                    out.push_str(&(occurrence + 1).to_string());
                }
                i = k + 1; // skip past the closing NUL
                continue;
            }
            // Malformed/truncated token — unreachable from generated output
            // (try_footnote_ref emits tokens atomically), but if it ever occurs
            // (e.g. a forged sentinel via unsafe_html) DROP the lone reserved
            // open byte rather than leaking a control char into user HTML.
            i += 1;
            continue;
        }
        // Copy this byte verbatim (UTF-8 safe: tokens are ASCII-delimited and we
        // only ever advance past whole tokens or single bytes; non-token bytes
        // are copied through unchanged, preserving multi-byte sequences).
        let ch_len = utf8_char_len(bytes[i]);
        out.push_str(&src[i..i + ch_len]);
        i += ch_len;
    }
    // Invariant: no unresolved footnote placeholder token may survive into
    // user-facing HTML. Debug-only (zero release cost); the streaming fuzz
    // corpus exercises this across thousands of docs/feature combos.
    debug_assert!(
        !out.as_bytes()[out_start..]
            .windows(3)
            .any(|w| w == [FN_TOK_OPEN, FN_TOK_TAG, FN_TOK_SEP]),
        "unresolved footnote placeholder token survived resolution",
    );
}

/// Length in bytes of the UTF-8 character whose lead byte is `b`.
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1 // continuation/invalid lead — copy one byte, never panic
    }
}

/// The footnote section, emitted once at finalize, in reference-number order.
/// `occ` gives the number of references per label so each gets its own backref
/// (`fnref-N`, `fnref-N-2`, …). Referenced-but-undefined labels render an empty
/// item (dangling — honest).
pub(crate) fn render_footnote_section(
    nums: &HashMap<String, usize>,
    defs: &HashMap<String, String>,
    occ: &HashMap<String, usize>,
    dir: &str,
) -> String {
    if nums.is_empty() {
        return String::new();
    }
    let ordered: Vec<(&String, &usize)> = nums.iter().collect();
    // Ascending by reference number (unique per label). Custom stable sort keeps
    // std's driftsort out of the WASM binary.
    let order = crate::sort::stable_order(&ordered, |a, b| *a.1 <= *b.1);
    let mut out = String::from("<section class=\"footnotes\" role=\"doc-endnotes\">\n<ol");
    out.push_str(dir);
    out.push_str(">\n");
    for &oi in &order {
        let (label, num) = ordered[oi];
        let n = num.to_string();
        out.push_str("<li id=\"fn-");
        out.push_str(&n);
        out.push('"');
        out.push_str(dir);
        out.push('>');
        if let Some(html) = defs.get(label) {
            out.push_str(html);
        }
        // One backref per reference occurrence (≥1; a referenced label always
        // has at least one). The Kth (K≥1) targets `fnref-N-(K+1)` with a small
        // ordinal so the arrows are distinguishable.
        let count = (*occ.get(label).unwrap_or(&0)).max(1);
        for k in 0..count {
            let target = if k == 0 { n.clone() } else { format!("{n}-{}", k + 1) };
            out.push_str(" <a href=\"#fnref-");
            out.push_str(&target);
            out.push_str("\" class=\"footnote-backref\" aria-label=\"Back to reference ");
            out.push_str(&n);
            if k > 0 {
                out.push('-');
                out.push_str(&(k + 1).to_string());
            }
            out.push_str("\">\u{21a9}");
            if k > 0 {
                out.push_str("<sup>");
                out.push_str(&(k + 1).to_string());
                out.push_str("</sup>");
            }
            out.push_str("</a>");
        }
        out.push_str("</li>\n");
    }
    out.push_str("</ol>\n</section>");
    out
}

/// Strip an item's marker and per-line content indentation, yielding the item
/// body as a mini-document to be scanned recursively. Column-based, so a tab
/// straddling the strip boundary is partially preserved as spaces (§2.2).
pub(crate) fn item_body(item: &[u8], ctx: ScanCtx<'_>) -> Option<String> {
    let first_line_end =
        item.iter().position(|&b| b == b'\n').map(|i| i + 1).unwrap_or(item.len());
    let first_line = &item[..first_line_end];
    let m = scan_marker(first_line, ctx)?;
    let ci = m.content_indent;
    let mut body = String::with_capacity(item.len());
    // A tab-padded marker leaves columns the byte slice can't carry — replay
    // them as spaces so the first line's geometry survives the re-basing.
    for _ in 0..m.content_overflow {
        body.push(' ');
    }
    body.push_str(std::str::from_utf8(&first_line[m.content_byte..]).unwrap_or(""));
    let mut pos = first_line_end;
    while pos < item.len() {
        let line = line_slice(item, pos);
        let is_blank = line.iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
        // A non-blank line indented less than the content column, immediately
        // after paragraph text, is a lazy continuation: it keeps its own line
        // (the soft break is a `\n` — examples 290-293, 312) but is re-indented
        // so the re-scan can't read it as a new block (e.g. a nested list).
        if !is_blank && indent_cols(line) < ci && !body.ends_with("\n\n") && !body.is_empty() {
            if !body.ends_with('\n') {
                body.push('\n');
            }
            push_lazy_line(&mut body, std::str::from_utf8(line).unwrap_or(""));
            if !body.ends_with('\n') {
                body.push('\n');
            }
        } else {
            body.push_str(&strip_cols(line, ci));
        }
        pos += line.len();
    }
    Some(body)
}

/// Consume up to `cols` columns of `line`'s leading whitespace (tabs expanding
/// to width-4 stops measured from the line start, §2.2) and report how to re-emit
/// the remainder at column 0: `(spaces_to_prepend, rest_bytes)`.
///
/// When the boundary falls exactly between characters, nothing is prepended. When
/// a TAB STRADDLES the boundary the rest of the leading whitespace run is consumed
/// too, and its full column width is returned as spaces: those later tabs' stops
/// were measured from the ORIGINAL column, so leaving them literal would re-expand
/// them from the re-based column and silently move the content (CommonMark
/// examples 5/7). Materializing the whole run as spaces is the only faithful
/// re-basing. O(leading whitespace run).
pub(crate) fn split_cols(line: &[u8], cols: usize) -> (usize, &[u8]) {
    let mut col = 0;
    let mut i = 0;
    while i < line.len() && col < cols {
        match line[i] {
            b' ' => {
                col += 1;
                i += 1;
            }
            b'\t' => {
                let w = 4 - (col % 4);
                i += 1;
                if col + w <= cols {
                    col += w;
                } else {
                    // Straddle: finish the whitespace run in true columns.
                    col += w;
                    while i < line.len() {
                        match line[i] {
                            b' ' => col += 1,
                            b'\t' => col += 4 - (col % 4),
                            _ => break,
                        }
                        i += 1;
                    }
                    return (col - cols, &line[i..]);
                }
            }
            _ => break,
        }
    }
    (0, &line[i..])
}

/// [`split_cols`], written into `out` (the spaces materialized, then the rest).
pub(crate) fn strip_cols_into(line: &[u8], cols: usize, out: &mut String) {
    let (spaces, rest) = split_cols(line, cols);
    for _ in 0..spaces {
        out.push(' ');
    }
    out.push_str(std::str::from_utf8(rest).unwrap_or(""));
}

/// Remove up to `cols` columns of leading whitespace — [`split_cols`], owned.
fn strip_cols(line: &[u8], cols: usize) -> String {
    let mut s = String::with_capacity(line.len());
    strip_cols_into(line, cols, &mut s);
    s
}

/// Render a list to HTML. When `opts.block_data` is on, also returns one
/// `ListItemData` per item carrying that item's inner `<li>` HTML (byte-identical
/// to the content between the matching `<li…>`/`</li>` in `out`) plus, when `base`
/// is `Some`, the item's absolute source offset (`base` is `slice`'s own absolute
/// offset, so `base + <item offset within slice>`); returns an empty `Vec` when off
/// (zero extra work / allocation).
fn render_list(slice: &str, ordered: bool, start: u32, base: Option<usize>, opts: &RenderOpts, out: &mut String) -> Vec<Rc<ListItemData>> {
    let bytes = slice.as_bytes();
    let mut items: Vec<Rc<ListItemData>> = Vec::new();
    // Split into sibling items by tracking each item's own content_indent
    // (CMark §5.2). A line opens a new sibling item iff it carries a marker of
    // this list's family, is indented at most `edge + 3` columns, and is
    // indented *less* than the current item's content_indent (otherwise the
    // marker belongs to the current item as nested-list content, parsed
    // recursively when the item body is re-scanned).
    let mut item_starts = Vec::new();
    let mut pos = 0;
    let mut prev_blank_count = 0;
    let mut had_blank_between = false;
    let mut edge = 0usize;
    let mut cur_ci = 0usize;
    while pos < bytes.len() {
        if is_blank_line(bytes, pos) {
            prev_blank_count += 1;
            pos = line_end(bytes, pos);
            continue;
        }
        let line = line_slice(bytes, pos);
        let ind = indent_cols(line);
        if item_starts.is_empty() {
            let m = scan_marker(line, opts.scan_ctx()).expect("list slice starts with a marker");
            edge = m.marker_indent;
            cur_ci = m.content_indent;
            item_starts.push(pos);
            prev_blank_count = 0;
        } else if ind >= cur_ci {
            // Nested content of the current item — skip. Any blanks seen so
            // far belong to this nested content, not *between* sibling items.
            prev_blank_count = 0;
        } else if ind <= edge + 3 {
            if let Some(m) = scan_marker(line, opts.scan_ctx()) {
                if m.ordered == ordered {
                    if prev_blank_count > 0 {
                        had_blank_between = true;
                    }
                    cur_ci = m.content_indent;
                    item_starts.push(pos);
                    prev_blank_count = 0;
                    pos = line_end(bytes, pos);
                    continue;
                }
            }
            // Not a marker for this family — lazy continuation of current item.
        }
        // else: lazy continuation / shallow content of current item.
        pos = line_end(bytes, pos);
    }
    if item_starts.is_empty() {
        return items;
    }
    item_starts.push(bytes.len());

    // Per-list looseness (§5.3): a list is loose if any two items are
    // separated by a blank line, or if any single item *directly* contains
    // two block-level elements separated by a blank line. Blanks buried inside
    // a nested list or fenced code block don't count — they belong to a child
    // block, not to this list's items.
    let mut loose = had_blank_between;
    if !loose {
        for win in item_starts.windows(2) {
            if item_directly_loose(&bytes[win[0]..win[1]], opts.scan_ctx()) {
                loose = true;
                break;
            }
        }
    }

    if ordered {
        out.push_str("<ol");
        out.push_str(opts.dir());
        if start != 1 {
            out.push_str(" start=\"");
            out.push_str(&start.to_string());
            out.push('"');
        }
        out.push('>');
    } else {
        out.push_str("<ul");
        out.push_str(opts.dir());
        out.push('>');
    }
    out.push('\n');
    for win in item_starts.windows(2) {
        let s = win[0];
        let e = win[1];
        let item_slice = &bytes[s..e];
        let inner = render_list_item(item_slice, ordered, loose, opts, out);
        // Structured channel (block_data on): slice the item's inner `<li>` HTML
        // out of `out` (the range render_list_item just recorded) so the keyed
        // renderer gets the exact bytes between this `<li…>` and its `</li>` —
        // no second render, no HTML re-parse.
        if let Some((lo, hi)) = inner {
            // `s` is the item's offset RELATIVE to `slice`; `base` lifts it to the
            // document. A `usize` add at a push that already happens — no rescan.
            items.push(Rc::new(ListItemData {
                html: out[lo..hi].to_string(),
                start: base.map(|b| b + s),
            }));
        }
        out.push('\n');
    }
    out.push_str(if ordered { "</ol>" } else { "</ul>" });
    items
}

/// Does a single list item *directly* contain two block-level elements
/// separated by a blank line? (§5.3 looseness.) We strip the item's marker +
/// content indentation, re-scan the body into top-level blocks, and check
/// whether any blank line sits in the gap between two consecutive blocks.
/// Blanks inside a single block (fenced code, a nested list) are part of that
/// child block and are invisible to this top-level scan, so they don't count.
pub(crate) fn item_directly_loose(item: &[u8], ctx: ScanCtx) -> bool {
    let body = match item_body(item, ctx) {
        Some(b) => b,
        None => return false,
    };
    let mut tmp = body;
    if !tmp.ends_with('\n') {
        tmp.push('\n');
    }
    let sub = scan(&tmp, ctx);
    if sub.len() < 2 {
        return false;
    }
    let tb = tmp.as_bytes();
    for w in sub.windows(2) {
        // Gap between the end of one block and the start of the next.
        let gap_start = w[0].range.end;
        let gap_end = w[1].range.start;
        let mut p = gap_start;
        while p < gap_end {
            if is_blank_line(tb, p) {
                return true;
            }
            p = line_end(tb, p);
        }
    }
    false
}

/// Render one `<li…>…</li>` into `out`. When `opts.block_data` is on, returns the
/// `(start, end)` byte range *within `out`* of this item's inner HTML (the bytes
/// between the `<li…>` opening tag and its `</li>`) so `render_list` can surface it
/// as `ListItemData` without a second render; returns `None` when off.
fn render_list_item(item: &[u8], ordered: bool, loose: bool, opts: &RenderOpts, out: &mut String) -> Option<(usize, usize)> {
    let _ = ordered;
    let body = match item_body(item, opts.scan_ctx()) {
        Some(b) => b,
        None => {
            let lo = out.len();
            out.push_str("<li></li>");
            return opts.block_data.then(|| (lo + 4, lo + 4));
        }
    };
    render_item_body(body, loose, opts, out)
}

/// Render the inner of one `<li>…</li>` from an item's ALREADY-de-indented body
/// (the [`item_body`] output) into `out`. Shared by the full path
/// ([`render_list_item`]) and the streaming `ListCache` (`fold_item_body` in
/// `parser.rs`) so both paths emit byte-identical `<li>` HTML — a nested sub-list
/// is just a block the body `scan` finds and `render_block` renders recursively,
/// which is what lets the cache stream nested lists in O(n) without re-implementing
/// the item engine. Returns the inner-HTML `(lo, hi)` byte span within `out` when
/// `block_data` is on (so the caller can surface it as `ListItemData` without a
/// second render); returns `None` when off.
pub(crate) fn render_item_body(mut body: String, loose: bool, opts: &RenderOpts, out: &mut String) -> Option<(usize, usize)> {
    // GFM task list: item body opening with "[ ] " / "[x] ".
    let mut task_state: Option<bool> = None;
    {
        let rb = body.as_bytes();
        if rb.len() >= 4 && rb[0] == b'[' && rb[2] == b']' && rb[3] == b' ' {
            let middle = rb[1];
            if middle == b' ' || middle == b'x' || middle == b'X' {
                task_state = Some(middle == b'x' || middle == b'X');
            }
        }
    }
    if task_state.is_some() {
        body.replace_range(0..4, "");
    }
    // Always scan the body; decide inline-vs-block based on the structure
    // we actually find. A nested list, code block, or quote inside a tight
    // item must still render as a block — only standalone paragraph content
    // in a tight item collapses to inline.
    // Trim + newline-terminate the OWNED body in place (no clone): `body` is
    // moved into `tmp` below and not referenced again.
    let keep = body
        .trim_end_matches(|c: char| matches!(c, '\n' | '\r' | ' ' | '\t'))
        .len();
    body.truncate(keep);
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let tmp = body;
    let sub = crate::scanner::scan(&tmp, opts.scan_ctx());

    // a11y: wrap a task checkbox + its text in a <label> for programmatic
    // association — but ONLY for a tight, non-empty, single-paragraph item,
    // the one shape where a <label> is valid (it must not wrap a nested list /
    // block). The streaming ListCache mirrors this exact condition (it shares
    // this very function), so the two paths stay byte-identical.
    let inline_task =
        !loose && sub.len() == 1 && matches!(sub[0].kind, RawBlockKind::Paragraph);
    let wrap_label = opts.a11y && task_state.is_some() && inline_task;

    out.push_str("<li");
    out.push_str(opts.dir());
    out.push('>');
    // Inner-HTML span starts just past the `<li…>` opening tag (block_data only).
    let inner_lo = out.len();
    if wrap_label {
        out.push_str("<label>");
    }
    if let Some(checked) = task_state {
        // GFM's exact byte-form for a task-list checkbox: attributes in this
        // order, boolean attrs spelled `=""` (GFM spec examples 279/280). Unlike
        // the `target`/`rel` we add to links, there is no product reason to
        // deviate here, so we match the reference byte-for-byte. The a11y
        // `<label>` wrap (above) reuses this same string.
        out.push_str(if checked {
            "<input checked=\"\" disabled=\"\" type=\"checkbox\"> "
        } else {
            "<input disabled=\"\" type=\"checkbox\"> "
        });
    }
    if sub.is_empty() {
        // Empty item.
    } else if !loose && sub.len() == 1 && matches!(sub[0].kind, RawBlockKind::Paragraph) {
        let slice = &tmp[sub[0].range.clone()];
        render_inline_para(trim_trailing_newlines(slice), opts, out);
    } else {
        // cmark's rule, child by child: a tight paragraph is inline text and
        // gets NO separator on either side (so `<li>a\n<ul>…` keeps `a` glued to
        // the `<li>`); every other child is line-oriented and is bracketed by
        // `cr()` — hence a `\n` right after `<li>` unless the first child is a
        // tight paragraph, and a `\n` right before `</li>` unless the last one
        // is. `cr()` (not a per-kind test) is what keeps a raw-HTML child, which
        // already ends in `\n`, from getting a second one (example 174).
        for b in &sub {
            if !loose && matches!(b.kind, RawBlockKind::Paragraph) {
                let slice = &tmp[b.range.clone()];
                render_inline_para(trim_trailing_newlines(slice), opts, out);
            } else {
                cr(out);
                render_block(&tmp, b, None, opts, out);
                cr(out);
            }
        }
    }
    if wrap_label {
        out.push_str("</label>");
    }
    // Inner-HTML span ends just before `</li>`.
    let inner_hi = out.len();
    out.push_str("</li>");
    opts.block_data.then_some((inner_lo, inner_hi))
}

/// Render a GFM table to HTML. When `opts.block_data` is on, also returns the
/// structured `TableData` (headers/rows/aligns with per-cell `{text,html}`) for
/// the opt-in `kind.data` channel; returns `None` when off (zero extra work).
///
/// Serialization follows the GFM reference renderer: every table element ends
/// its own output line, so `<table>`, `<thead>`, each `<tr>`, each cell,
/// `</tr>`, `</thead>`, `<tbody>` and `</tbody>` are each followed by `\n`
/// (cells carry theirs from [`push_table_cell`]). The closing `</table>` does
/// NOT — no block's HTML ends with a newline; the document join supplies the
/// one that follows a top-level block (see WIRE.md §12).
fn render_table(slice: &str, opts: &RenderOpts, out: &mut String) -> Option<TableData> {
    let lines: Vec<&str> = slice.lines().collect();
    if lines.len() < 2 {
        render_paragraph(slice, opts, out);
        return None;
    }
    let header = split_table_cells(lines[0]);
    let aligns = parse_alignments(lines[1]);
    // §GFM: every row is normalized to the header's column count — extra cells
    // are dropped, missing cells are rendered empty.
    let ncol = header.len();
    // Structured channel: only allocated when the flag is on.
    let mut td_headers: Vec<TableCell> = Vec::new();
    let mut td_rows: Vec<Rc<Vec<TableCell>>> = Vec::new();
    out.push_str("<table");
    out.push_str(opts.dir());
    out.push_str(">\n<thead>\n<tr>\n");
    for i in 0..ncol {
        let cell = push_table_cell("th", header.get(i).map(String::as_str).unwrap_or(""), aligns.get(i), opts, out);
        if let Some(c) = cell {
            td_headers.push(c);
        }
    }
    out.push_str("</tr>\n</thead>\n");
    let body: Vec<&&str> = lines[2..].iter().filter(|l| !l.trim().is_empty()).collect();
    if !body.is_empty() {
        out.push_str("<tbody>\n");
        for line in body {
            let cells = split_table_cells(line);
            out.push_str("<tr>\n");
            let mut row: Vec<TableCell> = Vec::new();
            for i in 0..ncol {
                let cell = push_table_cell("td", cells.get(i).map(String::as_str).unwrap_or(""), aligns.get(i), opts, out);
                if let Some(c) = cell {
                    row.push(c);
                }
            }
            if opts.block_data {
                td_rows.push(Rc::new(row));
            }
            out.push_str("</tr>\n");
        }
        out.push_str("</tbody>\n");
    }
    out.push_str("</table>");
    if opts.block_data {
        Some(TableData { headers: Rc::new(td_headers), rows: td_rows, aligns: Rc::new(aligns) })
    } else {
        None
    }
}

/// Render one cell's inline content to HTML (no `<td>`/`<th>` wrapper). Shared
/// by `push_table_cell` and the streaming `TableCache` so the structured
/// `TableCell.html` is byte-identical to the inline content the full path emits.
pub(crate) fn render_cell_inner(content: &str, opts: &RenderOpts) -> String {
    let mut s = String::new();
    render_inline(content, opts, &mut s);
    s
}

/// Emit a `<td>`/`<th>` opening tag (scope/alignment attrs included) — the
/// single source of truth for the cell opener, shared by [`push_table_cell`]
/// and the streaming partial-row cache (which splices cached inner HTML
/// between the opener and closer).
pub(crate) fn push_table_cell_open(
    tag: &str,
    align: Option<&Option<&'static str>>,
    opts: &RenderOpts,
    out: &mut String,
) {
    out.push('<');
    out.push_str(tag);
    // a11y: scope a header cell to its column (helps screen readers; deviates
    // from strict GFM byte-output, hence opt-in).
    if opts.a11y && tag == "th" {
        out.push_str(" scope=\"col\"");
    }
    if let Some(a) = align.and_then(|a| a.as_ref()) {
        out.push_str(" style=\"text-align:");
        out.push_str(a);
        out.push('"');
    }
    out.push('>');
}

/// Render a `<td>`/`<th>` cell into `out`, followed by the newline that ends
/// its output line (GFM serializes one row/cell element per line — see
/// [`render_table`]). When `opts.block_data` is on, also returns the structured
/// `TableCell` ({text,html}) for the same cell — which carries the cell's INLINE
/// content only, never the line terminator; returns `None` when off. The emitted
/// HTML is byte-identical either way.
///
/// The terminator lives here, not at the call sites, so the full renderer and
/// the streaming `TableCache` (which renders committed rows, the speculative
/// partial row, and its empty padding cells through this same function) cannot
/// drift apart on newline placement.
pub(crate) fn push_table_cell(
    tag: &str,
    content: &str,
    align: Option<&Option<&'static str>>,
    opts: &RenderOpts,
    out: &mut String,
) -> Option<TableCell> {
    push_table_cell_open(tag, align, opts, out);
    // OFF path (default): render straight into `out` — no intermediate String,
    // no memcpy (byte-identical to the pre-refactor behavior, zero new alloc).
    // ON path: capture the inner html once to also build the structured cell.
    let cell = if opts.block_data {
        let inner = render_cell_inner(content, opts);
        out.push_str(&inner);
        Some(TableCell { text: strip_inline_html(&inner), html: inner })
    } else {
        render_inline(content, opts, out);
        None
    };
    out.push_str("</");
    out.push_str(tag);
    out.push_str(">\n");
    cell
}

/// Derive a cell's plaintext from its rendered inline HTML: strip tags, then
/// decode the four entities `escape_html` produces (`&lt; &gt; &amp; &quot;`)
/// plus `&#39;` (harmless if absent), and collapse internal whitespace runs.
///
/// Ordering is load-bearing: tags are stripped FIRST. In escaped cell text a
/// literal `<`/`>` is already `&lt;`/`&gt;`, so decoding first would turn `&lt;`
/// into `<` and make the stripper eat the following text. Pass 1 is quote-aware
/// (a `>` inside a quoted attribute value does not end the tag), so it also
/// strips correctly through raw inline HTML — e.g. `<span title="x > y">` under
/// `unsafeHtml`, where an attribute value can carry a literal `>`.
///
/// Fidelity note: attribute-borne text is not surfaced, so an image-only cell
/// (`![alt](src)`) yields empty plaintext (its `alt` lives in an attribute). A
/// v1 limitation for the sort/filter/CSV channel; the display `html` is intact.
pub(crate) fn strip_inline_html(html: &str) -> String {
    // Pass 1: drop everything between `<` and the matching `>`, treating a `>`
    // inside a quoted attribute value as literal (matters only for raw inline
    // HTML under unsafeHtml; on the safe path every attribute `>` is `&gt;`).
    let mut stripped = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut in_quote: Option<char> = None;
    for c in html.chars() {
        if in_tag {
            match in_quote {
                Some(q) if c == q => in_quote = None,
                Some(_) => {}
                None => match c {
                    '"' | '\'' => in_quote = Some(c),
                    '>' => in_tag = false,
                    _ => {}
                },
            }
        } else if c == '<' {
            in_tag = true;
        } else {
            stripped.push(c);
        }
    }
    // Pass 2: decode the entities and collapse whitespace in one walk.
    let mut out = String::with_capacity(stripped.len());
    let bytes = stripped.as_bytes();
    let mut i = 0;
    let mut pending_ws = false;
    let mut started = false;
    let push_ch = |out: &mut String, ch: char, pending_ws: &mut bool, started: &mut bool| {
        if ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r' {
            if *started {
                *pending_ws = true;
            }
        } else {
            if *pending_ws {
                out.push(' ');
                *pending_ws = false;
            }
            out.push(ch);
            *started = true;
        }
    };
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if stripped[i..].starts_with("&lt;") {
                push_ch(&mut out, '<', &mut pending_ws, &mut started);
                i += 4;
                continue;
            } else if stripped[i..].starts_with("&gt;") {
                push_ch(&mut out, '>', &mut pending_ws, &mut started);
                i += 4;
                continue;
            } else if stripped[i..].starts_with("&quot;") {
                push_ch(&mut out, '"', &mut pending_ws, &mut started);
                i += 6;
                continue;
            } else if stripped[i..].starts_with("&#39;") {
                push_ch(&mut out, '\'', &mut pending_ws, &mut started);
                i += 5;
                continue;
            } else if stripped[i..].starts_with("&amp;") {
                push_ch(&mut out, '&', &mut pending_ws, &mut started);
                i += 5;
                continue;
            }
        }
        // Advance one full char (handles multi-byte UTF-8).
        let ch = stripped[i..].chars().next().unwrap();
        push_ch(&mut out, ch, &mut pending_ws, &mut started);
        i += ch.len_utf8();
    }
    out
}

/// The nine tag names the GFM "Disallowed Raw HTML" extension (tagfilter, GFM
/// spec §6.11) neutralizes when raw HTML is emitted verbatim.
const TAGFILTER_TAGS: &[&str] = &[
    "title", "textarea", "style", "xmp", "iframe", "noembed", "noframes", "script", "plaintext",
];

/// True when the bytes at a `<` open a disallowed tag per the tagfilter: `<` +
/// optional `/` + a disallowed name (ASCII case-insensitive) followed by
/// whitespace, `>`, or `/>`. End-of-chunk also counts as a boundary — a block's
/// content always ends in a line terminator at finalize, so a `<script` cut off
/// by buffer EOF must filter now for streamed/one-shot parity (and so a
/// mid-stream frame never shows the tag live).
fn tagfilter_match(t: &[u8]) -> bool {
    debug_assert_eq!(t.first(), Some(&b'<'));
    let name_start = if t.get(1) == Some(&b'/') { 2 } else { 1 };
    let mut i = name_start;
    while i < t.len() && t[i].is_ascii_alphabetic() {
        i += 1;
    }
    let name = &t[name_start..i];
    if !TAGFILTER_TAGS.iter().any(|d| d.as_bytes().eq_ignore_ascii_case(name)) {
        return false;
    }
    match t.get(i) {
        None => true,
        Some(&b'>') => true,
        Some(&b) if matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r') => true,
        Some(&b'/') => t.get(i + 1) == Some(&b'>'),
        _ => false,
    }
}

/// Copy a raw-HTML chunk to `out`, replacing the leading `<` of every
/// disallowed tag with `&lt;` (everything else verbatim, including the rest of
/// the tag). Scans every `<` in the chunk — also inside comments/CDATA, per the
/// extension. O(chunk).
pub(crate) fn push_tagfiltered(s: &str, out: &mut String) {
    let bytes = s.as_bytes();
    let mut emitted = 0;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'<' && tagfilter_match(&bytes[i..]) {
            out.push_str(&s[emitted..i]);
            out.push_str("&lt;");
            emitted = i + 1;
        }
    }
    out.push_str(&s[emitted..]);
}

fn render_html_block(slice: &str, opts: &RenderOpts, out: &mut String) {
    // A comment-ONLY HTML block has no visible representation: drop it (renders
    // to nothing), so `<!--marker-->` on its own line never surfaces as an
    // escaped code block. Only drop when nothing but whitespace follows the
    // comment's `-->` (otherwise trailing content would be lost — e.g.
    // `<!-- x --> text`). The one exception is bare `unsafe_html` pass-through
    // (no sanitizer engaged), which keeps it verbatim for CommonMark fidelity.
    let ts = slice.trim_start();
    let comment_only = ts.starts_with("<!--")
        && ts.find("-->").is_some_and(|e| ts[e + 3..].trim().is_empty());
    if comment_only && !(opts.unsafe_html && !opts.html_sanitize) {
        return;
    }
    // Block-level sanitize (opt-in `block_html`, sanitizer engaged), CommonMark
    // HTML block types 6 and 7 ONLY — see [`html_block_sanitizes`].
    if detect_html_block_open(slice.as_bytes(), 0)
        .is_some_and(|(_, ty)| html_block_sanitizes(opts.block_html, opts.html_sanitize, ty))
    {
        render_sanitized_html_block(slice, opts, out);
        return;
    }
    // The sanitizer takes precedence over `unsafe_html`: when it's engaged,
    // block-level raw HTML is escaped (unless `block_html` opted the block in
    // just above), so enabling the sanitizer can never let a block `<script>`
    // render raw even if `unsafe_html` is also on.
    if opts.unsafe_html && !opts.html_sanitize {
        let trimmed = slice.trim_end_matches(|c: char| c == '\n' || c == '\r');
        if opts.gfm_tagfilter {
            // Filtering the TRIMMED slice is equivalent to filtering the raw
            // one: trailing newlines and end-of-chunk are both tag boundaries,
            // and the filter never touches newlines.
            push_tagfiltered(trimmed, out);
        } else {
            out.push_str(trimmed);
        }
        // CommonMark output keeps a trailing newline after HTML blocks so
        // adjacent inline content doesn't smash against it.
        out.push('\n');
    } else {
        out.push_str("<pre><code>");
        escape_html(slice, out);
        out.push_str("</code></pre>");
    }
}

/// Does an HTML block of CommonMark `html_type` render through the raw-HTML
/// sanitizer? The single gate, shared by [`render_html_block`] and the streaming
/// [`crate::parser`] cache's arm-time lock + per-append re-validation.
///
/// Types 1–5 are deliberately excluded: type 1 is the raw-text family
/// (`<script>`, `<pre>`, `<style>`, `<textarea>`) — a browser reads everything
/// after such a tag as unparsed text, so a speculative mid-stream close is
/// mXSS-prone — and types 2–5 (comments, PIs, CDATA, declarations) carry no
/// renderable element. They stay escaped/dropped exactly as with the flag off.
pub(crate) fn html_block_sanitizes(block_html: bool, html_sanitize: bool, html_type: u8) -> bool {
    block_html && html_sanitize && matches!(html_type, 6 | 7)
}

/// Nesting cap for the block-HTML open-element stack. The speculative-closer
/// suffix is regenerated on every append, so an unbounded stack would make a
/// `<div><div><div>…` document quadratic (and is a classic sanitizer DoS). Past
/// the cap an opening tag ESCAPES instead of rendering — inert, visible, and the
/// emitted tree stays balanced. Matches [`MAX_RENDER_DEPTH`]'s budget; no real
/// document nests raw HTML this deep.
const MAX_BLOCK_HTML_DEPTH: usize = 100;

/// Fold the SETTLED prefix of `slice[from..]` through the safe raw-HTML
/// sanitizer, maintaining `open` (the open-element stack, outermost first, in
/// source spelling). Returns the offset where folding stopped: either
/// `slice.len()`, or the offset of a `<` that is still STREAMING (an incomplete
/// tag whose later bytes could still complete it) — the caller decides what that
/// trailing partial means (suppressed on the open tail, escaped once settled).
///
/// Every complete token routes through [`sanitize_html_token`], the one decision
/// path inline raw HTML uses; text between tokens is escaped; comments (and PIs
/// / CDATA / declarations) are dropped. A `<` that can never become a tag
/// (`a < b`) is settled literal text and folds as `&lt;`.
///
/// INCREMENTAL BY CONSTRUCTION, which is what lets `HtmlBlockCache` fold at
/// token boundaries: a consumed token can never be un-consumed (the tokenizer
/// stops at the first terminator), a text byte's escape is context-free, and
/// `inline_html_streams_to_eof` returning `false` means the `<` is broken
/// forever. So folding `[a,b)` then `[b,c)` produces exactly what folding
/// `[a,c)` in one call produces.
pub(crate) fn fold_block_html(
    slice: &str,
    from: usize,
    policy: HtmlPolicy<'_>,
    open: &mut Vec<String>,
    out: &mut String,
) -> usize {
    // Deterministic complexity probe (feature `perf_counters` only): a cache
    // that re-sanitized the whole growing block per append goes quadratic HERE.
    #[cfg(feature = "perf_counters")]
    crate::perf::add_render(slice.len() - from);
    let bytes = slice.as_bytes();
    let mut pos = from;
    while pos < bytes.len() {
        let Some(rel) = bytes[pos..].iter().position(|&b| b == b'<') else {
            escape_html(slice.get(pos..).unwrap_or(""), out);
            return bytes.len();
        };
        let lt = pos + rel;
        escape_html(slice.get(pos..lt).unwrap_or(""), out);
        let Some(consumed) = match_inline_html(bytes, lt) else {
            if inline_html_streams_to_eof(bytes, lt) {
                return lt; // still streaming — the caller owns the partial
            }
            out.push_str("&lt;"); // broken forever (`a < b`): literal text
            pos = lt + 1;
            continue;
        };
        let mark = out.len();
        match sanitize_html_token(&bytes[lt..lt + consumed], policy, out) {
            SanitizedTag::Rendered { name, close: true, .. } => {
                close_open_element(name, mark, open, out)
            }
            SanitizedTag::Rendered { name, close: false, void } if !void => {
                if open.len() >= MAX_BLOCK_HTML_DEPTH {
                    out.truncate(mark);
                    escape_html(slice.get(lt..lt + consumed).unwrap_or(""), out);
                } else {
                    open.push(name.to_string());
                }
            }
            _ => {}
        }
        pos = lt + consumed;
    }
    bytes.len()
}

/// Settle an author's `</name>` — already emitted at `mark` — against the open
/// stack. A matching element deeper in the stack implicitly closes everything
/// above it, and those closers are spliced in BEFORE the author's own close tag
/// so the emitted tree stays balanced (`<b><i></b>` → `<b><i></i></b>`). A close
/// tag matching nothing open is a stray: its markup is removed, since emitting
/// it could only unbalance the tree. `mark` sits at the very end of `out` bar
/// the close tag itself, so the splice shifts a handful of bytes.
fn close_open_element(name: &str, mark: usize, open: &mut Vec<String>, out: &mut String) {
    let Some(idx) = open.iter().rposition(|t| t.eq_ignore_ascii_case(name)) else {
        out.truncate(mark);
        return;
    };
    let mut implicit = String::new();
    for tag in open[idx + 1..].iter().rev() {
        implicit.push_str("</");
        implicit.push_str(tag);
        implicit.push('>');
    }
    if !implicit.is_empty() {
        out.insert_str(mark, &implicit);
    }
    open.truncate(idx);
}

/// Append one speculative closer per still-open element, innermost first, so the
/// HTML a consumer has seen SO FAR is a complete tree at every stream prefix.
/// Regenerated on each append (the stack is bounded by [`MAX_BLOCK_HTML_DEPTH`]);
/// when the real `</tag>` finally arrives the closer simply stops being
/// speculative and the emitted bytes are unchanged.
pub(crate) fn push_html_closers(open: &[String], out: &mut String) {
    for tag in open.iter().rev() {
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
    }
}

/// Render a type-6/7 raw-HTML block through the safe sanitizer (`block_html` +
/// `html_sanitize`). Shape mirrors the `unsafe_html` pass-through arm — body
/// with trailing newlines trimmed, then the single `\n` CommonMark puts after an
/// HTML block — with the speculative closers between them. For content that is
/// entirely allowlisted and attribute-free the two arms agree byte for byte.
///
/// The trim runs on the assembled BODY, not the source slice: a suppressed
/// trailing partial (`<div>\n<spa`) must not take the newline before it along.
fn render_sanitized_html_block(slice: &str, opts: &RenderOpts, out: &mut String) {
    let body_start = out.len();
    let mut open: Vec<String> = Vec::new();
    let stop = fold_block_html(slice, 0, opts.html_policy(), &mut open, out);
    if stop < slice.len() && !opts.open_tail {
        // A half-arrived tag that is SETTLED (finalize, or a block a blank line
        // already closed) can never complete, so it renders as escaped literal
        // text. On the genuine open tail it stays suppressed instead — the same
        // pending-invisible contract as a streaming link destination.
        escape_html(slice.get(stop..).unwrap_or(""), out);
    }
    let keep = out[body_start..].trim_end_matches(['\n', '\r']).len();
    out.truncate(body_start + keep);
    push_html_closers(&open, out);
    out.push('\n');
}

/// Render an opt-in component tag (`<Tag …>…</Tag>`) as `<tag …>inner</tag>`,
/// with the inner content parsed as markdown. The tag is allowlisted and its
/// attributes are sanitized (event handlers dropped, dangerous URL schemes
/// neutralized), so this is safe to emit even with `unsafe_html` off. The body
/// is scanned + rendered like a blockquote/alert; nested allowlisted tags are
/// recognized via `opts.scan_ctx()`.
///
/// Uses the PERMISSIVE `sanitize_attrs` (same as the `BlockKind::Component` prop
/// bag it must stay byte-consistent with), not the raw-HTML tier: a component's
/// attributes are consumer-mediated props, so `RAW_HTML_DROPPED_ATTRS` (`id`,
/// `slot`, `form*`, …) does not apply.
fn render_component(slice: &str, tag: &str, terminated: bool, opts: &RenderOpts, out: &mut String) {
    let open = slice.trim_start_matches([' ', '\t']);
    let attrs = sanitize_attrs(open, &opts.allow_schemes);
    let (open_end, inner_end) = component_inner_range(slice, tag, terminated);
    let inner = slice.get(open_end..inner_end).unwrap_or("");

    out.push('<');
    out.push_str(tag);
    for (k, v) in &attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        escape_attr(v, out);
        out.push('"');
    }
    out.push('>');

    let sub: Vec<_> = scan(inner, opts.scan_ctx())
        .into_iter()
        .filter(|b| !matches!(b.kind, RawBlockKind::LinkRefDefinition))
        .collect();
    if !sub.is_empty() {
        out.push('\n');
    }
    for b in &sub {
        render_block(inner, b, None, opts, out);
        // `cr()`, not an unconditional push: a raw-HTML-block child already ends
        // with `\n` and a second one is a stray blank line (the example-174 bug,
        // in a component body). The leading newline above keeps its
        // `!sub.is_empty()` guard — unlike a blockquote, an empty component body
        // stays `<Tag></Tag>`.
        cr(out);
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

pub(crate) fn split_table_cells(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('|').unwrap_or(trimmed);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut escape = false;
    for c in inner.chars() {
        if escape {
            current.push(c);
            escape = false;
        } else if c == '\\' {
            escape = true;
        } else if c == '|' {
            cells.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(c);
        }
    }
    cells.push(current.trim().to_string());
    cells
}

pub(crate) fn parse_alignments(line: &str) -> Vec<Option<&'static str>> {
    split_table_cells(line)
        .into_iter()
        .map(|cell| {
            let left = cell.starts_with(':');
            let right = cell.ends_with(':');
            match (left, right) {
                (true, true) => Some("center"),
                (true, false) => Some("left"),
                (false, true) => Some("right"),
                _ => None,
            }
        })
        .collect()
}

pub(crate) fn trim_trailing_newlines(s: &str) -> &str {
    s.trim_end_matches(|c: char| c == '\n' || c == '\r')
}

#[allow(dead_code)]
fn _keep_imports(bytes: &[u8], pos: usize) {
    let _ = is_blank_line(bytes, pos);
}

#[cfg(test)]
mod strip_tests {
    use super::strip_inline_html;

    #[test]
    fn strips_tags_and_decodes_entities() {
        assert_eq!(strip_inline_html("<strong>A</strong>"), "A");
        assert_eq!(
            strip_inline_html("<a href=\"z\" target=\"_blank\" rel=\"noopener\">y</a>"),
            "y"
        );
        // escaped `<`/`>` in cell text decode back to literals (not eaten).
        assert_eq!(strip_inline_html("a &lt;b&gt; c"), "a <b> c");
        assert_eq!(strip_inline_html("x &amp;&amp; y"), "x && y");
    }

    #[test]
    fn quote_aware_attribute_with_literal_gt() {
        // unsafeHtml: a raw `>` inside a quoted attribute value must NOT end the
        // tag early (regression: produced `y">hi` instead of `hi`).
        assert_eq!(strip_inline_html("<span title=\"x > y\">hi</span>"), "hi");
        assert_eq!(strip_inline_html("<img alt='a > b' src=\"s\">"), "");
        // the other quote char inside a value is literal, not a toggle.
        assert_eq!(strip_inline_html("<span title=\"it's > ok\">z</span>"), "z");
    }
}

#[cfg(test)]
mod strip_pass_bench {
    //! Isolated cost of the cell plaintext pass (`strip_inline_html`) — the one
    //! piece of work `kind.data` adds on top of html the parser already produces.
    //! `strip_inline_html` is `pub(crate)`, unreachable from `examples/`, so this
    //! lives here where it has crate access. `#[ignore]`d so it never runs on the
    //! CI floor (`cargo test --release`) and does not perturb the test count.
    //!
    //!   cargo test --release strip_pass_cost -- --ignored --nocapture
    //!
    //! It harvests the EXACT rendered cell HTML the production ON-path builds
    //! (via a real `StreamParser` with `block_data` on), then times only the
    //! strip pass over that corpus, against the cost of rendering the cell inline
    //! HTML itself — so strip's *share* of per-cell ON-path work is honest.

    use crate::blocks::BlockKind;
    use crate::parser::StreamParser;
    use std::time::Instant;

    /// Parse a markup-heavy table and return every cell's rendered html (the real
    /// strip-pass input). `cell.html` here is byte-identical to the inline content
    /// inside each `<td>`/`<th>`.
    fn harvest_cells(rows: usize) -> Vec<String> {
        let mut doc = String::from("| **Col A** | *Col B* | `Col C` |\n| --- | --- | --- |\n");
        for i in 0..rows {
            doc.push_str(&format!(
                "| **Item {i}** with *em* and `code` | a [link](https://example.com/{i}) here | plain text {i} |\n"
            ));
        }
        let mut p = StreamParser::new().with_gfm_autolinks(true).with_block_data(true);
        p.append(&doc);
        p.finalize();
        let mut cells = Vec::new();
        for b in p.all_blocks() {
            if let BlockKind::Table(Some(td)) = &b.kind {
                for h in td.headers.iter() {
                    cells.push(h.html.clone());
                }
                for r in &td.rows {
                    for c in r.iter() {
                        cells.push(c.html.clone());
                    }
                }
            }
        }
        cells
    }

    #[test]
    #[ignore]
    fn strip_pass_cost() {
        let cells = harvest_cells(4_000); // ~12k cells, markup-heavy
        let total_html_bytes: usize = cells.iter().map(|s| s.len()).sum();
        let n = cells.len();

        // Warm up.
        let mut sink = 0usize;
        for c in &cells {
            sink += super::strip_inline_html(c).len();
        }
        std::hint::black_box(sink);

        let reps = 50;
        let t0 = Instant::now();
        let mut acc = 0usize;
        for _ in 0..reps {
            for c in &cells {
                acc += super::strip_inline_html(c).len();
            }
        }
        std::hint::black_box(acc);
        let elapsed = t0.elapsed();

        let per_cell_ns = elapsed.as_nanos() as f64 / (reps as f64 * n as f64);
        let cells_per_pass = n as f64;
        let throughput_mbps =
            (total_html_bytes as f64 * reps as f64) / 1e6 / elapsed.as_secs_f64();
        println!(
            "\nstrip_inline_html: {n} markup cells, {total_html_bytes} html bytes, {reps} reps\n  total {:.2} ms  =>  {per_cell_ns:.1} ns/cell  ({:.1} M cells/s)  {throughput_mbps:.1} MB/s of html scanned",
            elapsed.as_secs_f64() * 1e3,
            (cells_per_pass * reps as f64) / 1e6 / elapsed.as_secs_f64(),
        );
        assert!(per_cell_ns > 0.0);
    }
}

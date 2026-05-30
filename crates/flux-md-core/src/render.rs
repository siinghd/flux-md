//! Block-level renderer. Takes a `RawBlock` (from `scanner`) plus its source
//! slice and emits sanitized HTML for it. Inline content is delegated to
//! `inline::render_inline`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::blocks::{AlertKind, BlockKind, HeadingData, MathBlockData, TableCell, TableData};
use crate::inline::render_inline;
use crate::scanner::{
    component_inner_range, indent_cols, is_blank_line, line_end, line_slice, scan, scan_marker,
    RawBlock, RawBlockKind, ScanCtx,
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
    /// GFM extended autolinks: recognize bare `www.`, `http(s)://`, `ftp://`
    /// URLs (and turn them into links) in ordinary text. Off by default so
    /// strict CommonMark output is unchanged.
    pub gfm_autolinks: bool,
    /// GitHub alerts: a `> [!NOTE]` blockquote becomes a styled callout
    /// (`<div class="markdown-alert …">`). Off by default so strict CommonMark
    /// output (a plain `<blockquote>`) is unchanged.
    pub gfm_alerts: bool,
    /// Math: recognize `$…$` / `\(…\)` inline and `$$…$$` / `\[…\]` display
    /// math. Off by default (so `$` in prose stays literal). The block-level
    /// half is also gated in the scanner via [`ScanCtx::math`].
    pub gfm_math: bool,
    /// Emit `dir="auto"` on block-level text elements for per-block bidi. Off by
    /// default (strict-CommonMark output is unchanged).
    pub dir_auto: bool,
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
    /// appears in `footnotes` renders as a superscript link.
    pub gfm_footnotes: bool,
    /// label → footnote number, assigned in first-reference order across the
    /// whole document (stable across reparses via the committed map). Empty
    /// unless `gfm_footnotes` is on.
    pub footnotes: HashMap<String, usize>,
    /// Per-label occurrence counter, mutated as `[^label]` references render
    /// (in document order) so the Kth reference to a label gets a unique id
    /// (`fnref-N`, `fnref-N-2`, …). Interior-mutable because emitting unique
    /// ids is inherently sequential state; the alternative (threading `&mut`
    /// through every render_inline caller) is far more invasive. Seeded from
    /// the committed occurrence counts so ids stay unique across the stream.
    pub footnote_occ: RefCell<HashMap<String, usize>>,
    /// Opt-in component-tag allowlist, carried so recursive sub-block scans
    /// (inside lists/quotes/components) recognize nested component tags too.
    pub component_tags: Vec<Box<str>>,
}

impl RenderOpts {
    pub fn lookup(&self, label: &str) -> Option<&LinkRef> {
        let key = normalize_label(label);
        // Committed (permanent) definitions win over tail ones — first-wins.
        self.committed_refs.get(&key).or_else(|| self.tail_refs.get(&key))
    }

    /// Scanner feature flags derived from these render options, so sub-blocks
    /// (inside lists, block quotes, alerts, components) scan with the same
    /// feature set as the top level.
    pub(crate) fn scan_ctx(&self) -> ScanCtx<'_> {
        ScanCtx { math: self.gfm_math, component_tags: &self.component_tags }
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

pub fn classify(raw: &RawBlockKind, slice: &str, gfm_alerts: bool) -> BlockKind {
    if gfm_alerts {
        if let RawBlockKind::Blockquote = raw {
            if let Some(kind) = alert_head(&blockquote_inner(slice)) {
                return BlockKind::Alert { kind };
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
            let lang = info.split_whitespace().next().unwrap_or("");
            match lang {
                "math" | "latex" | "tex" => BlockKind::MathBlock(None),
                "mermaid" => BlockKind::Mermaid,
                "" => BlockKind::CodeBlock { lang: None, code: None },
                other => BlockKind::CodeBlock { lang: Some(other.to_string()), code: None },
            }
        }
        RawBlockKind::IndentedCode => BlockKind::CodeBlock { lang: None, code: None },
        RawBlockKind::MathFence { .. } => BlockKind::MathBlock(None),
        RawBlockKind::List { ordered, .. } => BlockKind::List { ordered: *ordered, start: None },
        RawBlockKind::Blockquote => BlockKind::Blockquote,
        RawBlockKind::Table => BlockKind::Table(None),
        RawBlockKind::HorizontalRule => BlockKind::Rule,
        RawBlockKind::HtmlBlock { .. } => BlockKind::Html,
        RawBlockKind::ComponentBlock { tag, .. } => {
            // Attributes are parsed + sanitized from the open tag for the JS layer
            // (`components[tag]` receives them); the same sanitizer feeds the HTML
            // wrapper in render_component.
            let open = slice.trim_start_matches([' ', '\t']);
            BlockKind::Component { tag: tag.clone(), attrs: sanitize_attrs(open) }
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
    /// Ordered/unordered list — folds the start number onto
    /// `BlockKind::List { start: Some(_), .. }` (the classified `ordered` is
    /// preserved).
    List(u32),
}

/// Render one block to HTML. Returns `Some(Enrichment)` only for a top-level
/// block whose kind has an opt-in `kind.data` payload (Table, Heading) when
/// `opts.block_data` is on; `None` for every other kind and whenever the flag is
/// off. Nested (recursive) call sites ignore the return — only blocks that
/// appear at the document top level get a `kind.data`.
pub fn render_block(source: &str, raw: &RawBlock, opts: &RenderOpts, out: &mut String) -> Option<Enrichment> {
    let slice = &source[raw.range.clone()];
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
                        return Some(Enrichment::MathBlock(MathBlockData { latex: code }))
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
                .map(|latex| Enrichment::MathBlock(MathBlockData { latex }))
        }
        RawBlockKind::Blockquote => render_blockquote(slice, opts, out),
        RawBlockKind::List { ordered, start } => {
            render_list(slice, *ordered, *start, opts, out);
            if opts.block_data {
                return Some(Enrichment::List(*start));
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
    let mut tmp = String::with_capacity(content.len());
    render_inline(content, opts, &mut tmp);
    let trimmed = tmp.trim_end_matches(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r');
    out.push_str(trimmed);
    if opts.block_data {
        Some(trimmed.to_string())
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
    let mut tmp = String::with_capacity(trimmed.len());
    render_inline(trimmed, opts, &mut tmp);
    // CommonMark: trailing whitespace at end of final line is stripped.
    let final_text = tmp.trim_end_matches(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r');
    out.push_str(final_text);
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
            content_end = if last_line_start == content_start { content_start } else { last_line_start - 1 };
            if content_end < content_start {
                content_end = content_start;
            }
        }
    }
    let content = if content_end > content_start {
        std::str::from_utf8(&bytes[content_start..content_end]).unwrap_or("")
    } else {
        ""
    };

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

/// Inverse of [`escape_html`]: decode the four entities it can emit (`&lt; &gt;
/// &amp; &quot;`) back to their literals. Used by the streaming fence cache to
/// recover the decoded code/LaTeX source from an already-assembled, already-
/// trimmed HTML body — guaranteeing `kind.data.code`/`.latex` is byte-identical to
/// the full path (and to the client's `decodeCodeText`/`decodeMathText`). `&amp;`
/// is decoded LAST so `&amp;lt;` → `&lt;` (not `<`), mirroring the client's
/// `decodeEntities` ordering. The body never contains `&#39;` (`escape_html` does
/// not emit it), so it is not handled.
pub(crate) fn unescape_html_body(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if body[i..].starts_with("&lt;") {
                out.push('<');
                i += 4;
                continue;
            } else if body[i..].starts_with("&gt;") {
                out.push('>');
                i += 4;
                continue;
            } else if body[i..].starts_with("&quot;") {
                out.push('"');
                i += 6;
                continue;
            } else if body[i..].starts_with("&amp;") {
                out.push('&');
                i += 5;
                continue;
            }
        }
        let ch = body[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
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
    let trimmed = content.trim_end_matches(|c: char| c == '\n' || c == '\r' || c == ' ' || c == '\t');
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
            s = stripped.strip_prefix(' ').unwrap_or(stripped);
            inner.push_str(s);
            inner.push('\n');
        } else {
            // A line without a `>` is a lazy paragraph continuation (the
            // scanner only kept valid ones). Glue it to the previous line with
            // a space so the re-scan can't reinterpret it as a new block (a
            // setext underline, a list marker, …). A soft break renders as a
            // space anyway, so this is faithful.
            if inner.ends_with('\n') {
                inner.pop();
            }
            inner.push(' ');
            inner.push_str(s.trim_start());
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

fn render_blockquote(slice: &str, opts: &RenderOpts, out: &mut String) {
    let inner = blockquote_inner(slice);
    if opts.gfm_alerts {
        if let Some(kind) = alert_head(&inner) {
            render_alert(&inner, kind, opts, out);
            return;
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
    if !sub.is_empty() {
        out.push('\n');
    }
    for b in &sub {
        render_block(&inner, b, opts, out);
        out.push('\n');
    }
    out.push_str("</blockquote>");
}

/// Render a GitHub alert as `<div class="markdown-alert markdown-alert-TYPE">`
/// (GitHub-compatible class names so existing markdown CSS styles it). The body
/// is everything after the `[!TYPE]` title line, scanned as sub-blocks exactly
/// like a blockquote.
fn render_alert(inner: &str, kind: AlertKind, opts: &RenderOpts, out: &mut String) {
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
    for b in &sub {
        render_block(body, b, opts, out);
        out.push('\n');
    }
    out.push_str("</div>");
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

/// Count every reference occurrence per label (for backref generation).
pub(crate) fn count_footnote_refs(text: &str, counts: &mut HashMap<String, usize>) {
    for_each_footnote_ref(text, |label| {
        *counts.entry(label.to_string()).or_insert(0) += 1;
    });
}

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
    let mut ordered: Vec<(&String, &usize)> = nums.iter().collect();
    ordered.sort_by_key(|(_, n)| **n);
    let mut out = String::from("<section class=\"footnotes\" role=\"doc-endnotes\">\n<ol");
    out.push_str(dir);
    out.push_str(">\n");
    for (label, num) in ordered {
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
pub(crate) fn item_body(item: &[u8]) -> Option<String> {
    let first_line_end =
        item.iter().position(|&b| b == b'\n').map(|i| i + 1).unwrap_or(item.len());
    let first_line = &item[..first_line_end];
    let m = scan_marker(first_line)?;
    let ci = m.content_indent;
    let mut body = String::with_capacity(item.len());
    body.push_str(std::str::from_utf8(&first_line[m.content_byte..]).unwrap_or(""));
    let mut pos = first_line_end;
    while pos < item.len() {
        let line = line_slice(item, pos);
        let is_blank = line.iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
        // A non-blank line indented less than the content column, immediately
        // after paragraph text, is a lazy continuation: glue it on with a space
        // so the re-scan can't read it as a new block (e.g. a nested list).
        if !is_blank && indent_cols(line) < ci && !body.ends_with("\n\n") && !body.is_empty() {
            if body.ends_with('\n') {
                body.pop();
            }
            body.push(' ');
            body.push_str(std::str::from_utf8(line).unwrap_or("").trim_start());
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

/// Remove up to `cols` columns of leading whitespace; tabs expand to width-4
/// stops, and a tab that crosses the boundary re-emits its overflow as spaces.
fn strip_cols(line: &[u8], cols: usize) -> String {
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
                if col + w <= cols {
                    col += w;
                    i += 1;
                } else {
                    let overflow = (col + w) - cols;
                    i += 1;
                    let mut s = " ".repeat(overflow);
                    s.push_str(std::str::from_utf8(&line[i..]).unwrap_or(""));
                    return s;
                }
            }
            _ => break,
        }
    }
    std::str::from_utf8(&line[i..]).unwrap_or("").to_string()
}

fn render_list(slice: &str, ordered: bool, start: u32, opts: &RenderOpts, out: &mut String) {
    let bytes = slice.as_bytes();
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
            let m = scan_marker(line).expect("list slice starts with a marker");
            edge = m.marker_indent;
            cur_ci = m.content_indent;
            item_starts.push(pos);
            prev_blank_count = 0;
        } else if ind >= cur_ci {
            // Nested content of the current item — skip. Any blanks seen so
            // far belong to this nested content, not *between* sibling items.
            prev_blank_count = 0;
        } else if ind <= edge + 3 {
            if let Some(m) = scan_marker(line) {
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
        return;
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
        render_list_item(item_slice, ordered, loose, opts, out);
        out.push('\n');
    }
    out.push_str(if ordered { "</ol>" } else { "</ul>" });
}

/// Does a single list item *directly* contain two block-level elements
/// separated by a blank line? (§5.3 looseness.) We strip the item's marker +
/// content indentation, re-scan the body into top-level blocks, and check
/// whether any blank line sits in the gap between two consecutive blocks.
/// Blanks inside a single block (fenced code, a nested list) are part of that
/// child block and are invisible to this top-level scan, so they don't count.
fn item_directly_loose(item: &[u8], ctx: ScanCtx) -> bool {
    let body = match item_body(item) {
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

fn render_list_item(item: &[u8], ordered: bool, loose: bool, opts: &RenderOpts, out: &mut String) {
    let _ = ordered;
    let mut body = match item_body(item) {
        Some(b) => b,
        None => {
            out.push_str("<li></li>");
            return;
        }
    };

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
    let body_trimmed = body.trim_end_matches(|c: char| matches!(c, '\n' | '\r' | ' ' | '\t'));

    // Always scan the body; decide inline-vs-block based on the structure
    // we actually find. A nested list, code block, or quote inside a tight
    // item must still render as a block — only standalone paragraph content
    // in a tight item collapses to inline.
    let mut tmp = body_trimmed.to_string();
    if !tmp.ends_with('\n') {
        tmp.push('\n');
    }
    let sub = crate::scanner::scan(&tmp, opts.scan_ctx());

    // a11y: wrap a task checkbox + its text in a <label> for programmatic
    // association — but ONLY for a tight, non-empty, single-paragraph item,
    // the one shape where a <label> is valid (it must not wrap a nested list /
    // block). The streaming ListCache mirrors this exact condition, so the two
    // paths stay byte-identical (see render_item_line in parser.rs).
    let inline_task =
        !loose && sub.len() == 1 && matches!(sub[0].kind, RawBlockKind::Paragraph);
    let wrap_label = opts.a11y && task_state.is_some() && inline_task;

    out.push_str("<li");
    out.push_str(opts.dir());
    out.push('>');
    if wrap_label {
        out.push_str("<label>");
    }
    if let Some(checked) = task_state {
        out.push_str(if checked {
            "<input type=\"checkbox\" checked disabled> "
        } else {
            "<input type=\"checkbox\" disabled> "
        });
    }
    if sub.is_empty() {
        // Empty item.
    } else if !loose && sub.len() == 1 && matches!(sub[0].kind, RawBlockKind::Paragraph) {
        let slice = &tmp[sub[0].range.clone()];
        render_inline(trim_trailing_newlines(slice), opts, out);
    } else {
        // A leading newline after <li>, and a newline *between* blocks, but
        // none trailing — a trailing newline would normalize to a stray space
        // before </li> when the last block is tight inline text.
        for b in &sub {
            out.push('\n');
            if !loose && matches!(b.kind, RawBlockKind::Paragraph) {
                let slice = &tmp[b.range.clone()];
                render_inline(trim_trailing_newlines(slice), opts, out);
            } else {
                render_block(&tmp, b, opts, out);
            }
        }
    }
    if wrap_label {
        out.push_str("</label>");
    }
    out.push_str("</li>");
}

/// Render a GFM table to HTML. When `opts.block_data` is on, also returns the
/// structured `TableData` (headers/rows/aligns with per-cell `{text,html}`) for
/// the opt-in `kind.data` channel; returns `None` when off (zero extra work).
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
    out.push_str("><thead><tr>");
    for i in 0..ncol {
        let cell = push_table_cell("th", header.get(i).map(String::as_str).unwrap_or(""), aligns.get(i), opts, out);
        if let Some(c) = cell {
            td_headers.push(c);
        }
    }
    out.push_str("</tr></thead>");
    let body: Vec<&&str> = lines[2..].iter().filter(|l| !l.trim().is_empty()).collect();
    if !body.is_empty() {
        out.push_str("<tbody>");
        for line in body {
            let cells = split_table_cells(line);
            out.push_str("<tr>");
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
            out.push_str("</tr>");
        }
        out.push_str("</tbody>");
    }
    out.push_str("</table>");
    if opts.block_data {
        Some(TableData { headers: td_headers, rows: td_rows, aligns })
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

/// Render a `<td>`/`<th>` cell into `out`. When `opts.block_data` is on, also
/// returns the structured `TableCell` ({text,html}) for the same cell; returns
/// `None` when off. The emitted HTML is byte-identical either way.
pub(crate) fn push_table_cell(
    tag: &str,
    content: &str,
    align: Option<&Option<&'static str>>,
    opts: &RenderOpts,
    out: &mut String,
) -> Option<TableCell> {
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
    out.push('>');
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

fn render_html_block(slice: &str, opts: &RenderOpts, out: &mut String) {
    if opts.unsafe_html {
        let trimmed = slice.trim_end_matches(|c: char| c == '\n' || c == '\r');
        out.push_str(trimmed);
        // CommonMark output keeps a trailing newline after HTML blocks so
        // adjacent inline content doesn't smash against it.
        out.push('\n');
    } else {
        out.push_str("<pre><code>");
        escape_html(slice, out);
        out.push_str("</code></pre>");
    }
}

/// Render an opt-in component tag (`<Tag …>…</Tag>`) as `<tag …>inner</tag>`,
/// with the inner content parsed as markdown. The tag is allowlisted and its
/// attributes are sanitized (event handlers dropped, dangerous URL schemes
/// neutralized), so this is safe to emit even with `unsafe_html` off. The body
/// is scanned + rendered like a blockquote/alert; nested allowlisted tags are
/// recognized via `opts.scan_ctx()`.
fn render_component(slice: &str, tag: &str, terminated: bool, opts: &RenderOpts, out: &mut String) {
    let open = slice.trim_start_matches([' ', '\t']);
    let attrs = sanitize_attrs(open);
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
        render_block(inner, b, opts, out);
        out.push('\n');
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

fn trim_trailing_newlines(s: &str) -> &str {
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
                for h in &td.headers {
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

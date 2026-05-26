//! Block-level renderer. Takes a `RawBlock` (from `scanner`) plus its source
//! slice and emits sanitized HTML for it. Inline content is delegated to
//! `inline::render_inline`.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::blocks::{AlertKind, BlockKind};
use crate::inline::render_inline;
use crate::scanner::{
    indent_cols, is_blank_line, line_end, line_slice, scan, scan_marker, RawBlock, RawBlockKind,
    ScanCtx,
};
use crate::url::{escape_attr, escape_html};

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
    pub refs: HashMap<String, LinkRef>,
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
}

impl RenderOpts {
    pub fn lookup(&self, label: &str) -> Option<&LinkRef> {
        self.refs.get(&normalize_label(label))
    }

    /// Scanner feature flags derived from these render options, so sub-blocks
    /// (inside lists, block quotes, alerts) scan with the same feature set as
    /// the top level.
    pub(crate) fn scan_ctx(&self) -> ScanCtx {
        ScanCtx { math: self.gfm_math }
    }

    /// The ` dir="auto"` attribute (with a leading space) when bidi is on, else
    /// empty — appended inside block-level opening tags.
    fn dir(&self) -> &'static str {
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
        RawBlockKind::Heading { level } => BlockKind::Heading(*level),
        RawBlockKind::SetextHeading { level } => BlockKind::Heading(*level),
        RawBlockKind::Paragraph => BlockKind::Paragraph,
        RawBlockKind::CodeFence { info, .. } => {
            let lang = info.split_whitespace().next().unwrap_or("");
            match lang {
                "math" | "latex" | "tex" => BlockKind::MathBlock,
                "mermaid" => BlockKind::Mermaid,
                "" => BlockKind::CodeBlock { lang: None },
                other => BlockKind::CodeBlock { lang: Some(other.to_string()) },
            }
        }
        RawBlockKind::IndentedCode => BlockKind::CodeBlock { lang: None },
        RawBlockKind::MathFence { .. } => BlockKind::MathBlock,
        RawBlockKind::List { ordered, .. } => BlockKind::List { ordered: *ordered },
        RawBlockKind::Blockquote => BlockKind::Blockquote,
        RawBlockKind::Table => BlockKind::Table,
        RawBlockKind::HorizontalRule => BlockKind::Rule,
        RawBlockKind::HtmlBlock { .. } => BlockKind::Html,
        RawBlockKind::LinkRefDefinition => BlockKind::Paragraph, // no output anyway
    }
}

pub fn render_block(source: &str, raw: &RawBlock, opts: &RenderOpts, out: &mut String) {
    let slice = &source[raw.range.clone()];
    match &raw.kind {
        RawBlockKind::Heading { level } => render_heading(slice, *level, opts, out),
        RawBlockKind::SetextHeading { level } => render_setext_heading(slice, *level, opts, out),
        RawBlockKind::Paragraph => render_paragraph(slice, opts, out),
        RawBlockKind::CodeFence { info, fence_char, fence_len, terminated } => {
            render_code_fence(slice, info, *fence_char, *fence_len, *terminated, out)
        }
        RawBlockKind::IndentedCode => render_indented_code(slice, out),
        RawBlockKind::MathFence { terminated } => render_math_block(slice, *terminated, out),
        RawBlockKind::Blockquote => render_blockquote(slice, opts, out),
        RawBlockKind::List { ordered, start } => render_list(slice, *ordered, *start, opts, out),
        RawBlockKind::Table => render_table(slice, opts, out),
        RawBlockKind::HorizontalRule => out.push_str("<hr>"),
        RawBlockKind::HtmlBlock { .. } => render_html_block(slice, opts, out),
        RawBlockKind::LinkRefDefinition => { /* no output */ }
    }
}

/// ATX heading inner content — also strip trailing whitespace from final
/// inline rendering (mirrors render_paragraph).
fn render_heading_inner_trimmed(content: &str, opts: &RenderOpts, out: &mut String) {
    let mut tmp = String::with_capacity(content.len());
    render_inline(content, opts, &mut tmp);
    let trimmed = tmp.trim_end_matches(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r');
    out.push_str(trimmed);
}

fn render_heading(slice: &str, level: u8, opts: &RenderOpts, out: &mut String) {
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
    render_heading_inner_trimmed(content, opts, out);
    out.push_str("</h");
    out.push((b'0' + level) as char);
    out.push('>');
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

fn render_code_fence(
    slice: &str,
    info: &str,
    _fence_char: u8,
    _fence_len: usize,
    _terminated: bool,
    out: &mut String,
) {
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
fn render_math_block(slice: &str, terminated: bool, out: &mut String) {
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
}

fn render_setext_heading(slice: &str, level: u8, opts: &RenderOpts, out: &mut String) {
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
    render_heading_inner_trimmed(content, opts, out);
    out.push_str("</h");
    out.push((b'0' + level) as char);
    out.push('>');
}

fn render_indented_code(slice: &str, out: &mut String) {
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
    out.push_str("\" role=\"note\">\n<p class=\"markdown-alert-title\">");
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
) -> String {
    if nums.is_empty() {
        return String::new();
    }
    let mut ordered: Vec<(&String, &usize)> = nums.iter().collect();
    ordered.sort_by_key(|(_, n)| **n);
    let mut out = String::from("<section class=\"footnotes\" role=\"doc-endnotes\">\n<ol>\n");
    for (label, num) in ordered {
        let n = num.to_string();
        out.push_str("<li id=\"fn-");
        out.push_str(&n);
        out.push_str("\">");
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

    out.push_str("<li");
    out.push_str(opts.dir());
    out.push('>');
    if let Some(checked) = task_state {
        out.push_str(if checked {
            "<input type=\"checkbox\" checked disabled> "
        } else {
            "<input type=\"checkbox\" disabled> "
        });
    }
    // Always scan the body; decide inline-vs-block based on the structure
    // we actually find. A nested list, code block, or quote inside a tight
    // item must still render as a block — only standalone paragraph content
    // in a tight item collapses to inline.
    let mut tmp = body_trimmed.to_string();
    if !tmp.ends_with('\n') {
        tmp.push('\n');
    }
    let sub = crate::scanner::scan(&tmp, opts.scan_ctx());
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
    out.push_str("</li>");
}

fn render_table(slice: &str, opts: &RenderOpts, out: &mut String) {
    let lines: Vec<&str> = slice.lines().collect();
    if lines.len() < 2 {
        render_paragraph(slice, opts, out);
        return;
    }
    let header = split_table_cells(lines[0]);
    let aligns = parse_alignments(lines[1]);
    // §GFM: every row is normalized to the header's column count — extra cells
    // are dropped, missing cells are rendered empty.
    let ncol = header.len();
    out.push_str("<table");
    out.push_str(opts.dir());
    out.push_str("><thead><tr>");
    for i in 0..ncol {
        push_table_cell("th", header.get(i).map(String::as_str).unwrap_or(""), aligns.get(i), opts, out);
    }
    out.push_str("</tr></thead>");
    let body: Vec<&&str> = lines[2..].iter().filter(|l| !l.trim().is_empty()).collect();
    if !body.is_empty() {
        out.push_str("<tbody>");
        for line in body {
            let cells = split_table_cells(line);
            out.push_str("<tr>");
            for i in 0..ncol {
                push_table_cell("td", cells.get(i).map(String::as_str).unwrap_or(""), aligns.get(i), opts, out);
            }
            out.push_str("</tr>");
        }
        out.push_str("</tbody>");
    }
    out.push_str("</table>");
}

fn push_table_cell(
    tag: &str,
    content: &str,
    align: Option<&Option<&'static str>>,
    opts: &RenderOpts,
    out: &mut String,
) {
    out.push('<');
    out.push_str(tag);
    if let Some(a) = align.and_then(|a| a.as_ref()) {
        out.push_str(" style=\"text-align:");
        out.push_str(a);
        out.push('"');
    }
    out.push('>');
    render_inline(content, opts, out);
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
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

fn split_table_cells(line: &str) -> Vec<String> {
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

fn parse_alignments(line: &str) -> Vec<Option<&'static str>> {
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

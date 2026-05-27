//! Block-level scanner. Splits a buffer into top-level raw blocks
//! (headings, paragraphs, fenced code, blockquotes, lists, tables, rules)
//! with byte ranges into the source. Inline parsing happens later, per
//! block, in `inline.rs`.
//!
//! Streaming notes:
//! - The scanner is greedy. If a code fence opens but never closes in the
//!   input, the fence's content extends to end-of-input — the caller marks
//!   it as the active (still-open) tail block.
//! - Paragraphs end at a blank line OR at the start of any other recognized
//!   block (lazy continuation simplified — accurate for LLM output).
//! - Lists greedily absorb continuation lines that are blank-padded or
//!   indented past the bullet width.

use core::ops::Range;

/// Feature flags the scanner needs at block-detection time. Threaded through
/// `scan` and the paragraph-interruption checks. All-false by default, so the
/// scanner's behavior is byte-for-byte unchanged unless a flag is enabled.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanCtx<'a> {
    /// Recognize `$$…$$` / `\[…\]` display-math fences as standalone blocks.
    pub math: bool,
    /// Opt-in allowlist of custom component tag names (e.g. `Thinking`). A
    /// `<Tag>…</Tag>` whose name is listed scans as a `ComponentBlock` whose body
    /// is markdown. Empty (the default) means the feature is off.
    pub component_tags: &'a [Box<str>],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RawBlockKind {
    Heading { level: u8 },
    /// A setext-style heading: paragraph text followed by `===` (h1) or
    /// `---` (h2) underline. The slice spans both the text and underline
    /// lines; the renderer strips the underline.
    SetextHeading { level: u8 },
    Paragraph,
    /// Fenced code block. `info` is the language tag (post-fence text).
    /// `fence_char` is `'`'` or `'~'`. `fence_len` is the opening fence width.
    /// `terminated` is true iff a matching closing fence was found.
    CodeFence { info: String, fence_char: u8, fence_len: usize, terminated: bool },
    /// Display-math fence: a block opening with `$$` or `\[` (after ≤3 spaces
    /// of indent), closing at the matching `$$` / `\]`. Blank-line tolerant
    /// like a code fence. `terminated` is true once the closer is seen; an
    /// open one is kept speculative by the streaming parser. Only produced when
    /// `ScanCtx::math` is set.
    MathFence { terminated: bool },
    /// An opt-in custom component container (`<Tag …>…</Tag>`) whose name is in
    /// `ScanCtx::component_tags`. The slice spans the open tag through the
    /// matching close tag; the body is rendered as markdown. `terminated` is true
    /// once the matching `</Tag>` line is seen (an open one stays speculative
    /// while streaming, like a code fence). Self-closing `<Tag/>` is terminated.
    ComponentBlock { tag: String, terminated: bool },
    /// Code block formed by 4+ space indentation, no fence markers.
    IndentedCode,
    /// List as a whole. Items have their own ranges.
    List { ordered: bool, start: u32 },
    Blockquote,
    HorizontalRule,
    /// GFM table. Requires header row + separator row.
    Table,
    /// Raw HTML block. Passed through verbatim by the renderer when the
    /// parser is in unsafe-HTML mode; escaped otherwise. `closed` is true once
    /// the block's end condition is met (closing tag for types 1–5, a blank
    /// line / EOF for 6–7); a type-1–5 block still seeking its closer is left
    /// open so streaming doesn't commit it across an interior blank line.
    HtmlBlock { closed: bool },
    /// A `[label]: url "title"` line. Definitions are extracted into the
    /// parser's ref table; the block itself produces no output.
    LinkRefDefinition,
}

#[derive(Debug, Clone)]
pub struct RawBlock {
    pub kind: RawBlockKind,
    /// Absolute byte range in the scanned slice (caller adds base offset).
    pub range: Range<usize>,
}

pub fn scan(input: &str, ctx: ScanCtx<'_>) -> Vec<RawBlock> {
    let bytes = input.as_bytes();
    let mut pos = 0;
    let mut blocks = Vec::new();

    while pos < bytes.len() {
        // Skip blank lines between blocks.
        if is_blank_line(bytes, pos) {
            pos = line_end(bytes, pos);
            continue;
        }

        let start = pos;

        // Display-math fence (`$$` / `\[`) is unambiguous — no other block
        // opener begins with `$` or `\` — so it's safe to probe first.
        if ctx.math {
            if let Some(b) = scan_math_block(bytes, pos) {
                pos = b.range.end;
                blocks.push(b);
                continue;
            }
        }
        if let Some(b) = scan_fence(bytes, pos) {
            pos = b.range.end;
            blocks.push(b);
            continue;
        }
        // Opt-in component tags take priority over generic HTML-block handling,
        // so a `<Thinking>` in the allowlist becomes a markdown container rather
        // than a raw type-7 HTML block.
        if !ctx.component_tags.is_empty() {
            if let Some(b) = scan_component_block(bytes, pos, ctx.component_tags) {
                pos = b.range.end;
                blocks.push(b);
                continue;
            }
        }
        if let Some(b) = scan_html_block(bytes, pos) {
            pos = b.range.end;
            blocks.push(b);
            continue;
        }
        if let Some(b) = scan_link_ref_def(bytes, pos) {
            pos = b.range.end;
            blocks.push(b);
            continue;
        }
        if let Some(b) = scan_heading(bytes, pos) {
            pos = b.range.end;
            blocks.push(b);
            continue;
        }
        if let Some(b) = scan_indented_code(bytes, pos) {
            pos = b.range.end;
            blocks.push(b);
            continue;
        }
        if let Some(b) = scan_hr(bytes, pos) {
            pos = b.range.end;
            blocks.push(b);
            continue;
        }
        if let Some(b) = scan_blockquote(bytes, pos, ctx) {
            pos = b.range.end;
            blocks.push(b);
            continue;
        }
        if let Some(b) = scan_list(bytes, pos, ctx) {
            pos = b.range.end;
            blocks.push(b);
            continue;
        }
        if let Some(b) = scan_table(bytes, pos, ctx) {
            pos = b.range.end;
            blocks.push(b);
            continue;
        }
        // Default: paragraph.
        let p = scan_paragraph(bytes, pos, ctx);
        pos = p.range.end;
        blocks.push(p);
        if pos == start {
            // Defensive: never make zero progress.
            pos = line_end(bytes, pos);
        }
    }
    blocks
}

// ---------------------------------------------------------------------
// Per-block scanners
// ---------------------------------------------------------------------

fn scan_heading(bytes: &[u8], start: usize) -> Option<RawBlock> {
    let line = line_slice(bytes, start);
    let (indent, line) = strip_indent(line, 3);
    if indent > 3 {
        return None;
    }
    let mut i = 0;
    while i < line.len() && line[i] == b'#' && i < 6 {
        i += 1;
    }
    if i == 0 {
        return None;
    }
    // Must be followed by space, tab, or end of line.
    if i < line.len() && line[i] != b' ' && line[i] != b'\t' && line[i] != b'\n' && line[i] != b'\r' {
        return None;
    }
    let level = i as u8;
    let end = line_end(bytes, start);
    Some(RawBlock { kind: RawBlockKind::Heading { level }, range: start..end })
}

fn scan_fence(bytes: &[u8], start: usize) -> Option<RawBlock> {
    let line = line_slice(bytes, start);
    let (indent, line) = strip_indent(line, 3);
    if indent > 3 {
        return None;
    }
    if line.is_empty() {
        return None;
    }
    let fence_char = line[0];
    if fence_char != b'`' && fence_char != b'~' {
        return None;
    }
    let mut len = 0;
    while len < line.len() && line[len] == fence_char {
        len += 1;
    }
    if len < 3 {
        return None;
    }
    // CommonMark §4.5: for backtick fences, the info string can't contain
    // backticks. (Otherwise `` ``` ` `` looks like a fence but is actually
    // a code span.)
    if fence_char == b'`' {
        let info_end = line.iter().position(|&b| b == b'\n').unwrap_or(line.len());
        if line[len..info_end].iter().any(|&b| b == b'`') {
            return None;
        }
    }
    // Find closing fence (>= len of same char on its own line, possibly
    // indented up to 3 spaces).
    let mut pos = line_end(bytes, start);
    let mut terminated = false;
    while pos < bytes.len() {
        if is_closing_fence(bytes, pos, fence_char, len) {
            pos = line_end(bytes, pos);
            terminated = true;
            break;
        }
        pos = line_end(bytes, pos);
    }
    // Extract info string (text after the fence chars on the opening line).
    let info_start = len;
    let info_end_idx = line.iter().position(|&b| b == b'\n').unwrap_or(line.len());
    let info_slice = &line[info_start..info_end_idx.min(line.len())];
    let info = String::from_utf8_lossy(info_slice).trim().to_string();
    Some(RawBlock {
        kind: RawBlockKind::CodeFence { info, fence_char, fence_len: len, terminated },
        range: start..pos,
    })
}

fn is_closing_fence(bytes: &[u8], start: usize, fence_char: u8, min_len: usize) -> bool {
    let line = line_slice(bytes, start);
    let (indent, line) = strip_indent(line, 3);
    if indent > 3 {
        return false;
    }
    let mut len = 0;
    while len < line.len() && line[len] == fence_char {
        len += 1;
    }
    if len < min_len {
        return false;
    }
    // After the fence chars, only whitespace allowed on the line.
    for &b in &line[len..] {
        if b == b'\n' || b == b'\r' {
            break;
        }
        if b != b' ' && b != b'\t' {
            return false;
        }
    }
    true
}

/// First index of `needle` within `haystack`, byte-wise.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Display-math fence (CTX-gated). A line that begins (after ≤3 spaces) with
/// `$$` or `\[` opens a display-math block. It closes either later on the same
/// line (single-line `$$x$$`, which must be followed by only whitespace) or at
/// a subsequent line containing the matching closer — blank-line tolerant, so
/// multi-line LaTeX (e.g. `\begin{aligned}…`) is kept whole. An unterminated
/// fence runs to end-of-input and is left open (the parser keeps it speculative
/// during streaming, exactly like an open code fence).
fn scan_math_block(bytes: &[u8], start: usize) -> Option<RawBlock> {
    let line = line_slice(bytes, start);
    let (indent, body) = strip_indent(line, 3);
    if indent > 3 {
        return None;
    }
    let (open, close): (&[u8], &[u8]) = if body.starts_with(b"$$") {
        (b"$$", b"$$")
    } else if body.starts_with(b"\\[") {
        (b"\\[", b"\\]")
    } else {
        return None;
    };
    // Byte offset just past the opener within `line`.
    let after_open = (line.len() - body.len()) + open.len();
    let rest_of_line = &line[after_open..];
    if let Some(rel) = find_subslice(rest_of_line, close) {
        // Closer on the opening line: a clean single-line block only if nothing
        // but whitespace follows it. Trailing prose (`$$x$$ and more`) means it
        // isn't a standalone block — let the inline parser render it instead.
        let after_close = &rest_of_line[rel + close.len()..];
        if after_close.iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) {
            let end = line_end(bytes, start);
            return Some(RawBlock { kind: RawBlockKind::MathFence { terminated: true }, range: start..end });
        }
        return None;
    }
    // Multi-line: walk subsequent lines (blanks included) for the closer.
    let mut pos = line_end(bytes, start);
    let mut terminated = false;
    while pos < bytes.len() {
        let l = line_slice(bytes, pos);
        if find_subslice(l, close).is_some() {
            pos = line_end(bytes, pos);
            terminated = true;
            break;
        }
        pos = line_end(bytes, pos);
    }
    Some(RawBlock { kind: RawBlockKind::MathFence { terminated }, range: start..pos })
}

fn scan_hr(bytes: &[u8], start: usize) -> Option<RawBlock> {
    let line = line_slice(bytes, start);
    let (indent, line) = strip_indent(line, 3);
    if indent > 3 {
        return None;
    }
    if line.is_empty() {
        return None;
    }
    let c = line[0];
    if c != b'-' && c != b'*' && c != b'_' {
        return None;
    }
    let mut count = 0;
    for &b in line {
        if b == b'\n' || b == b'\r' {
            break;
        }
        if b == c {
            count += 1;
        } else if b != b' ' && b != b'\t' {
            return None;
        }
    }
    if count < 3 {
        return None;
    }
    let end = line_end(bytes, start);
    Some(RawBlock { kind: RawBlockKind::HorizontalRule, range: start..end })
}

/// The part of a `>`-prefixed line after the marker: strip ≤3 spaces, the `>`,
/// and one optional following space.
fn quote_content(line: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < line.len() && i < 3 && line[i] == b' ' {
        i += 1;
    }
    if i < line.len() && line[i] == b'>' {
        i += 1;
        if i < line.len() && line[i] == b' ' {
            i += 1;
        }
    }
    &line[i..]
}

/// If `content` opens a code fence (≥3 `` ` `` or `~`), return (char, len).
fn fence_marker(content: &[u8]) -> Option<(u8, usize)> {
    let mut i = 0;
    while i < content.len() && i < 3 && content[i] == b' ' {
        i += 1;
    }
    let c = *content.get(i)?;
    if c != b'`' && c != b'~' {
        return None;
    }
    let mut len = 0;
    while i + len < content.len() && content[i + len] == c {
        len += 1;
    }
    (len >= 3).then_some((c, len))
}

/// See through nested container markers (blockquote `>` and list markers) to
/// the innermost content, so we can tell whether a line leaves a paragraph
/// open even when it's wrapped in containers (e.g. `> 1. > text`).
fn strip_container_markers(mut c: &[u8]) -> &[u8] {
    loop {
        let mut i = 0;
        while i < c.len() && i < 3 && c[i] == b' ' {
            i += 1;
        }
        if i < c.len() && c[i] == b'>' {
            c = quote_content(c);
            continue;
        }
        if let Some(m) = scan_marker(c) {
            c = &c[m.content_byte..];
            continue;
        }
        return c;
    }
}

fn scan_blockquote(bytes: &[u8], start: usize, ctx: ScanCtx<'_>) -> Option<RawBlock> {
    if !line_starts_with_marker(bytes, start, b'>') {
        return None;
    }
    let mut pos = start;
    // Lazy continuation only applies to an *open paragraph*; track whether one
    // is open and whether a fenced code block is open (a non-`>` line cannot
    // continue either of those).
    let mut para_open = false;
    let mut fence: Option<(u8, usize)> = None;
    while pos < bytes.len() {
        if !line_starts_with_marker(bytes, pos, b'>') {
            // A non-`>` line extends the quote only as lazy paragraph
            // continuation text: a paragraph must be open, and the line must
            // not itself begin a block (a thematic break, heading, fence, …
            // ends the quote instead).
            if is_blank_line(bytes, pos)
                || fence.is_some()
                || !para_open
                || would_start_other_block(bytes, pos, ctx)
            {
                break;
            }
            pos = line_end(bytes, pos);
            continue;
        }
        let line = line_slice(bytes, pos);
        let content = quote_content(line);
        let content_blank = content.iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
        if let Some((fc, flen)) = fence {
            if is_closing_fence(content, 0, fc, flen) {
                fence = None;
            }
            para_open = false;
        } else if content_blank {
            para_open = false;
        } else if let Some(fm) = fence_marker(content) {
            fence = Some(fm);
            para_open = false;
        } else {
            // See through nested quotes to decide paragraph state. Plain text
            // opens/continues a paragraph; an interrupting block closes it;
            // indented code only continues an already-open paragraph.
            let inner = strip_container_markers(content);
            let inner_blank = inner.iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
            if inner_blank || would_start_other_block(inner, 0, ctx) {
                para_open = false;
            } else if indent_cols(inner) < 4 {
                para_open = true;
            }
            // else: indented code — leave para_open unchanged.
        }
        pos = line_end(bytes, pos);
    }
    Some(RawBlock { kind: RawBlockKind::Blockquote, range: start..pos })
}

fn scan_list(bytes: &[u8], start: usize, ctx: ScanCtx<'_>) -> Option<RawBlock> {
    let first_line = line_slice(bytes, start);
    let first = scan_marker(first_line)?;
    let ordered = first.ordered;
    let delim = first.delim;
    let edge = first.marker_indent;
    // `cur_ci` is the content_indent of the most recent item; it changes from
    // item to item (CMark §5.2), so we track it line by line rather than using
    // a single fixed width for the whole list.
    let mut cur_ci = first.content_indent;
    // §5.2: "A list item can begin with at most one blank line." If the marker
    // is the only thing on its line (empty item) and the next line is blank,
    // the item stays empty and the list ends — later content does not attach.
    let mut cur_empty = first_line
        .get(first.content_byte)
        .map_or(true, |&b| b == b'\n' || b == b'\r');
    let start_num = if ordered {
        let (_, body) = strip_indent(first_line, 3);
        let mut n = 0u32;
        let mut k = 0;
        while k < body.len() && body[k].is_ascii_digit() && k < 9 {
            n = n * 10 + (body[k] - b'0') as u32;
            k += 1;
        }
        n
    } else {
        1
    };
    let mut pos = line_end(bytes, start);
    while pos < bytes.len() {
        if is_blank_line(bytes, pos) {
            let mut peek = pos;
            while peek < bytes.len() && is_blank_line(bytes, peek) {
                peek = line_end(bytes, peek);
            }
            if peek >= bytes.len() {
                break;
            }
            let line = line_slice(bytes, peek);
            let ind = indent_cols(line);
            // Indented far enough to be nested content of the current item —
            // but an empty item cannot acquire content across a blank line.
            if ind >= cur_ci {
                if cur_empty {
                    break;
                }
                pos = line_end(bytes, pos);
                continue;
            }
            // A same-family marker at this list's level resumes the list.
            if let Some(m) = scan_marker(line) {
                if m.ordered == ordered && m.delim == delim && m.marker_indent <= edge + 3 {
                    cur_ci = m.content_indent;
                    cur_empty = line
                        .get(m.content_byte)
                        .map_or(true, |&b| b == b'\n' || b == b'\r');
                    pos = line_end(bytes, pos);
                    continue;
                }
            }
            // After a blank line there is no lazy continuation — the list ends.
            break;
        }
        let line = line_slice(bytes, pos);
        let ind = indent_cols(line);
        // Nested content of the current item.
        if ind >= cur_ci {
            cur_empty = false;
            pos = line_end(bytes, pos);
            continue;
        }
        // A thematic break at this list's own level ends the list.
        if ind <= edge + 3 && scan_hr(bytes, pos).is_some() {
            break;
        }
        // A marker at this list's level: same family → new sibling; different
        // family → a new list begins, so this one ends.
        if let Some(m) = scan_marker(line) {
            if m.marker_indent <= edge + 3 {
                if m.ordered == ordered && m.delim == delim {
                    cur_ci = m.content_indent;
                    cur_empty = line
                        .get(m.content_byte)
                        .map_or(true, |&b| b == b'\n' || b == b'\r');
                    pos = line_end(bytes, pos);
                    continue;
                }
                break;
            }
        }
        // A shallower line that would itself open a new block ends the list;
        // otherwise it is a lazy paragraph continuation of the current item.
        if would_start_other_block(bytes, pos, ctx) {
            break;
        }
        cur_empty = false;
        pos = line_end(bytes, pos);
    }
    Some(RawBlock { kind: RawBlockKind::List { ordered, start: start_num }, range: start..pos })
}

struct ListMarker {
    #[allow(dead_code)]
    width: usize,
    ordered: bool,
    start_num: u32,
    #[allow(dead_code)]
    marker_char: u8,
}

fn detect_list_marker(bytes: &[u8], start: usize) -> Option<ListMarker> {
    let line = line_slice(bytes, start);
    let (indent, body) = strip_indent(line, 3);
    if indent > 3 || body.is_empty() {
        return None;
    }
    // Unordered: -, *, +
    if body[0] == b'-' || body[0] == b'*' || body[0] == b'+' {
        let marker_w = 1usize;
        // Empty item: marker followed by end-of-line.
        if body.len() == 1
            || body[1] == b'\n'
            || body[1] == b'\r'
            || (body[1] == b' ' && body.get(2).is_some_and(|&b| b == b'\n' || b == b'\r'))
        {
            return Some(ListMarker {
                width: indent + marker_w + 1,
                ordered: false,
                start_num: 1,
                marker_char: body[0],
            });
        }
        // Marker followed by 1+ spaces then content.
        if body[1] == b' ' || body[1] == b'\t' {
            // CommonMark §5.2: marker width = leading indent + W + N, where
            // W=1 and N is the number of spaces (capped at 1 if 5+ spaces,
            // which means extra indent is "within the item").
            let mut spaces = 0;
            let mut k = 1;
            while k < body.len() && body[k] == b' ' {
                spaces += 1;
                k += 1;
            }
            let n = if spaces >= 5 { 1 } else { spaces };
            return Some(ListMarker {
                width: indent + marker_w + n,
                ordered: false,
                start_num: 1,
                marker_char: body[0],
            });
        }
        return None;
    }
    // Ordered: digits then . or ) then space or end-of-line.
    let mut i = 0;
    while i < body.len() && body[i].is_ascii_digit() && i < 9 {
        i += 1;
    }
    if i == 0 || i >= body.len() {
        return None;
    }
    if body[i] != b'.' && body[i] != b')' {
        return None;
    }
    let marker_w = i + 1;
    let num: u32 = std::str::from_utf8(&body[..i]).ok()?.parse().ok()?;
    let after_marker = i + 1;
    if after_marker >= body.len()
        || body[after_marker] == b'\n'
        || body[after_marker] == b'\r'
        || (body[after_marker] == b' '
            && body.get(after_marker + 1).is_some_and(|&b| b == b'\n' || b == b'\r'))
    {
        return Some(ListMarker {
            width: indent + marker_w + 1,
            ordered: true,
            start_num: num,
            marker_char: body[i],
        });
    }
    if body[after_marker] == b' ' || body[after_marker] == b'\t' {
        let mut spaces = 0;
        let mut k = after_marker;
        while k < body.len() && body[k] == b' ' {
            spaces += 1;
            k += 1;
        }
        let n = if spaces >= 5 { 1 } else { spaces };
        return Some(ListMarker {
            width: indent + marker_w + n,
            ordered: true,
            start_num: num,
            marker_char: body[i],
        });
    }
    None
}

/// CommonMark §5.2 marker scan, fully column-based (tabs expand to the next
/// multiple of 4). Returns the marker's structure if `line` begins (after at
/// most 3 columns of indentation) with a list marker.
///
///   content_indent = marker_indent + W + N        (§5.2 basic case)
///     W = marker width in columns (1 for bullets; digits + delimiter)
///     N = columns of indentation after the marker, where 1 ≤ N ≤ 4. If the
///       marker is followed by a blank line (empty item) or by ≥ 5 columns of
///       space, N := 1.
pub(crate) struct MarkerScan {
    pub marker_indent: usize,
    pub content_indent: usize,
    pub ordered: bool,
    /// Marker family char: b'-'/b'*'/b'+' for bullets, b'.'/b')' for ordered.
    pub delim: u8,
    /// Byte offset just past the consumed marker + content spaces.
    pub content_byte: usize,
}

pub(crate) fn scan_marker(line: &[u8]) -> Option<MarkerScan> {
    let mut col = 0;
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b' ' => col += 1,
            b'\t' => col += 4 - (col % 4),
            _ => break,
        }
        i += 1;
    }
    if col > 3 || i >= line.len() {
        return None;
    }
    let marker_indent = col;
    let (ordered, delim, w) = if matches!(line[i], b'-' | b'*' | b'+') {
        (false, line[i], 1usize)
    } else if line[i].is_ascii_digit() {
        let mut j = i;
        while j < line.len() && line[j].is_ascii_digit() && j - i < 9 {
            j += 1;
        }
        if j >= line.len() || (line[j] != b'.' && line[j] != b')') {
            return None;
        }
        (true, line[j], (j - i) + 1)
    } else {
        return None;
    };
    i += w;
    col += w;
    if i >= line.len() || line[i] == b'\n' || line[i] == b'\r' {
        return Some(MarkerScan {
            marker_indent,
            content_indent: marker_indent + w + 1,
            ordered,
            delim,
            content_byte: i,
        });
    }
    if line[i] != b' ' && line[i] != b'\t' {
        return None;
    }
    let mut k = i;
    let mut spcol = 0;
    while k < line.len() && (line[k] == b' ' || line[k] == b'\t') {
        if line[k] == b' ' {
            spcol += 1;
        } else {
            spcol += 4 - ((col + spcol) % 4);
        }
        k += 1;
    }
    if k >= line.len() || line[k] == b'\n' || line[k] == b'\r' {
        return Some(MarkerScan {
            marker_indent,
            content_indent: marker_indent + w + 1,
            ordered,
            delim,
            content_byte: i,
        });
    }
    let (n, content_byte) = if spcol >= 5 { (1, i + 1) } else { (spcol, k) };
    Some(MarkerScan {
        marker_indent,
        content_indent: marker_indent + w + n,
        ordered,
        delim,
        content_byte,
    })
}

/// Column where the first non-whitespace char of `line` begins (tabs → mult-4).
pub(crate) fn indent_cols(line: &[u8]) -> usize {
    let mut col = 0;
    for &b in line {
        match b {
            b' ' => col += 1,
            b'\t' => col += 4 - (col % 4),
            _ => break,
        }
    }
    col
}

fn scan_table(bytes: &[u8], start: usize, ctx: ScanCtx<'_>) -> Option<RawBlock> {
    // Quick gate: needs `|` in the first line, AND second line must be a
    // delimiter row (`|---|---|` with optional alignment colons).
    let line = line_slice(bytes, start);
    if !line.contains(&b'|') {
        return None;
    }
    let next = line_end(bytes, start);
    if next >= bytes.len() {
        return None;
    }
    let delim = line_slice(bytes, next);
    if !is_table_delimiter_row(delim) {
        return None;
    }
    // §GFM: the header and delimiter rows must have the same number of cells.
    if count_table_columns(line) != count_table_columns(delim) {
        return None;
    }
    let mut pos = line_end(bytes, next);
    while pos < bytes.len() {
        // A blank line or a line that starts another block ends the table; any
        // other line (even one without pipes) is a data row, normalized to the
        // header's column count by the renderer (§GFM).
        if is_blank_line(bytes, pos) || would_start_other_block(bytes, pos, ctx) {
            break;
        }
        pos = line_end(bytes, pos);
    }
    Some(RawBlock { kind: RawBlockKind::Table, range: start..pos })
}

/// Count the cells in a table row: unescaped `|` separators (+1), after
/// dropping one optional leading and trailing pipe.
fn count_table_columns(line: &[u8]) -> usize {
    // Trim trailing newline and surrounding whitespace.
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\n' | b'\r' | b' ' | b'\t') {
        end -= 1;
    }
    let mut start = 0;
    while start < end && matches!(line[start], b' ' | b'\t') {
        start += 1;
    }
    let mut inner = &line[start..end];
    if inner.first() == Some(&b'|') {
        inner = &inner[1..];
    }
    if inner.last() == Some(&b'|') {
        inner = &inner[..inner.len() - 1];
    }
    if inner.is_empty() {
        return 0;
    }
    let mut cells = 1;
    let mut escaped = false;
    for &b in inner {
        if escaped {
            escaped = false;
        } else if b == b'\\' {
            escaped = true;
        } else if b == b'|' {
            cells += 1;
        }
    }
    cells
}

fn is_table_delimiter_row(line: &[u8]) -> bool {
    let mut saw_dash = false;
    let mut saw_pipe = false;
    for &b in line {
        if b == b'\n' || b == b'\r' {
            break;
        }
        match b {
            b'|' => saw_pipe = true,
            b'-' => saw_dash = true,
            b':' | b' ' | b'\t' => {}
            _ => return false,
        }
    }
    saw_dash && saw_pipe
}

fn scan_paragraph(bytes: &[u8], start: usize, ctx: ScanCtx<'_>) -> RawBlock {
    let mut pos = line_end(bytes, start);
    while pos < bytes.len() {
        if is_blank_line(bytes, pos) {
            break;
        }
        // Setext underline: this line is `===` or `---` (possibly indented up
        // to 3 spaces). Promotes the paragraph to a heading.
        if let Some(level) = is_setext_underline(bytes, pos) {
            let end = line_end(bytes, pos);
            return RawBlock {
                kind: RawBlockKind::SetextHeading { level },
                range: start..end,
            };
        }
        if would_start_other_block(bytes, pos, ctx) {
            break;
        }
        pos = line_end(bytes, pos);
    }
    RawBlock { kind: RawBlockKind::Paragraph, range: start..pos }
}

pub(crate) fn is_setext_underline(bytes: &[u8], start: usize) -> Option<u8> {
    let line = line_slice(bytes, start);
    let (indent, body) = strip_indent(line, 3);
    if indent > 3 || body.is_empty() {
        return None;
    }
    let c = body[0];
    if c != b'=' && c != b'-' {
        return None;
    }
    let mut len = 0;
    while len < body.len() && body[len] == c {
        len += 1;
    }
    // Only whitespace allowed after the underline chars.
    for &b in &body[len..] {
        if b == b'\n' || b == b'\r' {
            break;
        }
        if b != b' ' && b != b'\t' {
            return None;
        }
    }
    Some(if c == b'=' { 1 } else { 2 })
}

fn scan_indented_code(bytes: &[u8], start: usize) -> Option<RawBlock> {
    let line = line_slice(bytes, start);
    let mut indent = 0;
    let mut idx = 0;
    while idx < line.len() {
        match line[idx] {
            b' ' => {
                indent += 1;
                idx += 1;
            }
            b'\t' => {
                indent += 4;
                idx += 1;
            }
            _ => break,
        }
        if indent >= 4 {
            break;
        }
    }
    if indent < 4 {
        return None;
    }
    if idx >= line.len() || line[idx] == b'\n' || line[idx] == b'\r' {
        return None;
    }
    let mut pos = line_end(bytes, start);
    let mut last_content_end = pos;
    while pos < bytes.len() {
        let l = line_slice(bytes, pos);
        if is_blank_line(bytes, pos) {
            pos = line_end(bytes, pos);
            continue;
        }
        let mut ind = 0;
        let mut i = 0;
        while i < l.len() {
            match l[i] {
                b' ' => {
                    ind += 1;
                    i += 1;
                }
                b'\t' => {
                    ind += 4;
                    i += 1;
                }
                _ => break,
            }
            if ind >= 4 {
                break;
            }
        }
        if ind < 4 {
            break;
        }
        pos = line_end(bytes, pos);
        last_content_end = pos;
    }
    Some(RawBlock { kind: RawBlockKind::IndentedCode, range: start..last_content_end })
}

// ---------------------------------------------------------------------
// Line helpers
// ---------------------------------------------------------------------

pub fn is_blank_line(bytes: &[u8], start: usize) -> bool {
    let line = line_slice(bytes, start);
    line.iter().all(|&b| b == b' ' || b == b'\t' || b == b'\n' || b == b'\r')
}

/// Byte offset of the start of the line after the one containing `start`.
/// Always > start unless we're already at EOF.
pub fn line_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    if i < bytes.len() {
        i + 1
    } else {
        i
    }
}

/// Slice of bytes from `start` up to and including the line terminator (or
/// EOF if none).
pub fn line_slice(bytes: &[u8], start: usize) -> &[u8] {
    let end = line_end(bytes, start);
    &bytes[start..end]
}

/// Strip up to `max` leading spaces (tabs count as 4). Returns (indent_width,
/// trimmed_slice).
fn strip_indent(line: &[u8], max: usize) -> (usize, &[u8]) {
    let mut indent = 0;
    let mut i = 0;
    while i < line.len() && indent < max {
        match line[i] {
            b' ' => {
                indent += 1;
                i += 1;
            }
            b'\t' => {
                indent += 4;
                i += 1;
            }
            _ => break,
        }
    }
    (indent, &line[i..])
}

fn line_starts_with_marker(bytes: &[u8], start: usize, marker: u8) -> bool {
    let line = line_slice(bytes, start);
    let (indent, body) = strip_indent(line, 3);
    if indent > 3 {
        return false;
    }
    !body.is_empty() && body[0] == marker
}

pub(crate) fn would_start_other_block(bytes: &[u8], start: usize, ctx: ScanCtx<'_>) -> bool {
    scan_heading(bytes, start).is_some()
        || scan_fence(bytes, start).is_some()
        || (ctx.math && scan_math_block(bytes, start).is_some())
        || scan_hr(bytes, start).is_some()
        || scan_html_block_interrupting(bytes, start)
        || line_starts_component(bytes, start, ctx.component_tags)
        || line_starts_with_marker(bytes, start, b'>')
        || marker_can_interrupt_paragraph(bytes, start)
}

/// An allowlisted component open tag at the line start interrupts a paragraph,
/// so `text` followed by `<Thinking>` splits cleanly into a paragraph and a
/// component (rather than the tag being absorbed as paragraph text).
fn line_starts_component(bytes: &[u8], start: usize, tags: &[Box<str>]) -> bool {
    if tags.is_empty() {
        return false;
    }
    let line = line_slice(bytes, start);
    let (indent, body) = strip_indent(line, 3);
    indent <= 3 && component_open_tag(body, tags).is_some()
}

/// HTML blocks of types 1-6 can interrupt a paragraph; type 7 cannot.
fn scan_html_block_interrupting(bytes: &[u8], start: usize) -> bool {
    if let Some((_, html_type)) = detect_html_block_open(bytes, start) {
        html_type < 7
    } else {
        false
    }
}

// ---------------------------------------------------------------------
// HTML block detection (CommonMark §4.6, all 7 types)
// ---------------------------------------------------------------------

const HTML_BLOCK_TAGS: &[&[u8]] = &[
    b"address", b"article", b"aside", b"base", b"basefont", b"blockquote",
    b"body", b"caption", b"center", b"col", b"colgroup", b"dd", b"details",
    b"dialog", b"dir", b"div", b"dl", b"dt", b"fieldset", b"figcaption",
    b"figure", b"footer", b"form", b"frame", b"frameset", b"h1", b"h2",
    b"h3", b"h4", b"h5", b"h6", b"head", b"header", b"hr", b"html",
    b"iframe", b"legend", b"li", b"link", b"main", b"menu", b"menuitem",
    b"nav", b"noframes", b"ol", b"optgroup", b"option", b"p", b"param",
    b"search", b"section", b"summary", b"table", b"tbody", b"td", b"tfoot",
    b"th", b"thead", b"title", b"tr", b"track", b"ul",
];

const TYPE1_TAGS: &[&[u8]] = &[b"script", b"pre", b"style", b"textarea"];

fn ascii_lower(b: u8) -> u8 {
    if b.is_ascii_uppercase() { b + 32 } else { b }
}

fn eq_ascii_ci(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| ascii_lower(*x) == ascii_lower(*y))
}

fn detect_html_block_open(bytes: &[u8], start: usize) -> Option<(usize, u8)> {
    let line = line_slice(bytes, start);
    let (indent, body) = strip_indent(line, 3);
    if indent > 3 || body.len() < 2 || body[0] != b'<' {
        return None;
    }
    // Type 2: <!--
    if body.starts_with(b"<!--") {
        return Some((start, 2));
    }
    // Type 3: <?
    if body.starts_with(b"<?") {
        return Some((start, 3));
    }
    // Type 5: <![CDATA[
    if body.starts_with(b"<![CDATA[") {
        return Some((start, 5));
    }
    // Type 4: <! + uppercase letter
    if body.len() >= 3 && body[1] == b'!' && body[2].is_ascii_uppercase() {
        return Some((start, 4));
    }
    // Type 1: <script | <pre | <style | <textarea  (case-insensitive,
    // followed by whitespace, >, or end of line)
    let after_lt = 1 + if body.get(1) == Some(&b'/') { 1 } else { 0 };
    let name_end = body
        .iter()
        .enumerate()
        .skip(after_lt)
        .find(|(_, b)| !b.is_ascii_alphanumeric() && **b != b'-')
        .map(|(i, _)| i)
        .unwrap_or(body.len());
    if name_end <= after_lt {
        return None;
    }
    let name = &body[after_lt..name_end];
    let after_name = body.get(name_end).copied();
    let valid_after = matches!(
        after_name,
        Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') | Some(b'>') | None
    );
    if valid_after && TYPE1_TAGS.iter().any(|t| eq_ascii_ci(name, t)) && body.get(1) != Some(&b'/') {
        return Some((start, 1));
    }
    // Type 6: block-level tag, valid after-char includes '/>'
    let valid6 = valid_after
        || (after_name == Some(b'/') && body.get(name_end + 1) == Some(&b'>'));
    if valid6 && HTML_BLOCK_TAGS.iter().any(|t| eq_ascii_ci(name, t)) {
        return Some((start, 6));
    }
    // Type 7: complete opening or closing tag, nothing else on the line
    if is_complete_html_tag_line(body) && !TYPE1_TAGS.iter().any(|t| eq_ascii_ci(name, t)) {
        return Some((start, 7));
    }
    None
}

fn is_complete_html_tag_line(body: &[u8]) -> bool {
    // A line is type 7 iff it's exactly one open or close tag with optional
    // whitespace after. We parse the tag and verify nothing else remains.
    let mut i = 0;
    if body.get(i) != Some(&b'<') {
        return false;
    }
    i += 1;
    let closing = body.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    // Tag name: letter then alnum/-
    if i >= body.len() || !body[i].is_ascii_alphabetic() {
        return false;
    }
    while i < body.len() && (body[i].is_ascii_alphanumeric() || body[i] == b'-') {
        i += 1;
    }
    if closing {
        // Closing tag: optional whitespace then >.
        while i < body.len() && (body[i] == b' ' || body[i] == b'\t') {
            i += 1;
        }
        if body.get(i) != Some(&b'>') {
            return false;
        }
        i += 1;
    } else {
        // Opening tag: attributes, then optional / then >.
        loop {
            // Skip whitespace.
            let prev = i;
            while i < body.len() && matches!(body[i], b' ' | b'\t') {
                i += 1;
            }
            if body.get(i) == Some(&b'/') {
                i += 1;
                break;
            }
            if body.get(i) == Some(&b'>') {
                break;
            }
            if i == prev {
                return false; // no progress
            }
            // Attribute name.
            if i >= body.len() || !(body[i].is_ascii_alphabetic() || body[i] == b'_' || body[i] == b':') {
                return false;
            }
            while i < body.len()
                && (body[i].is_ascii_alphanumeric()
                    || matches!(body[i], b'_' | b':' | b'.' | b'-'))
            {
                i += 1;
            }
            // Optional value.
            while i < body.len() && (body[i] == b' ' || body[i] == b'\t') {
                i += 1;
            }
            if body.get(i) == Some(&b'=') {
                i += 1;
                while i < body.len() && (body[i] == b' ' || body[i] == b'\t') {
                    i += 1;
                }
                if body.get(i) == Some(&b'"') {
                    i += 1;
                    while i < body.len() && body[i] != b'"' {
                        i += 1;
                    }
                    if body.get(i) != Some(&b'"') {
                        return false;
                    }
                    i += 1;
                } else if body.get(i) == Some(&b'\'') {
                    i += 1;
                    while i < body.len() && body[i] != b'\'' {
                        i += 1;
                    }
                    if body.get(i) != Some(&b'\'') {
                        return false;
                    }
                    i += 1;
                } else {
                    // Unquoted value.
                    if i >= body.len() {
                        return false;
                    }
                    while i < body.len()
                        && !matches!(body[i], b' ' | b'\t' | b'>' | b'<' | b'\'' | b'"' | b'=' | b'`')
                    {
                        i += 1;
                    }
                }
            }
        }
        if body.get(i) != Some(&b'>') {
            return false;
        }
        i += 1;
    }
    // Only whitespace allowed after.
    while i < body.len() {
        match body[i] {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            _ => return false,
        }
    }
    true
}

fn scan_html_block(bytes: &[u8], start: usize) -> Option<RawBlock> {
    let (_, html_type) = detect_html_block_open(bytes, start)?;
    let mut pos = start;
    // `closed` distinguishes "end condition met" from "ran out of input": a
    // type-1–5 block that hasn't seen its closing tag is still open and may
    // absorb more lines (including blank ones).
    let mut closed = false;
    loop {
        let line = line_slice(bytes, pos);
        let end_here = match html_type {
            1 => line_contains_type1_close(line),
            2 => line.windows(3).any(|w| w == b"-->"),
            3 => line.windows(2).any(|w| w == b"?>"),
            4 => line.contains(&b'>'),
            5 => line.windows(3).any(|w| w == b"]]>"),
            6 | 7 => false,
            _ => false,
        };
        let next = line_end(bytes, pos);
        if end_here {
            pos = next;
            closed = true;
            break;
        }
        if (html_type == 6 || html_type == 7) && is_blank_line(bytes, pos) {
            // The blank line terminates types 6/7 and is not part of the block.
            closed = true;
            break;
        }
        if next == pos {
            pos = next;
            break;
        }
        pos = next;
        if pos >= bytes.len() {
            break;
        }
    }
    Some(RawBlock { kind: RawBlockKind::HtmlBlock { closed }, range: start..pos })
}

/// If `body` (a line with leading indent already stripped) begins with an
/// allowlisted component open tag, return `(name, self_closing, end_in_body)`
/// where `end_in_body` is just past the tag's `>`. `None` if it isn't an
/// allowlisted open tag whose `>` is on this line (v1: single-line open tags).
fn component_open_tag<'a>(body: &'a [u8], tags: &[Box<str>]) -> Option<(&'a [u8], bool, usize)> {
    if body.first() != Some(&b'<') {
        return None;
    }
    let name_start = 1;
    let mut i = name_start;
    while i < body.len() && (body[i].is_ascii_alphanumeric() || body[i] == b'-') {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = &body[name_start..i];
    if !tags.iter().any(|t| t.as_bytes() == name) {
        return None;
    }
    // Find the `>` that closes the open tag, tolerating quoted attribute values.
    let mut in_quote = 0u8;
    while i < body.len() {
        let c = body[i];
        if in_quote != 0 {
            if c == in_quote {
                in_quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            in_quote = c;
        } else if c == b'>' {
            // Self-closing iff the last non-space byte before `>` is `/`.
            let mut k = i;
            while k > name_start && matches!(body[k - 1], b' ' | b'\t') {
                k -= 1;
            }
            let self_closing = body[k - 1] == b'/';
            return Some((name, self_closing, i + 1));
        }
        i += 1;
    }
    None
}

/// True iff `body` (indent stripped) is exactly a `</name>` close tag followed by
/// only whitespace — the content-aware "close line" (a `</name>` embedded in
/// other text or a code span is not a clean close line, so it stays content).
fn is_clean_close_tag(body: &[u8], name: &[u8]) -> bool {
    let mut i = 0;
    if !body.starts_with(b"</") {
        return false;
    }
    i += 2;
    if body.len() < i + name.len() || &body[i..i + name.len()] != name {
        return false;
    }
    i += name.len();
    if body.get(i) != Some(&b'>') {
        return false;
    }
    i += 1;
    body[i..].iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
}

/// Component container (`<Tag …>…</Tag>`) for an allowlisted `Tag`. The body is
/// markdown (rendered recursively). Scans line-by-line for the matching close,
/// tracking same-tag nesting depth and skipping code fences (so a `</Tag>` line
/// inside a ``` block does not close). Blank-line tolerant. Self-closing
/// `<Tag/>` is a terminated empty container.
fn scan_component_block(bytes: &[u8], start: usize, tags: &[Box<str>]) -> Option<RawBlock> {
    let line = line_slice(bytes, start);
    let (indent, body) = strip_indent(line, 3);
    if indent > 3 {
        return None;
    }
    let (name, self_closing, _end) = component_open_tag(body, tags)?;
    let tag = String::from_utf8_lossy(name).into_owned();
    if self_closing {
        return Some(RawBlock {
            kind: RawBlockKind::ComponentBlock { tag, terminated: true },
            range: start..line_end(bytes, start),
        });
    }
    let mut depth = 1usize;
    let mut pos = line_end(bytes, start);
    let mut terminated = false;
    let mut in_fence = false;
    while pos < bytes.len() {
        let l = line_slice(bytes, pos);
        let (_ind, lb) = strip_indent(l, 3);
        // A code fence delimiter line toggles fence state; inside a fence, tag
        // lines are content (a `</Tag>` in a code block must not close).
        if lb.starts_with(b"```") || lb.starts_with(b"~~~") {
            in_fence = !in_fence;
        } else if !in_fence {
            if is_clean_close_tag(lb, name) {
                depth -= 1;
                if depth == 0 {
                    pos = line_end(bytes, pos);
                    terminated = true;
                    break;
                }
            } else if let Some((n2, sc2, _)) = component_open_tag(lb, tags) {
                if n2 == name && !sc2 {
                    depth += 1; // same-tag nesting
                }
            }
        }
        pos = line_end(bytes, pos);
    }
    Some(RawBlock { kind: RawBlockKind::ComponentBlock { tag, terminated }, range: start..pos })
}

/// Byte range of a component block's inner (markdown) content within its own
/// slice: from just past the open tag's `>` to the start of the matching close
/// line (or the slice end if not terminated). Shares the scanner's line/indent
/// and clean-close helpers so render and scan agree on the boundary. Returns
/// `(open_end, inner_end)`.
pub(crate) fn component_inner_range(slice: &str, tag: &str, terminated: bool) -> (usize, usize) {
    let bytes = slice.as_bytes();
    let line = line_slice(bytes, 0);
    let (_indent, body) = strip_indent(line, 3);
    let indent_len = line.len() - body.len();
    // First unquoted `>` ends the open tag.
    let mut open_end = slice.len();
    let mut in_quote = 0u8;
    for (i, &c) in body.iter().enumerate() {
        if in_quote != 0 {
            if c == in_quote {
                in_quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            in_quote = c;
        } else if c == b'>' {
            open_end = indent_len + i + 1;
            break;
        }
    }
    if !terminated {
        return (open_end, slice.len().max(open_end));
    }
    // The matching close is the last clean `</tag>` line in the slice (any
    // earlier close-looking line is nested or inside a fence and precedes it).
    let mut inner_end = slice.len();
    let mut pos = 0;
    while pos < bytes.len() {
        let l = line_slice(bytes, pos);
        let (_i, lb) = strip_indent(l, 3);
        if is_clean_close_tag(lb, tag.as_bytes()) {
            inner_end = pos;
        }
        pos = line_end(bytes, pos);
    }
    (open_end.min(inner_end), inner_end)
}

fn line_contains_type1_close(line: &[u8]) -> bool {
    let mut i = 0;
    while i + 1 < line.len() {
        if line[i] == b'<' && line[i + 1] == b'/' {
            let after = i + 2;
            for tag in TYPE1_TAGS {
                if after + tag.len() <= line.len() && eq_ascii_ci(&line[after..after + tag.len()], tag) {
                    return true;
                }
            }
        }
        i += 1;
    }
    false
}

// ---------------------------------------------------------------------
// Link reference definition: `[label]: url "title"`  (CommonMark §4.7)
// ---------------------------------------------------------------------

fn scan_link_ref_def(bytes: &[u8], start: usize) -> Option<RawBlock> {
    let (_label, end) = parse_link_ref_def(bytes, start)?;
    Some(RawBlock { kind: RawBlockKind::LinkRefDefinition, range: start..end })
}

/// Returns `((label, url, title_opt), end_byte_offset)` if `bytes[start..]`
/// begins with a valid link reference definition. Used by both scanner and
/// parser (no duplicate parsing logic).
pub fn parse_link_ref_def(
    bytes: &[u8],
    start: usize,
) -> Option<((String, String, Option<String>), usize)> {
    // Leading indent up to 3.
    let mut pos = start;
    let mut indent = 0;
    while pos < bytes.len() && bytes[pos] == b' ' && indent < 4 {
        pos += 1;
        indent += 1;
    }
    if indent > 3 || pos >= bytes.len() || bytes[pos] != b'[' {
        return None;
    }
    pos += 1;
    // Label (may span lines but not blank lines).
    let label_start = pos;
    let mut depth = 1;
    while pos < bytes.len() && depth > 0 {
        let b = bytes[pos];
        if b == b'\\' && pos + 1 < bytes.len() {
            pos += 2;
            continue;
        }
        if b == b'[' {
            depth += 1;
            pos += 1;
            continue;
        }
        if b == b']' {
            depth -= 1;
            if depth == 0 {
                break;
            }
            pos += 1;
            continue;
        }
        if b == b'\n' {
            if pos + 1 < bytes.len() && bytes[pos + 1] == b'\n' {
                return None;
            }
            pos += 1;
            continue;
        }
        pos += 1;
    }
    if depth != 0 || pos >= bytes.len() {
        return None;
    }
    let label_end = pos;
    if label_end == label_start || bytes[label_start..label_end].iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) {
        return None;
    }
    // A label may not contain unescaped brackets (§6.3); such a "definition"
    // is just paragraph text.
    if let Ok(lbl) = std::str::from_utf8(&bytes[label_start..label_end]) {
        if !crate::render::valid_link_label(lbl) {
            return None;
        }
    }
    pos += 1; // past ]
    if bytes.get(pos) != Some(&b':') {
        return None;
    }
    pos += 1;
    // Skip whitespace, possibly one newline.
    let mut newlines = 0;
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        if bytes[pos] == b'\n' {
            newlines += 1;
            if newlines > 1 {
                return None;
            }
        }
        pos += 1;
    }
    if pos >= bytes.len() {
        return None;
    }
    // Read URL.
    let (url, after_url) = if bytes[pos] == b'<' {
        let mut j = pos + 1;
        while j < bytes.len() && bytes[j] != b'>' && bytes[j] != b'\n' && bytes[j] != b'<' {
            if bytes[j] == b'\\' && j + 1 < bytes.len() {
                j += 2;
            } else {
                j += 1;
            }
        }
        if j >= bytes.len() || bytes[j] != b'>' {
            return None;
        }
        let u = std::str::from_utf8(&bytes[pos + 1..j]).ok()?.to_string();
        (u, j + 1)
    } else {
        let s = pos;
        while pos < bytes.len()
            && !matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r')
            && bytes[pos] >= 0x20
        {
            if bytes[pos] == b'\\' && pos + 1 < bytes.len() {
                pos += 2;
            } else {
                pos += 1;
            }
        }
        if pos == s {
            return None;
        }
        (std::str::from_utf8(&bytes[s..pos]).ok()?.to_string(), pos)
    };
    pos = after_url;
    // Optional title — may be on next line.
    let after_url_pos = pos;
    let mut title_pos = pos;
    let mut ws_nl = 0;
    while title_pos < bytes.len() && matches!(bytes[title_pos], b' ' | b'\t' | b'\n' | b'\r') {
        if bytes[title_pos] == b'\n' {
            ws_nl += 1;
            if ws_nl > 1 {
                break;
            }
        }
        title_pos += 1;
    }
    let mut title: Option<String> = None;
    let mut end_after_title = pos;
    let mut accepted_title = false;
    // A title must be separated from the destination by whitespace; if a
    // title-opener abuts the destination (e.g. `<bar>(baz)`) it is not a title,
    // and the trailing junk makes the whole definition invalid.
    if title_pos > after_url_pos
        && title_pos < bytes.len()
        && matches!(bytes[title_pos], b'"' | b'\'' | b'(')
        && ws_nl <= 1
    {
        let close = match bytes[title_pos] {
            b'"' => b'"',
            b'\'' => b'\'',
            _ => b')',
        };
        let mut j = title_pos + 1;
        let mut had_blank = false;
        let mut prev_nl = false;
        while j < bytes.len() && bytes[j] != close {
            if bytes[j] == b'\\' && j + 1 < bytes.len() {
                j += 2;
                prev_nl = false;
                continue;
            }
            if bytes[j] == b'\n' {
                if prev_nl {
                    had_blank = true;
                    break;
                }
                prev_nl = true;
            } else if !matches!(bytes[j], b' ' | b'\t' | b'\r') {
                prev_nl = false;
            }
            j += 1;
        }
        if !had_blank && j < bytes.len() && bytes[j] == close {
            let t = std::str::from_utf8(&bytes[title_pos + 1..j]).ok()?.to_string();
            // Rest of line after closing must be whitespace.
            let mut k = j + 1;
            while k < bytes.len() && matches!(bytes[k], b' ' | b'\t') {
                k += 1;
            }
            if k >= bytes.len() || bytes[k] == b'\n' || bytes[k] == b'\r' {
                title = Some(t);
                let next = if k < bytes.len() {
                    line_end(bytes, k)
                } else {
                    k
                };
                end_after_title = next;
                accepted_title = true;
            }
        }
    }
    let final_end = if accepted_title {
        end_after_title
    } else {
        // No (valid) title; URL line must end with whitespace + newline.
        let mut k = after_url_pos;
        while k < bytes.len() && matches!(bytes[k], b' ' | b'\t') {
            k += 1;
        }
        if k < bytes.len() && bytes[k] != b'\n' && bytes[k] != b'\r' {
            return None;
        }
        if k < bytes.len() {
            line_end(bytes, k)
        } else {
            k
        }
    };
    let label_bytes = &bytes[label_start..label_end];
    let label_str = std::str::from_utf8(label_bytes).ok()?;
    // CommonMark: label can't be only whitespace (already checked).
    Some(((label_str.to_string(), url, title), final_end))
}

/// CommonMark rule: an ordered list with start != 1 can NOT interrupt a
/// paragraph. Also: an EMPTY marker (e.g. just `*` on a line) does not
/// interrupt — that prevents `*foo bar\n*\n` from being chopped into a
/// paragraph + empty list mid-stream.
fn marker_can_interrupt_paragraph(bytes: &[u8], start: usize) -> bool {
    match detect_list_marker(bytes, start) {
        Some(m) => {
            if m.ordered && m.start_num != 1 {
                return false;
            }
            // Reject empty markers (marker char at end of line).
            let line = line_slice(bytes, start);
            let (indent, body) = strip_indent(line, 3);
            let _ = indent;
            let marker_w = if m.ordered {
                body.iter().position(|&b| !b.is_ascii_digit()).unwrap_or(0) + 1
            } else {
                1
            };
            // After the marker (and optional space), there must be non-ws.
            let mut i = marker_w;
            while i < body.len() && (body[i] == b' ' || body[i] == b'\t') {
                i += 1;
            }
            i < body.len() && body[i] != b'\n' && body[i] != b'\r'
        }
        None => false,
    }
}

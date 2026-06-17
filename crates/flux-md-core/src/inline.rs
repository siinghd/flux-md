//! Inline tokenizer + HTML renderer.
//!
//! Walks an inline string left-to-right, emitting HTML directly. Handles:
//! - Backslash escapes (incl. line-break escape)
//! - HTML entity / numeric character references (&amp; &#65; &#x41; etc.)
//! - Code spans (`...`, ``...``, etc. — matching backtick run length)
//! - Strong/emphasis with a delimiter-run stack (CommonMark §6.2)
//! - Strikethrough (~~text~~)
//! - Links [text](url "title") and images ![alt](url "title")
//! - Reference-style links [text][label], [text][], [label]
//! - Autolinks <https://...>, <foo@bar>
//! - Inline raw HTML (pass-through when opts.unsafe_html is on)
//! - Hard breaks (backslash + newline, or two-spaces + newline)
//!
//! URLs go through `sanitize_url`; text is HTML-escaped; raw HTML in input
//! is escaped (or passed through if unsafe mode is enabled).

use crate::entities::decode_entity;
use crate::render::RenderOpts;
use crate::url::{escape_attr, escape_html, sanitize_attrs, sanitize_image_url, sanitize_url};

const ESCAPABLE: &[u8] = b"!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

/// Render an inline string to HTML. Thin wrapper over [`render_inline_core`]
/// with boundary tracking off (zero overhead, byte-identical output).
pub fn render_inline(input: &str, opts: &RenderOpts, out: &mut String) {
    render_inline_core(input, opts, out, false);
}

/// Like [`render_inline`], but also returns the largest *stable* input offset —
/// the byte position up to which the rendered output is final regardless of any
/// text appended after `input`. Used by the streaming parser to commit the
/// settled prefix of a long open paragraph. Output is identical to
/// `render_inline`; only the extra analysis runs.
pub fn render_inline_boundary(input: &str, opts: &RenderOpts, out: &mut String) -> usize {
    render_inline_core(input, opts, out, true)
}

/// A top-level position is a clean cut iff a word begins there right after a
/// single inter-word space (preceded by a non-space) or right after a newline —
/// never inside a multi-space hard-break run.
fn is_boundary(bytes: &[u8], pos: usize) -> bool {
    if pos == 0 || pos >= bytes.len() || matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        return false;
    }
    match bytes[pos - 1] {
        b'\n' => true,
        b' ' => pos >= 2 && !matches!(bytes[pos - 2], b' ' | b'\t' | b'\n' | b'\r'),
        _ => false,
    }
}

/// Largest boundary candidate that is stable: at/before any unstable construct
/// (`unstable`), at/before any unpaired can-open emphasis opener (could pair
/// forward), and not strictly inside any resolved emphasis pair `(a, b]`.
fn compute_cut(candidates: &[usize], unstable: usize, stack: &[Delim], pairs: &[(usize, usize)]) -> usize {
    let mut earliest_open = usize::MAX;
    for d in stack {
        if d.len > 0 && d.can_open && d.in_at < earliest_open {
            earliest_open = d.in_at;
        }
    }
    let limit = unstable.min(earliest_open);
    let mut best = 0;
    for &c in candidates {
        if c > best && c <= limit && !pairs.iter().any(|&(a, b)| a < c && c <= b) {
            best = c;
        }
    }
    best
}

fn render_inline_core(input: &str, opts: &RenderOpts, out: &mut String, track: bool) -> usize {
    let bytes = input.as_bytes();
    let mut pos = 0;
    let mut deli_stack: Vec<Delim> = Vec::new();
    // Streaming boundary tracking (only populated when `track`).
    let mut candidates: Vec<usize> = Vec::new();
    let mut unstable = usize::MAX;

    while pos < bytes.len() {
        if track && is_boundary(bytes, pos) {
            candidates.push(pos);
        }
        // GFM extended autolinks: bare www./http(s)://ftp:// URLs and email
        // addresses in text. Gated on a left boundary (start / whitespace /
        // `*_~(`) so we only probe at plausible starts.
        if opts.gfm_autolinks && ext_autolink_boundary(bytes, pos) {
            if matches!(bytes[pos], b'w' | b'h' | b'f') {
                if let Some(consumed) = try_ext_autolink(bytes, pos, out) {
                    pos = consumed;
                    continue;
                }
            }
            if bytes[pos].is_ascii_alphanumeric() || matches!(bytes[pos], b'.' | b'_' | b'+' | b'-') {
                if let Some(consumed) = try_ext_email(bytes, pos, out) {
                    pos = consumed;
                    continue;
                }
            }
        }
        let b = bytes[pos];
        match b {
            // LaTeX inline math `\(…\)` and inline display `\[…\]` (math on).
            // Probed before the generic backslash-escape arm because `(`/`[` are
            // escapable; if there's no matching closer we fall back to the
            // escape behavior (a literal `(` / `[`).
            b'\\' if opts.gfm_math && bytes.get(pos + 1) == Some(&b'(') => {
                match try_math_delim(bytes, pos, b"\\(", b"\\)", false, out) {
                    Some(end) => pos = end,
                    None => {
                        if track && pos < unstable {
                            unstable = pos; // a later `\)` could form inline math
                        }
                        push_escaped(b'(', out);
                        pos += 2;
                    }
                }
            }
            b'\\' if opts.gfm_math && bytes.get(pos + 1) == Some(&b'[') => {
                match try_math_delim(bytes, pos, b"\\[", b"\\]", true, out) {
                    Some(end) => pos = end,
                    None => {
                        if track && pos < unstable {
                            unstable = pos; // a later `\]` could form display math
                        }
                        push_escaped(b'[', out);
                        pos += 2;
                    }
                }
            }
            b'\\' if pos + 1 < bytes.len() && ESCAPABLE.contains(&bytes[pos + 1]) => {
                push_escaped(bytes[pos + 1], out);
                pos += 2;
            }
            b'\\' if pos + 1 < bytes.len() && bytes[pos + 1] == b'\n' => {
                out.push_str("<br>\n");
                pos += 2;
            }
            b'&' => {
                if let Some((decoded, consumed)) = decode_entity(&bytes[pos..]) {
                    for c in decoded.chars() {
                        push_escaped_char(c, out);
                    }
                    pos += consumed;
                } else {
                    out.push_str("&amp;");
                    pos += 1;
                }
            }
            b'`' => {
                if let Some(consumed) = try_code_span(bytes, pos, out) {
                    pos = consumed;
                } else {
                    // No matching close for this backtick run: the whole run is
                    // literal. Emit all of it and advance past it, so its inner
                    // backticks aren't re-tried as a shorter opening run.
                    // Unstable: a closer could still arrive and form a code span.
                    if track && pos < unstable {
                        unstable = pos;
                    }
                    let mut run = 0;
                    while pos + run < bytes.len() && bytes[pos + run] == b'`' {
                        run += 1;
                    }
                    for _ in 0..run {
                        out.push('`');
                    }
                    pos += run;
                }
            }
            b'<' => {
                if let Some(consumed) = try_autolink(bytes, pos, out) {
                    pos = consumed;
                } else {
                    match try_inline_component(input, bytes, pos, opts, out) {
                        InlineComp::Done(end) => pos = end,
                        InlineComp::Incomplete => {
                            // An allowlisted inline open tag with no matching
                            // close yet: keep it inert (escape the `<`) and
                            // re-tryable — a later `</tag>` can still form the
                            // component, and if none ever arrives it degrades to
                            // escaped text (never eats following content).
                            if track && pos < unstable {
                                unstable = pos;
                            }
                            out.push_str("&lt;");
                            pos += 1;
                        }
                        InlineComp::NotComponent => {
                            if let Some(consumed) = try_inline_html(bytes, pos, opts, out) {
                                pos = consumed;
                            } else {
                                // Unstable: a later `>` could form an autolink / inline HTML.
                                if track && pos < unstable {
                                    unstable = pos;
                                }
                                out.push_str("&lt;");
                                pos += 1;
                            }
                        }
                    }
                }
            }
            b'!' if pos + 1 < bytes.len() && bytes[pos + 1] == b'[' => {
                if let Some(consumed) = try_image(bytes, pos, opts, out) {
                    pos = consumed;
                } else {
                    out.push('!');
                    pos += 1;
                }
            }
            b'[' => {
                if !opts.in_link {
                    let fnref = if opts.gfm_footnotes {
                        try_footnote_ref(bytes, pos, opts, out)
                    } else {
                        None
                    };
                    if let Some(consumed) = fnref {
                        pos = consumed;
                    } else if let Some(consumed) = try_link(bytes, pos, opts, out) {
                        // Settled: an inline `[text](url)` or a reference resolved
                        // via `opts.refs`. (Safe to treat resolved refs as settled
                        // because an *open paragraph* — the only block this cache
                        // serves — defines no reference definitions of its own, and
                        // first-definition-wins makes later doc defs non-overriding.)
                        pos = consumed;
                    } else {
                        // Literal `[`: a later `](url)` or `[ref]` (or a forward
                        // `[ref]: …` definition) could still turn it into a link.
                        if track && pos < unstable {
                            unstable = pos;
                        }
                        out.push('[');
                        pos += 1;
                    }
                } else {
                    if track && pos < unstable {
                        unstable = pos;
                    }
                    out.push('[');
                    pos += 1;
                }
            }
            b'$' if opts.gfm_math => {
                match try_dollar_math(bytes, pos, out) {
                    Some(end) => pos = end,
                    None => {
                        if track && pos < unstable {
                            unstable = pos; // a later `$` could form inline math
                        }
                        out.push('$');
                        pos += 1;
                    }
                }
            }
            b'*' | b'_' | b'~' => {
                let run = scan_delim_run(bytes, pos);
                let len = run.len;
                let class = b;
                let (can_open, can_close) = flanking(input, pos, len);
                let written_at = out.len();
                for _ in 0..len {
                    out.push(class as char);
                }
                deli_stack.push(Delim { at: written_at, in_at: pos, class, len, can_open, can_close });
                pos += len;
            }
            b' ' if pos + 1 < bytes.len() && bytes[pos + 1] == b' ' && trailing_spaces_before_nl(bytes, pos) => {
                // CommonMark hard break: 2+ trailing spaces before \n.
                let mut k = pos;
                while k < bytes.len() && bytes[k] == b' ' {
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b'\n' {
                    out.push_str("<br>\n");
                    pos = k + 1;
                } else {
                    out.push(' ');
                    pos += 1;
                }
            }
            b'\n' => {
                if out.ends_with("  ") {
                    out.truncate(out.len() - 2);
                    out.push_str("<br>\n");
                } else {
                    out.push('\n');
                }
                pos += 1;
            }
            b'\r' => {
                pos += 1;
            }
            _ => {
                if b < 0x80 {
                    push_escaped(b, out);
                    pos += 1;
                } else if let Some(c) = input[pos..].chars().next() {
                    push_escaped_char(c, out);
                    pos += c.len_utf8();
                } else {
                    pos += 1;
                }
            }
        }
    }

    let mut pairs: Vec<(usize, usize)> = Vec::new();
    resolve_delimiters(out, &mut deli_stack, if track { Some(&mut pairs) } else { None });

    if track {
        compute_cut(&candidates, unstable, &deli_stack, &pairs)
    } else {
        0
    }
}

/// GFM §6.9: an extended autolink may begin at the start of the line or after
/// whitespace or one of `*`, `_`, `~`, `(`.
fn ext_autolink_boundary(bytes: &[u8], pos: usize) -> bool {
    pos == 0 || matches!(bytes[pos - 1], b' ' | b'\t' | b'\n' | b'\r' | b'*' | b'_' | b'~' | b'(')
}

/// Try to match a GFM extended URL autolink (`www.`, `http://`, `https://`,
/// `ftp://`) at `pos`, emitting `<a …>…</a>` on success and returning the byte
/// offset just past it. Applies GFM's trailing-punctuation, balanced-paren and
/// entity-reference trimming rules.
fn try_ext_autolink(bytes: &[u8], start: usize, out: &mut String) -> Option<usize> {
    let rest = &bytes[start..];
    let scheme_prefix = if rest.starts_with(b"http://")
        || rest.starts_with(b"https://")
        || rest.starts_with(b"ftp://")
    {
        ""
    } else if rest.starts_with(b"www.") {
        "http://"
    } else {
        return None;
    };

    // Consume up to the next whitespace or `<` (which truncates the link).
    let mut end = start;
    while end < bytes.len() && !matches!(bytes[end], b' ' | b'\t' | b'\n' | b'\r' | b'<') {
        end += 1;
    }

    // Trailing-punctuation trimming (applied repeatedly).
    loop {
        if end <= start {
            return None;
        }
        let last = bytes[end - 1];
        if matches!(last, b'?' | b'!' | b'.' | b',' | b':' | b'*' | b'_' | b'~') {
            end -= 1;
            continue;
        }
        if last == b')' {
            let opens = bytes[start..end].iter().filter(|&&b| b == b'(').count();
            let closes = bytes[start..end].iter().filter(|&&b| b == b')').count();
            if closes > opens {
                end -= 1;
                continue;
            }
        }
        if last == b';' {
            // Looks like a trailing entity reference `&name;`? Trim it.
            if let Some(amp) = bytes[start..end].iter().rposition(|&b| b == b'&') {
                let amp = start + amp;
                if end - 1 > amp + 1
                    && bytes[amp + 1..end - 1].iter().all(|b| b.is_ascii_alphanumeric())
                {
                    end = amp;
                    continue;
                }
            }
        }
        break;
    }

    let text = std::str::from_utf8(&bytes[start..end]).ok()?;
    if !valid_autolink_domain(text, scheme_prefix) {
        return None;
    }

    out.push_str("<a href=\"");
    let mut href = String::with_capacity(scheme_prefix.len() + text.len());
    href.push_str(scheme_prefix);
    href.push_str(text);
    escape_attr(&href, out);
    out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">");
    escape_html(text, out);
    out.push_str("</a>");
    Some(end)
}

/// Try to match a GFM extended email autolink (`local@domain`) at `pos`. The
/// local part allows `.+_-` and alphanumerics; the domain allows `._-` and
/// alphanumerics, must contain a `.`, and may not end in `.`, `-` or `_`
/// (those are excluded from the link).
fn try_ext_email(bytes: &[u8], start: usize, out: &mut String) -> Option<usize> {
    let mut i = start;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'+' | b'_' | b'-')) {
        i += 1;
    }
    if i == start || bytes.get(i) != Some(&b'@') {
        return None;
    }
    let at = i;
    i += 1;
    let domain_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'_' | b'-')) {
        i += 1;
    }
    // A trailing `.` is punctuation, not part of the address; strip it.
    let mut end = i;
    while end > domain_start && bytes[end - 1] == b'.' {
        end -= 1;
    }
    // But a domain ending in `-` or `_` is invalid outright (not a link).
    if end > domain_start && matches!(bytes[end - 1], b'-' | b'_') {
        return None;
    }
    let domain = std::str::from_utf8(&bytes[domain_start..end]).ok()?;
    // Domain: ≥1 dot, non-empty labels of alnum/`-`/`_`.
    if !domain.contains('.') {
        return None;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.iter().any(|l| l.is_empty())
        || !labels.iter().all(|l| l.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
    {
        return None;
    }
    let text = std::str::from_utf8(&bytes[start..end]).ok()?;
    out.push_str("<a href=\"mailto:");
    escape_attr(text, out);
    out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">");
    escape_html(text, out);
    out.push_str("</a>");
    let _ = at;
    Some(end)
}

/// Validate the domain of an extended autolink: the authority (up to the first
/// `/`, `?`, or `#`) must contain at least one `.` separating non-empty,
/// dash/underscore/alnum segments, and the last segment must not be empty.
fn valid_autolink_domain(text: &str, scheme_prefix: &str) -> bool {
    let after_scheme = if scheme_prefix.is_empty() {
        // Skip the literal scheme in the text (`http://`, etc.).
        match text.find("://") {
            Some(i) => &text[i + 3..],
            None => return false,
        }
    } else {
        text
    };
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if !authority.contains('.') {
        return false;
    }
    let labels: Vec<&str> = authority.split('.').collect();
    if labels.iter().any(|l| l.is_empty()) {
        return false;
    }
    labels
        .iter()
        .all(|l| l.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
}

fn trailing_spaces_before_nl(bytes: &[u8], start: usize) -> bool {
    let mut k = start;
    while k < bytes.len() && bytes[k] == b' ' {
        k += 1;
    }
    k - start >= 2 && k < bytes.len() && bytes[k] == b'\n'
}

fn push_escaped(b: u8, out: &mut String) {
    match b {
        b'<' => out.push_str("&lt;"),
        b'>' => out.push_str("&gt;"),
        b'&' => out.push_str("&amp;"),
        b'"' => out.push_str("&quot;"),
        _ => out.push(b as char),
    }
}

fn push_escaped_char(c: char, out: &mut String) {
    match c {
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '&' => out.push_str("&amp;"),
        '"' => out.push_str("&quot;"),
        _ => out.push(c),
    }
}

// ---------------------------------------------------------------------
// Code spans
// ---------------------------------------------------------------------

fn try_code_span(bytes: &[u8], start: usize, out: &mut String) -> Option<usize> {
    let mut open_len = 0;
    while start + open_len < bytes.len() && bytes[start + open_len] == b'`' {
        open_len += 1;
    }
    let mut i = start + open_len;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let mut close_len = 0;
            while i + close_len < bytes.len() && bytes[i + close_len] == b'`' {
                close_len += 1;
            }
            if close_len == open_len {
                let content = &bytes[start + open_len..i];
                let s = std::str::from_utf8(content).unwrap_or("");
                // CommonMark §6.1: line endings become spaces; then if the
                // resulting string starts AND ends with a space (and is not
                // all spaces), strip one space from each side.
                let mut buf = String::with_capacity(s.len());
                for c in s.chars() {
                    if c == '\n' || c == '\r' {
                        buf.push(' ');
                    } else {
                        buf.push(c);
                    }
                }
                let trimmed = if buf.len() >= 2
                    && buf.starts_with(' ')
                    && buf.ends_with(' ')
                    && buf.chars().any(|c| c != ' ')
                {
                    &buf[1..buf.len() - 1]
                } else {
                    buf.as_str()
                };
                out.push_str("<code>");
                for c in trimmed.chars() {
                    push_escaped_char(c, out);
                }
                out.push_str("</code>");
                return Some(i + close_len);
            }
            i += close_len;
        } else {
            i += 1;
        }
    }
    None
}

#[allow(dead_code)]
fn trim_code_span(s: &[u8]) -> &[u8] {
    if s.len() >= 2 && s[0] == b' ' && s[s.len() - 1] == b' ' && s.iter().any(|&b| b != b' ') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

// ---------------------------------------------------------------------
// Math (gated on opts.gfm_math)
// ---------------------------------------------------------------------

/// Emit `<span class="math math-(inline|display)">…</span>` carrying the
/// HTML-escaped LaTeX. The body is never markdown-processed; KaTeX (or a
/// `components.MathBlock`-style override) reads the LaTeX from text content.
fn emit_inline_math(content: &[u8], display: bool, out: &mut String) {
    let s = std::str::from_utf8(content).unwrap_or("");
    out.push_str(if display {
        "<span class=\"math math-display\">"
    } else {
        "<span class=\"math math-inline\">"
    });
    escape_html(s, out);
    out.push_str("</span>");
}

/// LaTeX-delimited inline math: `\(…\)` (inline) or `\[…\]` (display). Returns
/// the offset just past the closing delimiter, or None (no closer / empty body)
/// so the caller can fall back to literal output. The body may span soft line
/// breaks but not a blank line (which ends the paragraph).
fn try_math_delim(
    bytes: &[u8],
    start: usize,
    open: &[u8],
    close: &[u8],
    display: bool,
    out: &mut String,
) -> Option<usize> {
    let content_start = start + open.len();
    let mut i = content_start;
    while i < bytes.len() {
        if bytes[i..].starts_with(close) {
            if i == content_start {
                return None; // empty `\(\)` — leave literal
            }
            emit_inline_math(&bytes[content_start..i], display, out);
            return Some(i + close.len());
        }
        if bytes[i] == b'\n' && bytes.get(i + 1) == Some(&b'\n') {
            return None;
        }
        i += 1;
    }
    None
}

/// Dollar-delimited inline math: `$…$` (inline) or `$$…$$` (display). Uses the
/// pandoc disambiguation rule for single `$` so currency text stays literal:
/// the opener must be followed by a non-space, and a closer is only valid when
/// preceded by a non-space and not followed by an ASCII digit. Returns the
/// offset past the closing run, or None to fall back to a literal `$`.
fn try_dollar_math(bytes: &[u8], start: usize, out: &mut String) -> Option<usize> {
    let mut run = 0;
    while start + run < bytes.len() && bytes[start + run] == b'$' {
        run += 1;
    }
    let display = run >= 2;
    let n = if display { 2 } else { 1 };
    let content_start = start + n;
    if !display {
        // Opener must have a non-space, non-EOL char to its right.
        match bytes.get(content_start) {
            None => return None,
            Some(&c) if matches!(c, b' ' | b'\t' | b'\n' | b'\r') => return None,
            _ => {}
        }
    }
    let mut i = content_start;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let mut clen = 0;
            while i + clen < bytes.len() && bytes[i + clen] == b'$' {
                clen += 1;
            }
            if clen >= n {
                let content_end = i;
                if content_end == content_start {
                    return None; // empty `$$` / `$ $`-style — leave literal
                }
                if !display {
                    // pandoc: closer needs a non-space to its left and must not
                    // be immediately followed by a digit.
                    let prev = bytes[content_end - 1];
                    let bad_left = matches!(prev, b' ' | b'\t' | b'\n' | b'\r');
                    let bad_right = bytes.get(content_end + n).is_some_and(|b| b.is_ascii_digit());
                    if bad_left || bad_right {
                        i += clen;
                        continue;
                    }
                }
                emit_inline_math(&bytes[content_start..content_end], display, out);
                return Some(content_end + n);
            }
            i += clen;
        } else if bytes[i] == b'\n' && bytes.get(i + 1) == Some(&b'\n') {
            return None; // don't cross a blank line
        } else {
            i += 1;
        }
    }
    None
}

// ---------------------------------------------------------------------
// Autolinks
// ---------------------------------------------------------------------

fn try_autolink(bytes: &[u8], start: usize, out: &mut String) -> Option<usize> {
    let end = bytes[start..].iter().position(|&b| b == b'>')? + start;
    let inner = &bytes[start + 1..end];
    if inner.is_empty() {
        return None;
    }
    if inner.iter().any(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'<' | b'\0')) {
        return None;
    }
    // CommonMark §6.4: autolinks do NOT process backslash escapes — the
    // presence of a backslash means it's a literal char in the URL, which
    // makes the whole thing invalid as an email autolink at minimum.
    let s = std::str::from_utf8(inner).ok()?;
    let is_uri = is_uri_scheme(s);
    let is_email = !is_uri && is_email_like(s) && !s.contains('\\');
    if !is_uri && !is_email {
        return None;
    }
    out.push_str("<a href=\"");
    if is_email {
        out.push_str("mailto:");
        // Email autolinks: just escape the chars, no percent-encoding for ASCII.
        crate::url::escape_attr(s, out);
    } else {
        // URI autolinks bypass our scheme allowlist (CommonMark allows any
        // valid scheme) but still get percent-encoded for unsafe chars.
        // Backslash escapes are NOT processed in autolinks.
        let normalized = autolink_normalize(s);
        crate::url::escape_attr(&normalized, out);
    }
    out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">");
    for c in s.chars() {
        push_escaped_char(c, out);
    }
    out.push_str("</a>");
    Some(end + 1)
}

fn autolink_normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_autolink_safe(c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for &b in c.encode_utf8(&mut buf).as_bytes() {
                out.push('%');
                out.push(hex_digit(b >> 4));
                out.push(hex_digit(b & 0xF));
            }
        }
    }
    out
}

fn is_autolink_safe(c: char) -> bool {
    // CommonMark percent-encodes `[`, `]`, and `\` in autolinks even though
    // they're "reserved" in URI grammar — to avoid ambiguity with link
    // brackets and escapes in the surrounding markdown.
    matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9'
        | '-' | '_' | '.' | '~'
        | '!' | '*' | '\'' | '(' | ')' | ';' | ':' | '@' | '&'
        | '=' | '+' | '$' | ',' | '/' | '?' | '#'
        | '%')
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + n - 10) as char,
    }
}

fn is_uri_scheme(s: &str) -> bool {
    // CommonMark autolink: scheme + ":" + non-whitespace.
    let bytes = s.as_bytes();
    let colon = match bytes.iter().position(|&b| b == b':') {
        Some(i) => i,
        None => return false,
    };
    if colon < 2 || colon > 32 {
        return false;
    }
    if !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    for &b in &bytes[1..colon] {
        if !(b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.') {
            return false;
        }
    }
    true
}

fn is_email_like(s: &str) -> bool {
    let bytes = s.as_bytes();
    let at = match bytes.iter().position(|&b| b == b'@') {
        Some(i) if i > 0 && i < bytes.len() - 1 => i,
        _ => return false,
    };
    let _ = at;
    // crude: any non-space and includes one @ and one .
    s.chars().all(|c| !c.is_whitespace()) && s.contains('.')
}

// ---------------------------------------------------------------------
// Inline raw HTML
// ---------------------------------------------------------------------

/// Tags that are NEVER rendered in allow-all sanitize mode — script/exec/resource
/// vectors and content-as-raw-text elements. (In restrict mode they're simply not
/// in the allowlist, so they escape; this set is what "allow all" carves out.)
const DANGEROUS_HTML_TAGS: &[&[u8]] = &[
    b"script", b"style", b"iframe", b"object", b"embed", b"base", b"link", b"meta",
    b"noscript", b"template", b"title", b"textarea", b"form", b"input", b"button",
    b"select", b"option", b"frame", b"frameset", b"applet", b"svg", b"math",
    b"audio", b"video", b"source", b"track", b"canvas",
    // Raw-text / escapable-raw-text elements: a browser treats everything after
    // them as unparsed text, so rendering one corrupts the rest of the DOM (and
    // `<plaintext>` is unclosable). Drop them in allow-all mode.
    b"xmp", b"plaintext", b"noembed", b"noframes", b"listing",
];

fn try_inline_html(bytes: &[u8], start: usize, opts: &RenderOpts, out: &mut String) -> Option<usize> {
    let consumed = match_inline_html(bytes, start)?;
    let token = &bytes[start..start + consumed];

    // HTML comments have no visible representation: drop them. The one exception
    // is bare `unsafe_html` pass-through (no sanitizer engaged), which keeps them
    // verbatim for CommonMark fidelity — a browser ignores them either way.
    if token.starts_with(b"<!--") {
        if opts.unsafe_html && !opts.html_sanitize {
            out.push_str(std::str::from_utf8(token).ok()?);
        }
        return Some(start + consumed);
    }

    if opts.html_sanitize {
        sanitize_inline_html(token, opts, out);
        return Some(start + consumed);
    }

    if opts.unsafe_html {
        out.push_str(std::str::from_utf8(token).ok()?);
    } else {
        // Escape it.
        for &b in token {
            push_escaped(b, out);
        }
    }
    Some(start + consumed)
}

/// Extract a tag's name range from a matched inline-HTML token, plus whether
/// it's a close tag. `None` for non-tag tokens (PI / CDATA / declaration /
/// malformed). The range is over ASCII bytes, so it is a valid `&str` boundary.
fn inline_tag_name(token: &[u8]) -> Option<(core::ops::Range<usize>, bool)> {
    if token.first() != Some(&b'<') {
        return None;
    }
    let is_close = token.get(1) == Some(&b'/');
    let name_start = if is_close { 2 } else { 1 };
    let mut i = name_start;
    while i < token.len() && (token[i].is_ascii_alphanumeric() || matches!(token[i], b'-' | b':')) {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    Some((name_start..i, is_close))
}

/// Render one inline raw-HTML token under the safe sanitizer (`html_sanitize`).
/// Drop-list tags and (in allow-all mode) dangerous tags are removed; allowed
/// tags render natively with sanitized attributes; everything else is escaped.
fn sanitize_inline_html(token: &[u8], opts: &RenderOpts, out: &mut String) {
    let Some((name_range, is_close)) = inline_tag_name(token) else {
        return; // PI / CDATA / declaration / malformed: drop in sanitize mode
    };
    let name = &token[name_range.clone()];
    // Explicit drop-list removes the tag entirely (markup gone; any text between
    // an open/close pair stays as inert text).
    if opts.html_drop.iter().any(|t| t.as_bytes().eq_ignore_ascii_case(name)) {
        return;
    }
    // The dangerous set is NON-OVERRIDABLE: a script/iframe/svg/… is dropped in
    // BOTH allow-all and restrict mode, even if explicitly allowlisted — a caller
    // who truly wants raw script uses `unsafe_html`, not the sanitizer. Dropping
    // (rather than escaping) leaves any open/close pair's body as inert text.
    if DANGEROUS_HTML_TAGS.iter().any(|d| d.eq_ignore_ascii_case(name)) {
        return;
    }
    // Allow-all renders every (non-dangerous) tag; restrict renders only the
    // allowlisted ones and escapes the rest (visible as literal text, never
    // executed).
    if !opts.html_allowlist.is_empty()
        && !opts.html_allowlist.iter().any(|t| t.as_bytes().eq_ignore_ascii_case(name))
    {
        for &b in token {
            push_escaped(b, out);
        }
        return;
    }
    // Validate the whole token to UTF-8 once; the tag name is an ASCII sub-slice
    // of it, so it can be sliced out without a second validation pass.
    let token_str = std::str::from_utf8(token).unwrap_or("");
    let name_str = token_str.get(name_range).unwrap_or("");
    if is_close {
        out.push_str("</");
        out.push_str(name_str);
        out.push('>');
        return;
    }
    out.push('<');
    out.push_str(name_str);
    for (k, v) in sanitize_attrs(token_str) {
        out.push(' ');
        out.push_str(&k);
        out.push_str("=\"");
        escape_attr(&v, out);
        out.push('"');
    }
    // Preserve an author's self-closing slash (harmless for void elements; keeps
    // non-void self-closes balanced).
    if token.ends_with(b"/>") {
        out.push_str(" />");
    } else {
        out.push('>');
    }
}

fn match_inline_html(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'<') {
        return None;
    }
    let rest = &bytes[start..];
    // Comment
    if rest.starts_with(b"<!--") {
        let end = rest.windows(3).position(|w| w == b"-->")?;
        return Some(end + 3);
    }
    // PI
    if rest.starts_with(b"<?") {
        let end = rest.windows(2).position(|w| w == b"?>")?;
        return Some(end + 2);
    }
    // CDATA
    if rest.starts_with(b"<![CDATA[") {
        let end = rest.windows(3).position(|w| w == b"]]>")?;
        return Some(end + 3);
    }
    // Declaration
    if rest.len() > 2 && rest[1] == b'!' && rest[2].is_ascii_alphabetic() {
        let end = rest.iter().position(|&b| b == b'>')?;
        return Some(end + 1);
    }
    // Open or close tag
    let mut i = 1;
    let closing = rest.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    if !rest.get(i).map_or(false, |b| b.is_ascii_alphabetic()) {
        return None;
    }
    while i < rest.len() && (rest[i].is_ascii_alphanumeric() || rest[i] == b'-') {
        i += 1;
    }
    if closing {
        while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if rest.get(i) != Some(&b'>') {
            return None;
        }
        return Some(i + 1);
    }
    // Open tag: attributes
    loop {
        let prev = i;
        while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if rest.get(i) == Some(&b'/') {
            i += 1;
            break;
        }
        if rest.get(i) == Some(&b'>') {
            break;
        }
        if i == prev {
            return None;
        }
        if i >= rest.len() || !(rest[i].is_ascii_alphabetic() || rest[i] == b'_' || rest[i] == b':') {
            return None;
        }
        while i < rest.len() && (rest[i].is_ascii_alphanumeric() || matches!(rest[i], b'_' | b':' | b'.' | b'-')) {
            i += 1;
        }
        // Optional `= value`, whitespace allowed around `=`. Only consume the
        // whitespace after the name if it is actually followed by `=`;
        // otherwise it is the (required) separator before the next attribute.
        let save = i;
        while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if rest.get(i) == Some(&b'=') {
            i += 1;
            while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
                i += 1;
            }
            if rest.get(i) == Some(&b'"') {
                i += 1;
                while i < rest.len() && rest[i] != b'"' {
                    i += 1;
                }
                if rest.get(i) != Some(&b'"') {
                    return None;
                }
                i += 1;
            } else if rest.get(i) == Some(&b'\'') {
                i += 1;
                while i < rest.len() && rest[i] != b'\'' {
                    i += 1;
                }
                if rest.get(i) != Some(&b'\'') {
                    return None;
                }
                i += 1;
            } else {
                let prev = i;
                while i < rest.len() && !matches!(rest[i], b' ' | b'\t' | b'\n' | b'>' | b'<' | b'\'' | b'"' | b'=' | b'`') {
                    i += 1;
                }
                if i == prev {
                    return None;
                }
            }
        } else {
            i = save;
        }
    }
    if rest.get(i) != Some(&b'>') {
        return None;
    }
    Some(i + 1)
}

// ---------------------------------------------------------------------
// Inline custom components (opt-in `inline_component_tags`)
// ---------------------------------------------------------------------

/// Result of probing for an inline component at a `<`.
enum InlineComp {
    /// Not an allowlisted inline-component open tag — try inline raw HTML next.
    NotComponent,
    /// An allowlisted inline open tag whose matching close has not arrived yet
    /// (or whose open tag is still incomplete): keep the `<` inert + retryable.
    Incomplete,
    /// Rendered to `out`; resume scanning at this byte offset.
    Done(usize),
}

/// Dispatch an allowlisted inline component (`<tik …>…</tik>` or self-closing
/// `<tik …/>`) at `start`. Inner content is rendered as inline markdown;
/// attributes are sanitized (event handlers dropped, dangerous URL schemes
/// neutralized) so the emitted element is XSS-safe even with `unsafe_html` off,
/// and a JSX/DOM layer dispatches it via `components[tag]`. Same-tag nesting and
/// inline code spans are respected when locating the matching close.
fn try_inline_component(
    input: &str,
    bytes: &[u8],
    start: usize,
    opts: &RenderOpts,
    out: &mut String,
) -> InlineComp {
    let tags = &opts.inline_component_tags;
    if tags.is_empty() {
        return InlineComp::NotComponent;
    }
    let Some((name_end, attrs_end, self_closing)) = inline_open_tag(bytes, start, tags) else {
        return InlineComp::NotComponent;
    };
    let name = &input[start + 1..name_end];
    let attrs = sanitize_attrs(&input[start..attrs_end]);

    if self_closing {
        write_inline_component(name, &attrs, "", opts, out);
        return InlineComp::Done(attrs_end);
    }
    match find_inline_close(bytes, attrs_end, name.as_bytes()) {
        Some(close_start) => {
            write_inline_component(name, &attrs, &input[attrs_end..close_start], opts, out);
            // Advance past `</name>`.
            InlineComp::Done(close_start + 2 + name.len() + 1)
        }
        None => InlineComp::Incomplete,
    }
}

/// Emit `<name attrs>inner</name>`, with `inner` rendered as inline markdown.
fn write_inline_component(
    name: &str,
    attrs: &[(String, String)],
    inner: &str,
    opts: &RenderOpts,
    out: &mut String,
) {
    out.push('<');
    out.push_str(name);
    for (k, v) in attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        escape_attr(v, out);
        out.push('"');
    }
    out.push('>');
    if !inner.is_empty() {
        render_inline_core(inner, opts, out, false);
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

/// If an allowlisted inline-component OPEN tag starts at `start` (`bytes[start]`
/// is `<`, not `</`), return `(name_end, attrs_end, self_closing)` — `name_end`
/// just past the tag name, `attrs_end` just past the closing `>`. Tolerates
/// quoted attribute values containing `>`. `None` if it isn't an allowlisted
/// open tag, or the tag is not yet complete (no `>`), or it is malformed.
fn inline_open_tag(bytes: &[u8], start: usize, tags: &[Box<str>]) -> Option<(usize, usize, bool)> {
    if bytes.get(start) != Some(&b'<') {
        return None;
    }
    let name_start = start + 1;
    let mut i = name_start;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
        i += 1;
    }
    if i == name_start || !tags.iter().any(|t| t.as_bytes() == &bytes[name_start..i]) {
        return None;
    }
    let name_end = i;
    let mut in_quote = 0u8;
    while i < bytes.len() {
        let c = bytes[i];
        if in_quote != 0 {
            if c == in_quote {
                in_quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            in_quote = c;
        } else if c == b'<' {
            return None; // a new `<` before this tag closed — malformed
        } else if c == b'>' {
            let mut k = i;
            while k > name_end && matches!(bytes[k - 1], b' ' | b'\t') {
                k -= 1;
            }
            let self_closing = bytes[k - 1] == b'/';
            return Some((name_end, i + 1, self_closing));
        }
        i += 1;
    }
    None
}

/// Length of the backtick run starting at `i`.
fn backtick_run_len(bytes: &[u8], i: usize) -> usize {
    let mut n = 0;
    while i + n < bytes.len() && bytes[i + n] == b'`' {
        n += 1;
    }
    n
}

/// Find the close of a code span opened by a run of `run` backticks: the next
/// run of EXACTLY `run` backticks. Returns the offset just past it, or `None`
/// (an unclosed run — its backticks are literal).
fn matching_backtick_close(bytes: &[u8], from: usize, run: usize) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let r = backtick_run_len(bytes, i);
            if r == run {
                return Some(i + r);
            }
            i += r;
        } else {
            i += 1;
        }
    }
    None
}

/// Is `bytes[i..]` exactly `</name>`?
fn is_inline_close_tag(bytes: &[u8], i: usize, name: &[u8]) -> bool {
    bytes.get(i) == Some(&b'<')
        && bytes.get(i + 1) == Some(&b'/')
        && bytes.len() >= i + 2 + name.len() + 1
        && &bytes[i + 2..i + 2 + name.len()] == name
        && bytes[i + 2 + name.len()] == b'>'
}

/// Is `bytes[i..]` a same-`name` non-self-closing open tag `<name …>`? Returns
/// the offset just past its `>` (so the close-finder can count nesting depth).
fn inline_open_same(bytes: &[u8], i: usize, name: &[u8]) -> Option<usize> {
    if bytes.get(i) != Some(&b'<') || bytes.get(i + 1) == Some(&b'/') {
        return None;
    }
    let ns = i + 1;
    if bytes.len() < ns + name.len() || &bytes[ns..ns + name.len()] != name {
        return None;
    }
    // The name must end here (not be a prefix of a longer tag name).
    if matches!(bytes.get(ns + name.len()), Some(c) if c.is_ascii_alphanumeric() || *c == b'-') {
        return None;
    }
    let mut j = ns + name.len();
    let mut in_quote = 0u8;
    while j < bytes.len() {
        let c = bytes[j];
        if in_quote != 0 {
            if c == in_quote {
                in_quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            in_quote = c;
        } else if c == b'>' {
            let mut k = j;
            while k > ns + name.len() && matches!(bytes[k - 1], b' ' | b'\t') {
                k -= 1;
            }
            // A self-closing `<name/>` opens no new nesting level.
            return if bytes[k - 1] == b'/' { None } else { Some(j + 1) };
        }
        j += 1;
    }
    None
}

/// Locate the matching `</name>` close for an inline component opened at the
/// caller, scanning from `from`. Tracks same-tag nesting (`<name …>` opens) and
/// skips inline code spans (a `</name>` inside backticks is content). Returns
/// the byte offset of the `<` of the matching close, or `None` if it has not
/// arrived yet (the component is then incomplete).
fn find_inline_close(bytes: &[u8], from: usize, name: &[u8]) -> Option<usize> {
    let mut i = from;
    let mut depth = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'`' => {
                let run = backtick_run_len(bytes, i);
                i = matching_backtick_close(bytes, i + run, run).unwrap_or(i + run);
            }
            b'<' => {
                if is_inline_close_tag(bytes, i, name) {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                    i += 2 + name.len() + 1;
                } else if let Some(open_end) = inline_open_same(bytes, i, name) {
                    depth += 1;
                    i = open_end;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    None
}

// ---------------------------------------------------------------------
// Links + images
// ---------------------------------------------------------------------

/// GFM footnote reference `[^label]` → superscript link, if the label has been
/// numbered (i.e. it appears in `opts.footnotes`). Returns the position after
/// the closing `]`. Definitions (`[^x]:`) are handled at block level, not here.
fn try_footnote_ref(bytes: &[u8], start: usize, opts: &RenderOpts, out: &mut String) -> Option<usize> {
    if bytes.get(start + 1) != Some(&b'^') {
        return None;
    }
    let mut j = start + 2;
    while j < bytes.len() && bytes[j] != b']' {
        if bytes[j] == b'[' || bytes[j].is_ascii_whitespace() {
            return None;
        }
        j += 1;
    }
    if j >= bytes.len() || j == start + 2 {
        return None;
    }
    let label = std::str::from_utf8(&bytes[start + 2..j]).ok()?;
    let num = *opts.footnotes.get(label)?;
    let n = num.to_string();
    // Occurrence index for this label (0-based). The Kth (K≥1) reference gets a
    // unique id `fnref-N-(K+1)` so repeated references don't collide.
    let occurrence = {
        let mut occ = opts.footnote_occ.borrow_mut();
        let c = occ.entry(label.to_string()).or_insert(0);
        let k = *c;
        *c += 1;
        k
    };
    let id = if occurrence == 0 { n.clone() } else { format!("{n}-{}", occurrence + 1) };
    out.push_str("<sup class=\"footnote-ref\"><a href=\"#fn-");
    out.push_str(&n);
    out.push_str("\" id=\"fnref-");
    out.push_str(&id);
    out.push_str("\">");
    out.push_str(&n);
    out.push_str("</a></sup>");
    Some(j + 1)
}

fn try_link(bytes: &[u8], start: usize, opts: &RenderOpts, out: &mut String) -> Option<usize> {
    let (text_range, after_text) = read_balanced_brackets(bytes, start)?;

    // §6.6: links may not contain other links. CommonMark resolves brackets
    // inner-first, so a nested link inside our text means *this* (outer) link
    // is not formed — the bracket becomes literal and render_inline reprocesses
    // it, letting the inner link win. (Images in link text are fine.)
    if text_has_nested_link(bytes, text_range.clone(), opts) {
        return None;
    }

    if bytes.get(after_text) == Some(&b'(') {
        if let Some((url, title, after_paren)) = read_link_destination(bytes, after_text + 1) {
            return Some(write_link(bytes, &text_range, &url, title.as_deref(), opts, out, after_paren));
        }
    }
    let (label_range_opt, end_pos) = read_optional_ref_label(bytes, after_text);
    let label_bytes = match label_range_opt {
        Some(r) if !r.is_empty() => &bytes[r],
        _ => &bytes[text_range.clone()],
    };
    let label = std::str::from_utf8(label_bytes).ok()?;
    if !crate::render::valid_link_label(label) {
        return None;
    }
    let r = opts.lookup(label)?;
    let url = r.url.clone();
    let title = r.title.clone();
    Some(write_link(bytes, &text_range, &url, title.as_deref(), opts, out, end_pos))
}

/// Does a complete link parse starting at the `[` at `start`? Returns its end
/// offset. Used to detect a nested link inside link text (§6.6).
fn link_parses_at(bytes: &[u8], start: usize, opts: &RenderOpts) -> Option<usize> {
    let (text_range, after_text) = read_balanced_brackets(bytes, start)?;
    if bytes.get(after_text) == Some(&b'(') {
        if let Some((_, _, after_paren)) = read_link_destination(bytes, after_text + 1) {
            return Some(after_paren);
        }
    }
    let (label_range_opt, end_pos) = read_optional_ref_label(bytes, after_text);
    let label_bytes = match label_range_opt {
        Some(r) if !r.is_empty() => &bytes[r],
        _ => &bytes[text_range.clone()],
    };
    let label = std::str::from_utf8(label_bytes).ok()?;
    opts.lookup(label).map(|_| end_pos)
}

/// Scan a link-text byte range for a nested link (not an image). Code spans,
/// autolinks/HTML, and images are skipped so their internal brackets don't
/// count.
fn text_has_nested_link(
    bytes: &[u8],
    range: core::ops::Range<usize>,
    opts: &RenderOpts,
) -> bool {
    let mut i = range.start;
    while i < range.end {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'`' => i = skip_code_span(bytes, i).unwrap_or(i + 1),
            b'<' => i = skip_autolink_or_html(bytes, i).unwrap_or(i + 1),
            b'!' if bytes.get(i + 1) == Some(&b'[') => {
                // An image: skip its (balanced) alt so links inside the alt
                // don't invalidate the outer link.
                match read_balanced_brackets(bytes, i + 1) {
                    Some((_, after)) => i = after,
                    None => i += 2,
                }
            }
            b'[' => {
                if link_parses_at(bytes, i, opts).is_some() {
                    return true;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    false
}

fn write_link(
    bytes: &[u8],
    text_range: &core::ops::Range<usize>,
    url: &str,
    title: Option<&str>,
    opts: &RenderOpts,
    out: &mut String,
    end_pos: usize,
) -> usize {
    out.push_str("<a href=\"");
    sanitize_url(url, out, false);
    out.push('"');
    if let Some(t) = title {
        out.push_str(" title=\"");
        let decoded = crate::url::decode_text(t);
        escape_attr(&decoded, out);
        out.push('"');
    }
    out.push_str(" target=\"_blank\" rel=\"noopener noreferrer nofollow\">");
    let text = std::str::from_utf8(&bytes[text_range.clone()]).unwrap_or("");
    // CommonMark §6.6: links can't contain nested links.
    let mut inner_opts = opts.clone();
    inner_opts.in_link = true;
    render_inline(text, &inner_opts, out);
    out.push_str("</a>");
    end_pos
}

fn try_image(bytes: &[u8], start: usize, opts: &RenderOpts, out: &mut String) -> Option<usize> {
    let (alt_range, after_alt) = read_balanced_brackets(bytes, start + 1)?;
    if bytes.get(after_alt) == Some(&b'(') {
        if let Some((url, title, after_paren)) = read_link_destination(bytes, after_alt + 1) {
            return Some(write_image(bytes, &alt_range, &url, title.as_deref(), opts, out, after_paren));
        }
    }
    let (label_range_opt, end_pos) = read_optional_ref_label(bytes, after_alt);
    let label_bytes = match label_range_opt {
        Some(r) if !r.is_empty() => &bytes[r],
        _ => &bytes[alt_range.clone()],
    };
    let label = std::str::from_utf8(label_bytes).ok()?;
    if !crate::render::valid_link_label(label) {
        return None;
    }
    let r = opts.lookup(label)?;
    let url = r.url.clone();
    let title = r.title.clone();
    Some(write_image(bytes, &alt_range, &url, title.as_deref(), opts, out, end_pos))
}

fn write_image(
    bytes: &[u8],
    alt_range: &core::ops::Range<usize>,
    url: &str,
    title: Option<&str>,
    _opts: &RenderOpts,
    out: &mut String,
    end_pos: usize,
) -> usize {
    out.push_str("<img src=\"");
    sanitize_image_url(url, out);
    out.push_str("\" alt=\"");
    let alt_text = std::str::from_utf8(&bytes[alt_range.clone()]).unwrap_or("");
    let mut tmp = String::new();
    let opts_plain = RenderOpts::default();
    render_inline(alt_text, &opts_plain, &mut tmp);
    // `tmp` is already HTML-escaped inline output; flatten it to the alt text
    // (nested images contribute their own alt) and emit it verbatim — escaping
    // again would double-encode entities.
    out.push_str(&flatten_alt(&tmp));
    out.push('"');
    if let Some(t) = title {
        out.push_str(" title=\"");
        let decoded = crate::url::decode_text(t);
        escape_attr(&decoded, out);
        out.push('"');
    }
    out.push('>');
    end_pos
}

/// Reduce rendered inline HTML to image-alt text (§6.4): drop tags, but lift
/// the `alt` attribute of any nested `<img>`. Input is already HTML-escaped,
/// so the result is emitted verbatim. UTF-8 safe (operates on `&str` slices).
fn flatten_alt(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after = &rest[lt..];
        let gt = after.find('>').map(|g| g + 1).unwrap_or(after.len());
        let tag = &after[..gt];
        if tag.starts_with("<img") {
            if let Some(alt) = extract_attr(tag, "alt") {
                out.push_str(alt);
            }
        }
        rest = &after[gt..];
    }
    out.push_str(rest);
    out
}

/// Extract the value of `name="..."` from an HTML start tag.
fn extract_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("{}=\"", name);
    let start = tag.find(&pat)? + pat.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

fn read_balanced_brackets(bytes: &[u8], start: usize) -> Option<(core::ops::Range<usize>, usize)> {
    if bytes.get(start) != Some(&b'[') {
        return None;
    }
    let mut depth = 1;
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'`' => {
                // Code span takes precedence over link brackets — if a code
                // span eats past our `]`, the link parse fails.
                if let Some(after) = skip_code_span(bytes, i) {
                    i = after;
                } else {
                    i += 1;
                }
            }
            b'<' => {
                if let Some(after) = skip_autolink_or_html(bytes, i) {
                    i = after;
                } else {
                    i += 1;
                }
            }
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some((start + 1..i, i + 1));
                }
                i += 1;
            }
            b'\n' if depth > 0 => i += 1,
            _ => i += 1,
        }
    }
    None
}

/// If bytes starting at `start` form a complete code span, return the byte
/// offset just after the closing backtick run. Otherwise None.
fn skip_code_span(bytes: &[u8], start: usize) -> Option<usize> {
    let mut open_len = 0;
    while start + open_len < bytes.len() && bytes[start + open_len] == b'`' {
        open_len += 1;
    }
    if open_len == 0 {
        return None;
    }
    let mut i = start + open_len;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let mut close_len = 0;
            while i + close_len < bytes.len() && bytes[i + close_len] == b'`' {
                close_len += 1;
            }
            if close_len == open_len {
                return Some(i + close_len);
            }
            i += close_len;
        } else {
            i += 1;
        }
    }
    None
}

/// Same idea for inline HTML / autolinks: returns end position if matched.
fn skip_autolink_or_html(bytes: &[u8], start: usize) -> Option<usize> {
    // Autolink first: <scheme:...>
    if let Some(end_rel) = bytes[start..].iter().position(|&b| b == b'>') {
        let end = start + end_rel;
        let inner = &bytes[start + 1..end];
        if !inner.is_empty()
            && !inner.iter().any(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'<' | b'\0'))
        {
            if let Ok(s) = std::str::from_utf8(inner) {
                if is_uri_scheme(s) || (is_email_like(s) && !s.contains('\\')) {
                    return Some(end + 1);
                }
            }
        }
    }
    // Otherwise inline HTML
    match_inline_html(bytes, start).map(|n| start + n - start + start)
        .and_then(|_| match_inline_html(bytes, start).map(|n| start + n))
}

/// After `[text]`, look for an optional `[label]` reference. Returns
/// `(Some(label_range), pos_after)` for `[text][label]`,
/// `(Some(empty), pos_after)` for `[text][]`,
/// `(None, after_text)` for `[text]` (collapsed reference).
fn read_optional_ref_label(
    bytes: &[u8],
    after_text: usize,
) -> (Option<core::ops::Range<usize>>, usize) {
    if bytes.get(after_text) == Some(&b'[') {
        if let Some((r, end)) = read_balanced_brackets(bytes, after_text) {
            return (Some(r), end);
        }
    }
    (None, after_text)
}

fn read_link_destination(
    bytes: &[u8],
    start: usize,
) -> Option<(String, Option<String>, usize)> {
    let mut i = start;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n') {
        i += 1;
    }
    // Empty URL: immediate `)` after optional whitespace.
    if bytes.get(i) == Some(&b')') {
        return Some((String::new(), None, i + 1));
    }
    let (url, after_url) = if bytes.get(i) == Some(&b'<') {
        // Bracketed URL: <...> with no newlines, no unescaped < or >.
        let mut j = i + 1;
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
        let u = std::str::from_utf8(&bytes[i + 1..j]).ok()?.to_string();
        (u, j + 1)
    } else {
        let mut depth = 0;
        let mut j = i;
        while j < bytes.len() {
            let b = bytes[j];
            if matches!(b, b' ' | b'\t' | b'\n') {
                break;
            }
            if b == b'\\' && j + 1 < bytes.len() {
                j += 2;
                continue;
            }
            if b == b'(' {
                depth += 1;
                j += 1;
                continue;
            }
            if b == b')' {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                j += 1;
                continue;
            }
            // Control chars not allowed in bare destinations.
            if b < 0x20 {
                return None;
            }
            j += 1;
        }
        if j == i {
            // Bare empty destination is only valid via bracketed form.
            return Some((String::new(), None, j));
        }
        let u = std::str::from_utf8(&bytes[i..j]).ok()?.to_string();
        (u, j)
    };
    let mut i = after_url;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n') {
        i += 1;
    }
    let mut title: Option<String> = None;
    if i < bytes.len() && matches!(bytes[i], b'"' | b'\'' | b'(') {
        let close = match bytes[i] {
            b'"' => b'"',
            b'\'' => b'\'',
            _ => b')',
        };
        let start_t = i + 1;
        let mut j = start_t;
        let mut had_blank = false;
        let mut prev_was_nl = false;
        while j < bytes.len() && bytes[j] != close {
            if bytes[j] == b'\\' && j + 1 < bytes.len() {
                j += 2;
                prev_was_nl = false;
                continue;
            }
            if bytes[j] == b'\n' {
                if prev_was_nl {
                    had_blank = true;
                    break;
                }
                prev_was_nl = true;
            } else if !matches!(bytes[j], b' ' | b'\t' | b'\r') {
                prev_was_nl = false;
            }
            j += 1;
        }
        if !had_blank && j < bytes.len() {
            title = Some(std::str::from_utf8(&bytes[start_t..j]).ok()?.to_string());
            i = j + 1;
        }
    }
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n') {
        i += 1;
    }
    if bytes.get(i) != Some(&b')') {
        return None;
    }
    Some((url, title, i + 1))
}

// ---------------------------------------------------------------------
// Emphasis / strong / strikethrough — delimiter stack
// ---------------------------------------------------------------------

#[derive(Debug)]
struct Delim {
    at: usize,
    /// Byte offset of this delimiter run's start in the *input* (not the output
    /// `at`). Set at push, never modified by resolution — used by the streaming
    /// boundary computation to locate emphasis spans in the source.
    in_at: usize,
    class: u8,
    len: usize,
    can_open: bool,
    can_close: bool,
}

struct DelimRun {
    len: usize,
}

fn scan_delim_run(bytes: &[u8], start: usize) -> DelimRun {
    let c = bytes[start];
    let mut len = 0;
    while start + len < bytes.len() && bytes[start + len] == c {
        len += 1;
    }
    DelimRun { len }
}

/// CommonMark §6.2 flanking rules — operates on Unicode chars, not bytes,
/// so intra-word `_` in Cyrillic and similar work correctly.
fn flanking(input: &str, start: usize, len: usize) -> (bool, bool) {
    let prev = prev_char(input, start);
    let next = if start + len < input.len() {
        input[start + len..].chars().next()
    } else {
        None
    };
    let prev_ws = prev.map_or(true, |c| c.is_whitespace());
    let next_ws = next.map_or(true, |c| c.is_whitespace());
    let prev_punct = prev.map_or(false, is_unicode_punct);
    let next_punct = next.map_or(false, is_unicode_punct);
    let left_flank = !next_ws && (!next_punct || prev_ws || prev_punct);
    let right_flank = !prev_ws && (!prev_punct || next_ws || next_punct);
    let c = input.as_bytes()[start];
    if c == b'_' {
        let prev_alnum = prev.map_or(false, |c| c.is_alphanumeric());
        let next_alnum = next.map_or(false, |c| c.is_alphanumeric());
        (
            left_flank && (!right_flank || prev_punct) && !prev_alnum,
            right_flank && (!left_flank || next_punct) && !next_alnum,
        )
    } else {
        (left_flank, right_flank)
    }
}

fn prev_char(input: &str, pos: usize) -> Option<char> {
    if pos == 0 {
        return None;
    }
    input[..pos].chars().next_back()
}

/// Unicode punctuation: ASCII puncts plus any non-alphanumeric, non-whitespace
/// char. This catches the symbol/punctuation categories without needing a
/// full Unicode property table.
fn is_unicode_punct(c: char) -> bool {
    if matches!(c, '!'..='/' | ':'..='@' | '['..='`' | '{'..='~') {
        return true;
    }
    !c.is_alphanumeric() && !c.is_whitespace()
}

/// Resolve the emphasis delimiter stack into `<em>/<strong>/<del>` edits on
/// `out`. When `pairs` is `Some`, also records each pairing as an input-position
/// span `(opener_run_start, closer_run_start)` — the streaming boundary
/// computation uses these to avoid cutting inside a resolved emphasis span.
fn resolve_delimiters(out: &mut String, stack: &mut Vec<Delim>, mut pairs: Option<&mut Vec<(usize, usize)>>) {
    let mut edits: Vec<Edit> = Vec::new();

    let n = stack.len();
    let mut i = 0;
    while i < n {
        if !stack[i].can_close {
            i += 1;
            continue;
        }
        let mut j = i;
        let found = loop {
            if j == 0 {
                break None;
            }
            j -= 1;
            let s = &stack[j];
            if s.len == 0 {
                continue;
            }
            if s.class != stack[i].class {
                continue;
            }
            if !s.can_open {
                continue;
            }
            let sum_mod = (s.len + stack[i].len) % 3;
            if (s.can_close || stack[i].can_open) && sum_mod == 0 && !(s.len % 3 == 0 && stack[i].len % 3 == 0) {
                continue;
            }
            break Some(j);
        };

        if let Some(opener_idx) = found {
            let class = stack[i].class;
            let take = if class == b'~' {
                if stack[opener_idx].len >= 2 && stack[i].len >= 2 { 2 } else { 0 }
            } else if stack[opener_idx].len >= 2 && stack[i].len >= 2 {
                2
            } else {
                1
            };
            if take == 0 {
                i += 1;
                continue;
            }
            let (open_tag, close_tag) = match (class, take) {
                (b'~', 2) => ("<del>", "</del>"),
                (_, 2) => ("<strong>", "</strong>"),
                (_, 1) => ("<em>", "</em>"),
                _ => ("", ""),
            };

            let op_at = stack[opener_idx].at + stack[opener_idx].len - take;
            let cl_at = stack[i].at;
            edits.push(Edit { at: op_at, delete: take, insert: open_tag.to_string() });
            edits.push(Edit { at: cl_at, delete: take, insert: close_tag.to_string() });
            if let Some(p) = pairs.as_deref_mut() {
                p.push((stack[opener_idx].in_at, stack[i].in_at));
            }

            stack[opener_idx].len -= take;
            stack[i].len -= take;
            // The closer's remaining chars are to the RIGHT of what we just
            // consumed; the opener's remaining chars are to the LEFT of what
            // we consumed (its .at stays put).
            stack[i].at += take;
            if stack[i].len == 0 {
                i += 1;
            }
            for k in opener_idx + 1..i {
                stack[k].len = 0;
            }
        } else {
            i += 1;
        }
    }

    edits.sort_by(|a, b| b.at.cmp(&a.at));
    for e in edits {
        out.replace_range(e.at..e.at + e.delete, &e.insert);
    }
}

struct Edit {
    at: usize,
    delete: usize,
    insert: String,
}

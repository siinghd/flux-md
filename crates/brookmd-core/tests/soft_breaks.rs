//! `soft_breaks` — render a CommonMark SOFT line break (a bare `\n` inside
//! inline content) as `<br>`, the "GitHub comment"
//! convention where one Enter is one visual line. Opt-in; strict CommonMark
//! (the default) keeps a soft break as literal whitespace.
//!
//! The flag only ever ADDS breaks: hard breaks (two trailing spaces, trailing
//! `\`) are already `<br>` in both modes, and code — fenced, indented, or an
//! inline span — is never touched.

use brook_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn render_br(md: &str) -> String {
    let mut p = StreamParser::new().with_soft_breaks(true);
    p.append(md);
    p.finalize();
    collect(&p)
}

fn render_br_streamed(md: &str) -> String {
    let mut p = StreamParser::new().with_soft_breaks(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

fn render_plain(md: &str) -> String {
    let mut p = StreamParser::new();
    p.append(md);
    p.finalize();
    collect(&p)
}

// ── 1. off by default ────────────────────────────────────────────────────────

#[test]
fn off_by_default_keeps_literal_newline() {
    // Strict CommonMark: a soft break is whitespace, emitted as the newline.
    assert_eq!(render_plain("a\nb\n"), "<p>a\nb</p>");
    assert!(!render_plain("a\nb\n").contains("<br>"));
    // Several soft breaks in one paragraph — still no <br> anywhere.
    let three = render_plain("one\ntwo\nthree\n");
    assert_eq!(three, "<p>one\ntwo\nthree</p>");
    assert!(!three.contains("<br>"));
}

// ── 2. on: soft break becomes <br> ───────────────────────────────────────────

#[test]
fn on_renders_soft_break_as_br() {
    assert_eq!(render_br("a\nb\n"), "<p>a<br>\nb</p>");
    assert_eq!(render_br("one\ntwo\nthree\n"), "<p>one<br>\ntwo<br>\nthree</p>");
    // Inline markup spanning the break still resolves normally.
    assert_eq!(render_br("*a\nb*\n"), "<p><em>a<br>\nb</em></p>");
}

// ── 3. on: hard breaks stay exactly one <br> ─────────────────────────────────

#[test]
fn hard_break_is_not_doubled() {
    // Two trailing spaces: already a hard break. Turning the flag on must not
    // emit a second <br>, and must not leave the stripped spaces behind.
    let two_space = render_br("a  \nb\n");
    assert_eq!(two_space, "<p>a<br>\nb</p>");
    assert_eq!(two_space.matches("<br>").count(), 1);
    // Identical to the off-mode rendering of the same hard break.
    assert_eq!(two_space, render_plain("a  \nb\n"));

    // More than two trailing spaces, and the backslash form.
    assert_eq!(render_br("a     \nb\n"), "<p>a<br>\nb</p>");
    let backslash = render_br("a\\\nb\n");
    assert_eq!(backslash, "<p>a<br>\nb</p>");
    assert_eq!(backslash.matches("<br>").count(), 1);
    assert_eq!(backslash, render_plain("a\\\nb\n"));
}

// ── 4. nested parsers inherit the flag ───────────────────────────────────────

#[test]
fn soft_break_inside_list_item() {
    // A sub-block inside a list item must render the break the same way a
    // top-level paragraph does — proves the nested-parser flag propagation.
    let tight = render_br("- a\n  b\n");
    assert!(tight.contains("<br>"), "list item lost the soft break: {tight}");
    assert_eq!(tight, "<ul>\n<li>a<br>\nb</li>\n</ul>");
    assert!(!render_plain("- a\n  b\n").contains("<br>"));

    // Loose list: the item body is a real <p>, rendered by the sub-parser.
    let loose = render_br("- a\n  b\n\n- c\n");
    assert!(loose.contains("<br>"), "loose list item lost the break: {loose}");
}

#[test]
fn soft_break_inside_blockquote() {
    let bq = render_br("> a\n> b\n");
    assert!(bq.contains("<br>"), "blockquote lost the soft break: {bq}");
    assert_eq!(bq, "<blockquote>\n<p>a<br>\nb</p>\n</blockquote>");
    assert!(!render_plain("> a\n> b\n").contains("<br>"));

    // Nested one level deeper still inherits.
    let deep = render_br("> > a\n> > b\n");
    assert!(deep.contains("<br>"), "nested blockquote lost the break: {deep}");
}

// ── 5. code is never touched ─────────────────────────────────────────────────

#[test]
fn code_newlines_are_not_converted() {
    // Fenced code: newlines are literal content, never <br>.
    let fence = render_br("```\na\nb\n```\n");
    assert!(!fence.contains("<br>"), "fenced code must keep literal newlines: {fence}");
    assert_eq!(fence, render_plain("```\na\nb\n```\n"));

    // Indented code: same.
    let indented = render_br("    a\n    b\n");
    assert!(!indented.contains("<br>"), "indented code must keep newlines: {indented}");
    assert_eq!(indented, render_plain("    a\n    b\n"));

    // Inline code span: CommonMark normalizes an interior newline to a space.
    let span = render_br("`a\nb`\n");
    assert!(!span.contains("<br>"), "code span must not break: {span}");
    assert_eq!(span, "<p><code>a b</code></p>");
    assert_eq!(span, render_plain("`a\nb`\n"));
}

// ── 6. tables and headings behave sanely ─────────────────────────────────────

#[test]
fn tables_and_headings_are_sane() {
    // Row/cell newlines are table STRUCTURE, not inline soft breaks — the
    // rendered table must be byte-identical to the default rendering.
    let md = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let table = render_br(md);
    assert!(!table.contains("<br>"), "table structure must not break: {table}");
    assert_eq!(table, render_plain(md));

    // An ATX heading is single-line: nothing to break.
    assert_eq!(render_br("# title\n"), render_plain("# title\n"));
    assert!(!render_br("# title\n").contains("<br>"));

    // A setext heading's underline is structure, not a soft break; its multi-
    // line text collapses into the heading itself.
    let setext = render_br("a\nb\n===\n");
    assert!(setext.contains("<h1>"), "expected setext heading: {setext}");
}

// ── 7. streaming parity ──────────────────────────────────────────────────────

#[test]
fn streaming_matches_one_shot_with_flag_on() {
    // The flag changes the bytes emitted for `\n`, so every per-shape tail
    // cache (paragraph / table / container / list) must render through
    // RenderOpts and see it. Feed char-by-char and compare to a one-shot parse.
    for md in [
        "a\nb\n",
        "one\ntwo\nthree\n",
        "a  \nb\nc\n",
        "a\\\nb\n",
        "*a\nb* and `c\nd`\n",
        "- a\n  b\n- c\n  d\n",
        "- a\n  b\n\n- c\n",
        "> a\n> b\n",
        "> > a\n> > b\n",
        "```\na\nb\n```\n",
        "    a\n    b\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
        "# title\na\nb\n",
        "para one\nstill one\n\npara two\nstill two\n",
        "a\nb\n\n> q1\n> q2\n\n- l1\n  l2\n",
    ] {
        assert_eq!(
            render_br_streamed(md),
            render_br(md),
            "stream != one-shot with soft_breaks on for {md:?}"
        );
    }
}

// ── off-path byte parity ─────────────────────────────────────────────────────

#[test]
fn off_path_is_byte_identical_to_default() {
    // `with_soft_breaks(false)` must produce exactly the default bytes — the
    // flag is zero-cost when off.
    for md in [
        "a\nb\n",
        "a  \nb\n",
        "- a\n  b\n",
        "> a\n> b\n",
        "```\na\nb\n```\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
    ] {
        let mut p = StreamParser::new().with_soft_breaks(false);
        p.append(md);
        p.finalize();
        assert_eq!(collect(&p), render_plain(md), "off path differs for {md:?}");
    }
}

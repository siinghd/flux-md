//! Opt-in INLINE component tags: an allowlisted `<tik>…</tik>` (or self-closing
//! `<tik/>`) anywhere in inline content — paragraphs, table cells, headings,
//! list items — renders as a real custom element with markdown inner content and
//! sanitized attributes, so a JSX/DOM layer can dispatch it via `components[tag]`.
//! Covers the risky cases: attribute sanitization, self-closing, same-tag
//! nesting, a `</tik>` inside a code span (must not close), inert degradation of
//! an unclosed tag (must never eat following content), streaming convergence and
//! the no-orphan invariant, and that the feature is off unless configured.

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn render(md: &str, inline_tags: &[&str]) -> String {
    let mut p = StreamParser::new()
        .with_gfm_autolinks(true)
        .with_inline_component_tags(inline_tags.iter().map(|s| s.to_string()).collect());
    p.append(md);
    p.finalize();
    collect(&p)
}

fn render_streamed(md: &str, inline_tags: &[&str]) -> String {
    let mut p = StreamParser::new()
        .with_gfm_autolinks(true)
        .with_inline_component_tags(inline_tags.iter().map(|s| s.to_string()).collect());
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

#[test]
fn dispatches_in_paragraph_with_markdown_inner() {
    let out = render("Apple <tik symbol=\"AAPL\">**A**</tik> rose today.\n", &["tik"]);
    assert!(out.contains("<tik symbol=\"AAPL\"><strong>A</strong></tik>"), "got: {out}");
    assert!(out.contains("Apple ") && out.contains(" rose today."), "surrounding text kept: {out}");
    assert!(!out.contains("&lt;tik"), "the tag must not be escaped: {out}");
}

#[test]
fn dispatches_inside_table_cell() {
    // The must-have: inline components inside GFM table cells.
    let md = "| Stock | Note |\n| --- | --- |\n| <tik>AAPL</tik> | up |\n";
    let out = render(md, &["tik"]);
    assert!(out.contains("<table>"), "table renders: {out}");
    assert!(out.contains("<tik>AAPL</tik>"), "inline component inside the cell: {out}");
    assert!(out.contains("<td>up</td>") || out.contains("up"), "other cells intact: {out}");
}

#[test]
fn attributes_are_sanitized() {
    let out = render(
        "x <tik type=\"warn\" onclick=\"steal()\" href=\"javascript:alert(1)\">hi</tik> y\n",
        &["tik"],
    );
    assert!(out.contains("type=\"warn\""), "safe attr kept: {out}");
    assert!(!out.to_lowercase().contains("onclick"), "event handler dropped: {out}");
    assert!(!out.contains("javascript:"), "dangerous scheme neutralized: {out}");
    assert!(out.contains("href=\"#\""), "dangerous href becomes #: {out}");
}

#[test]
fn self_closing_inline_component() {
    let out = render("Buy <tik symbol=\"AAPL\" /> now.\n", &["tik"]);
    assert!(out.contains("<tik symbol=\"AAPL\"></tik>"), "self-closing → empty element: {out}");
    assert!(out.contains("Buy ") && out.contains(" now."), "surrounding text kept: {out}");
}

#[test]
fn same_tag_nesting() {
    let out = render("<tik>a<tik>b</tik>c</tik>\n", &["tik"]);
    // The OUTER close is the last `</tik>`; the inner pair renders recursively.
    assert_eq!(out.matches("<tik>").count(), 2, "two opens: {out}");
    assert_eq!(out.matches("</tik>").count(), 2, "two closes: {out}");
    assert!(out.contains("<tik>a<tik>b</tik>c</tik>"), "got: {out}");
}

#[test]
fn close_inside_code_span_does_not_close() {
    // A `</tik>` inside a code span is content; the real close is the later one.
    let out = render("<tik>x `</tik>` y</tik>\n", &["tik"]);
    assert_eq!(out.matches("</tik>").count(), 1, "only one real close: {out}");
    assert!(out.contains("<code>&lt;/tik&gt;</code>"), "coded close stays escaped content: {out}");
    assert!(out.contains(" y</tik>"), "body after the coded fake-close is inside: {out}");
}

#[test]
fn unclosed_inline_tag_degrades_inert_and_keeps_following_blocks() {
    // An unclosed `<tik>` must NOT eat the following table — it escapes inertly.
    let md = "<tik>AAPL is up\n\n| a | b |\n| --- | --- |\n| 1 | 2 |\n";
    let out = render(md, &["tik"]);
    assert!(out.contains("<table>"), "following table survives: {out}");
    assert!(out.contains("1") && out.contains("2"), "table cells survive: {out}");
    assert!(out.contains("&lt;tik&gt;"), "unclosed tag degrades to escaped text: {out}");
    assert!(!out.contains("<tik>"), "no half-open component element emitted: {out}");
}

#[test]
fn bare_inline_tag_line_is_not_an_escaped_html_block() {
    // A lone `<tik>…</tik>` on its own line (inline-only tag) must dispatch as an
    // inline component, not get captured + escaped as a type-7 HTML block.
    let out = render("<tik>AAPL</tik>\n", &["tik"]);
    assert!(out.contains("<tik>AAPL</tik>"), "dispatched inline: {out}");
    assert!(!out.contains("<pre>") && !out.contains("&lt;tik&gt;"), "not an escaped HTML block: {out}");
}

#[test]
fn dispatches_in_heading_and_list_item() {
    let out = render("# Title <tik>X</tik>\n", &["tik"]);
    assert!(out.contains("<tik>X</tik>"), "inline component in a heading: {out}");
    let out = render("- item <tik>Y</tik>\n", &["tik"]);
    assert!(out.contains("<tik>Y</tik>"), "inline component in a list item: {out}");
}

#[test]
fn not_recognized_unless_allowlisted() {
    // With no inline allowlist, `<tik>` is escaped raw HTML (feature off).
    let out = render("a <tik>AAPL</tik> b\n", &[]);
    assert!(!out.contains("<tik>"), "no dispatch without allowlist: {out}");
    assert!(out.contains("&lt;tik&gt;"), "escaped raw HTML instead: {out}");
    // A non-allowlisted tag with an allowlist present is also left alone.
    let out = render("a <other>z</other> b\n", &["tik"]);
    assert!(!out.contains("<other>z</other>"), "got: {out}");
}

#[test]
fn streaming_converges_to_one_shot() {
    for md in [
        "Apple <tik symbol=\"AAPL\">**A**</tik> rose and <tik>MSFT</tik> too.\n",
        "| Stock |\n| --- |\n| <tik>AAPL</tik> |\n",
        "<tik>a<tik>b</tik>c</tik> and `<tik>x</tik>` literal.\n",
        "text before\n\n<tik>body **md**</tik>\n\ntext after\n",
    ] {
        assert_eq!(render_streamed(md, &["tik"]), render(md, &["tik"]), "diverged: {md:?}");
    }
}

#[test]
fn streaming_has_no_orphan_blocks() {
    let md = "intro and <tik symbol=\"AAPL\">**Apple**</tik> here\nmore\n\nafter\n";
    let mut p = StreamParser::new().with_inline_component_tags(vec!["tik".to_string()]);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
        let blocks: Vec<_> = p.all_blocks().collect();
        let mut last_end = 0usize;
        let mut ids = std::collections::HashSet::new();
        for b in &blocks {
            assert!(b.start >= last_end, "overlap/disorder mid-stream");
            assert!(ids.insert(b.id), "duplicate id mid-stream");
            last_end = b.end;
        }
    }
    p.finalize();
    assert!(collect(&p).contains("<tik symbol=\"AAPL\"><strong>Apple</strong></tik>"));
}

#[test]
fn feature_off_is_byte_identical_for_raw_html() {
    // Empty inline allowlist must leave inline raw-HTML handling untouched.
    let with_empty = render("a <b>x</b> & <tik>y</tik>\n", &[]);
    let mut p = StreamParser::new().with_gfm_autolinks(true);
    p.append("a <b>x</b> & <tik>y</tik>\n");
    p.finalize();
    assert_eq!(with_empty, collect(&p), "empty allowlist must be byte-identical");
}

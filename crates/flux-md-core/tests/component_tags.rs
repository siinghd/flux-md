//! Opt-in component tags: `<Thinking>…**markdown**…</Thinking>` renders as a
//! component whose inner content is markdown, safely and without unsafe_html.
//! Covers the design's risky cases: blank lines inside, self-closing, attribute
//! sanitization, same-tag and cross-tag nesting, close-safety inside code
//! spans/fences, streaming convergence, the no-orphan-block flip, and that the
//! feature is fully off unless configured.

use flux_md_core::{BlockKind, StreamParser};

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn render(md: &str, tags: &[&str]) -> String {
    let mut p = StreamParser::new()
        .with_gfm_autolinks(true)
        .with_component_tags(tags.iter().map(|s| s.to_string()).collect());
    p.append(md);
    p.finalize();
    collect(&p)
}

fn render_streamed(md: &str, tags: &[&str]) -> String {
    let mut p = StreamParser::new()
        .with_gfm_autolinks(true)
        .with_component_tags(tags.iter().map(|s| s.to_string()).collect());
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

#[test]
fn renders_markdown_inside_component() {
    let out = render("<Thinking>\nThis is **bold** and a [link](http://x.com).\n</Thinking>\n", &["Thinking"]);
    assert!(out.contains("<Thinking>"), "got: {out}");
    assert!(out.contains("<strong>bold</strong>"), "inner markdown must render: {out}");
    assert!(out.contains("href=\"http://x.com\"") && out.contains(">link</a>"), "inner link must render: {out}");
    assert!(out.contains("</Thinking>"), "got: {out}");
    assert!(!out.contains("&lt;Thinking&gt;"), "tag must not be escaped: {out}");
}

#[test]
fn blank_lines_inside_stay_one_component() {
    // A blank line inside must NOT split the component (unlike a raw type-7 tag).
    let out = render("<Thinking>\n\nFirst para.\n\nSecond **para**.\n\n</Thinking>\n", &["Thinking"]);
    assert_eq!(out.matches("<Thinking>").count(), 1, "one open tag: {out}");
    assert_eq!(out.matches("</Thinking>").count(), 1, "one close tag: {out}");
    assert!(out.contains("<p>First para.</p>"), "got: {out}");
    assert!(out.contains("<p>Second <strong>para</strong>.</p>"), "got: {out}");
}

#[test]
fn self_closing_component() {
    let out = render("<Callout type=\"info\" />\n\nAfter **md**.\n", &["Callout"]);
    assert!(out.contains("<Callout type=\"info\"></Callout>") || out.contains("<Callout type=\"info\">"), "got: {out}");
    assert!(out.contains("<p>After <strong>md</strong>.</p>"), "markdown after a self-closing tag renders: {out}");
}

#[test]
fn attributes_are_sanitized() {
    let out = render(
        "<Callout type=\"warn\" onclick=\"steal()\" href=\"javascript:alert(1)\">\nhi\n</Callout>\n",
        &["Callout"],
    );
    assert!(out.contains("type=\"warn\""), "safe attr kept: {out}");
    assert!(!out.to_lowercase().contains("onclick"), "event handler dropped: {out}");
    assert!(!out.contains("javascript:"), "dangerous scheme neutralized: {out}");
    assert!(out.contains("href=\"#\""), "dangerous href becomes #: {out}");
}

#[test]
fn nesting_same_and_cross_tag() {
    // Same-tag nesting: inner </Thinking> must not close the outer prematurely.
    let out = render("<Thinking>\nouter\n<Thinking>\ninner\n</Thinking>\nstill outer\n</Thinking>\n", &["Thinking"]);
    assert_eq!(out.matches("<Thinking>").count(), 2, "two opens: {out}");
    assert_eq!(out.matches("</Thinking>").count(), 2, "two closes: {out}");
    assert!(out.contains("still outer"), "outer body after inner close kept: {out}");
    // Cross-tag nesting (both allowlisted).
    let out = render("<Thinking>\n<Callout>\nx **y**\n</Callout>\n</Thinking>\n", &["Thinking", "Callout"]);
    assert!(out.contains("<Thinking>") && out.contains("<Callout>"), "got: {out}");
    assert!(out.contains("<strong>y</strong>"), "deeply nested markdown renders: {out}");
}

#[test]
fn close_inside_code_does_not_close() {
    // A </Thinking> inside a fenced code block is content, not the close.
    let out = render("<Thinking>\n```\n</Thinking>\n```\nreal body\n</Thinking>\n", &["Thinking"]);
    assert!(out.contains("real body"), "body after the fenced fake-close must be inside: {out}");
    assert_eq!(out.matches("</Thinking>").count(), 1, "only the real close tag emitted: {out}");
    // The fenced </Thinking> renders as escaped code, not a tag.
    assert!(out.contains("&lt;/Thinking&gt;"), "fenced close is escaped code: {out}");
}

#[test]
fn not_recognized_unless_allowlisted() {
    // With no allowlist, <Thinking> is a plain (escaped) HTML block — feature off.
    let out = render("<Thinking>\n**bold**\n</Thinking>\n", &[]);
    assert!(!out.contains("<strong>bold</strong>"), "markdown must NOT render inside an un-allowlisted tag: {out}");
    // A non-allowlisted tag with an allowlist present is also left alone.
    let out = render("<Other>\n**bold**\n</Other>\n", &["Thinking"]);
    assert!(!out.contains("<strong>bold</strong>"), "got: {out}");
}

#[test]
fn streaming_converges_to_one_shot() {
    for md in [
        "<Thinking>\nstreamed **md** here\nand [a](http://b.co)\n</Thinking>\n",
        "<Thinking>\n\npara one\n\npara two\n\n</Thinking>\n",
        "<Callout type=\"info\">\n<Thinking>\nnested\n</Thinking>\n</Callout>\n",
        "text before\n\n<Thinking>\nbody\n</Thinking>\n\ntext after\n",
    ] {
        assert_eq!(render_streamed(md, &["Thinking", "Callout"]), render(md, &["Thinking", "Callout"]), "diverged: {md:?}");
    }
}

#[test]
fn streaming_open_tag_with_quoted_gt_in_attr() {
    // The open tag's `>` lives inside a quoted attr value; streaming char-by-char
    // must not mistake it for the tag end, must converge, and must never orphan.
    let md = "<Callout title=\"a > b\" type=\"x\">\nbody **md**\n</Callout>\n";
    assert_eq!(render_streamed(md, &["Callout"]), render(md, &["Callout"]), "must converge");
    let mut p = StreamParser::new().with_component_tags(vec!["Callout".to_string()]);
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
    let out = collect(&p);
    assert!(out.contains("title=\"a &gt; b\""), "quoted `>` preserved + escaped: {out}");
    assert!(out.contains("<strong>md</strong>"), "inner markdown renders: {out}");
}

#[test]
fn streaming_has_no_orphan_blocks() {
    let md = "intro\n\n<Thinking>\nbody **one**\nbody two\n</Thinking>\nafter\n";
    let mut p = StreamParser::new().with_component_tags(vec!["Thinking".to_string()]);
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
    let kinds: Vec<_> = p.all_blocks().map(|b| b.kind.tag()).collect();
    assert!(kinds.iter().any(|k| *k == "Component"), "a Component block must exist: {kinds:?}");
    // The component's kind carries its tag.
    let comp = p.all_blocks().find(|b| b.kind.tag() == "Component").unwrap();
    assert!(matches!(&comp.kind, BlockKind::Component { tag, .. } if tag == "Thinking"));
}

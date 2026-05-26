//! Per-block `dir="auto"` for bidirectional text (Streamdown #509). Each block
//! carries its own `dir="auto"` so the browser detects direction independently
//! (Arabic/Hebrew → RTL, English → LTR) — instead of one direction applied to
//! the whole document. Opt-in; code blocks always stay LTR (no `dir`).

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn render_dir(md: &str) -> String {
    let mut p = StreamParser::new().with_dir_auto(true);
    p.append(md);
    p.finalize();
    collect(&p)
}

fn render_dir_streamed(md: &str) -> String {
    let mut p = StreamParser::new().with_dir_auto(true);
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

#[test]
fn dir_auto_on_block_text_elements() {
    assert!(render_dir("hello\n").contains("<p dir=\"auto\">"));
    assert!(render_dir("# Title\n").contains("<h1 dir=\"auto\">"));
    assert!(render_dir("###### Six\n").contains("<h6 dir=\"auto\">"));
    assert!(render_dir("Big\n===\n").contains("<h1 dir=\"auto\">"), "setext heading");
    assert!(render_dir("> quote\n").contains("<blockquote dir=\"auto\">"));
    let ul = render_dir("- a\n- b\n");
    assert!(ul.contains("<ul dir=\"auto\">"), "{ul}");
    assert!(ul.contains("<li dir=\"auto\">"), "{ul}");
    assert!(render_dir("1. a\n2. b\n").contains("<ol dir=\"auto\">"));
    let ol3 = render_dir("3. a\n4. b\n");
    assert!(ol3.contains("<ol dir=\"auto\" start=\"3\">"), "{ol3}");
    assert!(render_dir("| a | b |\n|---|---|\n| 1 | 2 |\n").contains("<table dir=\"auto\">"));
}

#[test]
fn code_blocks_never_get_dir() {
    // Code is universally left-to-right; no dir on <pre>/<code>.
    let fence = render_dir("```rust\nfn main() {}\n```\n");
    assert!(!fence.contains("dir=\"auto\""), "fenced code must stay LTR: {fence}");
    let indented = render_dir("    indented code\n");
    assert!(!indented.contains("dir=\"auto\""), "indented code must stay LTR: {indented}");
}

#[test]
fn off_by_default_is_unchanged() {
    // The default parser emits no dir attribute at all (strict CommonMark).
    for md in [
        "hello\n",
        "# title\n",
        "- a\n- b\n",
        "3. a\n4. b\n",
        "> q\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
    ] {
        assert!(!render_plain(md).contains("dir="), "default must not emit dir for {md:?}");
    }
}

#[test]
fn per_block_direction_in_a_mixed_document() {
    // #509: a document mixing English and Arabic must give EACH block its own
    // dir="auto" — not a single direction for the whole thing.
    let md = "English paragraph here.\n\nمرحبا بالعالم هذا اختبار\n\nBack to English again.\n";
    let out = render_dir(md);
    assert_eq!(
        out.matches("<p dir=\"auto\">").count(),
        3,
        "each paragraph gets its own dir: {out}"
    );
    assert!(out.contains("مرحبا بالعالم هذا اختبار"), "{out}");
}

#[test]
fn dir_auto_streaming_matches_one_shot() {
    for md in [
        "# Title\n\nsome text\n",
        "- one\n- two\n- three\n",
        "> a quote\n> continued\n",
        "مرحبا بالعالم\n\nthen english\n",
        "| h | i |\n|---|---|\n| 1 | 2 |\n",
        "intro\n\n```rust\nfn x() {}\nlet y = 1;\n```\n\noutro\n", // code fence keeps no dir
    ] {
        assert_eq!(render_dir_streamed(md), render_dir(md), "stream≠oneshot: {md:?}");
    }
}

#[test]
fn alert_wrapper_and_title_get_dir() {
    // Alerts default ON in the worker, so the alert × dirAuto intersection is
    // the likely real case: the wrapper div and title both need dir.
    let mut p = StreamParser::new().with_gfm_alerts(true).with_dir_auto(true);
    p.append("> [!NOTE]\n> مرحبا بالعالم\n");
    p.finalize();
    let out = collect(&p);
    assert!(out.contains("role=\"note\" dir=\"auto\">"), "alert wrapper needs dir: {out}");
    assert!(
        out.contains("<p class=\"markdown-alert-title\" dir=\"auto\">"),
        "alert title needs dir: {out}"
    );
    // With dir off, the alert markup is unchanged (no dir anywhere).
    let mut q = StreamParser::new().with_gfm_alerts(true);
    q.append("> [!NOTE]\n> hi\n");
    q.finalize();
    assert!(!collect(&q).contains("dir="), "alert must be unchanged when dir off");
}

#[test]
fn footnote_section_gets_dir() {
    let mut p = StreamParser::new().with_gfm_footnotes(true).with_dir_auto(true);
    p.append("Text with a note[^1].\n\n[^1]: مرحبا\n");
    p.finalize();
    let out = collect(&p);
    assert!(out.contains("<ol dir=\"auto\">"), "footnote list needs dir: {out}");
    assert!(out.contains("<li id=\"fn-1\" dir=\"auto\">"), "footnote item needs dir: {out}");
}

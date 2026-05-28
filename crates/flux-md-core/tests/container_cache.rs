//! Correctness net for the incremental container cache (parser.rs) — the fast
//! path for a long open blockquote / GitHub alert at the tail. Char-by-char
//! and every chunk 1..=9 must produce byte-identical HTML to one-shot.
//!
//! Written before the cache lands so the test pins pre-cache correctness
//! (the regression we're fixing is perf, not output).

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn alerts(p: StreamParser) -> StreamParser {
    p.with_gfm_alerts(true)
}

fn render_with(make: impl Fn() -> StreamParser, md: &str) -> String {
    let mut p = make();
    p.append(md);
    p.finalize();
    collect(&p)
}

fn streamed_with(make: impl Fn() -> StreamParser, md: &str) -> String {
    let mut p = make();
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

fn chunked_with(make: impl Fn() -> StreamParser, md: &str, n: usize) -> String {
    let mut p = make();
    let b = md.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let mut e = (i + n).min(b.len());
        while e < b.len() && (b[e] & 0xC0) == 0x80 {
            e += 1;
        }
        p.append(&md[i..e]);
        i = e;
    }
    p.finalize();
    collect(&p)
}

#[test]
fn blockquote_cache_matches_full_render() {
    let make = || alerts(StreamParser::new());

    let mut big_para = String::new();
    for _ in 0..400 {
        big_para.push_str("> a continuation line with some **bold** and `code` here, plus more prose to bulk it up.\n");
    }

    let cases: &[&str] = &[
        // Plain blockquote, one-line paragraph
        "> Hello, world!\n",
        // Plain blockquote, multi-line paragraph (single inner paragraph)
        "> line one\n> line two\n> line three\n",
        // Inline markdown inside the blockquote
        "> a paragraph with **bold**, *italic*, `code`, and a [link](https://example.com)\n",
        // Lazy continuation (a non-`>` line that continues the paragraph)
        "> first line\nsecond line lazy\n> third line\n",
        // Multi-paragraph blockquote — must NOT cache as single-paragraph
        "> first paragraph\n> still first\n>\n> second paragraph\n> more of second\n",
        // Blockquote with a nested fenced code block
        "> Before\n>\n> ```rust\n> fn x() {}\n> ```\n>\n> After\n",
        // Blockquote followed by a paragraph (must commit cleanly)
        "> quoted\n> quoted more\n\nAnd a following paragraph.\n",
        // Blockquote with no trailing newline
        "> Hello, world!",
        // Empty blockquote-only line  → blank inner
        ">\n",
        // Big single-paragraph blockquote (stress)
        &big_para,
    ];
    for md in cases {
        let one = render_with(make, md);
        let preview: String = md.chars().take(60).collect();
        assert_eq!(streamed_with(make, md), one, "char-stream != one-shot for {:?}", preview);
        for n in 1..=9 {
            assert_eq!(chunked_with(make, md, n), one, "chunk={n} != one-shot for {:?}", preview);
        }
    }
}

#[test]
fn alert_cache_matches_full_render() {
    let make = || alerts(StreamParser::new());

    let mut big_alert = String::from("> [!NOTE]\n");
    for _ in 0..400 {
        big_alert.push_str("> a continuation line of the note body with **bold** and a [link](https://example.com) thrown in.\n");
    }

    let cases: &[&str] = &[
        // Each of the five alert kinds
        "> [!NOTE]\n> body of the note\n",
        "> [!TIP]\n> body of the tip\n",
        "> [!IMPORTANT]\n> body of the important\n",
        "> [!WARNING]\n> body of the warning\n",
        "> [!CAUTION]\n> body of the caution\n",
        // Alert with inline markup in body
        "> [!NOTE]\n> a body with **bold** and `code` and [link](https://x).\n",
        // Alert with empty body (marker only)
        "> [!NOTE]\n",
        // Alert with multi-line single paragraph body
        "> [!NOTE]\n> line one\n> line two\n> line three\n",
        // Alert followed by a paragraph
        "> [!NOTE]\n> quoted body\n\nA following paragraph.\n",
        // Alert with no trailing newline
        "> [!NOTE]\n> body without newline",
        // Multi-paragraph alert body
        "> [!NOTE]\n> first paragraph\n>\n> second paragraph\n",
        // Big single-paragraph alert (stress)
        &big_alert,
    ];
    for md in cases {
        let one = render_with(make, md);
        let preview: String = md.chars().take(60).collect();
        assert_eq!(streamed_with(make, md), one, "char-stream != one-shot for {:?}", preview);
        for n in 1..=9 {
            assert_eq!(chunked_with(make, md, n), one, "chunk={n} != one-shot for {:?}", preview);
        }
    }
}

#[test]
fn container_cache_with_dir_auto() {
    // dir_auto changes the wrapper HTML (`<blockquote dir="auto">`, `<p dir="auto">`,
    // and inside the alert div+title+body). The cache must produce identical bytes.
    let make = || StreamParser::new().with_gfm_alerts(true).with_dir_auto(true);
    let cases: &[&str] = &[
        "> Hello, world!\n",
        "> line one\n> line two\n",
        "> [!WARNING]\n> warning body\n> more of warning\n",
    ];
    for md in cases {
        let one = render_with(make, md);
        assert_eq!(streamed_with(make, md), one, "char-stream != one-shot for {md:?}");
        for n in 1..=9 {
            assert_eq!(chunked_with(make, md, n), one, "chunk={n} != one-shot for {md:?}");
        }
    }
}

#[test]
fn crlf_container_falls_back_correctly() {
    let make = || alerts(StreamParser::new());
    // The cache may bail on `\r`; CRLF blockquotes / alerts go through the full
    // renderer, so output still matches and nothing panics.
    let cases: &[&str] = &[
        "> Hello, world!\r\n> line two\r\n",
        "> [!NOTE]\r\n> body line\r\n",
    ];
    for md in cases {
        let one = render_with(make, md);
        assert_eq!(streamed_with(make, md), one, "char-stream != one-shot for {md:?}");
        for n in 1..=9 {
            assert_eq!(chunked_with(make, md, n), one, "chunk={n} != one-shot for {md:?}");
        }
    }
}

#[test]
fn open_alert_renders_incrementally() {
    // Pin block-id stability across the streaming-then-closing transition.
    let mut p = StreamParser::new().with_gfm_alerts(true);
    p.append("> [!NOTE]\n");
    p.append("> first body line\n");
    let id0 = p.all_blocks().last().unwrap().id;
    p.append("> second body line\n");
    let h = collect(&p);
    assert!(h.contains("markdown-alert-note") && h.contains("second body line"), "{h}");
    assert_eq!(p.all_blocks().last().unwrap().id, id0, "id stable while streaming");

    // Close it with a blank line + paragraph; the alert keeps its id.
    p.append("\nAfter the alert.\n");
    let blocks: Vec<_> = p.all_blocks().cloned().collect();
    assert!(blocks.iter().any(|b| b.id == id0), "alert block id survives close");
}

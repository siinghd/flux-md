//! Correctness net for the incremental indented-code and raw-HTML-block caches
//! (parser.rs). Streaming char-by-char exercises the O(new-bytes) cache path; a
//! single-shot parse uses the full renderer. They must produce byte-identical
//! committed + active HTML for every prefix shape below — if they do, the caches
//! are faithful and the unclosed-block O(n^2) tail re-scan is closed.

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

/// HTML + serialized `kind` of every block, so block_data parity is asserted too.
fn collect_with_kind(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
        out.push('\u{1}');
        out.push_str(&serde_json::to_string(&b.kind).unwrap());
        out.push('\u{2}');
    }
    out
}

/// One-shot: a single append then finalize — pure full-renderer path.
fn render(md: &str) -> String {
    let mut p = StreamParser::new();
    p.append(md);
    p.finalize();
    collect(&p)
}

/// Char-by-char: arms and drives the incremental caches.
fn render_streamed(md: &str) -> String {
    let mut p = StreamParser::new();
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

/// Same, with `unsafe_html` on (exercises the HTML pass-through branch).
fn render_unsafe(md: &str) -> String {
    let mut p = StreamParser::new().with_unsafe_html(true);
    p.append(md);
    p.finalize();
    collect(&p)
}

fn render_unsafe_streamed(md: &str) -> String {
    let mut p = StreamParser::new().with_unsafe_html(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

#[test]
fn indented_code_cache_matches_full_render() {
    let mut big = String::new();
    for i in 0..600 {
        big.push_str(&format!("    let x{i} = compute(a, b) + {i}; // c {i}\n"));
    }

    let cases: &[&str] = &[
        "    simple indented code\n    second line\n    third\n",
        "    a < b && c > \"d\" & e\n    let t = `x`;\n", // HTML-special chars
        "    one line only\n",
        "    no trailing newline",                       // open at finalize, no \n
        "    line with trailing spaces   \n    next\n",  // trailing-whitespace trim
        "\tone tab indent counts as four\n\tsecond\n",   // tab indent
        "    body\n  then a dedent ends it\n",           // dedent mid-stream
        "    code\n\n    more after a blank\n",           // interior blank (full path)
        "text first\n\n    indented after a paragraph\n    line two\n", // gap before
        "    deep\n        deeper still keeps the inner indent\n",      // >4 indent kept
        &big,
    ];
    for md in cases {
        assert_eq!(
            render_streamed(md),
            render(md),
            "streamed != one-shot for {:?}",
            &md[..md.len().min(40)]
        );
    }
}

#[test]
fn html_block_cache_matches_full_render_escaped() {
    // unsafe_html OFF — the escaped `<pre><code>` branch.
    let mut big = String::from("<div>\n");
    for i in 0..600 {
        big.push_str(&format!("  <span class=\"row\">item {i} &amp; more</span>\n"));
    }
    big.push_str("</div>\n");

    let cases: &[&str] = &[
        "<div>\n  <p>hello</p>\n</div>\n",                // type 6, closes on blank? no — EOF
        "<div>\nline one\nline two & < > chars\n</div>\n", // special chars escaped
        "<table>\n  <tr><td>a</td></tr>\n",               // type 6 open at finalize
        "<!-- a comment\nspanning lines\nbut never closing", // type 2 unclosed
        "<?php\n  echo 1;\n  echo 2;\n",                  // type 3 unclosed
        "<script>\nvar a = 1 < 2;\nvar b = 3 > 0;\n",     // type 1 unclosed
        "<pre>\n  raw &amp; text\n  more\n",              // type 1 (<pre>) unclosed
        "text before\n\n<div>\n  content\n  more\n",      // gap before the block
        &big,
    ];
    for md in cases {
        assert_eq!(
            render_streamed(md),
            render(md),
            "streamed != one-shot (escaped) for {:?}",
            &md[..md.len().min(40)]
        );
    }
}

#[test]
fn html_block_cache_matches_full_render_passthrough() {
    // unsafe_html ON — the verbatim pass-through branch.
    let mut big = String::from("<div>\n");
    for i in 0..600 {
        big.push_str(&format!("  <span class=\"row\">item {i}</span>\n"));
    }
    big.push_str("</div>\n");

    let cases: &[&str] = &[
        "<div>\n  <p>hello</p>\n</div>\n",
        "<div>\nline one\nline two\n</div>\n",
        "<table>\n  <tr><td>a</td></tr>\n",               // open at finalize
        "<!-- a comment\nspanning lines\nbut never closing",
        "<script>\nvar a = 1;\nvar b = 2;\n",             // type 1 unclosed
        "<div>\n  content\n  more\n",                     // open, multi-line
        &big,
    ];
    for md in cases {
        assert_eq!(
            render_unsafe_streamed(md),
            render_unsafe(md),
            "streamed != one-shot (pass-through) for {:?}",
            &md[..md.len().min(40)]
        );
    }
}

#[test]
fn html_block_closes_streamed() {
    // Closing lines (types 1–5) and blank lines (types 6/7) must commit at the
    // same boundary in both modes, with text after the block parsed normally.
    let cases: &[&str] = &[
        "<script>\nvar a = 1;\n</script>\n\nProse after.\n", // type 1 closes on </script>
        "<!-- comment -->\n\nProse after the comment.\n",     // type 2 closes on -->
        "<div>\n  content\n</div>\n\nProse after.\n",         // type 6 closes on blank
        "<pre>\n  raw text\n</pre>\n\nAfter.\n",              // type 1 (<pre>)
        "<?php echo 1; ?>\n\nAfter.\n",                       // type 3
    ];
    for md in cases {
        assert_eq!(
            render_streamed(md),
            render(md),
            "streamed != one-shot (close) for {:?}",
            &md[..md.len().min(40)]
        );
    }
}

#[test]
fn indented_code_id_stable_across_appends() {
    // An open indented-code block shows content as it arrives and keeps its id
    // across appends (no remount), then commits onto the same id when it ends.
    let mut p = StreamParser::new();
    let id0 = p.append("    first line\n").active[0].id;
    p.append("    second line\n");
    let patch = p.append("    third line\n");
    let out = collect(&p);
    assert!(out.contains("<pre><code>"), "{out}");
    assert!(out.contains("first line") && out.contains("third line"), "{out}");
    assert_eq!(patch.active.len(), 1, "still one open block");
    assert_eq!(patch.active[0].id, id0, "indented-code id must stay stable");

    // A dedent / blank ends it; finalize commits onto the same id.
    let closed = p.append("\nplain prose now\n");
    let _ = closed;
    p.finalize();
    let committed_id = p.all_blocks().next().map(|b| b.id);
    assert_eq!(committed_id, Some(id0), "closed indented code keeps its id");
}

#[test]
fn html_block_id_stable_across_appends() {
    let mut p = StreamParser::new();
    let id0 = p.append("<div>\n").active[0].id;
    p.append("  <p>a</p>\n");
    let patch = p.append("  <p>b</p>\n");
    assert_eq!(patch.active.len(), 1, "still one open block");
    assert_eq!(patch.active[0].id, id0, "html-block id must stay stable");
    let closed = p.append("</div>\n\n");
    assert_eq!(
        closed.newly_committed.last().map(|b| b.id),
        Some(id0),
        "closed html block keeps its id"
    );
}

#[test]
fn indented_code_block_data_matches_streamed() {
    // With block_data on, the cache must recover the same decoded `code` source
    // as the full path — assert HTML AND the serialized kind are byte-identical.
    fn render_kd(md: &str, stream: bool) -> String {
        let mut p = StreamParser::new().with_block_data(true);
        if stream {
            let mut buf = [0u8; 4];
            for ch in md.chars() {
                p.append(ch.encode_utf8(&mut buf));
            }
        } else {
            p.append(md);
        }
        p.finalize();
        collect_with_kind(&p)
    }
    let cases: &[&str] = &[
        "    let x = a < b && c > d;\n    return x;\n",
        "    trailing spaces survive trim   \n    next line\n",
        "\ttab indent\n\tsecond tab line\n",
    ];
    for md in cases {
        assert_eq!(render_kd(md, true), render_kd(md, false), "block_data {md:?}");
    }
}

#[test]
fn tiny_chunks_on_huge_indented_code() {
    // The O(n^2) scenario the cache exists for: a large indented-code block
    // streamed in 1-byte chunks must equal the one-shot render.
    let mut md = String::new();
    for i in 0..2000 {
        md.push_str(&format!("    line number {i} with some content\n"));
    }
    assert_eq!(render_streamed(&md), render(&md));
}

#[test]
fn tiny_chunks_on_huge_html_block() {
    let mut md = String::from("<div>\n");
    for i in 0..2000 {
        md.push_str(&format!("  <span>item {i}</span>\n"));
    }
    assert_eq!(render_streamed(&md), render(&md));
}

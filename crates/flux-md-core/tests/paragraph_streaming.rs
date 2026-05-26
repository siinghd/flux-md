//! Correctness net for the incremental long-paragraph cache (parser.rs).
//! Streaming a paragraph char-by-char (and in 256-byte chunks) must produce
//! byte-identical HTML to a one-shot parse, across plain text and every inline
//! construct — especially the ones that span inter-word spaces (emphasis, code
//! spans, links) where a naive cut would split a construct.
//!
//! Run against `main` first (no cache yet) to prove the harness, then again
//! after the cache lands. Final-state equality plus per-prefix block invariants.

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

/// Feed `md` in `chunk`-byte pieces (split on UTF-8 boundaries) and finalize.
fn run(md: &str, chunk: usize, mk: &dyn Fn() -> StreamParser) -> String {
    let mut p = mk();
    let bytes = md.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let mut e = (i + chunk).min(bytes.len());
        while e < bytes.len() && (bytes[e] & 0xC0) == 0x80 {
            e += 1;
        }
        p.append(&md[i..e]);
        i = e;
    }
    p.finalize();
    collect(&p)
}

fn short(md: &str) -> String {
    md.chars().take(48).collect::<String>().replace('\n', "\\n")
}

/// One-shot == char-by-char == 256-byte chunks.
fn parity(md: &str, mk: &dyn Fn() -> StreamParser) {
    let one = run(md, md.len().max(1), mk);
    assert_eq!(run(md, 1, mk), one, "char-by-char != one-shot: {:?}", short(md));
    assert_eq!(run(md, 256, mk), one, "256B chunks != one-shot: {:?}", short(md));
}

fn plain(prefix: &str, n: usize, suffix: &str) -> String {
    let mut s = String::from(prefix);
    for i in 0..n {
        s.push_str("word");
        s.push_str(&i.to_string());
        s.push(' ');
    }
    s.push_str(suffix);
    s.push('\n');
    s
}

fn default() -> StreamParser {
    StreamParser::new()
}
fn autolinks() -> StreamParser {
    StreamParser::new().with_gfm_autolinks(true)
}
fn all_on() -> StreamParser {
    StreamParser::new()
        .with_gfm_autolinks(true)
        .with_gfm_alerts(true)
        .with_gfm_math(true)
        .with_dir_auto(true)
}

#[test]
fn plain_paragraphs() {
    parity(&plain("", 500, ""), &default);
    parity(&plain("", 500, ""), &all_on);
    // Leading / trailing whitespace and tabs.
    parity(&plain("   ", 100, "   "), &default);
    parity("single-token-no-spaces-but-fairly-long-aaaaaaaaaaaaaaaaaaaaaaaaaaa\n", &default);
}

#[test]
fn soft_and_hard_breaks() {
    // Soft break (single \n) mid-paragraph.
    parity("first line of the paragraph here\nsecond line continues on\nthird line\n", &default);
    parity(&format!("{}\n{}\n{}\n", plain("", 80, ""), plain("", 80, ""), plain("", 80, "")), &default);
    // Hard break (2 trailing spaces before \n).
    parity("line one with a hard break  \nline two after the break here\n", &default);
    parity(&format!("{}  \n{}", plain("a ", 80, "end"), plain("b ", 80, "fin")), &default);
}

#[test]
fn emphasis_does_not_break() {
    // Closed emphasis / strong / strike spanning inter-word spaces: the cut must
    // never land inside them. The cache won't speed these up, but must not break.
    parity(&format!("{}*italic words here*{}", plain("", 60, ""), plain(" ", 60, "")), &default);
    parity(&format!("{}**bold words here**{}", plain("", 60, ""), plain(" ", 60, "")), &default);
    parity(&format!("{}~~struck words~~{}", plain("", 60, ""), plain(" ", 60, "")), &default);
    parity("a *b c* d _e f_ g **h i** j\n", &default);
    // Ambiguous / nested emphasis chains.
    parity("*a **b** c* and *_*_*_ tail\n", &default);
    parity("***triple*** and **_mixed_** stuff\n", &default);
    // Intra-word underscores (not emphasis) and approximate tildes.
    parity("a snake_case_name and another_one here ~5 things\n", &default);
}

#[test]
fn code_spans_with_spaces() {
    parity(&format!("{}`code with spaces`{}", plain("", 60, ""), plain(" ", 60, "")), &default);
    parity("text ``a ` b`` more and `simple span` end\n", &default);
    parity("an unclosed `code span that never closes in the paragraph here\n", &default);
}

#[test]
fn links_and_images() {
    parity(&format!("{}[link text here](http://example.com/x){}", plain("", 60, ""), plain(" ", 60, "")), &default);
    // Definition first, so it resolves identically streamed vs one-shot (a
    // *forward* reference is a known non-convergent case — out of scope here).
    parity("[r]: http://x.com\n\nsee [ref text] with a ref and [other][r] too\n", &default);
    parity("an ![image alt text](http://x.com/i.png) inline here\n", &default);
    parity("an unclosed [link text that never closes here in the paragraph\n", &default);
}

#[test]
fn autolinks_and_html() {
    parity("visit <https://example.com/path> for details about it\n", &default);
    parity(&format!("{}<https://example.com/a>{}", plain("", 60, ""), plain(" ", 60, "")), &autolinks);
    // GFM bare autolinks.
    parity(&format!("{}www.example.com/path{}", plain("", 60, ""), plain(" ", 60, "")), &autolinks);
    parity("mail me at foo@bar.example today please\n", &autolinks);
    parity("a bare url https://example.org/very/long/path?q=1 in text\n", &autolinks);
}

#[test]
fn unblocked_chars_commit_correctly() {
    // These chars are intentionally NOT blockers — they settle within a token.
    parity(&plain("AT&T and Tom&Jerry wow! a > b yes ] no ", 200, "end &amp; entity"), &default);
    parity(&plain("escape \\* and \\_ and \\` and \\\\ here ", 200, "done"), &default);
    parity("numeric &#65;&#x42; and named &amp; &lt; entities in a long line of prose here\n", &default);
}

#[test]
fn mixed_long_with_sparse_constructs() {
    // A long paragraph with constructs sprinkled in — the realistic-ish case.
    let mut s = String::new();
    for i in 0..200 {
        s.push_str(&format!("sentence {i} of the explanation continues "));
        if i % 50 == 0 {
            s.push_str("with **emphasis** and `code` and [a link](http://x.com/y) ");
        }
    }
    s.push('\n');
    parity(&s, &default);
    parity(&s, &all_on);
}

#[test]
fn math_spans_are_blockers() {
    // With gfmMath on, `$…$` and `\(…\)` span inter-word spaces, so a cut must
    // never land inside them. Currency stays literal (pandoc rule).
    let math = || StreamParser::new().with_gfm_math(true);
    parity(&format!("{}$a + b + c$ {}", plain("", 60, ""), plain("", 60, "")), &math);
    parity(&format!("{}\\(x + y + z\\) {}", plain("", 60, ""), plain("", 60, "")), &math);
    parity(&plain("I have $5 and $10 and $20 to spend on ", 200, "things"), &math);
    parity("the identity $$e^{i\\pi} = -1$$ holds in this long line of explanation here\n", &math);
}

#[test]
fn paragraph_termination_bails() {
    // The cache must bail when the open paragraph actually ends — a blank line,
    // a block-starting line, or a setext underline (which changes the kind).
    parity(&format!("{}\n\n{}", plain("", 120, "end"), plain("", 120, "more")), &default);
    parity(&format!("{}\n\n# A heading\n", plain("", 120, "end")), &default);
    parity(&format!("{}\n\n- a list item\n- another\n", plain("", 120, "end")), &default);
    parity(&format!("{}\n\n```\ncode block\n```\n", plain("", 120, "end")), &default);
    parity(&format!("{}\n> a block quote now\n", plain("", 120, "end")), &default);
    // Setext underline promotes the whole paragraph to a heading.
    parity("a long paragraph of text that will become a heading\n=====\n", &default);
    parity(&format!("{}\n-----\n", plain("", 80, "title")), &default);
}

#[test]
fn dir_auto_paragraph_parity() {
    // The cached path must still produce <p dir="auto"> when bidi is on.
    let md = plain("", 300, "");
    let out = run(&md, 1, &|| StreamParser::new().with_dir_auto(true));
    assert!(out.starts_with("<p dir=\"auto\">"), "dir on cached paragraph: {}", short(&out));
    parity(&md, &|| StreamParser::new().with_dir_auto(true));
    // RTL content streamed.
    parity("هذا اختبار طويل جدا يحتوي على كلمات كثيرة جدا ومتكررة هنا الآن واليوم\n", &|| {
        StreamParser::new().with_dir_auto(true)
    });
}

#[test]
fn intermediate_prefix_matches_full_render() {
    // Stronger than final equality: at every streamed prefix the rendered HTML
    // must match a fresh single-append parse of the same prefix (which always
    // takes the full-render path) — so the cache's *intermediate* output is
    // byte-identical too, not just the finalized result.
    for md in [
        plain("intro ", 120, "tail"),
        format!("plain start {}*emph words*{}", plain("", 40, ""), plain(" ", 40, "")),
    ] {
        let mut streamed = StreamParser::new();
        let mut buf = [0u8; 4];
        let mut at = 0usize;
        for ch in md.chars() {
            let s = ch.encode_utf8(&mut buf);
            streamed.append(s);
            at += s.len();
            let mut oneshot = StreamParser::new();
            oneshot.append(&md[..at]);
            let a: String = streamed.all_blocks().map(|b| b.html.clone()).collect();
            let b: String = oneshot.all_blocks().map(|b| b.html.clone()).collect();
            assert_eq!(a, b, "intermediate mismatch at {at} bytes of {:?}", short(&md));
        }
    }
}

#[test]
fn streaming_has_no_orphan_blocks() {
    // Per-prefix invariants while streaming a long paragraph: ordered,
    // non-overlapping, unique block ids, and a stable id for the open paragraph.
    let md = plain("intro words ", 400, "tail");
    let mut p = StreamParser::new();
    let mut buf = [0u8; 4];
    let mut para_id: Option<u64> = None;
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
        let blocks: Vec<_> = p.all_blocks().collect();
        let mut last_end = 0usize;
        let mut ids = std::collections::HashSet::new();
        for b in &blocks {
            assert!(b.start >= last_end, "overlap mid-stream");
            assert!(ids.insert(b.id), "duplicate id mid-stream");
            last_end = b.end;
        }
        // The single open paragraph keeps one stable id throughout.
        if blocks.len() == 1 {
            match para_id {
                None => para_id = Some(blocks[0].id),
                Some(id) => assert_eq!(blocks[0].id, id, "paragraph id must stay stable"),
            }
        }
    }
}

//! Streaming a GFM table must render incrementally — the header appears as soon
//! as the delimiter row arrives, and each body row appends as it streams — and
//! must converge to the one-shot parse. (Regression: the incremental paragraph
//! cache used to keep extending the header line as a paragraph and never form
//! the table until a non-streaming reparse, so a streaming table only appeared
//! once fully buffered.)

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn one_shot(md: &str) -> String {
    let mut p = StreamParser::new();
    p.append(md);
    p.finalize();
    collect(&p)
}

fn streamed(md: &str) -> String {
    let mut p = StreamParser::new();
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

fn chunked(md: &str, n: usize) -> String {
    let mut p = StreamParser::new();
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
fn table_streams_to_same_output() {
    let cases = [
        "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |\n",
        "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |", // no trailing newline
        "Intro paragraph.\n\n| A | B |\n| :- | -: |\n| 1 | 2 |\n\nAfter the table.\n",
        "| one |\n| --- |\n| x |\n| y |\n", // single column
        "text\n| H1 | H2 | H3 |\n| --- | --- | --- |\n| a | b | c |\n", // header not at block start
    ];
    for md in cases {
        assert_eq!(streamed(md), one_shot(md), "char-stream != one-shot for {md:?}");
        for n in 1..=9 {
            assert_eq!(chunked(md, n), one_shot(md), "chunk={n} != one-shot for {md:?}");
        }
    }
}

#[test]
fn header_renders_as_soon_as_delimiter_arrives() {
    let mut p = StreamParser::new();
    p.append("| Name | Age |\n");
    assert!(collect(&p).contains("<p>"), "header alone is still a paragraph");
    p.append("| --- | --- |\n");
    let h = collect(&p);
    assert!(h.contains("<table>") && h.contains("<th>Name</th>"), "delimiter forms the table header: {h}");
    assert!(!h.contains("<p>"), "no longer a paragraph: {h}");
    p.append("| Alice | 30 |\n");
    let h = collect(&p);
    assert!(h.contains("<td>Alice</td>"), "rows append incrementally: {h}");
}

#[test]
fn table_streaming_has_no_orphan_blocks() {
    let md = "intro\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |\nafter\n";
    let mut p = StreamParser::new();
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
}

#[test]
fn pipe_lines_that_are_not_tables_converge() {
    // A `|---|`-looking line whose header column count doesn't match (so it is
    // NOT a table) must stay a paragraph and still converge — i.e. the bail is
    // precise (it doesn't mis-fire and it doesn't loop).
    let cases = [
        "| a | b | c |\n| --- | --- |\n",       // 3 cols vs 2: not a table
        "plain text\n| --- | --- |\nmore text\n", // header has no pipes: not a table
        "plain prose with a | pipe in it.\n",      // just an inline pipe
    ];
    for md in cases {
        assert_eq!(streamed(md), one_shot(md), "non-table pipe content diverged: {md:?}");
    }
}

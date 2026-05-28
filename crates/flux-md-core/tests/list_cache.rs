//! Correctness net for the incremental list cache (parser.rs) — the fast
//! path for a long open flat list at the tail (the LLM-emit shape: every
//! line is a sibling marker, optionally separated by blank lines). Both
//! tight and loose flat lists go through the cache; nested lists, multi-line
//! items, and lazy continuations still route through the full renderer.
//! This test pins parity in every mode.

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn render(md: &str) -> String {
    let mut p = StreamParser::new();
    p.append(md);
    p.finalize();
    collect(&p)
}

fn render_streamed(md: &str) -> String {
    let mut p = StreamParser::new();
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

fn render_chunked(md: &str, n: usize) -> String {
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
fn list_cache_matches_full_render() {
    let mut big_flat = String::new();
    for i in 0..400 {
        big_flat.push_str(&format!("- item {i} with some **bold** and a `bit of code` for flavor\n"));
    }

    let cases: &[&str] = &[
        // Plain flat bullet list (the LLM-emit shape — the cache's hot path)
        "- one\n- two\n- three\n",
        // Inline markup inside items
        "- **bold** item\n- `code` item\n- [link](https://x) item\n",
        // Star and plus bullets (different marker family — must still arm)
        "* alpha\n* beta\n* gamma\n",
        "+ red\n+ green\n+ blue\n",
        // Ordered list with default start
        "1. one\n2. two\n3. three\n",
        // Ordered with explicit start
        "5. five\n6. six\n7. seven\n",
        // Ordered with parens marker
        "1) one\n2) two\n3) three\n",
        // No trailing newline (last item partial)
        "- one\n- two\n- three",
        // Two-item minimum
        "- one\n- two\n",
        // Followed by paragraph
        "- one\n- two\n\nA paragraph after the list.\n",
        // Preceded by paragraph
        "Intro paragraph.\n\n- one\n- two\n- three\n",
        // Loose list (blank line between items) — cache flips to loose and
        // re-renders prior items with `<p>` wrappers
        "- one\n\n- two\n\n- three\n",
        // Multi-line item (continuation) — cache must bail
        "- one\n  continuation\n- two\n",
        // Nested list — cache must bail
        "- outer 1\n  - inner 1\n  - inner 2\n- outer 2\n",
        // Mixed markers (the second `*` doesn't match the `-` family — ends the list)
        "- one\n- two\n\n* alpha\n",
        // Big stress case
        &big_flat,
    ];
    for md in cases {
        let one = render(md);
        let preview: String = md.chars().take(50).collect();
        assert_eq!(render_streamed(md), one, "char-stream != one-shot for {:?}", preview);
        for n in 1..=9 {
            assert_eq!(render_chunked(md, n), one, "chunk={n} != one-shot for {:?}", preview);
        }
    }
}

#[test]
fn list_cache_with_dir_auto() {
    let make = || StreamParser::new().with_dir_auto(true);
    let md = "- one\n- two\n- three\n";
    let one_shot = {
        let mut p = make();
        p.append(md);
        p.finalize();
        collect(&p)
    };
    let streamed = {
        let mut p = make();
        let mut buf = [0u8; 4];
        for ch in md.chars() {
            p.append(ch.encode_utf8(&mut buf));
        }
        p.finalize();
        collect(&p)
    };
    assert_eq!(streamed, one_shot);
}

#[test]
fn crlf_list_falls_back_correctly() {
    let md = "- one\r\n- two\r\n- three\r\n";
    assert_eq!(render_streamed(md), render(md));
    for n in 1..=9 {
        assert_eq!(render_chunked(md, n), render(md), "chunk={n}");
    }
}

#[test]
fn open_list_renders_incrementally() {
    let mut p = StreamParser::new();
    p.append("- one\n");
    let id0 = p.all_blocks().last().unwrap().id;
    p.append("- two\n");
    let h = collect(&p);
    assert!(h.contains("<li>one</li>") && h.contains("<li>two</li>"), "{h}");
    assert_eq!(p.all_blocks().last().unwrap().id, id0, "id stable");

    p.append("- three\n");
    assert!(collect(&p).contains("<li>three</li>"));
    assert_eq!(p.all_blocks().last().unwrap().id, id0, "id still stable");
}

#[test]
fn loose_list_parity_across_shapes() {
    let mut big_loose = String::new();
    for i in 0..400 {
        big_loose.push_str(&format!("- item {i} with **bold** and a `bit of code`\n\n"));
    }
    let cases: &[&str] = &[
        // Two-item minimum loose
        "- one\n\n- two\n",
        // Three items loose with inline markup
        "- **bold** item\n\n- `code` item\n\n- [link](https://x) item\n",
        // Star and plus loose
        "* alpha\n\n* beta\n\n* gamma\n",
        "+ red\n\n+ green\n",
        // Ordered loose
        "1. one\n\n2. two\n\n3. three\n",
        "5. five\n\n6. six\n",
        // Multiple blank lines between items still counts as one blank gap
        "- a\n\n\n- b\n\n\n\n- c\n",
        // GFM task list, loose
        "- [x] done\n\n- [ ] todo\n\n- [X] also done\n",
        // No trailing newline (partial last item) in a loose list
        "- one\n\n- two\n\n- three",
        // Trailing blank with no second marker yet — must NOT flip to loose
        "- only\n\n",
        // Loose followed by a paragraph
        "- one\n\n- two\n\nA paragraph.\n",
        // Loose preceded by paragraph
        "Intro.\n\n- one\n\n- two\n",
        // Big loose stress case
        &big_loose,
    ];
    for md in cases {
        let one = render(md);
        let preview: String = md.chars().take(50).collect();
        assert_eq!(render_streamed(md), one, "char-stream != one-shot for {preview:?}");
        for n in 1..=9 {
            assert_eq!(render_chunked(md, n), one, "chunk={n} != one-shot for {preview:?}");
        }
    }
}

#[test]
fn loose_list_with_dir_auto() {
    // The loose path emits an inner `<p dir?>` per item — a second `opts.dir()`
    // site distinct from the `<li dir?>` on the wrapper. Pin parity.
    let make = || StreamParser::new().with_dir_auto(true);
    let cases: &[&str] = &[
        "- one\n\n- two\n\n- three\n",
        "1. **a**\n\n2. b\n",
        "- [x] task\n\n- [ ] other\n",
    ];
    for md in cases {
        let one_shot = {
            let mut p = make();
            p.append(md);
            p.finalize();
            collect(&p)
        };
        let streamed = {
            let mut p = make();
            let mut buf = [0u8; 4];
            for ch in md.chars() {
                p.append(ch.encode_utf8(&mut buf));
            }
            p.finalize();
            collect(&p)
        };
        assert_eq!(streamed, one_shot, "{md:?}");
    }
}

#[test]
fn tight_to_loose_mid_stream_rewrites_prior_items() {
    // The first two items arrive tight (no blank between), then a blank
    // arrives before item 3 — the cache must rebuild prior items as `<p>`-
    // wrapped loose `<li>`s before appending the new one.
    let mut p = StreamParser::new();
    p.append("- one\n- two\n");
    // Sanity: tight at this point.
    assert!(collect(&p).contains("<li>one</li>"));
    // The blank line arrives, then a third marker line.
    p.append("\n- three\n");
    p.finalize();
    let streamed = collect(&p);
    // Compare with the one-shot rendering of the same full input.
    let one_shot = render("- one\n- two\n\n- three\n");
    assert_eq!(streamed, one_shot, "mid-stream tight→loose flip must converge");
}

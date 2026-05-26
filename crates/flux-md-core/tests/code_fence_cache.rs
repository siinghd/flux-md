//! Correctness net for the incremental code-fence cache (parser.rs). Streaming
//! char-by-char exercises the O(new-bytes) cache path; a single-shot parse uses
//! the full renderer. They must produce byte-identical HTML — if they do for
//! every prefix shape below, the cache is faithful.

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
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

/// Char-by-char: arms and drives the incremental code-fence cache.
fn render_streamed(md: &str) -> String {
    let mut p = StreamParser::new();
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

#[test]
fn cache_matches_full_render() {
    let mut big = String::from("```rust\n");
    for i in 0..600 {
        big.push_str(&format!("    let x{i} = compute(a, b) + {i}; // comment {i}\n"));
    }
    big.push_str("```\n");

    let cases: &[&str] = &[
        "```\nplain text body\n```\n",
        "```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n",
        "```js\nconst ok = a < b && c > \"d\" & e;\nlet t = `x`;\n```\n", // HTML-special chars
        "```\n\n\nbody after blank lines\n\n\nand more\n```\n",          // interior blank lines
        "````\n```\ntriple inside a quad fence stays body\n```\n````\n", // 4-backtick + ``` body
        "~~~\n```\nbacktick run inside a tilde fence is body\n```\n~~~\n", // ~~~ + ``` body
        "```python\ndef f():\n    return 1\n```\n\nProse after the fence.\n", // content after
        "```\nan unclosed fence\nthat never ends\nmid stream\n",          // open at finalize
        "```\n```\n",                                                     // empty body
        "```math\nE = mc^2\n```\n",                                       // MathBlock kind
        "    indented code\n    second line\n    third\n",                // indented (no fence)
        "```\ntrailing spaces here   \n\ttab line\there\n```\n",          // whitespace oddities
        "text before\n\n```\ncode after a paragraph\nline two\n```\n\ntext after\n",
        &big,
    ];
    for md in cases {
        assert_eq!(
            render_streamed(md),
            render(md),
            "streamed != one-shot for {:?}",
            &md[..md.len().min(50)]
        );
    }
}

#[test]
fn crlf_fence_falls_back_correctly() {
    // The cache bails on `\r`; CRLF fences go through the full renderer in both
    // modes, so output still matches and nothing panics.
    let md = "```rust\r\nfn main() {}\r\nlet x = 1;\r\n```\r\n";
    assert_eq!(render_streamed(md), render(md));
}

#[test]
fn open_fence_still_renders_incrementally() {
    // #473 must still hold with the cache: an open fence shows its content as it
    // arrives, and its block id stays stable across appends.
    let mut p = StreamParser::new();
    let id0 = {
        let patch = p.append("```js\n");
        patch.active[0].id
    };
    p.append("const x = 1;\n");
    let patch = p.append("const y = 2;\n");
    let out = collect(&p);
    assert!(out.contains("<pre><code class=\"language-js\""), "{out}");
    assert!(out.contains("const x = 1;") && out.contains("const y = 2;"), "{out}");
    assert_eq!(patch.active.len(), 1, "still one open block");
    assert_eq!(patch.active[0].id, id0, "fence id must stay stable across appends");

    // Closing it commits the same block id (no remount).
    let patch = p.append("```\n\n");
    let committed_id = patch.newly_committed.last().map(|b| b.id);
    assert_eq!(committed_id, Some(id0), "closed fence keeps its id");
}

#[test]
fn id_stable_across_cache_bail() {
    // The cache holds the fence's id and reuses it on each fast-path append.
    // When it bails (here, a `\r` mid-line), the full renderer takes over and
    // must keep the *same* id via active_blocks reuse — otherwise the streaming
    // UI would remount the block. Pin that invariant across the boundary.
    let mut p = StreamParser::new();
    p.append("```\n");
    let id = p.append("first line\n").active[0].id; // cache armed, fast path
    p.append("second line\n"); // fast path
    let after_bail = p.append("third\rline\n"); // `\r` forces bail to full path
    assert_eq!(after_bail.active.len(), 1, "still one open block after bail");
    assert_eq!(after_bail.active[0].id, id, "id must survive the cache bail");
    let more = p.append("fourth line\n"); // full path (cache stays off due to `\r`)
    assert_eq!(more.active[0].id, id, "id stays stable after the bail");
    let closed = p.append("```\n\n"); // closes onto the same id
    assert_eq!(closed.newly_committed.last().unwrap().id, id, "closed fence keeps its id");
}

#[test]
fn cache_handles_tiny_chunks_on_huge_fence() {
    // The scenario the cache exists for: a large fence streamed in 1-byte chunks
    // must equal the one-shot render (and finish quickly, though we only assert
    // correctness here).
    let mut md = String::from("```\n");
    for i in 0..2000 {
        md.push_str(&format!("line number {i} with some content\n"));
    }
    md.push_str("```\n");
    assert_eq!(render_streamed(&md), render(&md));
}

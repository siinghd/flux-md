//! Correctness net for the incremental display-math fence cache (parser.rs).
//! Streaming char-by-char exercises the O(new-bytes) cache path; a single-shot
//! parse uses the full renderer. They must produce byte-identical HTML for every
//! prefix shape below — math fences (`$$…$$` / `\[…\]`) trim surrounding
//! whitespace and use a different closer than code fences, so the cache must
//! reproduce that exactly. Math is opt-in, so every parser here enables it.

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
    let mut p = StreamParser::new().with_gfm_math(true);
    p.append(md);
    p.finalize();
    collect(&p)
}

/// Char-by-char: arms and drives the incremental math-fence cache.
fn render_streamed(md: &str) -> String {
    let mut p = StreamParser::new().with_gfm_math(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

#[test]
fn cache_matches_full_render() {
    // The bench shape: a long multi-line aligned environment.
    let mut big = String::from("$$\n\\begin{aligned}\n");
    for i in 0..600 {
        big.push_str(&format!("x_{{{i}}} &= \\frac{{1}}{{2}}\\left(y_{i} + z_{i}\\right) \\\\\n"));
    }
    big.push_str("\\end{aligned}\n$$\n");

    let cases: &[&str] = &[
        "$$\n\\sum_{i=1}^{n} x_i\n$$\n",                       // simple multi-line
        "$$\n\\begin{aligned}\na &= b \\\\\nc &= d\n\\end{aligned}\n$$\n",
        "$$\nx < y & z > w\n$$\n",                             // HTML-special chars in body
        "$$\nx\n\n\nstill body after blanks\n\n\n$$\n",        // interior + trailing blanks
        "$$\n\n\n\\alpha\n$$\n",                               // leading blank lines (trimmed)
        "\\[\n\\int_0^1 f(x)\\,dx\n\\]\n",                     // \[…\] variant, different closer
        "\\[\n\\frac{a}{b} \\\\\n\\frac{c}{d}\n\\]\n",         // \[…\] multi-line
        "$$\nE = mc^2\n$$\n\nProse after the block.\n",        // content after the block
        "$$\nan unclosed display block\nthat never ends\nmid stream\n", // open at finalize
        "$$\n$$\n",                                            // empty body
        "$$x = 1$$\n",                                         // single-line (closer on opener)
        "$$\nx = 1\nfoo $$ bar\n",                             // closer mid-line with content
        "$$\ntrailing spaces here   \n\ttab\there\n$$\n",      // whitespace oddities
        "text before\n\n$$\n\\nabla \\cdot E\n$$\n\ntext after\n",
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
fn crlf_math_fence_falls_back_correctly() {
    // The cache bails on `\r`; CRLF math fences go through the full renderer in
    // both modes, so output still matches and nothing panics.
    let md = "$$\r\n\\sum x_i\r\na = b\r\n$$\r\n";
    assert_eq!(render_streamed(md), render(md));
}

#[test]
fn open_math_fence_renders_incrementally() {
    // An open math block shows its content as it arrives, with a stable block id.
    let mut p = StreamParser::new().with_gfm_math(true);
    let id0 = p.append("$$\n").active[0].id;
    p.append("\\alpha + \\beta\n");
    let patch = p.append("+ \\gamma\n");
    let out = collect(&p);
    assert!(out.contains("<div class=\"math math-display\">"), "{out}");
    assert!(out.contains("\\alpha + \\beta") && out.contains("+ \\gamma"), "{out}");
    assert_eq!(patch.active.len(), 1, "still one open block");
    assert_eq!(patch.active[0].id, id0, "math fence id must stay stable across appends");

    // Closing it commits the same block id (no remount).
    let patch = p.append("$$\n\n");
    let committed_id = patch.newly_committed.last().map(|b| b.id);
    assert_eq!(committed_id, Some(id0), "closed math fence keeps its id");
}

#[test]
fn huge_math_fence_tiny_chunks() {
    // The scenario the cache exists for: a large math block streamed in 1-byte
    // chunks must equal the one-shot render.
    let mut md = String::from("$$\n\\begin{aligned}\n");
    for i in 0..2000 {
        md.push_str(&format!("t_{{{i}}} &= a_{i} + b_{i} \\\\\n"));
    }
    md.push_str("\\end{aligned}\n$$\n");
    assert_eq!(render_streamed(&md), render(&md));
}

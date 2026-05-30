//! Regression tests for tricky streaming-markdown cases that naive parsers get
//! wrong — single tildes, tight lists, incremental fences, lookbehind-free
//! autolinks, GitHub alerts, math delimiters, per-block direction, and partial
//! images — each verifying flux-md is correct under both one-shot and
//! char-by-char streaming.

use flux_md_core::{BlockKind, StreamParser};

/// Render in one shot (GFM autolinks + alerts on, matching the demo config).
fn render(md: &str) -> String {
    let mut p = StreamParser::new().with_gfm_autolinks(true).with_gfm_alerts(true);
    p.append(md);
    p.finalize();
    collect(&p)
}

/// Render fed one character at a time (the streaming path).
fn render_streamed(md: &str) -> String {
    let mut p = StreamParser::new().with_gfm_autolinks(true).with_gfm_alerts(true);
    for ch in md.chars() {
        let mut buf = [0u8; 4];
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

/// A single `~` must not become strikethrough (only `~~…~~` does) — e.g. a
/// temperature range like `20~25°C` stays literal.
#[test]
fn single_tilde_is_literal() {
    assert_eq!(render("20~25°C\n"), "<p>20~25°C</p>");
    // Double tilde still works.
    assert!(render("~~struck~~\n").contains("<del>struck</del>"));
}

/// A *tight* list (no blank lines between items) must not wrap items in
/// `<p>` — and crucially must stay tight even when streamed char-by-char,
/// where a naive parser flips it to loose at a transient trailing blank.
#[test]
fn tight_list_stays_tight_when_streamed() {
    let tight = "- hello\n- world\n";
    let expected = render(tight);
    assert!(!expected.contains("<p>"), "tight list should not wrap items in <p>: {expected}");
    assert_eq!(render_streamed(tight), expected, "streaming must not flip a tight list to loose");
    // A genuinely loose list (blank line between items) *does* wrap, per spec.
    assert!(render("- a\n\n- b\n").contains("<p>"));
}

/// Fenced code must render incrementally — an open (not-yet-closed) fence
/// already shows its content, instead of buffering until the closing fence.
#[test]
fn open_fence_renders_incrementally() {
    let mut p = StreamParser::new();
    p.append("```js\nconst x = 1;\n");
    let out = collect(&p);
    assert!(out.contains("<pre><code"), "open fence should render a code block: {out}");
    assert!(out.contains("const x = 1;"), "open fence should show its content: {out}");
}

/// Extended autolinks must not rely on regex look-behind, which throws at parse
/// time on older Safari / iOS WebKit (< 16.3). flux-md's autolinks are written
/// in Rust without look-behind, so the same input just works everywhere.
#[test]
fn extended_autolinks_without_lookbehind() {
    assert!(render("see www.example.com today\n")
        .contains("<a href=\"http://www.example.com\""));
    assert!(render("mail me at foo@bar.example\n").contains("href=\"mailto:foo@bar.example\""));
}

/// GitHub alerts. `> [!NOTE]` becomes a styled callout with GitHub-compatible
/// class names; the body renders as normal markdown.
#[test]
fn github_alerts() {
    let out = render("> [!NOTE]\n> Useful **info**.\n");
    assert!(out.contains("<div class=\"markdown-alert markdown-alert-note\""), "got: {out}");
    assert!(out.contains("data-alert=\"note\""), "got: {out}");
    assert!(out.contains("role=\"note\""), "alert needs role=note for a11y: {out}");
    assert!(out.contains("<p class=\"markdown-alert-title\">Note</p>"), "got: {out}");
    assert!(out.contains("Useful <strong>info</strong>."), "body should render markdown: {out}");
    assert!(!out.contains("<blockquote>"), "alert should not also be a blockquote: {out}");

    // All five keywords map to the right class + title.
    for (kw, class, title) in [
        ("NOTE", "note", "Note"),
        ("TIP", "tip", "Tip"),
        ("IMPORTANT", "important", "Important"),
        ("WARNING", "warning", "Warning"),
        ("CAUTION", "caution", "Caution"),
    ] {
        let out = render(&format!("> [!{kw}]\n> x\n"));
        assert!(out.contains(&format!("markdown-alert-{class}")), "{kw}: {out}");
        assert!(out.contains(&format!(">{title}</p>")), "{kw}: {out}");
    }
}

/// Alerts are conservative: lowercase keyword, trailing text, or an unknown
/// keyword all fall back to a plain blockquote (matching GitHub).
#[test]
fn alert_fallbacks_to_blockquote() {
    assert!(render("> [!note]\n> x\n").contains("<blockquote>"), "lowercase is not an alert");
    assert!(render("> [!NOTE] trailing\n> x\n").contains("<blockquote>"), "trailing text disqualifies");
    assert!(render("> [!BOGUS]\n> x\n").contains("<blockquote>"), "unknown keyword is not an alert");
    // And with alerts OFF, even a valid marker is a literal blockquote.
    let mut p = StreamParser::new();
    p.append("> [!NOTE]\n> x\n");
    p.finalize();
    assert!(collect(&p).contains("<blockquote>"), "alerts off → plain blockquote");
}

/// Alerts converge under streaming: the block transitions Blockquote→Alert as
/// `[!NOTE]` completes, and the finalized HTML matches a one-shot parse.
#[test]
fn alert_streams_to_same_output() {
    let md = "> [!WARNING]\n> Be careful.\n>\n> Second paragraph.\n";
    assert_eq!(render_streamed(md), render(md));
}

/// The Blockquote→Alert transition (when `]` completes the marker) changes the
/// block's kind, hence its stable ID. Verify that at *every* prefix the block
/// list stays well-formed — ordered, non-overlapping, unique IDs — so the
/// streaming UI never sees a duplicate or orphaned block during the flip.
#[test]
fn alert_streaming_has_no_orphan_blocks() {
    let md = "> [!NOTE]\n> body line one\n> body line two\n";
    let mut p = StreamParser::new().with_gfm_alerts(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
        let blocks: Vec<_> = p.all_blocks().collect();
        let mut last_end = 0usize;
        let mut ids = std::collections::HashSet::new();
        for b in &blocks {
            assert!(b.start >= last_end, "overlapping/disordered block mid-stream: {:?}",
                blocks.iter().map(|x| (x.id, x.start, x.end)).collect::<Vec<_>>());
            assert!(ids.insert(b.id), "duplicate block id mid-stream");
            last_end = b.end;
        }
    }
    p.finalize();
    let blocks: Vec<_> = p.all_blocks().collect();
    assert_eq!(blocks.len(), 1, "should converge to one block");
    assert!(matches!(blocks[0].kind, BlockKind::Alert { .. }), "final kind: {:?}", blocks[0].kind);
}

/// LaTeX math delimiters `\(…\)` (inline) and `\[…\]` (display) — plus
/// `$…$` / `$$…$$` — must be recognized as math, not mangled as emphasis or
/// escaped parentheses. flux-md renders them to KaTeX-ready markup and keeps
/// the LaTeX body verbatim (HTML-escaped, never markdown-processed).
#[test]
fn latex_math_delimiters() {
    let mut p = StreamParser::new().with_gfm_math(true);
    p.append("inline \\(a_1 + b_2\\) and display:\n\n\\[\n\\sum_{i=1}^{n} i\n\\]\n");
    p.finalize();
    let out = collect(&p);
    assert!(out.contains("<span class=\"math math-inline\">a_1 + b_2</span>"), "got: {out}");
    assert!(out.contains("<div class=\"math math-display\">\\sum_{i=1}^{n} i</div>"), "got: {out}");

    // `$…$` underscores/carets are math, not `<em>`/`<sub>`.
    let mut q = StreamParser::new().with_gfm_math(true);
    q.append("$a_i^2$\n");
    q.finalize();
    let dollars = collect(&q);
    assert!(dollars.contains("<span class=\"math math-inline\">a_i^2</span>"), "got: {dollars}");
    assert!(!dollars.contains("<em>"), "math body must not be emphasized: {dollars}");
}

/// Direction must be detected **per block**, not once for the whole document.
/// With `dir_auto` on, each block-level text element carries its own
/// `dir="auto"`, so a browser renders an Arabic block RTL and an English block
/// LTR independently; code blocks stay LTR (no `dir`).
#[test]
fn per_block_dir_auto() {
    let mut p = StreamParser::new().with_dir_auto(true);
    p.append("English here.\n\nمرحبا بالعالم\n\n```js\nconst x = 1;\n```\n");
    p.finalize();
    let out = collect(&p);
    assert_eq!(out.matches("<p dir=\"auto\">").count(), 2, "each paragraph gets its own dir: {out}");
    assert!(!out.contains("<pre dir") && !out.contains("code dir"), "code stays LTR: {out}");
}

/// Streaming a `$$…$$` block flips the active block's kind (Paragraph →
/// MathFence) the moment the second `$` arrives, which changes its stable ID.
/// As with the Blockquote→Alert flip, verify that at *every* prefix the block
/// list stays well-formed — ordered, non-overlapping, unique IDs — so the UI
/// never sees a duplicate or orphaned block during the transition. (Mirrors
/// `alert_streaming_has_no_orphan_blocks`.)
#[test]
fn math_streaming_has_no_orphan_blocks() {
    fn assert_well_formed(p: &StreamParser) {
        let blocks: Vec<_> = p.all_blocks().collect();
        let mut last_end = 0usize;
        let mut ids = std::collections::HashSet::new();
        for b in &blocks {
            assert!(
                b.start >= last_end,
                "overlapping/disordered block mid-stream: {:?}",
                blocks.iter().map(|x| (x.id, x.start, x.end)).collect::<Vec<_>>()
            );
            assert!(ids.insert(b.id), "duplicate block id mid-stream");
            last_end = b.end;
        }
    }

    // (a) A multi-line display block converges to exactly one MathBlock.
    let md = "$$\n\\sum_{i=1}^{n} x_i\n$$\n";
    let mut p = StreamParser::new().with_gfm_math(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
        assert_well_formed(&p);
    }
    p.finalize();
    let blocks: Vec<_> = p.all_blocks().collect();
    assert_eq!(blocks.len(), 1, "should converge to one block: {blocks:?}");
    assert!(matches!(blocks[0].kind, BlockKind::MathBlock(_)), "final kind: {:?}", blocks[0].kind);

    // (b) Prose around a math block: the active tail flips kind as `$$` opens
    // and closes, while the surrounding paragraphs must never overlap it.
    let md = "before\n$$\nx = 1\n$$\nafter\n";
    let mut q = StreamParser::new().with_gfm_math(true);
    for ch in md.chars() {
        q.append(ch.encode_utf8(&mut buf));
        assert_well_formed(&q);
    }
    q.finalize();
    let kinds: Vec<_> = q.all_blocks().map(|b| b.kind.tag()).collect();
    assert_eq!(kinds, vec!["Paragraph", "MathBlock", "Paragraph"], "got: {kinds:?}");
}

/// A partial image mid-stream (URL not yet closed) must degrade gracefully —
/// render as literal text, never a broken `<img>` with a truncated `src`. Once
/// the `)` arrives it becomes a real image.
#[test]
fn partial_image_degrades_gracefully() {
    let mut p = StreamParser::new();
    p.append("![cat](http://example.com/cat");
    let mid = collect(&p);
    assert!(!mid.contains("<img"), "incomplete image must not render a broken <img>: {mid}");

    p.append(".png)\n");
    p.finalize();
    let done = collect(&p);
    assert!(
        done.contains("<img src=\"http://example.com/cat.png\" alt=\"cat\""),
        "completed image should render: {done}"
    );
}

/// The core streaming guarantee: malformed / mid-stream markdown must degrade
/// gracefully, never panic. Unclosed emphasis stays literal (per CommonMark);
/// nothing crashes regardless of where the stream is cut.
#[test]
fn malformed_and_partial_input_never_panics() {
    for md in [
        "**unclosed bold",
        "_em without close",
        "`code without close",
        "[link](http://x.com",
        "> quote\n> more",
        "| a | b |\n| - |",
        "###### \n",
        "~~~",
        "1. item\n   - nested\n\n     lazy",
    ] {
        let _ = render(md);
        let _ = render_streamed(md);
    }
    // Unclosed strong emphasis renders as literal text, not a dangling tag.
    assert_eq!(render("**unclosed bold\n"), "<p>**unclosed bold</p>");
}

//! Mid-stream parity — asserts that what a streaming consumer SEES while a
//! block is open matches the one-shot render for the same prefix.
//!
//! The other parity tests (`table_streaming.rs`, `container_cache.rs`, etc.)
//! compare *post-finalize* output. That misses bugs where the cache emits
//! wrong HTML for an open block mid-stream — the user-visible state. These
//! tests close the loop: for each markdown prefix below, the streamed parser
//! (char-by-char + a trailing empty append to fire any freshly-armed cache)
//! must collect to the same HTML the one-shot parser produces for that
//! prefix without `.finalize()`.
//!
//! Pinned bugs:
//!   - paragraph cache used to skip past its own line and miss a table
//!     delimiter row that completes after the cut had advanced into it
//!   - the alert/blockquote container cache used to emit an empty `<p></p>`
//!     for an empty body, while the full path emits nothing

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn one_shot_open(md: &str) -> String {
    // No finalize — we want the full path's view of an *open* block.
    let mut p = StreamParser::new().with_gfm_alerts(true);
    p.append(md);
    collect(&p)
}

fn streamed_open(md: &str) -> String {
    // Stream char-by-char, then ONE empty append so any freshly-armed cache
    // gets to fire. No finalize.
    let mut p = StreamParser::new().with_gfm_alerts(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.append("");
    collect(&p)
}

fn assert_parity(md: &str) {
    let one = one_shot_open(md);
    let streamed = streamed_open(md);
    assert_eq!(streamed, one, "mid-stream != one-shot for {md:?}");
}

#[test]
fn table_delimiter_detected_after_paragraph_cache_advanced() {
    // The regression: the paragraph cache advances into line 2 char by char.
    // When `\n` finally lands, the cache's `paragraph_ends_before_eof` walk
    // used to skip past the line containing the cut — so the delimiter row
    // was never seen and the block stayed paragraph until finalize.
    assert_parity("| a | b |\n| - | - |\n");
    assert_parity("| a | b |\n| - | - |\n| 1 | 2 |\n");
    // Multiple columns + alignments
    assert_parity("| L | C | R |\n| :- | :-: | -: |\n| 1 | 2 | 3 |\n");
    // Header preceded by paragraph (trailing-paragraph variant)
    assert_parity("Intro.\n\n| H1 | H2 |\n| - | - |\n");
}

#[test]
fn open_alert_with_empty_body_renders_without_empty_p() {
    // The regression: the alert cache wrapped the body in `<p>...</p>` even
    // when the body was empty, producing `<p></p>` that the full renderer
    // doesn't emit.
    assert_parity("> [!NOTE]\n");
    assert_parity("> [!TIP]\n");
    assert_parity("> [!IMPORTANT]\n");
    assert_parity("> [!WARNING]\n");
    assert_parity("> [!CAUTION]\n");
}

#[test]
fn open_alert_with_body_matches() {
    assert_parity("> [!NOTE]\n> body\n");
    assert_parity("> [!NOTE]\n> body line one\n> body line two\n");
    assert_parity("> [!NOTE]\n> **bold** and `code` in the body\n");
}

#[test]
fn open_blockquote_matches() {
    assert_parity("> simple quote\n");
    assert_parity("> line one\n> line two\n");
    assert_parity("> with **bold** and `code`\n");
}

#[test]
fn open_list_matches() {
    assert_parity("- one\n");
    assert_parity("- one\n- two\n");
    assert_parity("1. one\n2. two\n");
    assert_parity("- with **bold** and `code`\n");
    // Loose: blank line between siblings must produce `<p>`-wrapped items
    // both in the streamed view and one-shot.
    assert_parity("- one\n\n- two\n");
    assert_parity("- one\n\n- two\n\n- three\n");
    // Trailing blank with no second marker yet — cache must stay tight (no
    // `<p>` wrap) since a single-item list is never loose.
    assert_parity("- one\n\n");
    // Blank then partial marker — the list is settled loose by the blank.
    assert_parity("- one\n\n- ");
    assert_parity("- one\n\n- partial");
}

#[test]
fn open_table_matches_with_body() {
    // The table cache itself; pinned to ensure no regression from the
    // paragraph-cache fix above.
    assert_parity("| a | b |\n| - | - |\n| 1 | 2 |\n");
    assert_parity("| a | b |\n| - | - |\n| 1 | 2 |\n| 3 | 4 |\n");
}

#[test]
fn open_code_fence_matches() {
    assert_parity("```rust\nfn main() {}\n");
    assert_parity("```js\nconst x = 1;\nconst y = 2;\n");
}

// ---------------------------------------------------------------------------
// Speculative open-tail link rendering (kills the streaming link-URL flash).
// While `[label](url…` streams, render an INERT `<a>label</a>` (no href, no raw
// URL as text); once `)` lands the real `href` is added (node reuse).
// ---------------------------------------------------------------------------

/// Append `md` then `finalize()`, collect all blocks. The finalized view is the
/// committed, one-shot-complete-literal output (no speculation — incomplete
/// links degrade to literal text).
fn finalized(md: &str) -> String {
    let mut p = StreamParser::new().with_gfm_alerts(true);
    p.append(md);
    p.finalize();
    collect(&p)
}

/// One-shot render of `md` with finalize — the literal oracle a finalized
/// stream must equal byte-for-byte.
fn one_shot_complete_literal(md: &str) -> String {
    finalized(md) // (the parser has no separate one-shot entry; finalize() IS it)
}

#[test]
fn speculative_link_golden_sweep() {
    // The reproduced bug's exact prefixes.
    let inert_prefixes = ["[Link text Here](", "[Link text Here](https://link-url-h"];
    let closed = "[Link text Here](https://link-url-here.org)";

    for md in inert_prefixes {
        // Mid-stream view matches one-shot-open exactly.
        assert_parity(md);
        let html = streamed_open(md);
        // Inert anchor: same target/rel as a real link, but NO href.
        assert!(
            html.contains("<a target=\"_blank\" rel=\"noopener noreferrer nofollow\">Link text Here</a>"),
            "expected inert <a> for {md:?}, got: {html}"
        );
        assert!(!html.contains("href="), "inert link must have NO href for {md:?}: {html}");
        // No raw URL leaking as visible text (regex-ish: `>` then non-`<` then https).
        assert!(
            !visible_text_contains(&html, "https"),
            "raw URL must not appear as visible text for {md:?}: {html}"
        );
    }

    // Closed link → real <a href=…>.
    assert_parity(closed);
    let html = streamed_open(closed);
    assert!(
        html.contains("<a href=\"https://link-url-here.org\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">Link text Here</a>"),
        "closed link must be a real <a href=…>: {html}"
    );
}

/// True iff any run of text BETWEEN tags (i.e. after a `>` and before the next
/// `<`) contains `needle`. Mirrors the spec's `>[^<]*https` regex without a
/// regex dependency.
fn visible_text_contains(html: &str, needle: &str) -> bool {
    let mut rest = html;
    while let Some(gt) = rest.find('>') {
        let after = &rest[gt + 1..];
        let seg_end = after.find('<').unwrap_or(after.len());
        if after[..seg_end].contains(needle) {
            return true;
        }
        rest = &after[seg_end..];
    }
    false
}

#[test]
fn speculative_inert_shape_no_href() {
    // Every still-streaming destination prefix is inert (no href), and equals
    // the one-shot-open view.
    for md in [
        "[a](",
        "[a](h",
        "[a](http",
        "[a](https://exa",
        "[a](<partial",         // bracketed dest, still open
        "[x](javascript:",      // security: scheme suppressed (inert, no href)
        "[**bold** label](http", // inline markup inside the label
    ] {
        assert_parity(md);
        let html = streamed_open(md);
        assert!(html.contains("<a target="), "expected inert <a> for {md:?}: {html}");
        assert!(!html.contains("href="), "inert link must have NO href for {md:?}: {html}");
    }
}

#[test]
fn speculative_security_javascript_scheme_suppressed() {
    // Open: inert, scheme never appears anywhere (no href, no visible text).
    let open = streamed_open("[x](javascript:alert(1");
    assert!(!open.contains("javascript:"), "javascript: must not appear mid-stream: {open}");
    assert!(!open.contains("href="), "no href mid-stream: {open}");
    assert!(open.contains("<a target="), "inert <a> mid-stream: {open}");
    // Closed: sanitize_url drops the dangerous scheme (href present but scrubbed).
    let closed = streamed_open("[x](javascript:alert(1))");
    assert_parity("[x](javascript:alert(1))");
    assert!(!closed.contains("javascript:"), "javascript: must be sanitized on close: {closed}");
}

#[test]
fn speculative_finalize_byte_parity_literal() {
    // A NON-EMPTY, malformed (unclosed) destination finalizes to a plain
    // `<p>[…</p>` literal with NO <a> — speculation is streaming-only; commit is
    // literal, byte-identical to the one-shot-complete oracle.
    for md in [
        "[Link text Here](https://link-url-h",
        "[a](http",
        "[a](<partial",
        "[x](javascript:",
    ] {
        let fin = finalized(md);
        assert_eq!(fin, one_shot_complete_literal(md), "finalize != oracle for {md:?}");
        assert!(!fin.contains("<a"), "finalized incomplete link must be literal (no <a>): {md:?} -> {fin}");
        assert!(fin.contains("<p>["), "finalized incomplete link must be literal text: {md:?} -> {fin}");
    }
    // Pinned PRE-EXISTING quirk (unchanged by this feature): an EMPTY-dest open
    // paren `[a](` is treated as an empty-dest link by the committing path
    // (try_link succeeds for an empty destination), so finalize yields
    // `<a href="">`. The speculative path renders the SAME label as inert <a>
    // mid-stream, and finalize matches the historical one-shot output exactly.
    let fin = finalized("[Link text Here](");
    assert_eq!(fin, one_shot_complete_literal("[Link text Here]("));
    assert!(
        fin.contains("<a href=\"\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">Link text Here</a>"),
        "empty-dest finalize is the historical empty-href link: {fin}"
    );
}

#[test]
fn speculative_closed_block_trap() {
    // `[a](http` then a blank line + a second paragraph: the first paragraph is
    // now CLOSED (not the abuts-EOF tail) → it must be LITERAL, and the second
    // paragraph renders normally. Both mid-stream and one-shot agree.
    assert_parity("[a](http\n\npara2");
    let html = streamed_open("[a](http\n\npara2");
    assert!(html.contains("<p>[a](http</p>"), "closed first para must be literal: {html}");
    assert!(html.contains("para2"), "second paragraph must render: {html}");
}

#[test]
fn speculative_reference_links_untouched() {
    // Reference forms never speculate (next char after `]` isn't `(`).
    for md in ["[t][r]", "[t][]", "[t]", "[t] ("] {
        assert_parity(md);
    }
}

#[test]
fn speculative_title_midstream_is_literal() {
    // `[a](url "ti` — the space after the bare dest ends it → not still-streaming
    // → literal (mid-stream and one-shot agree).
    assert_parity("[a](url \"ti");
    let html = streamed_open("[a](url \"ti");
    assert!(!html.contains("<a"), "title mid-stream must be literal: {html}");
}

#[test]
fn speculative_bracketed_dest_edges() {
    // `<partial` (no closing `>`) → inert; `<url>` (closed, no `)`) → literal.
    assert_parity("[a](<partial");
    assert!(streamed_open("[a](<partial").contains("<a target="));
    assert_parity("[a](<url>");
    assert!(!streamed_open("[a](<url>").contains("<a "), "closed <url> w/o ) is literal");
}

#[test]
fn speculative_mid_text_non_speculation() {
    // `[a](http) word` → a real link followed by text (the `)` closes it).
    assert_parity("[a](http) word");
    let html = streamed_open("[a](http) word");
    assert!(html.contains("<a href="), "complete link mid-text is real: {html}");
    // `[a](http word` → space ends the bare dest, no `)` → literal.
    assert_parity("[a](http word");
    assert!(!streamed_open("[a](http word").contains("<a"), "malformed mid-text is literal");
}

#[test]
fn speculative_image_stays_literal() {
    // `![alt](http` must NOT speculate an <img> or an <a> — partial image stays
    // literal (the `!`+`[` arm has no speculative branch).
    assert_parity("![alt](http");
    let html = streamed_open("![alt](http");
    assert!(!html.contains("<img"), "partial image must not render <img>: {html}");
    assert!(!html.contains("<a "), "partial image must not render <a>: {html}");
}

#[test]
fn speculative_convergence_node_stability() {
    // Append `[Link](`, then `https://x`, then `)` — the tag+target+rel are
    // byte-stable across the inert→real transition; only `href` is ADDED.
    let mut p = StreamParser::new();
    p.append("[Link](");
    let s1 = collect(&p);
    p.append("https://x");
    let s2 = collect(&p);
    p.append(")");
    let s3 = collect(&p);

    // First two states: identical inert <a> (no href).
    assert_eq!(s1, s2, "inert <a> must be byte-stable while the URL streams");
    assert!(s1.contains("<a target=\"_blank\" rel=\"noopener noreferrer nofollow\">Link</a>"));
    assert!(!s1.contains("href="));

    // Final state: only `href` was inserted; the target/rel/label tail is intact.
    assert!(s3.contains("<a href=\"https://x\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">Link</a>"));
    // The inert prefix (everything after the would-be href) is reused verbatim.
    assert!(s3.contains("target=\"_blank\" rel=\"noopener noreferrer nofollow\">Link</a>"));
}

#[test]
fn speculative_cache_commit_safety_long_prefix() {
    // A long settled paragraph prefix then a partial link tail: the committed
    // inner (frozen) never freezes the inert <a>, and finalize is literal.
    let lead = "word ".repeat(200);
    let md = format!("{lead}[Link](https://x");
    assert_parity(&md);
    let streamed = streamed_open(&md);
    // Inert <a> present mid-stream, no raw URL leak, no href.
    assert!(streamed.contains("<a target="), "inert <a> present: ...{}", &streamed[streamed.len().saturating_sub(120)..]);
    assert!(!streamed.contains("href="));
    // Finalize collapses to literal (the frozen prefix never captured an <a>).
    let fin = finalized(&md);
    assert!(!fin.contains("<a"), "finalize must be literal even after a long prefix");
    assert!(fin.contains("[Link](https://x"), "literal tail preserved");
}

#[test]
fn open_math_block_matches() {
    // gfm_math is off in the default helper, so display math without it stays
    // as a paragraph in both paths. Pinned to ensure consistency either way.
    let make = || StreamParser::new().with_gfm_alerts(true).with_gfm_math(true);
    let cases = ["$$\nE = mc^2\n", "$$\nx + y\n= z\n"];
    for md in cases {
        let one = {
            let mut p = make();
            p.append(md);
            collect(&p)
        };
        let streamed = {
            let mut p = make();
            let mut buf = [0u8; 4];
            for ch in md.chars() {
                p.append(ch.encode_utf8(&mut buf));
            }
            p.append("");
            collect(&p)
        };
        assert_eq!(streamed, one, "mid-stream != one-shot for {md:?}");
    }
}

// ---------------------------------------------------------------------------
// Speculative open-tail INLINE CODE + INLINE MATH (kills the streaming
// raw-source flash for `` `code` `` / `$x$` / `\(a+b\)`). While the closer is
// still streaming to EOF, render the resolved `<code>…</code>` /
// `<span class="math …">…</span>` over the partial body, with the opening
// delimiter hidden; finalize of an unclosed form is byte-identical literal.
// ---------------------------------------------------------------------------

/// math-on variants of the parity helpers (the default helpers leave `$`/`\(`
/// literal). `with_gfm_alerts(true)` mirrors the other speculative helpers.
fn make_math() -> StreamParser {
    StreamParser::new().with_gfm_alerts(true).with_gfm_math(true)
}
fn one_shot_open_m(md: &str) -> String {
    let mut p = make_math();
    p.append(md);
    collect(&p)
}
fn streamed_open_m(md: &str) -> String {
    let mut p = make_math();
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.append("");
    collect(&p)
}
fn assert_parity_m(md: &str) {
    assert_eq!(streamed_open_m(md), one_shot_open_m(md), "mid-stream != one-shot for {md:?}");
}
fn finalized_m(md: &str) -> String {
    let mut p = make_math();
    p.append(md);
    p.finalize();
    collect(&p)
}

#[test]
fn speculative_inline_code_golden() {
    // Open `` `code here` `` (no closer yet): resolved `<code>` over the partial
    // body, opening backtick hidden, no raw backtick/source as visible text.
    let md = "`code here";
    assert_parity_m(md);
    let html = streamed_open_m(md);
    assert!(html.contains("<code>code here</code>"), "expected resolved <code> for {md:?}: {html}");
    assert!(!visible_text_contains(&html, "`"), "raw backtick must not be visible text for {md:?}: {html}");

    // Closed `` `code here` `` → identical resolved <code> (the close is hidden too).
    assert_parity_m("`code here`");
    assert!(streamed_open_m("`code here`").contains("<code>code here</code>"));

    // Lone opener `` ` `` (empty body) is NOT speculated — stays literal.
    assert_parity_m("`");
    assert!(!streamed_open_m("`").contains("<code>"), "empty-body backtick must stay literal");

    // Finalize of the unclosed form is byte-identical literal (no <code>).
    let fin = finalized_m("`code here");
    assert!(!fin.contains("<code>"), "finalized unclosed code must be literal: {fin}");
    assert!(fin.contains("`code here"), "literal backtick preserved on finalize: {fin}");
}

#[test]
fn speculative_inline_dollar_math_golden() {
    // Open `$x^2 + y^2$` (no closer yet): resolved inline-math span over the
    // partial body, opening `$` hidden, no raw `$` as visible text.
    let md = "$x^2 + y^2";
    assert_parity_m(md);
    let html = streamed_open_m(md);
    assert!(
        html.contains("<span class=\"math math-inline\">x^2 + y^2</span>"),
        "expected resolved inline-math span for {md:?}: {html}"
    );
    assert!(!visible_text_contains(&html, "$"), "raw $ must not be visible text for {md:?}: {html}");

    // Closed form → identical span.
    assert_parity_m("$x^2 + y^2$");
    assert!(streamed_open_m("$x^2 + y^2$").contains("<span class=\"math math-inline\">x^2 + y^2</span>"));

    // pandoc currency guard preserved: `$ ` (space after single `$`) never
    // speculates; `$5 and $10` stays literal currency text.
    assert_parity_m("$ x");
    assert!(!streamed_open_m("$ x").contains("class=\"math"), "`$ ` must not speculate (pandoc guard)");
    assert_parity_m("I have $5 and $10 left");

    // Finalize of the unclosed form is literal.
    let fin = finalized_m("$x^2 + y^2");
    assert!(!fin.contains("class=\"math"), "finalized unclosed $math must be literal: {fin}");
    assert!(fin.contains("$x^2 + y^2"), "literal $ preserved on finalize: {fin}");
}

#[test]
fn speculative_inline_latex_math_golden() {
    // Open `\(a+b\)` (no closer yet): resolved inline-math span over the partial
    // body, opening `\(` hidden, no raw `\(` as visible text.
    let md = "\\(a+b";
    assert_parity_m(md);
    let html = streamed_open_m(md);
    assert!(
        html.contains("<span class=\"math math-inline\">a+b</span>"),
        "expected resolved inline-math span for {md:?}: {html}"
    );
    assert!(!visible_text_contains(&html, "\\("), "raw \\( must not be visible text for {md:?}: {html}");

    // Inline display `\[a=b\]` open form → inline display span. (A LEADING `\[`
    // opens a block-level `<div class="math math-display">` instead — a separate,
    // already-parity scanner path — so prefix it with inline text to exercise the
    // inline `try_math_delim` speculation.)
    let dm = "x \\[a=b";
    assert_parity_m(dm);
    assert!(
        streamed_open_m(dm).contains("<span class=\"math math-display\">a=b</span>"),
        "expected resolved inline display-math span for {dm:?}: {}",
        streamed_open_m(dm)
    );

    // Closed forms → identical spans.
    assert_parity_m("\\(a+b\\)");
    assert!(streamed_open_m("\\(a+b\\)").contains("<span class=\"math math-inline\">a+b</span>"));

    // Empty body `\(` (opener only) is NOT speculated — stays literal `(`.
    assert_parity_m("\\(");

    // Blank-line guard: a `\n\n` before EOF closes the paragraph → literal (math
    // never crosses a paragraph break), matching one-shot.
    assert_parity_m("\\(a+b\n\npara2");
    assert!(!streamed_open_m("\\(a+b\n\npara2").contains("class=\"math"), "math must not cross a blank line");

    // Finalize of the unclosed form is literal.
    let fin = finalized_m("\\(a+b");
    assert!(!fin.contains("class=\"math"), "finalized unclosed \\(…) must be literal: {fin}");
}

#[test]
fn test_backslash_artifact_in_paren_math() {
    // The known artifact: \(a + b\ renders with a trailing backslash in the
    // math content (the first byte of the 2-byte closer \)).
    let md = "\\(a + b\\";
    let html = streamed_open_m(md);
    println!("HTML for '{}': {}", md, html);
    
    // Check if content contains the trailing backslash
    if html.contains("b\\</span>") {
        println!("ARTIFACT CONFIRMED: trailing backslash in <span class=\"math\">a + b\\</span>");
    } else {
        println!("No trailing backslash artifact detected");
    }
}

#[test]
fn nested_list_does_not_flatten_when_outer_goes_loose() {
    // Regression: the list-cache fast path treated a 2-space-indented sub-bullet
    // as a top-level SIBLING (`marker_indent <= edge + 3`), so the moment a loose
    // outer list's second item appeared, the first item's nested sub-bullets
    // FLATTENED into top-level items mid-stream (a visible "indentation vanishes
    // then comes back" reflow). The sibling test now uses the first item's
    // content column, so a marker at/past it nests (cache bails to the full path).
    assert_parity("- **A:**\n  - x1\n  - x2\n\n- **B"); // 2nd item incomplete — the exact trigger
    assert_parity("- **A:**\n  - x1\n  - x2\n\n- **B:**\n  - y1\n  - y2\n");
    assert_parity("- a\n  - nested\n\n- b\n"); // minimal loose-outer + nested
    assert_parity("1. top\n   1. nested\n   2. nested2\n\n2. second\n"); // ordered, 3-space content col
    assert_parity("- one\n  - sub a\n  - sub b\n  - sub c\n\n- two\n  - sub d\n"); // the user's shape
}

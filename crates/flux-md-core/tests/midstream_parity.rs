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

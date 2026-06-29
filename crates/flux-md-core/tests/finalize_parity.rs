//! Finalize parity — the FINALIZED document must be identical whether the
//! bytes arrived char-by-char or in one shot.
//!
//! `midstream_parity.rs` compares the *open* (pre-finalize) view; it cannot
//! catch a bug where a block is COMMITTED (frozen) too early during streaming,
//! because committed blocks never change again. This harness finalizes both
//! paths and compares — the streamed finalize must equal the one-shot finalize.
//!
//! Pinned bug (the `reparse_tail` premature-commit): when the still-growing
//! final line transiently classified as a block start (`#x` → empty ATX
//! heading, `</p` → type-6 HTML, lone `*`/`-` → new list bullet), the parser
//! committed the PENULTIMATE block. Once the final line completed into a lazy
//! continuation (`#hashtag`, `</pre>`, `*emph*`), the now-frozen penultimate
//! could not merge back, so the streamed finalize permanently diverged from
//! the one-shot finalize.

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn one_shot_final(md: &str) -> String {
    let mut p = StreamParser::new().with_gfm_alerts(true);
    p.append(md);
    p.finalize();
    collect(&p)
}

fn streamed_final(md: &str) -> String {
    let mut p = StreamParser::new().with_gfm_alerts(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

fn assert_final_parity(md: &str) {
    let one = one_shot_final(md);
    let streamed = streamed_final(md);
    assert_eq!(streamed, one, "streamed finalize != one-shot finalize for {md:?}");
}

#[test]
fn lazy_continuation_after_transient_heading_marker() {
    // At buffer "...\n#" the lone '#' is a valid (empty) ATX heading and
    // interrupts the paragraph; "#hashtag" (no space) is NOT a heading, so the
    // line is a lazy continuation of the paragraph.
    assert_final_parity("A paragraph here\n#hashtag\n");
    assert_final_parity("A paragraph here\n#hashtag");
    assert_final_parity("first\n#notheading\nmore\n");
}

#[test]
fn lazy_continuation_after_transient_html_marker() {
    // At buffer "...\n</p" the lone "</p" matches type-6 HTML (p is a block
    // tag) and interrupts; the full "</pre>" is not a type-6 block, so it is a
    // lazy continuation of the paragraph.
    assert_final_parity("Hello world\n</pre>\n");
    assert_final_parity("Hello world\n</pre>");
}

#[test]
fn lazy_continuation_after_transient_list_marker() {
    // At buffer "...\n*" the lone '*' is a new empty bullet (scan yields two
    // list blocks); "*important*" is emphasis — a lazy continuation of the
    // list item's paragraph.
    assert_final_parity("- First point\n*important* note\n");
    assert_final_parity("- First point\n*important* note");
    assert_final_parity("- a\n-not a bullet\n");
}

#[test]
fn ordinary_sequences_still_parity() {
    // Guard against over-deferral regressions: genuinely separate blocks must
    // still finalize identically (and these stream cheaply — the penultimate
    // is NOT held back across a real blank line or a non-continuable block).
    assert_final_parity("para one\n\npara two\n");
    assert_final_parity("# Heading\n\nsome text\n");
    assert_final_parity("# Heading\nplain paragraph after\n");
    assert_final_parity("para\n\n# Real Heading\n");
    assert_final_parity("```\ncode\n```\nafter\n");
    assert_final_parity("- a\n- b\n- c\n");
    assert_final_parity("> quote\n\nafter\n");
    assert_final_parity("text\n\n## h2\n\nmore text\n");
}

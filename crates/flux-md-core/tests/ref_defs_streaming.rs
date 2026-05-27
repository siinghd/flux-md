//! Correctness net for committing a long trailing run of definition blocks
//! (link-reference defs and footnote defs) while streaming. Such a run produces
//! no renderable blocks, so the parser must still advance its committed offset
//! over the completed defs — otherwise the whole run is re-scanned every append
//! (O(n²)). Streaming char-by-char must produce byte-identical HTML to a
//! one-shot parse for every shape below.

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn render_with(md: &str, footnotes: bool) -> String {
    let mut p = StreamParser::new().with_gfm_autolinks(true).with_gfm_footnotes(footnotes);
    p.append(md);
    p.finalize();
    collect(&p)
}

fn render_streamed_with(md: &str, footnotes: bool) -> String {
    let mut p = StreamParser::new().with_gfm_autolinks(true).with_gfm_footnotes(footnotes);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

fn parity(md: &str, footnotes: bool) {
    assert_eq!(
        render_streamed_with(md, footnotes),
        render_with(md, footnotes),
        "streamed != one-shot for {:?}",
        &md[..md.len().min(60)]
    );
}

#[test]
fn long_ref_def_run_at_tail() {
    // The bench shape (sans forward references, which are a separate documented
    // limitation): some prose, then a long run of single-line definitions with no
    // blank lines between them. The def run is what must commit incrementally.
    let mut md = String::new();
    for i in 0..200 {
        md.push_str(&format!("Plain paragraph number {i} with ordinary prose text.\n\n"));
    }
    for i in 0..200 {
        md.push_str(&format!("[r{i}]: https://example.com/page/{i} \"Title {i}\"\n"));
    }
    // Resolve a couple of them via backward references after the run.
    md.push_str("\nSee [first][r0] and [mid][r100].\n");
    parity(&md, false);
}

#[test]
fn multiline_title_defs() {
    // The critical case for "commit all but the last def": each title is on the
    // line *after* its def, so a def isn't complete until the next line proves
    // it isn't a title continuation.
    let md = "[a]: /url-a\n\"Title A\"\n[b]: /url-b\n\"Title B\"\n[c]: /url-c\n\"Title C\"\n";
    parity(md, false);
    // And with backward references that should resolve.
    let md2 = "[a]: /url-a\n\"Title A\"\n[b]: /url-b\n\"Title B\"\n\nSee [one][a] and [two][b].\n";
    parity(md2, false);
}

#[test]
fn same_line_title_defs() {
    let md = "[a]: /url-a \"Title A\"\n[b]: /url-b \"Title B\"\n[c]: /url-c \"Title C\"\n";
    parity(md, false);
}

#[test]
fn backward_ref_resolves_after_commit() {
    // A definition arrives first (and commits), then a paragraph references it.
    // Backward references must still resolve once the def run has committed.
    let mut md = String::new();
    for i in 0..100 {
        md.push_str(&format!("[r{i}]: https://example.com/{i}\n"));
    }
    md.push_str("\nSee [first][r0] and [last][r99].\n");
    parity(&md, false);
}

#[test]
fn footnote_def_run() {
    // Footnotes on: references in earlier paragraphs, then a long run of footnote
    // definitions (also non-renderable, collected into the section at finalize).
    let mut md = String::from("Intro with [^1] and [^2] and [^3] refs.\n\n");
    for i in 1..=100 {
        md.push_str(&format!("[^{i}]: Footnote body number {i}.\n"));
    }
    parity(&md, true);
}

#[test]
fn blank_lines_interspersed_in_def_run() {
    let md = "[a]: /a\n\n[b]: /b\n\n\n[c]: /c\n[d]: /d\n\n[e]: /e\n";
    parity(md, false);
}

#[test]
fn incomplete_then_complete_trailing_def_converges() {
    // The trailing def is incomplete at intermediate prefixes (no URL yet, then a
    // title continuation), and must converge once finalized. render_streamed
    // feeds one char at a time, so it passes through every incomplete prefix.
    parity("[a]: /a\n[b]: /b\n[c]:", false); // ends mid-def (no url)
    parity("[a]: /a\n[b]: /b\n[c]: /c\n\"Late title\"\n", false); // last def gains a title
    parity("[a]: /a\n[b]: /b\n[c]: /c", false); // last def complete but no trailing newline
}

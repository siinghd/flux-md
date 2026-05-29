//! Opt-in accessibility markup (`with_a11y`). Deviates from strict GFM
//! byte-output, so it is OFF by default (CommonMark/GFM conformance unchanged):
//!   - a tight task-list item wraps its checkbox + text in a `<label>` (so the
//!     box is programmatically associated with its label for screen readers);
//!   - table header cells get `scope="col"`.
//! Only the tight, single-paragraph task item is wrapped — a `<label>` must not
//! wrap a nested list/block. The streaming cache path must match one-shot.

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn render_a11y(md: &str) -> String {
    let mut p = StreamParser::new().with_a11y(true);
    p.append(md);
    p.finalize();
    collect(&p)
}

fn render_a11y_streamed(md: &str) -> String {
    let mut p = StreamParser::new().with_a11y(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

fn render_plain(md: &str) -> String {
    let mut p = StreamParser::new();
    p.append(md);
    p.finalize();
    collect(&p)
}

#[test]
fn task_list_checkbox_wrapped_in_label() {
    let out = render_a11y("- [ ] foo\n- [x] bar\n");
    assert!(
        out.contains("<li><label><input type=\"checkbox\" disabled> foo</label></li>"),
        "unchecked task item must wrap checkbox+text in a label: {out}"
    );
    assert!(
        out.contains("<li><label><input type=\"checkbox\" checked disabled> bar</label></li>"),
        "checked task item must wrap checkbox+text in a label: {out}"
    );
}

#[test]
fn table_header_cells_get_scope_col() {
    let out = render_a11y("| a | b |\n|---|---|\n| 1 | 2 |\n");
    assert!(out.contains("<th scope=\"col\">a</th>"), "th needs scope=col: {out}");
    assert!(out.contains("<th scope=\"col\">b</th>"), "th needs scope=col: {out}");
    // Body cells are never scoped.
    assert!(out.contains("<td>1</td>"), "td must not be scoped: {out}");
}

#[test]
fn off_by_default_is_byte_identical_gfm() {
    // The default parser emits neither a <label> nor scope — strict GFM output.
    for md in [
        "- [ ] foo\n- [x] bar\n",
        "| a | b |\n|---|---|\n| 1 | 2 |\n",
        "- plain item\n- another\n",
    ] {
        let out = render_plain(md);
        assert!(!out.contains("<label>"), "default must not wrap in label: {md:?} -> {out}");
        assert!(!out.contains("scope="), "default must not scope th: {md:?} -> {out}");
    }
    // The exact legacy task-list markup is preserved by default.
    assert!(render_plain("- [ ] foo\n")
        .contains("<li><input type=\"checkbox\" disabled> foo</li>"));
}

#[test]
fn nested_task_item_is_not_label_wrapped() {
    // A `<label>` can't validly wrap a nested list, so only the inner (leaf)
    // task item is wrapped; the outer item (which holds the nested list) is not.
    let out = render_a11y("- [x] foo\n  - [ ] bar\n");
    assert_eq!(out.matches("<label>").count(), 1, "only the leaf item is wrapped: {out}");
    assert!(
        out.contains("checkbox\" disabled> bar</label>"),
        "the leaf task item is wrapped: {out}"
    );
    // The outer checkbox is emitted bare (not inside a label).
    assert!(
        out.contains("<li><input type=\"checkbox\" checked disabled> "),
        "outer item's checkbox stays unwrapped: {out}"
    );
}

#[test]
fn a11y_streaming_matches_one_shot() {
    // The streaming ListCache / TableCache emit the a11y markup too; pin that
    // the cached path is byte-identical to the one-shot render.
    for md in [
        "- [ ] foo\n- [x] bar\n- [ ] baz\n",
        "| h1 | h2 |\n|---|---|\n| a | b |\n| c | d |\n",
        "- [x] foo\n  - [ ] bar\n  - [x] baz\n- [ ] bim\n",
        "- [ ] task with **bold** and `code`\n",
        "| left | mid | right |\n|:--|:-:|--:|\n| 1 | 2 | 3 |\n",
    ] {
        assert_eq!(render_a11y_streamed(md), render_a11y(md), "stream≠oneshot: {md:?}");
    }
}

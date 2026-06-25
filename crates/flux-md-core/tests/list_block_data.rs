//! Opt-in structured list channel (`with_block_data`). When on, a `List`'s `kind`
//! becomes `BlockKind::List { ordered, start: Some(<start number>) }`, carrying
//! the ordered-list start number (the `start="N"` HTML attribute) so a consumer
//! can renumber / continue a split list from DATA — no re-parse of the `<ol
//! start=…>` attribute. Off by default — a `List` then serializes as
//! `{"type":"List","data":{"ordered":<bool>}}` (the opt-in `start` key omitted),
//! byte-identical to before. The always-on `ordered` is unaffected.

use flux_md_core::blocks::BlockKind;
use flux_md_core::StreamParser;

fn finalize(md: &str, block_data: bool) -> StreamParser {
    let mut p = StreamParser::new().with_block_data(block_data);
    p.append(md);
    p.finalize();
    p
}

fn first_list(p: &StreamParser) -> Option<(bool, Option<u32>)> {
    for b in p.all_blocks() {
        if let BlockKind::List { ordered, start, .. } = &b.kind {
            return Some((*ordered, *start));
        }
    }
    None
}

/// The per-item inner `<li>` HTML carried on the first List's `kind.data.items`
/// (the keyed-renderer channel).
fn first_list_items(p: &StreamParser) -> Option<Vec<String>> {
    for b in p.all_blocks() {
        if let BlockKind::List { items, .. } = &b.kind {
            return Some(items.iter().map(|it| it.html.clone()).collect());
        }
    }
    None
}

#[test]
fn off_path_is_byte_identical_no_start_key() {
    // Default (block_data off): a List serializes with only the always-on
    // `ordered` — the opt-in `start` key is omitted — byte-identical to before.
    let p = finalize("3. third\n4. fourth\n", false);
    let mut saw = false;
    for b in p.all_blocks() {
        if let BlockKind::List { start, .. } = &b.kind {
            saw = true;
            assert!(start.is_none(), "off path must never populate start");
            assert_eq!(
                serde_json::to_string(&b.kind).unwrap(),
                r#"{"type":"List","data":{"ordered":true}}"#,
                "off-path List must omit the start key"
            );
        }
    }
    assert!(saw, "expected a List");
}

#[test]
fn on_path_carries_start_and_keeps_ordered() {
    // block_data on: start = the ordered-list start number, ordered unchanged.
    let p = finalize("5. five\n6. six\n", true);
    let (ordered, start) = first_list(&p).expect("expected a List");
    assert!(ordered);
    assert_eq!(start, Some(5));

    let json = p
        .all_blocks()
        .filter(|b| matches!(b.kind, BlockKind::List { start: Some(_), .. }))
        .map(|b| serde_json::to_string(&b.kind).unwrap())
        .next()
        .unwrap();
    // `items` (the keyed-renderer channel) rides after `start` when on.
    assert_eq!(
        json,
        r#"{"type":"List","data":{"ordered":true,"start":5,"items":[{"html":"five"},{"html":"six"}]}}"#
    );
}

#[test]
fn unordered_list_start_is_one() {
    // An unordered list has no meaningful start; the field is `Some(1)` when on
    // (so a consumer reads a number, not undefined), `ordered: false`.
    let p = finalize("- a\n- b\n", true);
    let (ordered, start) = first_list(&p).expect("expected a List");
    assert!(!ordered);
    assert_eq!(start, Some(1));
    let json = p
        .all_blocks()
        .filter(|b| matches!(b.kind, BlockKind::List { start: Some(_), .. }))
        .map(|b| serde_json::to_string(&b.kind).unwrap())
        .next()
        .unwrap();
    assert_eq!(
        json,
        r#"{"type":"List","data":{"ordered":false,"start":1,"items":[{"html":"a"},{"html":"b"}]}}"#
    );
}

#[test]
fn start_matches_ol_start_attribute() {
    // The structured `start` must agree with the `start="N"` HTML attribute (or
    // its absence ⇒ 1) — so a consumer never needs to re-parse the attribute.
    let cases = [
        ("1. a\n2. b\n", 1u32),
        ("7. a\n8. b\n", 7),
        ("0. a\n1. b\n", 0),
        ("42. a\n", 42),
    ];
    for (md, want) in cases {
        let p = finalize(md, true);
        let (_, start) = first_list(&p).expect("list");
        assert_eq!(start, Some(want), "start for {md:?}");
        // Cross-check against the emitted HTML attribute.
        let html: String = p.all_blocks().map(|b| b.html.clone()).collect();
        if want == 1 {
            assert!(!html.contains("start="), "start=1 must omit the attribute: {html}");
        } else {
            assert!(
                html.contains(&format!("start=\"{want}\"")),
                "html must carry start=\"{want}\": {html}"
            );
        }
    }
}

#[test]
fn on_path_does_not_change_html() {
    let md = "3. a\n4. b\n\n- x\n- y\n\n1. one\n";
    let off: String = finalize(md, false).all_blocks().map(|b| b.html.clone()).collect();
    let on: String = finalize(md, true).all_blocks().map(|b| b.html.clone()).collect();
    assert_eq!(off, on, "block_data must not change rendered HTML");
}

#[test]
fn off_path_has_no_items() {
    // block_data off ⇒ no per-item HTML at all (empty Vec, omitted on the wire).
    let items = first_list_items(&finalize("- a\n- b\n", false)).expect("a List");
    assert!(items.is_empty(), "off path must not populate items: {items:?}");
}

#[test]
fn items_carry_inner_li_html_tight() {
    // A tight list's item HTML is the inline-rendered content (no <p> wrap),
    // byte-identical to the inner of each <li> in block.html.
    let items = first_list_items(&finalize("- a **b**\n- c `d`\n", true)).expect("a List");
    assert_eq!(items, vec!["a <strong>b</strong>".to_string(), "c <code>d</code>".to_string()]);
}

#[test]
fn items_carry_inner_li_html_loose() {
    // A loose list (blank line between siblings) wraps each item's paragraph in
    // <p>…</p> — the item HTML mirrors that.
    let items = first_list_items(&finalize("- a\n\n- b\n", true)).expect("a List");
    assert_eq!(items, vec!["\n<p>a</p>".to_string(), "\n<p>b</p>".to_string()]);
}

#[test]
fn items_carry_task_checkbox() {
    // GFM task-list items keep their disabled-checkbox prefix in the item HTML.
    let items = first_list_items(&finalize("- [ ] todo\n- [x] done\n", true)).expect("a List");
    assert_eq!(
        items,
        vec![
            "<input type=\"checkbox\" disabled> todo".to_string(),
            "<input type=\"checkbox\" checked disabled> done".to_string(),
        ]
    );
}

#[test]
fn items_reconstruct_the_list_html() {
    // Concatenating each item as <li>…</li> (joined with `\n`, between the
    // opener and closer) must reproduce block.html — proving the inner spans are
    // exactly the bytes between each <li…> and </li>.
    for md in ["1. one\n2. two\n3. three\n", "- x\n- y\n", "- a\n\n- b\n\n- c\n"] {
        let p = finalize(md, true);
        let list = p
            .all_blocks()
            .find(|b| matches!(b.kind, BlockKind::List { .. }))
            .expect("a List");
        let items = match &list.kind {
            BlockKind::List { items, .. } => items,
            _ => unreachable!(),
        };
        // `render_list` emits `<li>…</li>\n` per item (trailing `\n` on every
        // item, including the last, before the close tag).
        let inner: String =
            items.iter().map(|it| format!("<li>{}</li>\n", it.html)).collect();
        // block.html = <ol/ul …>\n {items joined by \n} </ol/ul>. Strip opener
        // (up to and including the first `\n`) and the trailing close tag.
        let html = &list.html;
        let opener_end = html.find('\n').unwrap() + 1;
        let close = if matches!(list.kind, BlockKind::List { ordered: true, .. }) {
            "</ol>"
        } else {
            "</ul>"
        };
        let body = &html[opener_end..html.len() - close.len()];
        assert_eq!(body, inner, "items must reconstruct the list body for {md:?}");
    }
}

#[test]
fn streamed_items_converge_to_one_shot() {
    // The active block's per-item HTML (streamed via the ListCache, including the
    // tight→loose rebuild) must converge to the one-shot parse at every chunk
    // size — same invariant as `start`, now for the keyed-renderer channel.
    let cases = [
        "1. **one**\n2. two\n3. three\n4. four\n",
        "- a\n- b\n- c\n- d\n",
        "- a\n\n- b\n\n- c\n", // loose: forces a tight→loose rebuild mid-stream
        "- [ ] x\n- [x] y\n- [ ] z\n",
    ];
    for md in cases {
        let one = first_list_items(&finalize(md, true)).expect("a List");

        for n in 1..=9 {
            let mut p = StreamParser::new().with_block_data(true);
            let bytes = md.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let mut e = (i + n).min(bytes.len());
                while e < bytes.len() && (bytes[e] & 0xC0) == 0x80 {
                    e += 1;
                }
                p.append(std::str::from_utf8(&bytes[i..e]).unwrap());
                i = e;
            }
            p.finalize();
            assert_eq!(first_list_items(&p), Some(one.clone()), "chunk={n} != one-shot for {md:?}");
        }
    }
}

#[test]
fn streamed_list_converges_to_one_shot() {
    // A long open list streams through the ListCache fast-path; its active
    // `kind.data.start` must converge to the one-shot parse at every chunk size.
    let cases = [
        "5. five\n6. six\n7. seven\n8. eight\n",
        "- a\n- b\n- c\n- d\n",
        "10. ten\n11. eleven\n12. twelve\n",
    ];
    for md in cases {
        let one = first_list(&finalize(md, true));
        assert!(one.is_some(), "expected a List for {md:?}");

        let mut p = StreamParser::new().with_block_data(true);
        let mut buf = [0u8; 4];
        for ch in md.chars() {
            p.append(ch.encode_utf8(&mut buf));
        }
        p.finalize();
        assert_eq!(first_list(&p), one, "char-stream != one-shot for {md:?}");

        for n in 1..=7 {
            let mut p = StreamParser::new().with_block_data(true);
            let bytes = md.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let mut e = (i + n).min(bytes.len());
                while e < bytes.len() && (bytes[e] & 0xC0) == 0x80 {
                    e += 1;
                }
                p.append(std::str::from_utf8(&bytes[i..e]).unwrap());
                i = e;
            }
            p.finalize();
            assert_eq!(first_list(&p), one, "chunk={n} != one-shot for {md:?}");
        }
    }
}

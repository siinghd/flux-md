//! Opt-in structured container channel (`with_block_data`). When on, a Blockquote
//! block's `kind` becomes `BlockKind::Blockquote(Some(ContainerData { nested }))`
//! and an Alert's becomes `BlockKind::Alert { kind, nested: Some(ContainerData{…}) }`,
//! where `nested` is the ordered list of each inner sub-block's pre-rendered HTML
//! (`<p>…</p>` per inner paragraph). A keyed override can then render the children
//! one node at a time so only the open last inner block re-renders while the
//! container streams. Off by default — a Blockquote then serializes as
//! `{"type":"Blockquote"}` (no `data`) and an Alert as `{"kind":…}`, byte-identical.
//!
//! The structured data must be produced consistently on BOTH the full
//! `render_blockquote`/`render_alert` path AND the incremental `ContainerCache`
//! fast path. The mid-stream parity test asserts on the active block (where the
//! cache is live) — at finalize the caches are dropped.

use flux_md_core::blocks::{BlockKind, ContainerData};
use flux_md_core::StreamParser;

/// The `ContainerData` of the first blockquote/alert among all blocks, if any.
fn container_data(p: &StreamParser) -> Option<ContainerData> {
    for b in p.all_blocks() {
        match &b.kind {
            BlockKind::Blockquote(Some(cd)) => return Some(cd.clone()),
            BlockKind::Alert { nested: Some(cd), .. } => return Some(cd.clone()),
            _ => {}
        }
    }
    None
}

#[test]
fn blockquote_emits_keyed_nested_when_on() {
    let md = "> first **para**\n>\n> second [y](z)\n";
    let mut p = StreamParser::new().with_block_data(true);
    p.append(md);
    p.finalize();

    let cd = container_data(&p).expect("blockquote carries structured kind.data when on");
    assert_eq!(cd.nested.len(), 2, "two inner paragraphs → two keyed entries");
    assert_eq!(cd.nested[0].html, "<p>first <strong>para</strong></p>");
    assert_eq!(
        cd.nested[1].html,
        "<p>second <a href=\"z\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">y</a></p>"
    );

    // Every nested fragment is byte-present in the block's wrapper html.
    let block_html = p
        .all_blocks()
        .find(|b| matches!(b.kind, BlockKind::Blockquote(_)))
        .map(|b| b.html.clone())
        .unwrap();
    for n in &cd.nested {
        assert!(block_html.contains(&n.html), "nested {:?} not in {:?}", n.html, block_html);
    }
}

#[test]
fn alert_nested_excludes_the_title_line() {
    let md = "> [!NOTE]\n> body text here\n";
    let mut p = StreamParser::new().with_block_data(true).with_gfm_alerts(true);
    p.append(md);
    p.finalize();

    // It must classify as an Alert, and its nested must be ONLY the body (the
    // `[!NOTE]` title line is the wrapper, not an inner block).
    let alert = p
        .all_blocks()
        .find(|b| matches!(b.kind, BlockKind::Alert { .. }))
        .expect("classifies as Alert");
    if let BlockKind::Alert { nested: Some(cd), .. } = &alert.kind {
        assert_eq!(cd.nested.len(), 1, "one body paragraph");
        assert_eq!(cd.nested[0].html, "<p>body text here</p>");
        assert!(
            !cd.nested.iter().any(|n| n.html.contains("markdown-alert-title")),
            "the title <p> is the wrapper, never a nested entry"
        );
    } else {
        panic!("Alert must carry nested when block_data is on: {:?}", alert.kind);
    }
}

/// Serialize each block's `kind` to JSON for shape assertions.
fn kinds_json(p: &StreamParser) -> Vec<String> {
    p.all_blocks()
        .map(|b| serde_json::to_string(&b.kind).unwrap())
        .collect()
}

#[test]
fn default_off_is_byte_identical_and_has_no_data_key() {
    let md = "> a quote\n>\n> two paras\n\n> [!TIP]\n> a tip body\n";

    // Off (default): a Blockquote serializes as {"type":"Blockquote"} (no data),
    // an Alert as {"kind":…} (no nested) — byte-identical to before.
    let mut off = StreamParser::new().with_gfm_alerts(true);
    off.append(md);
    off.finalize();
    let off_html: String = off.all_blocks().map(|b| b.html.clone()).collect();
    let mut saw_bq = false;
    let mut saw_alert = false;
    for b in off.all_blocks() {
        if matches!(b.kind, BlockKind::Blockquote(None)) {
            saw_bq = true;
            assert_eq!(serde_json::to_string(&b.kind).unwrap(), r#"{"type":"Blockquote"}"#);
        }
        if let BlockKind::Alert { nested, .. } = &b.kind {
            saw_alert = true;
            assert!(nested.is_none(), "off path must never populate alert nested");
            assert_eq!(serde_json::to_string(&b.kind).unwrap(), r#"{"type":"Alert","data":{"kind":"tip"}}"#);
        }
        assert!(!matches!(b.kind, BlockKind::Blockquote(Some(_))), "off must not populate blockquote");
    }
    assert!(saw_bq, "expected a Blockquote");
    assert!(saw_alert, "expected an Alert");

    // On: same HTML (block_data must not change byte-output), but a `nested` key.
    let mut on = StreamParser::new().with_block_data(true).with_gfm_alerts(true);
    on.append(md);
    on.finalize();
    let on_html: String = on.all_blocks().map(|b| b.html.clone()).collect();
    assert_eq!(off_html, on_html, "block_data must not change rendered HTML");

    let on_kinds = kinds_json(&on);
    assert!(
        on_kinds.iter().any(|k| k.starts_with(r#"{"type":"Blockquote","data":{"nested":"#)),
        "on path emits the blockquote nested data key: {on_kinds:?}"
    );
    assert!(
        on_kinds.iter().any(|k| k.contains(r#""kind":"tip","nested":"#)),
        "on path emits the alert nested data key: {on_kinds:?}"
    );
}

/// Nested data of the first container, serialized.
fn data_json(p: &StreamParser) -> String {
    match container_data(p) {
        Some(cd) => serde_json::to_string(&cd).unwrap(),
        None => String::new(),
    }
}

fn streamed_final(md: &str) -> String {
    let mut p = StreamParser::new().with_block_data(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    data_json(&p)
}

fn chunked_final(md: &str, n: usize) -> String {
    let mut p = StreamParser::new().with_block_data(true);
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
    data_json(&p)
}

fn one_shot_final(md: &str) -> String {
    let mut p = StreamParser::new().with_block_data(true);
    p.append(md);
    p.finalize();
    data_json(&p)
}

#[test]
fn streaming_data_matches_one_shot() {
    // The container's structured data converges to the same value whether parsed
    // char-by-char (the live ContainerCache fast path) or in one shot (the full
    // render path). Cases carry inline markup so the html differs per paragraph.
    let cases = [
        "> first para\n>\n> second para\n",
        "> only one para across\n> two soft-wrapped lines\n",
        "> [!WARNING]\n> alert body para\n>\n> second alert para\n",
        "> a **bold** and `code` para\n", // no trailing-blank close
        "Intro.\n\n> quoted **stuff**\n>\n> more\n\nAfter.\n",
    ];
    for md in cases {
        let one = one_shot_final(md);
        assert!(!one.is_empty(), "expected container data for {md:?}");
        assert_eq!(streamed_final(md), one, "char-stream != one-shot for {md:?}");
        for n in 1..=9 {
            assert_eq!(chunked_final(md, n), one, "chunk={n} != one-shot for {md:?}");
        }
    }
}

/// Mid-stream (no finalize, so the live `ContainerCache` is what produces this):
/// each committed inner paragraph becomes a stable nested entry, and the open
/// last paragraph is appended speculatively — mirroring the table cache's
/// committed-rows-then-partial behaviour.
#[test]
fn cache_folds_committed_paras_then_open_para() {
    let mut p = StreamParser::new().with_block_data(true);
    // Header line + an empty append so the freshly-armed cache fires.
    p.append("> first para\n");
    p.append("");
    let cd = container_data(&p).expect("blockquote with one open para");
    assert_eq!(cd.nested.len(), 1, "the single open paragraph");
    assert_eq!(cd.nested[0].html, "<p>first para</p>");

    // A blank `>` line closes the first paragraph; start a second.
    p.append(">\n> second para\n");
    p.append("");
    let cd = container_data(&p).unwrap();
    assert_eq!(cd.nested.len(), 2, "first committed + second open");
    assert_eq!(cd.nested[0].html, "<p>first para</p>");
    assert_eq!(cd.nested[1].html, "<p>second para</p>");

    // A trailing partial (no newline) soft-wraps the still-OPEN second
    // paragraph (no blank `>` closed it), so it stays at two entries and the
    // open entry grows — the speculative tail tracks it. Mid-stream, the cache
    // renders the soft break verbatim as `\n` (it normalizes to a space only at
    // the full-reparse fixed point — see streaming_data_matches_one_shot).
    p.append("> third");
    let cd = container_data(&p).unwrap();
    assert_eq!(cd.nested.len(), 2, "first committed + second still open (now soft-wrapped)");
    assert_eq!(cd.nested[0].html, "<p>first para</p>");
    assert_eq!(cd.nested[1].html, "<p>second para\nthird</p>");
}

/// Within a single streamed parse, the container's `kind.data.nested` fragments
/// must each be byte-present in that same block's `html` at every append — the
/// real consistency invariant covering the live cache fast path.
#[test]
fn nested_fragments_are_in_block_html_at_every_append() {
    let md = "> a **first** para\n>\n> a `second` one\n>\n> third [x](y)\n";
    let mut p = StreamParser::new().with_block_data(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
        p.append("");
        for b in p.all_blocks() {
            let cd = match &b.kind {
                BlockKind::Blockquote(Some(cd)) => Some(cd),
                BlockKind::Alert { nested: Some(cd), .. } => Some(cd),
                BlockKind::Blockquote(None) => panic!("block_data on must never emit bare Blockquote"),
                _ => None,
            };
            if let Some(cd) = cd {
                for n in &cd.nested {
                    assert!(
                        b.html.contains(&n.html),
                        "nested fragment {:?} not in block html {:?}",
                        n.html, b.html
                    );
                }
            }
        }
    }
}

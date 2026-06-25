//! Golden-snapshot regression net for the hand-written `impl Serialize for
//! BlockKind` (the generic opt-in `kind.data` carrier).
//!
//! `BlockKind` serialization is no longer derived — it is hand-written so a
//! single variant can emit either `{"type":"X"}` (no `data` key) or
//! `{"type":"X","data":…}` depending on an `Option` (the carrier mechanism that
//! keeps `Table` byte-identical when `block_data` is off). The cost of dropping
//! the derive is that the hand-written impl now owns EVERY kind's wire shape.
//!
//! These goldens are the exact strings the prior `#[serde(tag="type",
//! content="data")]` derive produced (captured from the un-refactored code), and
//! they are asserted here so a hand-typo cannot silently drift the wire for any
//! kind. The `kind` value crosses the WASM boundary via
//! `serde_wasm_bindgen::to_value`; because the impl uses `serialize_struct` (not
//! `serialize_map`), each value serializes to a plain JS object there too — the
//! same plain-object shape the derive produced — so these `serde_json` goldens
//! also pin the wire contract `props.table = block.kind.data` depends on.

use flux_md_core::blocks::{
    AlertKind, BlockKind, ContainerData, HeadingData, ListItemData, MathBlockData, NestedBlock,
    TableCell, TableData,
};
use std::rc::Rc;

fn j(k: &BlockKind) -> String {
    serde_json::to_string(k).unwrap()
}

#[test]
fn every_variant_matches_pre_refactor_golden() {
    let td = TableData {
        headers: vec![TableCell {
            text: "H".into(),
            html: "<strong>H</strong>".into(),
        }],
        rows: vec![Rc::new(vec![TableCell {
            text: "x".into(),
            html: "x".into(),
        }])],
        aligns: vec![Some("center"), None],
    };

    // Unit kinds — no `data` key.
    assert_eq!(j(&BlockKind::Paragraph), r#"{"type":"Paragraph"}"#);
    assert_eq!(j(&BlockKind::Mermaid), r#"{"type":"Mermaid"}"#);
    assert_eq!(j(&BlockKind::Blockquote(None)), r#"{"type":"Blockquote"}"#);
    assert_eq!(j(&BlockKind::Rule), r#"{"type":"Rule"}"#);
    assert_eq!(j(&BlockKind::Html), r#"{"type":"Html"}"#);

    // The MathBlock carrier: off (None) ⇒ unit `{"type":"MathBlock"}` with no
    // `data` key, byte-identical to the pre-carrier unit variant; on (Some) ⇒
    // `{"type":"MathBlock","data":{"latex":…}}`.
    assert_eq!(j(&BlockKind::MathBlock(None)), r#"{"type":"MathBlock"}"#);
    assert_eq!(
        j(&BlockKind::MathBlock(Some(MathBlockData {
            latex: "E = mc^2".into()
        }))),
        r#"{"type":"MathBlock","data":{"latex":"E = mc^2"}}"#
    );

    // The Heading carrier: off (rich: None) ⇒ naked-scalar level (byte-identical
    // to the pre-carrier `Heading(u8)` wire); on (rich: Some) ⇒ the {level,text,
    // id} object.
    assert_eq!(
        j(&BlockKind::Heading { level: 2, rich: None }),
        r#"{"type":"Heading","data":2}"#
    );
    assert_eq!(
        j(&BlockKind::Heading {
            level: 2,
            rich: Some(HeadingData {
                level: 2,
                text: "Hello world".into(),
                id: "hello-world".into(),
            }),
        }),
        r#"{"type":"Heading","data":{"level":2,"text":"Hello world","id":"hello-world"}}"#
    );

    // Object payloads (derive-checked helper structs). The opt-in `code`/`start`
    // field is OMITTED when `None` (off) via `skip_serializing_if`, so the off
    // wire is byte-identical to before; present (on) it carries the source/number.
    assert_eq!(
        j(&BlockKind::CodeBlock { lang: None, code: None }),
        r#"{"type":"CodeBlock","data":{"lang":null}}"#
    );
    assert_eq!(
        j(&BlockKind::CodeBlock {
            lang: Some("rust".into()),
            code: None,
        }),
        r#"{"type":"CodeBlock","data":{"lang":"rust"}}"#
    );
    // ON: the decoded source rides alongside the always-on `lang`.
    assert_eq!(
        j(&BlockKind::CodeBlock {
            lang: Some("rust".into()),
            code: Some("fn main() {}\n".into()),
        }),
        r#"{"type":"CodeBlock","data":{"lang":"rust","code":"fn main() {}\n"}}"#
    );
    assert_eq!(
        j(&BlockKind::CodeBlock {
            lang: None,
            code: Some("plain\n".into()),
        }),
        r#"{"type":"CodeBlock","data":{"lang":null,"code":"plain\n"}}"#
    );
    assert_eq!(
        j(&BlockKind::List { ordered: true, start: None, items: vec![] }),
        r#"{"type":"List","data":{"ordered":true}}"#
    );
    assert_eq!(
        j(&BlockKind::List { ordered: false, start: None, items: vec![] }),
        r#"{"type":"List","data":{"ordered":false}}"#
    );
    // ON: the ordered-list start number rides alongside the always-on `ordered`.
    // (An empty `items` is still omitted — `skip_serializing_if`.)
    assert_eq!(
        j(&BlockKind::List { ordered: true, start: Some(5), items: vec![] }),
        r#"{"type":"List","data":{"ordered":true,"start":5}}"#
    );
    assert_eq!(
        j(&BlockKind::List { ordered: false, start: Some(1), items: vec![] }),
        r#"{"type":"List","data":{"ordered":false,"start":1}}"#
    );
    // ON with per-item HTML: `items` rides after `start` (the keyed-renderer
    // channel). Each entry is `{ "html": <inner-<li> HTML> }`.
    assert_eq!(
        j(&BlockKind::List {
            ordered: true,
            start: Some(1),
            items: vec![
                ListItemData { html: "first".into() },
                ListItemData { html: "<strong>second</strong>".into() },
            ],
        }),
        r#"{"type":"List","data":{"ordered":true,"start":1,"items":[{"html":"first"},{"html":"<strong>second</strong>"}]}}"#
    );
    assert_eq!(
        j(&BlockKind::Alert {
            kind: AlertKind::Note,
            nested: None,
        }),
        r#"{"type":"Alert","data":{"kind":"note"}}"#
    );
    assert_eq!(
        j(&BlockKind::Component {
            tag: "Thinking".into(),
            attrs: vec![("a".into(), "b".into())],
        }),
        r#"{"type":"Component","data":{"tag":"Thinking","attrs":[["a","b"]]}}"#
    );

    // The carrier: off (None) drops the `data` key; on (Some) carries it.
    assert_eq!(j(&BlockKind::Table(None)), r#"{"type":"Table"}"#);
    assert_eq!(
        j(&BlockKind::Table(Some(td))),
        r#"{"type":"Table","data":{"headers":[{"text":"H","html":"<strong>H</strong>"}],"rows":[[{"text":"x","html":"x"}]],"aligns":["center",null]}}"#
    );

    // The Blockquote carrier: off (None) drops the `data` key (byte-identical to
    // the pre-carrier unit variant); on (Some) carries the keyed `nested` blocks.
    assert_eq!(j(&BlockKind::Blockquote(None)), r#"{"type":"Blockquote"}"#);
    assert_eq!(
        j(&BlockKind::Blockquote(Some(ContainerData {
            nested: vec![
                NestedBlock { html: "<p>a</p>".into() },
                NestedBlock { html: "<p>b</p>".into() },
            ],
        }))),
        r#"{"type":"Blockquote","data":{"nested":[{"html":"<p>a</p>"},{"html":"<p>b</p>"}]}}"#
    );

    // The Alert carrier: `kind` is always-on; the opt-in `nested` rides behind
    // `skip_serializing_if` so the off wire (`{"kind":…}`) is byte-identical.
    assert_eq!(
        j(&BlockKind::Alert {
            kind: AlertKind::Tip,
            nested: Some(ContainerData {
                nested: vec![NestedBlock { html: "<p>x</p>".into() }],
            }),
        }),
        r#"{"type":"Alert","data":{"kind":"tip","nested":[{"html":"<p>x</p>"}]}}"#
    );
}

/// Every `AlertKind` keyword still serializes to its lowercase string inside the
/// `Alert` payload (guards the `AlertData` helper + `rename_all = "lowercase"`).
#[test]
fn alert_kinds_serialize_lowercase() {
    for (k, want) in [
        (AlertKind::Note, "note"),
        (AlertKind::Tip, "tip"),
        (AlertKind::Important, "important"),
        (AlertKind::Warning, "warning"),
        (AlertKind::Caution, "caution"),
    ] {
        assert_eq!(
            j(&BlockKind::Alert { kind: k, nested: None }),
            format!(r#"{{"type":"Alert","data":{{"kind":"{want}"}}}}"#)
        );
    }
}

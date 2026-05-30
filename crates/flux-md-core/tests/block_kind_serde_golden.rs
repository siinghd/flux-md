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

use flux_md_core::blocks::{AlertKind, BlockKind, HeadingData, TableCell, TableData};
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
    assert_eq!(j(&BlockKind::MathBlock), r#"{"type":"MathBlock"}"#);
    assert_eq!(j(&BlockKind::Mermaid), r#"{"type":"Mermaid"}"#);
    assert_eq!(j(&BlockKind::Blockquote), r#"{"type":"Blockquote"}"#);
    assert_eq!(j(&BlockKind::Rule), r#"{"type":"Rule"}"#);
    assert_eq!(j(&BlockKind::Html), r#"{"type":"Html"}"#);

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

    // Object payloads (derive-checked helper structs).
    assert_eq!(
        j(&BlockKind::CodeBlock { lang: None }),
        r#"{"type":"CodeBlock","data":{"lang":null}}"#
    );
    assert_eq!(
        j(&BlockKind::CodeBlock {
            lang: Some("rust".into())
        }),
        r#"{"type":"CodeBlock","data":{"lang":"rust"}}"#
    );
    assert_eq!(
        j(&BlockKind::List { ordered: true }),
        r#"{"type":"List","data":{"ordered":true}}"#
    );
    assert_eq!(
        j(&BlockKind::List { ordered: false }),
        r#"{"type":"List","data":{"ordered":false}}"#
    );
    assert_eq!(
        j(&BlockKind::Alert {
            kind: AlertKind::Note
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
            j(&BlockKind::Alert { kind: k }),
            format!(r#"{{"type":"Alert","data":{{"kind":"{want}"}}}}"#)
        );
    }
}

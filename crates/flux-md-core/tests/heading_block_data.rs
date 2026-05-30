//! Opt-in structured heading channel (`with_block_data`). When on, a Heading
//! block's `kind` becomes `BlockKind::Heading { level, rich: Some(HeadingData {
//! level, text, id }) }`, carrying the inline-stripped plaintext and a GitHub-
//! style anchor slug so a consumer can build a table of contents with working
//! `#anchor` links from DATA — no re-parse of the rendered HTML. Off by default —
//! a Heading then serializes as `{"type":"Heading","data":<level>}` (a naked
//! int), byte-identical to before the carrier.
//!
//! The enrichment rides the same generic carrier as `Table(Option<TableData>)`
//! (one `Option`-bearing field, no paired bare/with-data variant) and is folded
//! at the same top-level promotion site (parser.rs). Headings have no streaming
//! cache (they commit single-line for ATX, or via the Paragraph→Heading setext
//! transition), so the enrichment drops on at exactly one place.

use flux_md_core::blocks::{BlockKind, HeadingData};
use flux_md_core::StreamParser;

/// The `HeadingData` of the first enriched heading among all blocks, if any.
fn first_heading_rich(p: &StreamParser) -> Option<HeadingData> {
    for b in p.all_blocks() {
        if let BlockKind::Heading { rich: Some(h), .. } = &b.kind {
            return Some(h.clone());
        }
    }
    None
}

/// Parse `md` to its committed/finalized blocks, one-shot.
fn finalize(md: &str, block_data: bool) -> StreamParser {
    let mut p = StreamParser::new().with_block_data(block_data);
    p.append(md);
    p.finalize();
    p
}

#[test]
fn off_path_is_byte_identical_naked_int() {
    // Default (block_data off): a Heading serializes as the naked level int with
    // no rich object — byte-identical to the pre-carrier `Heading(u8)` wire.
    let p = finalize("## Hello world\n", false);
    let mut saw = false;
    for b in p.all_blocks() {
        if matches!(b.kind, BlockKind::Heading { .. }) {
            saw = true;
            assert!(
                matches!(b.kind, BlockKind::Heading { rich: None, .. }),
                "off path must never populate rich"
            );
            assert_eq!(
                serde_json::to_string(&b.kind).unwrap(),
                r#"{"type":"Heading","data":2}"#,
                "off-path Heading must serialize as a naked level int"
            );
        }
    }
    assert!(saw, "expected a Heading block");
}

#[test]
fn on_path_emits_level_text_id() {
    // block_data on: rich = { level, text(plaintext), id(slug) }. Inline markup is
    // stripped from `text`; the slug is lowercase with non-alphanumerics → `-`.
    let p = finalize("## **Bold** & plain\n", true);
    let h = first_heading_rich(&p).expect("expected an enriched heading");
    assert_eq!(h.level, 2);
    assert_eq!(h.text, "Bold & plain", "inline markup must be stripped to plaintext");
    assert_eq!(h.id, "bold-plain", "id is a github-style slug of the plaintext");

    // The wire shape carries the object under `data`.
    let json = p
        .all_blocks()
        .filter(|b| matches!(b.kind, BlockKind::Heading { rich: Some(_), .. }))
        .map(|b| serde_json::to_string(&b.kind).unwrap())
        .next()
        .unwrap();
    assert_eq!(
        json,
        r#"{"type":"Heading","data":{"level":2,"text":"Bold & plain","id":"bold-plain"}}"#
    );
}

#[test]
fn on_path_does_not_change_html() {
    // The opt-in channel must never perturb the rendered HTML (data is additive).
    let md = "# Title\n\n## Sub *section* one\n\nbody\n";
    let off: String = finalize(md, false).all_blocks().map(|b| b.html.clone()).collect();
    let on: String = finalize(md, true).all_blocks().map(|b| b.html.clone()).collect();
    assert_eq!(off, on, "block_data must not change rendered HTML");
}

#[test]
fn all_levels_and_setext() {
    // ATX levels 1..6 plus the two setext forms (=/-), each enriched with the
    // right level and slug.
    let md = "# One\n\n## Two\n\n### Three\n\n#### Four\n\n##### Five\n\n###### Six\n\nSetext H1\n=========\n\nSetext H2\n---------\n";
    let p = finalize(md, true);
    let got: Vec<(u8, String, String)> = p
        .all_blocks()
        .filter_map(|b| match &b.kind {
            BlockKind::Heading { rich: Some(h), .. } => {
                Some((h.level, h.text.clone(), h.id.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (1, "One".into(), "one".into()),
            (2, "Two".into(), "two".into()),
            (3, "Three".into(), "three".into()),
            (4, "Four".into(), "four".into()),
            (5, "Five".into(), "five".into()),
            (6, "Six".into(), "six".into()),
            (1, "Setext H1".into(), "setext-h1".into()),
            (2, "Setext H2".into(), "setext-h2".into()),
        ]
    );
}

#[test]
fn slug_drops_punctuation_and_collapses_separators() {
    // The slug is the anchor target a ToC links to: lowercase, punctuation
    // dropped, runs of non-alphanumerics collapsed to a single `-`, no leading/
    // trailing `-`.
    let cases = [
        ("Hello, World!", "hello-world"),
        ("  Spaced   Out  ", "spaced-out"),
        ("C++ & Rust", "c-rust"),
        ("Foo-Bar", "foo-bar"),
        ("123 numbers ok", "123-numbers-ok"),
        ("!!!", ""), // all punctuation ⇒ empty slug (documented edge)
    ];
    for (md_text, want) in cases {
        let p = finalize(&format!("# {md_text}\n"), true);
        let h = first_heading_rich(&p).expect("heading");
        assert_eq!(h.id, want, "slug of {md_text:?}");
    }
}

#[test]
fn duplicate_texts_yield_identical_slugs_v1() {
    // v1 limitation, asserted so it is a documented contract (not an accident):
    // two headings with the same text get the SAME slug (no dedup counter yet).
    let p = finalize("# Intro\n\n## Intro\n", true);
    let slugs: Vec<String> = p
        .all_blocks()
        .filter_map(|b| match &b.kind {
            BlockKind::Heading { rich: Some(h), .. } => Some(h.id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(slugs, vec!["intro".to_string(), "intro".to_string()]);
}

#[test]
fn streamed_setext_converges_to_one_shot() {
    // The interesting streaming case: `text\n===` transitions Paragraph→Heading
    // mid-stream. A char-by-char (and chunked) stream must converge to the same
    // enriched HeadingData as a one-shot parse, and whenever a committed block IS
    // a Heading its rich payload must be well-formed (level in 1..=6, slug == the
    // slug of its own text).
    let cases = [
        "Setext Title\n============\n\nbody\n",
        "Sub heading\n-----------\n\nmore\n",
        "# ATX First\n\nSetext After\n====\n\ntail\n",
    ];
    for md in cases {
        let one = headings_of(&finalize(md, true));
        assert!(!one.is_empty(), "expected headings for {md:?}");

        // char-by-char
        let mut p = StreamParser::new().with_block_data(true);
        let mut buf = [0u8; 4];
        for ch in md.chars() {
            p.append(ch.encode_utf8(&mut buf));
        }
        p.finalize();
        assert_eq!(headings_of(&p), one, "char-stream != one-shot for {md:?}");

        // every chunk size 1..=7
        for n in 1..=7 {
            let mut p = StreamParser::new().with_block_data(true);
            let bytes = md.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                // chunk on a char boundary
                let mut end = (i + n).min(bytes.len());
                while end < bytes.len() && (bytes[end] & 0xC0) == 0x80 {
                    end += 1;
                }
                p.append(std::str::from_utf8(&bytes[i..end]).unwrap());
                i = end;
            }
            p.finalize();
            assert_eq!(headings_of(&p), one, "chunk={n} != one-shot for {md:?}");
        }
    }
}

/// The enriched headings of a parser's committed/finalized blocks, in order, as
/// (level, text, id) tuples. Also asserts each is well-formed: the slug is the
/// slug of its own text (streaming-consistency anchor), level in 1..=6.
fn headings_of(p: &StreamParser) -> Vec<(u8, String, String)> {
    p.all_blocks()
        .filter_map(|b| match &b.kind {
            BlockKind::Heading { level, rich: Some(h) } => {
                assert_eq!(*level, h.level, "outer level must match rich.level");
                assert!((1..=6).contains(&h.level), "level out of range: {}", h.level);
                Some((h.level, h.text.clone(), h.id.clone()))
            }
            // A heading with no rich payload while block_data is on would be a bug
            // (enrichment must drop on at the promotion site).
            BlockKind::Heading { rich: None, .. } => {
                panic!("block_data on but heading carries no rich payload")
            }
            _ => None,
        })
        .collect()
}

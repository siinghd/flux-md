//! Opt-in structured table channel (`with_block_data`). When on, a Table block's
//! `kind` becomes `BlockKind::TableWithData(TableData { headers, rows, aligns })`
//! with per-cell `{ text, html }` so a consumer can sort/filter/transpose/CSV/
//! chart from DATA without re-parsing the HTML. Off by default — a Table then
//! serializes as `{"type":"Table"}` (no `data` key), byte-identical to before.
//!
//! The structured data must be produced consistently on BOTH the full
//! `render_table` path AND the incremental `TableCache` fast path, and must stay
//! correct as rows stream (committed + the speculative trailing row). The
//! streaming-parity test asserts on the MID-STREAM active block (where the cache
//! is live) — at finalize the caches are dropped, so a finalized-only comparison
//! would never exercise the fast path.

use flux_md_core::blocks::{BlockKind, TableData};
use flux_md_core::StreamParser;

/// The `TableData` of the first table block among all blocks, if any.
fn table_data(p: &StreamParser) -> Option<TableData> {
    for b in p.all_blocks() {
        if let BlockKind::TableWithData(td) = &b.kind {
            return Some(td.clone());
        }
    }
    None
}

#[test]
fn table_emits_structured_data_when_on() {
    let md = "| **A** | B |\n|---|:-:|\n| x | [y](z) |\n";
    let mut p = StreamParser::new().with_block_data(true);
    p.append(md);
    p.finalize();

    let td = table_data(&p).expect("table carries structured kind.data when on");

    // Headers: inline markup is stripped for `text`, rendered for `html`.
    assert_eq!(td.headers.len(), 2);
    assert_eq!(td.headers[0].text, "A", "bold stripped in header text");
    assert_eq!(td.headers[0].html, "<strong>A</strong>", "header html keeps markup");
    assert_eq!(td.headers[1].text, "B");
    assert_eq!(td.headers[1].html, "B");

    // Alignments come straight off the delimiter row.
    assert_eq!(td.aligns, vec![None, Some("center")]);

    // Body row: a link renders to html, strips to its link text.
    assert_eq!(td.rows.len(), 1);
    assert_eq!(td.rows[0].len(), 2);
    assert_eq!(td.rows[0][0].text, "x");
    assert_eq!(td.rows[0][0].html, "x");
    assert_eq!(td.rows[0][1].text, "y", "link text survives the strip");
    assert_eq!(
        td.rows[0][1].html,
        "<a href=\"z\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">y</a>",
        "cell html is byte-identical to the inline html inside the <td>"
    );
}

#[test]
fn cell_text_strips_inline_and_decodes_entities() {
    // A cell containing a literal `<`, an entity-producing char, code, and emphasis.
    let md = "| H |\n|---|\n| a < b & `c` *d* |\n";
    let mut p = StreamParser::new().with_block_data(true);
    p.append(md);
    p.finalize();
    let td = table_data(&p).unwrap();
    // html keeps the escaping + tags; text is the decoded, tag-free plaintext.
    assert_eq!(td.rows[0][0].html, "a &lt; b &amp; <code>c</code> <em>d</em>");
    assert_eq!(td.rows[0][0].text, "a < b & c d");
}

/// Serialize each block's `kind` to a JSON string, for shape assertions and for
/// cross-path structured-data comparison.
fn kinds_json(p: &StreamParser) -> Vec<String> {
    p.all_blocks()
        .map(|b| serde_json::to_string(&b.kind).unwrap())
        .collect()
}

#[test]
fn default_off_is_byte_identical_and_has_no_data_key() {
    let md = "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |\n";

    // Off (default): html identical to a plain parse, and the Table kind
    // serializes WITHOUT a `data` key — guards the two-variant serde collapse.
    let mut off = StreamParser::new();
    off.append(md);
    off.finalize();
    let off_html: String = off.all_blocks().map(|b| b.html.clone()).collect();

    // The Table kind must be the bare variant and serialize as {"type":"Table"}.
    let mut saw_table = false;
    for b in off.all_blocks() {
        if matches!(b.kind, BlockKind::Table) {
            saw_table = true;
            assert_eq!(
                serde_json::to_string(&b.kind).unwrap(),
                r#"{"type":"Table"}"#,
                "off-path Table must serialize with no data key"
            );
        }
        assert!(
            !matches!(b.kind, BlockKind::TableWithData(_)),
            "off path must never produce TableWithData"
        );
    }
    assert!(saw_table, "expected a Table block");

    // On: same HTML (block_data must not change byte-output), but a data key.
    let mut on = StreamParser::new().with_block_data(true);
    on.append(md);
    on.finalize();
    let on_html: String = on.all_blocks().map(|b| b.html.clone()).collect();
    assert_eq!(off_html, on_html, "block_data must not change rendered HTML");

    let on_kinds = kinds_json(&on);
    assert!(
        on_kinds.iter().any(|k| k.starts_with(r#"{"type":"Table","data":"#)),
        "on path emits a data key: {on_kinds:?}"
    );
}

/// Serialized structured data of the first table block, or "" when there is no
/// table yet.
fn data_json(p: &StreamParser) -> String {
    match table_data(p) {
        Some(td) => serde_json::to_string(&td).unwrap(),
        None => String::new(),
    }
}

/// Parse `md` one char at a time, finalize, return the table's structured data.
fn streamed_final(md: &str) -> String {
    let mut p = StreamParser::new().with_block_data(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    data_json(&p)
}

/// Parse `md` in `n`-byte chunks (UTF-8-aligned), finalize, return table data.
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

/// Parse `md` in one append, finalize, return table data.
fn one_shot_final(md: &str) -> String {
    let mut p = StreamParser::new().with_block_data(true);
    p.append(md);
    p.finalize();
    data_json(&p)
}

#[test]
fn streaming_data_matches_one_shot() {
    // The task's "streaming consistency" requirement: a streamed (incremental)
    // table must converge to the same structured data as a one-shot parse, for
    // char-by-char and every chunk size 1..=9. Convergence is asserted at the
    // commit/finalize fixed point (the contract the parser provides — see
    // table_streaming.rs, which compares html the same way). Cases carry inline
    // markup so text/html differ per cell.
    let cases = [
        "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |\n",
        "| Name | Age |\n| --- | --- |\n| Alice | 30 |\n| Bob | 25 |", // no trailing newline
        "| **A** | B |\n| :- | -: |\n| `x` | [y](z) |\n| 1 | 2 |\n",
        "| one |\n| --- |\n| x |\n| y |\n", // single column
        "Intro.\n\n| H1 | H2 |\n| --- | --- |\n| a | b |\n\nAfter.\n", // table amid prose
    ];
    for md in cases {
        let one = one_shot_final(md);
        assert!(!one.is_empty(), "expected a table for {md:?}");
        assert_eq!(streamed_final(md), one, "char-stream != one-shot for {md:?}");
        for n in 1..=9 {
            assert_eq!(chunked_final(md, n), one, "chunk={n} != one-shot for {md:?}");
        }
    }
}

/// Within a SINGLE streamed parse, the table block's `kind.data` must mirror
/// that same block's own `html` at every append: a `TableWithData` kind iff the
/// html is a `<table>`, and one structured row per `<tr>` in the body with each
/// cell's `html` appearing inside the corresponding `<td>`/`<th>`. This is the
/// real consistency invariant (data derived from the same `push_table_cell`
/// calls as the html) and it covers the live `TableCache` fast path — at
/// finalize the caches are dropped, so this mid-stream check is what exercises
/// them.
#[test]
fn data_mirrors_own_html_at_every_append() {
    let md = "| **A** | B |\n| :- | -: |\n| `x` | [y](z) |\n| 1 | 2 |\n";
    let mut p = StreamParser::new().with_block_data(true);
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
        // One empty append so a freshly-armed cache fires this step (matches the
        // midstream_parity.rs convention).
        p.append("");
        for b in p.all_blocks() {
            let is_table_html = b.html.starts_with("<table");
            match &b.kind {
                BlockKind::TableWithData(td) => {
                    assert!(is_table_html, "TableWithData but html is not a table: {}", b.html);
                    // Header cell html appears inside the rendered thead.
                    for cell in &td.headers {
                        assert!(
                            cell.html.is_empty() || b.html.contains(&cell.html),
                            "header cell html {:?} not found in block html {:?}",
                            cell.html, b.html
                        );
                    }
                    // One structured row per body `<tr>`; each cell's html is present.
                    let tr_count = b.html.matches("<tr>").count();
                    // `<tr>` count includes the header row (in <thead>), so body
                    // rows = tr_count - 1 (the thead row).
                    assert_eq!(
                        td.rows.len(),
                        tr_count.saturating_sub(1),
                        "row count != body <tr> count; html={:?}",
                        b.html
                    );
                    for row in &td.rows {
                        for cell in row.iter() {
                            assert!(
                                cell.html.is_empty() || b.html.contains(&cell.html),
                                "row cell html {:?} not found in block html {:?}",
                                cell.html, b.html
                            );
                        }
                    }
                }
                BlockKind::Table => {
                    panic!("block_data on must never emit bare Table; html={}", b.html);
                }
                _ => {
                    assert!(!is_table_html, "table html but non-table kind: {}", b.html);
                }
            }
        }
    }
}

/// The committed-row fold path (`cache.body_cells.push(row)`) and the
/// speculative partial-row path must both populate `kind.data.rows`, in order,
/// mid-stream (no finalize — so the live `TableCache` is what produces this).
#[test]
fn cache_folds_committed_rows_then_appends_partial() {
    let mut p = StreamParser::new().with_block_data(true);
    p.append("| A | B |\n| --- | --- |\n");
    assert_eq!(table_data(&p).unwrap().rows.len(), 0, "no body rows yet");
    // First full (newline-terminated) row → folded into cache.body_cells.
    p.append("| x1 | y1 |\n");
    let td = table_data(&p).unwrap();
    assert_eq!(td.rows.len(), 1, "one committed row folded");
    assert_eq!(td.rows[0][0].text, "x1");
    // Second full row → second fold.
    p.append("| x2 | y2 |\n");
    let td = table_data(&p).unwrap();
    assert_eq!(td.rows.len(), 2, "two committed rows folded, in order");
    assert_eq!(td.rows[1][0].text, "x2");
    // Trailing partial (no newline) → speculative row appended after the two.
    p.append("| x3 | y3");
    let td = table_data(&p).unwrap();
    assert_eq!(td.rows.len(), 3, "committed rows + speculative partial");
    assert_eq!(td.rows[2][0].text, "x3");
    assert_eq!(td.rows[2][1].text, "y3");
}

#[test]
fn streaming_speculative_row_is_included_midstream() {
    // After the delimiter + a partial (newline-less) body row arrives, the
    // active block's structured data must already include that speculative row,
    // mirroring the HTML the consumer renders (emit-on-every-patch, not
    // commit-only). This is the cache fast path producing the partial row.
    let mut p = StreamParser::new().with_block_data(true);
    p.append("| A | B |\n| --- | --- |\n");
    assert!(table_data(&p).is_some(), "header forms a table");
    p.append("| x | y |"); // partial row, no trailing newline
    let td = table_data(&p).expect("still a table");
    assert_eq!(td.rows.len(), 1, "speculative partial row is in kind.data.rows");
    assert_eq!(td.rows[0][0].text, "x");
    assert_eq!(td.rows[0][1].text, "y");
}

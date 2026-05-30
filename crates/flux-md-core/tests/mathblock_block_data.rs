//! Opt-in structured math channel (`with_block_data`). When on, a `MathBlock`'s
//! `kind` becomes `BlockKind::MathBlock(Some(MathBlockData { latex }))`, carrying
//! the DECODED LaTeX source so a `components.MathBlock` override can re-render with
//! KaTeX from DATA — no re-parse (and entity-decode) of the display HTML. Off by
//! default — a `MathBlock` then serializes as the unit `{"type":"MathBlock"}` (no
//! `data` key), byte-identical to before.
//!
//! `latex` is byte-identical to the client's `decodeMathText(block.html)`: the
//! `<div class="math math-display">…</div>` body (or `<pre><code>…</code></pre>`
//! for a fenced `math` block), entity-decoded.

use flux_md_core::blocks::BlockKind;
use flux_md_core::StreamParser;

/// Parse with math on (the `$$`/`\[` block forms require `gfm_math`).
fn finalize(md: &str, block_data: bool) -> StreamParser {
    let mut p = StreamParser::new().with_gfm_math(true).with_block_data(block_data);
    p.append(md);
    p.finalize();
    p
}

fn first_latex(p: &StreamParser) -> Option<String> {
    for b in p.all_blocks() {
        if let BlockKind::MathBlock(Some(md)) = &b.kind {
            return Some(md.latex.clone());
        }
    }
    None
}

#[test]
fn off_path_is_byte_identical_unit() {
    // Default (block_data off): a MathBlock serializes as the unit variant — no
    // `data` key — byte-identical to the pre-carrier unit shape.
    let p = finalize("$$\nE = mc^2\n$$\n", false);
    let mut saw = false;
    for b in p.all_blocks() {
        if let BlockKind::MathBlock(opt) = &b.kind {
            saw = true;
            assert!(opt.is_none(), "off path must never populate the math payload");
            assert_eq!(
                serde_json::to_string(&b.kind).unwrap(),
                r#"{"type":"MathBlock"}"#,
                "off-path MathBlock must serialize as the unit variant"
            );
        }
    }
    assert!(saw, "expected a MathBlock");
}

#[test]
fn on_path_carries_decoded_latex() {
    // block_data on: latex = the decoded display-math body. Special chars decode
    // back to source.
    let p = finalize("$$\na < b \\& c\n$$\n", true);
    let latex = first_latex(&p).expect("expected a MathBlock");
    assert_eq!(latex, "a < b \\& c");

    let json = p
        .all_blocks()
        .filter(|b| matches!(b.kind, BlockKind::MathBlock(Some(_))))
        .map(|b| serde_json::to_string(&b.kind).unwrap())
        .next()
        .unwrap();
    assert_eq!(json, r#"{"type":"MathBlock","data":{"latex":"a < b \\& c"}}"#);
}

#[test]
fn fenced_math_block_also_enriched() {
    // A fenced ```math block classifies to MathBlock and is enriched too — the
    // source rides the MathBlock carrier (latex), not a CodeBlock.
    let p = finalize("```math\n\\frac{1}{2}\n```\n", true);
    let latex = first_latex(&p).expect("expected a fenced MathBlock");
    // A fenced code body carries the trailing `\n` the `<pre><code>` body holds.
    assert_eq!(latex, "\\frac{1}{2}\n");
}

#[test]
fn latex_equals_decoded_html_body() {
    // Drop-in contract: latex == the entity-decoded display-math body of block.html.
    let cases = ["$$\nx^2 + y^2 = z^2\n$$\n", "\\[\na \\le b\n\\]\n"];
    for md in cases {
        let p = finalize(md, true);
        for b in p.all_blocks() {
            if let BlockKind::MathBlock(Some(data)) = &b.kind {
                let decoded = decode_math_html(&b.html);
                assert_eq!(data.latex, decoded, "latex must equal decoded body for {md:?}");
            }
        }
    }
}

#[test]
fn on_path_does_not_change_html() {
    let md = "before\n\n$$\nx = 1\n$$\n\nafter\n";
    let off: String = finalize(md, false).all_blocks().map(|b| b.html.clone()).collect();
    let on: String = finalize(md, true).all_blocks().map(|b| b.html.clone()).collect();
    assert_eq!(off, on, "block_data must not change rendered HTML");
}

#[test]
fn streamed_math_converges_to_one_shot() {
    // A long open `$$` block streams through the FenceCache fast-path; its active
    // `kind.data.latex` must converge to the one-shot parse at every chunk size.
    let cases = [
        "$$\n\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}\n$$\n",
        "\\[\na < b \\& c > d\n\\]\n",
    ];
    for md in cases {
        let one = first_latex(&finalize(md, true));
        assert!(one.is_some(), "expected a MathBlock for {md:?}");

        let mut p = StreamParser::new().with_gfm_math(true).with_block_data(true);
        let mut buf = [0u8; 4];
        for ch in md.chars() {
            p.append(ch.encode_utf8(&mut buf));
        }
        p.finalize();
        assert_eq!(first_latex(&p), one, "char-stream != one-shot for {md:?}");

        for n in 1..=7 {
            let mut p = StreamParser::new().with_gfm_math(true).with_block_data(true);
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
            assert_eq!(first_latex(&p), one, "chunk={n} != one-shot for {md:?}");
        }
    }
}

#[test]
fn mermaid_fence_is_not_enriched_stays_unit() {
    // A ```mermaid fence classifies to the unit `Mermaid` kind, which is
    // intentionally NOT enriched (it carries no opt-in `kind.data`). With
    // block_data ON it must still serialize as the bare `{"type":"Mermaid"}` — and
    // must NOT be mis-routed onto a CodeBlock/MathBlock carrier.
    for on in [false, true] {
        let p = finalize("```mermaid\ngraph TD; A-->B;\n```\n", on);
        let mut saw = false;
        for b in p.all_blocks() {
            if let BlockKind::Mermaid = &b.kind {
                saw = true;
                assert_eq!(
                    serde_json::to_string(&b.kind).unwrap(),
                    r#"{"type":"Mermaid"}"#,
                    "Mermaid stays a bare unit (block_data={on})"
                );
            }
            assert!(
                !matches!(b.kind, BlockKind::CodeBlock { code: Some(_), .. } | BlockKind::MathBlock(Some(_))),
                "mermaid source must not leak onto a code/math carrier (block_data={on})"
            );
        }
        assert!(saw, "expected a Mermaid block (block_data={on})");
    }
}

/// Reference client derivation: entity-decode the display-math (`<div
/// class="math math-display">…</div>`) body, single left-to-right scan.
fn decode_math_html(html: &str) -> String {
    let open = "<div class=\"math math-display\">";
    let start = html.find(open).unwrap() + open.len();
    let end = html[start..].find("</div>").unwrap() + start;
    let body = &html[start..end];
    let mut out = String::with_capacity(body.len());
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if body[i..].starts_with("&lt;") {
            out.push('<');
            i += 4;
        } else if body[i..].starts_with("&gt;") {
            out.push('>');
            i += 4;
        } else if body[i..].starts_with("&quot;") {
            out.push('"');
            i += 6;
        } else if body[i..].starts_with("&amp;") {
            out.push('&');
            i += 5;
        } else {
            let ch = body[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

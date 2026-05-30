//! Opt-in structured code channel (`with_block_data`). When on, a `CodeBlock`'s
//! `kind` becomes `BlockKind::CodeBlock { lang, code: Some(<decoded source>) }`,
//! carrying the DECODED text inside `<pre><code>…</code></pre>` so a consumer can
//! build a copy-to-clipboard string / re-highlight from DATA — no re-parse (and
//! entity-decode) of the rendered HTML. Off by default — a `CodeBlock` then
//! serializes as `{"type":"CodeBlock","data":{"lang":<…>}}` (the opt-in `code`
//! key omitted), byte-identical to before. The always-on `lang` is unaffected.
//!
//! `code` is byte-identical to the client's `decodeCodeText(block.html)`: the
//! `<pre><code>` body, entity-decoded, with the same trailing-`\n` normalization
//! the renderer's HTML body carries.

use flux_md_core::blocks::BlockKind;
use flux_md_core::StreamParser;

fn finalize(md: &str, block_data: bool) -> StreamParser {
    let mut p = StreamParser::new().with_block_data(block_data);
    p.append(md);
    p.finalize();
    p
}

/// The (lang, code) of the first CodeBlock among a parser's blocks.
fn first_code(p: &StreamParser) -> Option<(Option<String>, Option<String>)> {
    for b in p.all_blocks() {
        if let BlockKind::CodeBlock { lang, code } = &b.kind {
            return Some((lang.clone(), code.clone()));
        }
    }
    None
}

#[test]
fn off_path_is_byte_identical_no_code_key() {
    // Default (block_data off): a CodeBlock serializes with only the always-on
    // `lang` — the opt-in `code` key is omitted — byte-identical to before.
    let p = finalize("```rust\nfn main() {}\n```\n", false);
    let mut saw = false;
    for b in p.all_blocks() {
        if let BlockKind::CodeBlock { code, .. } = &b.kind {
            saw = true;
            assert!(code.is_none(), "off path must never populate code");
            assert_eq!(
                serde_json::to_string(&b.kind).unwrap(),
                r#"{"type":"CodeBlock","data":{"lang":"rust"}}"#,
                "off-path CodeBlock must omit the code key"
            );
        }
    }
    assert!(saw, "expected a CodeBlock");
}

#[test]
fn on_path_carries_decoded_source_and_keeps_lang() {
    // block_data on: code = the decoded `<pre><code>` body (trailing `\n`), lang
    // unchanged. Special chars are entity-decoded back to source.
    let p = finalize("```rust\nlet x = a < b && c > d;\n```\n", true);
    let (lang, code) = first_code(&p).expect("expected a CodeBlock");
    assert_eq!(lang.as_deref(), Some("rust"));
    assert_eq!(code.as_deref(), Some("let x = a < b && c > d;\n"));

    let json = p
        .all_blocks()
        .filter(|b| matches!(b.kind, BlockKind::CodeBlock { code: Some(_), .. }))
        .map(|b| serde_json::to_string(&b.kind).unwrap())
        .next()
        .unwrap();
    assert_eq!(
        json,
        r#"{"type":"CodeBlock","data":{"lang":"rust","code":"let x = a < b && c > d;\n"}}"#
    );
}

#[test]
fn code_equals_decoded_html_body() {
    // The contract a JS consumer relies on: `code` is byte-identical to the
    // entity-decoded `<pre><code>…</code></pre>` body of `block.html` — proving the
    // structured channel is a drop-in for `decodeCodeText`, with no HTML re-parse.
    // (These cases contain no literal `&lt;`-style sequences, where a left-to-right
    // entity scan is correct but the client's chained regex is lossy; the
    // structured channel returns the correct lossless source either way.)
    let cases = [
        "```\nplain code\n```\n",
        "```js\nconst a = \"<tag>\" & b;\n```\n",
        "    indented one\n    indented two\n", // indented code
        "```py\nmulti\nline\n```\n",
        "```\n```\n", // empty fenced block
    ];
    for md in cases {
        let p = finalize(md, true);
        for b in p.all_blocks() {
            if let BlockKind::CodeBlock { code: Some(code), .. } = &b.kind {
                let decoded = decode_code_html(&b.html);
                assert_eq!(
                    *code, decoded,
                    "code must equal decoded <pre><code> body for {md:?}"
                );
            }
        }
    }
}

#[test]
fn on_path_does_not_change_html() {
    let md = "```rust\nfn main() {}\n```\n\nprose\n\n    four space code\n";
    let off: String = finalize(md, false).all_blocks().map(|b| b.html.clone()).collect();
    let on: String = finalize(md, true).all_blocks().map(|b| b.html.clone()).collect();
    assert_eq!(off, on, "block_data must not change rendered HTML");
}

#[test]
fn streamed_fence_converges_to_one_shot() {
    // A long open code fence streams through the FenceCache fast-path; its active
    // `kind.data.code` must converge to the one-shot parse at every chunk size.
    let cases = [
        "```rust\nfn a() {}\nfn b() {}\nlet x = 1 < 2;\n```\n",
        "```\nno lang\nx & y\n```\n",
        "    indent a\n    indent b\n    indent c\n",
    ];
    for md in cases {
        let one = first_code(&finalize(md, true));
        assert!(one.is_some(), "expected a CodeBlock for {md:?}");

        // char-by-char
        let mut p = StreamParser::new().with_block_data(true);
        let mut buf = [0u8; 4];
        for ch in md.chars() {
            p.append(ch.encode_utf8(&mut buf));
        }
        p.finalize();
        assert_eq!(first_code(&p), one, "char-stream != one-shot for {md:?}");

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
            assert_eq!(first_code(&p), one, "chunk={n} != one-shot for {md:?}");
        }
    }
}

/// The reference client derivation: entity-decode the `<pre><code>…</code></pre>`
/// body of a block's HTML. `code` (the structured channel) must equal this so it
/// is a drop-in for `decodeCodeText`. A single left-to-right scan (decode each
/// entity once, in place) — the order-independent, lossless form.
fn decode_code_html(html: &str) -> String {
    let start = html.find("<code").unwrap();
    let body_start = html[start..].find('>').unwrap() + start + 1;
    let body_end = html.find("</code></pre>").unwrap();
    let body = &html[body_start..body_end];
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

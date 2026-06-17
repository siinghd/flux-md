//! Safe raw-HTML handling: HTML comments are dropped (no longer escaped to
//! visible junk), and the opt-in sanitizer renders a safe subset of inline raw
//! HTML (allowlist / allow-all-minus-dangerous / drop-list) with attributes
//! sanitized — all without full `unsafe_html`.

use flux_md_core::StreamParser;

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn render(md: &str) -> String {
    let mut p = StreamParser::new();
    p.append(md);
    p.finalize();
    collect(&p)
}

fn render_unsafe(md: &str) -> String {
    let mut p = StreamParser::new().with_unsafe_html(true);
    p.append(md);
    p.finalize();
    collect(&p)
}

fn render_sanitize(md: &str, allow: &[&str], drop: &[&str]) -> String {
    let mut p = StreamParser::new().with_html_sanitize(
        true,
        allow.iter().map(|s| s.to_string()).collect(),
        drop.iter().map(|s| s.to_string()).collect(),
    );
    p.append(md);
    p.finalize();
    collect(&p)
}

// ----- comments -----

#[test]
fn inline_comment_dropped_when_safe() {
    let out = render("a <!--mk:marketcap--> b\n");
    assert!(!out.contains("&lt;!--"), "comment must not be escaped to text: {out}");
    assert!(!out.contains("marketcap"), "comment content gone: {out}");
    assert!(out.contains("a") && out.contains("b"), "surrounding text kept: {out}");
}

#[test]
fn block_comment_dropped_when_safe() {
    let out = render("<!--mk:marketcap-->\n");
    assert!(!out.contains("<pre>"), "comment must not become a code block: {out}");
    assert!(!out.contains("marketcap"), "comment dropped: {out}");
}

#[test]
fn comment_passes_through_in_bare_unsafe() {
    // Bare unsafe pass-through keeps comments verbatim (CommonMark fidelity).
    let out = render_unsafe("a <!--keep--> b\n");
    assert!(out.contains("<!--keep-->"), "bare unsafe keeps the comment: {out}");
}

#[test]
fn comment_dropped_in_sanitize_mode() {
    let out = render_sanitize("a <!--mk:x--> b\n", &["br"], &[]);
    assert!(!out.contains("mk:x") && !out.contains("&lt;!--"), "sanitizer drops comments: {out}");
}

// ----- allowlist (restrict) -----

#[test]
fn allowlist_renders_listed_inline_tags_escapes_others() {
    let out = render_sanitize("H<sub>2</sub>O, a<sup>2</sup>, line<br>break, <div>x</div>\n", &["sub", "sup", "br"], &[]);
    assert!(out.contains("<sub>2</sub>"), "sub renders: {out}");
    assert!(out.contains("<sup>2</sup>"), "sup renders: {out}");
    assert!(out.contains("<br>") || out.contains("<br/>") || out.contains("<br />"), "br renders: {out}");
    assert!(out.contains("&lt;div&gt;"), "non-allowed div is escaped, not rendered: {out}");
}

#[test]
fn restrict_is_case_insensitive() {
    let out = render_sanitize("x<BR>y\n", &["br"], &[]);
    assert!(out.contains("<BR>") || out.contains("<BR/>") || out.contains("<BR />"), "case-insensitive match renders: {out}");
}

// ----- allow-all (empty allowlist) -----

#[test]
fn allow_all_renders_safe_tags_drops_dangerous() {
    let out = render_sanitize("text <b>bold</b> and <script>alert(1)</script> and <em>em</em>\n", &[], &[]);
    assert!(out.contains("<b>bold</b>"), "safe tag renders in allow-all: {out}");
    assert!(out.contains("<em>em</em>"), "safe tag renders: {out}");
    assert!(!out.to_lowercase().contains("<script"), "dangerous tag dropped: {out}");
    assert!(out.contains("alert(1)"), "script body survives as inert text (not executed): {out}");
}

#[test]
fn allow_all_engaged_via_droplist_only() {
    // Setting only a drop-list still engages allow-all for everything else.
    let out = render_sanitize("a <mk>x</mk> <b>y</b> b\n", &[], &["mk"]);
    assert!(!out.to_lowercase().contains("<mk"), "drop-list tag removed: {out}");
    assert!(out.contains("x"), "dropped tag's text stays: {out}");
    assert!(out.contains("<b>y</b>"), "other tags still render (allow-all): {out}");
}

// ----- attribute sanitization on rendered tags -----

#[test]
fn rendered_tag_attributes_are_sanitized() {
    let out = render_sanitize("see <a href=\"javascript:alert(1)\" onclick=\"x()\" title=\"ok\">link</a>\n", &["a"], &[]);
    assert!(out.contains("<a "), "anchor renders: {out}");
    assert!(out.contains("title=\"ok\""), "safe attr kept: {out}");
    assert!(!out.to_lowercase().contains("onclick"), "event handler dropped: {out}");
    assert!(!out.contains("javascript:"), "dangerous href neutralized: {out}");
    assert!(out.contains("href=\"#\""), "dangerous href → #: {out}");
    assert!(out.contains(">link</a>"), "inner text + close kept: {out}");
}

// ----- feature off / safety -----

#[test]
fn feature_off_escapes_raw_tags_as_before() {
    // With the sanitizer off and unsafe off, raw tags are still escaped (only
    // comments changed). Byte-identical to prior behavior for tags.
    let out = render("a <br> b\n");
    assert!(out.contains("&lt;br&gt;"), "raw tag still escaped when feature off: {out}");
}

#[test]
fn sanitizer_overrides_unsafe_for_block_script() {
    // A block-level <script> with BOTH unsafe_html and the sanitizer on must NOT
    // pass through raw — the sanitizer wins and it is escaped.
    let mut p = StreamParser::new()
        .with_unsafe_html(true)
        .with_html_sanitize(true, vec![], vec![]);
    p.append("<script>alert(1)</script>\n");
    p.finalize();
    let out = collect(&p);
    assert!(!out.to_lowercase().contains("<script"), "block script must not render raw: {out}");
}

// ===== STREAMING DIFFERENTIAL PROBES (review) =====

fn render_streamed_sanitize(md: &str, allow: &[&str], drop: &[&str]) -> String {
    let mut p = StreamParser::new().with_html_sanitize(
        true,
        allow.iter().map(|s| s.to_string()).collect(),
        drop.iter().map(|s| s.to_string()).collect(),
    );
    for ch in md.chars() {
        let mut buf = [0u8; 4];
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

fn diff_case(md: &str, allow: &[&str], drop: &[&str]) {
    let one = render_sanitize(md, allow, drop);
    let stream = render_streamed_sanitize(md, allow, drop);
    assert_eq!(one, stream, "STREAM DIVERGENCE {md:?}");
}

fn real_tags(html: &str) -> Vec<String> {
    let b = html.as_bytes();
    let mut tags = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'<' && i + 1 < b.len() && (b[i+1].is_ascii_alphabetic() || b[i+1] == b'/') {
            if let Some(rel) = b[i..].iter().position(|&c| c == b'>') {
                tags.push(html[i..i+rel+1].to_ascii_lowercase());
                i += rel + 1;
                continue;
            }
        }
        i += 1;
    }
    tags
}

fn has_on_handler(tag: &str) -> bool {
    let bts = tag.as_bytes();
    let mut i = 0;
    while i + 2 < bts.len() {
        if bts[i] == b' ' && bts[i+1] == b'o' && bts[i+2] == b'n' {
            let mut j = i + 3;
            while j < bts.len() && (bts[j].is_ascii_alphanumeric() || bts[j] == b'-') { j += 1; }
            let mut k = j;
            while k < bts.len() && bts[k] == b' ' { k += 1; }
            if k < bts.len() && bts[k] == b'=' { return true; }
        }
        i += 1;
    }
    false
}

fn assert_no_exec_tags(html: &str, ctx: &str) {
    for t in real_tags(html) {
        assert!(!t.starts_with("<script"), "REAL <script> {t:?} {ctx}");
        assert!(!t.starts_with("<iframe"), "REAL <iframe> {t:?} {ctx}");
        assert!(!t.starts_with("<svg"), "REAL <svg> {t:?} {ctx}");
        assert!(!has_on_handler(&t), "on* handler {t:?} {ctx}");
        assert!(!t.contains("javascript:"), "javascript: {t:?} {ctx}");
    }
}

fn check_prefix(md: &str, allow: &[&str], drop: &[&str]) {
    let mut p = StreamParser::new().with_html_sanitize(
        true,
        allow.iter().map(|s| s.to_string()).collect(),
        drop.iter().map(|s| s.to_string()).collect(),
    );
    let mut sent = String::new();
    for ch in md.chars() {
        let mut buf = [0u8; 4];
        p.append(ch.encode_utf8(&mut buf));
        sent.push(ch);
        let html = collect(&p);
        assert_no_exec_tags(&html, &format!("prefix {sent:?} -> {html}"));
    }
}

#[test]
fn stream_diff_matches_oneshot() {
    let cases: &[&str] = &[
        "hello <b>world</b> ok\n",
        "x <script>alert(1)</script> y\n",
        "a <!--comment--> b\n",
        "a <img src=x onerror=alert(1)> b\n",
        "pre <span class=\"q\">mid</span> post and more words here to force cache\n",
        "intro words to arm cache here we go <!--unterminated marker keeps coming-->tail\n",
        "padding padding padding padding padding <svg onload=alert(1)>x</svg> trailing\n",
    ];
    for md in cases {
        diff_case(md, &[], &[]);
        diff_case(md, &["b", "i", "span", "a", "img"], &[]);
        diff_case(md, &[], &["script", "style"]);
    }
}

#[test]
fn stream_prefix_no_real_exec() {
    let cases: &[&str] = &[
        "words words words words words <script>alert(1)</script> tail\n",
        "words words words words words <img src=x onerror=alert(1)> tail\n",
        "words words words words words <svg onload=alert(1)></svg> tail\n",
        "words words words words words <iframe src=javascript:alert(1)></iframe> tail\n",
        "words words words words words <a href=javascript:alert(1)>x</a> tail\n",
        "words words words words words <a href=\"javascript:alert(1)\">x</a> tail\n",
        "words words words words words <b onmouseover=alert(1)>x</b> tail\n",
    ];
    for md in cases {
        check_prefix(md, &[], &[]);
        check_prefix(md, &["a", "b", "i", "img"], &[]);
    }
}

// ----- regressions for the adversarial-review findings -----

#[test]
fn style_attribute_is_dropped() {
    // `style` is a CSS-injection vector (beacon via url(), clickjack via
    // position:fixed) — drop it on every rendered tag, in allow-all and restrict.
    let out = render_sanitize("a <span style=\"background:url(https://evil/x)\">y</span> b\n", &[], &[]);
    assert!(!out.contains("style="), "style dropped in allow-all: {out}");
    assert!(out.contains("<span>y</span>"), "tag still renders without style: {out}");
    let out = render_sanitize("a <a href=\"#\" style=\"position:fixed;inset:0\">y</a> b\n", &["a"], &[]);
    assert!(!out.contains("style="), "style dropped in restrict: {out}");
}

#[test]
fn allowlisting_a_dangerous_tag_still_drops_it() {
    // The dangerous set is non-overridable: allowlisting `script`/`iframe`/`svg`
    // must NOT render them.
    for tag in ["script", "iframe", "svg"] {
        let out = render_sanitize("x <script>alert(1)</script> <iframe></iframe> <svg onload=x></svg> y\n", &[tag], &[]);
        assert!(!out.to_lowercase().contains("<script"), "script never renders (allow={tag}): {out}");
        assert!(!out.to_lowercase().contains("<iframe"), "iframe never renders (allow={tag}): {out}");
        assert!(!out.to_lowercase().contains("<svg"), "svg never renders (allow={tag}): {out}");
    }
}

#[test]
fn raw_text_elements_dropped_in_allow_all() {
    let out = render_sanitize("a <xmp>raw</xmp> <plaintext>more</plaintext> b\n", &[], &[]);
    assert!(!out.to_lowercase().contains("<xmp"), "xmp dropped: {out}");
    assert!(!out.to_lowercase().contains("<plaintext"), "plaintext dropped: {out}");
    assert!(out.contains("raw") && out.contains("more"), "their text stays inert: {out}");
}

#[test]
fn comment_block_with_trailing_content_does_not_lose_it() {
    // A comment-LED block that also has content after `-->` must not be dropped
    // wholesale — the trailing text survives (escaped), never lost.
    let out = render("<!-- x --> keep this disclaimer\n");
    assert!(out.contains("keep this disclaimer"), "trailing content preserved: {out}");
}

#[test]
fn stream_block_and_comment_forming() {
    let cases: &[&str] = &[
        "<!--marker-->\n",
        "<!-- multi\nline\ncomment -->\n",
        "<!--c--> visible text after\n",
        "<script>alert(1)</script>\n",
        "<div onclick=alert(1)>x</div>\n",
        "<iframe src=javascript:alert(1)></iframe>\n",
        "long lead text to arm the paragraph cache here ok <!--late comment--> done\n",
        "long lead text to arm the paragraph cache here ok <! not a comment > done\n",
        "aaaa bbbb cccc dddd eeee ffff gggg <!--x and then unterminated forever more\n",
    ];
    for md in cases {
        let one = render_sanitize(md, &[], &[]);
        let stream = render_streamed_sanitize(md, &[], &[]);
        assert_eq!(one, stream, "DIVERGENCE {md:?}");
        check_prefix(md, &[], &[]);
    }
}

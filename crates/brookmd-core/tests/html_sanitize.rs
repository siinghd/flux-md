//! Safe raw-HTML handling: HTML comments are dropped (no longer escaped to
//! visible junk), and the opt-in sanitizer renders a safe subset of inline raw
//! HTML (allowlist / allow-all-minus-dangerous / drop-list) with attributes
//! sanitized — all without full `unsafe_html`.

use brook_md_core::StreamParser;

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

// ----- raw-HTML attribute denylist (DOM-hazard attributes) -----
//
// The attribute layer used to be a pure denylist of `on*` / `style` / React-
// meaningful names, so anything unlisted rode through. These tests pin the
// DOM-hazard table: attributes that either execute, re-parent, hijack or clobber
// something. Several are inert today only because the tag that gives them
// meaning is in the dangerous set (`srcdoc` needs `iframe`) — they are dropped
// anyway so a future allowlist change cannot inherit an XSS.

/// Every raw-HTML-dropped attribute, in the form the sanitizer would see it.
const DOM_HAZARD_ATTRS: &[&str] = &[
    "srcdoc", "is", "autofocus", "contenteditable", "id", "name", "slot", "part",
    "exportparts", "form", "formaction", "formenctype", "formmethod",
    "formnovalidate", "formtarget", "xmlns", "xlink:href", "ping",
];

#[test]
fn dom_hazard_attrs_dropped_from_raw_html() {
    for attr in DOM_HAZARD_ATTRS {
        // Allowlisted tag: it still renders, just without the hazard attribute.
        let md = format!("a <span {attr}=\"v\" title=\"ok\">y</span> b\n");
        let out = render_sanitize(&md, &["span"], &[]);
        assert!(out.contains("<span "), "tag still renders ({attr}): {out}");
        assert!(out.contains("title=\"ok\""), "safe attr kept ({attr}): {out}");
        assert!(out.contains(">y</span>"), "inner text + close kept ({attr}): {out}");
        assert!(!out.contains(&format!("{attr}=")), "{attr} must be dropped: {out}");
        // …and identically in allow-all mode.
        let all = render_sanitize(&md, &[], &[]);
        assert!(!all.contains(&format!("{attr}=")), "{attr} dropped in allow-all: {all}");
    }
}

#[test]
fn dom_hazard_attr_matching_is_case_insensitive() {
    let md = "a <span SRCDOC='<script>' FormAction=/x oNcLiCk=evil() ID=q Name=n \
              CONTENTEDITABLE=true title=ok>y</span> b\n";
    let out = render_sanitize(md, &["span"], &[]);
    let low = out.to_lowercase();
    for attr in ["srcdoc", "formaction", "onclick", "id=", "name=", "contenteditable"] {
        assert!(!low.contains(attr), "{attr} dropped regardless of case: {out}");
    }
    assert!(out.contains("title=\"ok\""), "safe attr survives: {out}");
}

#[test]
fn dom_clobbering_vectors_are_defused() {
    // The classic DOM-clobbering pair: a named/id'd element shadows
    // `document.getElementById` / `window.location` for any script that looks
    // them up by name. Allow-all mode, so the tags themselves do render.
    let out = render_sanitize(
        "x <img name=\"getElementById\" src=\"/p.png\"> and <a id=\"location\" href=\"/q\">q</a> y\n",
        &[],
        &[],
    );
    assert!(!out.contains("name="), "name= gone: {out}");
    assert!(!out.contains("id="), "id= gone: {out}");
    assert!(out.contains("src=\"/p.png\""), "img still renders: {out}");
    assert!(out.contains("href=\"/q\""), "anchor still renders: {out}");
}

#[test]
fn namespace_prefixes_are_dropped_wholesale() {
    // Not just the two exact names: `xlink:show`/`xmlns:xlink` are the same
    // escape hatch, so the whole prefix goes.
    let out = render_sanitize(
        "a <span xmlns:xlink=\"http://www.w3.org/1999/xlink\" xlink:show=\"new\" \
         xlink:href=\"javascript:alert(1)\" title=ok>y</span> b\n",
        &["span"],
        &[],
    );
    assert!(!out.contains("xlink"), "xlink:* dropped: {out}");
    assert!(!out.contains("xmlns"), "xmlns:* dropped: {out}");
    assert!(out.contains("title=\"ok\""), "safe attr kept: {out}");
}

#[test]
fn target_on_raw_anchor_is_left_alone() {
    // DECISION (pinned, not a hardening): our own markdown links carry
    // `target="_blank" rel="noopener noreferrer nofollow"`, but a RAW `<a>` keeps
    // whatever the author wrote. Recorded here so a change is deliberate.
    let out = render_sanitize("a <a href=\"/p\" target=\"_blank\">y</a> b\n", &["a"], &[]);
    assert!(out.contains("target=\"_blank\""), "raw target passes through: {out}");
}

// ----- URL-bearing attributes beyond href/src -----

#[test]
fn secondary_url_attrs_are_scheme_checked() {
    // `cite` (blockquote/q/ins/del) is a URL carrier that is NOT dropped, so it
    // must go through the same scheme filter as href/src.
    let out = render_sanitize("a <q cite=\"javascript:alert(1)\">y</q> b\n", &["q"], &[]);
    assert!(!out.contains("javascript:"), "cite neutralized: {out}");
    assert!(out.contains("cite=\"#\""), "cite → #: {out}");
    let ok = render_sanitize("a <q cite=\"https://e.com/s\">y</q> b\n", &["q"], &[]);
    assert!(ok.contains("cite=\"https://e.com/s\""), "safe cite kept: {ok}");
}

#[test]
fn secondary_url_attrs_follow_allow_schemes() {
    fn sanitized(md: &str, allow: &[&str]) -> String {
        let mut p = StreamParser::new()
            .with_html_sanitize(true, Vec::new(), Vec::new())
            .with_allow_schemes(allow.iter().map(|s| s.to_string()).collect());
        p.append(md);
        p.finalize();
        collect(&p)
    }
    let md = "a <q cite=\"file:///etc/passwd\">y</q> b\n";
    assert!(!sanitized(md, &[]).contains("file:///"), "cite file: blocked by default");
    assert!(
        sanitized(md, &["file"]).contains("cite=\"file:///etc/passwd\""),
        "cite follows allowSchemes: {}",
        sanitized(md, &["file"])
    );
}

#[test]
fn formaction_and_ping_are_dropped_not_rewritten() {
    // Both are URL carriers, but neither has a benign use in untrusted content:
    // `formaction` re-points a submit, `ping` is a background beacon. Dropping is
    // strictly stronger than the `#` rewrite the scheme filter would apply.
    let out = render_sanitize(
        "a <a href=\"/p\" ping=\"https://evil/beacon\">y</a> \
         <span formaction=\"javascript:alert(1)\">z</span> b\n",
        &["a", "span"],
        &[],
    );
    assert!(!out.contains("ping"), "ping dropped entirely: {out}");
    assert!(!out.contains("formaction"), "formaction dropped entirely: {out}");
    assert!(!out.contains("javascript:"), "no live javascript: {out}");
    assert!(out.contains("href=\"/p\""), "the safe href still renders: {out}");
}

// ----- our own generated ids are unaffected -----

#[test]
fn our_generated_ids_survive_with_sanitizer_engaged() {
    // The hardening drops `id` from RAW HTML. Our own ids are emitted by the
    // renderers directly — `sanitize_attrs` only ever sees raw source tokens —
    // so footnote ids and heading slugs must be untouched with everything on at
    // once (footnotes + blockData + sanitizer + raw HTML carrying its own id).
    use brook_md_core::blocks::BlockKind;
    let md = "# My Heading\n\nsee note[^1] and <span id=\"evil\">raw</span>\n\n[^1]: the note\n";
    let mut p = StreamParser::new()
        .with_gfm_footnotes(true)
        .with_block_data(true)
        .with_html_sanitize(true, vec!["span".to_string()], vec![]);
    p.append(md);
    p.finalize();
    let out = collect(&p);

    assert!(out.contains("id=\"fnref-1\""), "footnote ref id intact: {out}");
    assert!(out.contains("id=\"fn-1\""), "footnote def id intact: {out}");
    assert!(out.contains("href=\"#fn-1\""), "footnote ref link intact: {out}");
    assert!(out.contains("href=\"#fnref-1\""), "footnote backref intact: {out}");
    assert!(!out.contains("id=\"evil\""), "raw-HTML id still dropped: {out}");
    assert!(out.contains("<span>raw</span>"), "raw span renders without id: {out}");

    // The heading slug rides `kind.data`, never an emitted `id=` attribute, so
    // the sanitizer cannot reach it at all.
    let slug = p.all_blocks().find_map(|b| match &b.kind {
        BlockKind::Heading { rich: Some(h), .. } => Some(h.id.clone()),
        _ => None,
    });
    assert_eq!(slug.as_deref(), Some("my-heading"), "heading slug intact: {out}");
}

// ----- component tags stay permissive (the documented asymmetry) -----

#[test]
fn component_tag_props_keep_dom_hazard_attrs() {
    // Component attributes become PROPS on `components[tag]` — the consumer's
    // component decides whether any of them reaches the DOM — so the raw-HTML
    // denylist deliberately does NOT apply. `on*` and URL schemes still do.
    let mut p = StreamParser::new()
        .with_inline_component_tags(vec!["Tab".to_string()]);
    p.append("x <Tab id=\"x\" name=\"n\" slot=\"s\" onClick=\"evil()\" href=\"javascript:alert(1)\">y</Tab> z\n");
    p.finalize();
    let out = collect(&p);
    assert!(out.contains("id=\"x\""), "component id survives as a prop: {out}");
    assert!(out.contains("name=\"n\""), "component name survives: {out}");
    assert!(out.contains("slot=\"s\""), "component slot survives: {out}");
    assert!(!out.to_lowercase().contains("onclick"), "on* still dropped: {out}");
    assert!(out.contains("href=\"#\""), "URL attrs still sanitized: {out}");
}

#[test]
fn block_component_props_keep_dom_hazard_attrs() {
    use brook_md_core::blocks::BlockKind;
    let mut p = StreamParser::new().with_component_tags(vec!["Thinking".to_string()]);
    p.append("<Thinking id=\"x\" ping=\"https://evil/b\" onclick=\"evil()\">\nbody\n</Thinking>\n");
    p.finalize();
    let out = collect(&p);
    assert!(out.contains("id=\"x\""), "block component id survives: {out}");
    assert!(!out.to_lowercase().contains("onclick"), "on* still dropped: {out}");
    // The prop bag on `kind` matches the wrapper (same sanitizer tier).
    let attrs: Vec<(String, String)> = p
        .all_blocks()
        .find_map(|b| match &b.kind {
            BlockKind::Component { attrs, .. } => Some(attrs.clone()),
            _ => None,
        })
        .expect("component block");
    assert!(attrs.iter().any(|(k, v)| k == "id" && v == "x"), "prop bag keeps id: {attrs:?}");
    assert!(!attrs.iter().any(|(k, _)| k.eq_ignore_ascii_case("onclick")), "{attrs:?}");
}

// ----- streaming parity for the new drops -----

/// Streamed open view == one-shot open view char-by-char and at every 2-chunk
/// split, finalize included (the `assert_sweep` shape from midstream_parity).
fn assert_sanitize_sweep(md: &str, allow: &[&str]) {
    let make = || {
        StreamParser::new().with_html_sanitize(
            true,
            allow.iter().map(|s| s.to_string()).collect(),
            Vec::new(),
        )
    };
    let one_open = {
        let mut p = make();
        p.append(md);
        collect(&p)
    };
    {
        let mut p = make();
        let mut buf = [0u8; 4];
        for ch in md.chars() {
            p.append(ch.encode_utf8(&mut buf));
        }
        p.append("");
        assert_eq!(collect(&p), one_open, "char-stream open != one-shot for {md:?}");
    }
    let one_final = {
        let mut q = make();
        q.append(md);
        q.finalize();
        collect(&q)
    };
    for cut in 1..md.len() {
        if !md.is_char_boundary(cut) {
            continue;
        }
        let mut p = make();
        p.append(&md[..cut]);
        p.append(&md[cut..]);
        assert_eq!(collect(&p), one_open, "2-chunk open split at {cut} != one-shot for {md:?}");
        p.finalize();
        assert_eq!(collect(&p), one_final, "2-chunk finalize split at {cut} != one-shot for {md:?}");
    }
}

#[test]
fn stream_parity_for_dom_hazard_attrs() {
    let cases: &[&str] = &[
        "a <span id=q name=n slot=s title=ok>y</span> b\n",
        "a <a href=\"/p\" ping=\"https://evil/b\" formaction=\"javascript:x\">y</a> b\n",
        "a <span SRCDOC='<x>' CONTENTEDITABLE=true is=evil-el autofocus>y</span> b\n",
        "lead words to arm the paragraph cache here <q cite=\"javascript:alert(1)\">y</q> tail\n",
    ];
    for md in cases {
        assert_sanitize_sweep(md, &[]);
        assert_sanitize_sweep(md, &["a", "span", "q"]);
    }
}

// ===== BLOCK-level raw HTML (`set_block_html`) =====
//
// Stage 1: CommonMark HTML block types 6 and 7 render through the SAME
// allow/drop/dangerous decision and hardened attribute policy as inline raw
// HTML. Types 1–5 stay escaped. The flag is inert unless the sanitizer is
// engaged, and off by default.

fn render_block_html(md: &str, allow: &[&str], drop: &[&str]) -> String {
    let mut p = StreamParser::new()
        .with_html_sanitize(
            true,
            allow.iter().map(|s| s.to_string()).collect(),
            drop.iter().map(|s| s.to_string()).collect(),
        )
        .with_block_html(true);
    p.append(md);
    p.finalize();
    collect(&p)
}

#[test]
fn block_html_needs_the_sanitizer_to_do_anything() {
    // `block_html` alone (no allow/drop list ⇒ sanitizer not engaged) is inert:
    // block raw HTML is still escaped, byte-identical to the default build.
    let mut p = StreamParser::new().with_block_html(true);
    p.append("<div>\nbody\n</div>\n");
    p.finalize();
    let out = collect(&p);
    assert!(out.contains("<pre><code>&lt;div&gt;"), "flag alone must not render raw HTML: {out}");
    // And with the sanitizer engaged but the flag off (today's behaviour).
    assert!(
        render_sanitize("<div>\nbody\n</div>\n", &[], &[]).contains("<pre><code>&lt;div&gt;"),
        "sanitizer without block_html keeps block HTML escaped",
    );
}

#[test]
fn block_details_summary_renders_under_allowlist() {
    // The disclosure-widget shape: a model emits <details>/<summary> raw HTML.
    let out = render_block_html(
        "<details>\n<summary>Sources</summary>\nthe body\n</details>\n",
        &["details", "summary"],
        &[],
    );
    assert!(out.contains("<details>"), "details renders: {out}");
    assert!(out.contains("<summary>Sources</summary>"), "summary renders: {out}");
    assert!(out.contains("the body"), "body text kept: {out}");
    assert!(out.contains("</details>"), "close tag kept: {out}");
    assert!(!out.contains("<pre><code>"), "not escaped into a code block: {out}");
}

#[test]
fn block_non_allowlisted_tag_inside_escapes() {
    let out = render_block_html(
        "<details>\n<summary>s</summary>\n<marquee>x</marquee>\n</details>\n",
        &["details", "summary"],
        &[],
    );
    assert!(out.contains("<details>") && out.contains("<summary>"), "allowed tags render: {out}");
    assert!(out.contains("&lt;marquee&gt;"), "non-allowlisted tag escapes: {out}");
    assert!(!out.contains("<marquee"), "…and never renders: {out}");
}

#[test]
fn block_allow_all_mode_works_at_block_level() {
    // `htmlAllowlist: []` = allow everything except the dangerous set.
    let out = render_block_html("<div class=\"card\">\n<b>hi</b>\n</div>\n", &[], &[]);
    assert!(out.contains("<div class=\"card\">"), "div renders with safe attr: {out}");
    assert!(out.contains("<b>hi</b>"), "nested safe tag renders: {out}");
    // Drop-list applies at block level too.
    let out = render_block_html("<div>\n<mk>x</mk>\n</div>\n", &[], &["mk"]);
    assert!(!out.to_lowercase().contains("<mk"), "drop-list tag removed: {out}");
    assert!(out.contains("x"), "its text stays inert: {out}");
}

#[test]
fn block_void_elements_get_no_closer() {
    let out = render_block_html("<div>\n<br>\n<img src=\"/a.png\">\n</div>\n", &[], &[]);
    assert!(!out.contains("</br>"), "void element never gets a closer: {out}");
    assert!(!out.contains("</img>"), "void element never gets a closer: {out}");
    assert!(out.contains("<img src=\"/a.png\">"), "img renders: {out}");
}

#[test]
fn block_comment_smuggling_is_dropped_entirely() {
    // A comment inside a type-6 block is dropped WITH its contents — the classic
    // `<!-- <script> -->` smuggle must not survive as markup or as text.
    let out = render_block_html("<div>\n<!-- <script>alert(1)</script> -->\n</div>\n", &[], &[]);
    assert!(!out.to_lowercase().contains("script"), "comment + contents dropped: {out}");
    assert!(!out.contains("<!--") && !out.contains("&lt;!--"), "no comment markup left: {out}");
    assert!(out.contains("<div>") && out.contains("</div>"), "the block still renders: {out}");
    // An UNTERMINATED comment can never complete: nothing of it may render.
    let out = render_block_html("<div>\n<!-- <script>alert(1)\n", &[], &[]);
    assert!(!out.to_lowercase().contains("<script"), "unterminated comment stays inert: {out}");
}

#[test]
fn block_stream_matches_one_shot_and_never_leaks_a_prefix() {
    let cases: &[&str] = &[
        "<details>\n<summary>Sources</summary>\nbody\n</details>\n",
        "<div>\n<img src=x onerror=alert(1)>\n</div>\n",
        "<div>\n<script>alert(1)</script>\n</div>\n",
        "<div>\n<iframe src=javascript:alert(1)></iframe>\n</div>\n",
        "<div>\n<svg onload=alert(1)></svg>\n</div>\n",
        "<div>\n<a href=\"javascript:alert(1)\">x</a>\n</div>\n",
        "<div>\n<!-- <script>alert(1)</script> -->\n</div>\n",
        "<div onclick=alert(1)>\nbody\n</div>\n",
        "<mytag onload=alert(1)>\nbody\n</mytag>\n",
    ];
    for md in cases {
        // Streamed char-by-char == one-shot, in allow-all and restrict mode.
        for allow in [&[][..], &["div", "details", "summary", "a", "b", "img"][..]] {
            let mut one = StreamParser::new()
                .with_html_sanitize(true, allow.iter().map(|s| s.to_string()).collect(), vec![])
                .with_block_html(true);
            one.append(md);
            one.finalize();
            let mut streamed = StreamParser::new()
                .with_html_sanitize(true, allow.iter().map(|s| s.to_string()).collect(), vec![])
                .with_block_html(true);
            let mut buf = [0u8; 4];
            for ch in md.chars() {
                streamed.append(ch.encode_utf8(&mut buf));
            }
            streamed.finalize();
            assert_eq!(collect(&one), collect(&streamed), "BLOCK STREAM DIVERGENCE {md:?}");
        }
    }
}

/// Mirror of [`check_prefix`] at BLOCK level: at EVERY append boundary of a
/// malicious block payload, the HTML emitted so far must contain no executable
/// construct — no real `<script>`/`<iframe>`/`<svg>`, no `on*` handler, no
/// `javascript:`. Speculative closers make the emitted tree complete at each
/// prefix; they must never complete it around something live.
fn check_block_prefix(md: &str, allow: &[&str]) {
    let mut p = StreamParser::new()
        .with_html_sanitize(true, allow.iter().map(|s| s.to_string()).collect(), vec![])
        .with_block_html(true);
    let mut sent = String::new();
    for ch in md.chars() {
        let mut buf = [0u8; 4];
        p.append(ch.encode_utf8(&mut buf));
        sent.push(ch);
        let html = collect(&p);
        assert_no_exec_tags(&html, &format!("block prefix {sent:?} -> {html}"));
    }
    p.finalize();
    assert_no_exec_tags(&collect(&p), &format!("block finalize {md:?}"));
}

#[test]
fn block_stream_prefix_no_real_exec() {
    let cases: &[&str] = &[
        "<div>\n<script>alert(1)</script>\n</div>\n",
        "<div>\n<img src=x onerror=alert(1)>\n</div>\n",
        "<div>\n<svg onload=alert(1)></svg>\n</div>\n",
        "<div>\n<iframe src=javascript:alert(1)></iframe>\n</div>\n",
        "<div>\n<a href=javascript:alert(1)>x</a>\n</div>\n",
        "<div>\n<b onmouseover=alert(1)>x</b>\n</div>\n",
        "<div onclick=alert(1) id=q>\nbody\n</div>\n",
        "<script>alert(1)</script>\n",
        "<svg onload=alert(1)>\nx\n</svg>\n",
        "<div>\n<!-- --><script>alert(1)</script>\n</div>\n",
        "<mytag>\n<img src=x onerror=alert(1)>\n</mytag>\n",
    ];
    for md in cases {
        check_block_prefix(md, &[]);
        check_block_prefix(md, &["div", "a", "b", "img", "mytag"]);
    }
}

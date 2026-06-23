//! Security regression tests. The core's whole promise is that its HTML output
//! is XSS-safe to inject (default config, raw HTML escaped). The subtle class
//! of bug here: a dangerous URL scheme obfuscated with HTML entities, backslash
//! escapes, control characters, or case so it slips past the scheme filter but
//! is reconstituted by the browser. The filter must run on the *decoded* form.

use flux_md_core::StreamParser;

fn render(md: &str) -> String {
    let mut p = StreamParser::new();
    p.append(md);
    p.finalize();
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

/// None of these obfuscations may yield a live `javascript:` (etc.) href.
#[test]
fn dangerous_link_schemes_are_neutralized() {
    let attacks = [
        "javascript:alert(1)",          // baseline
        "javascript&#58;alert(1)",      // numeric entity colon
        "javascript&#x3a;alert(1)",     // hex entity colon
        "javascript&#X3A;alert(1)",     // hex entity colon, upper X
        "javascript\\:alert(1)",        // backslash-escaped colon
        "JAVASCRIPT&#58;alert(1)",      // uppercase + entity
        "&#106;avascript:alert(1)",     // entity-encoded 'j'
        "java&#9;script:alert(1)",      // embedded tab
        "java&#10;script:alert(1)",     // embedded newline
        "  javascript:alert(1)",        // leading whitespace
        "vbscript&#58;msgbox(1)",       // vbscript via entity
        "data:text/&#104;tml,<script>", // data:text/html via entity 'h'
        "data:text/javascript,alert(1)",
    ];
    for a in attacks {
        let md = format!("[x]({a})\n");
        let out = render(&md);
        assert!(
            !out.contains("\"javascript:") && !out.contains("\"vbscript:"),
            "live dangerous scheme leaked for {a:?}: {out}"
        );
        assert!(!out.contains("data:text/html"), "data:text/html leaked for {a:?}: {out}");
        assert!(!out.contains("data:text/javascript"), "data:text/javascript leaked for {a:?}: {out}");
        // The blocked form is href="#".
        assert!(out.contains("href=\"#\""), "expected blocked href=# for {a:?}: {out}");
    }
}

/// Same obfuscations on image `src` must also be neutralized.
#[test]
fn dangerous_image_schemes_are_neutralized() {
    let attacks = [
        "javascript:alert(1)",
        "javascript&#58;alert(1)",
        "javascript\\:alert(1)",
        "vbscript&#58;x",
        "data:text/html,<script>alert(1)</script>",
    ];
    for a in attacks {
        let md = format!("![x]({a})\n");
        let out = render(&md);
        assert!(!out.contains("\"javascript:"), "img js leaked for {a:?}: {out}");
        assert!(!out.contains("data:text/html"), "img data:text/html leaked for {a:?}: {out}");
        assert!(out.contains("src=\"#\""), "expected blocked src=# for {a:?}: {out}");
    }
}

/// Legitimate URLs must still render (the fix must not over-block).
#[test]
fn legitimate_urls_still_render() {
    assert!(render("[x](https://example.com/a?b=1&c=2)\n")
        .contains("href=\"https://example.com/a?b=1&amp;c=2\""));
    assert!(render("[x](/relative/path)\n").contains("href=\"/relative/path\""));
    assert!(render("[x](mailto:a@b.com)\n").contains("href=\"mailto:a@b.com\""));
    assert!(render("[x](ftp://host/file)\n").contains("href=\"ftp://host/file\""));
    // A word that merely contains "javascript" is fine as a path.
    assert!(render("[x](/docs/javascript-guide)\n").contains("href=\"/docs/javascript-guide\""));
    // Images.
    assert!(render("![x](https://example.com/i.png)\n").contains("src=\"https://example.com/i.png\""));
    assert!(render("![x](data:image/png;base64,iVBOR)\n").contains("src=\"data:image/png;base64,iVBOR\""));
}

/// CommonMark URI autolinks (`<scheme:…>`) must route through the same
/// dangerous-scheme filter as regular links: a `javascript:`/`vbscript:`
/// autolink emits href="#", while a safe `https:` autolink still links.
#[test]
fn dangerous_autolink_schemes_are_neutralized() {
    for a in ["<javascript:alert(1)>", "<vbscript:msgbox(1)>", "<JaVaScRiPt:alert(1)>", "<file:///etc/passwd>"] {
        let out = render(&format!("{a}\n"));
        assert!(
            !out.contains("href=\"javascript:") && !out.contains("href=\"vbscript:"),
            "live dangerous autolink scheme leaked for {a:?}: {out}"
        );
        assert!(
            out.contains("href=\"#\""),
            "expected blocked href=# for autolink {a:?}: {out}"
        );
    }
}

/// A safe URI autolink still produces a working href (the fix must not over-block).
#[test]
fn safe_autolink_still_works() {
    let out = render("<https://example.com>\n");
    assert!(
        out.contains("href=\"https://example.com\""),
        "safe https autolink should still link: {out}"
    );
    // An email autolink is unaffected (separate code path).
    assert!(
        render("<a@b.com>\n").contains("href=\"mailto:a@b.com\""),
        "email autolink should still link"
    );
}

/// Raw HTML is escaped by default (unsafe_html off) — no tag injection.
#[test]
fn raw_html_is_escaped_by_default() {
    let out = render("<script>alert(1)</script>\n");
    assert!(!out.contains("<script>"), "raw <script> must be escaped: {out}");
    assert!(out.contains("&lt;script&gt;"), "expected escaped form: {out}");
}

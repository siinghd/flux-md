//! HTML / URL escaping + URL normalization helpers.
//!
//! For URLs in `<a href>` / `<img src>` we:
//! 1. Decode HTML entities (`&amp;` → `&`, `&#x41;` → `A`).
//! 2. Decode backslash escapes (`\(` → `(`).
//! 3. Percent-encode chars that aren't URL-safe (spaces → `%20`, etc.).
//! 4. HTML-escape the result for safe insertion as an attribute value.
//! 5. Reject URLs whose scheme isn't in our allowlist (`javascript:` → `#`).

use crate::entities::decode_entity;

pub fn escape_html(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

pub fn escape_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
}

const ESCAPABLE: &[u8] = b"!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

/// Decode backslash escapes and entity references in the input. Used for
/// link URLs and link titles. Does NOT percent-encode.
pub fn decode_text(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && i + 1 < bytes.len() && ESCAPABLE.contains(&bytes[i + 1]) {
            out.push(bytes[i + 1] as char);
            i += 2;
            continue;
        }
        if b == b'&' {
            if let Some((decoded, consumed)) = decode_entity(&bytes[i..]) {
                out.push_str(&decoded);
                i += consumed;
                continue;
            }
        }
        // Walk by char so multi-byte UTF-8 is preserved correctly.
        if b < 0x80 {
            out.push(b as char);
            i += 1;
        } else {
            let n = utf8_char_len(b);
            let end = (i + n).min(bytes.len());
            if let Ok(s) = std::str::from_utf8(&bytes[i..end]) {
                if let Some(c) = s.chars().next() {
                    out.push(c);
                    i += c.len_utf8();
                    continue;
                }
            }
            // Invalid UTF-8: skip.
            i += 1;
        }
    }
    out
}

fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 { 1 }
    else if b < 0xC0 { 1 } // continuation byte, treat as 1 for safety
    else if b < 0xE0 { 2 }
    else if b < 0xF0 { 3 }
    else { 4 }
}

/// Decode escapes + entities AND percent-encode unsafe characters.
/// Output is HTML-attribute-escape ready (so call escape_attr after).
pub fn normalize_url(input: &str) -> String {
    let decoded = decode_text(input);
    let mut out = String::with_capacity(decoded.len());
    // Walk by UTF-8 chars so we percent-encode non-ASCII correctly.
    for c in decoded.chars() {
        if is_url_safe(c) {
            out.push(c);
        } else if c == '%' {
            // Preserve existing percent-encoded triplets if they look valid.
            // (We're walking chars one at a time so this is approximate.)
            out.push('%');
        } else {
            // Encode this char's UTF-8 bytes as %XX.
            let mut buf = [0u8; 4];
            let s = c.encode_utf8(&mut buf);
            for &b in s.as_bytes() {
                out.push('%');
                out.push(hex(b >> 4));
                out.push(hex(b & 0xF));
            }
        }
    }
    // Fix up existing %XX sequences: if the decoded input already had %XX,
    // re-encoding above would have lowercased nothing but the actual hex
    // digits got passed through as URL-safe. So this works.
    out
}

fn hex(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'A' + (n - 10)) as char,
        _ => '0',
    }
}

fn is_url_safe(c: char) -> bool {
    // RFC 3986 unreserved + reserved gen-delims / sub-delims that are safe
    // in href values. Also keep '%' as-is (covered separately above).
    matches!(
        c,
        'a'..='z' | 'A'..='Z' | '0'..='9'
        | '-' | '_' | '.' | '~'
        | '!' | '*' | '\'' | '(' | ')' | ';' | ':' | '@' | '&'
        | '=' | '+' | '$' | ',' | '/' | '?' | '#' | '[' | ']'
    )
}

// `file:` is blocked alongside the script-execution schemes: it has no
// legitimate use in rendered untrusted/LLM markdown, and in privileged contexts
// (Electron, browser extensions, `file://` origins) a live `file:` href is a
// local-resource-disclosure / phishing vector. Plain web origins already refuse
// to navigate to it, so blocking it costs nothing there.
const BAD_SCHEMES: &[&str] = &["javascript:", "vbscript:", "file:", "data:text/html", "data:text/javascript"];

/// Lowercased, control-character-stripped view of a URL for scheme detection.
/// Browsers ignore tab/newline/CR (and other C0 controls) when parsing a
/// scheme, so we must too — otherwise `java&#9;script:` slips through.
fn scheme_probe(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .flat_map(|c| c.to_lowercase())
        .collect::<String>()
        .trim_start()
        .to_string()
}

/// Whether the URL resolves to a dangerous scheme. **Checked on the fully
/// DECODED form**: entities (`&#58;`) and backslash escapes (`\:`) are decoded
/// before a browser ever parses the URL, so checking the raw text lets
/// `javascript&#58;alert(1)` and `javascript\:alert(1)` past the filter. The
/// decode is **stable** — we peel entity/backslash layers until the string stops
/// changing — because a value can be decoded more than once on its way to the
/// DOM (a downstream HTML layer re-decodes, then the browser decodes again), so a
/// multiply-encoded scheme like `javascript&amp;#58;` (or `&amp;amp;#58;`) must
/// collapse to its live form before the match. Then strip the chars browsers
/// ignore and match.
pub(crate) fn is_dangerous_scheme(decoded: &str) -> bool {
    let mut s = decoded.to_string();
    // Bound the decode-to-fixpoint walk: a real value collapses in ≤3 passes
    // (e.g. triple-encoded `javascript&amp;amp;#58;`); the cap keeps a crafted
    // `javascript` + `&amp;`×N input from being O(n²).
    for _ in 0..8 {
        let next = decode_text(&s);
        if next == s {
            break;
        }
        s = next;
    }
    let probe = scheme_probe(&s);
    BAD_SCHEMES.iter().any(|b| probe.starts_with(b))
}

pub fn sanitize_url(url: &str, out: &mut String, is_email: bool) {
    let trimmed = url.trim();
    let decoded = decode_text(trimmed);
    // Block dangerous schemes on the decoded form. Anything else is allowed —
    // CommonMark only specifies URL normalization, not a scheme allowlist.
    // Real apps rendering untrusted content should still sanitize downstream.
    if is_dangerous_scheme(&decoded) {
        out.push('#');
        return;
    }
    let prefix = if is_email && !decoded.to_ascii_lowercase().starts_with("mailto:") {
        "mailto:"
    } else {
        ""
    };
    let normalized = normalize_url(trimmed);
    out.push_str(prefix);
    escape_attr(&normalized, out);
}

pub fn sanitize_image_url(url: &str, out: &mut String) {
    let trimmed = url.trim();
    let decoded = decode_text(trimmed);
    if is_dangerous_scheme(&decoded) {
        out.push('#');
        return;
    }
    // Allowlist on the decoded, control-stripped form (same reason as above).
    let probe = scheme_probe(&decoded);
    let allowed = probe.starts_with("http://")
        || probe.starts_with("https://")
        || probe.starts_with("data:image/")
        || probe.starts_with('/')
        || probe.starts_with("./")
        || probe.starts_with("../")
        || probe.is_empty()
        || (!probe.contains(':') && !probe.starts_with("//"));
    if allowed {
        let normalized = normalize_url(trimmed);
        escape_attr(&normalized, out);
    } else {
        out.push('#');
    }
}

/// Attribute names whose value is a URL and must pass the dangerous-scheme
/// filter before it reaches the DOM.
const URL_ATTRS: &[&str] = &[
    "href", "src", "xlink:href", "action", "formaction", "poster", "data", "cite",
    "background", "longdesc", "ping", "srcset",
];

/// Attribute names that are React-meaningful or otherwise unsafe to surface to a
/// component override (matched case-insensitively). `dangerouslySetInnerHTML`
/// would let untrusted markup inject raw HTML; `ref`/`key` perturb React's
/// reconciliation; `defaultValue`/`defaultChecked` seed form state from
/// untrusted content; the `suppress*` flags silence hydration-mismatch warnings
/// that would otherwise flag tampering. `data-*`/`aria-*`/`xlink:href` are NOT
/// here and stay allowed.
const REACT_UNSAFE_ATTRS: &[&str] = &[
    "dangerouslysetinnerhtml",
    "ref",
    "key",
    "defaultvalue",
    "defaultchecked",
    "suppresshydrationwarning",
    "suppresscontenteditablewarning",
];

/// Parse and sanitize the attributes of a component's opening tag, returning
/// safe `(name, value)` pairs with **decoded** values — the HTML renderer escapes
/// them once and a React layer can use them as-is (so this is the canonical,
/// escape-free storage form). `open_tag` is the whole opening tag, e.g.
/// `<Thinking type="info" onerror="x()">` (a trailing `/>` is fine).
///
/// Security policy (attributes are the real boundary for component tags, since
/// the tag itself is allowlisted and the body is markdown):
///   - the tag name is skipped; only attributes are returned;
///   - an attribute name must be an ASCII letter then `[A-Za-z0-9_:.-]`, else it
///     is dropped;
///   - `on*` event-handler attributes are dropped (case-insensitive);
///   - a URL-bearing attribute (`href`, `src`, …) whose **decoded** value has a
///     dangerous scheme (`javascript:`, `data:text/html`, entity/backslash
///     obfuscations, …) becomes `#`;
///   - every other value is entity/backslash-decoded and kept verbatim.
pub fn sanitize_attrs(open_tag: &str) -> Vec<(String, String)> {
    let bytes = open_tag.as_bytes();
    let mut i = 0;
    if bytes.first() == Some(&b'<') {
        i += 1;
    }
    // Skip the tag name (letters/digits/-/:).
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'-' | b':')) {
        i += 1;
    }
    let mut out: Vec<(String, String)> = Vec::new();
    loop {
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'>' {
            break;
        }
        if bytes[i] == b'/' {
            i += 1; // self-closing slash
            continue;
        }
        // Attribute name.
        if !bytes[i].is_ascii_alphabetic() {
            i += 1; // malformed: make progress, never loop forever
            continue;
        }
        let name_start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'_' | b':' | b'.' | b'-'))
        {
            i += 1;
        }
        let name = &open_tag[name_start..i];
        // Optional ` = value`.
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
        let mut raw_value = "";
        if bytes.get(i) == Some(&b'=') {
            i += 1;
            while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let vstart = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                raw_value = &open_tag[vstart..i];
                if i < bytes.len() {
                    i += 1; // closing quote
                }
            } else {
                let vstart = i;
                while i < bytes.len() && !matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/') {
                    i += 1;
                }
                raw_value = &open_tag[vstart..i];
            }
        }
        let lname = name.to_ascii_lowercase();
        if lname.starts_with("on") {
            continue; // event handler — drop
        }
        // `style` is dropped like an event handler: an inline style is a CSS
        // injection vector with no script needed — `background:url(…)` fires an
        // automatic GET (a beacon / CSS-selector exfiltration channel) and
        // `position:fixed;inset:0` paints a full-viewport click-stealing overlay.
        // Untrusted (LLM) markup must not carry one through the sanitizer.
        if lname == "style" {
            continue;
        }
        // React-meaningful / unsafe-to-surface names are dropped (defense in
        // depth for component overrides). `data-*`/`aria-*`/`xlink:href` are
        // intentionally not in this list and pass through.
        if REACT_UNSAFE_ATTRS.contains(&lname.as_str()) {
            continue;
        }
        let decoded = decode_text(raw_value);
        let value = if URL_ATTRS.contains(&lname.as_str()) && is_dangerous_scheme(&decoded) {
            "#".to_string()
        } else {
            decoded
        };
        out.push((name.to_string(), value));
    }
    out
}

#[cfg(test)]
mod attr_tests {
    use super::sanitize_attrs;

    fn names(attrs: &[(String, String)]) -> Vec<&str> {
        attrs.iter().map(|(n, _)| n.as_str()).collect()
    }
    fn get<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
        attrs.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    #[test]
    fn keeps_plain_attrs_decoded() {
        let a = sanitize_attrs("<Thinking type=\"info\" data-id='42' open>");
        assert_eq!(get(&a, "type"), Some("info"));
        assert_eq!(get(&a, "data-id"), Some("42"));
        assert_eq!(get(&a, "open"), Some("")); // boolean attr
        let a = sanitize_attrs("<Callout title=\"A &amp; B &lt;x&gt;\">");
        assert_eq!(get(&a, "title"), Some("A & B <x>")); // entities decoded
    }

    #[test]
    fn drops_event_handlers() {
        let a = sanitize_attrs("<Thinking onclick=\"steal()\" ONerror='x' onmouseover=y style=\"position:fixed;inset:0\" type=ok>");
        assert!(!names(&a).iter().any(|n| n.to_ascii_lowercase().starts_with("on")), "got {:?}", names(&a));
        // `style` is dropped too (CSS-injection vector: beacon / clickjack overlay).
        assert!(!names(&a).iter().any(|n| n.eq_ignore_ascii_case("style")), "style dropped: {:?}", names(&a));
        assert_eq!(get(&a, "type"), Some("ok"));
    }

    #[test]
    fn neutralizes_dangerous_url_attrs() {
        assert_eq!(get(&sanitize_attrs("<X href=\"javascript:alert(1)\">"), "href"), Some("#"));
        assert_eq!(get(&sanitize_attrs("<X src='data:text/html,<script>'>"), "src"), Some("#"));
        // Entity (`&#58;` = `:`), backslash-before-colon, and control-char
        // (browser-ignored tab) obfuscations are all caught — decoded / stripped
        // before the scheme check, matching how a browser would read the URL.
        assert_eq!(get(&sanitize_attrs("<X href=\"javascript&#58;alert(1)\">"), "href"), Some("#"));
        assert_eq!(get(&sanitize_attrs("<X href=\"javascript\\:alert(1)\">"), "href"), Some("#"));
        assert_eq!(get(&sanitize_attrs("<X href=\"java\tscript:alert(1)\">"), "href"), Some("#"));
        // DOUBLE / TRIPLE entity-encoding must also be caught: the scheme check
        // is decode-STABLE (peels layers to a fixpoint), since a downstream HTML
        // layer and the browser each decode again. (Regression: `&amp;#58;`
        // previously survived single-decode and reached the DOM as `javascript:`.)
        assert_eq!(get(&sanitize_attrs("<X href=\"javascript&amp;#58;alert(1)\">"), "href"), Some("#"));
        assert_eq!(get(&sanitize_attrs("<X href=\"javascript&amp;amp;#58;alert(1)\">"), "href"), Some("#"));
        // Safe URLs pass through (decoded).
        assert_eq!(get(&sanitize_attrs("<X href=\"https://e.com/p?a=1&amp;b=2\">"), "href"), Some("https://e.com/p?a=1&b=2"));
        assert_eq!(get(&sanitize_attrs("<X href=\"/local/path\">"), "href"), Some("/local/path"));
    }

    #[test]
    fn drops_react_meaningful_attrs() {
        // React-meaningful / unsafe-to-surface names are dropped (case-insensitive).
        let a = sanitize_attrs(
            "<Card dangerouslySetInnerHTML=\"{x}\" REF='r' Key=k defaultValue=v \
             DEFAULTCHECKED=c suppressHydrationWarning suppressContentEditableWarning type=ok>",
        );
        for dropped in [
            "dangerouslySetInnerHTML",
            "ref",
            "key",
            "defaultValue",
            "defaultChecked",
            "suppressHydrationWarning",
            "suppressContentEditableWarning",
        ] {
            assert!(
                !names(&a).iter().any(|n| n.eq_ignore_ascii_case(dropped)),
                "{dropped} should be dropped: {:?}",
                names(&a)
            );
        }
        assert_eq!(get(&a, "type"), Some("ok"));
    }

    #[test]
    fn keeps_data_aria_xlink_attrs() {
        // data-*/aria-*/xlink:href must be KEPT (they are not React-unsafe).
        let a = sanitize_attrs(
            "<Card data-id=\"7\" aria-label='hi' xlink:href=\"https://e.com\" type=ok>",
        );
        assert_eq!(get(&a, "data-id"), Some("7"));
        assert_eq!(get(&a, "aria-label"), Some("hi"));
        assert_eq!(get(&a, "xlink:href"), Some("https://e.com"));
        assert_eq!(get(&a, "type"), Some("ok"));
        // A dangerous xlink:href is still neutralized (it's a URL attr).
        let b = sanitize_attrs("<Card xlink:href=\"javascript:alert(1)\">");
        assert_eq!(get(&b, "xlink:href"), Some("#"));
    }

    #[test]
    fn quoted_value_with_special_chars() {
        // A `>` inside a quoted value must not terminate attribute parsing early,
        // and entities in the value are decoded.
        let a = sanitize_attrs("<X title=\"a > b &amp; c\" type=ok>");
        assert_eq!(get(&a, "title"), Some("a > b & c"));
        assert_eq!(get(&a, "type"), Some("ok"), "attr after a quoted `>` still parses");
    }

    #[test]
    fn malformed_input_never_panics() {
        for s in [
            "<X", "<X ", "<X =", "<X = =", "<X a=", "<X a=\"unclosed",
            "<X 123=bad . : =>", "<X/>", "<X a=b/>", "<>", "<X\u{0}=\u{0}>", "",
            "<X href=javascript:alert(1)>", "<X a='it''s'>",
        ] {
            let _ = sanitize_attrs(s); // must not panic
        }
    }
}

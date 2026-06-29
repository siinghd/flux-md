//! HTML entity / numeric character reference decoding.
//!
//! `decode_entity(bytes)` looks at bytes starting at `&` and tries to decode
//! a complete entity. Returns the decoded text + the number of bytes
//! consumed (including the leading `&` and trailing `;`). Returns None if
//! the bytes don't form a recognized entity.
//!
//! Coverage: numeric refs (&#65; &#x41; &#X41;) plus a large subset of HTML5
//! named entities — enough for the vast majority of real-world Markdown.

pub fn decode_entity(bytes: &[u8]) -> Option<(String, usize)> {
    if bytes.first() != Some(&b'&') {
        return None;
    }
    // Numeric: &#NNN; or &#xHHH;
    if bytes.get(1) == Some(&b'#') {
        let (hex, start) = if matches!(bytes.get(2), Some(&b'x') | Some(&b'X')) {
            (true, 3)
        } else {
            (false, 2)
        };
        // Bound the digit scan: the longest valid reference is 7 decimal digits
        // (1114111 = U+10FFFF) or 6 hex digits (10FFFF). Anything longer is
        // invalid per CommonMark, so capping here keeps `decode_entity` O(1) —
        // without the cap, input like `&#&#&#…` (which never contains a `;`)
        // re-scans to EOF on every `&`, making the whole decode O(n²). Mirrors
        // the named branch below, which is already bounded.
        let budget = if hex { 6 } else { 7 };
        let max = (start + budget).min(bytes.len());
        let mut i = start;
        while i < max && bytes[i] != b';' {
            i += 1;
        }
        if i == start || i >= bytes.len() || bytes[i] != b';' {
            return None;
        }
        let num_str = std::str::from_utf8(&bytes[start..i]).ok()?;
        let code = if hex {
            u32::from_str_radix(num_str, 16).ok()?
        } else {
            num_str.parse::<u32>().ok()?
        };
        let consumed = i + 1;
        // CommonMark: 0 → U+FFFD; surrogates and out-of-range → U+FFFD too,
        // BUT invalid numeric refs (no digits, > U+10FFFF) should leave the
        // text un-decoded. char::from_u32 returns None for out-of-range, so
        // we treat that as "not a valid entity."
        if code > 0x10FFFF {
            return None;
        }
        let c = if code == 0 || (0xD800..=0xDFFF).contains(&code) {
            '\u{FFFD}'
        } else {
            match char::from_u32(code) {
                Some(c) => c,
                None => return None,
            }
        };
        return Some((c.to_string(), consumed));
    }
    // Named: &name;
    // Find the semicolon (max 32 chars).
    let mut i = 1;
    while i < bytes.len() && i < 33 && bytes[i] != b';' {
        if !(bytes[i].is_ascii_alphanumeric()) {
            return None;
        }
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b';' {
        return None;
    }
    let name = std::str::from_utf8(&bytes[1..i]).ok()?;
    let decoded = lookup_named(name)?;
    Some((decoded.to_string(), i + 1))
}

fn lookup_named(name: &str) -> Option<&'static str> {
    // Case-sensitive. CommonMark uses the HTML5 named character references.
    // This is a curated list covering the entities seen in the CommonMark
    // 0.31 spec suite plus the most common HTML5 entities.
    match name {
        // Basic
        "amp" => Some("&"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "quot" => Some("\""),
        "apos" => Some("'"),
        "nbsp" => Some("\u{00A0}"),
        // Punctuation
        "iexcl" => Some("¡"),
        "iquest" => Some("¿"),
        "laquo" => Some("«"),
        "raquo" => Some("»"),
        "lsquo" => Some("\u{2018}"),
        "rsquo" => Some("\u{2019}"),
        "ldquo" => Some("\u{201C}"),
        "rdquo" => Some("\u{201D}"),
        "sbquo" => Some("\u{201A}"),
        "bdquo" => Some("\u{201E}"),
        "hellip" => Some("\u{2026}"),
        "mdash" => Some("\u{2014}"),
        "ndash" => Some("\u{2013}"),
        "bull" => Some("\u{2022}"),
        "middot" => Some("·"),
        "dagger" => Some("\u{2020}"),
        "Dagger" => Some("\u{2021}"),
        "permil" => Some("\u{2030}"),
        "prime" => Some("\u{2032}"),
        "Prime" => Some("\u{2033}"),
        "lsaquo" => Some("\u{2039}"),
        "rsaquo" => Some("\u{203A}"),
        "oline" => Some("\u{203E}"),
        "frasl" => Some("\u{2044}"),
        // Currency
        "cent" => Some("¢"),
        "pound" => Some("£"),
        "curren" => Some("¤"),
        "yen" => Some("¥"),
        "euro" => Some("€"),
        // Symbols
        "copy" => Some("©"),
        "reg" => Some("®"),
        "trade" => Some("\u{2122}"),
        "sect" => Some("§"),
        "para" => Some("¶"),
        "deg" => Some("°"),
        "plusmn" => Some("±"),
        "times" => Some("×"),
        "divide" => Some("÷"),
        "minus" => Some("\u{2212}"),
        "frac14" => Some("¼"),
        "frac12" => Some("½"),
        "frac34" => Some("¾"),
        "sup1" => Some("¹"),
        "sup2" => Some("²"),
        "sup3" => Some("³"),
        "micro" => Some("µ"),
        "not" => Some("¬"),
        "macr" => Some("¯"),
        "acute" => Some("´"),
        "cedil" => Some("¸"),
        "uml" => Some("¨"),
        "ordf" => Some("ª"),
        "ordm" => Some("º"),
        "shy" => Some("\u{00AD}"),
        // Latin letters (a tiny subset; most spec tests use these)
        "AElig" => Some("Æ"), "aelig" => Some("æ"),
        "Aacute" => Some("Á"), "aacute" => Some("á"),
        "Acirc" => Some("Â"), "acirc" => Some("â"),
        "Agrave" => Some("À"), "agrave" => Some("à"),
        "Aring" => Some("Å"), "aring" => Some("å"),
        "Atilde" => Some("Ã"), "atilde" => Some("ã"),
        "Auml" => Some("Ä"), "auml" => Some("ä"),
        "Ccedil" => Some("Ç"), "ccedil" => Some("ç"),
        "ETH" => Some("Ð"), "eth" => Some("ð"),
        "Eacute" => Some("É"), "eacute" => Some("é"),
        "Ecirc" => Some("Ê"), "ecirc" => Some("ê"),
        "Egrave" => Some("È"), "egrave" => Some("è"),
        "Euml" => Some("Ë"), "euml" => Some("ë"),
        "Iacute" => Some("Í"), "iacute" => Some("í"),
        "Icirc" => Some("Î"), "icirc" => Some("î"),
        "Igrave" => Some("Ì"), "igrave" => Some("ì"),
        "Iuml" => Some("Ï"), "iuml" => Some("ï"),
        "Ntilde" => Some("Ñ"), "ntilde" => Some("ñ"),
        "Oacute" => Some("Ó"), "oacute" => Some("ó"),
        "Ocirc" => Some("Ô"), "ocirc" => Some("ô"),
        "Ograve" => Some("Ò"), "ograve" => Some("ò"),
        "Oslash" => Some("Ø"), "oslash" => Some("ø"),
        "Otilde" => Some("Õ"), "otilde" => Some("õ"),
        "Ouml" => Some("Ö"), "ouml" => Some("ö"),
        "szlig" => Some("ß"),
        "THORN" => Some("Þ"), "thorn" => Some("þ"),
        "Uacute" => Some("Ú"), "uacute" => Some("ú"),
        "Ucirc" => Some("Û"), "ucirc" => Some("û"),
        "Ugrave" => Some("Ù"), "ugrave" => Some("ù"),
        "Uuml" => Some("Ü"), "uuml" => Some("ü"),
        "Yacute" => Some("Ý"), "yacute" => Some("ý"),
        "yuml" => Some("ÿ"),
        // Math
        "infin" => Some("\u{221E}"),
        "ne" => Some("\u{2260}"),
        "le" => Some("\u{2264}"),
        "ge" => Some("\u{2265}"),
        "sum" => Some("\u{2211}"),
        "prod" => Some("\u{220F}"),
        "int" => Some("\u{222B}"),
        "asymp" => Some("\u{2248}"),
        "equiv" => Some("\u{2261}"),
        "radic" => Some("\u{221A}"),
        "part" => Some("\u{2202}"),
        "exist" => Some("\u{2203}"),
        "forall" => Some("\u{2200}"),
        "empty" => Some("\u{2205}"),
        "nabla" => Some("\u{2207}"),
        "isin" => Some("\u{2208}"),
        "notin" => Some("\u{2209}"),
        "ni" => Some("\u{220B}"),
        "cap" => Some("\u{2229}"),
        "cup" => Some("\u{222A}"),
        "sub" => Some("\u{2282}"),
        "sup" => Some("\u{2283}"),
        "supe" => Some("\u{2287}"),
        "sube" => Some("\u{2286}"),
        "and" => Some("\u{2227}"),
        "or" => Some("\u{2228}"),
        "perp" => Some("\u{22A5}"),
        "ang" => Some("\u{2220}"),
        "alpha" => Some("α"), "Alpha" => Some("Α"),
        "beta" => Some("β"), "Beta" => Some("Β"),
        "gamma" => Some("γ"), "Gamma" => Some("Γ"),
        "delta" => Some("δ"), "Delta" => Some("Δ"),
        "epsilon" => Some("ε"), "Epsilon" => Some("Ε"),
        "zeta" => Some("ζ"), "Zeta" => Some("Ζ"),
        "eta" => Some("η"), "Eta" => Some("Η"),
        "theta" => Some("θ"), "Theta" => Some("Θ"),
        "iota" => Some("ι"), "Iota" => Some("Ι"),
        "kappa" => Some("κ"), "Kappa" => Some("Κ"),
        "lambda" => Some("λ"), "Lambda" => Some("Λ"),
        "mu" => Some("μ"), "Mu" => Some("Μ"),
        "nu" => Some("ν"), "Nu" => Some("Ν"),
        "xi" => Some("ξ"), "Xi" => Some("Ξ"),
        "omicron" => Some("ο"), "Omicron" => Some("Ο"),
        "pi" => Some("π"), "Pi" => Some("Π"),
        "rho" => Some("ρ"), "Rho" => Some("Ρ"),
        "sigma" => Some("σ"), "Sigma" => Some("Σ"),
        "tau" => Some("τ"), "Tau" => Some("Τ"),
        "upsilon" => Some("υ"), "Upsilon" => Some("Υ"),
        "phi" => Some("φ"), "Phi" => Some("Φ"),
        "chi" => Some("χ"), "Chi" => Some("Χ"),
        "psi" => Some("ψ"), "Psi" => Some("Ψ"),
        "omega" => Some("ω"), "Omega" => Some("Ω"),
        "larr" => Some("\u{2190}"),
        "uarr" => Some("\u{2191}"),
        "rarr" => Some("\u{2192}"),
        "darr" => Some("\u{2193}"),
        "harr" => Some("\u{2194}"),
        "lArr" => Some("\u{21D0}"),
        "uArr" => Some("\u{21D1}"),
        "rArr" => Some("\u{21D2}"),
        "dArr" => Some("\u{21D3}"),
        "hArr" => Some("\u{21D4}"),
        "ensp" => Some("\u{2002}"),
        "emsp" => Some("\u{2003}"),
        "thinsp" => Some("\u{2009}"),
        "zwnj" => Some("\u{200C}"),
        "zwj" => Some("\u{200D}"),
        "lrm" => Some("\u{200E}"),
        "rlm" => Some("\u{200F}"),
        // Common HTML5 entities the CommonMark spec test suite exercises.
        "Dcaron" => Some("\u{010E}"),
        "dcaron" => Some("\u{010F}"),
        "Ccaron" => Some("\u{010C}"),
        "ccaron" => Some("\u{010D}"),
        "Scaron" => Some("\u{0160}"),
        "scaron" => Some("\u{0161}"),
        "Zcaron" => Some("\u{017D}"),
        "zcaron" => Some("\u{017E}"),
        "Rcaron" => Some("\u{0158}"),
        "rcaron" => Some("\u{0159}"),
        "OElig" => Some("\u{0152}"),
        "oelig" => Some("\u{0153}"),
        "fnof" => Some("\u{0192}"),
        "circ" => Some("\u{02C6}"),
        "tilde" => Some("\u{02DC}"),
        // HTML5 named entities used in spec example 25.
        "HilbertSpace" => Some("\u{210B}"),
        "DifferentialD" => Some("\u{2146}"),
        "ClockwiseContourIntegral" => Some("\u{2232}"),
        "ngE" => Some("\u{2267}\u{0338}"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::decode_entity;

    #[test]
    fn valid_numeric_refs_decode() {
        assert_eq!(decode_entity(b"&#65;"), Some(("A".to_string(), 5)));
        assert_eq!(decode_entity(b"&#x41;"), Some(("A".to_string(), 6)));
        assert_eq!(decode_entity(b"&#X41;"), Some(("A".to_string(), 6)));
        // Longest valid forms: 7 decimal digits (U+10FFFF) / 6 hex digits.
        assert_eq!(decode_entity(b"&#1114111;").map(|(_, n)| n), Some(10));
        assert_eq!(decode_entity(b"&#x10FFFF;").map(|(_, n)| n), Some(10));
        // Zero-padded but within the digit budget still decodes.
        assert_eq!(decode_entity(b"&#0000065;"), Some(("A".to_string(), 10)));
    }

    #[test]
    fn over_long_and_unterminated_numeric_refs_are_rejected() {
        // The O(n²) DoS shape: `&#` with no terminator must NOT scan to EOF —
        // it returns None after at most the digit budget. (Correctness proxy
        // for the bound; the cap is what keeps the scan O(1) per `&`.)
        assert_eq!(decode_entity(b"&#"), None);
        assert_eq!(decode_entity(b"&#xZZZZZZZZZZZZZZZ"), None);
        let long = format!("&#{}", "9".repeat(10_000));
        assert_eq!(decode_entity(long.as_bytes()), None);
        // 8+ digit decimal (with a terminator) is invalid per CommonMark and
        // is now rejected at the digit budget rather than range-checked later.
        assert_eq!(decode_entity(b"&#00000065;"), None);
        assert_eq!(decode_entity(b"&#x1000000;"), None);
    }
}

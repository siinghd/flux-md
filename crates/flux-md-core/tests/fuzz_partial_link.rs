//! Truncate-at-every-byte-offset fuzz for speculative open-tail link rendering.
//!
//! The speculative path's `dest_streams_to_eof` MUST mirror the real
//! `read_link_destination` exactly. The sharpest pin for that mirror is: for
//! random markdown built from `[ ] ( )` + URLs + text, truncate it at EVERY byte
//! offset and assert, for each prefix:
//!
//!   1. `streamed_open(prefix) == one_shot_open(prefix)` — the streaming tail
//!      caches (open_tail=true) agree byte-for-byte with the full-rescan view of
//!      the same open prefix. This is where a `dest_streams_to_eof` vs
//!      `read_link_destination` drift would surface (one speculates an inert <a>,
//!      the other renders literal / a real link).
//!
//!   2. `finalized(prefix) == one_shot_complete(prefix)` — streaming the prefix
//!      then finalizing equals a one-shot append+finalize of the same prefix
//!      (committed output is literal, speculation is streaming-only).
//!
//! A fixed-seed PRNG (xorshift64) makes every failure reproduce; no wall-clock /
//! Math.random.

use flux_md_core::StreamParser;

/// Tiny deterministic PRNG (xorshift64) — fixed seed, no external crates.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Tokens deliberately weighted toward INLINE link/image construction so most
/// prefixes land mid-destination: `[`, `]`, `(`, `)`, `!`, URL fragments,
/// schemes, single spaces, titles, angle brackets, and inline markup inside
/// labels.
///
/// Intentionally NO block-structural tokens (`\n`, leading `>`, `-`, `|`, `#`).
/// Those create blockquotes / lists / tables whose streaming-vs-one-shot
/// lazy-continuation behavior diverges for reasons ORTHOGONAL to this feature
/// (a pre-existing property of the container caches), which would mask the one
/// thing this fuzz exists to pin: the `dest_streams_to_eof` ↔
/// `read_link_destination` mirror inside a single open paragraph (the block the
/// link-URL flash actually occurs in).
const ALPHABET: &[&str] = &[
    "[", "]", "(", ")", "!", "(", ")", // structure (extra weight on parens)
    "label", "text ", "a", "b", " ", "\\",
    "http", "://", "https://x", "example.com", "/path", "?q=1", "url",
    "<", ">", "\"title\"", "javascript:", "file:", ".org", ".png",
    "**bold**", "`code`", "x",
];

fn random_link_doc(rng: &mut Rng, max_tokens: usize) -> String {
    let n = 1 + rng.below(max_tokens);
    // Fixed inline lead: a leading `<` (or other block-significant first char)
    // would open an HTML block whose streaming detection diverges from one-shot
    // for reasons orthogonal to this feature. The lead guarantees every prefix
    // is a single inline paragraph, isolating the link mirror under test.
    let mut s = String::from("z ");
    for _ in 0..n {
        s.push_str(ALPHABET[rng.below(ALPHABET.len())]);
    }
    s
}

fn collect(p: &StreamParser) -> String {
    let mut out = String::new();
    for b in p.all_blocks() {
        out.push_str(&b.html);
    }
    out
}

fn fresh() -> StreamParser {
    // gfm_alerts on to mirror the midstream_parity helpers; the rest default.
    StreamParser::new().with_gfm_alerts(true)
}

/// Full-rescan view of an OPEN prefix (single append, no finalize).
fn one_shot_open(md: &str) -> String {
    let mut p = fresh();
    p.append(md);
    collect(&p)
}

/// Streaming view of an OPEN prefix: feed char-by-char, then one empty append to
/// fire any freshly-armed cache. No finalize.
fn streamed_open(md: &str) -> String {
    let mut p = fresh();
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.append("");
    collect(&p)
}

/// One-shot append + finalize (the committed-literal oracle).
fn one_shot_complete(md: &str) -> String {
    let mut p = fresh();
    p.append(md);
    p.finalize();
    collect(&p)
}

/// Stream char-by-char, then finalize.
fn finalized(md: &str) -> String {
    let mut p = fresh();
    let mut buf = [0u8; 4];
    for ch in md.chars() {
        p.append(ch.encode_utf8(&mut buf));
    }
    p.finalize();
    collect(&p)
}

/// Largest UTF-8 char boundary at or below `byte` (so prefixes stay valid str).
fn floor_char_boundary(s: &str, byte: usize) -> usize {
    let mut b = byte.min(s.len());
    while b < s.len() && (s.as_bytes()[b] & 0b1100_0000) == 0b1000_0000 {
        b -= 1;
    }
    b
}

#[test]
fn fuzz_truncate_every_offset_parity() {
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    let docs = 600;
    for it in 0..docs {
        let doc = random_link_doc(&mut rng, 24);
        // Truncate at EVERY byte offset (snapped to char boundaries).
        let mut byte = 0;
        while byte <= doc.len() {
            let cut = floor_char_boundary(&doc, byte);
            let prefix = &doc[..cut];

            // 1. Open view: streaming caches == full rescan.
            let so = streamed_open(prefix);
            let oo = one_shot_open(prefix);
            assert_eq!(
                so, oo,
                "OPEN parity broke (iter {it}, cut {cut}) for prefix {prefix:?}\n\
                 (whole doc {doc:?})"
            );

            // 2. Committed view: stream-then-finalize == one-shot-then-finalize.
            let fin = finalized(prefix);
            let osc = one_shot_complete(prefix);
            assert_eq!(
                fin, osc,
                "FINALIZE parity broke (iter {it}, cut {cut}) for prefix {prefix:?}\n\
                 (whole doc {doc:?})"
            );

            // Advance to the next char boundary (so the loop terminates).
            byte = if cut < doc.len() {
                cut + (doc[cut..].chars().next().map_or(1, |c| c.len_utf8()))
            } else {
                doc.len() + 1
            };
        }
    }
}

/// Directed corpus: every prefix of the canonical bug example + a handful of
/// adversarial mirror-edge constructs, each truncated at every offset.
#[test]
fn fuzz_directed_edge_prefixes() {
    let seeds = [
        "[Link text Here](https://link-url-here.org)",
        "[a](http) word",
        "[a](http word",
        "[a](url \"title\")",
        "[a](<bracketed>)",
        "[a](<bracketed url with space>)",
        "![img](https://x.png)",
        "[x](javascript:alert(1))",
        "[nested [inner](u)](http",
        "[a](url(paren))",
        "\\![esc](http",
    ];
    for doc in seeds {
        let mut byte = 0;
        while byte <= doc.len() {
            let cut = floor_char_boundary(doc, byte);
            let prefix = &doc[..cut];
            assert_eq!(
                streamed_open(prefix),
                one_shot_open(prefix),
                "directed OPEN parity broke at cut {cut} for {prefix:?} (of {doc:?})"
            );
            assert_eq!(
                finalized(prefix),
                one_shot_complete(prefix),
                "directed FINALIZE parity broke at cut {cut} for {prefix:?} (of {doc:?})"
            );
            byte = if cut < doc.len() {
                cut + (doc[cut..].chars().next().map_or(1, |c| c.len_utf8()))
            } else {
                doc.len() + 1
            };
        }
    }
}

//! Truncate-at-every-byte-offset fuzz for speculative open-tail INLINE CODE and
//! INLINE MATH rendering (the sibling of `fuzz_partial_link.rs`).
//!
//! Unlike links there is NO second scanner / mirror to pin: each `try_*`
//! (`try_code_span`, `try_dollar_math`, `try_math_delim`) is a single forward
//! scanner whose `None`-at-EOF-with-no-closer IS the "still streaming" signal,
//! and the speculative path is the SAME scan with a different terminal action.
//! So this fuzz instead pins the two parity invariants that the whole feature
//! rests on — for random single-paragraph docs built from code/math delimiters,
//! truncated at EVERY byte offset, for each prefix:
//!
//!   1. `streamed_open(prefix) == one_shot_open(prefix)` — the streaming tail
//!      caches (open_tail=true) agree byte-for-byte with the full-rescan view of
//!      the same open prefix (the per-block open_tail override path).
//!
//!   2. `finalized(prefix) == one_shot_complete(prefix)` — streaming then
//!      finalizing equals one-shot append+finalize (committed output is literal;
//!      speculation is streaming-only, so an unclosed `` ` `` / `$` / `\(`
//!      degrades to literal CommonMark).
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

/// Tokens weighted toward code/math construction so most prefixes land
/// mid-span: backtick runs, `$`/`$$`, the LaTeX `\(`,`\)`,`\[`,`\]` delimiters,
/// words, single spaces, escapes, and entity-ish `&`.
///
/// Intentionally NO block-structural OR raw-HTML tokens (`\n`, leading `>`,
/// `-`, `|`, `#`, `<`, `>`). A `\n` next to `$$`/a fence can flip an inline
/// paragraph into a *math/code block*, and a leading `<tagname>` opens an HTML
/// block — both have streaming-vs-one-shot block-classification differences that
/// are PRE-EXISTING and ORTHOGONAL to this feature (the same reason the sibling
/// `fuzz_partial_link.rs` keeps to a single inline paragraph). The fixed inline
/// lead + this alphabet guarantee every prefix is one inline paragraph, which
/// isolates the inline code/math speculation under test.
const ALPHABET: &[&str] = &[
    "`", "``", "$", "$$", // delimiters (extra weight)
    "`", "$", "\\(", "\\)", "\\[", "\\]",
    "code", "x^2", "y", "a + b", "word ", "text", " ",
    "\\", "&", "1", "2",
];

fn random_doc(rng: &mut Rng, max_tokens: usize) -> String {
    let n = 1 + rng.below(max_tokens);
    // Fixed inline lead so every prefix is a single inline paragraph (a leading
    // block-significant char would open a different block whose streaming
    // detection diverges for reasons orthogonal to this feature).
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

/// Math + alerts on (alerts to mirror the link/midstream helpers; math is the
/// feature under test). Footnotes/autolinks default off.
fn fresh() -> StreamParser {
    StreamParser::new().with_gfm_alerts(true).with_gfm_math(true)
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
    let mut rng = Rng(0x0BADC0DE_DEADBEEF);
    let docs = 600;
    for it in 0..docs {
        let doc = random_doc(&mut rng, 24);
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

            byte = if cut < doc.len() {
                cut + (doc[cut..].chars().next().map_or(1, |c| c.len_utf8()))
            } else {
                doc.len() + 1
            };
        }
    }
}

/// Directed corpus: every prefix of canonical + adversarial code/math shapes,
/// each truncated at every offset. Covers exact-run code spans, the pandoc
/// `$`-currency edge, empty-body literals, and the blank-line guard.
#[test]
fn fuzz_directed_edge_prefixes() {
    let seeds = [
        "`code here`",
        "``a ` b``",
        "$x^2 + y^2$",
        "$$E = mc^2$$",
        "\\(a+b\\)",
        "\\[a=b\\]",
        "I have $5 and $10 left",
        "a $ b $ c",
        "text `inline` and $math$ together",
        "`unclosed and a $half",
        "value `x` then `y` done",
        "&amp; in `code & span`",
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

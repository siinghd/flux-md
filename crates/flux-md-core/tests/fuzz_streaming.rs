//! Randomized robustness net. Feeds many pseudo-random markdown-stressing
//! inputs through the streaming parser at random chunk boundaries (and one-shot)
//! under every feature config, asserting the two invariants that must hold for
//! *any* input, no matter where the stream is cut:
//!
//!   1. It never panics (malformed / partial / adversarial input degrades
//!      gracefully — the core streaming guarantee).
//!   2. The block list is always well-formed: ordered, non-overlapping, unique
//!      stable ids, `start <= end` — so the streaming UI never sees an orphan or
//!      duplicate block mid-stream.
//!
//! Deterministic (fixed seeds, zero-dep xorshift PRNG) so a failure reproduces.

use std::collections::HashSet;
use flux_md_core::StreamParser;

/// Zero-dep xorshift64 PRNG — deterministic, good enough to shuffle token soup.
struct Rng(u64);
impl Rng {
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// Tokens chosen to exercise every block/inline construct, fence delimiters,
/// nesting starters, GFM features, UTF-8 boundaries, and hard-break whitespace.
const TOKENS: &[&str] = &[
    "*", "**", "_", "~", "~~", "`", "``", "```", "$", "$$", "\\", "\\(", "\\)", "\\[", "\\]",
    "[", "]", "(", ")", "<", ">", "#", "##", "-", "+", "=", "|", "!", "\"", "'", "&", "&amp;",
    ";", ":", "/", "://", " ", "  ", "\t", "\n", "\n\n", "\r\n", "a", "Word", "Z9", "1.", "2)",
    "é", "中", "🚀", "[!NOTE]", "[!warning]", "[^1]", "[^1]:", "](http://x.com)", "www.example.com",
    "http://a.b/c", "foo@bar.example", "> ", "- [ ] ", "- [x] ", "![alt]", "{aligned}",
    "\\begin", "\\end", "&#58;", "javascript:", "<div>", "</div>", "<br/>", "x_n", "E=mc^2",
];

fn random_doc(rng: &mut Rng) -> String {
    let n = 1 + rng.below(140);
    let mut s = String::with_capacity(n * 3);
    for _ in 0..n {
        s.push_str(TOKENS[rng.below(TOKENS.len())]);
    }
    s
}

/// Ordered, non-overlapping, unique ids, start<=end. Panics with the offending
/// block set on violation. Called after every append and at finalize.
fn check_invariants(p: &StreamParser, ctx: &str) {
    let mut last_end = 0usize;
    let mut ids = HashSet::new();
    for b in p.all_blocks() {
        assert!(b.start <= b.end, "start>end ({}, {}) [{ctx}]", b.start, b.end);
        assert!(b.start >= last_end, "overlap/disorder: start {} < prev end {} [{ctx}]", b.start, last_end);
        assert!(ids.insert(b.id), "duplicate block id {} [{ctx}]", b.id);
        last_end = b.end;
    }
}

fn configured(seed_bit: usize) -> StreamParser {
    // Cycle through feature combinations so the fuzz exercises each scanner path.
    StreamParser::new()
        .with_gfm_autolinks(seed_bit & 1 != 0)
        .with_gfm_alerts(seed_bit & 2 != 0)
        .with_gfm_footnotes(seed_bit & 4 != 0)
        .with_gfm_math(seed_bit & 8 != 0)
        .with_dir_auto(seed_bit & 16 != 0)
        .with_unsafe_html(seed_bit & 32 != 0)
}

#[test]
fn random_streaming_never_panics_and_blocks_stay_well_formed() {
    for seed in 1u64..=4000 {
        let mut rng = Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1);
        let doc = random_doc(&mut rng);
        let cfg = (seed as usize) & 63;

        // (a) Streamed in random char-boundary chunks; invariants after each append.
        let mut p = configured(cfg);
        let chars: Vec<char> = doc.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let take = 1 + rng.below(12);
            let chunk: String = chars[i..(i + take).min(chars.len())].iter().collect();
            p.append(&chunk);
            check_invariants(&p, "streaming");
            i += take;
        }
        p.finalize();
        check_invariants(&p, "after finalize (streamed)");

        // (b) One-shot, same input — must also be well-formed and not panic.
        let mut q = configured(cfg);
        q.append(&doc);
        q.finalize();
        check_invariants(&q, "one-shot");
    }
}

#[test]
fn single_byte_chunks_never_panic() {
    // The most demanding cut: one byte at a time (UTF-8-safe by char), every
    // construct half-formed at some prefix. Smaller corpus, all features on.
    for seed in 1u64..=600 {
        let mut rng = Rng(seed.wrapping_mul(0xD1B54A32D192ED03) | 1);
        let doc = random_doc(&mut rng);
        let mut p = configured(63);
        let mut buf = [0u8; 4];
        for ch in doc.chars() {
            p.append(ch.encode_utf8(&mut buf));
            check_invariants(&p, "1-char streaming");
        }
        p.finalize();
        check_invariants(&p, "1-char finalize");
    }
}

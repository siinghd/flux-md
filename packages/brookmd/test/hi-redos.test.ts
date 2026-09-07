import { test, expect } from "bun:test";
import { highlight, supportedLangs } from "../src/hi";

// Regression: the JS/TS `rx` and bash `str` patterns used to backtrack
// quadratically on adversarial input (a `/` followed by thousands of `[`, or a
// `"` followed by many `$(`). With the linearized patterns these tokenize in
// (sub-)linear time. We assert a string is returned well under a generous wall
// budget rather than a tight number, to stay deterministic across machines.

test("highlight(js) does not hang on /[[[[… (rx ReDoS)", () => {
  const evil = "/" + "[".repeat(100_000);
  const t0 = performance.now();
  const out = highlight(evil, "js");
  const dt = performance.now() - t0;
  expect(typeof out).toBe("string");
  expect(dt).toBeLessThan(1000);
});

test("highlight(bash) does not hang on \"$($($(… (str ReDoS)", () => {
  const evil = '"' + "$(".repeat(100_000);
  const t0 = performance.now();
  const out = highlight(evil, "bash");
  const dt = performance.now() - t0;
  expect(typeof out).toBe("string");
  expect(dt).toBeLessThan(1000);
});

test("size guard returns escaped plain text for huge blocks", () => {
  const huge = "<".repeat(60_000); // > 50_000
  const out = highlight(huge, "js");
  // Escaped, and no token spans (highlighter never ran on the body).
  expect(out).toBe("&lt;".repeat(60_000));
  expect(out).not.toContain('class="t-');
});

test("still highlights ordinary regex and bash strings", () => {
  // A common regex literal still gets the rx token class.
  expect(highlight("const re = /ab+c/gi;", "js")).toContain('class="t-rx"');
  // A bash double-quoted string containing $(...) still tokenizes as one string.
  const bash = highlight('echo "hi $(whoami) there"', "bash");
  expect(bash).toContain('class="t-str"');
});

// The same question asked of EVERY language, built-in or registered: a pattern
// list is only allowed into the table if it degrades linearly. Each input below
// is a long run of the characters that open one of the unbounded or backtracking
// forms — a language whose table nests quantifiers over overlapping classes
// blows its budget on one of them rather than waiting to be found in the wild.
const EVIL: Array<[string, string]> = [
  ["a regex-literal opener", "/" + "[".repeat(12_000)],
  ["a string opener", '"' + "$(".repeat(6_000)],
  ["a block-comment opener", "/*" + "a&<".repeat(4_000)],
  ["a run of quotes", "'".repeat(12_000)],
  ["a run of backticks", "`".repeat(12_000)],
  ["a run of dashes", "-".repeat(12_000)],
  ["a run of pluses", "+".repeat(12_000)],
  ["a run of hashes", "#".repeat(12_000)],
  ["a run of brackets", "[".repeat(12_000)],
  ["a run of at-signs", "@".repeat(12_000)],
  ["a dotted word run", "a.".repeat(6_000)],
  ["a colon run", "a:".repeat(6_000)],
  ["an equals run", "a = ".repeat(3_000)],
  ["an interpolation run", "${".repeat(6_000)],
  ["a digit run", "1".repeat(12_000) + "a"],
  // A hex run ending in a word char that is not a valid suffix: the shape that
  // made an overlapping numeric-suffix class backtrack quadratically (0.30.0).
  ["a hex run", "0x" + "d".repeat(12_000) + "g"],
  ["a hex run with a float suffix", "0x" + "f".repeat(12_000) + "L"],
  ["a line of soup", "a<b&\"'`/*#$@-+=".repeat(800)],
  ["lines of soup", "a<b&\"'`/*#$@-+=\n".repeat(750)],
];

test("every supported language tokenizes adversarial input inside the budget", () => {
  for (const lang of supportedLangs()) {
    for (const [what, evil] of EVIL) {
      const t0 = performance.now();
      const out = highlight(evil, lang);
      const dt = performance.now() - t0;
      expect(typeof out).toBe("string");
      // Generous against a linear pass over 12 KB (single-digit milliseconds) and
      // still orders of magnitude under what one nested quantifier would cost.
      if (dt >= 400) throw new Error(`highlight(${lang}) took ${dt.toFixed(0)}ms on ${what}`);
    }
  }
}, 30_000);

// Wall-clock budgets are generous by design, so a merely-quadratic pattern can
// hide under one at 12 KB. This pins the SHAPE instead: quadrupling the input
// must cost well under 16x (quadratic) — 4x is linear. Retry once on a noisy
// box before believing a failure.
test("every supported language scales linearly on a hex run", () => {
  const shape = (n: number) => "0x" + "d".repeat(n) + "g";
  const time = (lang: string, n: number) => {
    const t0 = performance.now();
    highlight(shape(n), lang);
    return performance.now() - t0;
  };
  for (const lang of supportedLangs()) {
    let ratio = Infinity;
    for (let attempt = 0; attempt < 2 && ratio >= 8; attempt++) {
      const small = Math.max(time(lang, 6_000), 0.05);
      const large = time(lang, 24_000);
      ratio = large / small;
    }
    if (ratio >= 8) throw new Error(`highlight(${lang}) scales ${ratio.toFixed(1)}x on a 4x larger hex run`);
  }
});

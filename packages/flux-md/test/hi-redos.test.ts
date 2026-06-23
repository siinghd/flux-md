import { test, expect } from "bun:test";
import { highlight } from "../src/hi";

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

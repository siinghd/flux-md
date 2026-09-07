import { test, expect } from "bun:test";
import { highlight } from "../src/hi";

/**
 * The counterexamples the incremental checkpoint rule is DERIVED from.
 *
 * hi-inc.ts may only freeze markup that a later append provably cannot rewrite.
 * Every clause of its rule exists because of one of the traces below — a prefix
 * of source tokenizing differently from the same prefix of a longer source. If
 * one of these stops holding (a pattern is retuned, a language table gains an
 * entry), the rule's justification has to be re-derived before the frozen prefix
 * can be trusted again, so they are pinned here rather than left as prose.
 *
 * These assert on `highlight()`'s ONE-SHOT output only: they are statements
 * about the tokenizer, independent of anything incremental.
 */

/** The token spans of `highlight(src, lang)`, as `[class, text]` pairs. */
function spans(src: string, lang: string): Array<[string, string]> {
  const markup = highlight(src, lang);
  const re = /<span class="t-([a-z]+)">([\s\S]*?)<\/span>/g;
  const out: Array<[string, string]> = [];
  for (let m = re.exec(markup); m !== null; m = re.exec(markup)) out.push([m[1], m[2]]);
  return out;
}

/** Everything `highlight` emitted OUTSIDE a token span — the raw/fallback text. */
function unspanned(src: string, lang: string): string {
  return highlight(src, lang).replace(/<span class="t-[a-z]+">[\s\S]*?<\/span>/g, "");
}

// CE-1 — an unterminated string is NOT a growing string token.
// `"` cannot match js's `"(?:\\.|[^"\\\n])*"` without its partner, so it falls
// all the way through to the one-character catch-all and `hello` tokenizes as a
// plain identifier. The closing quote does not EXTEND a token, it REPLACES two.
// This is why the tail can never be frozen, and why the checkpoint rule refuses
// to advance past a live opener.
test("CE-1: an unterminated js string is a fallback char + an ident, not a str", () => {
  const open = highlight('const s = "hello', "js");
  expect(open).not.toContain('class="t-str"');
  // The quote is emitted raw (escaped), and `hello` as a plain identifier.
  expect(open).toContain("&quot;hello");
  expect(spans('const s = "hello', "js")).toEqual([
    ["kw", "const"],
    ["pun", "="],
  ]);

  // One more byte and both become a single `str` span.
  expect(spans('const s = "hello"', "js")).toEqual([
    ["kw", "const"],
    ["pun", "="],
    ["str", "&quot;hello&quot;"],
  ]);
});

// CE-2 — a number backtracks into fragments and re-forms as one token.
// `123.456e` cannot satisfy the trailing `\b`, so the pattern backtracks to
// `123` and the rest degrades to `.`, three fallback digits and an ident. A
// single appended digit makes the whole run one `num`. This is why a checkpoint
// needs settled bytes behind it and cannot sit inside a word run.
for (const lang of ["js", "py"]) {
  test(`CE-2 (${lang}): 123.456e backtracks to num 123 + fragments; a digit re-forms it`, () => {
    expect(spans("123.456e", lang)).toEqual([
      ["num", "123"],
      ["pun", "."],
    ]);
    // The backtracked remainder is emitted raw, one character at a time.
    expect(unspanned("123.456e", lang)).toBe("456e");

    expect(spans("123.456e5", lang)).toEqual([["num", "123.456e5"]]);
    expect(unspanned("123.456e5", lang)).toBe("");
  });
}

// CE-4 — SQL's doubled-quote escaping backtracks out of an even quote run.
// The greedy content pairs `''` all the way to EOF, fails to find a closer, then
// gives one pair back: the literal ends one quote EARLY and a stray quote is
// left over. That stray quote is an opener the next patch may close, which is
// exactly what the liveness check has to notice.
test("CE-4 (sql): 'a'''' is a 5-char str plus a stray quote token", () => {
  const s = spans("'a''''", "sql");
  expect(s).toEqual([["str", "'a'''"]]);
  expect(s[0][1].length).toBe(5);
  // The 6th quote is not part of any span — it is the leftover opener.
  expect(unspanned("'a''''", "sql")).toBe("'");
});

// CE-5 — Rust's character literal matches ACROSS a newline in a 3-byte window.
// `[^'\\]` does not exclude `\n`, so `'`+`\n`+`'` is a `str`. A checkpoint
// placed right after that newline while the closing quote had not yet arrived
// would freeze two tokens the next byte merges into one — which is why the rule
// keeps GAP bytes of settled input behind every checkpoint.
test("CE-5 (rust): a char literal spans a newline in a 3-byte window", () => {
  expect(spans("'\n'", "rust")).toEqual([["str", "'\n'"]]);
  // Two bytes is not enough: it degrades to a fallback quote and raw whitespace.
  expect(highlight("'\n", "rust")).toBe("'\n");
});

// CE-7 — a regex literal one-shots over what looked like two division operators.
// `a / b / c` puts `/ b /` in a single `rx` span; while only `a / b ` had
// arrived, that `/` was a `pun`. Line-bounded (its class excludes `\n`), which
// is what lets the checkpoint sit immediately after a newline without tracking
// it as a live opener.
test("CE-7 (js): a / b / c one-shots into an rx token spanning `/ b /`", () => {
  expect(spans("a / b / c", "js")).toEqual([["rx", "/ b /"]]);
  // Before the second slash lands, the first is only punctuation.
  expect(spans("a / b ", "js")).toEqual([["pun", "/"]]);
  // …and a newline kills it for good: no rx can cross one.
  expect(spans("a / b \n/ c", "js")).toEqual([
    ["pun", "/"],
    ["pun", "/"],
  ]);
});

// CE-8 — HTML has no `ws` pattern: whitespace rides inside a `txt` span that
// crosses newlines. Cutting there would split `<span …>` down the middle, so
// html/xml checkpoint after a `>` instead.
test("CE-8 (html): txt spans newlines and emits a span, so `\\n` is not a cut point", () => {
  expect(spans("<p>hello\nworld\nmore", "html")).toEqual([
    ["tag", "&lt;p"],
    ["pun", "&gt;"],
    ["txt", "hello\nworld\nmore"],
  ]);
});

// CE-9 — diff's context-line catch-all must not be able to start on whitespace.
// The checkpoint rule cuts a `ws` run in two just after a newline and re-matches
// `\s+` from the cut, which is only byte-safe while `ws` is the pattern that WINS
// on whitespace. `[^\n]+` would swallow a line's indentation, so it is ordered
// after `ws` — a run that stopped at the cut and one that never stopped have to
// agree on where the token starts.
test("CE-9 (diff): a context line's indentation is raw whitespace, not part of txt", () => {
  expect(spans("@@ -1 +1 @@\n  ctx\n-gone\n+new\n", "diff")).toEqual([
    ["com", "@@ -1 +1 @@"],
    ["txt", "ctx"],
    ["kw", "-gone"],
    ["str", "+new"],
  ]);
  // The two spaces are emitted raw, outside every span.
  expect(unspanned("@@ -1 +1 @@\n  ctx\n", "diff")).toBe("\n  \n");
});

// CE-10 — a yaml key is decided by a TWO-character lookahead, and one more byte
// can take it away: `key:` at EOF is a key, `key:x` is not. Bounded on purpose —
// the frozen prefix reserves GAP bytes behind every checkpoint, so a token may
// only ever consult that far past its own end.
test("CE-10 (yaml): the key lookahead reaches exactly one byte past the colon", () => {
  const pun: [string, string] = ["pun", ":"];
  expect(spans("key: v\n", "yaml")).toEqual([["attr", "key"], pun]);
  expect(spans("key:", "yaml")).toEqual([["attr", "key"], pun]); // the `$` branch
  expect(spans("key:x", "yaml")).toEqual([pun]); // …and one byte later it is gone
  expect(spans("key", "yaml")).toEqual([]);
});

// CE-11 — the C preprocessor line is `#[ \t]*`, never `#\s*`. A `\s` there could
// match a newline, which would make `#include` an unbounded form that the
// incremental opener table does not list.
test("CE-11 (c): a preprocessor directive cannot span a newline", () => {
  expect(spans("#include <stdio.h>\n", "c")).toEqual([
    ["mac", "#include"],
    ["pun", "&lt;"],
    ["pun", "."],
    ["pun", "&gt;"],
  ]);
  expect(highlight("#\n\ninclude\n", "c")).not.toContain('class="t-mac"');
});

// CE-12 — toml table headers are anchored at the start of a line, so an
// arbitrary `[…]` in a value stays punctuation and cannot be rewritten into a
// header by anything a later byte does.
test("CE-12 (toml): only a line-initial [x] is a table header", () => {
  expect(spans("[a.b]\nk = [1, 2]\n", "toml")).toEqual([
    ["sel", "[a.b]"],
    ["attr", "k"],
    ["pun", "="],
    ["pun", "["],
    ["num", "1"],
    ["pun", ","],
    ["num", "2"],
    ["pun", "]"],
  ]);
});

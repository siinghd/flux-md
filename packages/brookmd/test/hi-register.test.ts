import { test, expect } from "bun:test";
import {
  highlight,
  registerLanguage,
  stepHighlight,
  supportedLangs,
  type HighlightState,
  type LanguageDef,
} from "../src/hi";
import { createInc, incHighlight, incSeed } from "../src/hi-inc";

/**
 * `registerLanguage()` — the caller-supplied language table — and a golden-ish
 * pass over the languages that ship in the table alongside the originals.
 *
 * A registered language has to clear the same bar a built-in one does: it
 * tokenizes through the same resumable core, so a chunked run must be
 * byte-identical to a one-shot run, and a streamed block must settle on exactly
 * the markup a one-shot `highlight()` produces. What it does NOT get is a frozen
 * prefix — the checkpoint rule is derived from knowing which of a table's forms
 * can run past a newline, and a caller's table does not say — so the streaming
 * state stays at `c === 0` and every patch re-tokenizes. That is asserted here
 * too, because "never freezes" is the property that makes an unknown table safe.
 */

// A small ini-ish language, used for every registration assertion below. It is
// deliberately line-bounded and linear: registering leaks into the module
// registry the rest of the suite shares, and hi-inc.test.ts fuzzes every
// language in supportedLangs() against every other language's corpus.
const KV_PATS: Array<[string, RegExp]> = [
  ["com", /;[^\n]*/y],
  ["sel", /^\[[^\]\n]*\]/my],
  ["str", /"(?:\\.|[^"\\\n])*"/y],
  ["num", /\b\d+(?:\.\d+)?\b/y],
  ["ident", /[A-Za-z_][\w-]*/y],
  ["pun", /[=,.:]/y],
  ["ws", /\s+/y],
];

const KV_DEF: LanguageDef = { pats: KV_PATS, kw: ["on", "off", "yes", "no"] };

const KV_SRC = '; a & comment\n[server]\nhost = "a<b"\nport = 8080\ndebug = on\n';

/** Run the resumable core to completion in `chars`-sized slices. */
function chunked(code: string, lang: string, chars: number): string {
  const state: HighlightState = { pos: 0, out: "" };
  while (!stepHighlight(code, lang, state, chars)) {
    /* every slice consumes at least one whole token */
  }
  return state.out;
}

/** Finish a (possibly seeded) run with no budget — the settled markup. */
function settle(text: string, lang: string, seed?: HighlightState): string {
  const state: HighlightState = seed ? { pos: seed.pos, out: seed.out } : { pos: 0, out: "" };
  while (!stepHighlight(text, lang, state, text.length)) {
    /* a budget that big always finishes */
  }
  return state.out;
}

// ---------------------------------------------------------------------------
// registerLanguage
// ---------------------------------------------------------------------------

test("a registered language tokenizes and joins supportedLangs()", () => {
  registerLanguage(["brookIni", "bIni"], KV_DEF);

  // Names are lower-cased on the way in, and looked up case-insensitively.
  expect(supportedLangs()).toContain("brookini");
  expect(supportedLangs()).toContain("bini");
  const out = highlight(KV_SRC, "brookini");
  expect(highlight(KV_SRC, "BrookIni")).toBe(out);
  expect(highlight(KV_SRC, "bini")).toBe(out); // the alias shares the table

  for (const cls of ["com", "sel", "str", "num", "kw"]) {
    expect(out).toContain(`class="t-${cls}"`);
  }
  // `kw` came from the keyword set, through the `ident` class.
  expect(out).toContain('<span class="t-kw">on</span>');
  // …and the escaper still runs over a registered language's tokens.
  expect(out).toContain('<span class="t-str">&quot;a&lt;b&quot;</span>');
});

test("a registered language is chunk-resumable, byte for byte", () => {
  registerLanguage("brookini", KV_DEF);
  const oneShot = highlight(KV_SRC, "brookini");
  for (const chars of [1, 2, 3, 7, 64, 4096, KV_SRC.length * 4]) {
    expect(chunked(KV_SRC, "brookini", chars)).toBe(oneShot);
  }
});

test("a registered language streams: it settles identically and freezes nothing", () => {
  registerLanguage("brookini", KV_DEF);
  const st = createInc("brookini");
  expect(st).not.toBeNull();
  expect(st!.freeze).toBe(false);

  for (let i = 1; i <= KV_SRC.length; i += 3) {
    const fed = KV_SRC.slice(0, i);
    const shown = incHighlight(st!, fed);
    // Mid-stream markup is a full re-tokenization of what has arrived…
    expect(shown).toBe(highlight(fed, "brookini"));
    // …and nothing is ever frozen, which is what makes an unknown table safe.
    expect(st!.c).toBe(0);
    expect(st!.frozenHtml).toBe("");
  }
  incHighlight(st!, KV_SRC);
  expect(incSeed(st!, KV_SRC, "brookini")).toBeUndefined();
  expect(settle(KV_SRC, "brookini", incSeed(st!, KV_SRC, "brookini"))).toBe(
    highlight(KV_SRC, "brookini"),
  );
});

test("registering an existing name replaces its table", () => {
  // Only ever a name this suite owns: a registration is global, and clobbering
  // a built-in would hand the rest of the suite a different `yaml`.
  registerLanguage("brookswap", { pats: [["kw", /[A-Za-z]+/y], ["ws", /\s+/y]] });
  expect(highlight("alpha beta", "brookswap")).toBe(
    '<span class="t-kw">alpha</span> <span class="t-kw">beta</span>',
  );

  registerLanguage("brookswap", { pats: [["num", /[A-Za-z]+/y], ["ws", /\s+/y]] });
  expect(highlight("alpha beta", "brookswap")).toBe(
    '<span class="t-num">alpha</span> <span class="t-num">beta</span>',
  );
  // Replaced, not duplicated.
  expect(supportedLangs().filter((l) => l === "brookswap")).toEqual(["brookswap"]);
});

test("the pattern array is copied, so a later mutation cannot rewrite markup", () => {
  const pats: Array<[string, RegExp]> = [["kw", /[A-Za-z]+/y], ["ws", /\s+/y]];
  registerLanguage("brookfrozen", { pats });
  pats.length = 0;
  pats.push(["num", /[A-Za-z]+/y]);
  expect(highlight("alpha", "brookfrozen")).toBe('<span class="t-kw">alpha</span>');
});

test("a name nobody registered is still the plain-escape fallback", () => {
  expect(highlight("a <b> & c", "brooknope")).toBe("a &lt;b&gt; &amp; c");
  expect(createInc("brooknope")).toBeNull();
  // Prototype keys are ordinary misses, not `Object.prototype` members.
  for (const key of ["toString", "constructor", "__proto__", "hasOwnProperty"]) {
    expect(highlight("a <b>", key)).toBe("a &lt;b&gt;");
    expect(createInc(key)).toBeNull();
  }
});

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

test("a non-sticky regex is refused, naming the pattern index", () => {
  expect(() =>
    registerLanguage("brookbad", { pats: [["com", /;[^\n]*/y], ["str", /"[^"]*"/]] }),
  ).toThrow(/pattern 1 \(str\).*sticky/s);
  expect(() => registerLanguage("brookbad", { pats: [["ws", /\s+/g]] })).toThrow(TypeError);
  expect(supportedLangs()).not.toContain("brookbad");
});

test("an unknown token class is refused, naming the pattern index", () => {
  expect(() =>
    registerLanguage("brookbad2", { pats: [["ws", /\s+/y], ["sparkle", /[a-z]+/y]] }),
  ).toThrow(/pattern 1 has unknown token class "sparkle"/);
  // A class with no style is exactly as wrong as a typo.
  expect(() => registerLanguage("brookbad2", { pats: [["keyword", /[a-z]+/y]] })).toThrow(
    TypeError,
  );
  expect(supportedLangs()).not.toContain("brookbad2");
});

test("a malformed definition or name is refused", () => {
  const ok: LanguageDef = { pats: [["ws", /\s+/y]] };
  expect(() => registerLanguage("brookbad3", { pats: [] })).toThrow(/non-empty array/);
  expect(() => registerLanguage([], ok)).toThrow(/non-empty array of names/);
  expect(() => registerLanguage("", ok)).toThrow(/non-empty string/);
  expect(() =>
    registerLanguage("brookbad3", { pats: [["ws"] as unknown as [string, RegExp]] }),
  ).toThrow(/pattern 0 must be a \[token, regexp\] pair/);
  expect(() =>
    registerLanguage("brookbad3", {
      pats: [["ws", "\\s+" as unknown as RegExp]],
    }),
  ).toThrow(/sticky/);
  expect(supportedLangs()).not.toContain("brookbad3");
});

// ---------------------------------------------------------------------------
// The built-in languages this table gained alongside the originals
// ---------------------------------------------------------------------------

/** `[language, snippet, the token classes the snippet must light up]`. */
const GOLDENS: Array<[string, string, string[]]> = [
  [
    "yaml",
    '# a & comment\nname: brookmd\nversion: 1.5\nenabled: true\nquoted: "a<b"\nlist:\n  - one\n',
    ["com", "attr", "num", "lt", "str"],
  ],
  ["yml", '# c\nkey: "v"\nn: 2\nok: false\n', ["com", "attr", "str", "num", "lt"]],
  [
    "toml",
    '# a & comment\n[server]\nhost = "localhost"\nport = 8080\nok = true\n',
    ["com", "sel", "attr", "str", "num", "lt"],
  ],
  [
    "diff",
    "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n context\n-gone\n+added\n",
    ["com", "kw", "str", "txt"],
  ],
  [
    "java",
    '// a & comment\npublic class Foo {\n  /* block */\n  private String s = "a<b";\n  int n = 0x1f;\n}\n',
    ["com", "kw", "str", "num", "ty"],
  ],
  [
    "c",
    '#include <stdio.h>\n// a & comment\nint main(void) {\n  const char *s = "a<b";\n  return 0;\n}\n',
    ["mac", "com", "kw", "str", "num"],
  ],
  [
    "cpp",
    '#include <vector>\n// c\nnamespace ns { const char *s = "a"; int n = 42; }\n',
    ["mac", "com", "kw", "str", "num"],
  ],
  ["c++", '// c\nauto f = 1.5;\nconst char *s = "a<b";\n', ["com", "kw", "str", "num"]],
  [
    "cs",
    '// a & comment\npublic class Svc {\n  /* block */\n  private const int N = 42;\n  public string Name = "a<b";\n}\n',
    ["com", "kw", "str", "num"],
  ],
  ["csharp", '// c\nvar xs = 3;\nstring s = "a";\n', ["com", "kw", "str", "num"]],
  [
    "swift",
    '// a & comment\n@objc final class Store {\n  let limit: Int = 0x20\n  var name = "a<b"\n}\n',
    ["com", "dec", "kw", "str", "num"],
  ],
  [
    "kt",
    '// a & comment\ndata class Point(val x: Int = 0) {\n  /* block */\n  fun name(): String = "a<b"\n}\n',
    ["com", "kw", "str", "num"],
  ],
  ["kotlin", '// c\nval n = 42\nfun f(): String = "a"\n', ["com", "kw", "str", "num"]],
  [
    "php",
    "<?php\n// a & comment\n# hash\n$x = 'a';\nfunction f(): string { return \"a<b\"; }\n$n = 42;\n",
    ["tag", "com", "var", "kw", "str", "num"],
  ],
  [
    "rb",
    '# a & comment\nclass Point < Base\n  attr_reader :x\n  def run(v = 1.5)\n    @x = "a<b"\n  end\nend\n',
    ["com", "kw", "lt", "var", "str", "num"],
  ],
  [
    "ruby",
    '# c\nif n > 1\n  s = "a & b"\nend\nsym = :name\nn = 42\n',
    ["com", "kw", "str", "num", "lt"],
  ],
  [
    "dockerfile",
    '# a & comment\nFROM node:20-alpine AS build\nENV PORT=3000\nCMD ["node", "x.js"]\n',
    ["com", "kw", "num", "str"],
  ],
];

test("every language added to the table lights up its keyword/string/comment/number", () => {
  for (const [lang, code, want] of GOLDENS) {
    const out = highlight(code, lang);
    for (const cls of want) {
      if (!out.includes(`class="t-${cls}"`)) {
        throw new Error(`highlight(${lang}) emitted no t-${cls} span:\n${out}`);
      }
    }
    // Never a class the stylesheet has no colour for.
    const emitted = new Set([...out.matchAll(/class="t-([a-z]+)"/g)].map((m) => m[1]));
    const known = new Set(
      "kw str rx num lt com fn ty mac dec attr sel tag var pun txt".split(" "),
    );
    expect([...emitted].filter((c) => !known.has(c))).toEqual([]);
  }
});

test("the new languages hold the hand-checked span shapes", () => {
  // A diff's line classes: additions `str`, deletions `kw`, hunks and file
  // headers `com`, context `txt`.
  expect(highlight("@@ -1 +1 @@\n-a\n+b\n", "diff")).toBe(
    '<span class="t-com">@@ -1 +1 @@</span>\n' +
      '<span class="t-kw">-a</span>\n' +
      '<span class="t-str">+b</span>\n',
  );
  // A yaml key is `attr`, its scalar is plain.
  expect(highlight("a: b\n", "yaml")).toBe(
    '<span class="t-attr">a</span><span class="t-pun">:</span> b\n',
  );
  // A toml table header is `sel`, its key `attr`.
  expect(highlight("[t]\nk = 1\n", "toml")).toBe(
    '<span class="t-sel">[t]</span>\n' +
      '<span class="t-attr">k</span> <span class="t-pun">=</span> ' +
      '<span class="t-num">1</span>\n',
  );
  // A preprocessor directive is `mac`; a Dockerfile instruction is `kw`.
  expect(highlight("#include", "c")).toBe('<span class="t-mac">#include</span>');
  expect(highlight("FROM x", "dockerfile")).toBe('<span class="t-kw">FROM</span> x');
  // …and the lower-case spelling is not an instruction.
  expect(highlight("from x", "dockerfile")).not.toContain('class="t-kw"');
});

// ---------------------------------------------------------------------------
// Regressions from the 0.30.0 review.

import { __resetLanguages, isRegisteredLang } from "../src/hi";

test("replacing a built-in drops its frozen-prefix mode (the old opener table is wrong for the new patterns)", () => {
  try {
    // A `json` that knows block comments — the built-in json opener table has
    // no comment form, so freezing on it would pin a half-typed `/* … */` as
    // plain text for the rest of the stream.
    registerLanguage("json", {
      pats: [
        ["com", /\/\*[\s\S]*?\*\//y],
        ["str", /"(?:\\.|[^"\\\n])*"/y],
        ["num", /-?\b\d+(?:\.\d+)?\b/y],
        ["lt", /\b(?:true|false|null)\b/y],
        ["pun", /[{}[\]:,]/y],
        ["ws", /\s+/y],
      ],
    });
    expect(isRegisteredLang("json")).toBe(true);
    const st = createInc("json");
    expect(st).not.toBeNull();
    expect(st!.freeze).toBe(false);

    const src = '{"a": 1,\n/* a comment that\nspans lines */\n"b": [true, null]}\n';
    for (let i = 1; i <= src.length; i += 2) {
      const fed = src.slice(0, i);
      expect(incHighlight(st!, fed)).toBe(highlight(fed, "json"));
      expect(st!.c).toBe(0);
    }
    expect(highlight(src, "json")).toContain('class="t-com"');
  } finally {
    __resetLanguages();
  }
  expect(isRegisteredLang("json")).toBe(false);
  expect(createInc("json")!.freeze).toBe(true);
});

test("a pattern that matches the empty string is refused", () => {
  expect(() =>
    registerLanguage("brookempty", { pats: [["ident", /\w*/y], ["ws", /\s+/y]] }),
  ).toThrow(/pattern 0 \(ident\) matches the empty string/);
  expect(createInc("brookempty")).toBeNull();
});

test("a context-dependent zero-width match cannot stall the tokenizer", () => {
  try {
    // Passes registration (no match on ""), but matches empty before any `x`.
    registerLanguage("brookzw", { pats: [["kw", /(?=x)/y], ["ident", /\w+/y], ["ws", /\s+/y]] });
    const out = highlight("axb xx", "brookzw");
    expect(out).toBe("axb xx"); // terminates; plain idents and raw whitespace
    expect(out).toContain("axb");
  } finally {
    __resetLanguages();
  }
});

test("no built-in pattern can match the empty string", () => {
  for (const lang of supportedLangs()) {
    const st: HighlightState = { pos: 0, out: "" };
    // A zero-width match would leave pos at 0 and spin; one step must consume.
    stepHighlight("x", lang, st, 1);
    expect(st.pos).toBe(1);
  }
});

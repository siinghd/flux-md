import { test, expect } from "bun:test";
import { highlight, stepHighlight, supportedLangs, type HighlightState } from "../src/hi";
import {
  createInc,
  incHighlight,
  incSeed,
  __getIncScanned,
  __resetIncScanned,
  type IncState,
} from "../src/hi-inc";

/**
 * THE parity proof for incremental streaming highlighting.
 *
 * hi-inc.ts freezes markup for a prefix of an OPEN block and never revisits it,
 * betting that no later byte can rewrite what is behind the checkpoint. This
 * suite is that bet's proof: every language, every unbounded construct, streamed
 * in at randomized chunk sizes, must
 *
 * 1. **settle byte-identically** — the close-time seeded run over `[c, EOF)`
 *    emits exactly `highlight(full, lang)`, and
 * 2. **never revise a frozen byte** — every `frozenHtml` observed at any point
 *    during the stream is a byte-PREFIX of that final markup.
 *
 * (2) is the stronger claim and the one that catches a checkpoint placed
 * somewhere a later byte can still reach.
 *
 * hi-stability.test.ts pins the tokenizer traces the checkpoint rule is derived
 * from; hi-chunked.test.ts proves the same tokenizer is resumable at all.
 */

/** Finish a (possibly seeded) run with no time budget — the settled markup. */
function settle(text: string, lang: string, seed?: HighlightState): string {
  const state: HighlightState = seed ? { pos: seed.pos, out: seed.out } : { pos: 0, out: "" };
  while (!stepHighlight(text, lang, state, text.length)) {
    /* a budget that big always finishes */
  }
  return state.out;
}

/** Deterministic xorshift — a seeded fuzz has to be reproducible from its seed. */
function rng(seed: number): () => number {
  let s = seed >>> 0 || 1;
  return () => {
    s ^= s << 13;
    s >>>= 0;
    s ^= s >> 17;
    s ^= s << 5;
    s >>>= 0;
    return s / 4294967296;
  };
}

/**
 * Stream `src` in at random chunk sizes and assert both invariants. Returns the
 * state so a caller can inspect what got frozen.
 */
function streamAndCheck(lang: string, src: string, seed: number, maxChunk: number): IncState {
  const st = createInc(lang);
  expect(st).not.toBeNull();
  const rand = rng(seed);
  const frozen: string[] = [];
  let i = 0;
  while (i < src.length) {
    i = Math.min(src.length, i + 1 + Math.floor(rand() * maxChunk));
    incHighlight(st!, src.slice(0, i));
    frozen.push(st!.frozenHtml);
  }
  const want = highlight(src, lang);
  // (1) The close-time run resumes from the frozen prefix and lands on the byte.
  expect(settle(src, lang, incSeed(st!, src, lang))).toBe(want);
  // (2) Nothing frozen at any point was ever wrong.
  for (const f of frozen) {
    if (!want.startsWith(f)) {
      // Report the divergence rather than a 40 KB diff.
      let k = 0;
      while (k < Math.min(f.length, want.length) && f[k] === want[k]) k++;
      throw new Error(
        `frozen prefix diverges at ${k} (lang=${lang} seed=${seed} maxChunk=${maxChunk})\n` +
          `  src : ${JSON.stringify(src)}\n` +
          `  got : ${JSON.stringify(f.slice(Math.max(0, k - 40), k + 60))}\n` +
          `  want: ${JSON.stringify(want.slice(Math.max(0, k - 40), k + 60))}`,
      );
    }
  }
  return st!;
}

// One representative snippet per language family, plus — for every language —
// the unbounded forms whose terminator may never arrive. These are the shapes
// the checkpoint rule exists for: the block comment, the template literal, the
// Python triple-quote, the multi-line Rust string and `#[…]`, the SQL literal
// with doubled quotes, the HTML comment and multi-line attribute, the Go raw
// string, the Bash string and `${…}`, and an unterminated variant of each.
const CORPUS: Record<string, string[]> = {
  js: [
    `// head <&>\nimport { readFile } from "node:fs";\nconst re = /ab+c/gi, s = 'x\\'y', t = \`a\${b}"c"\`;\nexport async function main(argv) {\n  /* block & comment */\n  let n = 0x1f_ff + 0b1010 + 1.25e3;\n  if (n < 10 && n > 2) return new Map([[1, "a<b"]]);\n  return await Foo.run(argv ?? []);\n}\n`,
    'const s = "hello\nconst t = \'unterminated\nlet u = `tpl\nmore\nlines` ;\n',
    "/* never closed\nstill going\nand going\n",
    "const x = 123.456e\nconst y = 1e+\na / b / c\n",
    "const t = `a\\\nb`;\nconst q = 1;\n", // backslash-newline inside a template
  ],
  ts: [
    'export interface User { id: number; name: string; tags?: readonly string[] }\ntype Maybe<T> = T | null;\nconst q = "a & b <c> \\"d\\"";\n',
  ],
  rust: [
    `//! crate doc\nuse std::collections::HashMap;\n#[derive(Debug, Clone)]\npub struct Cfg<'a> { pub name: &'a str, n: u32, f: f64 }\nimpl<'a> Cfg<'a> {\n    pub fn new(name: &'a str) -> Self {\n        m.insert("a<b", b'x');\n        println!("{} & {}", name, 0xff_u32);\n    }\n}\n`,
    'let s = "multi\nline\nstring";\nlet t = \'\n\';\n',
    "#[attr(\nmultiline\n)]\nfn f() {}\n",
    "let a = '\n", // CE-5, cut one byte short
    'let s = "never closed\nnext line\n',
  ],
  py: [
    `import os\n\n@dataclass\nclass P:\n    """A <point> & such.\n    more\n    """\n    x: int = 0\n\n    def n(self, k: float = 1.5):\n        s = f'{self.x!r}' + "a<b" + '''tri\nple'''\n        return None\n`,
    's = f"""abc\ndef\n', // the `f""` / `f"""` ambiguity, unterminated
    's = """unterminated\nnext\n',
  ],
  go: [
    'package main\n\nimport "fmt"\n\nfunc main() {\n\t// go <&>\n\ts := `raw\nmulti\nline`\n\tfmt.Printf("%q\\n", s)\n}\n',
    "s := `never closed\nmore\n",
    "/* block\ncomment\n",
  ],
  bash: [
    `#!/usr/bin/env bash\nset -euo pipefail\nNAME="\${1:-world}"\nfor f in *.txt; do\n  echo "hi $NAME <$f> & done" > "$f.out"\ndone\n`,
    "X=\"multi\nline\nstring\"\nY='single\nquoted'\n",
    'X="never closed\nnext\n',
    "X=${unterminated\nnext\n",
    'X="a\\\nb"\nY=1\n',
  ],
  json: [
    `{\n  "name": "brookmd",\n  "n": -12.5e3,\n  "ok": true,\n  "nil": null,\n  "list": [1, 2, {"a": "b<c>&"}]\n}\n`,
    '{\n "a": "unterminated\n',
  ],
  sql: [
    `-- pick users\n/* multi\n   line */\nSELECT u.id, COUNT(*) AS n\nFROM users u\nWHERE u.name LIKE 'a''b%'\nGROUP BY u.id;\n`,
    "SELECT 'a''''\nFROM t\n", // CE-4
    "SELECT 'multi\nline\nliteral' FROM t;\n",
    'SELECT "quoted""ident" FROM t;\n',
    "SELECT 'never closed\nnext\n",
  ],
  html: [
    `<!-- page & such -->\n<div class="wrap" data-n='1'>\n  <p>Text &amp; more &lt;here&gt;</p>\n  <img src="a.png" alt="a > b"/>\n</div>\n`,
    "<!-- unterminated\nmore\n",
    '<div class="multi\nline\nattr">x</div>\n',
    "<p>plain text\nacross lines\nno tags\n", // CE-8: one txt span, no `>` after
  ],
  css: [
    `/* theme & vars */\n:root { --gap: 8px; }\n.brook-md > pre[data-lang="ts"]::after {\n  content: "a<b";\n  margin: 0 auto -1.5rem;\n}\n`,
    "/* unterminated\nmore\n",
    'a { content: "multi\nline"; }\n',
  ],
  java: [
    "// head <&>\npublic class Foo extends Bar {\n  /* block & comment */\n  private final String s = \"a<b\";\n  int n = 0x1f_ff;\n  char c = 'x';\n}\n",
    "/* never closed\nstill going\n",
    "String s = \"unterminated\nint n = 1;\n",
  ],
  c: [
    "#include <stdio.h>\n#define WIDE(x) ((x) * 2)\n// c <&>\nint main(void) {\n  /* block */\n  return 0;\n}\n",
    "/* open\nmore\n",
    "#\n\ninclude <stdio.h>\n",
  ],
  php: [
    "<?php\n// c\n$x = 'a';\n/* block\nspanning\n*/\necho \"<b>$x</b>\";\n?>\n",
    "<?php /* never closed\nmore\n",
  ],
  rb: [
    "# c <&>\nclass A < B\n  attr_reader :x\n  def b!(v = 1.5)\n    @c = :sym\n    \"a<b\"\n  end\nend\n",
    "s = \"unterminated\nt = 1\n",
  ],
  swift: ["@objc final class A {\n  let n: Int = 0x1f\n  /* open\n  and open\n"],
  yaml: [
    "# c <&>\nkey: value\nnum: 1.5\nok: true\nq: \"a & b\"\ns: 'it''s'\nlist:\n  - one\n  - two\nref: *base\n",
    "key: \"unterminated\nnext: 1\n",
    "key\n",
  ],
  toml: [
    "# c <&>\n[a.b]\nkey = \"v\"\nn = 1.5\nok = true\n\n[[c]]\nd = 'e'\n",
    "key = \"unterminated\nnext = 1\n",
  ],
  diff: [
    "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,3 @@\n ctx & <more>\n-gone\n+new\n",
    "  indented context\n-  removed\n",
  ],
  dockerfile: [
    "# c <&>\nFROM alpine:3 AS b\nENV X=\"y\"\nRUN a && b\nCMD [\"x\"]\n",
  ],
};

test("every language in supportedLangs() has an incremental table", () => {
  for (const lang of supportedLangs()) {
    expect(createInc(lang)).not.toBeNull();
  }
  // An unknown language has no token boundaries to checkpoint on and opts out.
  expect(createInc("cobol")).toBeNull();
  expect(createInc("")).toBeNull();
  // Case-insensitive, like the tokenizer's own LANGS lookup.
  expect(createInc("TypeScript")).not.toBeNull();
});

test("append-fuzz: the corpus settles byte-identically and never revises a frozen byte", () => {
  let cases = 0;
  for (const [lang, sources] of Object.entries(CORPUS)) {
    for (const src of sources) {
      for (let seed = 1; seed <= 8; seed++) {
        for (const maxChunk of [1, 3, 7, 17, 64]) {
          streamAndCheck(lang, src, seed, maxChunk);
          cases++;
        }
      }
    }
  }
  expect(cases).toBeGreaterThan(1000);
});

test("append-fuzz: every language against every other language's corpus", () => {
  const sources = Object.values(CORPUS).flat();
  for (const lang of supportedLangs()) {
    for (let i = 0; i < sources.length; i++) {
      streamAndCheck(lang, sources[i], i + 1, 5);
    }
  }
});

// Character soups weighted toward the bytes that drive every opener, every
// backtrack and every fallback — the shapes a hand-written corpus never reaches.
const SOUPS: Record<string, string> = {
  quotes: `"'\`\n\\ `,
  slashes: `/*-<>!\n `,
  numbers: `0123456789.eE+-_\n `,
  pyPrefixes: `fFrRbB"'\n \\`,
  sqlQuotes: `'"\n abc`,
  htmlish: `<>"'=/-! \nabcp`,
  mixed: `abc {}[]()<>"'\`\\/*-#$@!;:,.\n\t 019eE_&fFrRbB`,
};

test("append-fuzz: adversarial character soup, every language", () => {
  let cases = 0;
  for (const lang of supportedLangs()) {
    for (const [name, alphabet] of Object.entries(SOUPS)) {
      for (let seed = 1; seed <= 25; seed++) {
        const rand = rng(seed * 104729 + lang.length * 31 + name.length);
        const n = 5 + Math.floor(rand() * 200);
        let src = "";
        for (let k = 0; k < n; k++) src += alphabet[Math.floor(rand() * alphabet.length)];
        streamAndCheck(lang, src, seed, 1 + Math.floor(rand() * 20));
        cases++;
      }
    }
  }
  expect(cases).toBeGreaterThan(2000);
});

// Real token shapes glued at random, so multi-line constructs and the
// terminators that close them actually co-occur.
const FRAGMENTS = [
  "\n", "\n", "\n", " ", "  ", "\t", "\n  ", "\n\n",
  "/*", "*/", "//", "*", "/", "-->", "<!--", "--", "-",
  '"', "'", "`", '"""', "'''", "''", '""', "``",
  "\\", "\\n", "\\\\", '\\"', "\\'", "\\`",
  "abc", "Foo", "f", "rb", "b", "e", "E", "_x",
  "123", "1.5", "0x1f", "1e", "1e5", ".", "+", "-",
  "${", "}", "$foo", "#[", "#![", "]", "#", "@dec",
  "<div", "<p", ">", "=", "/>", "</p", "class", "&",
  "(", ")", "{", "}", "[", "]", ";", ":", ",", "!",
  "fn", "let", "const", "SELECT", "FROM", "def", "func",
];

test("append-fuzz: random token fragments, every language", () => {
  for (const lang of supportedLangs()) {
    for (let seed = 1; seed <= 90; seed++) {
      const rand = rng(seed * 2654435761 + lang.length * 97);
      const n = 4 + Math.floor(rand() * 80);
      let src = "";
      for (let k = 0; k < n; k++) src += FRAGMENTS[Math.floor(rand() * FRAGMENTS.length)];
      streamAndCheck(lang, src, seed, 1 + Math.floor(rand() * 24));
    }
  }
});

test("the frozen prefix really does advance (the fuzz is not passing on an empty one)", () => {
  const src = CORPUS.js[0];
  const st = streamAndCheck("js", src, 1, 4);
  expect(st.c).toBeGreaterThan(src.length / 2);
  expect(st.frozenHtml.length).toBeGreaterThan(0);
  expect(highlight(src, "js").startsWith(st.frozenHtml)).toBe(true);
  // …and it stops GAP bytes short of EOF, never at it.
  expect(st.c).toBeLessThanOrEqual(src.length - 3);
});

test("html/xml checkpoint after `>`, not inside the newline-spanning txt span", () => {
  const src = '<div class="a">\n  <p>one</p>\n  <p>two</p>\n</div>\n';
  const st = streamAndCheck("html", src, 3, 4);
  expect(st.c).toBeGreaterThan(0);
  // Every checkpoint html takes is immediately after a `>`.
  expect(src[st.c - 1]).toBe(">");
});

test("a live unbounded opener pins the checkpoint and releases it on close", () => {
  const st = createInc("js")!;
  incHighlight(st, "const a = 1;\n/* open\nstill open\n");
  const pinned = st.c;
  expect(st.opener).not.toBeNull();
  incHighlight(st, "const a = 1;\n/* open\nstill open\nand still\n");
  expect(st.c).toBe(pinned); // nothing past the live comment may freeze
  expect(st.opener).not.toBeNull();

  const full = "const a = 1;\n/* open\nstill open\nand still\n*/\nconst b = 2;\nconst c = 3;\n";
  incHighlight(st, full);
  expect(st.opener).toBeNull();
  expect(st.c).toBeGreaterThan(pinned);
  expect(highlight(full, "js").startsWith(st.frozenHtml)).toBe(true);
  expect(settle(full, "js", incSeed(st, full, "js"))).toBe(highlight(full, "js"));
});

test("cap: a 50 KB single line stays plain mid-stream and still settles identically", () => {
  // One unbroken line: there is no newline to checkpoint on, so the unfrozen
  // tail grows past CAP and the tokenizer bows out for the rest of the stream.
  const line = "const x = " + "a".repeat(50_000 - 40) + ";";
  const src = line + "\nconst y = 2;\n";
  expect(src.length).toBeLessThan(50_000);
  const st = createInc("js")!;
  __resetIncScanned();
  let capped = 0;
  let scannedBefore = 0;
  for (let i = 512; i <= src.length; i += 512) {
    const before = __getIncScanned();
    const out = incHighlight(st, src.slice(0, i));
    if (__getIncScanned() === before) capped++; // this patch tokenized nothing
    if (i > 20_000 && scannedBefore === 0) scannedBefore = __getIncScanned();
    // Whatever it returns mid-stream is still the plain escaped body: no spans.
    if (i > 20_000) expect(out).not.toContain('<span class="t-');
  }
  expect(capped).toBeGreaterThan(50); // the cap really did engage, and stayed on
  // Once the cap is on, per-patch tokenizing is zero — flatly O(new bytes).
  expect(__getIncScanned()).toBe(scannedBefore);

  incHighlight(st, src);
  expect(settle(src, "js", incSeed(st, src, "js"))).toBe(highlight(src, "js"));
});

test("cliff: crossing 50 000 chars discards state and matches today's output", () => {
  const src = "const x = 1;\n".repeat(4200); // 54 600 chars
  expect(src.length).toBeGreaterThan(50_000);
  const st = createInc("js")!;
  let out: string | null = null;
  for (let i = 2000; i < 50_000; i += 2000) out = incHighlight(st, src.slice(0, i));
  expect(out).not.toBeNull();
  expect(st.c).toBeGreaterThan(0);

  // The byte that crosses it: state gone, and the caller is told to render plain.
  out = incHighlight(st, src.slice(0, 50_001));
  expect(out).toBeNull();
  expect(st.c).toBe(0);
  expect(st.frozenHtml).toBe("");
  expect(incSeed(st, src, "js")).toBeUndefined();

  incHighlight(st, src);
  // Unseeded, so identical to what the block always produced past the guard.
  expect(settle(src, "js", incSeed(st, src, "js"))).toBe(highlight(src, "js"));
});

/**
 * The core speculatively CLOSES an open fence on every patch, which terminates a
 * partial last line with a newline: `"expo\n"` is followed by `"export fu\n"`.
 * So a streaming block's source is NOT append-only, and a plain
 * `text.startsWith(prev)` guard resets on every single patch — throwing the
 * frozen prefix away and putting the block straight back to O(n²). This is the
 * shape that actually arrives from the parser, so it gets its own test.
 */
function specClose(src: string, upto: number): string {
  const fed = src.slice(0, upto);
  return fed.endsWith("\n") ? fed : fed + "\n";
}

test("speculative closure: a moving trailing newline keeps the frozen prefix", () => {
  const stream = (reps: number) => {
    const src = CORPUS.ts[0].repeat(reps);
    const st = createInc("ts")!;
    __resetIncScanned();
    let backwards = 0;
    let prevC = 0;
    for (let i = 5; i <= src.length; i += 5) {
      incHighlight(st, specClose(src, i));
      if (st.c < prevC) backwards++;
      prevC = st.c;
    }
    incHighlight(st, src);
    expect(settle(src, "ts", incSeed(st, src, "ts"))).toBe(highlight(src, "ts"));
    return { backwards, c: st.c, bytes: src.length, perByte: __getIncScanned() / src.length };
  };

  const small = stream(4);
  const big = stream(16);
  // The checkpoint never goes backwards: the speculative newline is recognized
  // as a tail revision, not as a different document.
  expect(small.backwards).toBe(0);
  expect(big.backwards).toBe(0);
  expect(big.c).toBeGreaterThan(big.bytes * 0.9);
  // And the cost per source byte does not grow with the block — the regression
  // this test exists for turned a flat ~7x into 4x that at four times the size.
  expect(big.perByte).toBeLessThan(small.perByte * 1.1);
  expect(big.perByte).toBeLessThan(9);
});

test("revision-fuzz: a churning tail never leaves a wrong frozen byte behind", () => {
  // The strongest form of the invariant: at EVERY step, whatever is frozen must
  // be a prefix of the markup for the text as it stood AT THAT STEP — even when
  // the previous patch's tail was rewritten rather than extended.
  const alphabets = Object.values(SOUPS);
  for (const lang of supportedLangs()) {
    for (let seed = 1; seed <= 12; seed++) {
      for (const mode of [0, 1, 2]) {
        const rand = rng(seed * 7919 + lang.length * 13 + mode);
        const alphabet = alphabets[(seed + mode) % alphabets.length];
        let src = "";
        const n = 5 + Math.floor(rand() * 160);
        for (let k = 0; k < n; k++) src += alphabet[Math.floor(rand() * alphabet.length)];

        const st = createInc(lang)!;
        const maxChunk = 1 + Math.floor(rand() * 15);
        let i = 0;
        while (i < src.length) {
          i = Math.min(src.length, i + 1 + Math.floor(rand() * maxChunk));
          // mode 0: the parser's speculative newline. mode 1: a rewritten tail.
          // mode 2: both at once.
          let fed = mode === 1 ? src.slice(0, i) : specClose(src, i);
          if (mode !== 0 && rand() < 0.35) {
            const back = 1 + Math.floor(rand() * 12);
            fed = fed.slice(0, Math.max(0, fed.length - back)) + "?;`'\"*/".slice(0, 3);
          }
          const shown = incHighlight(st, fed);
          if (shown !== null) expect(shown.startsWith(st.frozenHtml)).toBe(true);
          expect(highlight(fed, lang).startsWith(st.frozenHtml)).toBe(true);
        }
        incHighlight(st, src);
        expect(settle(src, lang, incSeed(st, src, lang))).toBe(highlight(src, lang));
      }
    }
  }
});

test("monotonicity: a change that reaches into the frozen prefix restarts cleanly", () => {
  const first = "const first = 1;\nconst a = 2;\nconst b = 3;\nconst c = 4;\n";
  const st = createInc("ts")!;
  for (let i = 4; i <= first.length; i += 4) incHighlight(st, first.slice(0, i));
  expect(st.c).toBeGreaterThan(0);
  const staleFrozen = st.frozenHtml;

  // A speculative revision: same block, different bytes — NOT an append.
  const second = "const second = 9;\nconst z = 8;\nconst y = 7;\nconst x = 6;\n";
  expect(second.startsWith(first)).toBe(false);
  const out = incHighlight(st, second);
  expect(out).toBe(highlight(second, "ts"));
  expect(st.frozenHtml).not.toBe(staleFrozen);
  expect(highlight(second, "ts").startsWith(st.frozenHtml)).toBe(true);

  // …and it keeps streaming correctly from there.
  const grown = second + "const w = 5;\nconst v = 4;\n";
  incHighlight(st, grown);
  expect(settle(grown, "ts", incSeed(st, grown, "ts"))).toBe(highlight(grown, "ts"));

  // A shrink (a tail block replaced by a shorter one) is the same story.
  incHighlight(st, "const q");
  expect(incHighlight(st, "const q = 1;\n")).toBe(highlight("const q = 1;\n", "ts"));
});

test("incSeed refuses a seed that does not belong to the text", () => {
  const src = "const a = 1;\nconst b = 2;\nconst c = 3;\n";
  const st = createInc("js")!;
  for (let i = 4; i <= src.length; i += 4) incHighlight(st, src.slice(0, i));
  expect(incSeed(st, src, "js")).toBeDefined();
  expect(incSeed(st, src, "JS")).toBeDefined(); // case-insensitive, like LANGS
  expect(incSeed(st, src, "python")).toBeUndefined(); // wrong language
  expect(incSeed(st, "let z = 9;\n", "js")).toBeUndefined(); // wrong text
  expect(incSeed(st, "const a", "js")).toBeUndefined(); // shorter than the cursor
  expect(incSeed(createInc("js")!, src, "js")).toBeUndefined(); // nothing frozen
});

test("re-feeding the same text is free and returns the same markup", () => {
  const src = "const a = 1;\nconst b = 2;\n";
  const st = createInc("js")!;
  const first = incHighlight(st, src);
  __resetIncScanned();
  expect(incHighlight(st, src)).toBe(first);
  expect(__getIncScanned()).toBe(0);
});

test("work bound: a streamed code corpus stays inside ~6x its own bytes", () => {
  // The whole point of the frozen prefix: work per patch is the unfrozen TAIL
  // (about one source line), not the block. Without it this ratio grows with the
  // block — a 21 KB block at 4-byte chunks would re-tokenize ~57 MB.
  const sources = [
    CORPUS.js[0],
    CORPUS.ts[0],
    CORPUS.rust[0],
    CORPUS.py[0],
    CORPUS.go[0],
    CORPUS.bash[0],
    CORPUS.json[0],
    CORPUS.sql[0],
    CORPUS.html[0],
    CORPUS.css[0],
  ];
  const langs = ["js", "ts", "rust", "py", "go", "bash", "json", "sql", "html", "css"];
  __resetIncScanned();
  let bytes = 0;
  for (let k = 0; k < langs.length; k++) {
    const src = sources[k].repeat(6); // a realistic multi-KB fence
    bytes += src.length;
    const st = createInc(langs[k])!;
    for (let i = 4; i <= src.length; i += 4) incHighlight(st, src.slice(0, i));
    incHighlight(st, src);
  }
  const ratio = __getIncScanned() / bytes;
  expect(ratio).toBeLessThan(6);
  // …and it is genuinely doing the work, not skipping it.
  expect(ratio).toBeGreaterThan(1);
});

test("work bound: once the cap engages, per-patch work is O(new bytes)", () => {
  // The adversarial shape — one enormous line, so no checkpoint is ever
  // available and the unfrozen tail can only grow.
  const stream = (len: number): { max: number; total: number } => {
    const src = "x".repeat(len);
    const st = createInc("js")!;
    __resetIncScanned();
    let max = 0;
    for (let i = 256; i <= src.length; i += 256) {
      const before = __getIncScanned();
      incHighlight(st, src.slice(0, i));
      const spent = __getIncScanned() - before;
      if (i > 9000 && spent > max) max = spent; // past the cap
    }
    return { max, total: __getIncScanned() };
  };

  const small = stream(20_000);
  const big = stream(40_000);
  // Past the cap, no patch tokenizes ANYTHING — the tail is escaped, not parsed.
  expect(small.max).toBe(0);
  expect(big.max).toBe(0);
  // So the total is whatever the pre-cap ramp cost and nothing more: doubling
  // the block does not add a byte of tokenizing. Without the cap this would be
  // quadratic in the block (~3.1 GB at 40 KB).
  expect(big.total).toBe(small.total);
  expect(big.total).toBeLessThan(200_000);
});

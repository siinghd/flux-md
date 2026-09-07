import { test, expect, afterEach } from "bun:test";
import {
  __builtinLangs,
  highlight,
  stepHighlight,
  supportedLangs,
  type HighlightState,
} from "../src/hi";
import { highlightDeferred, highlightWithin, __setSliceMs } from "../src/hi-defer";

/**
 * THE parity proof for non-blocking highlighting.
 *
 * `highlight()` is a single linear pass whose only carried state is `pos`, so
 * pausing it and resuming it later cannot change what it emits. The renderers
 * bet the whole design on that: they stop the pass on a time budget, hand the
 * main thread back, and resume on a later task. This suite is the bet's proof —
 * for every language in the corpus, at chunk sizes from one token per slice up
 * to the whole block, the chunked run's output is BYTE-identical to the
 * one-shot `highlight()` output.
 *
 * The redos suite (hi-redos.test.ts) stays untouched and keeps asserting the
 * one-shot output on its own, so a regression there is a regression here too.
 */

// One representative snippet per supported language family, chosen to hit the
// branchy parts of each pattern list: comments, every string form, numbers,
// keywords, type-cased and call-position identifiers, and characters that force
// the escaper to run (`<`, `>`, `&`, `"`).
const CORPUS: Record<string, string> = {
  js: `// head <&>\nimport { readFile } from "node:fs";\nconst re = /ab+c/gi, s = 'x\\'y', t = \`a\${b}"c"\`;\nexport async function main(argv) {\n  /* block & comment */\n  let n = 0x1f_ff + 0b1010 + 1.25e3;\n  if (n < 10 && n > 2) return new Map([[1, "a<b"]]);\n  class Foo extends Bar { #p = null; }\n  return await Foo.run(argv ?? []);\n}\n`,
  ts: `export interface User { id: number; name: string; tags?: readonly string[] }\ntype Maybe<T> = T | null;\ndeclare const x: unknown;\nexport const pick = <T,>(v: T): Maybe<T> => (v as unknown as Maybe<T>);\nnamespace NS { export enum E { A = 1, B }\n}\nconst q = "a & b <c> \\"d\\"";\n`,
  tsx: `export function App({ items }: Props) {\n  return <ul className="list">{items.map((i) => <li key={i.id}>{i.name}</li>)}</ul>;\n}\n`,
  jsx: `const El = () => <div data-x="1">{"a<b"}</div>;\n`,
  javascript: `for (var i = 0; i < 10; i++) console.log(i, "&");\n`,
  typescript: `let v: Array<string> = [];\nv satisfies string[];\n`,
  rust: `//! crate doc\nuse std::collections::HashMap;\n#[derive(Debug, Clone)]\npub struct Cfg<'a> { pub name: &'a str, n: u32, f: f64 }\nimpl<'a> Cfg<'a> {\n    pub fn new(name: &'a str) -> Self {\n        let mut m: HashMap<&str, u8> = HashMap::new();\n        m.insert("a<b", b'x');\n        println!("{} & {}", name, 0xff_u32);\n        Self { name, n: 1_000, f: 2.5 }\n    }\n}\n`,
  rs: `fn main() { let s = "a\\"b"; /* c */ let _ = &s; }\n`,
  py: `import os\nfrom typing import Optional\n\n@dataclass\nclass Point:\n    """A <point> & such."""\n    x: int = 0\n\n    def norm(self, k: float = 1.5) -> Optional[float]:\n        s = f'{self.x!r}' + "a<b" + '''tri'''\n        return None if self.x < 0 else s and 0o17 or 1j\n`,
  python: `def f(*a, **kw):\n    # comment &\n    return [i for i in a if i > 0]\n`,
  go: `package main\n\nimport "fmt"\n\ntype Server struct{ Addr string }\n\nfunc (s *Server) Run(ctx context.Context) error {\n\t// serve <&>\n\tconst n = 1_000\n\tch := make(chan int, n)\n\tgo func() { ch <- 'a' }()\n\tfmt.Printf("%q & %v\\n", s.Addr, ` + "`raw`" + `)\n\treturn nil\n}\n`,
  bash: `#!/usr/bin/env bash\nset -euo pipefail\nNAME="\${1:-world}"\nfor f in *.txt; do\n  if [[ -f "$f" ]]; then\n    echo "hi $NAME <$f> & done" | tr 'a-z' 'A-Z' > "$f.out"\n  fi\ndone\nexit 0\n`,
  sh: `cd /tmp && ls -la | grep '^d' || echo "none"\n`,
  shell: `while read -r line; do printf '%s\\n' "$line"; done < in.txt\n`,
  json: `{\n  "name": "brookmd",\n  "n": -12.5e3,\n  "ok": true,\n  "nil": null,\n  "list": [1, 2, {"a": "b<c>&\\"d\\""}]\n}\n`,
  sql: `-- pick users\n/* multi\n   line */\nSELECT u.id, COUNT(*) AS n\nFROM users u\nLEFT JOIN orders o ON o.user_id = u.id\nWHERE u.name LIKE 'a''b%' AND u.age > 21\nGROUP BY u.id HAVING COUNT(*) >= 2\nORDER BY n DESC LIMIT 10;\n`,
  html: `<!-- page & such -->\n<div class="wrap" data-n='1'>\n  <p>Text &amp; more &lt;here&gt;</p>\n  <img src="a.png" alt="a > b"/>\n</div>\n`,
  xml: `<?xml version="1.0"?>\n<root><item id="1">a &amp; b</item></root>\n`,
  css: `/* theme & vars */\n:root { --gap: 8px; }\n.brook-md > pre[data-lang="ts"]::after {\n  content: "a<b";\n  margin: 0 auto -1.5rem;\n  transition: all 120ms ease-in-out;\n}\n`,
  java: "// head <&>\npackage com.example;\n\nimport java.util.List;\n\n@Override\npublic class Foo extends Bar {\n  private static final int N = 0x1f_ff;\n  private final String s = \"a<b\";\n  public List<String> run(char c) {\n    /* block & comment */\n    double d = 1.25e3;\n    if (N < 10 && N > 2) return List.of(s, \"&\");\n    return null;\n  }\n}\n",
  c: "#include <stdio.h>\n#define N 10\n// head <&>\nstatic const char *msg = \"a & b <c>\";\n\nint main(void) {\n  /* block */\n  for (int i = 0; i < N; i++) {\n    printf(\"%d %c\\n\", i, 'x');\n  }\n  return 0;\n}\n",
  cpp: "#include <vector>\n// head <&>\nnamespace ns {\ntemplate <typename T>\nclass Vec {\n public:\n  explicit Vec(T v) : x(v) {}\n  T x = 1.5f;\n};\n}  // namespace ns\nauto *p = new ns::Vec<int>(0xff);\n",
  "c++": "// c++ <&>\nauto f = [](int x) -> int { return x * 2; };\nstd::string s = \"a<b & c\";\n",
  cs: "// head <&>\nusing System;\n\nnamespace App {\n  public class Svc {\n    private const int N = 42;\n    public string Name { get; set; } = \"a<b\";\n    public async Task<int> RunAsync(double d = 1.5) {\n      /* block & */\n      await Task.Delay(10);\n      return N;\n    }\n  }\n}\n",
  csharp: "var xs = new List<int> { 1, 2, 3 };\nConsole.WriteLine(\"n & <more>\");\n",
  swift: "// head <&>\nimport Foundation\n\n@objc final class Store: NSObject {\n  let limit: Int = 0x20\n  private var items: [String] = [\"a<b\", \"&\"]\n\n  func add(_ s: String) throws -> Bool {\n    /* block & comment */\n    guard !s.isEmpty else { return false }\n    items.append(s)\n    return items.count < limit\n  }\n}\n",
  kt: "// head <&>\npackage com.example\n\nimport kotlin.math.max\n\ndata class Point(val x: Int = 0, val y: Double = 1.5) {\n  fun norm(): Double {\n    /* block & */\n    return max(x.toDouble(), y)\n  }\n}\n\nval label = \"a<b & c\"\n",
  kotlin: "fun main() {\n  val xs = listOf(1, 2, 3)\n  println(\"hi & <there>\")\n}\n",
  php: "<?php\n// head <&>\n# hash comment\nnamespace App;\n\n/* block & comment */\nfunction render(array $rows): string {\n    $out = '';\n    foreach ($rows as $k => $v) {\n        $out .= \"<li>{$k} & {$v}</li>\";\n    }\n    return $out;\n}\n?>\n",
  rb: "# head <&>\nrequire 'json'\n\nclass Point < Base\n  attr_reader :x\n\n  def initialize(x = 0, y = 1.5)\n    @x = x\n    @@count = 0\n    $global = nil\n  end\n\n  def to_s\n    \"a<b & more\"\n  end\nend\n",
  ruby: "puts [1, 2, 3].map { |n| n * 2 }.inspect\nsym = :name\n",
  yaml: "# head <&>\nname: brookmd\nversion: 1.2\nenabled: true\nempty: null\nquoted: \"a & b <c>\"\nsingle: 'it''s'\nlist:\n  - one\n  - two\nnested:\n  key: value\nanchor: &base\n  a: 1\nref: *base\n",
  yml: "a: 1\nb: \"two & <three>\"\n",
  toml: "# head <&>\ntitle = \"brookmd & co\"\nversion = 1\nratio = 1.5\nok = true\n\n[server]\nhost = 'localhost'\nports = [8000, 8001]\n\n[[bin]]\nname = \"cli\"\n",
  diff: "diff --git a/src/hi.ts b/src/hi.ts\nindex 1234567..89abcde 100644\n--- a/src/hi.ts\n+++ b/src/hi.ts\n@@ -1,6 +1,7 @@\n context line & <more>\n-const old = \"a\";\n+const now = \"b\";\n+const extra = 1;\n unchanged\n",
  dockerfile: "# head <&>\nFROM node:20-alpine AS build\nWORKDIR /app\nENV NODE_ENV=\"production\" PORT=3000\nCOPY package.json ./\nRUN npm ci && npm run build\nEXPOSE 3000\nCMD [\"node\", \"dist/index.js\"]\n",
};

// Inputs that are not "a language sample": the fallbacks, the pathological
// shapes the redos suite guards, and the empty edge.
const EDGE: Array<[string, string]> = [
  ["", "js"],
  ["x", "js"],
  ["\n\n\t  \n", "js"],
  ["<&>\"'", "js"],
  ["plain text, unknown language <&>", "cobol"], // no LANGS entry -> escape path
  ["a".repeat(3000), "not-a-lang"], // escape path, several chunks
  ["<".repeat(60_000), "js"], // over the 50 000 guard -> escape path
  ["/" + "[".repeat(20_000), "js"], // rx backtracking shape
  ['"' + "$(".repeat(20_000), "bash"], // bash string backtracking shape
];

/** Run the resumable core to completion in `chars`-sized slices. */
function chunked(code: string, lang: string, chars: number): string {
  const state: HighlightState = { pos: 0, out: "" };
  let guard = 0;
  while (!stepHighlight(code, lang, state, chars)) {
    // Every slice must consume at least one token, so this can only spin if the
    // core stopped making progress — fail loudly instead of hanging the suite.
    if (++guard > code.length + 16) throw new Error("stepHighlight made no progress");
  }
  return state.out;
}

const CHUNK_SIZES = [1, 2, 3, 7, 64, 997, 4096];

afterEach(() => __setSliceMs());

test("chunked stepHighlight === one-shot highlight for every supported language", () => {
  // The BUILT-IN table, not supportedLangs(): another suite may have called
  // registerLanguage() into the same module registry, and a caller's language is
  // not this corpus's business. Every language that SHIPS must have a sample.
  const langs = __builtinLangs();
  expect(langs.filter((l) => !(l in CORPUS))).toEqual([]);
  expect(supportedLangs()).toEqual(expect.arrayContaining(langs));
  for (const lang of langs) {
    const code = CORPUS[lang];
    const oneShot = highlight(code, lang);
    for (const chars of CHUNK_SIZES) {
      expect(chunked(code, lang, chars)).toBe(oneShot);
    }
    // A slice larger than the input is the degenerate one-shot case.
    expect(chunked(code, lang, code.length * 4 + 1)).toBe(oneShot);
  }
});

test("chunked === one-shot on the fallback, empty and pathological inputs", () => {
  for (const [code, lang] of EDGE) {
    const oneShot = highlight(code, lang);
    for (const chars of [1, 13, 1024]) {
      expect(chunked(code, lang, chars)).toBe(oneShot);
    }
  }
});

test("a non-positive slice still makes progress (one token per call)", () => {
  const code = CORPUS.js;
  expect(chunked(code, "js", 0)).toBe(highlight(code, "js"));
  expect(chunked(code, "js", -5)).toBe(highlight(code, "js"));
});

test("highlight() is unchanged for a hand-checked sample (span shapes intact)", () => {
  // Guards the escaper rewrite: keywords, calls, types, strings with entities.
  expect(highlight('const a = f("x<y&z");', "js")).toBe(
    '<span class="t-kw">const</span> a <span class="t-pun">=</span> ' +
      '<span class="t-fn">f</span><span class="t-pun">(</span>' +
      '<span class="t-str">&quot;x&lt;y&amp;z&quot;</span>' +
      '<span class="t-pun">)</span><span class="t-pun">;</span>',
  );
  // Whitespace passes through unescaped and unwrapped, as before.
  expect(highlight("a\n\tb", "js")).toBe("a\n\tb");
});

// ---------------------------------------------------------------------------
// The sliced driver
// ---------------------------------------------------------------------------

test("a small block finishes inside the first (synchronous) slice", () => {
  const code = CORPUS.rust;
  expect(highlightWithin(code, "rust")).toBe(highlight(code, "rust"));
  const run = highlightDeferred(code, "rust");
  expect(run.html).toBe(highlight(code, "rust")); // applied in the caller's tick
  expect(run.rest).toBeNull(); // nothing left to schedule
});

test("a block that outruns the budget resolves with the identical markup", async () => {
  __setSliceMs(0); // force the sliced path regardless of machine speed
  const code = CORPUS.ts.repeat(40);
  const run = highlightDeferred(code, "ts");
  expect(run.html).toBeNull(); // nothing to apply synchronously
  expect(run.rest).not.toBeNull();
  expect(await run.rest).toBe(highlight(code, "ts"));
});

test("the escape fallback (over the size guard) is sliced too", async () => {
  __setSliceMs(0);
  const code = "<a & b>".repeat(9_000); // > 50 000 chars: plain-escape path
  const run = highlightDeferred(code, "js");
  expect(run.html).toBeNull();
  expect(await run.rest).toBe(highlight(code, "js"));
});

test("cancel() abandons the run and settles with null", async () => {
  __setSliceMs(0);
  const code = CORPUS.py.repeat(60);
  const run = highlightDeferred(code, "py");
  expect(run.html).toBeNull();
  run.cancel();
  expect(await run.rest).toBeNull();
});

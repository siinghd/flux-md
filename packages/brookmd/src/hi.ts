/**
 * In-house syntax highlighter. Native RegExp only. Covers the languages an LLM
 * typically emits: js/ts/tsx/jsx, rust, python, go, bash, json, html, css, sql,
 * yaml, toml, diff, java, c/c++, c#, php, ruby, swift, kotlin and dockerfile.
 * {@link registerLanguage} adds more at runtime; anything still unknown falls
 * through to plain escaped text.
 *
 * Highlighting is per-block, runs once when the block closes. We never
 * highlight an open (streaming) block, which avoids re-highlighting the same
 * code on every chunk — the main perf win for streaming code.
 */

const KEYWORDS_JS = new Set(
  "async await break case catch class const continue debugger default delete do else export extends false finally for from function if import in instanceof let new null of return static super switch this throw true try typeof undefined var void while with yield".split(
    " ",
  ),
);
const KEYWORDS_TS = new Set([
  ...KEYWORDS_JS,
  ...["any", "as", "boolean", "declare", "enum", "interface", "is", "keyof", "module", "namespace", "never", "number", "private", "protected", "public", "readonly", "string", "type", "unknown", "satisfies"],
]);
const KEYWORDS_RUST = new Set(
  "as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod move mut pub ref return Self self static struct super trait true type unsafe use where while".split(
    " ",
  ),
);
const KEYWORDS_PY = new Set(
  "False None True and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield".split(
    " ",
  ),
);
const KEYWORDS_GO = new Set(
  "break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var nil true false".split(
    " ",
  ),
);
const KEYWORDS_BASH = new Set(
  "if then elif else fi case esac for select while until do done function in time coproc return break continue".split(
    " ",
  ),
);
const KEYWORDS_SQL = new Set(
  "SELECT FROM WHERE JOIN LEFT RIGHT INNER OUTER ON GROUP BY ORDER HAVING LIMIT OFFSET INSERT INTO VALUES UPDATE SET DELETE CREATE TABLE DROP ALTER INDEX VIEW IF EXISTS NOT NULL DEFAULT PRIMARY KEY FOREIGN REFERENCES UNIQUE AS WITH UNION ALL DISTINCT IS BETWEEN LIKE IN AND OR".split(
    " ",
  ),
);

const KEYWORDS_JAVA = new Set(
  "abstract assert boolean break byte case catch char class const continue default do double else enum extends false final finally float for goto if implements import instanceof int interface long native new null package private protected public record return sealed short static strictfp super switch synchronized this throw throws transient true try var void volatile while yield".split(
    " ",
  ),
);
const KEYWORDS_C = new Set(
  "auto bool break case char const continue default do double else enum extern false float for goto if inline int long register restrict return short signed sizeof static struct switch true typedef union unsigned void volatile while NULL".split(
    " ",
  ),
);
const KEYWORDS_CPP = new Set([
  ...KEYWORDS_C,
  ...["catch", "class", "concept", "constexpr", "const_cast", "decltype", "delete", "dynamic_cast", "explicit", "export", "final", "friend", "mutable", "namespace", "new", "noexcept", "nullptr", "operator", "override", "private", "protected", "public", "reinterpret_cast", "requires", "static_assert", "static_cast", "template", "this", "throw", "try", "typeid", "typename", "using", "virtual"],
]);
const KEYWORDS_CS = new Set(
  "abstract as async await base bool break byte case catch char checked class const continue decimal default delegate do double dynamic else enum event explicit extern false finally fixed float for foreach get goto if implicit in int interface internal is lock long nameof namespace new null object operator out override params private protected public readonly record ref return sbyte sealed set short sizeof stackalloc static string struct switch this throw true try typeof uint ulong unchecked unsafe ushort using var virtual void volatile while yield".split(
    " ",
  ),
);
const KEYWORDS_PHP = new Set(
  "abstract and array as bool break callable case catch class clone const continue declare default do echo else elseif empty enum extends final finally float fn for foreach function global goto if implements include include_once instanceof insteadof int interface isset list match namespace new null or parent print private protected public readonly require require_once return self static string switch throw trait true try unset use var void while xor yield false".split(
    " ",
  ),
);
const KEYWORDS_RUBY = new Set(
  "BEGIN END alias and begin break case class def defined? do else elsif end ensure false for if in module next nil not or redo require require_relative rescue retry return self super then true undef unless until when while yield attr_accessor attr_reader attr_writer lambda proc".split(
    " ",
  ),
);
const KEYWORDS_SWIFT = new Set(
  "actor any as associatedtype async await break case catch class continue default defer deinit do else enum extension fallthrough false fileprivate for func guard if import in indirect init inout internal is lazy let mutating nil open operator private protocol public repeat rethrows return self Self some static struct subscript super switch throw throws true try typealias var where while".split(
    " ",
  ),
);
const KEYWORDS_KOTLIN = new Set(
  "abstract actual annotation as break by catch class companion const constructor continue crossinline data do dynamic else enum expect external false field final finally for fun get if import in infix init inline inner interface internal is it lateinit noinline null object open operator out override package private protected public reified return sealed set super suspend tailrec this throw true try typealias typeof val var vararg when where while".split(
    " ",
  ),
);
// Dockerfile instructions are the keywords, and they are conventionally upper
// case — the lookup is case-sensitive, so `run` stays a plain identifier.
const KEYWORDS_DOCKER = new Set(
  "ADD ARG AS CMD COPY ENTRYPOINT ENV EXPOSE FROM HEALTHCHECK LABEL MAINTAINER ONBUILD RUN SHELL STOPSIGNAL USER VOLUME WORKDIR as".split(
    " ",
  ),
);

// Each language is described by an ordered list of (token-class, regex) pairs.
// The regex must be sticky (y flag) so it only matches at the current cursor.
// First match wins.
type Pat = [string, RegExp];

const jsPats: Pat[] = [
  ["com", /\/\/[^\n]*/y],
  ["com", /\/\*[\s\S]*?\*\//y],
  ["str", /"(?:\\.|[^"\\\n])*"/y],
  ["str", /'(?:\\.|[^'\\\n])*'/y],
  ["str", /`(?:\\.|[^`\\])*`/y],
  ["rx", /\/(?![*/])(?:\\.|[^/\\\n])+\/[gimsuy]*/y],
  ["num", /\b(?:0x[\da-fA-F_]+|0b[01_]+|0o[0-7_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)\b/y],
  ["ident", /[A-Za-z_$][\w$]*/y],
  ["pun", /[+\-*/=<>!&|^~?:;,.[\](){}]/y],
  ["ws", /\s+/y],
];

const rustPats: Pat[] = [
  ["com", /\/\/[^\n]*/y],
  ["com", /\/\*[\s\S]*?\*\//y],
  ["str", /b?"(?:\\.|[^"\\])*"/y],
  ["str", /b?'(?:\\.|[^'\\])'/y],
  ["lt", /'[a-zA-Z_][\w]*/y],
  ["num", /\b\d[\d_]*(?:\.\d[\d_]*)?(?:[ui](?:8|16|32|64|128|size)|f(?:32|64))?\b/y],
  ["mac", /[A-Za-z_]\w*!/y],
  ["attr", /#!?\[[^\]]*\]/y],
  ["ident", /[A-Za-z_]\w*/y],
  ["pun", /[+\-*/=<>!&|^~?:;,.\[\](){}@]/y],
  ["ws", /\s+/y],
];

const pyPats: Pat[] = [
  ["com", /#[^\n]*/y],
  ["str", /[fFrRbB]{0,2}"""[\s\S]*?"""/y],
  ["str", /[fFrRbB]{0,2}'''[\s\S]*?'''/y],
  ["str", /[fFrRbB]{0,2}"(?:\\.|[^"\\\n])*"/y],
  ["str", /[fFrRbB]{0,2}'(?:\\.|[^'\\\n])*'/y],
  ["num", /\b(?:0x[\da-fA-F_]+|0b[01_]+|0o[0-7_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?[jJ]?)\b/y],
  ["dec", /@[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)*/y],
  ["ident", /[A-Za-z_]\w*/y],
  ["pun", /[+\-*/=<>!&|^~?:;,.[\](){}@%]/y],
  ["ws", /\s+/y],
];

const goPats: Pat[] = [
  ["com", /\/\/[^\n]*/y],
  ["com", /\/\*[\s\S]*?\*\//y],
  ["str", /"(?:\\.|[^"\\\n])*"/y],
  ["str", /`[^`]*`/y],
  ["str", /'(?:\\.|[^'\\\n])'/y],
  ["num", /\b\d[\d_]*(?:\.\d[\d_]*)?\b/y],
  ["ident", /[A-Za-z_]\w*/y],
  ["pun", /[+\-*/=<>!&|^~?:;,.[\](){}]/y],
  ["ws", /\s+/y],
];

const bashPats: Pat[] = [
  ["com", /#[^\n]*/y],
  ["str", /"(?:\\.|[^"\\])*"/y],
  ["str", /'[^']*'/y],
  ["var", /\$\{[^}]+\}|\$\w+|\$[*@#?!$0-9]/y],
  ["num", /\b\d+\b/y],
  ["ident", /[A-Za-z_][\w-]*/y],
  ["pun", /[|&;<>(){}[\]=]/y],
  ["ws", /\s+/y],
];

const jsonPats: Pat[] = [
  ["str", /"(?:\\.|[^"\\\n])*"/y],
  ["num", /-?\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b/y],
  ["kw", /\b(?:true|false|null)\b/y],
  ["pun", /[{}[\]:,]/y],
  ["ws", /\s+/y],
];

const sqlPats: Pat[] = [
  ["com", /--[^\n]*/y],
  ["com", /\/\*[\s\S]*?\*\//y],
  ["str", /'(?:''|[^'])*'/y],
  ["str", /"(?:""|[^"])*"/y],
  ["num", /\b\d+(?:\.\d+)?\b/y],
  ["ident", /[A-Za-z_][\w]*/y],
  ["pun", /[+\-*/=<>!,;.(){}]/y],
  ["ws", /\s+/y],
];

const htmlPats: Pat[] = [
  ["com", /<!--[\s\S]*?-->/y],
  ["tag", /<\/?[A-Za-z][\w-]*/y],
  ["str", /"[^"]*"/y],
  ["str", /'[^']*'/y],
  ["attr", /[A-Za-z][\w-]*(?==)/y],
  ["pun", /[=/>]/y],
  ["txt", /[^<>"'=]+/y],
];

const cssPats: Pat[] = [
  ["com", /\/\*[\s\S]*?\*\//y],
  ["str", /"[^"]*"/y],
  ["str", /'[^']*'/y],
  ["num", /-?\d+(?:\.\d+)?(?:px|em|rem|%|vh|vw|s|ms|deg)?/y],
  ["sel", /[#.]?[A-Za-z][\w-]*/y],
  ["pun", /[:;,{}()]/y],
  ["ws", /\s+/y],
];

// The C family. `//` + `/* … */` comments, line-bounded quoted strings, one
// annotation form and a numeric literal with the usual suffixes — the shape is
// identical across java/c#/swift/kotlin, so they share this list and differ only
// in their keyword set. c/cpp prepend the preprocessor line (see `cPats`).
const cFamilyPats: Pat[] = [
  ["com", /\/\/[^\n]*/y],
  ["com", /\/\*[\s\S]*?\*\//y],
  ["str", /"(?:\\.|[^"\\\n])*"/y],
  ["str", /'(?:\\.|[^'\\\n])*'/y],
  // The suffix class after a hex literal must not overlap the hex digits:
  // `[\da-fA-F_]+` followed by `[lLfFdDuU]*` shares `d D f F`, and a long hex
  // run ending in a non-suffix word char then backtracks quadratically (a
  // 16 KB run cost ~0.5 s). Hex/binary take only `[lLuU]`; decimal keeps the
  // float suffixes, whose class is disjoint from digits.
  ["num", /\b(?:0x[\da-fA-F_]+[lLuU]*|0b[01_]+[lLuU]*|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?[lLfFdDuU]*)\b/y],
  ["dec", /@[A-Za-z_]\w*/y],
  ["ident", /[A-Za-z_]\w*/y],
  ["pun", /[+\-*/=<>!&|^~?:;,.[\](){}%]/y],
  ["ws", /\s+/y],
];

// `#include` and friends. The gap after `#` is `[ \t]*`, NOT `\s*`: a pattern
// that could swallow a newline would be an unbounded opener, and the streaming
// checkpoint rule (hi-inc.ts) is entitled to assume this table has none beyond
// the block comment.
const cPats: Pat[] = [["mac", /#[ \t]*[A-Za-z_]+/y], ...cFamilyPats];

const phpPats: Pat[] = [
  ["com", /\/\/[^\n]*/y],
  ["com", /#[^\n]*/y],
  ["com", /\/\*[\s\S]*?\*\//y],
  ["tag", /<\?php\b|<\?=|\?>/y],
  ["str", /"(?:\\.|[^"\\\n])*"/y],
  ["str", /'(?:\\.|[^'\\\n])*'/y],
  ["var", /\$+[A-Za-z_]\w*/y],
  ["num", /\b(?:0x[\da-fA-F_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)\b/y],
  ["ident", /[A-Za-z_]\w*/y],
  ["pun", /[+\-*/=<>!&|^~?:;,.[\](){}%@\\]/y],
  ["ws", /\s+/y],
];

const rubyPats: Pat[] = [
  ["com", /#[^\n]*/y],
  ["str", /"(?:\\.|[^"\\\n])*"/y],
  ["str", /'(?:\\.|[^'\\\n])*'/y],
  ["lt", /:[A-Za-z_]\w*[?!]?/y],
  ["var", /@@?[A-Za-z_]\w*|\$[A-Za-z_]\w*/y],
  ["num", /\b\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?\b/y],
  ["ident", /[A-Za-z_]\w*[?!]?/y],
  ["pun", /[+\-*/=<>!&|^~?:;,.[\](){}%]/y],
  ["ws", /\s+/y],
];

// yaml. Keys are `attr`; the lookahead that recognizes them is TWO characters
// wide (`:` plus the space or newline that must follow) so that a frozen token
// never depends on a byte further ahead than hi-inc.ts's GAP reserve.
const yamlPats: Pat[] = [
  ["com", /#[^\n]*/y],
  ["str", /"(?:\\.|[^"\\\n])*"/y],
  ["str", /'(?:''|[^'\n])*'/y],
  ["attr", /[A-Za-z_][\w.\-]*(?=:(?:\s|$))/y],
  ["lt", /\b(?:true|false|null|True|False|Null|TRUE|FALSE|NULL|yes|no|on|off)\b/y],
  ["num", /-?\b\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?\b/y],
  ["var", /[&*][A-Za-z_][\w.\-]*/y],
  ["ident", /[A-Za-z_][\w.\-]*/y],
  ["pun", /[:\-?[\]{},>|!]/y],
  ["ws", /\s+/y],
];

// toml. `[table]` / `[[array]]` headers are `sel` and only at the start of a
// line (`^` under `m` reads bytes BEHIND the cursor, which are always settled);
// `key =` is `attr` through a lookahead of at most three characters.
const tomlPats: Pat[] = [
  ["com", /#[^\n]*/y],
  ["sel", /^\[\[?[^\]\n]*\]\]?/my],
  ["str", /"(?:\\.|[^"\\\n])*"/y],
  ["str", /'[^'\n]*'/y],
  ["lt", /\b(?:true|false)\b/y],
  ["attr", /[A-Za-z_][\w.\-]*(?=[ \t]{0,2}=)/y],
  ["num", /[+-]?\b\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?\b/y],
  ["ident", /[A-Za-z_][\w.\-]*/y],
  ["pun", /[=[\]{},.:+\-]/y],
  ["ws", /\s+/y],
];

// Unified diff. Every pattern is a whole line anchored at `^` (with `m`), so the
// class of a line can never be rewritten by a byte on a later one: headers and
// `@@` hunks are `com`, additions `str`, deletions `kw`, context lines `txt`.
const diffPats: Pat[] = [
  ["com", /^(?:diff |index |similarity |rename |new file|deleted file|old mode|new mode|Binary |@@|---|\+\+\+)[^\n]*/my],
  ["str", /^\+[^\n]*/my],
  ["kw", /^-[^\n]*/my],
  // `ws` FIRST, ahead of the context-line catch-all: the streaming checkpoint
  // (hi-inc.ts) cuts a whitespace run in two just after a newline and re-matches
  // `\s+` from there, so no other pattern may be able to start on whitespace —
  // `[^\n]+` would happily swallow a line's indentation and re-tokenize the cut
  // differently from a run that never stopped.
  ["ws", /\s+/y],
  ["txt", /[^\n]+/y],
];

// Dockerfile. `${…}` is line-bounded here (bash's is not, and is an opener
// there), so this table has no unbounded form at all.
const dockerPats: Pat[] = [
  ["com", /#[^\n]*/y],
  ["str", /"(?:\\.|[^"\\\n])*"/y],
  ["str", /'[^'\n]*'/y],
  ["var", /\$\{[^}\n]*\}|\$\w+/y],
  ["num", /\b\d+\b/y],
  ["ident", /[A-Za-z_][\w.\-]*/y],
  ["pun", /[|&;<>(){}[\]=,:@/\\*+]/y],
  ["ws", /\s+/y],
];

/** One language table: the ordered patterns, plus the words `ident` promotes. */
interface LangEntry {
  pats: Pat[];
  kw?: Set<string>;
}

/**
 * The language table, keyed by lower-cased name.
 *
 * Prototype-free on purpose: a key here is user data — a fence's info string, or
 * a name handed to {@link registerLanguage} — so `toString` and `__proto__` have
 * to be ordinary misses rather than reaching `Object.prototype`. Still a plain
 * object, so a lookup is still one hash probe.
 */
const LANGS: Record<string, LangEntry> = Object.create(null) as Record<string, LangEntry>;
/** Names (lower-cased) that `registerLanguage` has written, built-in or not. */
const REGISTERED = new Set<string>();

/** Bind one pattern table (and keyword set) to a space-separated list of names. */
function def(names: string, pats: Pat[], kw?: Set<string>): void {
  const entry: LangEntry = kw === undefined ? { pats } : { pats, kw };
  for (const name of names.split(" ")) LANGS[name] = entry;
}

def("js javascript jsx", jsPats, KEYWORDS_JS);
def("ts tsx typescript", jsPats, KEYWORDS_TS);
def("rust rs", rustPats, KEYWORDS_RUST);
def("py python", pyPats, KEYWORDS_PY);
def("go", goPats, KEYWORDS_GO);
def("bash sh shell", bashPats, KEYWORDS_BASH);
def("json", jsonPats);
def("sql", sqlPats, KEYWORDS_SQL);
def("html xml", htmlPats);
def("css", cssPats);
def("java", cFamilyPats, KEYWORDS_JAVA);
def("c", cPats, KEYWORDS_C);
def("cpp c++", cPats, KEYWORDS_CPP);
def("cs csharp", cFamilyPats, KEYWORDS_CS);
def("swift", cFamilyPats, KEYWORDS_SWIFT);
def("kt kotlin", cFamilyPats, KEYWORDS_KOTLIN);
def("php", phpPats, KEYWORDS_PHP);
def("rb ruby", rubyPats, KEYWORDS_RUBY);
def("yaml yml", yamlPats);
def("toml", tomlPats);
def("diff", diffPats);
def("dockerfile", dockerPats, KEYWORDS_DOCKER);

/** The languages that ship with the highlighter, before any registration. */
const BUILTIN: readonly string[] = Object.keys(LANGS);
const BUILTIN_ENTRIES: ReadonlyMap<string, LangEntry> = new Map(Object.entries(LANGS));

/** @internal Test-only: the built-in languages, without registered additions. */
export function __builtinLangs(): string[] {
  return BUILTIN.slice();
}

/** @internal Whether `lang` has a tokenizer table (built-in or registered). */
/**
 * Whether `lang` currently resolves to a table supplied through
 * {@link registerLanguage} — including a built-in name the caller replaced. The
 * streaming highlighter asks this so a replaced built-in loses the frozen-prefix
 * mode that was derived from the ORIGINAL table's forms.
 */
export function isRegisteredLang(lang: string): boolean {
  return REGISTERED.has(lang.toLowerCase());
}

/** @internal Test-only: restore the built-in tables and forget registrations. */
export function __resetLanguages(): void {
  for (const k of Object.keys(LANGS)) delete LANGS[k];
  for (const [k, v] of BUILTIN_ENTRIES) LANGS[k] = v;
  REGISTERED.clear();
}

export function hasLang(lang: string): boolean {
  return LANGS[lang.toLowerCase()] !== undefined;
}

// `<`, `>`, `&`, `"` — the only characters escapeHtml rewrites. Context-free by
// construction (one character in, one fixed entity out), which is what lets the
// incremental path escape a growing tail one appended slice at a time. It used to
// build its result one character at a time (`out += c`), which costs a string
// append per character of every token and of every byte of an escape-fallback
// block. Now it scans for the next special character and copies the run before
// it in one slice: the overwhelmingly common "nothing to escape" case returns
// the input untouched, and the rest costs one append per SPECIAL character
// instead of one per character. Same bytes out.
export function escapeHtml(s: string): string {
  const n = s.length;
  let i = 0;
  for (; i < n; i++) {
    const c = s.charCodeAt(i);
    if (c === 60 || c === 62 || c === 38 || c === 34) break;
  }
  if (i === n) return s; // nothing to escape: zero copies, zero allocations
  let out = s.slice(0, i);
  let last = i;
  for (; i < n; i++) {
    const c = s.charCodeAt(i);
    let esc: string;
    if (c === 60) esc = "&lt;";
    else if (c === 62) esc = "&gt;";
    else if (c === 38) esc = "&amp;";
    else if (c === 34) esc = "&quot;";
    else continue;
    out += s.slice(last, i) + esc;
    last = i + 1;
  }
  return out + s.slice(last);
}

/**
 * The resumable tokenizer's cursor: `pos` is the next source index to consume,
 * `out` the markup emitted so far. Start a run at `{ pos: 0, out: "" }`.
 */
export interface HighlightState {
  pos: number;
  out: string;
}

/**
 * Called once per token the tokenizer emits, AFTER its markup is appended:
 * `(cls, start, end, outLen)` where `cls` is the PATTERN class (`ws`, `str`,
 * `com`, `pun`, `ident`… — not the `kw`/`fn`/`ty` refinement), `[start, end)` is
 * the source span, and `outLen` is `state.out.length` once the token has been
 * written. The catch-all one-character fallback reports `cls === ""`.
 *
 * Passing no sink is the default and costs one `undefined` test per token; the
 * escape-fallback path (unknown language / over the size guard) emits no tokens
 * and so reports nothing.
 *
 * @internal The incremental streaming path (hi-inc.ts) is the only consumer —
 * it needs token boundaries to pick a checkpoint that survives an append.
 */
export type TokenSink = (cls: string, start: number, end: number, outLen: number) => void;

/**
 * One resumable slice of {@link highlight}. Consumes WHOLE tokens from
 * `state.pos` until at least `chars` source characters have been taken (or the
 * input ends), appending to `state.out`; returns true once the input is fully
 * consumed.
 *
 * The pass carries NO state between tokens beyond `pos` — every pattern is
 * sticky and matched against the immutable `code` — so stopping and resuming is
 * invisible: for ANY sequence of chunk sizes the final `state.out` is
 * byte-identical to `highlight(code, lang)` (test/hi-chunked.test.ts proves it
 * over the language corpus, down to one token per slice). That is what lets a
 * renderer spread a big block's highlight across several tasks without changing
 * a byte of markup.
 *
 * @internal Not part of the semver surface — use {@link highlight}.
 */
export function stepHighlight(
  code: string,
  lang: string,
  state: HighlightState,
  chars: number,
  sink?: TokenSink,
): boolean {
  // Defense-in-depth: never tokenize a pathologically huge block — fall back to
  // plain escaped text. An unknown language is the same fallback. Both are
  // sliced through the same cursor, so even a 2 MB block escapes incrementally.
  const conf = code.length > 50_000 ? undefined : LANGS[lang.toLowerCase()];
  // `chars` is a floor, not a cap: the token straddling the boundary is emitted
  // whole. A non-positive budget would make no progress, so it counts as one.
  const stop = state.pos + (chars > 0 ? chars : 1);
  if (!conf) {
    const end = stop < code.length ? stop : code.length;
    state.out += escapeHtml(code.slice(state.pos, end));
    state.pos = end;
    return state.pos >= code.length;
  }

  let out = state.out;
  let pos = state.pos;
  const pats = conf.pats;
  const kw = conf.kw;
  // Linear pass with sticky regex tracking lastIndex.
  while (pos < code.length && pos < stop) {
    let matched = false;
    for (let i = 0; i < pats.length; i++) {
      const [cls, re] = pats[i];
      re.lastIndex = pos;
      const m = re.exec(code);
      // A zero-length match would make no progress and spin forever; no
      // built-in pattern can match empty (pinned by test) and registerLanguage
      // rejects tables that can, but a context-dependent zero-width match is
      // only detectable here.
      if (!m || m.index !== pos || m[0].length === 0) continue;
      const text = m[0];
      const after = pos + text.length;
      let finalCls = cls;
      if (cls === "ident") {
        if (kw && kw.has(text)) {
          finalCls = "kw";
        } else if (after < code.length && code[after] === "(") {
          finalCls = "fn";
        } else if (text.length > 1 && text[0] >= "A" && text[0] <= "Z") {
          finalCls = "ty";
        } else {
          // Plain identifier — no span needed.
          out += escapeHtml(text);
          if (sink) sink(cls, pos, after, out.length);
          pos = after;
          matched = true;
          break;
        }
      }
      if (cls === "ws") {
        out += text;
      } else {
        out += `<span class="t-${finalCls}">${escapeHtml(text)}</span>`;
      }
      if (sink) sink(cls, pos, after, out.length);
      pos = after;
      matched = true;
      break;
    }
    if (!matched) {
      // No pattern matched (shouldn't happen with a catch-all ws/other) — emit
      // one char as plain text to make progress.
      out += escapeHtml(code[pos]);
      if (sink) sink("", pos, pos + 1, out.length);
      pos += 1;
    }
  }
  state.out = out;
  state.pos = pos;
  return pos >= code.length;
}

export function highlight(code: string, lang: string): string {
  // One unbounded slice: `chars = code.length` reaches the end of the input on
  // the first call, so this is the same single linear pass it always was.
  const state: HighlightState = { pos: 0, out: "" };
  while (!stepHighlight(code, lang, state, code.length)) {
    /* a slice that big always finishes; the loop is belt-and-braces */
  }
  return state.out;
}

/**
 * The token classes a pattern may emit. Everything but `ws` and `ident` is
 * written out as `<span class="t-…">`, and each has a colour in styles.css;
 * `ws` passes through raw, and `ident` is the class that the language's keyword
 * set refines into `kw` / `fn` / `ty` (or into no span at all).
 */
const TOKEN_CLASSES = new Set([
  "kw", "str", "rx", "num", "lt", "com", "fn", "ty", "mac", "dec",
  "attr", "sel", "tag", "var", "pun", "txt", "ident", "ws",
]);

/** A language table for {@link registerLanguage}. */
export interface LanguageDef {
  /**
   * Ordered `[token class, sticky regex]` pairs. At each cursor position the
   * FIRST pattern that matches wins, so put the longer forms first. Every regex
   * must carry the `y` flag; it is matched against the whole source with
   * `lastIndex` at the cursor, so `^`/`$` (with `m`) and lookaheads work, and
   * `\b` sees the real neighbouring characters.
   */
  pats: Array<[token: string, re: RegExp]>;
  /** Words an `ident` token is promoted to `kw` for. Stored as a `Set`. */
  kw?: Iterable<string>;
}

/**
 * Teach {@link highlight} a language, under one name or several aliases.
 *
 *     registerLanguage(["hcl", "tf"], {
 *       pats: [
 *         ["com", /#.+/y],
 *         ["str", /"(?:\\.|[^"\\\n])*"/y],
 *         ["num", /\b\d+(?:\.\d+)?/y],
 *         ["ident", /\w+/y],
 *         ["pun", /[=[\]{}(),.]/y],
 *         ["ws", /\s+/y],
 *       ],
 *       kw: ["resource", "variable", "module", "output", "true", "false"],
 *     });
 *
 * Names are lower-cased, and registering a name that already exists REPLACES
 * its table — including a built-in one, which is how a caller retunes `yaml` or
 * `json` rather than living with this module's taste.
 *
 * Throws a `TypeError`, naming the offending pattern index, for a regex that is
 * not sticky or a token class that has no style.
 *
 * A registered language highlights exactly like a built-in one when its block
 * closes. While the block is still STREAMING it is re-tokenized from the top on
 * each patch instead of growing a frozen prefix: the frozen-prefix rule needs to
 * know which of a table's forms can run past a newline (a block comment, a
 * multi-line string), and a caller-supplied table does not say. The size cap in
 * the streaming path bounds that work; nothing about the settled markup differs.
 */
export function registerLanguage(names: string | string[], def_: LanguageDef): void {
  const list = typeof names === "string" ? [names] : names;
  if (!Array.isArray(list) || list.length === 0) {
    throw new TypeError("registerLanguage: expected a language name or a non-empty array of names");
  }
  // Validated up front, so a bad name in the middle of a list cannot leave the
  // table half-registered.
  for (const name of list) {
    if (typeof name !== "string" || name === "") {
      throw new TypeError("registerLanguage: every language name must be a non-empty string");
    }
  }
  if (def_ === null || typeof def_ !== "object" || !Array.isArray(def_.pats) || def_.pats.length === 0) {
    throw new TypeError("registerLanguage: `pats` must be a non-empty array of [token, regexp] pairs");
  }
  // Copied rather than kept by reference: the table is consulted on every token
  // of every block, and a caller mutating its array afterwards would rewrite
  // markup that has already been frozen mid-stream.
  const pats: Pat[] = [];
  for (let i = 0; i < def_.pats.length; i++) {
    const pair = def_.pats[i];
    if (!Array.isArray(pair) || pair.length < 2) {
      throw new TypeError(`registerLanguage: pattern ${i} must be a [token, regexp] pair`);
    }
    const [cls, re] = pair;
    if (typeof cls !== "string" || !TOKEN_CLASSES.has(cls)) {
      throw new TypeError(
        `registerLanguage: pattern ${i} has unknown token class ${JSON.stringify(cls)} — ` +
          `expected one of ${[...TOKEN_CLASSES].join(", ")}`,
      );
    }
    if (!(re instanceof RegExp) || !re.sticky) {
      throw new TypeError(`registerLanguage: pattern ${i} (${cls}) must be a sticky RegExp (the \`y\` flag)`);
    }
    // A pattern that matches the empty string would never advance the cursor.
    // The tokenizer also guards at match time (a zero-width match can be
    // context-dependent), but rejecting the obvious case here surfaces the
    // mistake at registration instead of as a silent no-op token.
    re.lastIndex = 0;
    if (re.exec("") !== null) {
      throw new TypeError(`registerLanguage: pattern ${i} (${cls}) matches the empty string`);
    }
    pats.push([cls, re]);
  }
  const kw = def_.kw === undefined ? undefined : new Set(def_.kw);
  // One entry shared by every alias, exactly as the built-in table does it.
  const entry: LangEntry = kw === undefined ? { pats } : { pats, kw };
  for (const name of list) {
    const key = name.toLowerCase();
    LANGS[key] = entry;
    REGISTERED.add(key);
  }
}

/** Every language {@link highlight} knows, built-in and registered alike. */
export function supportedLangs(): string[] {
  return Object.keys(LANGS);
}

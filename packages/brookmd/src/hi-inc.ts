import { escapeHtml, hasLang, isRegisteredLang, stepHighlight, type HighlightState, type TokenSink } from "./hi";

/**
 * Incremental syntax highlighting for an OPEN (streaming) code block.
 *
 * The one-shot highlighter re-tokenizes a whole block from byte 0, so
 * highlighting a growing fence on every patch is O(n²) — which is exactly why
 * open blocks used to render as plain escaped text and only lit up on close.
 * This module makes the open block linear instead, by splitting it in two:
 *
 * - a **frozen prefix** `[0, c)` whose markup (`frozenHtml`) is already final —
 *   appending bytes at EOF provably cannot change a byte of it — and
 * - a **speculative tail** `[c, EOF)`, re-tokenized from `c` on every patch.
 *
 * `c` only ever advances, so the frozen markup is a growing byte-PREFIX of the
 * markup the block will settle to. Work per patch is the tail, which a
 * checkpoint at the last newline keeps to roughly one source line.
 *
 * ## The tail is speculative — never freeze it
 *
 * A prefix of source does not tokenize like the same prefix of a longer source.
 * `const s = "hello` tokenizes the quote as a one-character fallback and `hello`
 * as an identifier; the next byte (`"`) merges them into a single `str` span.
 * `123.456e` is `num 123` plus four backtracked fragments until a digit arrives
 * and makes it one number. So the tail's COLOURS may change as bytes land, and
 * may differ from a one-shot of the same prefix. That is unavoidable, and it is
 * why only the part behind `c` is ever kept.
 *
 * ## Why a checkpoint survives an append
 *
 * `c` may advance to a position `p` that is
 *
 * 1. immediately after a `\n` **inside a `ws` token**. Whitespace is emitted
 *    RAW (hi.ts, the `cls === "ws"` branch), so cutting a whitespace run in two
 *    is byte-safe: the frozen half contributes `code.slice(s, p)` and the
 *    resumed run re-matches `\s+` from `p`. And a newline terminates every
 *    line-bounded pattern in every language table (`//…`, `#…`, `"…"`, `/…/`,
 *    and every word-class run: `ident`, `num`, `sel`, `dec`, `lt`, `mac`), so
 *    none of them can be rewritten by bytes arriving after it.
 * 2. at least {@link GAP} bytes before the current EOF. This is what covers the
 *    small fixed-width lookaheads: Rust's `b?'x'` matches a character literal
 *    across a newline in a 3–4 byte window, `\b` at a number's end inspects the
 *    byte past it, and an `ident` peeks one byte ahead for `(`. Frozen tokens
 *    end at `p ≤ EOF − GAP`, so every such window is fully inside settled bytes.
 * 3. with **no unbounded opener live** at any token start before it — a block
 *    comment, template literal, Python triple-quote, Rust multi-line string or
 *    `#[…]`, SQL quoted literal, Bash string or `${…}`, Go raw string, HTML
 *    comment, or HTML/CSS attribute string that has not met its terminator.
 *    Those are precisely the forms whose match scans past a newline to EOF, so
 *    a later byte can turn a run of fallback characters into one span.
 *
 * **HTML/XML is the exception.** Its table has no `ws` pattern at all: run-of-
 * text is `txt` (`[^<>"'=]+`), which spans newlines AND emits a span, so cutting
 * inside one would split `<span …>` down the middle. There, `>` plays the role
 * `\n` plays elsewhere — it terminates `txt`, `tag` and `attr` alike — so the
 * checkpoint goes immediately after a `>` `pun` token, a real token boundary.
 *
 * ## Per patch
 *
 * 1. An opener is live → run only its continuation predicate over only the NEW
 *    bytes. Each predicate carries O(1) state and answers one question ("has the
 *    terminator arrived?"); it never emits markup. The tail renders as plain
 *    escaped text until the opener closes and a real scan can resume.
 * 2. Otherwise re-tokenize `[c, EOF)`, show `frozenHtml + tail`, then advance
 *    `c` to the last eligible checkpoint and extend `frozenHtml`.
 * 3. If the unfrozen tail is over {@link CAP} bytes, skip the re-scan entirely
 *    and render it plain. Correctness is unaffected: the close-time run always
 *    starts from `c`.
 * 4. Past {@link CLIFF} the whole state is discarded and the block falls back to
 *    today's plain-escape behaviour exactly (`stepHighlight` escapes rather than
 *    tokenizes a block that big, so a seeded resume would be meaningless).
 *
 * ## The source is not append-only
 *
 * The core speculatively closes an open fence on every patch, so a partial last
 * line arrives newline-terminated: `"expo\n"` then `"export fu\n"`. The previous
 * text is therefore a prefix of the next one only by accident, and the obvious
 * `startsWith` guard would discard the frozen prefix on every patch. What the
 * frozen markup depends on is bounded — nothing behind `c` looks past `c + GAP`
 * — so a revision at or beyond that keeps it. See {@link adopt}.
 *
 * On close the renderer hands {@link incSeed}'s `{pos, out}` to
 * `highlightWithin`/`highlightDeferred`, which resumes the ordinary tokenizer at
 * `c`. The settled bytes are therefore identical to `highlight(text, lang)` by
 * construction, and test/hi-inc.test.ts proves it over randomized chunkings of
 * every supported language.
 */

/** Bytes of settled input kept between the last checkpoint and EOF (see 2 above). */
const GAP = 3;
/** Unfrozen tail size past which a patch stops tokenizing and renders plain. */
const CAP = 8192;
/** `stepHighlight`'s own size guard: past this a block is escaped, not tokenized. */
const CLIFF = 50_000;

/** Continuation-predicate result. */
const OPEN = 0;
const CLOSED = 1;
/** The opener can never close — its pattern has already failed permanently. */
const DEAD = 2;

/**
 * How an unbounded opener recognizes its terminator, with the O(1) state each
 * one carries between patches:
 *
 * - `char` — the first occurrence of `ch` closes (`[^ch]*ch` forms: Go raw
 *   strings, HTML/CSS attribute strings, Rust `#[…]`, Bash `'…'` and `${…}`).
 * - `starSlash` — `prevStar`, for `/* … *\/`.
 * - `dashGt` — `dashRun`, for `<!-- … -->`.
 * - `esc` — `esc`, for backslash-escaped strings and template literals. A
 *   backslash immediately before a newline is {@link DEAD}, not an escape: the
 *   pattern's `\\.` does not match a newline, so the form can never close.
 * - `triple` — `runLen`, for Python `"""` / `'''`.
 * - `dbl` — `pendingQuote`, for SQL's doubled-quote escaping (`'a''b'`).
 */
type PredKind =
  | { k: "char"; ch: string }
  | { k: "starSlash" }
  | { k: "dashGt" }
  | { k: "esc"; ch: string }
  | { k: "triple"; ch: string }
  | { k: "dbl"; ch: string };

/** @internal */
export interface Opener {
  /** Sticky regex for the OPENING delimiter only — bounded, O(1) to test. */
  re: RegExp;
  /** The pattern class the tokenizer emits when this form IS terminated. */
  cls: string;
  /** The literal terminator. */
  end: string;
  pred: PredKind;
}

/** @internal The carried state of one opener's continuation predicate. */
export interface OpenerScanState {
  n: number;
  f: boolean;
}

const BLOCK_COMMENT: Opener = { re: /\/\*/y, cls: "com", end: "*/", pred: { k: "starSlash" } };

// Only forms that can scan PAST A NEWLINE to EOF belong here. Everything else in
// the language tables is line-bounded (`"…"` in js/go/py/json, `/…/`, `//…`,
// `#…`, `--…`) or fixed-width (Rust `b'x'`), and is covered by the newline and
// GAP conditions instead.
const JS_OPENERS: Opener[] = [
  BLOCK_COMMENT,
  { re: /`/y, cls: "str", end: "`", pred: { k: "esc", ch: "`" } },
];
const RUST_OPENERS: Opener[] = [
  BLOCK_COMMENT,
  { re: /b?"/y, cls: "str", end: '"', pred: { k: "esc", ch: '"' } },
  { re: /#!?\[/y, cls: "attr", end: "]", pred: { k: "char", ch: "]" } },
];
const PY_OPENERS: Opener[] = [
  { re: /[fFrRbB]{0,2}"""/y, cls: "str", end: '"""', pred: { k: "triple", ch: '"' } },
  { re: /[fFrRbB]{0,2}'''/y, cls: "str", end: "'''", pred: { k: "triple", ch: "'" } },
];
const GO_OPENERS: Opener[] = [
  BLOCK_COMMENT,
  { re: /`/y, cls: "str", end: "`", pred: { k: "char", ch: "`" } },
];
const BASH_OPENERS: Opener[] = [
  { re: /"/y, cls: "str", end: '"', pred: { k: "esc", ch: '"' } },
  { re: /'/y, cls: "str", end: "'", pred: { k: "char", ch: "'" } },
  { re: /\$\{/y, cls: "var", end: "}", pred: { k: "char", ch: "}" } },
];
const SQL_OPENERS: Opener[] = [
  BLOCK_COMMENT,
  { re: /'/y, cls: "str", end: "'", pred: { k: "dbl", ch: "'" } },
  { re: /"/y, cls: "str", end: '"', pred: { k: "dbl", ch: '"' } },
];
const HTML_OPENERS: Opener[] = [
  { re: /<!--/y, cls: "com", end: "-->", pred: { k: "dashGt" } },
  { re: /"/y, cls: "str", end: '"', pred: { k: "char", ch: '"' } },
  { re: /'/y, cls: "str", end: "'", pred: { k: "char", ch: "'" } },
];
const CSS_OPENERS: Opener[] = [
  BLOCK_COMMENT,
  { re: /"/y, cls: "str", end: '"', pred: { k: "char", ch: '"' } },
  { re: /'/y, cls: "str", end: "'", pred: { k: "char", ch: "'" } },
];

// java / c / c++ / c# / swift / kotlin / php. Their strings are all line-bounded
// (`[^"\\\n]`), so the block comment is the only form that can run to EOF.
const C_OPENERS: Opener[] = [BLOCK_COMMENT];

/** Nothing in the table can run past a newline (yaml, toml, diff, ruby, …). */
const NO_OPENERS: Opener[] = [];

// Mirrors the LANGS table in hi.ts — every entry there needs one here (a test
// asserts the two stay in step, so a new language cannot silently stream wrong).
const OPENERS: Record<string, Opener[]> = {
  js: JS_OPENERS,
  javascript: JS_OPENERS,
  ts: JS_OPENERS,
  tsx: JS_OPENERS,
  jsx: JS_OPENERS,
  typescript: JS_OPENERS,
  rust: RUST_OPENERS,
  rs: RUST_OPENERS,
  py: PY_OPENERS,
  python: PY_OPENERS,
  go: GO_OPENERS,
  bash: BASH_OPENERS,
  sh: BASH_OPENERS,
  shell: BASH_OPENERS,
  json: [],
  sql: SQL_OPENERS,
  html: HTML_OPENERS,
  xml: HTML_OPENERS,
  css: CSS_OPENERS,
  java: C_OPENERS,
  c: C_OPENERS,
  cpp: C_OPENERS,
  "c++": C_OPENERS,
  cs: C_OPENERS,
  csharp: C_OPENERS,
  swift: C_OPENERS,
  kt: C_OPENERS,
  kotlin: C_OPENERS,
  php: C_OPENERS,
  rb: NO_OPENERS,
  ruby: NO_OPENERS,
  yaml: NO_OPENERS,
  yml: NO_OPENERS,
  toml: NO_OPENERS,
  diff: NO_OPENERS,
  dockerfile: NO_OPENERS,
};

/** Languages whose checkpoint goes after a `>` instead of after a `\n` (no `ws` pattern). */
const GT_CHECKPOINT = new Set(["html", "xml"]);

/** Per-block incremental state. Create with {@link createInc}, feed {@link incHighlight}. */
export interface IncState {
  /** The language key this state's tables were chosen for. */
  readonly lang: string;
  /** The unbounded openers of `lang` — resolved once, at {@link createInc}. */
  readonly openers: Opener[];
  /**
   * May this state freeze a prefix at all?
   *
   * False for a language added through `registerLanguage`: the checkpoint rule
   * is derived from knowing which of a table's forms can run past a newline, and
   * a caller-supplied table does not say. Such a block is re-tokenized from the
   * top on each patch (the {@link CAP} bound still applies) and freezes nothing,
   * which is the one setting that is safe for ANY pattern list — with `c` at 0
   * there is no frozen byte to be wrong, and the close-time run is unseeded.
   */
  readonly freeze: boolean;
  /** Settled through here: `[0, c)` of the source will never re-tokenize. */
  c: number;
  /** Markup for `[0, c)`. Always a byte-prefix of the block's final markup. */
  frozenHtml: string;
  /**
   * Bumped whenever `frozenHtml` is TRUNCATED or cleared — the one checkpoint of
   * rewind {@link adopt} performs, or a restart. It never moves while the prefix
   * merely GROWS.
   *
   * A renderer that mirrors the frozen prefix into the DOM append-only reads it
   * as the "may I splice?" token: same rev ⇒ the prefix only grew, so appending
   * `frozenHtml.slice(alreadyWritten)` is exact; a changed rev means the prefix
   * was rewritten underneath and the mirror must be re-seeded. Length alone is
   * NOT enough — one call can rewind to `c0` and then re-freeze past the old
   * length, which looks like a plain append but is not one.
   */
  frozenRev: number;
  /**
   * The length `frozenHtml` was TRUNCATED to at the last {@link frozenRev} bump:
   * `frozenLen0` for {@link adopt}'s one-checkpoint rewind, `0` for a restart.
   *
   * The rev alone says "rewritten"; this says *from where*, which is what lets a
   * DOM mirror rewind to a boundary it already holds instead of re-seeding the
   * whole run. It is load-bearing that this is reported rather than inferred:
   * `adopt` frequently rewinds and then re-freezes PAST the old length within
   * the same call, so the observable `frozenHtml.length` can come back unchanged
   * while its bytes have moved.
   */
  frozenCut: number;
  /** The checkpoint BEFORE `c`, and the `frozenHtml` length that went with it —
   *  the one step of rewind a tail revision needs (see {@link adopt}). */
  c0: number;
  frozenLen0: number;
  /** The unbounded opener live at the tail, or `null`. */
  opener: Opener | null;
  /** `opener`'s carried predicate state. */
  scan: OpenerScanState;
  /** `opener` can never close — stop re-scanning and render the tail plain. */
  sealed: boolean;
  /** The source last fed in, for the append/revision guard. */
  text: string;
  /**
   * `text.slice(0, c + GAP)` — the settled source the frozen markup was derived
   * from, and {@link adopt}'s whole question in one string: a revision keeps the
   * prefix iff the new text still STARTS WITH this.
   *
   * Cached rather than re-sliced because it changes only when `c` does (once per
   * checkpoint, so once per source line) while the question is asked on every
   * patch. Asking it as `text.startsWith(pfx)` hands the compare to the engine's
   * native string compare; the char-by-char loop it replaces was, on a streamed
   * 32 KB fence, 373 ms of the 480 ms this module spent in total — ten times
   * what the same question costs natively.
   */
  pfx: string;
  /** The same, for the checkpoint BEFORE `c` — {@link adopt}'s second question. */
  pfx0: string;
  /** Escaped source for `[c, plainUpto)` — the plain tail, extended in place. */
  plain: string;
  plainFrom: number;
  plainUpto: number;
  /** The markup last handed out, so a repeated feed of the same text is free. */
  html: string | null;
}

/**
 * State for a block in `lang`, or `null` when the language has no table (the
 * plain-escape fallback has no token boundaries to checkpoint on).
 */
export function createInc(lang: string): IncState | null {
  const key = lang.toLowerCase();
  // A built-in name the caller REPLACED through registerLanguage must not keep
  // the built-in opener table: that table describes the original patterns'
  // unbounded forms, and freezing on it against a different table would pin
  // wrong markup for the rest of the stream.
  const known = Object.prototype.hasOwnProperty.call(OPENERS, key) && !isRegisteredLang(key);
  // A registered language has a tokenizer table but no opener table, so it runs
  // in the conservative never-freeze mode above. A language with neither has no
  // token boundaries at all and opts out entirely.
  if (!known && !hasLang(key)) return null;
  return {
    lang: key,
    openers: known ? OPENERS[key] : NO_OPENERS,
    freeze: known,
    c: 0,
    frozenHtml: "",
    frozenRev: 0,
    frozenCut: 0,
    c0: 0,
    frozenLen0: 0,
    opener: null,
    scan: { n: 0, f: false },
    sealed: false,
    text: "",
    pfx: "",
    pfx0: "",
    plain: "",
    plainFrom: 0,
    plainUpto: 0,
    html: null,
  };
}

/**
 * Salvage the frozen prefix across a text change that is NOT an append. Returns
 * false when nothing survives.
 *
 * This is not an edge case: a streaming fence's source is not append-only. The
 * core speculatively CLOSES the block on every patch, which terminates a partial
 * last line with a newline — so `"expo\n"` is followed by `"export fu\n"`, and
 * the previous text is a prefix of the next one only by accident. Resetting on
 * that (the plain `startsWith` guard) would throw the frozen prefix away on
 * every single patch and put the whole block back to O(n²).
 *
 * What the frozen markup actually depends on is the source it was DERIVED from,
 * and that reaches no further than `c + GAP`: the newline at `c - 1` terminates
 * every line-bounded and word-class pattern behind it, unbounded openers behind
 * it were checked for their terminators, and GAP is exactly the reserve the
 * fixed-width lookaheads need. So the prefix survives any revision at or after
 * `c + GAP`, and one checkpoint of rewind covers the off-by-a-line case where
 * the revision lands just inside it.
 *
 * "The revision is at or after `c + GAP`" is exactly "the new text still starts
 * with the old one's first `c + GAP` bytes", which is what {@link IncState.pfx}
 * holds — so the question is one native `startsWith` rather than a scan for the
 * divergence point, whose exact position was never wanted for anything else.
 */
function adopt(st: IncState, text: string): boolean {
  if (st.c > 0 && text.startsWith(st.pfx)) {
    dropTail(st);
    return true;
  }
  if (st.c0 > 0 && text.startsWith(st.pfx0)) {
    st.frozenHtml = st.frozenHtml.slice(0, st.frozenLen0);
    st.frozenRev++; // the prefix SHRANK — an append-only mirror must rewind
    st.frozenCut = st.frozenLen0;
    st.c = st.c0;
    st.pfx = st.pfx0;
    st.c0 = 0;
    st.pfx0 = "";
    st.frozenLen0 = 0;
    dropTail(st);
    return true;
  }
  return false;
}

/** Forget everything about the unfrozen tail; `[0, c)` stays. */
function dropTail(st: IncState): void {
  st.opener = null;
  st.scan = { n: 0, f: false };
  st.sealed = false;
  st.plain = "";
  st.plainFrom = st.c;
  st.plainUpto = st.c;
  st.html = null;
}

function reset(st: IncState): void {
  st.c = 0;
  st.frozenHtml = "";
  // Monotonic, never zeroed: a reset back to an equal-length prefix must still
  // read as "rewritten" to a mirror that only remembers the length.
  st.frozenRev++;
  st.frozenCut = 0;
  st.c0 = 0;
  st.frozenLen0 = 0;
  st.pfx = "";
  st.pfx0 = "";
  st.opener = null;
  st.scan = { n: 0, f: false };
  st.sealed = false;
  st.text = "";
  st.plain = "";
  st.plainFrom = 0;
  st.plainUpto = 0;
  st.html = null;
}

/**
 * Feed the block's CURRENT full source and get the markup for all of it, or
 * `null` when the incremental path has bowed out (past {@link CLIFF}) and the
 * caller should render the plain escaped body exactly as it does today.
 *
 * Calls must be append-only. Anything else — a `reset()`, a speculative tail
 * revision, one block's id being reused for different content — is detected
 * (`text.startsWith(prev)`, the same guard the DOM renderer's prefix-append fast
 * path uses) and simply restarts the state from scratch.
 */
export function incHighlight(st: IncState, text: string): string | null {
  if (st.text === text) return st.html;
  // Both guards are native prefix compares, not a scan for the exact divergence
  // point: the only two things anyone asked of it were "is this a plain append?"
  // and "did the revision land at or after `c + GAP`?".
  const appended = text.length > st.text.length && text.startsWith(st.text);
  let from = st.text.length;
  if (!appended) {
    // Not a plain append. Keep as much of the frozen prefix as the revision
    // leaves untouched; anything still live in the tail is thrown away.
    if (!adopt(st, text)) reset(st);
    from = 0;
  }
  st.text = text;

  // (4) The cliff. `stepHighlight` escapes rather than tokenizes a block this
  // big, so a frozen prefix of token spans could never be resumed into it.
  if (text.length > CLIFF) {
    reset(st);
    st.text = text;
    return null;
  }

  // (1) A live opener: look at the new bytes, and only for the terminator.
  const live = st.opener as Opener | null;
  if (live !== null) {
    const r = st.sealed ? OPEN : feed(live.pred, st.scan, text, from, text.length);
    if (r === DEAD) st.sealed = true;
    if (r !== CLOSED) {
      st.html = st.frozenHtml + plainTail(st, text);
      return st.html;
    }
    st.opener = null;
  }

  // (3) Too much unfrozen tail to re-tokenize this patch.
  if (text.length - st.c > CAP) {
    st.html = st.frozenHtml + plainTail(st, text);
    return st.html;
  }

  // (2) Tail render, then re-freeze.
  st.html = rescan(st, text);
  return st.html;
}

/**
 * The `{pos, out}` a close-time `highlightWithin`/`highlightDeferred` run should
 * resume from, or `undefined` when nothing was frozen or the state does not
 * belong to `text`/`lang` (a revised block, a different language, a block that
 * crossed the cliff). Then the close-time run is the unseeded one it always was.
 */
export function incSeed(st: IncState, text: string, lang: string): HighlightState | undefined {
  if (st.c === 0 || st.lang !== lang.toLowerCase()) return undefined;
  if (text.length > CLIFF || text.length < st.c) return undefined;
  // The frozen markup describes these bytes; if the block was revised rather
  // than appended to, it describes bytes that are no longer there.
  if (!text.startsWith(st.text.slice(0, st.c))) return undefined;
  return { pos: st.c, out: st.frozenHtml };
}

// Bytes handed to the tokenizer, summed over every patch. The O(n) regression
// guard asserts against it; `__`-prefixed and never read in production.
let scanned = 0;
/** @internal Test-only: total source bytes re-tokenized since the last reset. */
export function __getIncScanned(): number {
  return scanned;
}
/** @internal Test-only. */
export function __resetIncScanned(): void {
  scanned = 0;
}

/**
 * The plain escaped tail, extended by the appended bytes rather than rebuilt.
 * `escapeHtml` is context-free, so `escape(a + b) === escape(a) + escape(b)` and
 * a patch costs O(new bytes) even while an opener holds the tokenizer off.
 */
function plainTail(st: IncState, text: string): string {
  if (st.plainFrom !== st.c || st.plainUpto > text.length) {
    st.plainFrom = st.c;
    st.plain = "";
    st.plainUpto = st.c;
  }
  if (st.plainUpto < text.length) {
    st.plain += escapeHtml(text.slice(st.plainUpto));
    st.plainUpto = text.length;
  }
  return st.plain;
}

/**
 * Re-tokenize `[c, EOF)`, collecting the last eligible checkpoint and the first
 * live opener as the tokens go by, then freeze up to that checkpoint. Returns
 * the markup for the WHOLE block (frozen prefix as it was on entry + the tail).
 */
function rescan(st: IncState, text: string): string {
  const openers = st.openers;
  const gt = GT_CHECKPOINT.has(st.lang);
  const limit = text.length - GAP;
  // The best checkpoint so far, and the tail-output length that goes with it.
  let cp = -1;
  let cpOut = 0;
  // The FIRST live opener in the tail. Nothing at or past it can be frozen, so
  // once one is found the sink stops looking at anything else.
  let liveAt = -1;
  let liveOp: Opener | null = null;
  let liveLen = 0;

  const sink: TokenSink = (cls, start, end, outLen) => {
    if (liveAt >= 0) return;
    if (!st.freeze) return; // never-freeze mode: no checkpoint, no opener to pin
    for (let i = 0; i < openers.length; i++) {
      const op = openers[i];
      op.re.lastIndex = start;
      const m = op.re.exec(text);
      if (m === null || m.index !== start) continue;
      // The opener is terminated iff the winning token IS that form and reaches
      // its terminator: opener + terminator bytes at least, ending in the
      // terminator. Python's `f""` (from the single-quote pattern) fails this
      // against the `f"""` opener, which is exactly the point.
      const terminated =
        cls === op.cls &&
        end - start >= m[0].length + op.end.length &&
        text.startsWith(op.end, end - op.end.length);
      if (terminated) continue;
      liveAt = start;
      liveOp = op;
      liveLen = m[0].length;
      return;
    }
    if (gt) {
      // HTML/XML: immediately after a `>`, a real token boundary that every
      // bounded pattern in the table stops at.
      if (cls === "pun" && end - start === 1 && text.charCodeAt(start) === 62 && end <= limit) {
        cp = end;
        cpOut = outLen;
      }
      return;
    }
    if (cls !== "ws") return;
    // The last newline in this whitespace run that still leaves GAP bytes of
    // settled input behind it. Whitespace is emitted raw, so the output length
    // at the cut is the token's end minus the bytes we are giving back.
    const hi = end < limit ? end : limit;
    if (hi <= start) return;
    const nl = text.lastIndexOf("\n", hi - 1);
    if (nl < start) return;
    cp = nl + 1;
    cpOut = outLen - (end - cp);
  };

  const state: HighlightState = { pos: st.c, out: "" };
  scanned += text.length - st.c;
  // A budget of the whole input finishes in one pass, exactly as `highlight()`
  // does; the loop is the same belt-and-braces.
  while (!stepHighlight(text, st.lang, state, text.length, sink)) {
    /* one slice that big always completes */
  }
  const full = st.frozenHtml + state.out;

  if (cp > st.c) {
    st.c0 = st.c;
    st.pfx0 = st.pfx;
    st.frozenLen0 = st.frozenHtml.length;
    st.frozenHtml += state.out.slice(0, cpOut);
    st.c = cp;
    // `cp <= text.length - GAP` by construction, so this is a full-width prefix
    // and `pfx.length === c + GAP` — the invariant `adopt` reads it under.
    st.pfx = text.slice(0, cp + GAP);
  }

  const found = liveOp as Opener | null;
  if (found === null) {
    st.opener = null;
  } else {
    st.scan = { n: 0, f: false };
    const r = feed(found.pred, st.scan, text, liveAt + liveLen, text.length);
    // The predicate is an approximation of the pattern, never the other way
    // round: if it thinks the form already closed, the pattern disagrees and we
    // simply keep re-scanning rather than trusting either one.
    st.opener = r === CLOSED ? null : found;
    st.sealed = r === DEAD;
  }
  return full;
}

/** Consume `s[from, to)` looking only for `p`'s terminator. */
function feed(p: PredKind, st: OpenerScanState, s: string, from: number, to: number): number {
  switch (p.k) {
    case "char": {
      const i = s.indexOf(p.ch, from);
      return i >= 0 && i < to ? CLOSED : OPEN;
    }
    case "starSlash": {
      for (let i = from; i < to; i++) {
        const ch = s[i];
        if (st.f && ch === "/") return CLOSED;
        st.f = ch === "*";
      }
      return OPEN;
    }
    case "dashGt": {
      for (let i = from; i < to; i++) {
        const ch = s[i];
        if (ch === ">" && st.n >= 2) return CLOSED;
        st.n = ch === "-" ? st.n + 1 : 0;
      }
      return OPEN;
    }
    case "esc": {
      for (let i = from; i < to; i++) {
        const ch = s[i];
        if (st.f) {
          st.f = false;
          // `\\.` does not match a newline, and no other alternative can consume
          // the backslash — the form is unclosable from here on.
          if (ch === "\n") return DEAD;
          continue;
        }
        if (ch === "\\") {
          st.f = true;
          continue;
        }
        if (ch === p.ch) return CLOSED;
      }
      return OPEN;
    }
    case "triple": {
      for (let i = from; i < to; i++) {
        if (s[i] === p.ch) {
          st.n++;
          if (st.n === 3) return CLOSED;
        } else {
          st.n = 0;
        }
      }
      return OPEN;
    }
    case "dbl": {
      for (let i = from; i < to; i++) {
        const ch = s[i];
        if (st.f) {
          st.f = false;
          // A doubled quote is an escape; a lone one closed the literal.
          if (ch !== p.ch) return CLOSED;
          continue;
        }
        if (ch === p.ch) st.f = true;
      }
      return OPEN;
    }
  }
}

# Changelog

Notable changes to brookmd (formerly `flux-md`). Format based on
[Keep a Changelog](https://keepachangelog.com/); this project aims to follow
[Semantic Versioning](https://semver.org/).

## 0.30.0 — 2026-09-07

**Agent-ready, chat-ready.** brookmd now ships as an installable agent skill,
and the surface a chat UI needs on day one — a styled theme for the code
chrome, a streaming caret, a link-click hook, a highlighter you can teach new
languages, and copy-pasteable recipes for the AI SDK, KaTeX, Mermaid, and
table toolbars — is in the package and the docs instead of the demo app.
Requires `brookmd-core` 0.27.0 (parser-side performance fix below; the wire
contract is unchanged at v1.2.0).

### Added

- **Agent skill.** `npx skills add siinghd/brookmd` installs a `brookmd` skill
  into any coding agent that reads the `SKILL.md` format (Claude Code, Cursor,
  Codex, …); `/plugin marketplace add siinghd/brookmd` does the same through
  Claude Code's plugin marketplace. The skill (`skills/brookmd/`) carries a
  trigger-listing description, quick setup, the chat-UI defaults, a props
  table with real defaults, twelve gotchas, six reference documents (API,
  frameworks, styling, security, recipes, troubleshooting) and seven typed
  examples. `llms.txt` at the repo root and `llms.txt` / `llms-full.txt` on
  the demo site give agents an index without cloning. Every API fact in the
  skill was verified against source, not the README — and stays that way: CI
  typechecks the examples against `src/`, and `test/skill-docs.test.ts` fails
  on malformed frontmatter, a dead relative link, an import of a
  `brookmd/<subpath>` the package does not export, or a public doc that names
  another rendering library.
- **`onLinkClick`** on `<BrookMarkdown>` (React), `mountBrookMarkdown`
  (`MountOptions`), the Vue / Svelte / Solid bindings and the
  `<brook-markdown>` element (`.onLinkClick` property): one delegated listener
  on the `.brook-md` root hands back `(event, { href, text, element })`
  (`LinkClickInfo`, exported from `brookmd`, `brookmd/types` and
  `brookmd/dom`); `event.preventDefault()` cancels navigation, so link
  interstitials and in-app routing need no per-anchor override. Links still
  streaming their URL (`<a data-brook-pending>`) are never reported. The hook
  adds zero per-block or per-anchor work — a changing handler identity
  re-renders no blocks (pinned by test).
- **`registerLanguage(names, { pats, kw })`** on `brookmd/highlight` (also
  re-exported from `brookmd`): teach the built-in streaming highlighter a
  language at runtime, under one name or several aliases. Regexes must be
  sticky, token classes must be ones the stylesheet colours (`ident` is the
  class a `kw` set refines), and re-registering a name replaces it. A
  registered language highlights identically to a built-in one once its block
  closes; while streaming it re-tokenizes instead of growing a frozen prefix,
  since a caller's table does not say which of its forms can run past a
  newline.
- **Twelve more built-in language families**: yaml/yml, toml, diff, java, c,
  cpp/c++, cs/csharp, php, rb/ruby, swift, kt/kotlin and dockerfile — 36
  language keys in total. Language lookup is now a prototype-free table, so a
  fence tagged `constructor` or `__proto__` is an ordinary miss. The ReDoS
  suite runs 17 adversarial inputs against *every* supported language under a
  per-input time budget, so a new table cannot land a quadratic pattern.
- **The theme finally styles the fenced-block chrome.** `brookmd/styles.css`
  now gives the built-in code, math and mermaid renderers a bordered
  container, a header bar with the language label, a ghost copy button (hover,
  focus ring, copied state) and a pulsing "streaming" pill — previously the
  theme left a bare button stack above every fence. Also new: GitHub-style
  alerts for all five kinds (`--brook-alert-*` variables in light and dark),
  footnote sections and references, block/inline math, disabled task-list
  checkboxes. A contract test collects every `brook-*` class the renderers
  emit and fails if the theme does not style it.
- **Opt-in streaming caret.** Add `brook-caret` to the root
  (`<BrookMarkdown className="brook-caret" />`) and the block still streaming
  gets a blinking cursor at its tail (`--brook-caret` retints it). Fences show
  the streaming pill instead. Both animations route through
  `--brook-caret-anim` / `--brook-pill-anim` and switch off under
  `prefers-reduced-motion: reduce`.
- **Block-state classes are now a documented, stable styling contract**:
  `brook-block`, `brook-block-<kind>`, `brook-open`, `brook-speculative`, and
  `brook-streaming` on the code/math/mermaid slots.
- `<brook-markdown>` honours the **`stick-to-bottom`** and **`virtualize`**
  attributes the README had advertised; changing either after mount re-applies
  it, including with a caller-owned client.

### Fixed

- **Streaming parity: a table delimiter row still being typed no longer
  freezes the paragraph above it** (`brookmd-core` 0.27.0). With
  `| name | value |` / `value |` / `|:-----|--`, the moment the buffer held
  `|:-` the parser saw a one-column delimiter under the one-column line
  `value |`, formed a table, and committed `<p>| name | value |</p>` as a
  separate block; one byte later the delimiter widened, the table dissolved,
  but the commit could not be taken back — so the finalized document differed
  from the one-shot parse (one paragraph). A table whose delimiter row sits on
  the buffer's unterminated final line is now held speculative until that line
  completes, matching the existing guard for provisionally classified blocks.
  Found by the coverage-guided parity fuzzer; pinned by a regression test
  across every chunking of 1–8 bytes; a 240 s fuzz run afterwards found
  nothing new.
- The generic block wrapper was eating the theme's spacing rules: the last
  block's margin escaped the root and the first heading sat indented from the
  top. `.brook-bottom-anchor` (the `stickToBottom` sentinel) no longer occupies
  a full block gap of dead space at the end of the document.
- README corrections, each verified against source: `<brook-markdown
  stick-to-bottom>` (now real — see above), the root class is `brook-md` (not
  `brookmd`), `new BrookClient({ pool })` (not `new BrookClient(pool)`),
  `MountHandle` also has `openBlockId()`, the real `Components` and
  `BlockComponentProps` declarations, the Blockquote/Alert `{ nested }` →
  `props.container` and `ListData.items` / `ListItemData.start` rows in the
  `blockData` table, and the previously undocumented surface — `deferTail`,
  `childMemo`, `onRenderMetrics`, `onStreamError`, `streamConfig`, `coalesce`,
  `reattach()`, `retainCommittedHtml`, `bootTimeoutMs`, `supportedLangs()`,
  and the `brookmd/types`, `brookmd/html-to-react`, `brookmd/block-props`,
  `brookmd/worker-core` entry points. `SHIPPING.md` no longer claims the
  package ships as source (compiled ESM since 0.17.0).
- `brookmd-react-native` now depends on `brookmd ^0.30.0` (it pinned `^0.26.0`,
  which a 0.x caret resolves to `<0.27`, so its workspace install had been
  fetching an old brookmd from the registry instead of the sibling package).

### Performance

- **Open blockquote / alert / component bodies keep a settled HTML prefix**
  (`brookmd-core` 0.27.0). The assembler for a structured container or a
  component block used to re-walk every committed inner sub-block and re-copy
  its HTML on *every* append — the residual wall-clock cliff documented since
  0.28.0. Each committed sub-block is now folded into a per-cache prefix once
  (one prefix per nested-parser twin on the component path, since the settled
  twin's committed HTML differs) and re-emitted as one contiguous copy; only
  the active tail is rebuilt. A streaming component body of paragraphs is
  1.9–5.3× faster at 256 KB (2,048-paragraph body 152 ms → 66 ms; dense
  small-paragraph body 571 ms → 107 ms, growth 34× → 11× over 32→256 KB).
  Output is byte-identical: 652/652 CommonMark + 24/24 GFM byte-exact, every
  parity suite green, and a differential harness hashing the full document
  after every append found no difference from the previous implementation
  across 896 hand-written cases and 20,000 randomized container/component
  documents × 4 chunkings. A new wall-clock scaling guard
  (`wrapper_body_prefix_is_wall_linear`, control-twin primary, retry-once)
  pins the shape — the work counters are structurally blind to this class —
  and was verified to fail on the pre-fix tree. Blockquote/alert bodies whose
  nested parser commits nothing (one giant list) see no change; their residual
  cost is the two unavoidable O(body) `Block.html` materializations per append.

### Docs

- New README sections written for someone building a chat UI: **Chat UI
  defaults** (`softBreaks`, `dirAuto`, `a11y`, `blockData`, `gfmMath`, hoisted
  `components`, `warm()`, the caret), **With the Vercel AI SDK (`useChat`)**
  including the `streaming: false`-on-finish rule and why brookmd refuses to
  infer it, **Accessible chat**, **Tailwind / design systems** (and why
  `@tailwindcss/typography` users should skip the theme import), and recipes
  for KaTeX, Mermaid, a custom highlighter, interactive task lists, lazy
  images, and a CSV/copy table toolbar over `props.table`.

## 0.29.1 — 2026-07-31

Performance only; rendered bytes unchanged (asserted: settled markup identical
with streaming highlight on and off, and mid-stream parity suites untouched).

### Performance

- **Two quadratic terms removed from the streaming-highlight hot path.** An
  external benchmark of 0.27 showed streaming highlighting costing +36%
  main-thread time on a code-heavy stream. Most of that was the 0.27 DOM
  write pattern, already fixed in 0.28 (the on/off DOM-write delta is 151×
  smaller at this release). Decomposing what remained found two genuinely
  quadratic terms: the revision guard re-scanned the block character-by-
  character on *every* patch to find a divergence point whose exact value
  was never used (~2,070× the source in `charCodeAt` calls — replaced with
  two `startsWith` questions against a cached prefix), and the fence body
  was re-decoded out of its rendered HTML on every patch through a lazy
  regex plus five unconditional entity passes (~5,169× the source — replaced
  with landmark indexing and an `&`-gated entity chain). Streaming a 32 KB
  code block, highlight-only JS cost drops **782 ms → 163 ms**, and both
  terms now scale linearly. A scale-free regression gate
  (`test/streaming-highlight-cost.test.ts`) pins ratios against source and
  markup size — verified to fail on the pre-fix tree.
- Evaluated and rejected on measurement: computing the highlight in the
  worker (the worker forwards patches as opaque strings; parsing them there
  costs more than the work it would relocate).

## 0.29.0 — 2026-07-31

**Sublinear resources.** Total parse work has an Ω(n) floor and stays exactly
there — but retained memory and reopen latency don't have to be O(n), and now
they aren't. Requires `brookmd-core` 0.26.0. Both features are **web-path**
(WASM + TS client); the native bindings (React Native / Kotlin / Swift /
Flutter) share every parser-core improvement but consume `allBlocks()` and so
keep full retention — see the platform matrix in the root README.

### Added

- **Instant thread reopen — `getPersistable()` / `hydrate()` /
  `sourceFingerprint()`.** A finished (or in-flight) stream can be persisted
  as a small JSON envelope — the blocks `getSnapshot()` showed plus a source
  length/hash and a `done` flag — and a fresh `BrookClient` hydrates from it
  with **no worker, no WASM, no parsing**: 1 MB / 2,250 blocks re-streams in
  ~110 ms, hydrates in **~5.5 ms** (and a real browser reopen also skips
  worker spin-up and WASM init). To *continue* a non-finalized thread, pass
  the original source to `beginResume()`: the UI keeps showing hydrated
  blocks while a background re-parse catches up — reusing the existing
  divergence/adoption machinery, so every hydrated block keeps its exact id
  across the swap and nothing visible remounts; appends stream live once
  caught up. The envelope is a versioned **package** format
  (`hydrateVersion`), deliberately not the wire contract; the README's
  "Instant thread reopen" section documents the persistence and invalidation
  contract.
- **`retainCommittedHtml` — the parser no longer hoards the rendered
  document.** The core kept every committed block's HTML alive for
  `allBlocks()` — which the web worker path never calls. The worker now
  defaults the flag **off**: retained memory beyond the source buffer drops
  to the open tail only (measured: **53–70% less total retention** on 1 MB
  streams, with the flag-off overhead above the raw source staying under
  300 bytes at every point). Patches are byte-identical either way — committed
  blocks are emitted exactly once and never re-read. Rust-side default stays
  ON, so native bindings and `allBlocks()` consumers are unaffected;
  `renderToString` pins it on (it assembles from `allBlocks`). Set
  `retainCommittedHtml: true` in `ParserConfig` if you drive the worker and
  want `allBlocks` fidelity.

## 0.28.0 — 2026-07-31

**Browser-side linearity.** The parser and wire have been O(new bytes) per
append for a while; this release makes the *DOM application* match. Before, an
open (streaming) block was fully rebuilt on every animation frame — a 20 KB
highlighted code block wrote ~44 million characters of `innerHTML` over its
lifetime (≈2,150× the wire bytes), and a per-frame `replaceWith` destroyed any
text selection or `<pre>` scroll position inside the block. Every open-block
path now applies patches incrementally, measured at **2–3.4× the block's final
markup, flat across sizes** (1× is the write-it-once floor), and enforced by a
chars-written regression gate so browser-side linearity is CI-pinned like the
parser's scaling shapes.

### Performance

- **Open blocks apply the wire's splice instead of rebuilding.** The delta
  signal (`keep_units`) the wire already computed was being discarded at the
  client; it now reaches both renderers, which rebuild only the element the
  splice lands in (fast path fires ~94% of syncs; any ambiguity falls back to
  a full rebuild, so correctness never depends on the fast path).
- **Open code blocks reuse the streaming highlighter's frozen prefix**: the
  frozen markup is appended once and never rewritten; only the bounded
  speculative tail repaints. 20 KB highlighted stream: ~44 MB written → ~370 KB
  (**~120× less**).
- **Keyed list/container sync in the DOM renderer** (`blockData` on): settled
  `<li>`s / nested container children are never re-rendered — new items append,
  only the open last item repaints; a tight→loose flip resyncs once, keyed by
  `(index, html)` so it cannot be silently missed. Streamed 20 KB list: 284× →
  **2.9×**; blockquote: 318× → **3.4×**.
- **React's keyed container path now engages by default.** It was accidentally
  gated behind a `components` map (git history shows the gate was incidental);
  a default-config streaming blockquote/alert now renders keyed — 22.6× and
  growing → **3.4× flat**. React and DOM implementations were cross-checked to
  byte-identical work counts.
- Known documented fallback: a streamed table with `blockData: false` still
  takes the full-rebuild path (the splice refuses table tag chains — foster
  parenting). `blockData: true` tables were already incremental (1.7×). A
  table-scoped splice was measured (15% improvement) and rejected as not worth
  the parser-divergence risk.

### Fixed

- **Text selection and `<pre>` scroll survive streaming.** Selecting text in
  the already-settled part of a streaming block no longer collapses on the
  next token; a code block's horizontal scroll position is preserved. Both are
  pinned by tests with negative controls.

### Changed

- The DOM inside an open, streaming `<code>` element is now two spans (frozen +
  speculative tail) while streaming; it settles to the same single-markup form
  as before when the block closes. CSS that targets `code > *` structurally
  may observe the difference mid-stream only. Settled output is unchanged.
- React's default open blockquote/alert omits inter-block whitespace text
  nodes mid-stream (matching the DOM renderer's long-standing keyed behavior);
  layout is unaffected and settle output is byte-identical.

## 0.27.0 — 2026-07-31

### Added

- **Streaming syntax highlighting — open code blocks now highlight live, on by
  default.** Since 0.19 the built-in highlighter deliberately waited for a
  fence to close; a streaming block showed plain text. Open blocks now render
  highlighted as they grow, and the mechanism preserves every guarantee the
  deferred design existed to protect:

  - **Byte-identical at settle.** All committed markup comes from seeded runs
    of the same resumable tokenizer that `highlight()` uses — the frozen
    prefix is only ever extended at *checkpoints* (positions provably outside
    any future token: after a newline in whitespace, ≥ 3 bytes behind the
    stream head, with no unterminated string/comment/template live; HTML
    checkpoints after `>` instead). When the block closes, one seeded run
    finishes the tail, so the final bytes equal a one-shot `highlight()` —
    pinned by a ~44,000-case fuzz (all languages, chunk sizes down to 1 byte,
    plus speculative-revision streams) asserting settle-identity and that
    every intermediate frozen prefix is a byte-prefix of the final output.
  - **Bounded work, independent of block size.** Between checkpoints, an
    unterminated string/comment advances via an O(1)-state scanner over only
    the new bytes; the tail re-scan is capped at 8 KB (past the cap the tail
    renders plain until the next checkpoint — correctness unaffected). Total
    tokenization work measures ≈ 5.7× the streamed bytes end-to-end against
    the real parser, flat from 288 B to 23 KB blocks (naive re-highlighting
    measures 812× more at 23 KB); a work-bound test enforces < 6×. The
    50 000-char plain-escape guard still applies and discards streaming state.
  - **The tail is speculative.** The few tokens nearest the stream head may
    change color as bytes arrive (an unterminated `"` reads as plain until its
    closer lands) — the same speculative-tail behavior brookmd's links and
    emphasis already have. Frozen output never changes.

  Opt out with `streamingHighlight={false}` on `BrookMarkdown` (React) or
  `streamingHighlight: false` in `mountBrookMarkdown` options (DOM, and via it
  the Web Component and Vue/Svelte/Solid adapters) — that restores 0.26.1's
  plain-until-close exactly. Not a `ParserConfig` field: highlighting is a
  renderer concern; the parser, wire, and WASM are untouched. SSR is
  unchanged (closed blocks, synchronous), and `components.CodeBlock` /
  `pre` / `code` overrides still bypass the built-in highlighter entirely.
  Closing a block that streamed in now also highlights near-instantly: the
  close-time pass reuses the accumulated prefix instead of starting over.

## 0.26.1 — 2026-07-30

Two fixes found by benchmarking 0.26.0 against real chat traffic. Requires
`brookmd-core` 0.25.1.

### Fixed

- **A GFM table could not interrupt an open paragraph — anywhere.**
  `scan_paragraph` had no table arm, so `item\n| a | b |\n|---|---|` swallowed
  the delimiter row as paragraph text. The visible symptom was "tables inside
  list items don't parse" (an item's de-indented body opens with a paragraph),
  but the bug was position-independent and hit top level and blockquotes the
  same way. The fix is an O(2-line) gate — header row with a pipe, delimiter
  row next, matching cell counts, both rows indented ≤ 3 columns — checked once
  per line as it arrives, on both the full-reparse and streaming paths, so a
  scan started at a commit boundary renders byte-identically to a cold one.
  Verified against GitHub's rendering for every non-pathological shape; the
  deliberate exception (both rows must be ≤ 3-indented, so two exotic
  mixed-indent shapes stay paragraphs) is pinned in `tests/nested_tables.rs`.
  A nested table renders inside the item's `html` and — like nested lists —
  carries no structured `blockData` of its own.

### Performance

- **Close-time syntax highlighting no longer blocks the main thread.** The
  built-in highlighter ran as one synchronous task when a block closed —
  ~110 ms for a large fence on a mid desktop. The tokenizer loop was already
  resumable at any offset with no carried state, so it now runs in ~5 ms
  slices (`scheduler.yield()` where available, `MessageChannel` otherwise),
  with the first slice synchronous so small blocks render highlighted in the
  same tick with no flash. Output is byte-identical — pinned by a chunked ==
  one-shot property test across all 20 languages at chunk sizes down to 1,
  plus a 6,556-case differential fuzz against the previous implementation.
  Also: the renderers now reuse the parser's already-decoded source
  (`CodeBlockData.code`) when `blockData` is on instead of re-deriving it from
  the HTML, and `escapeHtml` no longer concatenates per character. A 49 KB
  block's longest main-thread task drops from ~38 ms to ≤ 6 ms on the same
  hardware; `highlight()`'s public signature and bytes are unchanged, SSR
  stays synchronous, and `components.CodeBlock`/`pre`/`code` overrides are
  unaffected.

## 0.26.0 — 2026-07-30

**Rendered HTML bytes change in this release.** Everything new below is opt-in
and default-off — with the flags off those paths are byte-identical to 0.25.2 —
but the output-fidelity work is unconditional: container framing newlines,
paragraph whitespace, table line breaks, fenced-code indentation, and the
task-list checkbox form now match the CommonMark and GFM reference renderers
exactly. No API is removed, and the wire envelope, ids, and commit semantics are
untouched — but HTML snapshots taken against 0.25.x will need regenerating.
That, not the new features, is why this is a minor bump.

What it buys: **652/652 CommonMark 0.31 and 24/24 GFM, byte-exact** against the
reference renderers, where before the suites passed only after structural
normalization. Byte-exactness is now the harnesses' default floor and is pinned
in CI, so one regressed byte fails the build.

Requires `brookmd-core` 0.25.0.

### Added

- **`softBreaks` — a soft line break renders as `<br>`.** Strict CommonMark
  treats a bare `\n` inside a paragraph as whitespace, so a model that writes one
  thought per line gets one reflowed blob. This is the `remark-breaks` /
  chat-comment convention where one Enter is one visual line, and it is what most
  chat UIs actually want. Off by default; it only ever *adds* breaks (a hard break
  is `<br>` either way), so no existing output loses a line.
- **`allowSchemes` — un-block a URL scheme brookmd blocks by default.** Bare
  scheme names, no colon (`allowSchemes: ["file"]`), matched case-insensitively.
  It reaches exactly one tier: the *overridable*-blocked schemes, today `file:`.
  The script-executing tier (`javascript:`, `data:text/html`, …) is
  non-overridable — listing one is a silent no-op, not an escape hatch — and the
  encoded-evasion neutralization (percent- and entity-encoded scheme prefixes) is
  unchanged and runs before the check. This exists for privileged embedders —
  Electron shells, extensions, editor preview panes — that intercept link clicks
  instead of navigating.
- **`lenientLists` — rescue an over-indented list item from becoming a code
  block.** CommonMark §5.2 says a marker followed by 5+ columns of whitespace
  starts an indented code block, so a model emitting `-       const value = 1;`
  renders a `<pre><code>` instead of a list item. With the flag on, a marker
  followed by **6 or more columns of literal spaces** absorbs the padding into the
  content column and the text parses as the item's own markdown. Deliberately
  narrow: exactly 5 spaces still opens code (that column is the spec boundary
  itself), a fence on the marker line stays a fence, an indent on a *later* line
  stays code, and **tab** padding stays code — tabs are an authoring choice, model
  over-indentation is always literal spaces. Excluding tabs is what holds the
  divergence to a single spec example (274) and only while the flag is on; the
  conformance suites run strict and are unaffected.
- **`blockHtml` — block-level raw HTML through the safe sanitizer.** A
  `<details><summary>…</summary>…</details>` block rendered as an escaped code
  block before; now it renders as real elements, sanitized. Only CommonMark HTML
  block **types 6 and 7** qualify — types 1–5 (`<script>`/`<pre>`/`<style>`/
  `<textarea>`, comments, processing instructions, declarations, CDATA) stay
  escaped by design, so the constructs that can execute or swallow the rest of
  the document never take effect.
  It shares its token core with the inline raw-HTML path (one policy, not two),
  tracks an open-tag stack with speculative closers so a half-streamed
  `<details` never leaks a broken element, carries a void-element table so `<hr>`
  and friends are not pushed onto that stack, and caps nesting depth at 100.
  Half-streamed tags are suppressed while open and settle at finalize. Takes
  effect only when the sanitizer is engaged (`htmlAllowlist` or `dropHtmlTags`);
  on its own it does nothing.
- **`CodeBlockData.meta` — the fence info string's remainder.**
  ```` ```ts title="src/main.ts" ```` now yields `lang: "ts"` **and**
  `meta: 'title="src/main.ts"'` on the data channel and as a `meta` prop on
  `components.CodeBlock`. Always-on like `lang` (no `blockData` needed) and
  omitted entirely when the fence carried none, so a fence without meta is
  byte-identical to before. It is deliberately **not** in the rendered HTML —
  there is no `data-meta` attribute — because a filename header is a component's
  job, not the parser's. While streaming it appears only once it can no longer
  change (the opening fence line terminated by a newline, or at finalize), so a
  header never flickers through a half-typed `title="src/ma`.
- **`ListItemData.start` — a source byte offset per list item.** Under
  `blockData`, each top-level `items[]` entry carries the document-absolute offset
  of the byte where its marker begins, same origin as `Block.start` and stable as
  the document grows. That is enough to build task-checkbox writeback: locate the
  `[ ]` from the item's offset and flip it in the original markdown, no HTML
  round-trip. **Nested** items carry no offset rather than a wrong one — a nested
  list is not a separate block, it renders against a synthesized de-indented
  string with no document offset — and that limitation is documented on the type.
- Together `meta` and `start` bump the **wire contract to 1.3.0** (WIRE.md §9):
  purely additive optional `data` keys, byte-identical when unexercised.
- All four flags are plumbed through every surface: client/worker config, the
  server renderer, the `<brook-markdown>` element (`soft-breaks`,
  `lenient-lists`, `block-html`, `allow-schemes`), React Native, Kotlin, Swift,
  and the Flutter hand-written config.
- **Byte-exact conformance mode in the spec harnesses**, with default ratchet
  floors `CMARK_MIN_EXACT=652` / `GFM_MIN_EXACT=24` — pinned explicitly in both
  the CI and publish workflows alongside the older normalized floors. The four
  deliberate differences from the reference (`target`/`rel` on links, `data-lang`
  on code blocks, HTML5 void `<br>`, `style="text-align:…"` instead of GFM's
  deprecated `align`) are folded by a documented `canonicalize` step applied to
  **both** sides — the only transform on the byte-exact path, so it can erase our
  intentional extras but never hide a structural divergence.
- **An unconditional `bindings` CI job** — FFI and C-ABI crate tests plus
  regeneration-freshness checks for the React Native, Kotlin, and Swift bindings.
  The existing binding workflows only ran on `pull_request` behind `paths:`
  filters, so a push to main or a JS-only PR gated on nothing; that is how stale
  binding goldens survived.

### Changed

- **Container inner newlines now match the reference exactly** (cmark's `cr()`
  rule): `<li>` / `<blockquote>` / alert framing newlines land where the
  reference puts them, an empty blockquote gets its newline, and a `<li>` opening
  with a tight paragraph no longer emits a spurious `\n`.
- **Paragraph whitespace follows the spec line by line.** Leading indentation is
  stripped on *every* line, not just the first; trailing spaces and tabs are
  dropped before a soft break; a hard-break line sheds the *next* line's indent.
  Lazy continuation lines in blockquotes and lists keep their `\n` instead of
  being glued with a space.
- **GFM tables emit one element per line**, matching the reference's framing, and
  task-list checkboxes emit the reference byte-form
  (`checked="" disabled="" type="checkbox"`).
- **Fenced code**: the body is de-indented by the opening fence's own indent (so
  a fence at columns 1–3 no longer carries that indent into every line);
  significant trailing spaces inside the last line are preserved; interior blank
  lines before the closer are preserved. Tab-column arithmetic is fixed after
  blockquote markers, across item-content boundaries, and after list markers.
- **Document assembly is now defined and documented** ([WIRE.md
  §12](../../crates/brookmd-core/WIRE.md)): a block's `html` carries **no
  trailing newline** — the terminator after a top-level block belongs to the
  document, not the block — and concatenating blocks is a `cr()`-join.
  `renderToString` follows that rule, so a server-rendered document and a
  reference render agree byte-for-byte.
- **Native `BrookConfig` gained four fields** (`soft_breaks`, `allow_schemes`,
  `lenient_lists`, `block_html`), appended in a newly documented **append-only
  zone**: uniffi serializes a record's fields *positionally*, so inserting a field
  anywhere above shifts every later read. The wire was verified to be
  exact-consumption — a binding built against the old record fails loudly on a
  version mismatch rather than silently mis-decoding — and the RN, Kotlin, and
  Swift bindings are regenerated, with the Flutter hand-written config synced
  (it also gained the previously-missing `wire_delta`). Stale wire goldens across
  all four languages were re-synced. `brookmd-ffi` and `brookmd-cabi` go to
  **0.3.0**: the native library and its generated bindings must ship in lockstep.

### Performance

Every item below is pinned by a **wall-time** regression guard in
`crates/brookmd-core/tests/scaling.rs`, each paired with a flush-control twin
that isolates the shape from its chunking. That pairing is the point: work
counters alone provably miss the allocation-class quadratics below — the parser
was doing O(n) *counted* work while re-allocating O(n) bytes per append, so only
the clock could see it.

| Shape | Measured as | Before | After |
| --- | --- | --- | --- |
| growing fence opener line | growth over an 8× input span | 50.5× | **9.1×** |
| indented continuation lines | vs the unindented control | 304.6× | **1.1×** |
| ragged chunks parked on a blank partial line | vs control | 110.2× | **1.1×** |
| the same, inside a list item | work counters | 212× | **12.8×** |
| block-HTML sanitize | vs the escaped control | 247× | **1.3×** |

- **Fence opener-line growth** re-sliced the whole opener on every append —
  quadratic in the info string's length. Now linear-ish: 9.1× over an 8× span.
- **Indented continuation lines** made `is_boundary` produce *zero* cut
  candidates, so every append re-rendered the entire paragraph from its start. A
  new indent-led boundary rule (with a hard-break-straddle exclusion) restores
  cutting; the shape now costs 1.1× its unindented control.
- **Ragged chunking that parks on a whitespace-only partial line** dropped the
  paragraph cache outright, forcing a full rebuild per append. The cache now
  **suspends instead of dropping**: the closed view stays byte-identical and the
  parse resumes when the line completes. List items inherit the fix through the
  nested parser.
- **Block-level raw HTML** would otherwise re-sanitize its whole body per append
  (measured 247×); the sanitize cache folds at token boundaries, shipping at 1.3×
  the cost of the escaped control. Container, table-cell, and heading caches were
  proven structurally immune to the same class and are pinned so they stay that
  way.

### Security

- **The raw-HTML attribute policy is now an explicit dropped-attribute table**
  rather than scheme-checking alone: `srcdoc`, `is`, `autofocus`,
  `contenteditable`, the DOM-clobbering `id` / `name`, the shadow-piercing
  `slot` / `part` / `exportparts`, the `form*` family, the `xmlns:` / `xlink:`
  prefixes, and `ping`. `formaction` and `ping` are **dropped outright** — for a
  URL carrier that re-targets a form or fires a background beacon, dropping is
  strictly stronger than validating its scheme. Component-tag props stay
  permissive by design (they are consumer-mediated — your component decides what
  to do with them, and they never become DOM attributes); that asymmetry is now
  documented rather than incidental.
- **The web component's `gfm-tagfilter` attribute was never observed.** It was in
  the element's config map but missing from `observedAttributes`, so it applied at
  mount and then silently ignored every later change — a security-relevant flag
  that could not be turned on after the fact. Fixed, and the list now carries a
  comment tying it to the map so the next flag cannot repeat it.

### Fixed

- **A mid-stream committed-view divergence**: a cut taken after a *single*
  inter-word space could straddle the hard-break lookbehind, so a paragraph
  containing entity-produced spaces rendered differently mid-stream than it did
  one-shot. The streamed view now matches the one-shot render at every chunk
  boundary again.

## brookmd-react-native 0.1.7 — 2026-07-30

### Added

- `softBreaks`, `allowSchemes`, `lenientLists`, and `blockHtml` on the native
  `BrookConfig`, reaching the on-device parser through the same JSI path as the
  existing flags. The generated TS/C++ bindings are regenerated from
  `brookmd-ffi` 0.3.0 and are now freshness-checked on every push by the new CI
  `bindings` job.

### Changed

- `brookmd` dependency range `^0.25.0` → `^0.26.0`, and the vendored native
  binaries are built against `brookmd-core` 0.25.0 — so the on-device renderer
  produces the same reference-exact bytes as the browser. Native and JS must move
  together here: uniffi records are positional, and the four new `BrookConfig`
  fields land in the record's append-only zone.

## 0.25.2 — 2026-07-27

Performance only. Rendered output is unchanged — byte-identical mid-stream, not
just at finalize.

### Fixed

- **An open component block no longer re-scans its whole body on every blank
  line — O(n²) → O(n).** `try_incremental_component` bailed whenever the fed
  buffer ended on a blank line, and because the cache had already been taken the
  bail dropped it, forcing a full tail re-scan, a full re-render, and a fresh
  nested parser over the entire body. Blank lines are legal component-body
  content, so that fired once per body paragraph.

  The bail existed for a real reason: when the buffer ends blank the full rescan
  renders the component and all its sub-blocks with `open_tail = false`, which
  the nested parser's `force_open_tail = true` commits can never match. Rather
  than work around it with a trigger-byte heuristic (measured: still 3.9×/doubling
  on any body containing a backtick or bracket — i.e. all real ones), the cache
  now carries a **settled twin**: a second nested parser with `force_open_tail`
  off, fed lazily only on the appends that read it, so each catch-up spans just
  the bytes since the previous blank line. A body with no blank lines never
  allocates it. The cache now arms once per stream instead of once per paragraph.

  Streaming a 64 KB `<Thinking>` body, scan work and wall time:

  | body | before | after |
  | --- | --- | --- |
  | plain paragraphs | 117,333,095 B / 11,954 ms | 229,837 B / **269 ms** |
  | with inline code | 101,046,887 B / 9,316 ms | 224,977 B / **226 ms** |
  | with links | 86,362,310 B / 6,837 ms | 220,835 B / **180 ms** |
  | with `$` and `<` | 117,333,095 B / 7,333 ms | 229,837 B / **388 ms** |

  Scan and render work are now 2.00×/doubling (dead linear); the residual
  3.3–3.6×/doubling in wall time is a separate, pre-existing cliff —
  `assemble_wrapped_body` re-materializes an open block's HTML every append —
  which the already-registered `open-block-html-reemit` shape sits in too.

  **Trade-off:** the twin holds a second copy of an open component's body bytes
  plus one rendered-HTML set, freed when the block closes. Roughly 2× transient
  memory for open component blocks *that contain blank lines*; blank-free bodies
  are unaffected.

### Added

- `tests/scaling.rs` gains `component_multi_para` and `component_multi_para_rich`
  (backticks, brackets, `$`, `<`), both gated `Linear`. The existing
  `component_block_open` shape generates no blank lines and so was structurally
  blind to this: its `scanned` is a flat 249 B from 8 KB to 64 KB. The new shapes
  measure 252.4× on the pre-fix parser (gate fails) and 16.0× over a 16× span
  after.

## 0.25.1 — 2026-07-27

A correction release. 0.25.0 shipped one user-visible regression and a dev-gate
that failed open in production; both are fixed here, along with a pre-existing
memory leak on the opt-in `childMemo` path. Upgrade from 0.25.0.

### Fixed

- **WITHDRAWN: the streaming component-tag deferral from 0.25.0.** That change
  gated component rendering on `open_tail`, which a container propagates
  UNCHANGED to every sub-block of every list item. A component tag alone in a
  **non-last list item** — whose one-line body carries no trailing newline of its
  own — therefore never satisfied the gate again, and rendered as the literal
  text `<Thinking>` for the entire rest of the stream instead of mounting the
  consumer's component. Measured 201/201 ticks affected in a 200-item list, and
  the deferral bought exactly one tick across the three shapes it was written
  for. `brookmd-core` is reverted to its pre-0.24.0 behavior here (0.24.1); the
  convergence property 0.25.0 claimed is withdrawn with it. The one-tick issue it
  was chasing is already contained by the per-block error boundary.
- **Dev-only warnings ran in production.** The gate read `globalThis.process?.env`,
  which is a different member path from the `process.env.NODE_ENV` free
  identifier that esbuild / Vite / Next substitute — so in every bundled browser
  production build it evaluated to "development", printed to real users, and
  prevented dead-code elimination of the message strings. Every gate is now at
  the call site in the literal form bundlers fold. Verified against a real Vite
  build: 11 dev-warning strings survived a production bundle before, 0 now.
  (Note the trade: in a realm with no `process` at all — an unbundled CDN
  consumer — these six development warnings no longer fire. Error reporting via
  `onBlockError` / `console.error` is NOT gated and is unaffected.)
- **`childMemo` retained every intermediate tree on a single-element block**
  (pre-existing, opt-in path). The cache key embeds the segment text, so a block
  whose HTML is ONE top-level element — which is every core-emitted block kind —
  could never hit: each patch wrote an entry that was never read again, and
  `CHILD_MEMO_CAP` bounds entry count, not bytes. Peak RSS grew 154 MB → 1.65 GB
  over 800 patches of a streaming table. Such blocks now take the ordinary
  whole-block walk.

### Changed

- Shipped bundles are smaller than 0.25.0: gzipped, minified, production —
  `index` 14,232 → 13,407 B, `react` 14,185 → 13,365 B, `client` 4,651 → 4,283 B.
  The boundary's `console.error` keeps everything actionable in production (block
  id, kind, override keys, HTML excerpt, the error) and moves only the
  explanatory prose behind the dev gate.

## brookmd-react-native 0.1.6 — 2026-07-27

### Fixed

- Vendored native binaries rebuilt against `brookmd-core` 0.24.2, which makes an
  open component block's streaming cost linear in its body. This matters more
  on-device than in the browser: the pre-fix path spent ~24 s of native CPU on a
  64 KB token-streamed `<Thinking>` body. No JS changes.

## brookmd-react-native 0.1.5 — 2026-07-27

### Fixed

- Vendored native binaries rebuilt against `brookmd-core` 0.24.1, so the on-device
  parser no longer carries the component-tag deferral regression withdrawn in
  brookmd 0.25.1 (a component tag alone in a non-last list item rendered as
  literal text for the rest of the stream). No JS changes.

## brookmd-react-native 0.1.4 — 2026-07-26

### Fixed

- **The native transport shim routes worker listeners by event type.** brookmd
  0.24.0 taught `BrookPool` to listen on `error` / `messageerror` (the browser's
  out-of-band worker-failure channels). `NativeWorker.addEventListener` ignored
  its `type` argument and registered every listener in one set, so the pool's
  fatal handler received the ordinary `ready` / `patch` envelopes and read the
  first as `brookmd worker failed to load`, killing every stream before it
  started. Neither channel can fire for an in-process shim — there is no script
  URL to fetch and no structured-clone step — so they are now accepted and never
  fired. This was latent: the package pinned `brookmd` to `^0.23.0`, which
  predates those listeners, so it only surfaced when the range moved.
- `brookmd` dependency range corrected `^0.23.0` → `^0.25.0`; vendored native
  binaries rebuilt against `brookmd-core` 0.24.0.

## 0.25.0 — 2026-07-26

Fixes a crash class where a `components` override could be invoked with the
wrong prop shape and take the entire document down with it. If you pass
`components` and read `props.block` in any of them, upgrade.

### Fixed

- **A `components` override no longer receives two incompatible prop shapes
  without warning.** The map is consulted by two dispatchers: the block-kind
  dispatcher (which supplies `BlockComponentProps`, including `block`) and the
  element-name walker that powers `a`/`code`/`table` overrides (which supplies
  attributes + `children` and **no `block`**). The same key reaches both — an
  `inlineComponentTags` chip, or a `componentTags` tag nested inside a list item
  or blockquote — so an override reading `props.block.…` threw
  `can't access property "kind", block is undefined`, intermittently, depending
  on where the model put the tag. Three defenses now apply: block-kind keys are
  typed to `BlockComponentProps` (a mismatched override is a compile error), a
  raw element whose name collides with a block-kind key is never dispatched to
  that override, and every block renders inside its own error boundary.
- **A throwing override costs one block, not the document.** React unmounts the
  whole tree on an uncaught render error, so a single bad override blanked the
  page. Each block now has its own boundary; the failed block is skipped and
  retried as soon as its HTML changes, so a streaming-tail failure heals itself
  when the block settles.
- **The parser no longer emits a component tag it will retract.** A component
  open tag was recognized as soon as it looked whole-line, but during streaming
  end-of-buffer is not end-of-line — so `> <Thinking>x</Thinking>` rendered a
  raw `<Thinking>` element for one tick before settling to escaped text. That
  transient raw element is what reached overrides with the wrong props. A
  component tag now opens a block only once its line is known to be complete.
- **Recovery no longer loses the parser config.** The worker keeps config per
  stream id, so the one-shot recovery re-feed landed on a worker that had never
  seen it while `configSent` stayed latched — the healed parser was silently
  rebuilt with library defaults, dropping `componentTags`, `blockData`,
  `gfmMath` and the whole `kind.data` structured channel for the rest of the
  session.
- **A terminal worker failure no longer corrupts the document.** The store kept
  the dead generation's blocks while the fresh parser renumbered from zero, so
  the next append merged two generations under colliding ids (duplicate React
  keys, silently overwritten blocks, a shrinking document). The generation now
  restarts cleanly, with a one-time warning. Transient failures still heal
  invisibly through recovery, unchanged.
- **Per-block error containment is free for committed blocks.** The boundary
  lives inside the per-block memo, so a settled document re-renders no
  boundaries when the streaming tail patches — a new React-side complexity gate
  (`test/boundary-linearity.test.tsx`) pins this, counting work rather than
  timing it, mirroring the Rust `scaling` gate.
- `applyPatch` and the stale-view merge now drop a malformed entry rather than
  publishing a hole into the snapshot, and both renderers skip a block with no
  `kind` instead of dereferencing it.

### Added

- **`onBlockError`** on `<BrookMarkdown>` — fires when a block's render throws,
  with `{ blockId, kind, componentKeys, html }`. Without it the same detail goes
  to `console.error`.

### Changed

- `Components` is now a mapped type: block-kind keys (`CodeBlock`, `Table`,
  `Alert`, …) are typed to `BlockComponentProps`; every other key stays
  permissive. Type-only, but it will surface existing mismatches at compile time.
- Requires `brookmd-core` 0.24.0 (the streaming component-tag fix above).
- `onBlockError` is identity-sensitive like `components` / `onRenderMetrics` —
  hoist or memoize it, or every block re-renders on every patch.

## 0.24.0 — 2026-07-24

### Added

- **Worker load failures are now detected and self-heal.** A worker script
  that fails to load (e.g. a stale hashed worker URL held by an already-open
  tab after a redeploy) fires a DOM `error` event instead of ever posting a
  message; previously nothing listened, so the client waited forever and the
  container stayed permanently empty with no console output. The pool now
  listens for `error`/`messageerror`, arms a per-worker boot deadline
  (default 20s, configurable/disable-able via the `BrookPool` options), and
  routes every fatal trigger — WASM init failure, load error, deserialization
  error, deadline — through one idempotent failure path: pending waiters
  reject, each affected client's `onError` fires, and the dead worker is
  terminated and evicted so the pool capacity recovers.
- **One-shot automatic recovery.** A client whose worker dies transiently
  heals invisibly: the driven document accumulates in a recovery buffer (both
  `setContent` and `append`/`pipeFrom` modes) and is re-fed once to a fresh
  worker through the preserved-view swap path, so the rendered view never
  blanks and in-flight chunks are folded in safely. Recovery re-arms only
  when the caller advances the content, so a document that deterministically
  crashes the parser surfaces an error after exactly one retry instead of
  respawning workers. Opt out with `new BrookClient({ recovery: false })`
  for memory-sensitive giant documents.
- **`client.failed: Error | null`** — synchronous getter for terminal worker
  failure (stays `null` through a successful invisible heal), for rendering a
  degraded fallback.
- **React hooks error surface.** `useBrookStream`'s `onError` now also
  receives worker-level errors (with a `fatal` flag on the `Error`), and
  `useBrookMarkdownString` gains an `onError` option; both default to
  `console.error`.

### Fixed

- Fatally failed workers no longer leak their per-stream handler-map entries
  in the pool.

## 0.23.2 — 2026-07-22

### Fixed

- **Streaming/one-shot parity: non-ASCII whitespace no longer blanks a line.**
  The streaming premature-commit guard tested "previous line non-blank" with
  Unicode-aware trimming, so a line consisting only of form feed (U+000C),
  vertical tab (U+000B), NBSP, or other non-ASCII whitespace was treated as a
  blank line. A paragraph opened by such a line could be committed early and a
  later lazy continuation could never merge back — the streamed output
  permanently diverged from the one-shot parse (nightly parity-fuzz catch,
  input `\x0c\n-- - @`). The guard now uses the CommonMark blank-line
  definition (ASCII space/tab only), matching the one-shot path; regression
  tests pin the artifact and variants across chunk splits.

## 0.23.1 — 2026-07-21

### Changed

- Documentation only: the README now covers wire delta mode (the 0.23.0
  headline) — measured numbers, the automatic worker↔client behavior, and the
  raw-boundary opt-in — plus refreshed platform-status notes across the
  repository. No code changes; behavior identical to 0.23.0.

## 0.23.0 — 2026-07-21

### Added

- **Wire delta mode — the streaming re-emit floor is retired** (wire contract
  **v1.2.0**, additive). Previously, every `append` re-emitted each open
  block's full HTML across the WASM→worker→main-thread boundary, making total
  emitted bytes O(n²/chunk) for a block that grows across many chunks — the
  documented "re-emit floor". Now the parser can emit a **verified splice**
  (`html_delta: { keep_bytes, keep_units, append }`) against the block's
  previous emit instead; splices are established by byte comparison, never by
  structural inference, so reconstruction is byte-exact by construction.

  Measured at 200 KB / 256-byte chunks: a streaming list's total patch JSON
  drops **119.6 MB → 0.78 MB (153× less)** with **2.8× faster** end-to-end
  parse+serialize; an unclosed code fence **80.1 MB → 0.58 MB (137×, 4.9×
  faster)**. Fast-committing shapes are unchanged (no regression). The curve
  is now linear, so the gap keeps widening with document size.

  - The npm package enables this **automatically and invisibly** — the worker
    opts in, and the client reconstructs full blocks before anything else sees
    them. No API change; `Block.html` is always complete.
  - `brookmd-react-native` does the same across the JSI boundary.
  - Raw-boundary consumers (`BrookParser`, native `BrookSession`, C ABI): the
    mode is **opt-in** (`setWireDelta(true)` / `wire_delta: true`) and the
    default wire is byte-identical to contract v1.1.0. Enabling it obliges you
    to reconstruct per [WIRE.md §11](../../crates/brookmd-core/WIRE.md).
  - Pinned by: reconstruction-parity suites (Rust corpus × chunkings, dual
    UTF-8/UTF-16 offset verification), wire goldens in all five language
    surfaces (core, FFI, C-ABI, Kotlin, Swift), a fuzzer extension asserting
    per-patch reconstruction on arbitrary inputs, and a new scaling-gate test
    that *fails CI* if delta-mode emitted bytes regress from linear.

## 0.22.2 — 2026-07-21

### Performance

- **7–24% faster streaming across every benchmarked document shape** (native
  and WASM builds alike), from a final profiling pass over the Rust core:
  - the scanner's newline search (`line_end`) — the hottest primitive, re-run
    by every block probe on every line — now scans 8 bytes per step (SWAR)
    instead of one;
  - the inline renderer copies runs of plain text with a single `push_str`
    instead of per-byte dispatch + escape calls.

  Rendered HTML is byte-identical (spec goldens, wire-envelope goldens,
  chunk-independence property tests, and the streaming-parity fuzzer all
  pass); the WASM binary grows 338 bytes (+0.16%). Ships as `brookmd-core`
  0.22.1 on crates.io.

## 0.22.1 — 2026-07-21

### Fixed

- **React Native / Metro compatibility**, both issues found by the new
  app-level e2e gate (a real RN app on an emulator + simulator asserting the
  wire goldens through the native parser):
  - every conditional-exports subpath now ships a `default` condition, so
    Metro's stock package-exports resolution works without custom
    `unstable_conditionNames` (which could poison `@babel/runtime` helper
    resolution and crash release bundles at startup);
  - the browser worker bootstrap moved behind a `react-native` field map to
    an `import.meta`-free shim, so Hermes can parse release bundles (Hermes
    rejects `import.meta` at parse time even in unreachable code). Web
    behavior is unchanged — the `new Worker(new URL(...))` pattern bundlers
    analyze stays intact.

## 0.22.0 — 2026-07-20

### Changed

- **The project has a new name: `flux-md` is now `brookmd`.** Same engine, same
  wire contract, same APIs — only the name changed. The lineage continues from
  0.21.0 (this is 0.22.0), and the rendered HTML is byte-identical apart from the
  one renamed marker below. Install `brookmd` and update your imports:

  | Was (`flux-md`) | Now (`brookmd`) |
  | --- | --- |
  | npm package `flux-md` | npm package `brookmd` |
  | `import { FluxMarkdown } from "flux-md"` | `import { BrookMarkdown } from "brookmd"` |
  | `FluxClient` | `BrookClient` |
  | `FluxPool` | `BrookPool` |
  | `FluxMarkdown`, `FluxMarkdownStatic` | `BrookMarkdown`, `BrookMarkdownStatic` |
  | `useFluxMarkdown`, `useFluxMarkdownString` | `useBrookMarkdown`, `useBrookMarkdownString` |
  | `initFlux`, `initFluxSync` | `initBrook`, `initBrookSync` |
  | `flux-md/server`, `flux-md/dom`, `flux-md/styles.css`, … | `brookmd/server`, `brookmd/dom`, `brookmd/styles.css`, … |
  | `<flux-markdown>` custom element | `<brook-markdown>` custom element |
  | `.flux-md` root class, `.flux-block*`, `flux-dark`/`flux-light` | `.brook-md`, `.brook-block*`, `brook-dark`/`brook-light` |
  | `--flux-*` CSS variables | `--brook-*` CSS variables |
  | `data-flux-pending` pending-link marker | `data-brook-pending` |

  The Rust crate `flux-md-core` is likewise now `brookmd-core`; the wire contract
  ([`WIRE.md`](../../crates/brookmd-core/WIRE.md)) moves to **v1.1.0** for the
  single marker rename (envelope structure unchanged).

- The old `flux-md` npm package is **deprecated and frozen at 0.21.0**; all future
  releases ship as `brookmd`.

## 0.21.0 — 2026-07-20

### Added

- **New subpath exports** — `flux-md/html-to-react`, `flux-md/block-props`,
  and `flux-md/worker-core` expose the pure HTML tokenizer, block-prop
  helpers, and the backend-agnostic worker state machine for custom
  renderers. These are the building blocks the upcoming React Native
  package consumes; no behavior change for existing imports.
- **Native bindings in the repository** (experimental, not yet published as
  packages): Swift (SPM, iOS + macOS), Kotlin/Android, a plain C ABI crate,
  and a Flutter/Dart scaffold — every boundary emits the same versioned wire
  JSON (`WIRE.md` v1.0.0), pinned by golden byte-equality tests per language.

### Performance

- **Streaming worst-case campaign: five previously quadratic shapes are now
  linear**, with byte-identical output (streamed views equal one-shot renders
  at every chunk boundary; verified by the perf gate, mid-stream parity
  sweeps, and fuzzing):
  - an open, unclosed `$…` inline-math span (a streaming formula before its
    closer): 64 KB in 1 KB chunks 154 ms → 10 ms;
  - a growing all-alphanumeric word with autolinks on: 103 ms → 5.5 ms;
  - an unterminated raw tag with `unsafeHtml`: 78 ms → 2.6 ms;
  - emphasis soups held open by the CommonMark mod-3 rule: 247 ms → 5.3 ms;
  - ever-deepening blockquote nesting: depth-110 staircase 341 ms → 1.0 ms;
  - bonus: blockquotes/alerts whose first line hasn't completed now engage
    their incremental caches mid-line (1005 ms → 487 ms at 64 KB).

## 0.20.1 – 0.20.3 — 2026-07-15 *(backfilled)*

- 0.20.1: streaming divergence swaps preserve the rendered view during
  incremental re-merges (no collapse until final); `doFinalize` wire-loss
  fix; fuzz-caught dangling-`[text](` mis-parse and reattach re-feed fixes.
- 0.20.3: an open raw `<a href="…` under `unsafeHtml`/sanitize no longer
  flashes the URL while streaming; a changed block keeps its position's id
  across a divergence swap, so stateful component overrides survive.

## 0.20.0 — 2026-07-02

### Added

- **Streaming links render cleanly from the first character.** An open link's
  label used to show as raw bracketed text (`[Earnings Call](`) until the URL
  started arriving, then snap into an anchor. The label now renders inside an
  inert pending anchor from its first character — no brackets, no raw URL, and
  the pending HTML is byte-stable through the whole URL so completion is a
  single attribute swap (no DOM churn). Pending anchors carry
  `data-flux-pending=""` (gone the moment `href` lands, never in final output)
  so you can style them like settled links immediately; the optional theme
  does this out of the box. Deliberate exclusions so common non-link brackets
  never flash as links: footnote refs (`[^1]`), task checkboxes, alert
  markers, and all-digit citation labels (`[1]`, `[12]`).
- **`gfmTagfilter`** — the GFM "Disallowed Raw HTML" extension, opt-in like
  the other GFM options (`gfm-tagfilter` on the custom element,
  `with_gfm_tagfilter` in the core). With raw HTML enabled, the nine
  page-hijacking tags (`title`, `textarea`, `style`, `xmp`, `iframe`,
  `noembed`, `noframes`, `script`, `plaintext`) get their leading `<`
  escaped, matching GitHub's rendering. The GFM extension suite is now 24/24.

### Performance

- **Fourteen classes of streaming O(n²) eliminated.** An adversarial
  multi-agent audit probed 137 document shapes, confirmed 17 quadratic
  root-cause groups at the work-counter level, and this release fixes 14 —
  every one now streams in O(new bytes) and is pinned linear by the
  deterministic complexity gate (now three counters: slow-path scans,
  inline-render bytes, emitted bytes). Highlights at 512 KB streamed in
  256-byte chunks unless noted:
  - **CRLF input** made every incremental cache bail — a plain list cost
    49.4 s; now line endings normalize at ingest and CRLF streams cost the
    same as LF (210 ms).
  - **Footnotes**: a no-blank run of definitions stalled commits and every
    append recloned the footnote maps — a list with per-item refs cost 48 s
    at 256 KB; now 200 ms.
  - **Link-reference definitions inside blockquotes** armed no cache
    (44 s → 134 ms); lazy continuation lines no longer disarm quote caches.
  - **Open list items with multi-line bodies** re-rendered whole every
    append (23 s → 0.3 s); legal interior blank lines no longer permanently
    disarm the list/indented-code caches (5 s → 54 ms).
  - **Tables**: the growing trailing row re-split and re-rendered every
    append (23.8 s → 254 ms; a thousand-column row 25.4 s → 65 ms).
  - **`blockData` mode** disabled the container cache outright (a 256 KB
    alert cost 41.7 s → 226 ms, 185×) and rebuilt code/math/list/table data
    channels from scratch per append (512 KB math fence 7.4 s → 132 ms).
  - **Streaming component blocks** (`<Chart>` bodies), giant headings,
    thematic breaks, and growing fence info strings had no incremental cache
    at all (a 256 KB component body 48 s → 345 ms).
  - **Inline engine**: emphasis edits now apply in one forward pass instead
    of per-pair splicing, unpairable delimiters no longer rescan the whole
    stack (the CommonMark mod-3 pathology was 54 s for a one-shot 256 KB
    render — now 51 ms, and it hit server-side rendering too), unmatched `$`
    with math enabled no longer rescans to end-of-input per candidate
    (57.7 s → 33 ms), and space-free text (entities, long tokens) commits
    incrementally instead of pinning the whole paragraph.
  - HTML blocks whose appends landed exactly on line boundaries dropped
    their cache every append (7.7 s → 59 ms).
- WASM binary: 197 KB (+28 KB), the cost of seven new incremental caches.

### Fixed

- CRLF documents no longer leak raw `\r` bytes into rendered code blocks;
  CRLF output is byte-identical to the LF equivalent (line endings are
  equivalent per CommonMark).
- Two latent mid-stream parity divergences (speculative rendering at
  whitespace-only tails; a link-reference definition on an alert's first
  body line) now match the one-shot render exactly.
- A link awaiting its title (`[label](url "ti…`) no longer flashes literal
  source mid-stream.

## 0.19.0 — 2026-06-30

### Added

- **`decorators` — wrap/replace matched inline text while streaming.** A
  declarative matcher list (`{ match: RegExp | string, replace: (text, groups) =>
  node, skipInside?: string[] }`) on `<FluxMarkdown>` (React) and the DOM mount
  options, applied to inline **text nodes only** after parsing — so it never sees
  link URLs, code, or markup (no avoidance rules to hand-roll), and it runs once
  per committed block, staying linear over a stream. Wrapping matched figures
  (e.g. `$2.5B`, `10-15%`) is a one-liner. Decorator output is a **trusted**
  surface (like `components`); `safeUrl` is now exported and `wrapLink(text, {
  href })` ships as the safe link path. The `decorators` prop must be
  referentially stable (hoist/memoize) — a dev-mode warning fires if it isn't,
  since an unstable prop would re-decorate every committed block each tick.
- **`urlTransform`** — rewrite `href`/`src`/`poster` URLs (image proxy, allowlist,
  relative resolution). The output is re-sanitized through the same scheme filter,
  so a transform can't introduce a dangerous URL.

### Performance

- **Nested lists now stream in O(n) instead of O(n²).** A loose outer list with
  indented sub-bullets — and any list whose items have multi-line or nested-block
  bodies — used to make the incremental list cache bail to a full reparse on every
  appended chunk (re-scanning the whole growing list). It now renders each item's
  full body, nested sub-lists included, through the shared item renderer, so it
  stays linear. Streamed and one-shot output are byte-identical. (WASM −0.3 KB.)

## 0.18.5 — 2026-06-30

### Performance

- **Blockquotes and GFM alerts with structured bodies now stream in O(n) instead
  of O(n²).** When a `>` blockquote or `> [!NOTE]` alert contains a list, table,
  nested quote, heading, or code block, the incremental container cache used to
  bail to a full reparse on every appended chunk — re-scanning and re-rendering
  the whole growing block, so a long quoted list or alert-with-list went
  quadratic (a 256 KB body streamed in small chunks did ~250× the parse work of a
  16 KB one). It now renders the `>`-stripped inner through a recursive nested
  parser, committing settled inner blocks and re-rendering only the open tail, so
  the work is linear in document size. Streamed and one-shot output stay
  byte-identical. (WASM +3.8 KB.)

### Internal

- A deterministic complexity-scaling gate (`cargo test --features perf_counters
  --test scaling`), a proptest chunk-independence parity suite, and a cargo-fuzz
  parity target now run in CI to catch O(n²) streaming regressions and chunk-
  boundary divergences before they ship. The container regression above was
  surfaced by the new gate on its first run.

## 0.18.4 — 2026-06-29

### Fixed

- **Blockquote / alert inner content flattened mid-stream (same flicker class as
  0.18.3's nested lists).** The container (blockquote / GFM alert) cache rendered
  ALL inner content as plain paragraph text while streaming, so a list, nested
  blockquote, heading, setext heading, fenced or indented code, table, thematic
  break, HTML block, ordered list (incl. `start ≠ 1`), or link-reference
  definition inside a `>` block showed as escaped paragraph text until finalize,
  then snapped into its real structure. The cache now bails to the full reparse
  whenever an inner line is anything other than plain paragraph prose. Found by
  fuzzing the streaming prefix-parity invariant (the streamed view must equal a
  one-shot parse at **every** prefix) over ~15k construct interactions plus an
  adversarial corpus; streamed output now matches one-shot at every prefix for
  these shapes.

### Internal

- Removed a dead struct field and an unnecessary `mut` left by recent changes
  (clean build, no warnings).

## 0.18.3 — 2026-06-29

### Fixed

- **Nested bullets flattened mid-stream (a visible list reflow).** While
  streaming a *loose* outer list (items separated by a blank line) whose items
  contain indented nested sub-bullets, the incremental list fast path treated a
  2-space-indented sub-bullet marker as a top-level **sibling** (it accepted any
  marker within `edge + 3` columns). So the moment the outer list's second item
  began streaming, the first item's nested `<ul>` **collapsed into flat top-level
  items**, then re-nested at finalize — a jarring "indentation disappears then
  comes back" flicker. The sibling test now uses the first item's content column,
  so a marker at or past it correctly nests (the cache bails to the full reparse,
  which renders the nesting). Streamed output now matches a one-shot parse at
  **every prefix**; the only remaining list change while streaming is the
  inherent tight→loose spacing, which a non-streaming parser shows too.

## 0.18.2 — 2026-06-29

### Fixed

- **Streaming O(n²) cliff on a paragraph followed by a long link-reference /
  footnote definition run** (e.g. reference-heavy LLM output: prose, then a
  block of `[id]: url` definitions). The paragraph stayed speculative until
  `finalize()` — a definition is not a renderable block, so the paragraph never
  became "the last block" and `committed_offset` stalled, re-scanning the whole
  growing definition run on every append. A 235 KB document streamed at a
  256-byte chunk took **~59 s**; it now takes **~20 ms**, and streaming is linear
  in document size across all chunk sizes. A renderable block followed by a
  definition run now commits (a definition only parses at a block boundary, so
  the block is closed). Narrow behavior note, within the existing
  forward-reference limitation: the single paragraph immediately before such a
  run now commits before the later definitions, so a *forward* reference from it
  renders literally instead of resolving at finalize — consistent with every
  earlier paragraph, which already commits mid-stream.

## 0.18.1 — 2026-06-29

Performance + size pass. No API or output changes — CommonMark 652/652 and
GFM 23/24 are byte-for-byte unchanged.

### Changed

- **WASM binary −9.6 KB (175.1 KB → 165.5 KB, −5.4%).** Three levers, measured:
  a compact stable merge sort replaces the standard library's general-purpose
  stable sort (driftsort) at the two sort sites (−7.3 KB incl. simpler escape
  codegen); `wasm-opt` switches from `-O3` to `-Oz` (−2.3 KB) — and since the
  Rust codegen is already `opt-level=z`, `-Oz` is a Pareto win (equal-or-slightly
  faster parse, never slower, in a Node WASM A/B).
- **Faster HTML escaping.** `escape_html` / `escape_attr` now scan bytes and copy
  plain runs with one `push_str` (a memcpy) instead of decoding + re-encoding
  every character. Output is byte-identical (only ASCII `< > & " '` are
  rewritten). Measured **+9–23%** parse throughput on escape-heavy documents —
  large fenced code, display math, and HTML/list-heavy content (the common
  LLM-output shape); prose is unchanged.
- **Fewer allocations on the render path.** Paragraphs, headings, and list items
  render their inline content directly into the output buffer and trim in place,
  dropping one temporary `String` + copy per block (helps the SSR / one-shot
  `renderToString` / `parseToBlocks` path).
- **One fewer React render per patch (default path).** `<FluxMarkdown>` fed a
  changing value to `useDeferredValue` even when tail deferral was off (the
  default), so React scheduled a throwaway low-priority catch-up render every
  patch. It now feeds a stable value unless `deferTail` is set, so the default
  path renders exactly once per patch.

## 0.18.0 — 2026-06-29

### Added

- **`flux-md/server/react` subpath.** Exports `FluxMarkdownStatic` (the hookless
  RSC / SSR React component), moved here from `flux-md/server` so that the core
  server entry stays React-free (see Changed).

### Changed

- **`FluxMarkdownStatic` moved from `flux-md/server` to `flux-md/server/react`.**
  `flux-md/server` (`initFlux` / `initFluxSync` / `isFluxReady` / `parseToBlocks`
  / `renderToString`) is now genuinely **React-free**: it imports no framework, so
  a non-React build step or a Vue/Svelte SSR app can
  `import { renderToString } from "flux-md/server"` even when `react` is not
  installed. (Previously the entry failed to load without `react`, because the
  component pulled it in eagerly — contradicting the "zero React dependency"
  promise.) Update RSC/SSR imports to
  `import { FluxMarkdownStatic } from "flux-md/server/react"`.

### Fixed

- **Streaming finalize divergence (correctness).** A document streamed
  char-by-char could finalize to different HTML than the same bytes parsed in one
  shot, when the still-growing final line transiently looked like a block start
  (`#…`, `</p…`, a lone `*` / `-`) and then completed into a lazy continuation of
  the previous block (`#hashtag`, `</pre>`, `*emph*`). The penultimate block was
  committed too early and frozen, permanently splitting a block the one-shot parse
  keeps whole. The streaming commit boundary now keeps the penultimate block
  speculative across such a provisional final line.
- **Coalesced completion deferred a frame.** Under the React hooks' rAF
  coalescing (default since 0.17.0), the terminal `finalize()` patch could be
  delivered one frame late — its synchronous-flush signal was consumed by an
  earlier in-flight append patch — briefly showing a finished code block without
  its highlight / copy button. The terminal patch is now tagged `final` at the
  worker, so the sync flush binds to it regardless of how many append patches
  precede it.
- **`reset()` ghost blocks.** Swapping a streaming source mid-flight (e.g. a React
  "regenerate") could leave stale blocks from the previous content in the store,
  because an in-flight patch raced the `reset()`. A per-stream generation counter
  now drops pre-reset patches before they reach the cleared store.
- **Worker-pool robustness.** A fatally-failed worker (WASM-init failure, or a
  trap that poisoned the shared instance) is now terminated and removed from the
  pool — previously it lingered and could bypass the pool cap, leaking a worker
  per stream. A WASM trap is escalated to a fatal worker error (the stream then
  recovers onto a fresh worker) instead of being mishandled as a recoverable
  per-stream error, and `free()` on a poisoned instance is guarded so teardown
  can't throw out of the message loop.

### Security

- **O(n²) entity-decode DoS.** The numeric character-reference scan (`&#…`) was
  unbounded; input like `&#&#&#…` (no terminator) re-scanned to end-of-input on
  every `&`, freezing the single-threaded parser for seconds on a few hundred KB.
  The scan is now bounded to the longest valid reference (7 decimal / 6 hex
  digits), matching the already-bounded named-entity branch.
- **Incomplete `data:` link blocklist.** Script-capable `data:` media types
  (`image/svg+xml`, `application/xhtml+xml`, `text/xml`, `application/xml`,
  `application/javascript`, …) could render as a live link / autolink /
  component-attribute `href` — a browser navigating to one runs its script. They
  are now blocked on the href path. Inert `data:image/…` raster images via
  `![]()` are unaffected (an `<img>`-loaded SVG cannot run script).

## 0.17.0 — 2026-06-27

### Added

- **Compiled `dist/`.** The package now ships compiled, non-minified ESM
  (`dist/*.js` + `.d.ts`) instead of raw TypeScript source — fixing consumers that
  don't transpile `node_modules` (e.g. Next.js no longer needs
  `transpilePackages`) and the Socket "unusual packaging" signal. The worker and
  WASM remain separate assets so a consumer bundler still re-emits the worker
  chunk and fetches the `.wasm`.

### Changed

- WASM shadow stack reduced from 1 MB to 256 KB, cutting the WASM initial-memory
  floor from ~1088 KB to ~320 KB (memory stays growable for large documents).
- Worker→main wire format is now a JSON string (a string structured-clones far
  cheaper than an object graph); dropped `serde-wasm-bindgen` (smaller binary).
- React `useFluxStream` / `useFluxMarkdownString` default to rAF coalescing (one
  render per frame), matching the framework-neutral DOM adapter.

### Fixed

- Bounded three recursive descents in the parser (block render, link-reference
  sweep, inline-component tags) at depth 100. With the smaller shadow stack an
  unbounded descent on deeply nested input could trap and poison the worker;
  beyond the cap, content is preserved as escaped text.

## 0.16.2 — 2026-06-26

### Fixed

- **Retryable WASM init.** A transient failure fetching the `.wasm` asset (web
  path) no longer poisons every subsequent `initFlux()` / `renderToString()` —
  the cached rejected promise is dropped so the next call retries.
- **Defensive `blockData` guards.** A malformed/drifted keyed-list `items` field
  or table `rows`/`aligns`/`headers` now falls back to the full-HTML render path
  instead of crashing the streaming render. The start-only ordered-list
  renumber path is unaffected.

### Changed

- `<flux-markdown>` stream-failure logging now logs only the error *message*,
  not the raw `src` URL or the full error object (avoids a console forwarder
  shipping a tokenized URL / bulky error body to monitoring).
- Micro-perf: memoized the components normalization and hoisted `parseOpenTag`'s
  single-char regexes to module scope on the React render path.

## 0.16.1 — 2026-06-25

### Fixed

- **Streaming flash for incomplete inline links, code, and math.** While an
  inline construct is still streaming in (no closing delimiter yet), it no
  longer flashes its raw markdown source. A half-typed link renders just its
  label as an inert (non-navigable) `<a>` with the destination hidden until the
  closing `)` lands (then only `href` is added — the element is reused, not
  remounted); inline code shows `<code>…</code>` with the backtick hidden;
  inline math (`$…$`, `\(…\)`, `\[…\]`) shows the rendered `<span class="math
  …">` with the `$`/`\(` hidden. Previously these showed `[label](https://… `,
  `` `code… ``, and `$x^2 +…` as raw text until the closer arrived.
  Final output is unchanged, and an inline construct that never closes still
  finalizes to literal text, byte-identical to a one-shot parse (pinned by
  truncate-at-every-offset streaming-parity fuzz). Images, emphasis/strong, and
  reference links intentionally still render literally while open.

## 0.16.0 — 2026-06-25

### Added

- **Keyed streaming renderers (opt-in via `blockData`).** Tables, lists, and
  blockquote/alert containers now render keyed sub-blocks (`<tr>` / `<li>` /
  inner blocks), so while a block streams only the growing tail row/item
  re-renders instead of the whole block — committed rows keep their DOM
  identity (scroll/selection survive). React + vanilla DOM. Backed by new
  `ListData.items` and `ContainerData` block-data channels.
- **`onRenderMetrics` hook + render counters.** Opt-in per-block render-churn
  probe; `getMetrics()` gains `renderCount` / `rebuildCount`. Zero cost when
  unused.
- **Opt-in render/scheduling knobs (all default off):** `coalesce` (rAF patch
  coalescing for the React/store path), `deferTail` (`useDeferredValue`),
  `childMemo` (fine-grained `htmlToReact` reuse), `morphOpenBlocks` (in-place
  DOM morph of open blocks), a DOM prefix-extension tail-append fast path, and
  fine-grained tail-block signals for Solid/Vue/Svelte.

### Performance

- **Footnotes no longer disable the streaming caches.** The paragraph, list,
  table, and blockquote/alert caches now stay armed when `gfm_footnotes` is on,
  via placeholder occurrence-id tokens resolved on commit — closing the O(n²)
  tail re-scan for footnote-bearing streamed blocks. Output is byte-identical
  to a one-shot render.
- **Huge unclosed blocks stream in O(new bytes).** New incremental caches for
  open indented-code and raw-HTML blocks remove their O(n²) tail re-scan.
- Single-pass URL scheme probe and memoized keyed-table header sniffs trim two
  hot paths.

### Build & size

- The published tarball is ~32 KB gzip smaller. The WASM core is rebuilt with
  `-Z build-std` + `panic=immediate-abort` (~219 → ~178 KB), and `CHANGELOG.md`
  + a stray wasm-pack `package.json` no longer ship. **Note:** building the
  WASM now requires the nightly Rust toolchain + `rust-src`; consumers are
  unaffected (the prebuilt binary ships), and `build:wasm:stable` remains for
  stable toolchains.

### Security

- Footnote occurrence-id placeholder tokens can never leak into rendered HTML
  (defensive guard + a debug assertion exercised by the streaming fuzz corpus).

## 0.15.1 — 2026-06-22

### Security

- **XSS — dangerous-scheme autolinks are neutralized.** A CommonMark URI autolink
  (`<javascript:alert(1)>`, `<vbscript:…>`, `<file:…>`) previously emitted a live
  `href`, because autolinks bypassed the scheme allowlist that regular links go
  through. They now route through the same decode-stable dangerous-scheme filter:
  the `href` becomes `#` while the visible link text is unchanged. `file:` is now
  blocked everywhere (links, autolinks, URL attributes) — it has no legitimate use
  in rendered untrusted markdown and is a local-resource / phishing vector in
  privileged contexts (Electron, extensions, `file://` origins).
- **Component-tag / `htmlToReact` attribute hardening.** Sanitized attributes now
  also drop React-meaningful names (`dangerouslySetInnerHTML`, `ref`, `key`,
  `defaultValue`, `defaultChecked`, `suppressHydrationWarning`, …) so a hostile
  attribute can't crash the render tree or smuggle in a prop. Attribute→prop
  lookup maps are prototype-free (`Object.create(null)`), and only HTML / `data-`
  / `aria-` attribute names are forwarded to React.

### Fixed

- **ReDoS / quadratic blow-ups on untrusted input.**
  - Highlighter (`hi.ts`): the JS/TS regex-literal and bash double-quoted-string
    patterns could backtrack quadratically on crafted code blocks; both rewritten
    to linear forms, plus a 50 KB per-block size guard.
  - URL scheme check: the decode-to-fixpoint loop (Rust `is_dangerous_scheme` and
    JS `safeUrl`) is capped at 8 passes — still catches multi-encoded
    `javascript&amp;amp;#58;` payloads, no longer O(n²) on `&amp;`-spam.
  - Inline parser: nested / unbalanced link-bracket scanning is bounded
    (depth + length caps); GFM extended-autolink trailing-paren trimming is now
    linear instead of recounting the span each iteration.

### Changed

- **`flux-md/server` uses a literal `import("node:fs/promises")`** instead of a
  variable specifier, resolving the `dynamicRequire` supply-chain signal. Behavior
  is unchanged — still a Node-only, `file:`-guarded branch.
- Added a **`## Security`** / supply-chain-transparency section to the README and a
  documented **`socket.yml`** covering the inherent `nativeCode` / `networkAccess`
  / `filesystemAccess` signals (the WebAssembly core and the opt-in
  `<flux-markdown src>` fetch).

### Performance

- **No redundant re-renders / rebuilds on no-op updates.**
  - `<flux-markdown>` ignores a `setAttribute` whose value didn't change (a host
    framework re-applying identical attributes no longer tears down the self-owned
    client and reparses the whole document), and the `components` / `sanitize`
    property setters skip the remount when assigned the same identity.
  - `FluxClient.reset()` no longer notifies subscribers when the store was already
    empty — skips a wasted, output-identical render pass.
  - Documented that `sanitize` (like `components`) should be memoized/hoisted in
    React, so a fresh closure each render doesn't bust the per-block memo.
- Added render-count / node-reuse / no-remount regression tests across the React,
  DOM, store, custom-element, and Vue bindings, locking in that committed blocks
  never re-render or rebuild as the stream grows (only the streaming tail does).

### Known limitations

- Streaming a single very large **unclosed** block (a multi-megabyte indented code
  block, open HTML block, or footnote-disarmed list delivered across many chunks)
  is still O(n²) in the uncommitted-tail length. A bounded incremental cache for
  these resumable containers is tracked as follow-up; finalized / closed blocks and
  all other inputs are unaffected.

## 0.15.0 — 2026-06-17

### Added

- **Safe raw-HTML sanitizer (`htmlAllowlist` / `dropHtmlTags`)** — render a safe
  subset of *inline* raw HTML (`<br>`, `<sub>`, `<sup>`, `<mark>`, …) **without**
  `unsafeHtml`. Setting either list (even to `[]`) engages it: `htmlAllowlist`
  non-empty renders only those tags (others escaped); **empty allows all tags
  except a built-in, non-overridable dangerous set** (`script`, `style`,
  `iframe`, `object`, `embed`, `form`, `svg`, `xmp`, `plaintext`, …);
  `dropHtmlTags` removes tags entirely. Every rendered tag's attributes are
  sanitized — `on*` handlers and `style` (a CSS beacon / clickjacking vector)
  dropped, dangerous URL schemes (incl. multi-encoded) → `#`. Inline-scoped;
  block-level raw HTML stays escaped. Matching is case-insensitive.

### Fixed

- **HTML comments are dropped instead of escaped to visible text.** `<!--mk:id-->`
  (a common LLM marker) previously rendered as a literal `&lt;!--…--&gt;` run or a
  `<pre><code>` block; it now has no visible representation, in every mode except
  bare `unsafeHtml` pass-through (which keeps it verbatim for CommonMark fidelity —
  the browser ignores it either way). A comment-led block with trailing content
  keeps that content (only comment-*only* blocks are dropped).

### Security

- The dangerous-tag set is **non-overridable** (allowlisting `script`/`iframe`/`svg`
  still drops them), `style` is stripped from every sanitized/component tag, and
  raw-text elements (`xmp`/`plaintext`/`noembed`/`noframes`/`listing`) are blocked
  in allow-all mode — closing CSS-exfiltration / clickjacking / DOM-corruption
  vectors found in adversarial review. The React `htmlToReact` path mirrors the
  `style` value-filter as defense-in-depth (safe declarations like `text-align`
  still pass).

Feature-off output is byte-identical except HTML comments now drop (the
CommonMark/GFM suites run with `unsafeHtml` on, so the 652/GFM floors are
unaffected).

## 0.14.0 — 2026-06-17

### Added

- **Inline custom component tags (`inlineComponentTags`)** — the headline gap for
  rich apps. An allowlisted inline tag like `<tik symbol="AAPL">AAPL</tik>` (or
  self-closing `<tik/>`) **anywhere inline** — paragraphs, headings, list items,
  and **table cells** — renders as a real custom element with its inner parsed as
  **inline markdown** and its attributes sanitized (event handlers dropped,
  dangerous URL schemes → `#`). The React renderer dispatches it to
  `components[tag]` with the inner markdown as `children` and the attributes as
  props — **XSS-safe without `unsafeHtml`**. Independent of `componentTags`
  (block containers): list a tag under either or both. Use lowercase tag names.
- **`children` on `Component` block overrides** — a `Component` override now also
  receives the inner content pre-parsed to a React tree (`children`), so you can
  `return <Chip {...attrs}>{children}</Chip>` instead of
  `dangerouslySetInnerHTML`-ing `html`. The html-vs-children contract is now loud
  in the types and docs (an override that renders neither shows empty).
- **`flux-md/server` — worker-free synchronous SSR / RSC rendering.** The Rust→
  WASM core is a plain synchronous parser, so finished markdown renders on the
  server with no worker: `initFlux()` (async, idempotent — reads the co-located
  `.wasm` in Node, or `initFluxSync(bytes)` on edge), `renderToString(md, {
  config })` (sync HTML string, zero React dep), `parseToBlocks(md, { config })`,
  and `<FluxMarkdownStatic content config components />` — a hookless, RSC-safe
  React component that emits the same `flux-md` tree a client `<FluxMarkdown>`
  hydrates, with the same overrides (inline/block component tags dispatch on the
  server too).
- **`FluxParser.allBlocks()` (WASM)** — returns the whole parsed document as a
  block array, the one-shot render primitive used by `flux-md/server`.

### Fixed

- **Data-loss: a block component tag used inline swallowed sibling blocks.** With
  e.g. `componentTags: ["tik"]`, an inline occurrence such as
  `<tik>AAPL</tik> is up.` on a line with following content opened a block
  container that consumed the rest of the document (the paragraph and a following
  table vanished). A block component open tag must now be the **whole line** (only
  trailing whitespace after `>`); otherwise it's treated as inline and degrades
  inertly — it never eats surrounding content.

### Changed

- The React HTML→tree converter (`htmlToReact` / `parseTrustedHtml`) now preserves
  a tag's original **case** for component dispatch (so a capitalized inline tag
  like `<Cite>` maps to `components.Cite`); HTML semantics (void elements, `input`,
  close-tag matching) still compare case-insensitively, so standard output is
  unchanged.

Feature-off output is byte-identical (CommonMark 652 + GFM floors hold); both
allowlists are empty by default.

## 0.13.0 — 2026-06-04

### Added

- **`FluxClient.setContent(content, { done })` + controlled-string helpers for
  every binding** — a first-class bridge for UIs that hold a streaming message as
  a single growing/controlled string prop (rather than a stream). setContent diffs
  against the last value: a **prefix-extension** appends only the delta (committed
  blocks stay put); any **divergence** (e.g. a finished message swapped for a
  re-processed final string) resets and reparses. No hand-rolled diff, no
  readiness gate. Pass `{ done: true }` / `streaming: false` to finalize. The
  framework-neutral `setContent` is wrapped by an idiomatic, client-owning helper
  per framework — React `useFluxMarkdownString`, Vue `useFluxMarkdownString`
  (composable), Solid `createFluxMarkdownString`, Svelte `fluxMarkdownString`
  (action) — each SSR-safe (feeds only in the client-only lifecycle hook). Vanilla
  / `<flux-markdown>` use a caller-owned client + `setContent` directly.
- **`FluxPool.warm()`** — eagerly initialize one worker (`getDefaultPool().warm()`
  on app load) so the one-time WASM init is off the first-token critical path; the
  warm worker is the one the first stream attaches to, so the work isn't wasted.
- **Custom-component & `sanitize` overrides now apply to the OPEN (streaming)
  block**, not just settled ones — a design-system renderer (Tailwind classes on
  `p`/`ul`/`li`, inline `<a>`/`<code>` overrides) stays styled mid-stream instead
  of only after a block commits. This also closes a gap where a supplied
  `sanitize` previously bypassed component-rendered blocks; it now runs on every
  block. The no-`components` path is unchanged (byte-identical `innerHTML`).

### Fixed

- **Worker no longer drops the first chunk(s) under a slow WASM load.** The
  worker buffered appends but did not gate parser creation on WASM readiness, so
  an append that arrived before `init()` resolved would call `new FluxParser()`
  against an uninitialized module — throwing `fluxparser_new of undefined` and
  silently losing that chunk. Appends now accumulate (and `finalize` defers)
  until init completes, then drain in order. Surfaced on a fresh Next.js /
  Turbopack production load, where the worker+WASM fetch is slow enough to lose
  the race; the fix is bundler-agnostic. The worker's message/readiness state
  machine was extracted to `worker-core.ts` (dependency-injected, like
  `FluxPool`'s worker factory) and now has a unit test (`worker-core.test.ts`)
  covering the gate — buffer-until-ready, drain order, finalize/reset before
  ready — so the regression can't silently return.
- **React 19 / Next.js type compatibility.** The shipped source used the global
  `JSX.Element`, which React 19's `@types/react` removed — a consumer's
  `next build` type-checks flux-md's source (it ships as `.tsx`) and failed with
  *"Cannot find namespace 'JSX'"*. Now uses `ReactElement`, which type-checks
  under `@types/react` 18 **and** 19.

### Docs

- **Next.js (App Router) is now documented and verified** (Turbopack + webpack,
  Next.js 16, `next dev` and `next build`): add flux-md to `transpilePackages`
  and use it from a `"use client"` component. See the README's Next.js callout.

## 0.12.0 — 2026-05-30

### Added

- **Optional default theme — `import "flux-md/styles.css"`.** A drop-in stylesheet
  for good-looking output out of the box, **including the built-in syntax
  highlighter's colors** (without any CSS, `highlight()` output is uncolored).
  Scoped to `.flux-md`, driven by `--flux-*` CSS variables (re-theme by overriding
  a few), light by default with automatic dark via `prefers-color-scheme` (force
  with `class="flux-md flux-dark"` / `flux-light`). Opt-in and zero-runtime — the
  rendered HTML is unchanged; skip the import to bring your own CSS.

## 0.11.0 — 2026-05-30

### Added

- **Opt-in live region + root attributes** on `<FluxMarkdown>` and
  `mountFluxMarkdown`. The root accepts `className` (appended to `flux-md`),
  `id`, `role`, and `aria-live` / `aria-atomic`. Set `aria-live="polite"` to
  announce streamed content to screen readers — `polite` coalesces rapid updates
  and does **not** read every token. Off by default; covers React and the DOM
  mount (so the Web Component and the Vue/Svelte/Solid adapters too).

### Docs

- A repository root README, a "Structured block data" guide in the package
  README, and a runnable **Data Studio** demo in the playground — a
  sort/filter/CSV table and a live table of contents built entirely from
  `block.data`, mid-stream.

## 0.10.0 — 2026-05-30

Server-side rendering safety, plus an opt-in structured-data channel so consumers
build toolbars / tables of contents / charts from **data** instead of re-parsing
rendered HTML (no hast tree, no rehype).

### Added

- **SSR-safe.** `new FluxClient()` and `renderToString(<FluxMarkdown …/>)` no
  longer touch a Web Worker during construction or server render — worker
  creation is deferred to the first `append`/`pipeFrom` (client-side) — so the
  library imports and server-renders cleanly across React / Vue / Solid / Svelte.
  A fresh-process SSR cold-import check guards it in CI.
- **Structured block data — `blockData: true`** (per-stream config; opt-in,
  default off — output and CommonMark/GFM conformance are **byte-identical** when
  off). When on, `block.kind.data` carries typed structured data per kind, also
  surfaced as typed `BlockComponentProps` fields, and it **streams** in lock-step
  with the HTML:
  - **Table** → `{ headers, rows, aligns }`, cells `{ text, html }` (`props.table`)
    — sort / filter / transpose / CSV / chart.
  - **Heading** → `{ level, text, id }` (`props.heading`) — TOC with anchors.
  - **CodeBlock** → `{ lang, code }` (`props.code`) — decoded source.
  - **MathBlock** → `{ latex }` (`props.math`) — LaTeX source.
  - **List** → `{ ordered, start }` (`props.list`).

### Fixed

- Packaging: the published tarball ships the WASM deterministically on every npm
  version (build removes wasm-pack's nested `.gitignore`), with a tarball tripwire
  in CI and the publish workflow.

## 0.9.0 — 2026-05-29

Kills the React streaming boilerplate. The common case — render an LLM stream —
goes from ~17 lines of hand-rolled lifecycle to one:

```tsx
<FluxMarkdown stream={stream} />
```

### Added

- **`stream` prop on React `<FluxMarkdown>`** — pass an `AsyncIterable<string>`
  (SSE deltas), a `Response`, or a `ReadableStream<Uint8Array>` and the
  component owns an internal client, pipes the stream, supersedes it on change,
  and destroys it on unmount. The `client` prop is unchanged (now optional);
  passing a `client` keeps the existing caller-owned behavior.
- **`useFluxStream(stream, options?)` hook (React)** — same lifecycle, returns
  the owned `FluxClient` (so you can read `outline()` / `getMetrics()` or pass it
  to `<FluxMarkdown client={…} />`).
- **`pipeFrom` now also accepts an `AsyncIterable<string>`** and an optional
  `{ signal }` — the signal is checked every iteration, so an aborted stream
  appends no further chunks and is **not** finalized (and a byte reader is
  `cancel()`'d). Existing `pipeFrom(Response | ReadableStream)` calls are
  unchanged.

### Notes

- A stream is single-use, so React StrictMode's dev-only double-mount may
  truncate it in development; production mounts once and is unaffected (the
  prior manual `useEffect` form had the same caveat).
- Rules of Hooks are respected — `<FluxMarkdown>` dispatches to one of two
  sibling components, never a conditional hook.

## 0.8.0 — 2026-05-29

A self-review of 0.7.0 (adversarial multi-agent pass) fixed two robustness gaps
in the worker pool and added two small, streaming-native conveniences.

### Added

- **`FluxClient.pipeFrom(src)`** — hand it a `Response` or a
  `ReadableStream<Uint8Array>` and it reads the body, `append()`s each decoded
  chunk, and `finalize()`s. The LLM-native one-liner:
  `await client.pipeFrom(await fetch("/api/chat"))`.
- **`onBlock` option** — `new FluxClient({ onBlock })` fires once per block as it
  commits (document order), for side effects like lazily highlighting a finished
  code block or analytics. Committed blocks never re-fire.

### Fixed

- **Worker pool: a throwing stream handler no longer breaks sibling streams.** A
  user `onError` (or any handler) that threw could abort the fatal-error fan-out
  mid-loop and escape the worker message listener; dispatch is now isolated.
- **Worker pool: a fatally-failed worker is no longer re-assigned.** `pick()`
  skipped the `failed` flag, so after a WASM-init failure a new stream could be
  routed onto the dead worker and hang (a client that didn't `await whenReady()`
  had no safety net). Failed workers are now excluded from selection.
- **`<flux-markdown>`: manual `append()`/`finalize()` supersede an in-flight
  `src` fetch** (mirroring `reset()`), so mixing the two can't interleave.
- Hardened the CI/publish tarball check (explicit failure if `npm pack` yields
  no tarball) and documented the `htmlToText` core-HTML-only invariant.

## 0.7.0 — 2026-05-29

DX, robustness, and accessibility round — the streaming core (perf, CommonMark
652/652, GFM) was already comprehensive, so this release sharpens the surface
around it.

### Added

- **`onError` on `FluxClient`** — `new FluxClient({ onError })` receives worker
  and parse errors (previously only `console.error`'d). A **WASM-init failure**
  now also surfaces: `whenReady()` **rejects** instead of hanging forever, and
  `onError` fires with `{ fatal: true }`.
- **`a11y` parser option** (`ParserConfig.a11y` / `setA11y` / `<flux-markdown
  a11y>`) — opt-in accessibility markup that intentionally deviates from strict
  GFM byte-output: wraps a task-list checkbox + its text in a `<label>` (so the
  box is programmatically associated for screen readers), and adds
  `scope="col"` to table header cells. **Off by default** (conformance output
  unchanged). Streaming output stays byte-identical to one-shot.
- **`FluxClient.outline()`** — a heading table-of-contents (level / text /
  stable id) from the current snapshot, in document order; works mid-stream.
- **`FluxClient.toPlaintext()`** — the rendered document as plain text (tags
  stripped, entities decoded, blocks blank-line separated) for search indexing
  / summaries.

### Fixed

- **`<flux-markdown>` `src` race** — rapidly changing `src` (or switching
  between a `src` URL and inline `markdown`/`textContent`) could interleave two
  fetch streams into one parser, corrupting the parse tree. The element now
  supersedes any in-flight fetch (monotonic token + `AbortController`) at a
  single chokepoint.

### Docs / packaging

- README documents the one-line Vite `optimizeDeps.exclude` requirement.
- `"sideEffects": ["./src/worker.ts"]` so bundlers can drop unused framework
  adapters from the export surface.
- CI now publishes via a tag-triggered workflow with `npm publish --provenance`,
  and asserts every published tarball ships a non-empty WASM artifact.

## 0.6.0 — 2026-05-28

### Added — flux-md is no longer React-only

The core (`FluxClient` + the WASM worker) was always framework-neutral; only
the renderer was React-bound. This release adds five new entry points, each
**thin lifecycle glue** over one new framework-agnostic DOM renderer — none
re-implements the subscribe/diff loop, and none destroys your client (you own
the worker/stream).

- **`flux-md/dom`** — the foundation. `mountFluxMarkdown(client, container,
  options?) → { destroy(), refresh() }` incrementally patches a DOM subtree
  using the parser's stable block IDs: a committed block's node is never
  recreated (so one-shot work like syntax highlighting and the copy-button
  listener runs exactly once), only the streaming tail re-renders. Reuses the
  in-house highlighter for deferred code, applies your `sanitize` hook to the
  open/speculative tail, and batches patches per `requestAnimationFrame`.
  Block-kind overrides via `components` (`(props) => HTMLElement | string`);
  tag-level overrides remain React-only.
- **`flux-md/element`** — `defineFluxMarkdown(tag = "flux-markdown")` defines a
  `<flux-markdown>` custom element. Light DOM (your markdown CSS applies),
  SSR-safe (no auto-register), and usable three ways: a caller-owned `client`
  property, a self-owned client driven by `append()`/`finalize()`, or zero-JS
  via a `src` URL it fetch-streams / inline text / a `markdown` attribute.
  Config flags map to tri-state attributes (`gfm-math`, `dir-auto`, …). Covers
  **Angular** with `CUSTOM_ELEMENTS_SCHEMA` — no separate package.
- **`flux-md/vue`** — a `<FluxMarkdown>` component + `useFluxMarkdown`
  composable (Vue 3, optional peer dep).
- **`flux-md/svelte`** — a `fluxMarkdown` action, `use:fluxMarkdown={{ client }}`
  (Svelte 4 and 5, optional peer dep).
- **`flux-md/solid`** — a `<FluxMarkdown>` component (Solid, optional peer dep).
  Newest binding: its mount/teardown glue is tested, but the JSX component shell
  has only been exercised via a real `vite-plugin-solid` build, not in CI — the
  `flux-md/dom` mount inside `onMount`/`onCleanup` is the fallback if your Solid
  toolchain trips on it.

Purely additive — existing `flux-md` / `flux-md/react` / `flux-md/client` users
are unaffected (the React renderer and core are byte-identical; the only change
to existing code was a type-only import repoint so the neutral entry points
typecheck without React). `vue`, `svelte`, and `solid-js` join `react` as
optional peer dependencies — import only the binding you need. See the new
"Framework bindings" section in the README. 65 → 85 tests.

## 0.5.6 — 2026-05-28

### Performance

- **`ContainerCache` now handles multi-paragraph inner content.** A blockquote
  or GitHub alert with blank `>` lines inside (`> [!NOTE]\n> Para one.\n>\n>
  Para two.\n`) used to drop the cache and fall back to the O(n²) full path
  the moment the first blank arrived. The cache now closes the current
  paragraph on a blank `>` and starts a new one, preserving the
  streaming-O(new bytes) shape across multi-paragraph inner content. Each
  completed inner paragraph is pre-rendered into a growing
  `committed_paras_html` string; the single-paragraph fast path (the bench's
  `big_blockquote` / `big_alert`) is unchanged within noise.

- **`ListCache` now handles loose lists.** A flat list with blank lines
  between siblings (`- one\n\n- two\n\n- three\n`) is a CommonMark "loose"
  list — every item body gets wrapped in `<p>…</p>` — and the cache used to
  bail on the first blank. The cache now flips to loose on the first
  blank-then-marker sequence, re-renders prior cached items with `<p>`
  wrappers from stored source spans (one-time O(items)), and continues the
  streaming-O(new bytes) shape from there. Tight→loose is sticky.

  50 KB loose-list bench, before-fix → after-fix:

  | chunk |  before  |  after  | speedup |
  |------:|---------:|--------:|--------:|
  |  16   | 5593 ms  | 21 ms   | ~272×   |
  | 256   |  355 ms  |  7 ms   | ~49×    |

  Tight `big_list` perf is unchanged within bench noise.

### Added

- **React `CodeBlock` default renderer ships a copy-to-clipboard button.**
  Closed code blocks now show an icon + "Copy" in their header (the existing
  "streaming" pill takes that slot until close, so streaming code is never
  copy-clickable mid-arrival). Click → copies the decoded source via
  `navigator.clipboard.writeText` → swaps to a checkmark + "Copied" for
  1.5 s → reverts. Native `<button>` (keyboard-reachable), `aria-label`
  toggles between "Copy code" and "Copied" with `aria-live="polite"`,
  guards against `navigator.clipboard` being absent (SSR / insecure context)
  and rejected `writeText` promises (permission denied) — both leave the
  button silently usable. No new dependency.

### Documentation

- README quickstart now uses `useState(() => new FluxClient())` + an
  unmount-only destroy effect instead of `useMemo(() => new FluxClient(),
  [])` + cleanup-on-stream-change (which destroyed the client when the
  `stream` prop changed, leaking a freed parser on the next append).
- New "when to enable each flag" guide for `ParserConfig` with concrete
  LLM-output triggers (`gfmMath` when `$…$` arrives, `componentTags` for
  `<Thinking>` blocks, etc.) — so a reader picks flags without reading the
  full reference further down.
- `Alert` block-kind override example added to the `components` docs.
- `sanitize` example mirrors the realistic memoize-at-module-scope pattern
  from the live demo (a fresh arrow each render busts the per-block memo).
- New "Performance" section pointing to CHANGELOG / `examples/bench.rs` for
  numbers (no numbers baked into the README — those rot).

## 0.5.5 — 2026-05-28

### Performance

- 1× memcpy in the paragraph / container cache assembly (was 2×). Both caches
  were building the block HTML in two stages — concatenate
  `committed + active` into an intermediate `String`, then concatenate
  `<p>` + that into the output — so a long open paragraph or container did two
  memcpys of the committed inner per append. The fix builds directly into the
  output buffer and trims trailing whitespace in-place; the container case
  backs out a provisional `<p>` opener if the body content turns out to be
  empty (preserving the empty-body fix from 0.5.4). Output is byte-identical.

  200 KB bench (best of 7), chunk=16:

  | shape           | 0.5.4    | 0.5.5    | speedup |
  |-----------------|---------:|---------:|--------:|
  | `long_paragraph`| 142 ms   | **96 ms**| 1.48× |
  | `emphasis_para` | 170 ms   | **116 ms**| 1.47× |
  | `big_blockquote`| 213 ms   | **157 ms**| 1.36× |
  | `big_alert`     | 343 ms   | **237 ms**| 1.45× |

  Modest wins at every chunk size for the affected caches; the
  table / list / fence caches are unchanged (they were already 1× memcpy).

## 0.5.4 — 2026-05-28

### Fixed (mid-stream rendering)

- **GFM tables now form during streaming, not just at finalize.** Streaming a
  table char-by-char (or in any chunking where the delimiter row's `\n` lands
  in a different chunk than the row's content) used to leave the block as a
  `<p>` spanning both lines until `.finalize()` ran. The paragraph cache's
  delimiter-detection walked from the line AFTER the cut and so missed a
  delimiter row that completed inside the line the cut had advanced into. The
  fix re-checks the line containing the cut whenever it has just completed,
  guarded by a cheap `bytes[cut..].contains('\n')` so long open paragraphs
  without interior `\n` still take the O(new bytes) per-call path.
- **Open alerts/blockquotes with an empty body no longer render an empty
  `<p></p>`.** A `> [!NOTE]\n` shown mid-stream now matches the full renderer:
  `<div class="markdown-alert ...">…<p class="...title">Note</p></div>` with
  no empty body paragraph. The container cache was wrapping the body in
  `<p>…</p>` unconditionally, even when the body was empty.

Both bugs only manifested *before* `finalize()`. The post-finalize output —
what every existing parity test checks — was already correct, which is why
neither was caught earlier. A new `tests/midstream_parity.rs` asserts that the
streamed view of an open block matches what one-shot parsing produces for the
same prefix (tables, alerts, blockquotes, lists, code fences, math fences).

### Performance

- `big_table` at the artificial `chunk=16` stress case is ~280 ms (was ~145 ms
  in 0.5.3). The 145 ms was the *incorrect* path: the paragraph cache treated
  the whole 200 KB table as a single growing paragraph until finalize, never
  engaging the table cache. The 280 ms is the cost of correctly emitting the
  table mid-stream at the smallest chunk size. Every realistic LLM streaming
  chunk size (≥64 bytes) is unchanged — `big_table` at chunk=64 is 73 ms,
  chunk=256 is 38 ms, etc.

## 0.5.3 — 2026-05-28

### Performance

- **Streaming long open resumable containers is now O(n).** A long
  `> [!NOTE]` alert, a `>`-quoted explanation, or a flat bullet/ordered list
  used to re-run scan + inline render over the whole growing inner on every
  append (O(n²)). Three new tail caches mirror the existing fence/table
  pattern:

  - `ContainerCache` — single-paragraph blockquote / GitHub alert. Wraps
    the existing paragraph-cache (inline-boundary commit) with a
    `>`-stripped inner buffer; the wrapper HTML (`<blockquote>` /
    alert `<div>`) is built once at arm time, each new `> ` line is
    stripped once into the inner buffer, only the unsettled inline tail is
    re-rendered. Bails on a blank `>`-line (paragraph break inside the
    container), lazy continuation, or `\r`.

  - `ListCache` — tight, flat list (the LLM-emit shape: one sibling marker
    per line, no blanks, no continuation, no nesting). Opener
    (`<ul>` / `<ol start=N>`) pre-rendered at arm time; each new sibling
    line renders directly into the cache as a tight `<li>…</li>` (GFM
    task-list `[ ] `/`[x] ` supported). Bails on the first blank line
    (loose-list signal), non-marker line, over-edge marker (nested), or
    foreign-family marker — the full path handles those.

  Measured at 50 KB (best of 7), before → after:

  | shape           | chunk=16          | chunk=256       |
  |-----------------|-------------------|-----------------|
  | `big_blockquote`| 5164 → **22 ms**  | 332 → **8.5 ms**|
  | `big_list`      | 6141 → **18 ms**  | 391 → **7.4 ms**|
  | `big_alert`     | 6298 → **28 ms**  | 404 → **11 ms** |

  At 200 KB, `big_list` chunk=256 was extrapolating to ~6.2 s before the
  cache; now **36 ms** (~170×). Every realistic streaming shape now has a
  flat chunk-size curve.

  Output is byte-identical. Parity gated by `tests/container_cache.rs`
  (blockquote + all five alert kinds, dir_auto, CRLF, lazy continuation,
  multi-paragraph fallback, 400-line stress) and `tests/list_cache.rs` (5
  marker families, ordered with non-default start, dir_auto, CRLF, loose /
  nested / multi-line fallback, 400-item stress).

### Documentation

- Reworded the "future plugin slot" comments in `renderers/Math.tsx` and
  `renderers/Mermaid.tsx`. The actual extension path is the
  `components.MathBlock` / `components.Mermaid` overrides, which already
  works end-to-end.

### Known limitations

- The three new caches disarm when `gfmFootnotes` is on, mirroring
  `TableCache` from 0.5.2: cell-level `[^x]` occurrence ids would diverge
  across the cache vs. full-reparse boundary. Footnotes + a long container
  / table stays on the full O(n²) path — rare combination, may be lifted
  in a later release by tracking per-cache footnote-occ deltas.
- The blockquote/alert cache covers the *single-paragraph* inner case (the
  realistic LLM shape). A long open container with a multi-block inner
  (lists inside, fenced code inside, etc.) still routes through the full
  path. The bench's `big_blockquote` / `big_alert` are single-paragraph
  shapes — what these caches were built for.

## 0.5.2 — 2026-05-28

### Performance

- **Streaming a long GFM table is now O(n) at every chunk size.** Tables already
  rendered visually incrementally (header at the delimiter row, rows append as
  they arrive) — but `render_table` re-walked every row on every append, so the
  total work was O(n²) once chunks exceeded ~30 bytes (a row). The fix is an
  incremental `TableCache` that mirrors the existing code/math `FenceCache`:
  `<thead>` is pre-rendered once, each newly-complete `<tr>` is folded into the
  cached prefix, and only the trailing partial row is re-rendered each append.
  Output is byte-identical; parity gated by `tests/table_cache.rs` (every chunk
  size 1..=9 × char-by-char against one-shot, with alignments, inline markdown,
  link refs, CRLF fallback, and a 400-row stress case).

  Measured on a 200 KB table (best of 7 — chunk varies on each row):

  | chunk |  before  | after | speedup |
  |------:|---------:|------:|--------:|
  |    16 |   143 ms | 145 ms | ~1× (was already fast) |
  |    64 | 20807 ms |  78 ms | **267×** |
  |   128 | 10414 ms |  54 ms | **193×** |
  |   256 |  5373 ms |  40 ms | **134×** |
  |   512 |  2608 ms |  34 ms |  **77×** |
  |  1024 |  1322 ms |  31 ms |  **43×** |

  The pre-fix bench printed only chunks 16 and 256, which hid the regression
  (16 was fine, 256 was the cliff floor). The bench now sweeps 16/64/128/256/
  512/1024 so the next regression in this shape can't slip in unnoticed.

  Footnotes are the one combination still on the full O(n²) path: the
  cell-level `[^x]` occurrence counter would diverge across the
  cache/full-reparse boundary, so the cache disarms when `gfmFootnotes` is on
  (rare enough to defer to a later release).

## 0.5.1 — 2026-05-27

### Performance

- A document with a very large number of link-reference definitions is now O(n)
  instead of O(n²). The committed reference table was cloned on every append
  (O(refs) per chunk); it's now shared into each render via an `Rc` (O(1)) with a
  two-level lookup (committed, then the uncommitted tail), and folded in place
  via `Rc::make_mut` once the render's clone is dropped. A 235 KB
  reference-definition stream at 16-byte chunks: **~1,395 ms → ~53 ms** (~26×).
  This was believed to be the last remaining O(n²) streaming shape; in fact a
  long open GFM table was still O(n²) (fixed in 0.5.2 — `big_table` at
  chunk=256 went from ~5,400 ms to ~40 ms). Output is unchanged.

## 0.5.0 — 2026-05-27

### Fixed

- **Streaming GFM tables now render incrementally.** A table no longer waits for
  the whole block to arrive: the header renders the moment the delimiter row
  (`|---|`) streams in, and each body row appends as it arrives. Previously the
  incremental paragraph fast-path kept extending the header line as a paragraph
  and only formed the table on a full reparse, so a streaming table appeared all
  at once. The fast-path now bails (like it does for a setext underline) when a
  delimiter row forms a table with its preceding header. Output is unchanged for
  one-shot parsing; streamed output now matches one-shot at every prefix.

### Added

- **`<FluxMarkdown sanitize={fn} />`** — an optional HTML sanitizer hook. When
  provided, flux-md runs every block's HTML through it before injecting via
  `innerHTML`, **including the streaming (open/speculative) tail** that the raw
  fast path would otherwise expose. Bring your own sanitizer (e.g.
  `DOMPurify.sanitize`) to render untrusted / LLM HTML with `unsafeHtml` on;
  flux-md stays zero-dep. Built-in code/math renderers (already-escaped content)
  are not run through it, so highlighting and math markup are preserved. Omitting
  the prop is byte-identical and zero-cost.

## 0.4.0 — 2026-05-27

### Added

- **`componentTags`** — opt-in custom component tags. List tag names (e.g.
  `componentTags: ['Thinking', 'Callout']`) and a `<Thinking>…</Thinking>` in the
  stream renders as a component whose **inner content is parsed as markdown** —
  safely, **without `unsafeHtml`**: the tag is allowlisted and its attributes are
  sanitized (event handlers dropped, dangerous URL schemes neutralized). The
  container spans blank lines (unlike a raw HTML block) up to its matching close
  tag, supports nesting, and ignores a `</Tag>` inside a code fence. Each renders
  as a `Component` block dispatched on the React side via `components[tag]` (e.g.
  `components.Thinking`) or the generic `components.Component`, receiving `{ tag,
  attrs, … }`. Off unless configured; tag names match case-sensitively.

### Performance

- Streaming a long open display-math block (`$$…$$` / `\[…\]`) is now O(n)
  instead of O(n²). The incremental fence cache that already covered code fences
  was generalized to math fences: an append only escapes the newly arrived lines
  instead of re-scanning and re-escaping the whole growing body. Measured on a
  200 KB `$$…$$` block at 16-byte chunks: **16,271 ms → ~93 ms** (~174×). Output
  is byte-identical (gated by `tests/math_fence_cache.rs`).
- A long trailing run of link-reference / footnote definitions now commits
  incrementally instead of being re-scanned on every append. Previously such a
  run produced no renderable blocks, so the committed offset never advanced. A
  document ending in a large reference section streams ~10× faster (235 KB at
  16-byte chunks: **13,799 ms → ~1,380 ms**). Output is byte-identical (gated by
  `tests/ref_defs_streaming.rs`).

## 0.3.2 — 2026-05-27

### Documentation

- Rewrote the README to describe flux-md on its own terms and removed all
  references to and comparisons with other libraries. No code changes — the
  published API and behavior are identical to 0.3.1.
- Fixed the React quick-start example: import `useEffect` and guard the async
  append loop so it can't run after unmount or a stream change.

## 0.3.1 — 2026-05-27

### Performance

- Streaming a long unbroken paragraph is now O(n) instead of O(n²) — including
  paragraphs **dense with inline constructs** (emphasis, code spans, links,
  inline math), not just plain text. The open paragraph commits its settled
  prefix and re-renders only the short active tail. Because inline output isn't
  prefix-stable (a late `*` re-emphasizes earlier text, a late backtick opens a
  code span), the stable boundary is computed inside the inline renderer itself:
  it tracks unmatched openers, unpaired forward-pairable emphasis, and resolved
  emphasis spans, and commits only up to the largest provably-final cut. Output
  is byte-identical. Measured on 200 KB single paragraphs at 16-byte chunks:
  plain **34,167 ms → ~130 ms** (~260×); emphasis-rich **60,569 ms → ~157 ms**
  (~386×).
- The open-code-fence fast path no longer clones the accumulated escaped body on
  every append; it assembles the block HTML directly from the cached pieces,
  dropping one full O(body) copy per append. A 200 KB fence streams in **~82 ms**
  at 16-byte chunks (was ~154 ms, ~1.9×). Output is byte-identical.

## 0.3.0

### Added

- **`gfmMath`** — opt-in math. Inline `$…$` and `\(…\)`; display `$$…$$` and
  `\[…\]`. Inline `$` uses the pandoc rule, so currency like `$5 and $10` stays
  literal. Emits KaTeX-ready markup (`<span class="math math-inline">` /
  `<div class="math math-display">`) carrying the LaTeX as text content — bring
  your own KaTeX (flux-md stays zero-dep) or override `components.MathBlock`
  (which receives the LaTeX as `text`). Display fences are blank-line tolerant
  and stream incrementally. Off by default.
- **`dirAuto`** — opt-in per-block `dir="auto"` on block-level text elements
  (`p`, `h1`–`h6`, `blockquote`, `ul`/`ol`/`li`, `table`, alerts, footnotes), so
  the browser detects each block's direction (RTL/LTR) independently in
  mixed-language documents. Code blocks stay LTR. Off by default.

### Performance

- Streaming a long fenced code block is now **O(n) instead of O(n²)**: an open
  code fence caches its escaped body and extends it by only the newly arrived
  lines. Measured on a 200 KB fence — **14,278 ms → 230 ms** at 16-byte chunks,
  **898 ms → 22 ms** at 256-byte chunks. Output is byte-identical.
- Dropped a redundant per-append clone of the link-reference table.

### Known limitations

- Streaming a very long **unbroken** paragraph (no blank lines) is still O(n²):
  inline rendering re-runs over the whole paragraph each chunk, and unlike code
  it can't be prefix-cached (a late `*` can emphasize earlier text). Tracked for
  a future release; breaking the text into paragraphs avoids it.

### Internal

- Added a Rust streaming-throughput benchmark (`cargo run --release --example
  bench`) plus char-by-char streaming-parity tests for the code-fence cache,
  math, and bidi paths.

## 0.2.0

- Initial public release: zero-dep streaming markdown, Rust→WASM core, one Web
  Worker per stream, CommonMark 0.31 (652/652) + GFM (tables, strikethrough,
  task lists, extended autolinks, GitHub alerts, footnotes).

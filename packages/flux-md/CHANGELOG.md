# Changelog

Notable changes to flux-md. Format based on
[Keep a Changelog](https://keepachangelog.com/); this project aims to follow
[Semantic Versioning](https://semver.org/).

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

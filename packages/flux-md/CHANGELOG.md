# Changelog

Notable changes to flux-md. Format based on
[Keep a Changelog](https://keepachangelog.com/); this project aims to follow
[Semantic Versioning](https://semver.org/).

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

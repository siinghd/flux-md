# Changelog

Notable changes to flux-md. Format based on
[Keep a Changelog](https://keepachangelog.com/); this project aims to follow
[Semantic Versioning](https://semver.org/).

## Unreleased

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

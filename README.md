# flux-md

[![npm](https://img.shields.io/npm/v/flux-md.svg)](https://www.npmjs.com/package/flux-md)
[![CI](https://github.com/siinghd/flux-md/actions/workflows/ci.yml/badge.svg)](https://github.com/siinghd/flux-md/actions/workflows/ci.yml)
[![license](https://img.shields.io/npm/l/flux-md.svg)](LICENSE)

**Zero-dep streaming markdown for the browser.** A Rust→WASM core with one pooled
Web Worker per stream, incremental parse with speculative closure for mid-stream
constructs, and stable block identities so unchanged blocks never re-reconcile.

Wire each LLM stream to a `FluxClient` and the markdown renders incrementally
**off the main thread**, block by block — so many concurrent streams render
without melting the UI thread. 100% CommonMark 0.31 + GFM.

**[Live demo](https://md.hsingh.app/)** · **[Full docs &amp; API →](packages/flux-md/README.md)** · **[Changelog](packages/flux-md/CHANGELOG.md)**

```bash
npm i flux-md
```

```tsx
import { FluxMarkdown } from "flux-md/react";

// `stream` is an AsyncIterable<string> (SSE deltas), a Response, or a ReadableStream
<FluxMarkdown stream={stream} />;
```

## Highlights

- **Off the main thread** — a pooled Web Worker per stream; the parser re-parses
  only the active tail on each token, and heavy renderers (highlighting, math,
  mermaid) defer until a block closes.
- **SSR-safe** — imports and `renderToString` cleanly on the server across React,
  Vue, Solid, and Svelte; the worker is created lazily on the client.
- **Structured `block.data` channel** *(opt-in, default off)* — tables, headings,
  code, math, and lists are exposed as **typed, streaming data** on
  `block.kind.data`, so you build toolbars (sort/filter/CSV), tables of contents,
  charts, and copy buttons from data — no HTML re-parsing, no AST tree to walk.
- **Renderers for every stack** — React, Vue 3, Svelte (4 & 5), Solid, a
  framework-agnostic `<flux-markdown>` Web Component, and a vanilla DOM mount.
- **Zero runtime dependencies.** The whole engine is one WASM binary plus a small
  TypeScript client.

See the **[package README](packages/flux-md/README.md)** for the full API,
per-stream config, framework bindings, security model, and scaling helpers
(`virtualize`, `stickToBottom`).

## Repository layout

| Path | What |
|------|------|
| [`packages/flux-md`](packages/flux-md) | The published npm package — TS client + renderers, and the full docs. |
| [`crates/flux-md-core`](crates/flux-md-core) | The Rust parser/renderer compiled to WASM (built on demand; not committed). |
| [`web`](web) | The live demo / playground ([md.hsingh.app](https://md.hsingh.app/)). |

## Development

```bash
bun install
bun run build:wasm        # compile the Rust core → WASM
cd packages/flux-md && bun test
```

CI enforces the CommonMark 652/652 + GFM conformance floors, the JS test suite, a
fresh-process SSR cold-import check, and that the published tarball ships the WASM.

## License

[MIT](LICENSE)

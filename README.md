# brookmd

[![npm](https://img.shields.io/npm/v/brookmd.svg)](https://www.npmjs.com/package/brookmd)
[![CI](https://github.com/siinghd/brookmd/actions/workflows/ci.yml/badge.svg)](https://github.com/siinghd/brookmd/actions/workflows/ci.yml)
[![license](https://img.shields.io/npm/l/brookmd.svg)](LICENSE)

**Streaming markdown for every platform.** One Rust core — incremental parse
with speculative closure for mid-stream constructs, stable block identities so
unchanged blocks never re-reconcile — compiled to WASM for the web and to
native libraries for mobile and desktop. Every boundary emits the same
versioned JSON wire, byte-for-byte. 100% CommonMark 0.31 + GFM — 652/652 and
24/24 **byte-exact** against the reference renderers.

In the browser, wire each LLM stream to a `BrookClient` and the markdown
renders incrementally **off the main thread**, block by block — so many
concurrent streams render without melting the UI thread.

**[Live demo](https://md.hsingh.app/)** · **[Full docs &amp; API →](packages/brookmd/README.md)** · **[Changelog](packages/brookmd/CHANGELOG.md)**

```bash
npm i brookmd
```

```tsx
import { BrookMarkdown } from "brookmd/react";

// `stream` is an AsyncIterable<string> (SSE deltas), a Response, or a ReadableStream
<BrookMarkdown stream={stream} />;
```

## Agent skill

Teach your coding agent brookmd in one command — setup, framework bindings,
`components` overrides, math, highlighting, security, styling and troubleshooting,
with typechecked examples:

```bash
npx skills add siinghd/brookmd        # installs into every detected agent
npx skills add siinghd/brookmd -a claude-code   # …or pick one
```

In Claude Code you can install it as a plugin instead:

```
/plugin marketplace add siinghd/brookmd
/plugin install brookmd@brookmd
```

Agents that read the repo or the site directly can start from
[`llms.txt`](llms.txt) — also served at
[md.hsingh.app/llms.txt](https://md.hsingh.app/llms.txt), with the whole
documentation set concatenated at
[md.hsingh.app/llms-full.txt](https://md.hsingh.app/llms-full.txt).

## Highlights

- **Off the main thread** — a pooled Web Worker per stream; the parser re-parses
  only the active tail on each token, patches cross the worker boundary as
  verified splices (emitted bytes stay O(n) even for one giant growing block —
  wire delta mode, contract v1.2.0), and heavy renderers (highlighting, math,
  mermaid) defer until a block closes.
- **SSR-safe** — imports and `renderToString` cleanly on the server across React,
  Vue, Solid, and Svelte; the worker is created lazily on the client.
- **Structured `block.data` channel** *(opt-in, default off)* — tables, headings,
  code, math, and lists are exposed as **typed, streaming data** on
  `block.kind.data`, so you build toolbars (sort/filter/CSV), tables of contents,
  charts, and copy buttons from data — no HTML re-parsing, no AST tree to walk.
- **Renderers for every stack** — React, Vue 3, Svelte (4 & 5), Solid, a
  framework-agnostic `<brook-markdown>` Web Component, and a vanilla DOM mount
  on the web; a React Native renderer and Swift/Kotlin/Flutter bindings over
  the native core (experimental — see [Platforms](#platforms)).
- **Zero runtime dependencies.** The whole engine is one WASM binary plus a small
  TypeScript client.

See the **[package README](packages/brookmd/README.md)** for the full API,
per-stream config, framework bindings, security model, and scaling helpers
(`virtualize`, `stickToBottom`).

## Repository layout

| Path | What |
|------|------|
| [`packages/brookmd`](packages/brookmd) | The published npm package — TS client + renderers, and the full docs. |
| [`crates/brookmd-core`](crates/brookmd-core) | The Rust parser/renderer, published to [crates.io](https://crates.io/crates/brookmd-core); compiled to WASM for the npm package. Emits the versioned JSON [wire contract](crates/brookmd-core/WIRE.md). |
| [`crates/brookmd-ffi`](crates/brookmd-ffi) | uniffi wrapper over the core for native targets (React Native, Swift, Kotlin). |
| [`crates/brookmd-cabi`](crates/brookmd-cabi) | Plain C-ABI wrapper (Dart/Flutter and any C FFI consumer). |
| [`packages/brookmd-react-native`](packages/brookmd-react-native) | React Native renderer over the native core, published as [`brookmd-react-native`](https://www.npmjs.com/package/brookmd-react-native) (experimental). |
| [`packages/brookmd-flutter`](packages/brookmd-flutter) | Flutter/Dart scaffold over the C ABI (experimental). |
| [`bindings/kotlin`](bindings/kotlin) | Kotlin/Android bindings (experimental). |
| [`bindings/swift`](bindings/swift) | Swift package (iOS + macOS) bindings (experimental). |
| [`web`](web) | The live demo / playground ([md.hsingh.app](https://md.hsingh.app/)). |

## Platforms

The same Rust core streams the same versioned JSON wire
([WIRE.md](crates/brookmd-core/WIRE.md)) across every boundary; golden tests
pin every binding to byte-identical output.

| Platform | Use | Status |
|----------|-----|--------|
| Browser / Node / SSR | [`brookmd`](https://www.npmjs.com/package/brookmd) on npm (React, Vue, Svelte, Solid, Web Component, DOM, server) | **Stable** — published |
| Rust | [`brookmd-core`](https://crates.io/crates/brookmd-core) on crates.io | **Stable** — published |
| React Native (iOS + Android) | [`brookmd-react-native`](https://www.npmjs.com/package/brookmd-react-native) on npm — native parser via JSI, RN renderer | Experimental — published; app-level e2e on an Android emulator + iOS simulator in CI; physical-device validation pending |
| iOS / macOS (Swift) | [`bindings/swift`](bindings/swift) — SPM package `BrookMd` over an XCFramework | Experimental — CI-built; goldens pass on an iOS Simulator; not yet on a registry |
| Android (Kotlin) | [`bindings/kotlin`](bindings/kotlin) — Android library + uniffi bindings | Experimental — CI-built; instrumented goldens pass on an Android emulator; not yet on Maven |
| Flutter / Dart | [`packages/brookmd-flutter`](packages/brookmd-flutter) over the C ABI | Experimental — scaffold |
| Anything with a C FFI | [`crates/brookmd-cabi`](crates/brookmd-cabi) + `include/brook_md.h` | Experimental — tested on host |

Swift/Kotlin/C-ABI bindings ship prebuilt from CI as checksummed
[release assets](https://github.com/siinghd/brookmd/releases) (Android `.so`
per ABI, Apple XCFrameworks with iOS + macOS slices); the React Native package
vendors its binaries in the npm tarball.

**What each layer gets.** Everything in the Rust core — byte-exact
CommonMark/GFM output, the streaming caches and O(new-bytes) guarantees, the
sanitizers, and every `ParserConfig` feature — is identical on **all**
platforms above. The client-layer optimizations live in the npm package's
TS/renderer layer and are **web-path only**: streaming syntax highlighting,
incremental DOM application, `hydrate()` instant reopen, and the
`retainCommittedHtml` memory default (native bindings consume `allBlocks()`
and keep full retention). Native hosts render the wire themselves, so those
concerns — and those optimizations — belong to their renderer layer.

## Development

```bash
bun install
bun run build:wasm        # compile the Rust core → WASM
cd packages/brookmd && bun test
```

CI enforces the conformance floors — **652/652 CommonMark 0.31 and 24/24 GFM,
byte-exact**, not merely structurally normalized — as the harnesses' default
floors (`CMARK_MIN_EXACT` / `GFM_MIN_EXACT`, pinned explicitly in the
workflows), plus the JS test suite, a fresh-process SSR cold-import check, and
that the published tarball ships the WASM.

The only differences from the reference output are deliberate brookmd choices,
folded by a documented `canonicalize` step applied to **both** sides before the
byte comparison (so it can never hide a structural divergence): the security-only
`target="_blank" rel="noopener noreferrer nofollow"` on links, `data-lang` on code
blocks, HTML5 void elements (`<br>`) where the spec prints XHTML (`<br />`), and
the modern `style="text-align:…"` in place of GFM's deprecated `align="…"`.

## License

[MIT](LICENSE)

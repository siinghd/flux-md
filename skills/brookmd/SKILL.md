---
name: brookmd
description: >-
  Render streaming markdown with brookmd — a Rust→WASM core in a Web Worker that
  incrementally parses an LLM token stream, speculatively closes mid-stream
  constructs, and keeps stable block identities so committed blocks never
  re-render. Use this skill when: installing or setting up brookmd
  (`npm i brookmd`); rendering streaming AI/LLM markdown in React, Vue, Svelte,
  Solid, a `<brook-markdown>` Web Component, or vanilla DOM; wiring a chat UI to
  the Vercel AI SDK, `useChat`, SSE deltas, or a growing string; SSR / Next.js
  App Router / Vite / RSC integration; overriding rendering with `components`,
  `decorators`, `blockData` toolbars, or component tags; math, KaTeX, or Mermaid;
  syntax highlighting, Shiki, or `registerLanguage`; security — `sanitize`,
  `unsafeHtml`, `htmlAllowlist`, `allowSchemes`, `urlTransform`, `onLinkClick`;
  styling, Tailwind, dark mode, or the streaming caret; performance —
  `virtualize`, `stickToBottom`, `hydrate`/`persist`, `warm()`, `getMetrics()`;
  or troubleshooting a blank `.brook-md`, a worker/WASM load failure, or blocks
  re-rendering on every patch.
---

# brookmd

Streaming markdown for the browser. One Rust core (CommonMark 0.31 + GFM,
byte-exact) compiled to WASM, run in a pooled Web Worker per stream, emitting
patches over a versioned JSON wire. Parsing is off the main thread and re-parses
only the active tail, so committed blocks never re-reconcile.

Zero runtime dependencies. `react` / `vue` / `svelte` / `solid-js` are optional
peers — you only need the one whose binding you import.

## Quick setup

```bash
npm i brookmd
```

```ts
import "brookmd/styles.css"; // optional theme; skip it to bring your own CSS
```

**Vite — one required line.** Pre-bundling hoists the wasm-bindgen glue into
`.vite/deps/`, which breaks the relative WASM lookup (404 / "magic word"):

```ts
// vite.config.ts
export default defineConfig({ optimizeDeps: { exclude: ["brookmd"] } });
```

**Next.js App Router** needs no `transpilePackages` and no loader config, but the
component must be `'use client'` (it uses hooks and spawns a Worker), and the
stream must be created in client code (a `Response`/`ReadableStream` is not
serializable across the RSC boundary).

## Basic usage

**From a stream** — the component owns the client, pipes the stream, supersedes
it if it changes, and cleans up on unmount:

```tsx
import { BrookMarkdown } from "brookmd/react";

// stream: AsyncIterable<string> | ReadableStream<Uint8Array> | Response
<BrookMarkdown stream={stream} />;
```

**From a growing string** — the shape most React chat UIs already have:

```tsx
import { BrookMarkdown, useBrookMarkdownString } from "brookmd/react";

const client = useBrookMarkdownString(text, { streaming });
<BrookMarkdown client={client} />;
```

`setContent` diffing is done for you: a prefix-extension appends only the delta,
a divergence resets and reparses.

> **Pass `streaming: false` once the content is final.** Omitted or `true`
> leaves the stream OPEN, so the last block never commits — a finished code
> fence never highlights and never shows its Copy button. brookmd deliberately
> refuses to infer "done" from an absent flag: doing so would re-finalize on
> every token for callers who grow the string without it, an O(n²) reparse trap.

## Chat UI defaults

These four parser flags default **off** but are what a chat app almost always
wants, plus two client-side habits:

```tsx
const CONFIG: ParserConfig = {
  softBreaks: true, // a single \n becomes <br> — the chat convention models emit for
  dirAuto: true,    // per-block bidi, so an Arabic paragraph sits correctly beside an English one
  a11y: true,       // <label>-wrapped task checkboxes, scope="col" on table headers
  blockData: true,  // typed table/heading/code/math data → toolbars without an HTML re-parse
  gfmMath: true,    // $…$ / $$…$$ → KaTeX-ready markup (off by default so `$` in prose stays literal)
};

// 1. HOIST the components map (and decorators / urlTransform / sanitize) to module
//    scope. A fresh identity each render busts the per-block memo, so every
//    committed block re-renders on every patch.
const COMPONENTS: Components = { /* … */ };

// 2. Warm the pool on route entry so WASM init is off the first-token path.
useEffect(() => { getDefaultPool().warm(); }, []);
```

## Key `<BrookMarkdown>` props

| Prop | Type | Default | Meaning |
|---|---|---|---|
| `client` / `stream` | `BrookClient` / `AsyncIterable<string> \| ReadableStream<Uint8Array> \| Response` | — | Exactly one is required; `client` wins. |
| `streamConfig` | `ParserConfig` | — | Config for the internally created client (stream mode). |
| `components` | `Components` | — | Overrides. **Two prop contracts** — see below. |
| `sanitize` | `(html: string) => string` | — | Runs before every injection **including the open tail**. |
| `decorators` | `Decorator[]` | — | Inline TEXT-node matcher. Output is **trusted**. |
| `urlTransform` | `UrlTransform` | — | Rewrites `href`/`src`/`poster`; output is re-sanitized. |
| `onLinkClick` | `(event, { href, text, element }) => void` | — | *Since 0.30.0.* One delegated listener on the root; `preventDefault()` cancels. |
| `virtualize` | `boolean` | `false` | `content-visibility: auto` on **closed** blocks. |
| `stickToBottom` | `boolean` | `false` | Emits a snap sentinel; you add `scroll-snap-type: y proximity`. |
| `streamingHighlight` | `boolean \| "wavefront" \| "eager"` | `"wavefront"` | Highlight an open fence. |
| `className`, `id`, `role`, `aria-live`, `aria-atomic` | | — | Land on the `.brook-md` root. |
| `onBlockError` | `(error, info) => void` | — | Per-block error boundary hook. |
| `onStreamError` | `(err) => void` | — | Pipe errors; worker fatals carry `err.fatal`. |
| `onRenderMetrics` | `RenderMetricsHook` | — | Fires per ACTUAL block render. |
| `childMemo`, `deferTail` | `boolean` | `false` | Open-block child reuse / `useDeferredValue` routing. |

## `ParserConfig` — real defaults

| Field | Default | | Field | Default |
|---|---|---|---|---|
| `gfmAutolinks` | **`true`** | | `unsafeHtml` | `false` |
| `gfmAlerts` | **`true`** | | `componentTags` | off |
| `gfmTagfilter` | `false` | | `inlineComponentTags` | off |
| `gfmFootnotes` | `false` | | `htmlAllowlist` | unset = off |
| `gfmMath` | `false` | | `dropHtmlTags` | unset = off |
| `dirAuto` | `false` | | `blockHtml` | `false` |
| `lenientLists` | `false` | | `allowSchemes` | `[]` |
| `softBreaks` | `false` | | `blockData` | `false` |
| `a11y` | `false` | | `retainCommittedHtml` | `false` (worker path) |

Config is applied once when the stream's parser is created and is **immutable**
for that stream's lifetime — `reset()` keeps it; use a new client for different
flags. `streamingHighlight` is a *renderer* option, not part of `ParserConfig`.

## Framework bindings

```tsx
import { BrookMarkdown } from "brookmd/react";     // <BrookMarkdown stream={s} />
```
```ts
import { mountBrookMarkdown } from "brookmd/dom";  // mountBrookMarkdown(client, el, opts)
import { defineBrookMarkdown } from "brookmd/element"; // <brook-markdown src="…" gfm-math>
import { BrookMarkdown } from "brookmd/vue";       // <BrookMarkdown :client="client" />
import { brookMarkdown } from "brookmd/svelte";    // <div use:brookMarkdown={{ client }} />
import { BrookMarkdown } from "brookmd/solid";     // <BrookMarkdown client={client} />
import { renderToString } from "brookmd/server";   // sync, worker-free, React-free
import { BrookMarkdownStatic } from "brookmd/server/react"; // hookless, RSC-safe
```

On React Native use the **`brookmd-react-native`** package instead — the same
Rust core over JSI, no Web Worker.

## Common gotchas

1. **`components` has two prop contracts.** A capitalized block-kind key gets
   `BlockComponentProps` (with `block`); a lowercase tag key gets the element's
   attributes and `children` only — **no `block` prop**. A name reachable from
   both must guard: `block ? … : …`. Reading `props.block` unconditionally
   yields `can't access property "kind", block is undefined`.
2. **Inline object identity is the #1 perf footgun.** Hoist `components`,
   `decorators`, `urlTransform`, `sanitize`, `onBlockError` — a fresh identity
   re-renders every committed block on every patch. (`onLinkClick` is exempt: it
   is delegated to the root.)
3. **There is no `done()`** — it is `finalize()`. And `beginResume` is private.
4. **The root class is `brook-md`** (hyphenated); `className` is appended to it.
5. **`stickToBottom` is CSS-only** — you must add `scroll-snap-type: y proximity`
   to your scroll container.
6. **Blank `.brook-md` with no console output** = the worker never booted (often
   a stale hashed URL after a deploy). Watch `client.failed` / `onError`;
   `recovery` (default true) heals the first death invisibly, the second is fatal.
7. **Uncoloured code** = `brookmd/styles.css` was not imported.
8. **KaTeX/Mermaid must skip open blocks** (`.brook-open` / `.brook-streaming`,
   or `props.open`) — a half-typed formula or diagram throws on every patch.
9. **Vite needs `optimizeDeps.exclude`**; nothing else does.
10. **Next.js** — `'use client'`, and build the stream client-side.
11. **`getDefaultPool()` is browser-only** and a per-page singleton; don't rely
    on it in SSR/RSC.
12. **Don't SSR `BrookMarkdownStatic` and hydrate with `BrookMarkdown`** — use
    the same component on both sides.

## References

- [references/api.md](references/api.md) — subpath export map, every
  `<BrookMarkdown>` prop, `BrookClient` methods and options, full `ParserConfig`,
  the `components` two-contract rules, `BlockComponentProps` and the per-kind
  `blockData` shapes, the server API.
- [references/frameworks.md](references/frameworks.md) — React (incl.
  `useBrookStream`), Vue, Svelte, Solid, the Web Component's attributes and
  properties, vanilla `mountBrookMarkdown` + `MountHandle`, `brookmd/server`,
  Next.js, Vite, webpack/Rollup/Parcel, plain ESM, React Native, CSP.
- [references/styling.md](references/styling.md) — `styles.css`, the `--brook-*`
  variables, dark mode, the `brook-block` / `brook-open` / `brook-speculative` /
  `brook-streaming` class contract, the opt-in caret, code-block chrome, Tailwind
  (and why `prose` users should skip the theme import).
- [references/security.md](references/security.md) — the URL policy and
  never-allowed schemes, `allowSchemes`, raw-HTML tiers, the `sanitize` hook,
  trusted surfaces, `onLinkClick` interstitials.
- [references/recipes.md](references/recipes.md) — Vercel AI SDK `useChat`,
  KaTeX, Mermaid, Shiki, `registerLanguage`, a CSV table toolbar off `blockData`,
  interactive checkboxes, lazy images, accessible chat, persist/hydrate,
  `getMetrics()`.
- [references/troubleshooting.md](references/troubleshooting.md) — symptom →
  cause → fix for every failure above.
- [assets/examples/](assets/examples/tsconfig.json) — `basic-stream.tsx`,
  `ai-sdk-chat.tsx`, `full-featured.tsx`, `vanilla-dom.ts`, `web-component.html`,
  `server-render.ts`, `nextjs-app-router.tsx`. All typechecked.

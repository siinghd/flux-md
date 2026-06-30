# flux-md

Zero-dep streaming markdown for the browser. Rust→WASM core, one Web Worker per stream, incremental parse with speculative closure for mid-stream constructs.

Drop in a streaming-aware renderer — **React, Vue, Svelte, Solid, a framework-agnostic `<flux-markdown>` Web Component, or the vanilla DOM mount** — wire each LLM stream to a `FluxClient`, and the markdown renders incrementally off the main thread, block by block, with stable identities so unchanged blocks never re-reconcile.

Parsing runs entirely **off the main thread** — each stream gets its own pooled Web Worker, so many concurrent LLM responses render without contending for the UI thread. On each token the parser re-parses only the **active tail**, not the whole document, and heavy renderers (syntax highlighting, math, mermaid) are **deferred until a block closes**. The result is low retained memory and a main thread that stays responsive while streaming. See [the live demo](https://md.hsingh.app/).

## Install

```bash
bun add flux-md     # or: npm i flux-md / pnpm add flux-md
```

flux-md ships **compiled, non-minified ESM** (`dist/*.js` + `.d.ts` types) plus
the compiled WASM — no raw `.ts`/`.tsx` source. The worker and WASM asset are
referenced with the **web-standard `new URL(asset,
import.meta.url)`** pattern, so any bundler with asset-module support resolves
them: **Vite** (the reference setup), **webpack 5**, **Rollup** (with asset
modules), **Parcel**, and **Next.js** (App Router — Turbopack *and* webpack;
**verified on Next.js 16**, see the [Next.js callout](#nextjs) below).

The streaming client (`<FluxMarkdown>` / `FluxClient`) is **browser-only** (it
constructs Web Workers). For **server-side / static rendering of finished
content** — SSR, React Server Components, build steps — use the worker-free,
synchronous [`flux-md/server`](#server-side-rendering) entry. The framework packages — `react`,
`vue`, `svelte`, `solid-js` — are all **optional** peer dependencies; you only
need the one whose binding you import. The framework-free entries
(`flux-md/client`, `flux-md/dom`, `flux-md/element`, and `flux-md/server`) need
none. (The bare `flux-md` entry re-exports the React component surface, so it
pulls `react` — import from `flux-md/client` if you want a framework-free core.)

> **Vite — one-line config.** Vite's dependency pre-bundling (esbuild) hoists
> the wasm-bindgen glue into `.vite/deps/`, which breaks the relative
> `new URL("…_bg.wasm", import.meta.url)` lookup so the worker can't load WASM
> (you'll see a 404 / "magic word" error). Exclude flux-md from pre-bundling:
>
> ```ts
> // vite.config.ts
> export default defineConfig({
>   optimizeDeps: { exclude: ["flux-md"] },
> });
> ```
>
> No other bundler needs this — it's specific to Vite's optimizer.

<a id="nextjs"></a>

> **Next.js (App Router) — one requirement.** Works on **Next.js** with
> **Turbopack** (the default for both `next dev` and `next build`) or webpack.
> Since 0.17.0 flux-md ships **compiled ESM**, so **no `transpilePackages` or
> other build config is needed** — earlier versions required it only because the
> package shipped raw TypeScript, which Next does not compile inside
> `node_modules`. That no longer applies.
>
> **Use it from a Client Component.** `<FluxMarkdown>` uses React hooks (and
> spawns a Web Worker on mount), so it must carry `"use client"` — it can't be
> a Server Component. (It is still SSR-safe: on the server it renders an empty
> shell and only starts streaming after hydration, so there's no SSR crash —
> the constraint is hooks, not the worker.)
>
> ```tsx
> "use client";
> import { FluxMarkdown } from "flux-md/react";
>
> export default function Answer({ stream }: { stream: AsyncIterable<string> }) {
>   return <FluxMarkdown stream={stream} />;
> }
> ```
>
> **Create the `stream` in Client Component code, not in a Server Component.**
> A `Response` / `ReadableStream` / `AsyncIterable` isn't serializable, so it
> can't be passed as a prop from a Server Component (e.g. `page.tsx`) — that
> throws *"Only plain objects can be passed to Client Components."* Pass a
> serializable prop (a URL, the chat messages) from the server and open the
> stream on the client — e.g. `stream={await fetch("/api/chat")}` from a client
> effect, or the `useFluxStream` hook (see [Quick start](#quick-start)).
>
> That's it — Turbopack bundles the worker and emits the `.wasm` to
> `_next/static/media` itself, so no extra asset/loader config is needed (and the
> Vite `optimizeDeps` workaround above does **not** apply). Both `next dev` and
> `next build && next start` are verified to spawn the worker, load the WASM, and
> stream markdown. _Dev tip:_ open the app on `localhost` — Next dev blocks
> cross-origin dev resources (HMR, chunks) from other hosts (e.g. `127.0.0.1`)
> unless you add them to `allowedDevOrigins` in `next.config`.

## Quick start

```ts
import { FluxClient, FluxMarkdown } from "flux-md";

// One client per stream. Spawns a Web Worker that owns a Rust parser.
const client = new FluxClient();

// Feed chunks as they arrive from your SSE / fetch reader.
for await (const delta of streamFromAi()) {
  client.append(delta);
}
client.finalize();
```

In React — pass the stream straight to `<FluxMarkdown>`. It owns the client,
pipes the stream, supersedes it if it changes, and cleans up on unmount:

```tsx
import { FluxMarkdown } from "flux-md/react";

export function ChatMessage({ stream }: { stream: AsyncIterable<string> }) {
  return <FluxMarkdown stream={stream} />;
}
```

`stream` accepts an `AsyncIterable<string>` (e.g. SSE deltas), a `Response`, or
a `ReadableStream<Uint8Array>` — so `<FluxMarkdown stream={await fetch("/api/chat")} />`
works too.

Need the client handle (for `outline()` / `getMetrics()` / a shared client)? Use
the `useFluxStream` hook — same lifecycle, returns the owned client:

```tsx
import { FluxMarkdown, useFluxStream } from "flux-md/react";

export function ChatMessage({ stream }: { stream: AsyncIterable<string> }) {
  const client = useFluxStream(stream);
  return <FluxMarkdown client={client} />;
}
```

### Already holding a growing string? — `useFluxMarkdownString`

Many apps keep the streaming message as a **single growing string prop** (it
re-renders with the full text-so-far each token), not as a stream. Feed that
string straight in — `useFluxMarkdownString` diffs it for you and forwards only
the delta, so you don't hand-roll an append/reset bridge:

```tsx
import { FluxMarkdown, useFluxMarkdownString } from "flux-md/react";

export function ChatMessage({ text, streaming }: { text: string; streaming: boolean }) {
  const client = useFluxMarkdownString(text, { streaming });
  return <FluxMarkdown client={client} />;
}
```

It handles the two shapes a controlled string takes: a **prefix-extension** (the
common token-by-token growth) appends only the new suffix; a **divergence** (e.g.
the finished text swapped for a re-processed final string — bolded numbers,
wrapped tickers) resets and reparses. Pass `streaming: false` once the content is
final so the last block commits (a finished code fence then highlights). The
framework-neutral primitive is **`client.setContent(fullString, { done })`** —
use it from any binding.

> **Transforming streamed content?** If the enrichment runs **live per token**
> (e.g. bold every number as it arrives), do it at **render time** via
> [`components`](#custom-components--overrides) — keep the markdown source
> append-only so parsing stays incremental. Re-transforming the *whole* string
> each token (so earlier bytes change) forces `setContent` to reparse every tick
> (O(n²)); that's what render-time overrides avoid. `setContent`'s reset path is
> for the **once**-at-the-end reprocess swap, not per-token rewrites.

<details>
<summary>Full manual control (caller-owned client)</summary>

When you want to drive the stream yourself, pass a `client` you own — the
component never destroys it:

```tsx
import { useEffect, useState } from "react";
import { FluxClient, FluxMarkdown } from "flux-md";

export function ChatMessage({ stream }: { stream: AsyncIterable<string> }) {
  const [client] = useState(() => new FluxClient());
  useEffect(() => () => client.destroy(), [client]);
  useEffect(() => {
    const ac = new AbortController();
    client.pipeFrom(stream, { signal: ac.signal }); // pipeFrom also accepts AsyncIterable
    return () => ac.abort();
  }, [client, stream]);
  return <FluxMarkdown client={client} />;
}
```

</details>

> **StrictMode note:** a stream (SSE generator / `Response`) can be consumed only
> once, so React StrictMode's dev-only double-mount may truncate it in
> development. Production mounts once and is unaffected.

Multiple concurrent streams just need multiple clients — each runs in its own worker, so they don't share main-thread budget.

## Framework bindings

`FluxClient` is framework-neutral — it owns the worker and exposes
`subscribe`/`getSnapshot`. Pick a renderer to put its blocks on screen. Every
binding below is thin glue over the same incremental DOM renderer, so they
share one identity contract: a committed block's node is never recreated, only
the streaming tail re-renders.

**One ownership rule across all bindings:** the renderer's teardown (React
unmount, `handle.destroy()`, element disconnect, etc.) frees only the rendered
DOM and the subscription — it **never** destroys the client. You call
`client.destroy()` when you're done with the stream. (React's `<FluxMarkdown>`,
documented [below](#fluxmarkdown-react), is the same.)

### Vanilla / any framework — `flux-md/dom`

```ts
import { FluxClient } from "flux-md/client";
import { mountFluxMarkdown } from "flux-md/dom";

const client = new FluxClient();
const handle = mountFluxMarkdown(client, document.getElementById("out")!, {
  stickToBottom: true,
});

// Feed it from a fetch/SSE reader:
const reader = (await fetch("/api/chat")).body!.getReader();
const dec = new TextDecoder();
for (;;) {
  const { value, done } = await reader.read();
  if (done) break;
  client.append(dec.decode(value, { stream: true })); // stream:true carries multibyte across chunks
}
client.append(dec.decode());
client.finalize();

// Teardown: destroy BOTH — the renderer and the client you created.
handle.destroy();
client.destroy();
```

**Already holding a growing string?** There's no framework reactivity to wrap,
so just call **`client.setContent(fullString, { done })`** instead of the
`append` loop — it diffs internally (prefix → delta; divergence → reparse) and
finalizes on `done`. That's the same primitive the React/Vue/Svelte/Solid
controlled-string helpers wrap; in vanilla you call it directly.

`mountFluxMarkdown(client, container, options?)` returns `{ destroy(), refresh() }`.
Options: `components`, `sanitize`, `virtualize`, `stickToBottom`, `highlightCode`
(default true), `batch` (default true — one DOM write per `requestAnimationFrame`),
`morphOpenBlocks` (default false — morph a growing generic open block's subtree in
place instead of rebuilding it via `innerHTML`, so only the changed parts repaint
and focus/selection in the streaming tail survive; the rendered result is
equivalent to the default rebuild path).
Block-kind overrides use `components` keyed by block-kind (`CodeBlock`, `Table`,
`Alert`, `Component`, …) with values `(props) => HTMLElement | string`. Tag-level
(lowercase `a`/`table`/`code`) overrides are **React-only** — there's no virtual
tree on the fast `innerHTML` path; a block-kind override can rewrite the `html`
it's handed instead.

### Web Component `<flux-markdown>` — `flux-md/element`

The universal binding — plain HTML, Angular, or any framework that renders DOM.
Register once, then use the element:

```ts
import { defineFluxMarkdown } from "flux-md/element";
defineFluxMarkdown(); // defines <flux-markdown>; pass a custom tag name if you like
```

```html
<!-- zero-JS streaming straight from a URL -->
<flux-markdown src="/api/post.md" gfm-math stick-to-bottom></flux-markdown>

<!-- one-shot from inline text -->
<flux-markdown># Hello **world**</flux-markdown>
```

```js
// or caller-owned streaming — drive your own client:
const el = document.querySelector("flux-markdown");
el.client = myFluxClient;             // element subscribes; never destroys it
el.components = { Thinking: (p) => myNode(p) };
myFluxClient.append(delta);
```

Config flags are **tri-state attributes**: absent = library default;
`gfm-math` / `gfm-math="true"` / `="1"` = on; `gfm-math="false"` / `="0"` = off
(the only way to turn off a default-on flag such as `gfm-alerts`). It renders in
light DOM so your markdown CSS applies, and `defineFluxMarkdown` is a no-op under
SSR (no `customElements`). A self-owned element (`src` / `markdown` / inline
text / `append()`) is torn down on disconnect; a caller-supplied `client` is left
alone.

**Angular** consumes the same element — no separate package:

```ts
import { Component, CUSTOM_ELEMENTS_SCHEMA } from "@angular/core";
import { defineFluxMarkdown } from "flux-md/element";
defineFluxMarkdown(); // once at bootstrap

@Component({
  standalone: true,
  schemas: [CUSTOM_ELEMENTS_SCHEMA],
  template: `<flux-markdown [attr.src]="url" stick-to-bottom></flux-markdown>`,
})
export class Answer { url = "/api/post.md"; }
```

**Controlled growing string?** Assign a caller-owned client and drive it with
`setContent` — `el.client = myClient; myClient.setContent(fullString, { done })`
— the element subscribes and renders, you own the diffing. (The self-owned
`markdown` attribute is **one-shot** — it re-parses the whole document on each
change, so don't point it at a per-token-growing string; use a client +
`setContent` for that.)

### Vue 3 — `flux-md/vue`

```vue
<script setup lang="ts">
import { onBeforeUnmount } from "vue";
import { FluxClient } from "flux-md/client";
import { FluxMarkdown } from "flux-md/vue";

const client = new FluxClient();
// feed client.append(delta) from your stream, then client.finalize()
onBeforeUnmount(() => client.destroy());
</script>

<template>
  <FluxMarkdown :client="client" stick-to-bottom />
</template>
```

Props: `client` (required), `components`, `sanitize`, `virtualize`,
`stickToBottom`. There's also a `useFluxMarkdown` composable returning a
`container` ref if you'd rather mount into your own element.

**Already holding a growing string?** `useFluxMarkdownString` owns a client and
diffs the string for you (the Vue analogue of the React hook — see
[Controlled strings](#already-holding-a-growing-string--usefluxmarkdownstring)):

```vue
<script setup lang="ts">
import { FluxMarkdown, useFluxMarkdownString } from "flux-md/vue";
const props = defineProps<{ text: string; streaming: boolean }>();
// Pass getters so the composable tracks the live values; it owns + destroys the client.
const client = useFluxMarkdownString(() => props.text, () => ({ streaming: props.streaming }));
</script>
<template><FluxMarkdown :client="client" /></template>
```

### Svelte (4 & 5) — `flux-md/svelte`

A Svelte action — works in both v4 and v5, no `.svelte` build step:

```svelte
<script lang="ts">
  import { onDestroy } from "svelte";
  import { FluxClient } from "flux-md/client";
  import { fluxMarkdown } from "flux-md/svelte";

  const client = new FluxClient();
  // feed client.append(delta) then client.finalize()
  onDestroy(() => client.destroy());
</script>

<div use:fluxMarkdown={{ client, stickToBottom: true }} />
```

**Growing string?** The `fluxMarkdownString` action owns a client and diffs the
string — `use:fluxMarkdownString={{ content, streaming }}` (it destroys its
client on `destroy`, so no manual cleanup):

```svelte
<script lang="ts">
  import { fluxMarkdownString } from "flux-md/svelte";
  export let content: string;     // the growing message
  export let streaming: boolean;  // false once complete → finalizes
</script>

<div use:fluxMarkdownString={{ content, streaming, stickToBottom: true }} />
```

### Solid — `flux-md/solid`

```tsx
import { onCleanup } from "solid-js";
import { FluxClient } from "flux-md/client";
import { FluxMarkdown } from "flux-md/solid";

const client = new FluxClient();
// feed client.append(delta) then client.finalize()
onCleanup(() => client.destroy());

<FluxMarkdown client={client} stickToBottom />;
```

**Growing string?** `createFluxMarkdownString` owns a client and diffs the string
(the Solid analogue of the React hook), driving `setContent` from a
`createEffect` and destroying the client on cleanup:

```tsx
import { FluxMarkdown, createFluxMarkdownString } from "flux-md/solid";

function Message(props: { text: string; streaming: boolean }) {
  const client = createFluxMarkdownString(() => props.text, () => ({ streaming: props.streaming }));
  return <FluxMarkdown client={client} />;
}
```

The Solid binding's mount/teardown logic is tested, but its JSX component shell
has so far only been exercised through a real Solid (`vite-plugin-solid`) build
in development, not in CI — treat it as the newest of the bindings and file an
issue if your Solid setup trips on it. The component is a thin `ref`'d `<div>`;
if you hit a transform edge, `mountFluxMarkdown` from `flux-md/dom` inside
`onMount`/`onCleanup` is the zero-surprise fallback.

## Server-side rendering

`<FluxMarkdown>` / `FluxClient` are browser-only (they spawn a Web Worker), but
the Rust→WASM core is a plain **synchronous** parser. So `flux-md/server` renders
**finished** markdown on the server with no worker and no async ceremony — Node
SSR, React Server Components, or a build step:

```ts
import { initFlux, renderToString } from "flux-md/server";

await initFlux();                                       // once at startup (loads the WASM)
const html = renderToString("# Hello\n\n**world**");   // sync HTML string, no worker
```

For React server rendering (RSC, static generation, or SSR), use
`<FluxMarkdownStatic>` from **`flux-md/server/react`** — a hookless, RSC-safe
component that renders finished content with the same `components` overrides
(inline/block component tags dispatch on the server too). It lives in its own
subpath so the core `flux-md/server` above stays importable with no `react`
installed:

```tsx
import { initFlux } from "flux-md/server";
import { FluxMarkdownStatic } from "flux-md/server/react";

await initFlux();
export default function Doc({ md }: { md: string }) {
  return (
    <FluxMarkdownStatic
      content={md}
      config={{ inlineComponentTags: ["tik"] }}
      components={{ tik: ({ symbol }) => <span className="ticker">{symbol}</span> }}
    />
  );
}
```

- **`initFlux()`** — async, idempotent. In Node it reads the package's `.wasm` off
  disk (Node's `fetch` can't load `file://`); on the web it fetches the
  bundler-resolved asset. On edge runtimes pass bytes yourself:
  `initFluxSync(wasmBytes)`.
- **`renderToString(md, { config })`** — synchronous HTML string, **zero React
  dependency** (imports cleanly with no `react` installed).
- **`parseToBlocks(md, { config })`** — the block array, for custom rendering.
- **`<FluxMarkdownStatic content config components />`** (from
  `flux-md/server/react`) — synchronous React tree for **render-once** contexts;
  render it with your framework's server renderer
  (`renderToStaticMarkup`, RSC, …). For live streaming, client-side code
  highlighting, or Mermaid, render `<FluxMarkdown>` on the client instead — it's a
  separate component. (If you SSR-then-hydrate, use the *same* component on both
  sides; the dedicated client renderers in `<FluxMarkdown>` don't hydrate
  `<FluxMarkdownStatic>`'s plainer markup.)

## What it does

| Concern | flux-md | conventional main-thread renderer |
|---|---|---|
| Re-parse on each token | No — only the active tail | Yes, full string |
| Where parse runs | Web Worker (off main thread) | Main thread |
| Block identity across chunks | Stable monotonic IDs | New keys on every render |
| Mid-stream unclosed `` ``` `` / `*` / `**` | Speculatively closed in render, replaced cleanly | Often renders raw or breaks |
| Heavy renderers (syntax, math, mermaid) | Deferred until block close | Re-run per chunk |
| XSS sanitization | Allowlist in Rust + URL scheme check | Downstream sanitizer pass on the JS thread |

## Styling

flux-md emits semantic HTML under a `.flux-md` root and **ships no CSS by
default** — bring your own design system, or opt into the bundled theme:

```ts
import "flux-md/styles.css";
```

It gives good-looking output out of the box, **including the built-in syntax
highlighter's colors** (without any CSS, `highlight()` renders uncolored). The
theme is scoped to `.flux-md`, zero-runtime, and **does not change the rendered
HTML** — skip the import and nothing is styled.

> **Next.js Pages Router:** `flux-md/styles.css` is global CSS, which the Pages
> Router only allows importing from `pages/_app`. Import it there (App Router and
> other bundlers can import it from any component). Or skip it and bring your own
> `.flux-md` styles.

Re-theme by overriding a few CSS variables; it's light by default and switches to
dark automatically via `prefers-color-scheme` (force a mode with
`class="flux-md flux-dark"` or `flux-light`):

```css
.flux-md {
  --flux-accent: #7c3aed;   /* links */
  --flux-bg-code: #faf7ff;  /* code background */
  --flux-t-kw: #c026d3;     /* syntax: keywords (also --flux-t-str/num/com/fn/ty/…) */
}
```

## Public API

### `FluxClient`

```ts
class FluxClient {
  constructor(options?: {
    pool?: FluxPool;
    config?: ParserConfig;
    onError?: (err: { message: string; fatal?: boolean }) => void; // worker/parse + WASM-init errors
    onBlock?: (block: Block) => void;                 // fires once per block as it commits
  });
  append(chunk: string): void;                      // queue text for parsing
  pipeFrom(                                         // read → append → finalize
    src: ReadableStream<Uint8Array> | Response | AsyncIterable<string>,
    opts?: { signal?: AbortSignal },                // abort to supersede (no finalize)
  ): Promise<void>;
  finalize(): void;                                 // mark stream complete
  setContent(                                       // drive from a controlled full string
    full: string,                                   // diffs vs last: prefix → append delta; else reset+reparse
    opts?: { done?: boolean },                      // done:true → finalize
  ): void;
  reset(): void;                                    // wipe and reuse
  destroy(): void;                                  // free this stream's parser
  whenReady(): Promise<void>;                       // resolves once WASM loaded; rejects on init failure
  subscribe(listener: () => void): () => void;      // React-friendly store
  getSnapshot(): Block[];                           // ordered current blocks
  outline(): { level: number; text: string; id: number }[]; // heading table-of-contents (works mid-stream)
  toPlaintext(): string;                            // rendered document as plain text (search / summaries)
  getMetrics(): { bytes, patches, totalParseMs, throughputKBs,
                   retainedBytes, wasmMemoryBytes, ... };
}
```

`pipeFrom` is the LLM-native shortcut — hand it a `fetch` response and it
reads, appends, and finalizes for you:

```ts
const client = new FluxClient();
await client.pipeFrom(await fetch("/api/chat")); // streams the body in, then finalizes
```

Pass `onError` to be notified of worker/parse errors and a fatal WASM-init
failure (`{ fatal: true }`); without it, errors are only `console.error`'d and a
load failure surfaces as a rejected `whenReady()`. Pass `onBlock` to run a side
effect each time a block commits (e.g. lazy-highlight a finished code block).

#### Per-stream config

```ts
const client = new FluxClient({
  config: {
    gfmAutolinks: true,   // bare www./http(s):// URLs + emails → links (default true)
    gfmAlerts: true,      // > [!NOTE] → callouts (default true)
    gfmFootnotes: true,   // [^1] + [^1]: → footnote section (default false)
    gfmMath: true,        // $…$ / \(…\) inline + $$…$$ / \[…\] display math (default false)
    dirAuto: true,        // per-block dir="auto" for RTL/bidi text (default false)
    a11y: true,           // task-list <label> + <th scope="col"> a11y markup (default false)
    unsafeHtml: false,    // pass raw HTML through (default false — keep it false for untrusted input)
    componentTags: ["Thinking", "Callout"], // BLOCK custom tags w/ markdown inside (default none)
    inlineComponentTags: ["tik", "cite"],   // INLINE custom tags (chips/citations) w/ markdown inside (default none)
    htmlAllowlist: ["br", "sub", "sup"],    // safe raw-HTML sanitizer: [] = allow all but dangerous; list = only those (default off)
    dropHtmlTags: [],                        // tags removed entirely (comments always dropped when sanitizing; default off)
    blockData: true,      // opt-in structured kind.data per block (default false — see "Structured block data")
  },
});
```

Omitted fields use the defaults above, so `new FluxClient()` is unchanged.
Config is applied when the stream's parser is created and is **immutable** for
that stream (`reset()` keeps it; use a new client for different flags).

When to enable each flag:

- `gfmAutolinks` — on by default. Leave it on unless you want strict CommonMark.
- `gfmAlerts` — on by default. Leave it on unless you want strict CommonMark.
- `gfmMath: true` — when your LLM emits `$…$` or `$$…$$` (or LaTeX `\(…\)` /
  `\[…\]`). flux-md emits KaTeX-ready markup; you bring the KaTeX pass (or
  `components.MathBlock`).
- `gfmFootnotes: true` — when your input uses `[^1]` references and `[^1]:`
  definitions. Off by default; see the footnote streaming caveat above.
- `dirAuto: true` — when content can be RTL / mixed-direction. Emits per-block
  `dir="auto"` so the browser detects direction independently per block.
- `a11y: true` — opt-in accessibility markup that deviates from strict GFM
  byte-output: wraps task-list checkboxes in a `<label>` (screen-reader
  association) and adds `scope="col"` to table headers. Off by default so
  conformance output stays exact.
- `unsafeHtml: true` — only when rendering trusted HTML. For untrusted /
  LLM-produced HTML, pair this with `<FluxMarkdown sanitize={…} />` (DOMPurify or
  similar — see [Security](#security)).
- `componentTags: ["Thinking", …]` — when your LLM emits **block** custom tags
  like `<Thinking>…</Thinking>` (on their own line) and you want their inner
  content parsed as markdown and dispatched to a React component. Safe without
  `unsafeHtml` (attributes are sanitized; allowlisted tags only).
- `inlineComponentTags: ["tik", …]` — same idea for **inline** custom elements
  that sit inside a paragraph, heading, list item, or **table cell** (ticker
  chips, citations, `@mentions`). See [Inline component tags](#inline-component-tags).
- `htmlAllowlist` / `dropHtmlTags` — render a **safe subset of raw HTML** (e.g.
  `<br>`, `<sub>`, `<sup>`) natively without `unsafeHtml`, drop specific tags, and
  drop HTML comments. See [Safe raw HTML](#safe-raw-html).

**Footnotes** (`gfmFootnotes`) work in streaming with one honest caveat: a
`[^1]` reference renders speculatively the moment it's seen (committed blocks
can't re-render), and the footnote **section is emitted at finalize**. So a
reference whose definition never arrives leaves a dangling link — the same
forward-reference cost as link reference definitions. Multiple references to
the same footnote each get a **unique id** (`fnref-N`, `fnref-N-2`, …) and the
definition lists **one backref per reference**. Remaining v1 limits:
single-block definitions (no continuation-indent / multi-paragraph) and no
nested footnotes. The section uses GitHub-style markup
(`<section class="footnotes">`, `<sup class="footnote-ref">`).

**Math** (`gfmMath`) recognizes both delimiter families LLMs emit — `$…$` /
`$$…$$` and LaTeX `\(…\)` / `\[…\]`. Inline math renders to
`<span class="math math-inline">…</span>`, display math to
`<div class="math math-display">…</div>` (and inline display to a `math-display`
span), each carrying the **HTML-escaped LaTeX as its text content** — exactly
what [KaTeX](https://katex.org)'s auto-render / `rehype-katex` consume. flux-md
stays **zero-dep**: it produces the KaTeX-ready markup and never processes the
body as markdown; you bring the KaTeX pass (or override `components.MathBlock`,
which receives the raw LaTeX as `text`). Single `$` uses the **pandoc rule** so
prose and currency stay literal — the opener needs a non-space to its right, the
closer a non-space to its left and no digit after it, so `$5 and $10` is **not**
math. A `$$`/`\[` block is **blank-line tolerant** (multi-line `\begin{aligned}…`
stays one block) and renders incrementally while streaming, like a code fence.
Off by default (so `$` in plain prose is untouched) — enable it per stream when
your model emits LaTeX.

**Bidirectional text** (`dirAuto`) emits `dir="auto"` on each block-level text
element (`p`, `h1`–`h6`, `blockquote`, `ul`/`ol`/`li`, `table`), so the browser
runs the Unicode bidi algorithm **per block** — an Arabic/Hebrew paragraph
renders RTL while the English one beside it stays LTR, with no JS direction
detection. Code blocks never get it (code is always LTR). This is the per-block
model GitHub uses; it's the right fix for the common failure mode of detecting
one direction for a whole mixed-language document. Off by default (strict
CommonMark output is unchanged); turn it on for RTL or mixed-direction content.

### `FluxMarkdown` (React)

Subscribes to a `FluxClient`, renders each block keyed by its stable parser-assigned ID. Memoized so unchanged blocks never re-reconcile.

```tsx
<FluxMarkdown client={client} />
```

The root element accepts opt-in `className` (appended to `flux-md`), `id`,
`role`, and `aria-live` / `aria-atomic`. Set `aria-live="polite"` to make the
output a live region so screen readers announce streamed content as it settles —
`polite` coalesces rapid updates and does **not** read every token. The same
options exist on the DOM mount (`mountFluxMarkdown(client, el, { ariaLive: "polite" })`),
covering the Web Component and the Vue/Svelte/Solid adapters.

#### Custom components / overrides

Pass a `components` map to replace how elements render. Keys come in **two
namespaces**:

```tsx
import { useMemo } from "react";
import { FluxClient, FluxMarkdown, type Components } from "flux-md";

function Message({ client }: { client: FluxClient }) {
  // Memoize (or hoist to module scope). A fresh object every render busts
  // FluxMarkdown's block memo, so every block re-parses on every patch.
  const components: Components = useMemo(
    () => ({
      // tag-level (lowercase HTML names) — applied inside a block's HTML
      table: (props) => <table className="rounded border" {...props} />,
      a: (props) => <a target="_blank" rel="noreferrer" {...props} />,
      h1: "h2", // a string value just swaps the tag

      // block-kind (capitalized BlockKindTag) — replaces the whole block
      CodeBlock: ({ text, language, open }) => (
        <MyCodeBlockWithCopyButton code={text} lang={language} streaming={open} />
      ),

      // GitHub alerts (`> [!NOTE]` / `[!TIP]` / `[!WARNING]` / `[!CAUTION]` /
      // `[!IMPORTANT]`) — swap in your own callout component. The alert kind
      // is on `block.kind.data.kind`; `html` is the rendered inner body.
      Alert: ({ block, html }) => (
        <MyCallout kind={(block.kind.data as { kind: string }).kind}>
          <div dangerouslySetInnerHTML={{ __html: html }} />
        </MyCallout>
      ),
    }),
    [],
  );
  return <FluxMarkdown client={client} components={components} />;
}
```

**Tag-level** keys (`table`, `thead`, `tr`, `td`, `a`, `code`, `pre`, `h1`–`h6`,
`ul`, `ol`, `li`, `blockquote`, `p`, `img`, `del`, `input`, `hr`, …) replace that
element wherever it appears. The component receives the element's parsed
attributes (with `class`→`className` and `style` as an object) plus `children`.

**Block-kind** keys (`CodeBlock`, `Mermaid`, `MathBlock`, `Alert`, `Paragraph`,
`Heading`, `List`, `Blockquote`, `Table`, `Rule`, `Html`) replace the entire
block. The component receives [`BlockComponentProps`](#types): `{ block, html,
open, speculative }`, plus `text`/`language` for code/math blocks (the alert
type is at `block.kind.data.kind`).

Rules worth knowing:

- **There is no `node` prop / no hast tree.** Introspect via `className` /
  `data-*`, or — better — opt into the typed **[structured-data
  channel](#structured-block-data-setblockdata)** (`blockData: true`) and read
  `block.kind.data` (and the typed `props.table` / `heading` / `code` / `math` /
  `list` fields) directly — no HTML re-parsing.
- **Overrides apply to the OPEN (streaming) block too**, not just settled ones —
  so a design-system renderer (Tailwind classes on `p`/`ul`/`li`, inline
  `<a>`/`<code>` overrides) stays styled mid-stream. The tail's HTML is always
  well-formed (the parser speculatively closes it). If a `sanitize` is supplied
  it runs first, on every block.
- **No `components` prop ⇒ the original fast path** (`innerHTML`, byte-identical
  output). The HTML→React conversion runs only when you actually supply
  overrides, and is memoized per `(block id, html)` so committed blocks don't
  re-parse as the stream grows.
- For **code blocks** the built-in highlighter is the default; it is bypassed
  (so your override wins) when you pass `components.CodeBlock`, `components.pre`,
  or `components.code`.

#### Inline text decorators

Wrap or replace matched inline **text** while streaming — e.g. bold financial
figures — without writing your own HTML re-parser. A `decorators` entry runs
POST-parse on real inline **text nodes only** (never URLs, code, or markup), once
per committed block, so a long document stays **O(n)**.

```tsx
import { FluxMarkdown, wrapLink } from "flux-md";

// HOIST it (module scope) or memoize — a fresh identity each render busts the
// per-block memo and re-decorates every block on every patch (a dev warning fires).
const decorators = [
  { match: /\$[\d.]+[BMK]|FY\d{4}|\d+(?:[-–]\d+)?%/g, replace: (t) => <mark>{t}</mark> },
  // Linkify a ticker — route the href through the safe helper (see below):
  { match: /\$[A-Z]{1,5}\b/g, replace: (t) => wrapLink(t, { href: `/sym/${t.slice(1)}` }) },
];

<FluxMarkdown client={client} decorators={decorators} />;
```

- **Trusted surface — not sanitized.** A decorator's `replace` output is spliced
  straight into the tree and does **not** pass through flux's attribute sanitizer
  (React renders a `javascript:` href without complaint). Treat `decorators`
  exactly like `components`: build only trusted nodes, and route any link href
  through `wrapLink` or the exported `safeUrl`.
- **`skipInside`** defaults to `['a','code','pre','kbd']`; override per decorator.
- **Per-text-node.** A value split by inline markup (e.g. `$2.<em>5</em>B`) is two
  text nodes and won't match across them — match against settled, contiguous text.
- Matching is pure and stateless, so a value streamed char-by-char decorates
  **identically** to a one-shot render. Same API on `flux-md/dom`
  (`mountFluxMarkdown(client, el, { decorators })`); a decorator there returns a
  `Node` or string.

`urlTransform?: (url, { tag, attr }) => string` rewrites `href`/`src`/`poster`
URLs as blocks render (proxy images, add UTM params). Its output is re-sanitized
(`safeUrl(urlTransform(safeUrl(value)))`), so a buggy transform can never emit a
`javascript:` / `data:text/html` URL. Hoist/memoize it for the same reason as
`decorators`.

### Structured block data (`setBlockData`)

Set `blockData: true` in the per-stream config and each block carries typed
structured data on `block.kind.data`, also surfaced as typed fields on the
component props — so you build toolbars, tables of contents, charts, copy
buttons, etc. from **data**, never by re-parsing the rendered HTML (no hast tree,
no rehype). Off by default; when off, output and CommonMark/GFM conformance are
byte-identical, so non-users pay nothing.

| Kind | `block.kind.data` | prop | use |
|------|-------------------|------|-----|
| `Table` | `{ headers, rows, aligns }`, cells `{ text, html }` | `props.table` | sort / filter / transpose / CSV / chart |
| `Heading` | `{ level, text, id }` | `props.heading` | table of contents with anchors |
| `CodeBlock` | `{ lang, code }` | `props.code` | decoded source (copy / run) |
| `MathBlock` | `{ latex }` | `props.math` | LaTeX source (re-render) |
| `List` | `{ ordered, start }` | `props.list` | ordered-list numbering |

Each cell's `text` is inline-stripped plaintext (for sort/filter/CSV/logic);
`html` is the inline-rendered display HTML. The data **streams** with the
document — a growing table or a heading carries its structured data on every
patch, in lock-step with the HTML — something a batch HTML-AST cannot do.

```tsx
// Table of contents from heading data — no DOM, works mid-stream:
const toc = client.getSnapshot()
  .filter((b) => b.kind.type === "Heading" && b.kind.data)
  .map((b) => b.kind.data as { level: number; text: string; id: string });
```

### Component tags

LLMs increasingly emit custom component tags like `<Thinking>…</Thinking>`. By
default these are inert (escaped, or — with `unsafeHtml` — raw HTML whose body
is *not* markdown). Opt in by allowlisting the tag names:

```tsx
const client = new FluxClient({ config: { componentTags: ["Thinking", "Callout"] } });
```

Now a listed tag is a **markdown container**: its inner content is parsed as
markdown, it spans blank lines up to its matching close tag (not split like a
raw HTML block), it nests, and a `</Tag>` inside a code fence stays content. It's
**safe without `unsafeHtml`** — the tag is allowlisted and its attributes are
sanitized (event handlers dropped, dangerous URL schemes → `#`).

Each renders as a `Component` block. Override it in React by tag name (or with
the generic `Component` fallback). The override receives `tag`, the sanitized
`attrs`, the inner content as ready-to-render **`children`** (the easy path), and
also `html` (the inner already-rendered markdown string, for
`dangerouslySetInnerHTML`):

```tsx
<FluxMarkdown
  client={client}
  components={{
    Thinking: ({ children }) => (
      <details className="thinking">
        <summary>Reasoning</summary>
        {children}
      </details>
    ),
  }}
/>
```

> **`children` vs `html`.** A `Component` override that renders *neither* shows
> **empty** (a common first-try gotcha). Prefer **`children`** — a parsed React
> tree with nested overrides applied; reach for `dangerouslySetInnerHTML={{ __html:
> html }}` only when you need the raw string. `attrs` keys are React-form
> (`class`→`className`, `for`→`htmlFor`) so `{...attrs}` spreads cleanly. While
> streaming, both reflect the partial inner content and re-render as more arrives.
> With no override the block renders as `<thinking …>…</thinking>`. Tag names
> match case-sensitively; off unless `componentTags` is set.

<a id="inline-component-tags"></a>

#### Inline component tags

`componentTags` handles **block** containers (a `<Thinking>` on its own line). For
**inline** custom elements — ticker chips, citations, `@mentions`, inline tooltips
that sit *inside* a paragraph, heading, list item, or **table cell** — use
`inlineComponentTags`:

```tsx
const client = new FluxClient({ config: { inlineComponentTags: ["tik"] } });

<FluxMarkdown
  client={client}
  components={{
    tik: ({ symbol, children }) => <span className="ticker">{children ?? symbol}</span>,
  }}
/>;
```

Now `Apple <tik symbol="AAPL">AAPL</tik> rose 2%` (or self-closing
`<tik symbol="AAPL"/>`) dispatches the inline `<tik>` to `components.tik`: its
inner is parsed as **inline markdown** (the `children`), its attributes become
props, and it's **safe without `unsafeHtml`** (attributes sanitized, allowlisted
tags only). It works everywhere inline content does — **including table cells**.
Tag names match **case-sensitively** and dispatch verbatim to `components[tag]`
(`<tik>`→`components.tik`, `<Cite>`→`components.Cite`). The
two lists are independent: list a tag under `componentTags` for blocks,
`inlineComponentTags` for inline, or both for both. An allowlisted tag used in an
unsupported position degrades **inertly** (escaped) — it never consumes
surrounding content.

> **Link-bridge alternative.** Before `inlineComponentTags`, the way to get an
> inline custom element was the link bridge: emit `[$AAPL](tik://AAPL)` and
> override `a` to render a chip when the href scheme matches. It's XSS-safe and
> renders inline-in-cells too — `inlineComponentTags` simply replaces that
> workaround with first-class inline elements.

### Safe raw HTML

LLMs emit a little raw HTML — `<br>`, `<sub>`/`<sup>`, `<mark>`, and HTML comments
as markers (`<!--mk:id-->`). `unsafeHtml` is all-or-nothing; instead opt into a
**sanitizer** that renders a safe subset natively. Setting `htmlAllowlist` and/or
`dropHtmlTags` (even to `[]`) engages it:

```ts
// Render only these inline tags; escape everything else:
new FluxClient({ config: { htmlAllowlist: ["br", "sub", "sup", "mark"] } });

// Or allow everything except a built-in dangerous set:
new FluxClient({ config: { htmlAllowlist: [] } });
```

- **HTML comments are dropped** — no more `<!--mk:id-->` surfacing as escaped text
  — in every mode except bare `unsafeHtml` pass-through.
- **`htmlAllowlist: ["br", …]`** renders only those inline tags; everything else is
  escaped. **`htmlAllowlist: []`** (empty) allows *all* tags **except a built-in
  dangerous set** (`script`, `style`, `iframe`, `object`, `embed`, `form`, `svg`,
  `xmp`, `plaintext`, … — **non-overridable**: allowlisting one still drops it).
- **`dropHtmlTags: ["mk", …]`** removes those tags entirely (markup gone; inner
  text stays as inert text).
- Every rendered tag's **attributes are sanitized**: `on*` handlers and `style`
  (a CSS beacon / clickjacking vector) are dropped, and dangerous URL schemes
  (`javascript:`, …, including multi-encoded) become `#`.
- **Scope:** *inline* raw HTML. Block-level raw HTML stays escaped for now (use
  `unsafeHtml` **without** the sanitizer to render block HTML — when the sanitizer
  is engaged, block HTML stays escaped even if `unsafeHtml` is also on). Tag
  matching is case-insensitive.

### Types

```ts
interface Block {
  id: number;
  kind: { type: "Paragraph" | "Heading" | "CodeBlock" | "List" | ...; data?: unknown };
  html: string;        // safe to inject via dangerouslySetInnerHTML
  open: boolean;       // still being built (last block in active tail)
  speculative: boolean; // closed by inference, may be revised
  start: number;
  end: number;
}

// Override map for <FluxMarkdown components={...} />
type Components = Record<string, React.ComponentType<any> | string>;

// Props a block-kind override receives (e.g. components.CodeBlock)
interface BlockComponentProps {
  block: Block;
  html: string;
  open: boolean;
  speculative: boolean;
  text?: string;      // decoded source — CodeBlock / MathBlock
  language?: string;  // info string — CodeBlock
}
```

`htmlToReact(html, components)` and `parseTrustedHtml(html)` are also exported
for advanced use (e.g. rendering a single block's HTML to a React tree yourself).

### `highlight(code, lang)`

Optional. Tiny native-RegExp tokenizer covering js/ts/tsx/jsx, rust, python, go, bash, sql, json, html, css. Unknown languages fall through to plain escaped text.

```ts
import { highlight } from "flux-md/highlight";
const html = highlight("const x = 1;", "ts");
```

## Coverage

**CommonMark 0.31: 100% (652/652 spec examples)** — every section, including
the hard ones (nested/loose lists, link reference definitions, link precedence,
lazy blockquote continuation). Plus GFM extensions: tables, strikethrough, task
lists, extended autolinks, GitHub alerts (`> [!NOTE]` → styled callouts),
footnotes (`[^1]` + `[^1]:`), and math (`$…$`, `$$…$$`, `\(…\)`, `\[…\]`).
Autolinks and alerts are on by default; footnotes and math are opt-in per stream
(see [Per-stream config](#per-stream-config)). See
`crates/flux-md-core/tests/{cmark_spec,gfm_spec,footnotes,math}.rs` for runners and floors.

GitHub alerts render to GitHub-compatible markup
(`<div class="markdown-alert markdown-alert-note">…`), so existing markdown CSS
styles them, and they're overridable as a block kind via `components.Alert`.

## What it doesn't do

By design, not yet, or only partially:

- **Raw HTML in markdown** — escaped by default, not passed through. (Security
  default. The `unsafeHtml: true` config flag disables the escape but must never
  be enabled for untrusted input without a `sanitize` hook.)
- **Forward link references when streaming** — a `[ref]` used *before* its later
  `[ref]: url` definition can't resolve until the definition arrives; one-shot
  parsing handles it fully, streaming converges once the definition streams in.
- **Definition lists** — out of scope for v1.
- **KaTeX / Mermaid rendering** — flux-md emits KaTeX-ready math markup
  (`<span>`/`<div class="math …">` with `gfmMath` on) and a `Mermaid` slot, but
  stays zero-dep: bring your own KaTeX / mermaid pass (or a `components.MathBlock`
  / `components.Mermaid` override) for the actual SVG/MathML output.
- **Syntax highlighting on open code blocks** — deferred until close. This is a
  deliberate perf choice.

## Performance

Every realistic streaming shape (long paragraph, fenced code block, GFM table,
blockquote/alert, flat list, math fence, reference-heavy document) parses in
**O(n) total work**, not O(n²) — at every chunk size from 16 bytes (char-by-char)
up. Each shape has an incremental cache that mirrors the structure of the block
so that an append only does work proportional to the *newly arrived* bytes, not
the growing tail. See [CHANGELOG.md](./CHANGELOG.md) for per-shape numbers and
the regression that prompted each cache; the canonical bench is
`crates/flux-md-core/examples/bench.rs` (`cargo run --release --example bench`).

Headline numbers are not durable across machines, but the curve is: chunk size
shouldn't change the order of magnitude for any shape. If you hit one that does,
file an issue with the input and chunking — that's the next bench scenario.

## Security

flux-md is XSS-safe by default — its HTML output is meant to be injected via
`innerHTML` without a downstream sanitizer:

- **Raw HTML is escaped** (the `unsafeHtml: true` config flag disables this;
  **never enable it for untrusted input without a `sanitize` hook**).
- **Dangerous URL schemes are neutralized** in `<a href>` and `<img src>` —
  `javascript:`, `vbscript:`, `data:text/html`, `data:text/javascript` become
  `#`. The check runs on the *decoded* URL and strips characters browsers
  ignore in the scheme, so obfuscations like `javascript&#58;…`,
  `javascript\:…`, `&#106;avascript:…`, and embedded tabs/newlines are caught,
  not just the literal form. (See `crates/flux-md-core/tests/security.rs`.)
- **`htmlToReact` defends in depth**: it drops inline `on*` event-handler
  attributes and runs URL attributes through the same scheme filter. It's
  intended for flux-md's own (already-sanitized) HTML; if you hand it arbitrary
  third-party HTML, these guards are your only line of defense — prefer a
  dedicated HTML sanitizer for genuinely hostile input.

### Rendering untrusted / LLM HTML safely

If you enable `unsafeHtml` to render HTML from an untrusted source (e.g. an LLM
that returns raw HTML), **bring a real sanitizer** and pass it via
`<FluxMarkdown sanitize={…} />`. flux-md applies it to every block's HTML before
injection — **including the streaming (open) tail**, which the raw-`innerHTML`
fast path would otherwise expose. flux-md stays zero-dep; you choose the
sanitizer. The realistic pattern (matches the live demo):

```tsx
import DOMPurify from "dompurify";

// Hoist to module scope (or wrap in useCallback). A fresh arrow each render
// busts FluxMarkdown's per-block memo and re-runs every block through sanitize.
const sanitize = (html: string) => DOMPurify.sanitize(html);

// …then in your component:
<FluxMarkdown client={client} sanitize={sanitize} />
```

The built-in code/math renderers operate on already-escaped content and are not
run through `sanitize`, so syntax highlighting and math markup are preserved.
With no `sanitize` prop, rendering is byte-identical and zero-cost. For
genuinely hostile content where CSS-overlay/clickjacking matters, render inside
a sandboxed `<iframe>` instead — sanitization stops injection, not every
visual-overlay trick.

### Supply chain & security posture

flux-md ships **zero runtime dependencies** — `dependencies` and
`optionalDependencies` in `package.json` are both empty. The parsing core is Rust
compiled to WebAssembly, reproducibly buildable from `crates/flux-md-core/` via
`bun run build:wasm`. The package publishes **compiled, non-minified ESM**
(`dist/*.js` + `.d.ts`); it does not ship raw `.ts`/`.tsx` source.

**Frameworks are optional peers, by design.** `react`, `vue`, `svelte`, and
`solid-js` are declared as `peerDependencies` with
`peerDependenciesMeta.optional: true`. You install only the one you use — or none
(the `flux-md/dom` and `flux-md/element` entries need no framework at all). This
is the most important supply-chain property of the package: **a React-only
consumer never installs `vue` or `solid-js`, so those frameworks' transitive
internals never enter that consumer's lockfile.** `npm i flux-md` on its own pulls
in nothing else.

**Why a registry scan may flag `seroval` and `@vue/compiler-*`.** When a scanner
resolves *all* declared peers, it surfaces alerts on framework internals reachable
only through the optional peers — **none of which is flux-md code, and none of
which is installed unless you opt into that framework:**

- `seroval` (transitive of **solid-js**) — its `deserialize()` uses
  `(0, eval)(source)` and touches the network. This is Solid's SSR serialization
  layer; it is also the package some scanners label a "potential vulnerability".
- `@vue/compiler-core` (transitive of **vue**) — uses the `Function` constructor
  for template codegen.
- `@vue/compiler-sfc` (transitive of **vue**) — references `globalThis["fetch"]`.
- minified esm-bundler builds of those compilers read as "obfuscated code".

flux-md's own source contains **no `eval` and no `Function(...)` constructor**
(`grep -rnE '\beval\s*\(|\bnew Function\b|\bFunction\s*\(' packages/flux-md/src`
returns nothing). The
repository's [`socket.yml`](https://github.com/siinghd/flux-md/blob/main/socket.yml)
documents this and disables those upstream-framework alert types for flux-md's own
CI (which installs every framework as a devDependency for cross-framework tests).
If you prefer surgical handling, ignore the specific transitive packages instead
(e.g. `@SocketSecurity ignore seroval@<version>`).

**flux-md is browser-oriented (Web Worker + WASM).** The default path runs the
WASM parser inside a Web Worker — ideal for browsers and modern Node
(`worker_threads`), but **not** intended for non-browser or older environments
that lack Workers/WASM. If you need a worker-free, synchronous path (Node SSR /
React Server Components), use **`flux-md/server`** — it loads the same WASM
synchronously off disk and renders to a string without spawning a worker.

**First-party signals a scanner will (correctly) show.** These describe flux-md
itself and are kept *visible* rather than silenced:

- **Native code (`hasNativeCode`).** The first-party `dist/wasm/flux_md_core_bg.wasm`
  (~180 KB) is built from the Rust source in this repo and runs inside a sandboxed
  Web Worker (browser) or Node worker thread. It is reproducible from source, not a
  vendored third-party binary.
- **Network access (`networkAccess`).** Only `<flux-markdown src="URL">` (the URL
  *you* supply) and the wasm-bindgen glue loading the co-located `.wasm` via
  `fetch(new URL("…_bg.wasm", import.meta.url))` — which bundlers resolve to a
  local build artifact. No telemetry, no analytics, no first-party remote
  endpoints. In privileged contexts (browser extensions, Electron) treat the `src`
  value as any external URL and allowlist it in your CSP.
- **Filesystem access (`filesystemAccess`).** Node/SSR only: `flux-md/server` reads
  the package's own `.wasm` off disk (Node's `fetch` cannot load `file://` URLs).
  It reads only the package-internal asset, never a caller-supplied path.

The `socket.yml` at the repository root documents every signal with its
justification for Socket's GitHub app.

## Scaling

`FluxClient`s share a **worker pool** (`getDefaultPool()`), so concurrency
doesn't oversubscribe OS threads. Worker creation is lazy and load-aware:

- **1 stream → 1 worker**, and each new stream gets its own worker until the cap
  (`Math.min(navigator.hardwareConcurrency || 4, 8)`) — identical to the
  per-worker behavior for small stream counts.
- **Past the cap**, new streams attach to the least-loaded worker, which
  multiplexes them (a `FluxParser` per stream id). So **50 concurrent streams
  run on ≤8 workers (~6 each)**, not 50 threads.

`destroy()` frees a stream's parser and keeps the worker warm for its siblings;
the workers persist for the life of the page. Need isolation or manual
teardown? Construct your own `new FluxPool(factory, cap)` and pass it to
`new FluxClient(pool)`, or call `pool.disposeAll()`.

`getDefaultPool()` is **browser-only** (it constructs `Worker`s) and is a
**per-page singleton** — don't rely on it in SSR/RSC. For isolation between
independent feature areas, give each its own `new FluxPool()`.

**Warm the pool to hide WASM init.** The one-time WASM load happens on the first
worker-bound op, which lands on the first-token critical path. Call
`getDefaultPool().warm()` on app load / route entry to start it early — the warm
worker is the one the first stream attaches to, so the init isn't wasted:

```ts
import { getDefaultPool } from "flux-md";
useEffect(() => { getDefaultPool().warm(); }, []); // (or your framework's mount hook)
```

### Long documents — `virtualize`

For very long documents (hundreds+ of blocks), pass `virtualize` to apply CSS
`content-visibility: auto` (+ a per-kind `contain-intrinsic-size`) to **closed**
blocks, so the browser skips style/layout/paint for off-screen content:

```tsx
<FluxMarkdown client={client} virtualize />
```

It's opt-in (off by default — short docs gain nothing) and never defers the
streaming tail (open/speculative blocks always render fully, so no flicker
where you're looking). It cuts **rendering cost, not DOM node count** — nodes
stay in the document (search, anchors, and a11y all keep working), they just
don't lay out while off-screen. Measured on a ~1800-block demo, an off-screen
**layout pass is ~7× cheaper** (≈1980ms → ≈284ms over 30 forced relayouts),
identical node count — i.e. whenever the browser would otherwise lay out
off-screen blocks (initial paint, resize, font load, scroll), that work is
skipped. No JS windowing, no scroll math, no dep — the browser does it natively.

Works best when `<FluxMarkdown>`'s parent uses normal block flow; a `flex`/`grid`
parent can interact with `contain-intrinsic-size` in surprising ways.

### Stick to bottom while streaming — `stickToBottom`

Pass `stickToBottom` and the view **follows the streaming tail, releasing when
the user scrolls up** (and re-locking when they scroll back near the bottom) —
the behavior every chat UI wants. It's **CSS-only** (CSS Scroll Snap, no JS, no
scroll listeners): flux-md emits a bottom snap target; you add one line to your
scroll container:

```tsx
<div className="chat-scroller">          {/* your existing scroll container */}
  <FluxMarkdown client={client} stickToBottom />
</div>
```
```css
.chat-scroller { overflow-y: auto; scroll-snap-type: y proximity; }
```

That's the whole feature. `proximity` (not `mandatory`) is what lets the user
scroll up freely. Note it **follows** the bottom — during very fast streaming
the lock can lag by a few px between snaps; it doesn't *hard-pin*. Re-snap on
content growth is solid in Chromium/Firefox; **Safari is weaker** at
re-snapping during streaming, so treat smooth following there as best-effort.

> **Metrics note:** because workers are shared, `getMetrics().wasmMemoryBytes`
> is the *shared* worker's heap — clients on the same worker report the same
> value. Aggregate with `Math.max`, not a sum.

## Architecture

```
┌── main thread ────────────────────────┐
│  FluxMarkdown — React, useSyncStore  │
│  FluxClient — message routing        │
└──┬──── postMessage(chunk) ────────────┘
   ▼
┌── Web Worker ─────────────────────────┐
│  worker.ts — coalesces chunks per    │
│              microtask, calls WASM   │
└──┬──── ffi ───────────────────────────┘
   ▼
┌── Rust → WASM (~170 KB after opt) ────┐
│  StreamParser:                        │
│    buffer: append-only                │
│    committed_offset                   │
│    [committed_blocks]                 │
│    [active_blocks]  (re-parsed tail)  │
│                                       │
│  scanner.rs → raw blocks              │
│  inline.rs  → emphasis stack + safe   │
│              link/code rendering      │
│  render.rs  → HTML with URL sanitize  │
└───────────────────────────────────────┘
```

Active tail re-parses on each chunk; committed blocks are frozen forever. Each block's ID is monotonic and is *preserved* across re-parses when its start offset and kind match a previously-seen active block — so React's keyed reconciliation reuses the DOM instead of remounting.

## License

MIT.

# brookmd — framework bindings & bundler setup

One Rust→WASM core, one framework-neutral DOM renderer, thin bindings on top.
Entries labelled **since 0.30.0** are new in that release.

---

## React — `brookmd/react`

```tsx
import { BrookMarkdown } from "brookmd/react";

<BrookMarkdown stream={stream} />;              // component owns the client
```

```tsx
import { BrookMarkdown, useBrookStream } from "brookmd/react";

const client = useBrookStream(stream);          // returns the owned client
<BrookMarkdown client={client} />;              // read outline()/getMetrics() off it
```

```tsx
import { BrookMarkdown, useBrookMarkdownString } from "brookmd/react";

// Already holding a growing string? setContent-diffing is done for you.
const client = useBrookMarkdownString(text, { streaming });
<BrookMarkdown client={client} />;
```

`useBrookStream(stream, { config, onError })` owns a client with
`coalesce: true`, `reattach()`es on mount, `destroy()`s on unmount, and aborts +
`reset()`s when the stream identity changes. A single-use stream (a `Response`,
a `ReadableStream`, an async generator) can only be consumed once, so React
**StrictMode**'s dev-only double mount may truncate it in development;
production mounts once and is unaffected.

`useBrookMarkdownString(content, { config, streaming, onError })` drives
`client.setContent(content, { done: streaming === false })`.

Full prop list: [api.md](api.md#2-brookmarkdown-react--props).
Examples: [basic-stream.tsx](../assets/examples/basic-stream.tsx),
[full-featured.tsx](../assets/examples/full-featured.tsx).

---

## Vanilla DOM — `brookmd/dom`

```ts
import { BrookClient } from "brookmd/client";
import { mountBrookMarkdown } from "brookmd/dom";

const client = new BrookClient();
const handle = mountBrookMarkdown(client, container, options);
// handle: { destroy(), refresh(), openBlockId(): number | null }
```

`mountBrookMarkdown` **throws** if `document` is undefined — import it anywhere,
call it only in the browser. The mount never destroys the client: you own the
worker/stream lifecycle.

`MountOptions`:

| Option | Default | Notes |
|---|---|---|
| `components` | — | `DomComponents` — block-kind / component-tag keys only, values `(props) => HTMLElement \| string`. **No lowercase tag-level overrides** (React-only). |
| `sanitize` | — | Applied to every generic block's HTML, including the open tail. |
| `virtualize` | `false` | `content-visibility: auto` on closed blocks. |
| `stickToBottom` | `false` | Emits the snap sentinel; you add `scroll-snap-type`. |
| `highlightCode` | `true` | Suppressed when `components.CodeBlock` is supplied. |
| `streamingHighlight` | `"wavefront"` | `true` \| `"wavefront"` \| `"eager"` \| `false`. |
| `batch` | `true` | One DOM write per `requestAnimationFrame`. |
| `morphOpenBlocks` | `false` | Morph a growing generic block's subtree in place, so only changed parts repaint and focus/selection in the tail survives. |
| `decorators`, `urlTransform` | — | Parity with the React props. |
| `onLinkClick` | — | **Since 0.30.0.** `(event: MouseEvent, link: LinkClickInfo) => void`. One listener on the root, removed by `handle.destroy()`. |
| `className`, `id`, `role`, `ariaLive`, `ariaAtomic` | — | Set on the root. |
| `onRenderMetrics` | — | Per-block render probe. |

Also exported: `tailOpenBlockId(snapshot): number | null`.

Example: [vanilla-dom.ts](../assets/examples/vanilla-dom.ts).

---

## Web Component `<brook-markdown>` — `brookmd/element`

```ts
import { defineBrookMarkdown } from "brookmd/element";
defineBrookMarkdown(); // or defineBrookMarkdown("my-markdown")
```

Idempotent, and a no-op where `customElements` is undefined (so it is
SSR-import-safe). It renders in **light DOM**, so your markdown CSS reaches the
content.

**Content priority:** `src` (fetch + stream) > `markdown` attribute > text
content. The `markdown` attribute is one-shot — it re-parses the whole document
on change, so never point it at a per-token-growing string.

**Config attributes are tri-state:** absent = library default; `""` / `"true"` /
`"1"` = true; `"false"` / `"0"` = false. Tri-state is the only way to turn OFF a
default-on flag.

| Attribute group | Attributes |
|---|---|
| Content | `markdown`, `src` |
| Lists | `component-tags`, `allow-schemes` |
| Parser flags | `gfm-autolinks`, `gfm-alerts`, `gfm-tagfilter`, `gfm-footnotes`, `gfm-math`, `dir-auto`, `lenient-lists`, `soft-breaks`, `a11y`, `unsafe-html`, `block-html`, `retain-committed-html` |
| Renderer flags (**since 0.30.0**) | `stick-to-bottom`, `virtualize` |

There is no attribute for `inline-component-tags`, `html-allowlist`,
`drop-html-tags` or `block-data`; drive those with a caller-owned client.

**Properties** (objects and functions cannot be attributes): `client`,
`components`, `sanitize`, `onLinkClick` (**since 0.30.0**).
**Methods:** `append(chunk)`, `finalize()`, `reset()`, `getClient()`.

A self-owned element (`src` / `markdown` / inline text / `append()`) destroys its
client on disconnect; a caller-supplied `client` is left alone.

Angular consumes the same element — add `CUSTOM_ELEMENTS_SCHEMA` and call
`defineBrookMarkdown()` once at bootstrap.

Example: [web-component.html](../assets/examples/web-component.html).

---

## Vue 3 — `brookmd/vue`

```vue
<script setup lang="ts">
import { BrookClient } from "brookmd/client";
import { BrookMarkdown } from "brookmd/vue";
const client = new BrookClient();
</script>

<template>
  <BrookMarkdown :client="client" :stick-to-bottom="true" />
</template>
```

Component props: `client` (required), `components`, `sanitize`, `virtualize`,
`stickToBottom`, `onLinkClick` (**since 0.30.0**) — only these reach the mount.

- `useBrookMarkdown(getOpts)` → `{ container }`; bind it as the `ref` of the
  element to fill. `getOpts` must read fields lazily so the watcher sees live
  identities; the six watched identities are `client`, `components`, `sanitize`,
  `virtualize`, `stickToBottom`, `onLinkClick`.
- `useBrookMarkdownString(getContent, getOptions?)` → the owned client
  (destroyed on unmount). `setContent` runs only in `onMounted` and a
  non-immediate `watch`, so nothing spawns a worker during SSR.
- `useTailBlockId(client)` → a ref of the open block's id.

---

## Svelte 4 & 5 — `brookmd/svelte`

```svelte
<script lang="ts">
  import { BrookClient } from "brookmd/client";
  import { brookMarkdown, tailBlockId } from "brookmd/svelte";
  const client = new BrookClient();
  const tail = tailBlockId(client);
</script>

<div use:brookMarkdown={{ client, stickToBottom: true }} />
```

Plain `.ts` actions — no `.svelte` compile step, so `use:` works unchanged in 4
and 5. `brookMarkdown` owns only the mount; the caller owns the client. It
remounts only when `client` / `components` / `sanitize` / `virtualize` /
`stickToBottom` / `onLinkClick` actually change identity.

`brookMarkdownString(node, { content, streaming?, config?, ...mountOptions })`
OWNS its client (constructed once from `config`, destroyed on teardown) and
reuses it across remounts so the `setContent` diff baseline survives.

`tailBlockId(client)` is a `Readable<number | null>` that changes only when the
tail id changes.

---

## Solid — `brookmd/solid`

```tsx
import { BrookClient } from "brookmd/client";
import { BrookMarkdown } from "brookmd/solid";

<BrookMarkdown client={client} class="chat" stickToBottom />;
```

`BrookMarkdownProps extends MountOptions` plus `client`, `class`, `style`, but
only `components`, `sanitize`, `virtualize`, `stickToBottom`, `onLinkClick`
(**since 0.30.0**), `highlightCode` and `batch` are forwarded to the mount.
`BrookMarkdown` returns `undefined` when `document` is undefined, so SSR is safe.

Also: `mountSolid`, `setupTailBlockId`, `createTailBlockId`,
`setupBrookMarkdownString`, `createBrookMarkdownString`.

---

## Server / SSR / RSC — `brookmd/server` + `brookmd/server/react`

```ts
import { initBrook, renderToString } from "brookmd/server";
await initBrook();                    // once at startup
const html = renderToString(markdown); // sync, no worker
```

```tsx
import { initBrook } from "brookmd/server";
import { BrookMarkdownStatic } from "brookmd/server/react";

await initBrook();
<BrookMarkdownStatic content={md} config={{ gfmMath: true }} components={COMPONENTS} />;
```

`brookmd/server` is React-free (the build asserts it imports no React), so it
works with no `react` installed. `BrookMarkdownStatic` is hookless and RSC-safe.

**SSR invariants.** The `BrookClient` constructor never touches `Worker` — the
pool is acquired lazily on the first worker-bound operation. Every entrypoint
cold-imports on a server with `window`/`document`/`navigator`/`Worker` deleted,
and the controlled-string helpers never feed the parser on the server. Do not
SSR `BrookMarkdownStatic` and hydrate with `BrookMarkdown`: use the same
component on both sides.

Example: [server-render.ts](../assets/examples/server-render.ts).

---

## Bundlers

### Vite — one required line

Vite's dependency pre-bundling hoists the wasm-bindgen glue into `.vite/deps/`,
which breaks the relative `new URL("…_bg.wasm", import.meta.url)` lookup, so the
worker cannot load WASM (a 404, or a "magic word" error). Exclude brookmd:

```ts
// vite.config.ts
export default defineConfig({
  optimizeDeps: { exclude: ["brookmd"] },
  worker: { format: "es" },
  build: { assetsInlineLimit: 0 },
});
```

`worker: { format: "es" }` and `assetsInlineLimit: 0` are what the reference
setup uses; only `optimizeDeps.exclude` is strictly required. No other bundler
needs this — it is specific to Vite's optimizer.

### Next.js App Router

Verified on Next 16 with Turbopack (the default for `next dev` and `next build`)
and with webpack. Two rules:

1. **`'use client'`.** `<BrookMarkdown>` uses hooks and spawns a Web Worker on
   mount, so it cannot be a Server Component. It is still SSR-safe: on the
   server it renders an empty shell and starts streaming after hydration.
2. **Open the stream in client code.** A `Response` / `ReadableStream` /
   `AsyncIterable` is not serializable, so passing one from a Server Component
   throws *"Only plain objects can be passed to Client Components."* Pass a URL
   or the messages, and fetch on the client.

**No `transpilePackages`** and no asset/loader config — brookmd has shipped
compiled ESM since 0.17.0, and Turbopack emits the `.wasm` itself. The Vite
`optimizeDeps` workaround does not apply.

`brookmd/styles.css` is global CSS: the App Router can import it anywhere; the
Pages Router only allows it from `pages/_app`.

Dev tip: open the app on `localhost` — Next dev blocks cross-origin dev
resources from other hosts (e.g. `127.0.0.1`) unless you list them in
`allowedDevOrigins`.

Example: [nextjs-app-router.tsx](../assets/examples/nextjs-app-router.tsx).

### webpack 5 / Rollup / Parcel

The worker and WASM are referenced with the web-standard
`new URL(asset, import.meta.url)` pattern, so any bundler with asset-module
support resolves them. These three are not machine-verified in the brookmd repo;
Vite and Node ESM/RSC are.

### Plain ESM / CDN, no bundler

Not supported as a first-class path. Nothing is inlined and there is no blob
worker — the wasm is a real asset fetched next to the worker script — so a
no-bundler setup must serve `dist/` with its relative layout intact.

### React Native

`brookmd` maps `dist/asset-urls.js` → `dist/asset-urls.native.js` under Metro,
and that shim throws with "use the brookmd-react-native package". Install
**`brookmd-react-native`** instead: it runs the same Rust core natively over JSI
rather than a Web Worker (RN ≥ 0.76, new architecture).

---

## Content Security Policy

Inferred from the loading mechanism — nothing in the repo states these:

- Same-origin **module worker** → `worker-src 'self'` (and `script-src 'self'`).
- Chromium with a restrictive `script-src` may additionally need
  `'wasm-unsafe-eval'` to instantiate the WASM module.
- **No `blob:` and no `eval` are required** — brookmd uses neither.

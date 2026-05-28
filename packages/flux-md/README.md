# flux-md

Zero-dep streaming markdown for the browser. Rust→WASM core, one Web Worker per stream, incremental parse with speculative closure for mid-stream constructs.

Drop in a streaming-aware renderer — **React, Vue, Svelte, Solid, a framework-agnostic `<flux-markdown>` Web Component, or the vanilla DOM mount** — wire each LLM stream to a `FluxClient`, and the markdown renders incrementally off the main thread, block by block, with stable identities so unchanged blocks never re-reconcile.

Parsing runs entirely **off the main thread** — each stream gets its own pooled Web Worker, so many concurrent LLM responses render without contending for the UI thread. On each token the parser re-parses only the **active tail**, not the whole document, and heavy renderers (syntax highlighting, math, mermaid) are **deferred until a block closes**. The result is low retained memory and a main thread that stays responsive while streaming. See [the live demo](https://md.hsingh.app/).

## Install

```bash
bun add flux-md     # or: npm i flux-md / pnpm add flux-md
```

flux-md ships as **source** (TypeScript + the compiled WASM). The worker and
WASM asset are referenced with the **web-standard `new URL(asset,
import.meta.url)`** pattern, so any bundler with asset-module support resolves
them: **Vite** (the reference setup), **webpack 5**, **Rollup** (with asset
modules), and **Parcel**. Next.js (webpack/turbopack) should work but is
untested — file an issue if it doesn't. It is **browser-only** (it constructs
Web Workers); it does not run under SSR/RSC. The framework packages — `react`,
`vue`, `svelte`, `solid-js` — are all **optional** peer dependencies; you only
need the one whose binding you import. The core (`flux-md`, `flux-md/client`,
`flux-md/dom`, `flux-md/element`) needs none.

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

In React:

```tsx
import { useEffect, useState } from "react";
import { FluxClient, FluxMarkdown } from "flux-md";

export function ChatMessage({ stream }: { stream: AsyncIterable<string> }) {
  // One client per component instance. Destroy on unmount, not on stream change.
  const [client] = useState(() => new FluxClient());
  useEffect(() => () => client.destroy(), [client]);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      for await (const chunk of stream) {
        if (cancelled) return; // stream changed / unmounted mid-flight
        client.append(chunk);
      }
      if (!cancelled) client.finalize();
    })();
    return () => { cancelled = true; };
  }, [client, stream]);

  return <FluxMarkdown client={client} />;
}
```

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

`mountFluxMarkdown(client, container, options?)` returns `{ destroy(), refresh() }`.
Options: `components`, `sanitize`, `virtualize`, `stickToBottom`, `highlightCode`
(default true), `batch` (default true — one DOM write per `requestAnimationFrame`).
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

The Solid binding's mount/teardown logic is tested, but its JSX component shell
has so far only been exercised through a real Solid (`vite-plugin-solid`) build
in development, not in CI — treat it as the newest of the bindings and file an
issue if your Solid setup trips on it. The component is a thin `ref`'d `<div>`;
if you hit a transform edge, `mountFluxMarkdown` from `flux-md/dom` inside
`onMount`/`onCleanup` is the zero-surprise fallback.

## What it does

| Concern | flux-md | conventional main-thread renderer |
|---|---|---|
| Re-parse on each token | No — only the active tail | Yes, full string |
| Where parse runs | Web Worker (off main thread) | Main thread |
| Block identity across chunks | Stable monotonic IDs | New keys on every render |
| Mid-stream unclosed `` ``` `` / `*` / `**` | Speculatively closed in render, replaced cleanly | Often renders raw or breaks |
| Heavy renderers (syntax, math, mermaid) | Deferred until block close | Re-run per chunk |
| XSS sanitization | Allowlist in Rust + URL scheme check | Downstream sanitizer pass on the JS thread |

## Public API

### `FluxClient`

```ts
class FluxClient {
  constructor(options?: { pool?: FluxPool; config?: ParserConfig });
  append(chunk: string): void;                      // queue text for parsing
  finalize(): void;                                 // mark stream complete
  reset(): void;                                    // wipe and reuse
  destroy(): void;                                  // free this stream's parser
  whenReady(): Promise<void>;                       // resolves once WASM loaded
  subscribe(listener: () => void): () => void;      // React-friendly store
  getSnapshot(): Block[];                           // ordered current blocks
  getMetrics(): { bytes, patches, totalParseMs, throughputKBs,
                   retainedBytes, wasmMemoryBytes, ... };
}
```

#### Per-stream config

```ts
const client = new FluxClient({
  config: {
    gfmAutolinks: true,   // bare www./http(s):// URLs + emails → links (default true)
    gfmAlerts: true,      // > [!NOTE] → callouts (default true)
    gfmFootnotes: true,   // [^1] + [^1]: → footnote section (default false)
    gfmMath: true,        // $…$ / \(…\) inline + $$…$$ / \[…\] display math (default false)
    dirAuto: true,        // per-block dir="auto" for RTL/bidi text (default false)
    unsafeHtml: false,    // pass raw HTML through (default false — keep it false for untrusted input)
    componentTags: ["Thinking", "Callout"], // custom tags with markdown inside (default none)
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
- `unsafeHtml: true` — only when rendering trusted HTML. For untrusted /
  LLM-produced HTML, pair this with `<FluxMarkdown sanitize={…} />` (DOMPurify or
  similar — see [Security](#security)).
- `componentTags: ["Thinking", …]` — when your LLM emits custom tags like
  `<Thinking>…</Thinking>` and you want their inner content parsed as markdown
  and dispatched to a React component. Safe without `unsafeHtml` (attributes are
  sanitized; allowlisted tags only).

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

- **There is no `node` prop.** flux-md has no hast tree; introspect via
  `className` / `data-*` instead.
- **Open (streaming) blocks render via `innerHTML`** — their HTML is still
  partial, so a tag-level override takes effect the moment the block commits.
- **No `components` prop ⇒ the original fast path** (`innerHTML`, byte-identical
  output). The HTML→React conversion only runs for closed blocks when you
  actually supply overrides, and is memoized per `(block id, html)`.
- For **code blocks** the built-in highlighter is the default; it is bypassed
  (so your override wins) when you pass `components.CodeBlock`, `components.pre`,
  or `components.code`.

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
`attrs`, and `html` — the **inner** (already-rendered markdown) HTML, so you can
wrap it in your own element:

```tsx
<FluxMarkdown
  client={client}
  components={{
    Thinking: ({ html }) => (
      <details className="thinking">
        <summary>Reasoning</summary>
        <div dangerouslySetInnerHTML={{ __html: html }} />
      </details>
    ),
  }}
/>
```

With no override, the component renders as `<thinking …>…</thinking>` HTML. The
override's `html` is the inner content only; `attrs` keys are React-form
(`class`→`className`, `for`→`htmlFor`) so `{...attrs}` spreads cleanly. While the
component is still streaming, `html` is the partial inner content and re-renders
as more arrives. Tag names match case-sensitively; the feature is off unless
`componentTags` is set.

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
┌── Rust → WASM (~150 KB after opt) ────┐
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

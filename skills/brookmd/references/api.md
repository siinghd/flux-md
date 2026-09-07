# brookmd — API reference

Everything here is verified against the package source. Entries labelled
**since 0.30.0** are new in that release.

---

## 1. Subpath exports

`brookmd` ships compiled ESM (`dist/*.js` + `.d.ts`) plus the WASM binary. There
is no default export; import from the narrowest subpath you need.

| Subpath | Exports | SSR-safe? |
|---|---|---|
| `brookmd` | `BrookClient`, `BrookPool`, `getDefaultPool`, `sourceFingerprint`, `BrookMarkdown`, `useBrookStream`, `useBrookMarkdownString`, `highlight`, `registerLanguage`, `supportedLangs`, `htmlToReact`, `parseTrustedHtml`, `safeUrl`, `wrapLink` + all public types | yes |
| `brookmd/client` | `BrookClient`, `BrookPool`, `getDefaultPool`, `sourceFingerprint`, `applyPatch`, `emptyBlockStore`; types `BlockStore`, `OutlineEntry`, `PersistableSnapshot` | yes — the constructor never touches `Worker` |
| `brookmd/react` | `BrookMarkdown`, `useBrookStream`, `useBrookMarkdownString`, `blockKindProps`, `blocksEqual`; type `BlockErrorInfo` | yes |
| `brookmd/server` | `isBrookReady`, `initBrookSync`, `initBrook`, `parseToBlocks`, `renderToString` | server-only by design; **React-free** |
| `brookmd/server/react` | `BrookMarkdownStatic` (hookless, RSC-safe) | server / RSC |
| `brookmd/dom` | `mountBrookMarkdown`, `tailOpenBlockId`; types `MountHandle`, `MountOptions`, `DomComponents`, `DomBlockComponent` | import-safe; `mountBrookMarkdown` **throws** without `document` |
| `brookmd/element` | `defineBrookMarkdown`, `parseTriBool`; type `LinkClickHandler` | import-safe; `defineBrookMarkdown` is a no-op without `customElements` |
| `brookmd/vue` | `useBrookMarkdown`, `useTailBlockId`, `BrookMarkdown`, `useBrookMarkdownString` | yes |
| `brookmd/svelte` | `brookMarkdown`, `tailBlockId`, `brookMarkdownString` | yes |
| `brookmd/solid` | `BrookMarkdown`, `mountSolid`, `setupTailBlockId`, `createTailBlockId`, `setupBrookMarkdownString`, `createBrookMarkdownString` | yes |
| `brookmd/highlight` | `highlight`, `supportedLangs`, `escapeHtml`, `stepHighlight`, `registerLanguage` (**since 0.30.0**) | yes (pure strings) |
| `brookmd/html-to-react` | `htmlToReact`, `parseTrustedHtml`, `wrapLink`, `safeUrl`, `decodeEntities` | yes |
| `brookmd/block-props` | `blockProps`, `extractLang`, `htmlAttrs` | yes |
| `brookmd/worker-core` | `WorkerCore`; types `ParserLike`, `WorkerCoreDeps` | yes |
| `brookmd/types` | every public type (`RenderMetrics`, `RenderMetricsHook`, `ListItemData`, `WireHtmlDelta`, `WireActiveBlock`, …) | type-only |
| `brookmd/styles.css` | the optional default theme | n/a |

`react`, `vue`, `svelte` and `solid-js` are **optional** peer dependencies. The
bare `brookmd` entry re-exports the React surface, so it pulls `react` — import
from `brookmd/client` for a framework-free core.

---

## 2. `<BrookMarkdown>` (React) — props

`BrookMarkdownProps` is not exported as a type; use the prop names directly.
Exactly one of `client` or `stream` is required (`client` wins); passing neither
throws.

| Prop | Type | Default | Meaning |
|---|---|---|---|
| `client` | `BrookClient` | — | Caller-owned client. The component never destroys it. |
| `stream` | `AsyncIterable<string> \| ReadableStream<Uint8Array> \| Response` | — | One-line mode: the component owns an internal client via `useBrookStream`. |
| `streamConfig` | `ParserConfig` | — | Config for the internally created client (stream mode only). |
| `onStreamError` | `(err: Error) => void` | — | Pipe/source errors, and worker fatals carrying `err.fatal`. |
| `components` | `Components` | — | Override map. See §5 — it has **two prop contracts**. |
| `virtualize` | `boolean` | `false` | `content-visibility: auto` wrapper on **closed** blocks only. |
| `stickToBottom` | `boolean` | `false` | Emits `<div class="brook-bottom-anchor" style="scroll-snap-align:end">`; you add `scroll-snap-type: y proximity` to the scroller. |
| `sanitize` | `(html: string) => string` | — | Runs before every `innerHTML` injection **including the open tail**. Built-in code/math/mermaid renderers bypass it. |
| `decorators` | `Decorator[]` | — | Post-parse inline TEXT-node matcher. **Output is trusted / unsanitized.** |
| `urlTransform` | `UrlTransform` | — | Rewrites `href`/`src`/`poster`; output is re-sanitized. |
| `onLinkClick` | `(event: React.MouseEvent<HTMLElement>, link: LinkClickInfo) => void` | — | **Since 0.30.0.** One delegated listener on the `.brook-md` root. `event.preventDefault()` cancels navigation. Streaming links with no href yet (`<a data-brook-pending>`) never fire it. |
| `childMemo` | `boolean` | `false` | Reuse unchanged top-level children of an OPEN block (only on the `components`/`sanitize` walk path). |
| `streamingHighlight` | `boolean \| "wavefront" \| "eager"` | `true` ⇒ `"wavefront"` | Highlight an open fence. `"eager"` recolours the tail per patch; `false` = plain until close. |
| `deferTail` | `boolean` | `false` | Routes the block list through `useDeferredValue`; adds `brook-deferred` to the root while deferring. |
| `className` | `string` | — | Appended to the root class, which is always `brook-md`. |
| `id`, `role` | `string` | — | Set on the root. |
| `aria-live` | `"off" \| "polite" \| "assertive"` | off | Live region. |
| `aria-atomic` | `boolean` | off | Pair with `aria-live`. |
| `onRenderMetrics` | `RenderMetricsHook` | — | Fires per ACTUAL block render; also advances `client.getMetrics().renderCount`. |
| `onBlockError` | `(error, info: BlockErrorInfo) => void` | — | Per-block error boundary. `BlockErrorInfo = { blockId, kind, componentKeys, html }`. |

`LinkClickInfo = { href: string; text: string; element: HTMLAnchorElement }`.

`onLinkClick` is delegated, so — unlike `components` / `decorators` /
`urlTransform` / `sanitize` — a fresh closure each render re-renders **no**
blocks. Every other non-primitive prop must be hoisted or memoized.

### Hooks

```ts
useBrookStream(stream, options?): BrookClient
// options: { config?: ParserConfig; onError?: (err: Error & { fatal?: boolean }) => void }
```
Owns a client (`coalesce: true`), reattaches on mount, destroys on unmount,
aborts + `reset()`s when the stream identity changes.

```ts
useBrookMarkdownString(content, options?): BrookClient
// options: { config?: ParserConfig; streaming?: boolean; onError?: ... }
```
Drives `client.setContent(content, { done: streaming === false })`. A
prefix-extension appends only the delta; a divergence resets and reparses.

**Pass `streaming: false` once the content is final.** Omitted or `true` leaves
the stream OPEN, so the last block never commits — a finished fence never
highlights and never shows its Copy button. brookmd deliberately refuses to
infer "done" from an absent flag: that would re-finalize on every token for
callers who grow the string without it, which is an O(n²) reparse trap.

---

## 3. `BrookClient`

```ts
new BrookClient(options?)
```

| Option | Default | Meaning |
|---|---|---|
| `pool` | `getDefaultPool()` | Worker pool. Pass your own `BrookPool` for a custom worker factory. |
| `config` | — | `ParserConfig`, applied once when the stream's parser is created and **immutable** for that stream (a `reset()` keeps it). |
| `onError` | — | `(err: { message: string; fatal?: boolean }) => void`. |
| `onBlock` | — | `(block: Block) => void`, once per commit. |
| `coalesce` | `false` | rAF-batch store notifications. The finalize patch always flushes synchronously. |
| `recovery` | `true` | One transparent re-feed after a fatal worker death, so the view never blanks. |

**Methods.** `append(chunk)`, `finalize()` (there is **no `done()`**),
`pipeFrom(src, { signal })` (an abort skips finalize), `setContent(content, { done })`,
`reset()`, `destroy()`, `reattach()` (StrictMode), `subscribe(fn) => unsub`,
`getSnapshot(): Block[]`, `whenReady(): Promise<void>`,
`getPersistable(source?): PersistableSnapshot`, `hydrate(snapshot, { source }?)`,
`outline(): OutlineEntry[]`, `toPlaintext(): string`, `getMetrics()`.

`subscribe` and `getSnapshot` are arrow properties — safe to pass unbound to
`useSyncExternalStore`.

**Getters.** `ready: boolean`, `failed: Error | null`.

`getMetrics()` returns `{ bytes, patches, meanParseMicros, totalParseMs,
throughputKBs, committedBlocks, activeBlocks, lastPatchAgoMs, retainedBytes,
wasmMemoryBytes, renderCount, rebuildCount }`.

**Persistence.** `getPersistable()` captures `{ hydrateVersion, blocks,
sourceLength, sourceHash, done }`; `hydrate()` restores it into an untouched
client with no worker, no WASM and no parse. Validate
`snap.sourceHash === sourceFingerprint(mySource)` before hydrating. A `done: true`
snapshot is terminal (appending throws); a `done: false` one needs `{ source }`
to resume.

**Pool.** `new BrookPool(factory, cap, { bootTimeoutMs = 20000, setTimeout, clearTimeout })`
with `acquire`/`release`/`reattach`/`send`/`whenWorkerReady`/`warm`/`disposeAll`
and getters `workerCount` / `handlerCount`. The default pool's cap is
`min(navigator.hardwareConcurrency || 4, 8)`. `getDefaultPool()` is browser-only
and a per-page singleton — don't rely on it in SSR/RSC.

---

## 4. `ParserConfig`

Per-stream, applied once, immutable for that stream's lifetime. Use a new client
for different flags.

| Field | Type | Default | What it does |
|---|---|---|---|
| `gfmAutolinks` | `boolean` | **`true`** | Bare `www.` / `http(s)://` / `ftp://` / email autolinks. |
| `gfmAlerts` | `boolean` | **`true`** | `> [!NOTE]` → callouts. |
| `gfmTagfilter` | `boolean` | `false` | With `unsafeHtml` on, escapes the nine GFM-disallowed tags. |
| `gfmFootnotes` | `boolean` | `false` | `[^1]` + `[^1]:` → a footnote section. |
| `gfmMath` | `boolean` | `false` | `$…$` / `\(…\)` inline and `$$…$$` / `\[…\]` display → KaTeX-ready `.math` markup. |
| `dirAuto` | `boolean` | `false` | `dir="auto"` on block-level text elements (per-block bidi). |
| `lenientLists` | `boolean` | `false` | A marker followed by ≥6 columns of SPACE padding yields item text instead of indented code. |
| `softBreaks` | `boolean` | `false` | A bare `\n` inside inline content renders as `<br>`. |
| `a11y` | `boolean` | `false` | `<label>`-wrapped task checkboxes, `scope="col"` on `<th>`. |
| `unsafeHtml` | `boolean` | `false` | Pass raw HTML through unescaped. **Never for untrusted input.** |
| `componentTags` | `string[]` | off | Allowlist of BLOCK component tags; inner content parsed as markdown, attributes sanitized. Case-sensitive. |
| `inlineComponentTags` | `string[]` | off | Same, for INLINE positions (chips, citations, mentions). |
| `htmlAllowlist` | `string[]` | unset = off | Engages the safe raw-HTML sanitizer even when `[]` (= allow all but a dangerous set). A non-empty array renders only those tags. |
| `dropHtmlTags` | `string[]` | unset = off | Tags removed entirely (inner text stays). Also engages the sanitizer. |
| `blockHtml` | `boolean` | `false` | Extends the sanitizer to CommonMark HTML block types 6–7. Needs `htmlAllowlist`/`dropHtmlTags` to do anything. |
| `allowSchemes` | `string[]` | `[]` | Un-blocklist for overridable-blocked schemes (currently only `file`). Bare names, no colon. |
| `blockData` | `boolean` | `false` | Populate `block.kind.data` with typed table/heading/code/math/list/container data. |
| `retainCommittedHtml` | `boolean` | `false` on the worker path | Keep every committed block's HTML inside the parser. The server renderers pin it on. |

`streamingHighlight` is a **renderer** option (React prop / `MountOptions`), not
part of `ParserConfig`.

---

## 5. `components` — the two prop contracts

The same map is read by two dispatchers, and they pass different props.

**Block contract.** A capitalized block-kind key (`BlockKindTag`:
`Paragraph`, `Heading`, `CodeBlock`, `MathBlock`, `Mermaid`, `List`,
`Blockquote`, `Alert`, `Table`, `Rule`, `Html`, `Component`) — or a
`componentTags` tag matched at block level — receives `BlockComponentProps`.

**Tag contract.** The SAME key is also matched by ELEMENT NAME while converting
a block's HTML to React — that is how `a` / `code` / `table` / `img` overrides
work, and how an `inlineComponentTags` chip, or a component tag nested in a list
item or blockquote, renders. That call passes the element's attributes and
`children` only: **there is no `block` prop.**

So a component registered for a name that can appear in both positions must not
assume `block` exists:

```tsx
const Thinking = ({ block, children }: any) =>
  block ? <Panel data={block.kind.data}>{children}</Panel> : <span>{children}</span>;
```

Block-kind keys are typed to `BlockComponentProps` in the `Components` mapped
type, so the mismatch is a compile error rather than a runtime `undefined` deref.
brookmd also refuses to dispatch a raw element whose name collides with a
block-kind key, and wraps every block in an error boundary so a throwing
override costs one block instead of the document.

Dispatch order for a `Component` block: `components[tag]` → `components.Component`
→ `components[kind]`. The built-in `CodeBlock` / `MathBlock` / `Mermaid`
renderers are bypassed by `components.CodeBlock`, `components.pre`, or
`components.code`.

On the React path `attrs` are renamed `class` → `className` and `for` →
`htmlFor`, so `{...attrs}` spreads cleanly. The DOM path keeps the literal HTML
names.

The DOM renderer (`brookmd/dom`, and therefore `<brook-markdown>`) has only the
BLOCK contract: `DomComponents = Record<string, (props: BlockComponentProps) => HTMLElement | string>`.
Tag-level overrides are React-only.

### `BlockComponentProps`

| Prop | Type | When |
|---|---|---|
| `block` | `Block` | always (block contract) |
| `html` | `string` | always — for a `Component` block this is the INNER rendered HTML |
| `children` | `unknown` | React only, when a `components` map is supplied; cast to `ReactNode` |
| `open` | `boolean` | always — true while the block is still streaming |
| `speculative` | `boolean` | always — closed speculatively, may yet be revised |
| `text` | `string?` | decoded source for `CodeBlock` / `MathBlock` (populated even with `blockData` off) |
| `language` | `string?` | info-string language for `CodeBlock` (always on) |
| `meta` | `string?` | everything after the language word, trimmed (always on); appears only once it can no longer change |
| `tag` | `string?` | component tag name for `Component` blocks |
| `attrs` | `Record<string, string>?` | sanitized attributes for `Component` blocks |
| `table` | `TableData?` | `Table` + `blockData` |
| `heading` | `HeadingData?` | `Heading` + `blockData` |
| `code` | `CodeBlockData?` | `CodeBlock` + `blockData` |
| `math` | `MathBlockData?` | `MathBlock` + `blockData` |
| `list` | `ListData?` | `List` + `blockData` |
| `container` | `ContainerData?` | `Blockquote` / `Alert` + `blockData` |

`Block = { id: number; kind: BlockKind; start: number; end: number; html: string; open: boolean; speculative: boolean }`
and `BlockKind = { type: BlockKindTag; data?: unknown }`. `start`/`end` are
offsets into the source.

### `blockData` shapes

| Kind | `kind.data` with `blockData: true` | with it off |
|---|---|---|
| `Table` | `{ headers: TableCell[]; rows: TableCell[][]; aligns: Align[] }`, cell = `{ text, html }` | `undefined` |
| `Heading` | `{ level, text, id }` | a bare `number` (the level) — it is a union, guard before reading |
| `CodeBlock` | `{ lang, meta?, code }` — `code` is the decoded source | `{ lang }` (+ `meta`) |
| `MathBlock` | `{ latex }` — decoded LaTeX | no data |
| `List` | `{ ordered, start?, items?: ListItemData[] }`; `ListItemData = { html, start? }` where `start` is the document-absolute marker offset, absent for nested items | `{ ordered }` |
| `Blockquote` / `Alert` | `{ nested: NestedBlock[] }` (Alert also carries `{ kind }`) | Blockquote none; Alert `{ kind }` |

`Align = "left" | "center" | "right" | null`.

`props.list` is set only when `typeof data.start === "number"`; `props.heading`
only when the data is an object. `blockData` also enables keyed streaming fast
paths for open `Table` / `List` / `Blockquote`-`Alert` blocks, so a growing
container re-renders only its open last inner block.

---

## 6. Other types

```ts
interface Decorator {
  match: RegExp | string;                                  // tested per inline TEXT node
  replace: (matchText: string, groups: string[]) => BrookNode; // PURE
  skipInside?: string[];                                   // default ['a','code','pre','kbd']
}

type UrlTransform = (url: string, ctx: { tag: string; attr: "href" | "src" | "poster" }) => string;

interface OutlineEntry { level: number; text: string; id: number }
```

`decorators` run post-parse on real inline TEXT nodes only, once per committed
block — they never see URLs, code or markup, and a value split by inline markup
(`$2.<em>5</em>B`) is two text nodes and won't match across them.

---

## 7. Server API

```ts
import { initBrook, initBrookSync, isBrookReady, parseToBlocks, renderToString } from "brookmd/server";
import { BrookMarkdownStatic } from "brookmd/server/react";
```

- `initBrook(opts?)` — async, idempotent. Node reads the co-located `.wasm` off
  disk; the web path fetches the bundler-resolved asset. `initBrook({ wasm })`
  supplies bytes yourself. A failed init is not cached, so it can be retried.
- `initBrookSync(bytes)` — for edge runtimes with no filesystem.
- `renderToString(md, { config })` — synchronous HTML string, zero React.
- `parseToBlocks(md, { config })` — the block array.
- `<BrookMarkdownStatic content config components className id role aria-live aria-atomic />`
  — hookless, RSC-safe, **render-once**. No error boundary, no Mermaid, no
  client-side highlighting. Do not SSR `BrookMarkdownStatic` and hydrate with
  `BrookMarkdown`: use the same component on both sides.

**Document assembly rule.** A `Block.html` never ends with a newline. Insert a
`\n` before a block only when the output does not already end with one, and end
the document with a single `\n`. An unconditional `"\n".join(...)` doubles the
newline a raw HTML block serializes for itself.

See [../assets/examples/server-render.ts](../assets/examples/server-render.ts).

# brookmd — troubleshooting

Symptom → cause → fix.

---

## The container is empty: `<div class="brook-md">` with nothing in it and no console output

**Cause.** The worker script 404'd or its boot never completed — most often a
stale hashed worker URL in a long-lived tab after a deploy, or a bundler that
did not emit the worker/WASM asset.

**Fix.**
- Be on ≥ 0.24.0, where the client registers `error` / `messageerror` listeners
  on the worker and enforces a per-worker **boot deadline (default 20 000 ms)**.
- Render a fallback off the failure signals:

```ts
const client = new BrookClient({
  onError: (err) => { if (err.fatal) showReloadPrompt(); },
});
// …or poll the getter:
if (client.failed) showReloadPrompt();
```

- `recovery` defaults to **true**: the first fatal worker death is healed
  invisibly by re-feeding the buffered document, `failed` stays `null`, and
  `onError` does **not** fire. A *second* death is terminal — that is when
  `failed` is set and `onError` fires. Long-lived tabs should prompt a reload.
- Tune or disable the boot deadline with `new BrookPool(factory, cap, { bootTimeoutMs })`
  (`0` disables it) and pass the pool as `new BrookClient({ pool })`.
- With `recovery: false` you save the buffer on giant documents, but
  `getPersistable()` then throws unless you pass the source explicitly, and a
  worker death restarts the document with a dev warning instead of healing.

---

## Vite: WASM 404, or "magic word" / bad module error

**Cause.** Vite's dependency pre-bundling hoists the wasm-bindgen glue into
`.vite/deps/`, which breaks the relative `new URL("…_bg.wasm", import.meta.url)`
lookup.

**Fix.**

```ts
// vite.config.ts
export default defineConfig({
  optimizeDeps: { exclude: ["brookmd"] },
  worker: { format: "es" },
  build: { assetsInlineLimit: 0 },
});
```

Only `optimizeDeps.exclude` is strictly required. This is specific to Vite; no
other bundler needs it.

---

## Next.js: "Only plain objects can be passed to Client Components", or a hooks error

**Cause (a).** The component is a Server Component. `<BrookMarkdown>` uses hooks
and spawns a Web Worker on mount.
**Fix.** Put `'use client'` at the top of the file.

**Cause (b).** A `Response` / `ReadableStream` / `AsyncIterable` was passed as a
prop from a Server Component. Streams are not serializable.
**Fix.** Pass a URL or the messages from the server and open the stream in
client code.

You do **not** need `transpilePackages` (not since 0.17.0) or any
asset/loader config, and the Vite `optimizeDeps` workaround does not apply.

---

## React hydration mismatch on the root

**Cause.** The root's `className` / `role` / `aria-live` / `aria-atomic` / `id`
differ between the server markup and the client render.

**Fix.** Pass identical root a11y props on both sides. Also: do not SSR
`<BrookMarkdownStatic>` and hydrate with `<BrookMarkdown>` — the client
renderer's markup (code chrome, Mermaid slots) does not hydrate the static
renderer's plainer markup. Use the same component on both sides.

---

## Everything re-renders on every patch

**Cause.** An inline object/array/function identity for `components`,
`decorators`, `urlTransform`, `sanitize` or `onBlockError`. A fresh identity
busts the per-block memo, so every committed block re-renders per patch —
quadratic over a stream.

**Fix.** Define them at module scope, or `useMemo`/`useCallback` with a stable
dependency list. In development brookmd emits a one-shot warning when
`decorators` or `urlTransform` change identity. `onLinkClick` is exempt: it is
delegated to the root, so a fresh closure re-renders nothing.

Confirm with `onRenderMetrics` — a committed block whose `renderCount` keeps
climbing is the smoking gun.

---

## `can't access property "kind", block is undefined` and the page blanks

**Cause.** The `components` **two-contract collision**. The same key is served by
two dispatchers: the block dispatcher passes `BlockComponentProps` (with
`block`), the element dispatcher passes attributes + `children` only (**no
`block`**). A component registered for a name that can appear in both positions
read `props.block` unconditionally.

**Fix.** Guard:

```tsx
const Thinking = ({ block, children }: any) =>
  block ? <Panel data={block.kind.data}>{children}</Panel> : <span>{children}</span>;
```

Type block-kind keys to `BlockComponentProps` (the `Components` mapped type does
this for you, so the mismatch becomes a compile error), and never read
`props.block` on an element-path override. The per-block error boundary contains
the crash to one block; wire `onBlockError` to see it.

---

## Code renders uncoloured

**Cause.** `brookmd/styles.css` was not imported. The highlighter emits
`<span class="t-…">` and ships no colours of its own.

**Fix.** `import "brookmd/styles.css";` — or define the `--brook-t-*` variables /
`.t-*` rules yourself if you bring your own theme.

Related: a big fence can render unhighlighted for a beat because close-time
highlighting is sliced across tasks. The settled markup is byte-identical to a
direct `highlight()` call.

---

## A finished code fence never highlights and shows no Copy button

**Cause.** The stream was never finalized. With `useBrookMarkdownString` /
`brookMarkdownString` / `useBrookMarkdown`-string helpers, omitting `streaming`
(or leaving it `true`) keeps the stream OPEN forever.

**Fix.** Pass `streaming: false` when the content is final — or call
`client.finalize()` / `client.setContent(text, { done: true })` directly.

---

## Math is not typeset while streaming

**Cause.** By design. An open block holds partial LaTeX, and KaTeX throws on it
on essentially every patch.

**Fix.** Skip anything inside `.brook-open` / `.brook-streaming` in your typeset
pass, mark what you have typeset (`data-tex`) so the pass is idempotent, and
re-run it from a `MutationObserver`. Same rule for Mermaid: gate strictly on
`!props.open`. See [recipes.md](recipes.md#katex).

---

## `componentTags` / `blockData` / `gfmMath` silently stop working mid-session

**Cause.** The stream's config was latched and not resent when the parser was
rebound to a new worker. Fixed in 0.24.0 / 0.25.0.

**Fix.** Upgrade. Remember that `ParserConfig` is applied once and is
**immutable** for a stream's lifetime — `reset()` keeps it; use a new client for
different flags.

---

## Duplicate React keys, or the document shrinks after an error

**Cause.** Two parser generations merged under colliding block ids after a
worker restart. Fixed: a restart now starts a clean generation and warns.

**Fix.** Upgrade. In development you will see "the parser was rebuilt on a new
worker … document restarted".

---

## A missed final render, or a frame-late "done"

**Cause.** rAF coalescing (`coalesce: true` on the React hooks' internal client,
`batch: true` on the DOM mount).

**Fix.** Nothing — the finalize patch always flushes **synchronously**, and
`reset()` cancels the pending frame. If you own the client and need every patch
synchronously, construct it with `coalesce: false` (the constructor default).

---

## First-token latency spike

**Cause.** WASM init is on the critical path for the first stream.

**Fix.** Warm the pool on route entry:

```tsx
useEffect(() => { getDefaultPool().warm(); }, []);
```

---

## Content Security Policy

Inferred from the loading mechanism — nothing in the repo states these
directives, so treat them as a starting point:

- The worker is a **same-origin ES module worker**, so you need
  `worker-src 'self'` and `script-src 'self'`.
- Chromium with a restrictive `script-src` may additionally require
  `'wasm-unsafe-eval'` to instantiate the module.
- **No `blob:` and no `eval` are needed** — brookmd creates neither a blob
  worker nor inlined base64 WASM. The package's own build asserts this.

---

## React Native: "use the brookmd-react-native package"

**Cause.** Metro resolved `brookmd`'s `react-native` field, which maps the asset
resolver to a shim that throws — the Web Worker path cannot work on Hermes.

**Fix.** Install **`brookmd-react-native`**, which runs the same Rust core
natively over JSI (RN ≥ 0.76, new architecture).

---

## Memory grows on very long streams

- `retainCommittedHtml` is **off** on the worker path by default, which is what
  you want: the client receives each committed block once and stores it itself.
  Turn it on only if you need the parser to still hold the whole document.
  The server renderers pin it on regardless.
- `virtualize` applies `content-visibility: auto` to **closed** blocks (hundreds
  of blocks and up). It never defers the tail. Beware `flex`/`grid` parents.
- `stickToBottom` is CSS-only: it emits a snap sentinel, and you must add
  `scroll-snap-type: y proximity` to your scroll container. Safari re-snap is
  best-effort.
- Read `getMetrics().retainedBytes` / `wasmMemoryBytes` to confirm — but note
  worker-level numbers are shared by every stream multiplexed onto that worker.

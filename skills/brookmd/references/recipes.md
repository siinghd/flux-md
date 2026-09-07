# brookmd — recipes

Copy-pasteable solutions to the things people actually build. Every map,
function and array below belongs at **module scope**: a fresh identity each
render busts the per-block memo and re-renders the whole document per patch.

Runnable versions: [../assets/examples/](../assets/examples/tsconfig.json).

---

## Vercel AI SDK `useChat`

`useChat` gives you a growing **string** per message (joined from
`message.parts`), not a stream — so bridge it with `useBrookMarkdownString`.

```tsx
import { memo } from "react";
import { useChat } from "@ai-sdk/react";
import { BrookMarkdown, useBrookMarkdownString } from "brookmd/react";
import { getDefaultPool } from "brookmd/client";
import type { ParserConfig } from "brookmd";

const CONFIG: ParserConfig = { softBreaks: true, dirAuto: true, a11y: true, blockData: true };

const Message = memo(function Message({ text, streaming }: { text: string; streaming: boolean }) {
  const client = useBrookMarkdownString(text, { config: CONFIG, streaming });
  return <BrookMarkdown client={client} components={COMPONENTS} className="brook-caret" />;
});

export function Chat() {
  const { messages, status } = useChat();
  useEffect(() => { getDefaultPool().warm(); }, []);
  const lastId = messages.at(-1)?.id ?? null;

  return messages.map((m) => (
    <Message
      key={m.id}
      text={m.parts.filter((p) => p.type === "text").map((p) => p.text).join("")}
      streaming={status === "streaming" && m.id === lastId}
    />
  ));
}
```

Three things that are easy to get wrong:

1. **`streaming: false` on finish.** Omitted or `true` leaves the stream OPEN,
   so the last block never commits: a finished fence never highlights and never
   shows its Copy button. brookmd refuses to infer "done" from an absent flag
   because that would re-finalize on every token — an O(n²) reparse trap.
2. **Only the last message can be streaming.** Gate on the message id, or every
   older bubble stays open forever.
3. **`getDefaultPool().warm()`** in the chat shell moves WASM init off the
   first-token critical path.

Full file: [ai-sdk-chat.tsx](../assets/examples/ai-sdk-chat.tsx).

---

## KaTeX

With `gfmMath: true` the core emits KaTeX-ready markup — `<span class="math
math-inline">` / `<div class="math math-display">` carrying the LaTeX — and
stays zero-dep. Two ways to typeset it.

### A. One observer pass over the subtree (what the demo app ships)

Cheap, framework-agnostic, and correct for the streaming case. The two
non-obvious guards are the ones that matter: **skip anything still inside an
open block** (half-typed LaTeX makes KaTeX throw on every patch) and mark what
you typeset so the pass is idempotent.

```tsx
useEffect(() => {
  const root = scrollerRef.current;
  if (!root) return;
  let retry = 0;

  const pass = () => {
    const katex = (window as any).katex;
    if (!katex) { retry = window.setTimeout(pass, 200); return; } // CDN not up yet
    root.querySelectorAll<HTMLElement>(".math:not([data-tex])").forEach((el) => {
      if (el.closest(".brook-streaming, .brook-open")) return; // still streaming
      el.setAttribute("data-tex", "1");                        // idempotence marker
      try {
        katex.render(el.textContent ?? "", el, {
          displayMode: el.classList.contains("math-display"),
          throwOnError: false,
          output: "html",
        });
      } catch { /* leave the raw LaTeX in place */ }
    });
  };

  const obs = new MutationObserver(() => pass());
  obs.observe(root, { childList: true, subtree: true });
  pass();
  return () => { obs.disconnect(); if (retry) clearTimeout(retry); };
}, []);
```

### B. A `components.MathBlock` override

Uses the **block** contract, so `props.text` is the decoded LaTeX (populated
even with `blockData` off; `props.math.latex` is the typed form when it is on)
and `props.open` tells you whether it is safe to typeset yet.

```tsx
const MathBlock = memo((props: BlockComponentProps) => {
  const html = useMemo(
    () => (props.open ? null : katex.renderToString(props.text ?? "", {
      displayMode: true, throwOnError: false,
    })),
    [props.text, props.open],
  );
  return html
    ? <div className="math math-display" dangerouslySetInnerHTML={{ __html: html }} />
    : <pre>{props.text}</pre>;
});
```

Note that `components.MathBlock` replaces the *block* slot only; **inline** math
still needs pass A (or your own inline walk).

---

## Mermaid

`mermaid.render` throws on a half-arrived diagram, which during streaming is
every patch but the last. Gate strictly on `!open`:

```tsx
const Mermaid = memo((props: BlockComponentProps) => {
  const [svg, setSvg] = useState<string | null>(null);
  const id = useRef(`mmd-${Math.random().toString(36).slice(2)}`);

  useEffect(() => {
    if (props.open) { setSvg(null); return; }          // never render a partial diagram
    let live = true;
    mermaid.render(id.current, props.text ?? "")
      .then((r) => { if (live) setSvg(r.svg); })
      .catch(() => {});
    return () => { live = false; };
  }, [props.text, props.open]);

  return svg
    ? <div dangerouslySetInnerHTML={{ __html: svg }} />
    : <pre className="mermaid-source">{props.text}</pre>;   // readable while streaming
});

const COMPONENTS: Components = { Mermaid };
```

The DOM renderer's variant returns `HTMLElement | string` instead of JSX.
brookmd's own default `Mermaid` slot just renders the source verbatim — it ships
no Mermaid runtime.

---

## Shiki (or any async highlighter)

`components.CodeBlock` bypasses the built-in highlighter entirely (so does
`components.pre` or `components.code`). An async highlighter cannot run per
patch, so render plain while `open` and highlight once on close:

```tsx
const CodeBlock = memo((props: BlockComponentProps) => {
  const { text = "", language = "text", open } = props;
  const [html, setHtml] = useState<string | null>(null);

  useEffect(() => {
    if (open) { setHtml(null); return; }               // plain until the fence closes
    let live = true;
    codeToHtml(text, { lang: language, theme: "github-dark" })
      .then((out) => { if (live) setHtml(out); }, () => {});
    return () => { live = false; };
  }, [text, language, open]);

  return html
    ? <div dangerouslySetInnerHTML={{ __html: html }} />
    : <pre><code>{text}</code></pre>;
});
```

`props.meta` carries everything after the language word in the info string
(e.g. `title="src/main.ts"`), and it appears only once it can no longer change —
so a filename header never flickers through a half-typed value.

Trade-off: replacing the slot forfeits brookmd's incremental streaming
highlighter. If all you need is a missing language, `registerLanguage` keeps it.

---

## Register a language with the built-in highlighter

**Since 0.30.0.** Adds a language *inside* the incremental streaming
highlighter, so it keeps colouring the open fence as it grows.

```ts
import { registerLanguage } from "brookmd/highlight";

registerLanguage(["hcl", "tf"], {
  // Ordered [token class, sticky regex] pairs. At each cursor position the
  // FIRST match wins, so put longer forms first. Every regex MUST carry the
  // `y` flag — it is matched against the whole source with `lastIndex` at the
  // cursor, so `^`/`$` (with `m`), lookaheads and `\b` all still work.
  pats: [
    ["com", /#.+/y],
    ["str", /"(?:\\.|[^"\\\n])*"/y],
    ["num", /\b\d+(?:\.\d+)?/y],
    ["ident", /\w+/y],   // required for `kw` to have anything to promote
    ["pun", /[=[\]{}(),.]/y],
    ["ws", /\s+/y],
  ],
  kw: ["resource", "variable", "module", "output", "true", "false"],
});
```

`names` is one name or an array of aliases, lower-cased. Token classes are the
`t-*` set the bundled theme already colours — `kw`, `str`, `rx`, `num`, `lt`,
`com`, `fn`, `ty`, `mac`, `dec`, `attr`, `sel`, `tag`, `var`, `pun`, `txt` —
plus `ws` (whitespace, emitted raw with no span) and `ident`, the pseudo-class
whose matches are promoted to `kw` when they appear in the `kw` set. `kw`
without an `ident` pattern does nothing. A pattern must never match the empty
string (registration throws).

It throws a `TypeError` (naming the offending pattern index) for a non-sticky
regex or an unknown token class. Registering an existing name **replaces** its
table — including a built-in one, which is how you retune `yaml` or `json`
rather than living with the shipped taste.

Caveat: while the block is still streaming, a registered language is
re-tokenized from the top on each patch instead of growing a frozen prefix (the
frozen-prefix rule needs to know which forms can run past a newline, and a
caller-supplied table does not say). The size cap bounds that work, and nothing
about the settled markup differs.

Built in as of 0.30.0: js/javascript/jsx, ts/tsx/typescript, rust/rs,
py/python, go, bash/sh/shell, json, sql, html/xml, css, java, c, cpp/c++,
cs/csharp, swift, kt/kotlin, php, rb/ruby, yaml/yml, toml, diff, dockerfile.
`supportedLangs()` returns the live list, registrations included. An unknown
language, or code longer than 50 000 characters, renders as plain escaped text.

---

## Table toolbar from `blockData`

With `blockData: true` a `Table` block's `props.table` is
`{ headers, rows, aligns }` where every cell is `{ text, html }` — `text` is
inline-stripped plaintext for sort/filter/CSV/chart, `html` is the display
markup. No HTML re-parse, no AST walk.

```tsx
// RFC-4180: quote any field containing a comma, quote, or newline; double
// internal quotes. Built from cell.text — never from the display HTML.
function toCsv(table: TableData): string {
  const quote = (s: string) => (/[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
  const line = (cells: { text: string }[]) => cells.map((c) => quote(c.text)).join(",");
  return [line(table.headers), ...table.rows.map(line)].join("\n");
}

const Table = memo((props: BlockComponentProps) => {
  const table = props.table;
  const [filter, setFilter] = useState("");

  // Guard: blockData may be off, or an open table may arrive before its data is
  // populated. Fall back to the block HTML instead of crashing.
  if (!table) return <div dangerouslySetInnerHTML={{ __html: sanitize(props.html) }} />;

  // Keep only UI state in React and DERIVE the rows, so the table keeps
  // reflecting rows that stream in (the override runs on open blocks too).
  const rows = filter
    ? table.rows.filter((r) => r.some((c) => c.text.toLowerCase().includes(filter.toLowerCase())))
    : table.rows;

  return (
    <div>
      <input value={filter} onChange={(e) => setFilter(e.target.value)} aria-label="Filter rows" />
      <button onClick={() => navigator.clipboard.writeText(toCsv(table))}>Copy CSV</button>
      {props.open && <span className="live-dot" title="streaming" />}
      <table>{/* render headers/rows from table.headers / rows, aligns[i] for text-align */}</table>
    </div>
  );
});
```

The block is keyed by its stable parser id, so this component's state survives
every streaming patch.

---

## Interactive task checkboxes

GFM task-list checkboxes are emitted `disabled` (byte-exact GFM). Override the
`input` **tag** to make them live. That is the element contract — attributes and
`children`, **no `block` prop** — so map back to the source via the block you
render it in, using `block.start` / `block.end`.

```tsx
const COMPONENTS: Components = {
  input: (p: any) =>
    p.type === "checkbox"
      ? <input type="checkbox" checked={!!p.checked} onChange={onToggle} />
      : <input {...p} />,
};
```

With `a11y: true` the core wraps the checkbox and its text in a `<label>`, which
is what you want for screen readers.

---

## Lazy images and image proxying

```tsx
const COMPONENTS: Components = {
  img: (p: any) => <img {...p} loading="lazy" decoding="async" />,
};

// Proxy remote images. Output is re-sanitized, so it can't smuggle a scheme.
const URL_TRANSFORM: UrlTransform = (url, ctx) =>
  ctx.attr === "src" && /^https?:/i.test(url)
    ? `/img-proxy?u=${encodeURIComponent(url)}`
    : url;
```

`urlTransform` works standalone — it does not need a `components` map. The core
emits `<img src alt [title]>` and nothing else, so `loading`/`decoding`/sizing
are yours to add.

---

## Accessible chat

```tsx
<BrookMarkdown
  client={client}
  role="log"            // a running record of messages
  aria-live="polite"    // coalesced announcements, not per-token
  aria-atomic={false}
  streamConfig={{ a11y: true }}
/>
```

`role`, `aria-live`, `aria-atomic`, `id` and `className` all land on the
`.brook-md` root, on React and on the DOM mount (and therefore on the web
component and the Vue/Svelte/Solid bindings). `a11y: true` adds `<label>`-wrapped
task checkboxes and `scope="col"` on table headers. If you SSR and hydrate, the
root's a11y attributes must match on both sides or React reports a hydration
mismatch.

Focus management (restoring focus after a stream replaces content, "skip to end
of message") belongs to your app shell — brookmd does not move focus.

---

## Persist and reopen a thread instantly

Re-feeding a long thread through the parser on reopen is O(source) and blocks
the first paint. Persist the rendered snapshot instead: restoring it is pure
JSON — **no worker, no WASM, no parse**.

```ts
import { BrookClient, sourceFingerprint } from "brookmd/client";

// On close / navigate away:
const snap = client.getPersistable(sourceText);   // { hydrateVersion, blocks, sourceLength, sourceHash, done }
localStorage.setItem(`thread:${id}`, JSON.stringify(snap));

// On reopen — must be an untouched client:
const client = new BrookClient();
const snap = JSON.parse(localStorage.getItem(`thread:${id}`)!);
if (snap.sourceHash === sourceFingerprint(sourceText)) {
  client.hydrate(snap, { source: sourceText });   // { source } only needed to resume
}
```

- A `done: true` snapshot is terminal: appending to it throws.
- A `done: false` snapshot needs `{ source }` to resume; the first
  `append`/`finalize` transparently restarts the parser from it.
- `getPersistable()` throws if you constructed the client with
  `recovery: false` and fed it manual `append`s — pass the source explicitly.
- `sourceHash` is a staleness check (FNV-1a), deliberately not a security
  primitive.

---

## Measure parse time

```ts
const m = client.getMetrics();
// { bytes, patches, meanParseMicros, totalParseMs, throughputKBs,
//   committedBlocks, activeBlocks, lastPatchAgoMs, retainedBytes,
//   wasmMemoryBytes, renderCount, rebuildCount }

console.log(`${m.bytes} B in ${m.totalParseMs.toFixed(1)} ms `
          + `(${m.meanParseMicros.toFixed(0)} µs/patch, ${m.throughputKBs.toFixed(0)} kB/s)`);
```

Per-block render churn:

```tsx
<BrookMarkdown
  client={client}
  onRenderMetrics={(blockId, m) => {
    // Fires only on an ACTUAL render — a memo-skipped committed block never does.
    if (m.renderCount > 50) console.warn("hot block", blockId, m.kind, m.renderCount);
  }}
/>
```

A committed block that keeps re-rendering means an unstable prop identity
(`components` / `decorators` / `urlTransform` / `sanitize` / `onBlockError`).

Note that `wasmMemoryBytes` and worker-level counters are shared across the
streams multiplexed onto one worker, so don't sum them across clients.

---

## Table of contents

```ts
const entries = client.outline();  // [{ level, text, id }, …] — id is the block id
```

`text` is plaintext with tags stripped and entities decoded; `id` is the stable
block id, usable as a scroll target and a React key. With `blockData: true` a
`Heading` block also carries `{ level, text, id }` where `id` is a GitHub-style
anchor slug — note `kind.data` for headings is a **union**: an object with
`blockData` on, a bare `number` (the level) with it off.

`client.toPlaintext()` gives the whole document as plain text.

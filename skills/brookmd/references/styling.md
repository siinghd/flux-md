# brookmd — styling

brookmd emits semantic HTML under a `.brook-md` root and **ships no CSS by
default**. Either bring your own design system, or opt into the bundled theme:

```ts
import "brookmd/styles.css";
```

The theme is scoped to `.brook-md`, zero-runtime, and **does not change the
rendered HTML** — skip the import and nothing is styled (including the built-in
highlighter's colours: without CSS, highlighted code renders uncoloured).

Entries labelled **since 0.30.0** are new in that release.

---

## 1. CSS variables

Re-theme by overriding a handful of custom properties rather than rewriting
selectors:

```css
.brook-md {
  --brook-accent: #7c3aed;   /* links */
  --brook-bg-code: #faf7ff;  /* code background */
  --brook-t-kw: #c026d3;     /* syntax: keywords */
}
```

| Group | Variables |
|---|---|
| Surfaces / text | `--brook-fg`, `--brook-fg-muted`, `--brook-fg-faint`, `--brook-border`, `--brook-bg-code`, `--brook-bg-inline`, `--brook-bg-quote`, `--brook-accent` |
| Alerts | `--brook-alert-note`, `--brook-alert-tip`, `--brook-alert-important`, `--brook-alert-warning`, `--brook-alert-caution` |
| Syntax tokens | `--brook-t-kw`, `--brook-t-str`, `--brook-t-num`, `--brook-t-com`, `--brook-t-fn`, `--brook-t-ty`, `--brook-t-mac`, `--brook-t-attr`, `--brook-t-tag`, `--brook-t-var`, `--brook-t-pun` |
| Motion | `--brook-caret` (colour), `--brook-caret-anim`, `--brook-pill-anim` — both animations are set to `none` under `prefers-reduced-motion: reduce` |
| Sizing | `--brook-radius`, `--brook-gap` |

**Dark mode** is automatic via `prefers-color-scheme`. Force a mode with
`class="brook-md brook-dark"` or `class="brook-md brook-light"` — the
`prefers-color-scheme` block is written as `.brook-md:not(.brook-light)`, so
`brook-light` wins in a dark OS and `.brook-md.brook-dark` wins in a light one.

---

## 2. The block-state class contract

These classes are a **stable, documented styling contract** (**since 0.30.0** —
they were emitted before, but undocumented). React, the DOM renderer, and the
static server renderer all emit the same names.

| Class | Where | Meaning |
|---|---|---|
| `brook-md` | root `<div>` | always present; `className` is appended to it |
| `brook-block` | per-block wrapper | every block on the generic path |
| `brook-block-<kind>` | per-block wrapper | lowercased kind: `brook-block-paragraph`, `-heading`, `-codeblock`, `-mathblock`, `-mermaid`, `-list`, `-blockquote`, `-alert`, `-table`, `-rule`, `-html`, `-component` |
| `brook-open` | per-block wrapper | the block is still streaming; its HTML may still change |
| `brook-speculative` | per-block wrapper | closed speculatively, may yet be revised |
| `brook-streaming` | code / math / mermaid slot | that renderer's own "still open" marker |
| `brook-deferred` | root | present while `deferTail` is deferring |
| `brook-bottom-anchor` | sentinel `<div>` | emitted by `stickToBottom` (`scroll-snap-align: end`) |

Core-emitted markup you can also target: `code.language-X[data-lang="X"]`,
`div.math.math-display` / `span.math.math-inline`,
`div.markdown-alert.markdown-alert-<kind>` with `p.markdown-alert-title`,
`section.footnotes[role="doc-endnotes"]` with `sup.footnote-ref`, and
`a[data-brook-pending]` for a streaming link whose URL has not arrived yet.

An anchor with no `href` gets no default link styling, so without a rule the
link "pops" blue only when the URL completes. The bundled theme already styles
it; if you bring your own CSS, copy:

```css
.brook-md a[data-brook-pending] {
  color: var(--brook-accent, #0969da);
  cursor: default;
}
```

---

## 3. Code-block chrome

The built-in `CodeBlock` renderer emits its own header bar with a language
label, a Copy button (closed blocks) or a "streaming" pill (open blocks), and a
scrollable body:

```
div.brook-code-block[.brook-streaming]
  div.brook-code-header
    span.brook-code-lang
    span.brook-code-streaming-pill   (open)
    button.brook-code-copy           (closed)
  div.brook-code-body
    pre > code                       (highlighted)
```

`MathBlock` and `Mermaid` mirror it: `brook-math-block` / `-header` / `-lang` /
`-body` and `brook-mermaid-block` / `-header` / `-lang` / `-body`, both reusing
`brook-code-streaming-pill` while open.

**Since 0.30.0 `brookmd/styles.css` styles all of this chrome.** Before that the
theme left it unstyled, so `import "brookmd/styles.css"` produced a themed
document with a raw button stack above every fence. If you are on an older
version, or you skip the theme, style those classes yourself.

The code body is keyboard-scrollable and labelled (`tabIndex={0}`,
`role="region"`, `aria-label="<lang> code"`), and the copy button carries
`aria-label` + `aria-live="polite"` — preserve those if you replace the slot.

---

## 4. Streaming caret (opt-in, since 0.30.0)

Add the `brook-caret` class to the root and `brookmd/styles.css` draws a blinking
caret at the end of the open (streaming) block:

```tsx
<BrookMarkdown stream={stream} className="brook-caret" />
```

```ts
mountBrookMarkdown(client, el, { className: "brook-caret" });
```

It is opt-in so default output is unchanged, and it is pure CSS keyed off
`brook-open` — no JS, no extra DOM, nothing on the streaming path. The selectors
are anchored so exactly one element in an open block can match: the trailing
paragraph or heading, the deepest last list item, the last cell of a streaming
table, the tail of a blockquote.

- **Retint** with `--brook-caret` (default `currentColor`).
- **Retime** with `--brook-caret-anim`; `prefers-reduced-motion: reduce` sets it
  (and the "streaming" pill's pulse) to `none`.
- **Switch it off wholesale** with
  `.brook-md.brook-caret ::after { content: none }`.
- It is deliberately **not** drawn inside code / math / mermaid fences — those
  show the "streaming" pill instead.

---

## 5. Tailwind and design systems

Two mechanisms, both of which also apply to the **open/streaming** block:

**Root class.** `className` is appended to `brook-md`, which is always kept:

```tsx
<BrookMarkdown stream={stream} className="prose prose-slate max-w-none" />
```

**Element-path `components`.** Lowercase tag keys receive the element's
attributes and `children` — no `block` prop. Hoist the map:

```tsx
const COMPONENTS: Components = {
  p:  (p: any) => <p className="my-3 leading-7" {...p} />,
  ul: (p: any) => <ul className="my-3 list-disc pl-6" {...p} />,
  ol: (p: any) => <ol className="my-3 list-decimal pl-6" {...p} />,
  li: (p: any) => <li className="my-1" {...p} />,
  a:  (p: any) => <a className="text-sky-600 underline underline-offset-2" {...p} />,
  h1: (p: any) => <h1 className="mt-6 mb-3 text-2xl font-semibold" {...p} />,
  h2: (p: any) => <h2 className="mt-6 mb-3 text-xl font-semibold" {...p} />,
  h3: (p: any) => <h3 className="mt-5 mb-2 text-lg font-semibold" {...p} />,
  table: (p: any) => <table className="w-full border-collapse text-sm" {...p} />,
  code: (p: any) => <code className="rounded bg-slate-100 px-1 py-0.5" {...p} />,
  blockquote: (p: any) => <blockquote className="border-l-4 pl-4 italic" {...p} />,
};
```

`class` is renamed to `className` on the React path, so `{...p}` spreads cleanly.
The DOM renderer has no tag-level path — use a block-kind override there and
rewrite the `html` it is handed.

> **`@tailwindcss/typography` users: skip `brookmd/styles.css`.** `.brook-md` is
> a plain `<div>`, so `class="prose brook-md"` works — but the theme's
> `.brook-md > * { margin: 0 0 var(--brook-gap) }` reset fights `prose`'s own
> vertical rhythm. Import the theme or use `prose`, not both. If you skip the
> theme you must supply the highlighter token colours yourself (see §6).

**The identity footgun.** A fresh `components` / `decorators` / `urlTransform` /
`sanitize` object on each render busts the per-block memo, so every committed
block re-renders on every patch. Define them at module scope, or `useMemo` with
a stable dependency list. (`onLinkClick` is the exception — it is delegated, so
a fresh closure costs nothing.)

---

## 6. Highlighter token classes

The built-in highlighter emits `<span class="t-…">`. The bundled theme colours
them from the `--brook-t-*` variables; without any CSS, code renders uncoloured.

`t-kw` (keyword), `t-str` (string), `t-rx` (regex), `t-num` (number), `t-lt`
(literal), `t-com` (comment), `t-fn` (function), `t-ty` (type), `t-mac`
(macro), `t-dec` (decorator), `t-attr` (attribute), `t-sel` (selector), `t-tag`
(tag), `t-var` (variable), `t-pun` (punctuation), `t-txt` (text). Whitespace
(the `ws` token class) is emitted raw, with no span.

A language table may also use the pseudo-class `ident`, whose matches are
promoted to `kw` when the word is in that language's keyword set.

These are also the token classes `registerLanguage` definitions emit — see
[recipes.md](recipes.md#register-a-language-with-the-built-in-highlighter).

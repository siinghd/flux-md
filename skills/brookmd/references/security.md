# brookmd — security model

Sanitization happens in the Rust core, before any HTML crosses the worker
boundary. The defaults are safe for untrusted / model-generated markdown: raw
HTML is escaped, dangerous URL schemes are neutralized, and every link is
`rel`-hardened. Everything below is about the knobs that relax or extend that.

---

## 1. URL policy

Applied uniformly to links, URI autolinks, images, and sanitized URL attributes.
A blocked URL becomes `#`.

**Never allowed, non-overridable** — `allowSchemes` cannot re-enable these, the
same way allowlisting `<script>` cannot re-enable it:

- `javascript:`
- `vbscript:`
- `data:text/html`
- `data:text/javascript`
- the scriptable `data:` media types on the **href** path — `data:image/svg`,
  `data:application/xhtml`, `data:text/xml`, …

**Overridable-blocked** — currently just `file:`. Re-enable with bare scheme
names, no colon, matched case-insensitively:

```ts
new BrookClient({ config: { allowSchemes: ["file"] } });
```

Only do that in a host that intercepts link clicks instead of navigating (an
Electron or extension UI that opens the path in an editor). Local-resource
disclosure then becomes the embedder's responsibility.

**Not a general allowlist.** `allowSchemes` never *restricts* anything. Schemes
outside the built-in blocklist (`vscode:`, `ftp:`, `mailto:`, …) already render
and are unaffected.

**Images** keep their own allowlist: `http(s)`, `data:image/` (minus SVG, which
is never allowed), and relative paths.

**Every rendered link carries** `target="_blank" rel="noopener noreferrer nofollow"`
— including autolinks and the pending streaming anchor. This is a deliberate,
documented deviation from reference CommonMark output, folded by the
`canonicalize` step used for conformance comparison.

### `safeUrl` — the JS-side filter

```ts
import { safeUrl, wrapLink } from "brookmd";
```

`safeUrl(value)` decodes entities to a fixpoint (capped at 8 iterations), strips
control characters, and blocks the four script-executing prefixes, returning `#`
otherwise. Use it on any URL you build yourself in a `decorator` or a
`components` override. `wrapLink(text, attrs)` builds an `<a>` whose `href` is
already routed through it.

---

## 2. Raw HTML — four tiers

| Tier | Config | Behaviour |
|---|---|---|
| Default | — | Raw HTML is **escaped**. Safe for untrusted input. |
| Safe inline subset | `htmlAllowlist: [...]` or `dropHtmlTags: [...]` | Setting **either** (even to `[]`) engages the sanitizer for *inline* raw HTML. `htmlAllowlist: []` = allow every tag except a built-in dangerous set (`script`, `style`, `iframe`, `object`, `embed`, `form`, `input`, `svg`, …). A non-empty array renders only those tags and escapes the rest. Attributes are sanitized (event handlers dropped, dangerous URL schemes → `#`); HTML comments are dropped. Case-insensitive. |
| Safe block subset | `blockHtml: true` **plus** one of the above | Extends the sanitizer to CommonMark HTML block **types 6 and 7** — a known block-level tag alone on its line, so `<details><summary>…</summary>…</details>` renders as real elements. Types 1–5 stay escaped: type 1 is the raw-text family (`<script>`, `<pre>`, `<style>`, `<textarea>`), where a speculative mid-stream close is mXSS-prone; types 2–5 carry no renderable element. While the block streams, still-open elements get speculative closers, so what the reader has seen is a complete tree at every append. Markdown *inside* the HTML is not parsed. |
| Everything | `unsafeHtml: true` | Raw HTML passes through unescaped. **Never enable for untrusted input.** With it on, `gfmTagfilter: true` at least escapes GFM's nine disallowed tags. |

`dropHtmlTags` removes the markup entirely and leaves any text between an
open/close pair as inert text — useful for app marker tags, or as
belt-and-suspenders `["script","style"]`.

**Component tags are the safe alternative to `unsafeHtml`.** `componentTags:
["Thinking"]` / `inlineComponentTags: ["cite"]` allowlist named tags whose inner
content is parsed as **markdown** and whose attributes are sanitized — no
`unsafeHtml` needed. The renderer dispatches them through `components[tag]`.

---

## 3. The `sanitize` hook

For markdown from a model you do not control, and especially with `unsafeHtml`
on, pass a **hoisted** sanitizer. It runs before every `innerHTML` injection,
**including the open (streaming) tail** — so a half-arrived hostile tag is
sanitized on every patch, not just at commit.

```tsx
import DOMPurify from "dompurify";
import { BrookMarkdown } from "brookmd/react";

// Module scope: a fresh identity each render busts the per-block memo.
const sanitize = (html: string) => DOMPurify.sanitize(html);

<BrookMarkdown
  stream={stream}
  streamConfig={{ unsafeHtml: true }}
  sanitize={sanitize}
/>;
```

The same option exists on `MountOptions` (`brookmd/dom`), as the
`<brook-markdown>` `.sanitize` property, and on the Vue / Svelte / Solid
bindings.

**What `sanitize` does NOT cover:** the built-in `CodeBlock` / `MathBlock` /
`Mermaid` renderers operate on already-escaped content and deliberately bypass
it. If you replace those slots and inject HTML yourself, sanitize it yourself.

---

## 4. Trusted surfaces — read before writing an override

Two extension points are **trusted and un-sanitized**:

- **`decorators`.** A decorator's `replace` output is spliced straight into the
  render tree; it does not pass through brookmd's attribute sanitizer (that only
  runs on attributes the trusted core emitted). React and the DOM both happily
  render a `javascript:` href. Build only trusted nodes, and route any href
  through `safeUrl` or `wrapLink`.
- **`components`.** Same rule. Anything you `dangerouslySetInnerHTML` inside an
  override is on you.

**`urlTransform` is the exception — its output IS re-sanitized**
(`safeUrl(transform(safeUrl(value)))`), so a buggy or hostile transform can
never emit a `javascript:` / `data:text/html` URL that reaches the DOM.

---

## 5. `onLinkClick` — interstitials and in-app routing (since 0.30.0)

Model-generated URLs are attacker-influenced content. `onLinkClick` gives you a
single delegated hook to gate navigation, without a `components.a` override:

```tsx
const ALLOWED = /^https:\/\/(docs\.)?example\.com\//;

<BrookMarkdown
  stream={stream}
  onLinkClick={(event, link) => {
    if (!ALLOWED.test(link.href)) {
      event.preventDefault();      // cancels navigation
      openInterstitial(link.href); // "You are leaving…"
    }
  }}
/>;
```

`link` is `{ href, text, element }`. Exactly ONE listener sits on the
`.brook-md` root and resolves the anchor from the event target — no per-anchor
prop, no per-block cost, and the streaming path is untouched. A still-streaming
anchor (`<a data-brook-pending>`, label rendered, URL not yet arrived) is never
reported, because there is no `href` to hand you yet.

The same option exists on `MountOptions`, as the `<brook-markdown>`
`.onLinkClick` property, and on the Vue / Svelte / Solid bindings (there the
event is a native `MouseEvent`; in React it is the synthetic event).

`onLinkClick` is not a security boundary on its own — `rel="noopener
noreferrer nofollow"` and the scheme blocklist still do the hard work. It is the
place to add product policy on top.

---

## 6. Checklist for untrusted / model output

1. Leave `unsafeHtml` **off**. If the model must emit HTML, prefer
   `htmlAllowlist` (+ `blockHtml`) over `unsafeHtml`.
2. Pass a hoisted `sanitize` when you do relax raw HTML.
3. Use `componentTags` / `inlineComponentTags` instead of `unsafeHtml` for
   structured model output.
4. Never build a URL in a `decorator` or override without `safeUrl` / `wrapLink`.
5. Use `urlTransform` for image proxying — its output is re-sanitized.
6. Gate navigation with `onLinkClick` if your product needs an interstitial.
7. Leave `allowSchemes` empty unless you own the click handling.
8. Wire `onBlockError` so a throwing override is reported rather than silently
   swallowed by the per-block error boundary.

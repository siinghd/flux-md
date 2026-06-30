import { createElement, Fragment, type ReactElement, type ReactNode } from "react";
import type { Components } from "./types";
import type { Decorator, UrlTransform } from "./types-core";
import { decorateSegments } from "./decorate";
import { decodeEntities, safeUrl } from "./url-safety";

// `decodeEntities` + `safeUrl` now live in the framework-neutral ./url-safety so
// the DOM renderer can share the EXACT scheme filter; re-exported here for the
// existing public/test import sites. `safeUrl` is part of the public surface so
// users of `decorators` can build XSS-safe links (React does not block
// `javascript:` hrefs — decorator output is a TRUSTED, un-sanitized surface).
export { decodeEntities, safeUrl };

// HTML void elements: no closing tag, never have children.
const VOID = new Set([
  "area", "base", "br", "col", "embed", "hr", "img", "input",
  "link", "meta", "param", "source", "track", "wbr",
]);

// Attribute name → React prop name, for the handful that differ. Anything not
// listed passes through verbatim (React forwards data-*/aria-* and lowercase
// attributes unchanged).
// Prototype-free map so an attribute named `constructor`/`hasOwnProperty`/etc.
// returns undefined (and the `?? name` fallback fires) rather than resolving to
// an inherited Object.prototype member.
const ATTR_MAP: Record<string, string> = Object.assign(Object.create(null), {
  class: "className",
  for: "htmlFor",
  colspan: "colSpan",
  rowspan: "rowSpan",
  tabindex: "tabIndex",
  maxlength: "maxLength",
  minlength: "minLength",
  readonly: "readOnly",
  autocomplete: "autoComplete",
  autofocus: "autoFocus",
  spellcheck: "spellCheck",
  contenteditable: "contentEditable",
  crossorigin: "crossOrigin",
  enterkeyhint: "enterKeyHint",
  inputmode: "inputMode",
});

// URL-bearing attributes whose value must be scheme-checked. `htmlToReact` is
// exported and may be handed untrusted HTML directly; React happily renders a
// `javascript:` href (it only warns), so we neutralize it here as
// defense-in-depth — the core's own output is already sanitized.
const URL_ATTRS = new Set(["href", "src", "xlink:href", "formaction", "action", "poster", "data"]);

// React-meaningful prop names that must never be forwarded from (possibly
// untrusted) HTML attributes: `dangerouslySetInnerHTML` as a prop crashes the
// whole render tree (DoS), and ref/key/defaultValue/etc. are injectable.
const PROP_DENY = new Set([
  "dangerouslysetinnerhtml", "ref", "key", "defaultvalue", "defaultchecked",
  "suppresshydrationwarning", "suppresscontenteditablewarning",
]);

// Only forward attribute names that are a plain HTML attribute identifier
// (so camelCase / `__proto__` / `constructor` never reach React props). The
// explicit ATTR_MAP renames and `xlink:href` are allowed past this gate.
const SAFE_ATTR_NAME = /^[a-z][a-z0-9-]*$/i;

type HNode =
  | { kind: "text"; text: string }
  | { kind: "el"; tag: string; attrs: Record<string, string | true>; children: HNode[] };

/**
 * Parse an inline CSS string (`"text-align:left;color:red"`) into the object
 * React's `style` prop requires, camelCasing property names. Custom properties
 * (`--x`) keep their literal name.
 */
export function parseStyle(css: string): Record<string, string> {
  const out: Record<string, string> = {};
  for (const decl of css.split(";")) {
    const c = decl.indexOf(":");
    if (c === -1) continue;
    const rawName = decl.slice(0, c).trim();
    const value = decl.slice(c + 1).trim();
    if (!rawName || !value) continue;
    const name = rawName.startsWith("--")
      ? rawName
      : rawName.toLowerCase().replace(/-([a-z])/g, (_, ch: string) => ch.toUpperCase());
    out[name] = value;
  }
  return out;
}

// CSS values that beacon/exfiltrate (`url(`), execute (legacy `expression(`,
// `-moz-binding`, `behavior:`), or pull external resources (`@import`,
// `image-set(`). Defense-in-depth: the core sanitizer already drops `style`, but
// `htmlToReact` is exported and may be handed untrusted HTML directly.
const DANGEROUS_CSS_VALUE = /url\(|expression\(|image-set\(|-moz-binding|@import|behavior\s*:/i;

/** Strip CSS declarations that can beacon/exfiltrate, execute, or overlay the
 *  viewport (`position: fixed/sticky` → clickjacking). Safe declarations
 *  (`text-align`, `color`, …) — including flux's own table-alignment style —
 *  pass through untouched. */
function safeStyle(style: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const k in style) {
    const v = style[k];
    if (DANGEROUS_CSS_VALUE.test(v)) continue;
    if (k.toLowerCase() === "position" && /\b(?:fixed|sticky)\b/i.test(v)) continue;
    out[k] = v;
  }
  return out;
}

// Single-char classifiers for parseOpenTag, hoisted to module scope so the
// scan loops don't re-evaluate a fresh regex literal per character on every
// block render (parseTrustedHtml runs this for every open streaming block).
const TAG_CHAR = /[a-zA-Z0-9-]/;
const WS_CHAR = /\s/;
const ATTR_NAME_END = /[\s=>/]/;
const UNQUOTED_VALUE_END = /[\s>]/;

/** Parse one opening tag starting at `start` (the `<`). */
function parseOpenTag(html: string, start: number) {
  let i = start + 1;
  let j = i;
  while (j < html.length && TAG_CHAR.test(html[j])) j++;
  // Preserve the tag's ORIGINAL case so an inline custom-component element (e.g.
  // `<Cite>`) dispatches to `components.Cite`. Standard elements the core emits
  // are already lowercase; the semantic checks below (VOID, `input`, close-tag
  // matching) lowercase as needed, so HTML behavior is unchanged.
  const tag = html.slice(i, j);
  i = j;
  const attrs: Record<string, string | true> = {};
  while (i < html.length) {
    const loopStart = i;
    while (i < html.length && WS_CHAR.test(html[i])) i++;
    if (html[i] === ">") return { tag, attrs, selfClose: false, next: i + 1 };
    if (html[i] === "/" && html[i + 1] === ">") return { tag, attrs, selfClose: true, next: i + 2 };
    if (i >= html.length) break;
    let k = i;
    while (k < html.length && !ATTR_NAME_END.test(html[k])) k++;
    const name = html.slice(i, k);
    i = k;
    while (i < html.length && WS_CHAR.test(html[i])) i++;
    if (html[i] === "=") {
      i++;
      while (i < html.length && WS_CHAR.test(html[i])) i++;
      let value = "";
      const q = html[i];
      if (q === '"' || q === "'") {
        i++;
        const e = html.indexOf(q, i);
        value = html.slice(i, e === -1 ? html.length : e);
        i = e === -1 ? html.length : e + 1;
      } else {
        let v = i;
        while (v < html.length && !UNQUOTED_VALUE_END.test(html[v])) v++;
        value = html.slice(i, v);
        i = v;
      }
      if (name) attrs[name] = decodeEntities(value);
    } else if (name) {
      attrs[name] = true; // boolean attribute (e.g. checked, disabled)
    }
    // Guarantee forward progress on malformed input (e.g. a stray '/').
    if (i <= loopStart) i = loopStart + 1;
  }
  return { tag, attrs, selfClose: false, next: i };
}

/**
 * Tokenize **trusted** HTML (the kind flux-md's core emits: well-formed,
 * entity-escaped, allowlisted) into a small node tree. This is deliberately not
 * a defensive HTML5 parser — the threat model is "our own serializer output",
 * not hostile markup. Unrecognized constructs (comments, doctype) are skipped.
 */
// Instrumentation: how many times the tokenizer has run. Used by tests to
// prove open blocks and the no-override fast path never reach the parser, and
// that memoized closed blocks parse exactly once. Negligible cost in prod.
let parseCount = 0;
export function getParseCount(): number {
  return parseCount;
}
export function resetParseCount(): void {
  parseCount = 0;
}

export function parseTrustedHtml(html: string): HNode[] {
  parseCount++;
  const root: HNode[] = [];
  const stack: Array<Extract<HNode, { kind: "el" }>> = [];
  let i = 0;
  const push = (n: HNode) => {
    if (stack.length) stack[stack.length - 1].children.push(n);
    else root.push(n);
  };
  while (i < html.length) {
    const lt = html.indexOf("<", i);
    if (lt === -1) {
      const t = html.slice(i);
      if (t) push({ kind: "text", text: decodeEntities(t) });
      break;
    }
    if (lt > i) push({ kind: "text", text: decodeEntities(html.slice(i, lt)) });

    if (html.startsWith("<!--", lt)) {
      const end = html.indexOf("-->", lt + 4);
      i = end === -1 ? html.length : end + 3;
      continue;
    }
    if (html[lt + 1] === "!") {
      const end = html.indexOf(">", lt);
      i = end === -1 ? html.length : end + 1;
      continue;
    }
    if (html[lt + 1] === "/") {
      const end = html.indexOf(">", lt);
      const closeLower = html.slice(lt + 2, end === -1 ? html.length : end).trim().toLowerCase();
      for (let s = stack.length - 1; s >= 0; s--) {
        if (stack[s].tag.toLowerCase() === closeLower) {
          stack.length = s;
          break;
        }
      }
      i = end === -1 ? html.length : end + 1;
      continue;
    }
    // An opening tag must start with an ASCII letter. Anything else (a stray
    // '<', as in "3 < 4") is literal text. (Real core output escapes '<' to
    // &lt;, so this only matters for hand-fed input — but it must not hang.)
    const c1 = html[lt + 1];
    const isName = (c1 >= "a" && c1 <= "z") || (c1 >= "A" && c1 <= "Z");
    if (!isName) {
      push({ kind: "text", text: "<" });
      i = lt + 1;
      continue;
    }
    const { tag, attrs, selfClose, next } = parseOpenTag(html, lt);
    const el: Extract<HNode, { kind: "el" }> = { kind: "el", tag, attrs, children: [] };
    push(el);
    if (!selfClose && !VOID.has(tag.toLowerCase())) stack.push(el);
    i = next;
  }
  return root;
}

// Threaded through the walk so the text branch can decorate and the URL sites
// can apply an opt-in `urlTransform`. Both default off (identity-unchanged).
interface WalkCtx {
  decorators?: Decorator[];
  urlTransform?: UrlTransform;
}

function attrsToProps(
  tag: string,
  attrs: Record<string, string | true>,
  key: string,
  urlTransform?: UrlTransform,
): Record<string, unknown> {
  const props: Record<string, unknown> = { key };
  for (const name in attrs) {
    const value = attrs[name];
    const lower = name.toLowerCase();
    // Defense-in-depth: never forward inline event handlers, even though
    // React drops most lowercase `on*` attrs — this also covers casings and
    // future React behavior.
    if (lower.startsWith("on")) continue;
    // Reject React-meaningful names that would crash the render tree or inject
    // internals (dangerouslySetInnerHTML, ref, key, defaultValue, …).
    if (PROP_DENY.has(lower)) continue;
    if (lower === "style" && typeof value === "string") {
      props.style = safeStyle(parseStyle(value));
      continue;
    }
    // Neutralize dangerous-scheme URLs (javascript:, vbscript:, data:text/html).
    // An opt-in `urlTransform` may rewrite href/src/poster — its OUTPUT is
    // re-sanitized (`safeUrl(urlTransform(safeUrl(value)))`) so a buggy/hostile
    // transform can't reintroduce a dangerous scheme that reaches the DOM.
    if (URL_ATTRS.has(lower) && typeof value === "string") {
      let url = safeUrl(value);
      if (urlTransform && (lower === "href" || lower === "src" || lower === "poster")) {
        url = safeUrl(urlTransform(url, { tag: tag.toLowerCase(), attr: lower }));
      }
      props[ATTR_MAP[lower] ?? name] = url;
      continue;
    }
    // A static checkbox carries `checked` with no handler; render it
    // uncontrolled so React doesn't warn about a missing onChange.
    if (tag.toLowerCase() === "input" && lower === "checked") {
      props.defaultChecked = value === true ? true : value;
      continue;
    }
    // Restrict forwarded ORIGINAL names to a plain HTML attribute identifier
    // (plus the ATTR_MAP renames and xlink:href handled above) so weird casings
    // / `__proto__` / `constructor` can never become a React prop.
    if (!(lower in ATTR_MAP) && !SAFE_ATTR_NAME.test(name)) continue;
    props[ATTR_MAP[lower] ?? name] = value;
  }
  return props;
}

// Split one trusted (already escaped) text node into the decorator output and
// push it onto `out`. The matcher feeds the ORIGINAL text string (never a prior
// decorator's replacement), so there is no `<mark><mark>` double-wrap; each
// match is wrapped in a keyed Fragment so React reconciles the mixed
// string/element children without a missing-key warning. `replace` is treated as
// pure, so a streamed-in node decorates identically to a one-shot render.
function pushDecoratedText(
  out: ReactNode[],
  text: string,
  decorators: Decorator[],
  ancestors: string[],
  keyBase: string,
): void {
  const segs = decorateSegments(text, decorators, ancestors);
  if (segs === null) {
    out.push(text); // nothing matched — original text node, byte-identical
    return;
  }
  for (let i = 0; i < segs.length; i++) {
    const s = segs[i];
    if (s.type === "text") {
      out.push(s.text);
      continue;
    }
    const replacement = s.decorator.replace(s.matchText, s.groups) as ReactNode;
    out.push(createElement(Fragment, { key: keyBase + ":" + i }, replacement));
  }
}

function nodesToReact(
  nodes: HNode[],
  components: Components,
  keyPrefix: string,
  ctx: WalkCtx | undefined,
  ancestors: string[],
): ReactNode {
  const out: ReactNode[] = [];
  for (let idx = 0; idx < nodes.length; idx++) {
    const n = nodes[idx];
    if (n.kind === "text") {
      if (ctx && ctx.decorators) {
        pushDecoratedText(out, n.text, ctx.decorators, ancestors, keyPrefix + idx);
      } else {
        out.push(n.text);
      }
      continue;
    }
    const key = keyPrefix + idx;
    const type = components[n.tag] ?? n.tag;
    const props = attrsToProps(n.tag, n.attrs, key, ctx?.urlTransform);
    if (VOID.has(n.tag.toLowerCase())) {
      out.push(createElement(type, props));
    } else {
      // Track the enclosing tag chain (mutated push/pop, no per-node alloc) so a
      // decorator's `skipInside` (default a/code/pre/kbd) can be honored.
      ancestors.push(n.tag);
      out.push(createElement(type, props, nodesToReact(n.children, components, key + ".", ctx, ancestors)));
      ancestors.pop();
    }
  }
  // `null` (not an empty array) for no children, so a self-closing / empty inline
  // component's `children` is nullish and a `{children ?? fallback}` override fires.
  return out.length === 0 ? null : out.length === 1 ? out[0] : out;
}

/**
 * Split trusted HTML into its TOP-LEVEL node segments (each segment is the
 * exact substring spanning one root node — an element with its full subtree, or
 * a run of text between elements). Used only by the opt-in child-memo path so an
 * OPEN block whose leading children are unchanged can reuse their already-built
 * React nodes instead of re-parsing the whole string every tick. Mirrors
 * `parseTrustedHtml`'s tokenizer for boundary detection, but records spans only.
 */
function topLevelSegments(html: string): string[] {
  const segs: string[] = [];
  let depth = 0; // open-element nesting depth
  let segStart = 0; // start of the current top-level segment
  let i = 0;
  while (i < html.length) {
    const lt = html.indexOf("<", i);
    if (lt === -1) break;
    if (html.startsWith("<!--", lt)) {
      const end = html.indexOf("-->", lt + 4);
      i = end === -1 ? html.length : end + 3;
      continue;
    }
    if (html[lt + 1] === "!") {
      const end = html.indexOf(">", lt);
      i = end === -1 ? html.length : end + 1;
      continue;
    }
    if (html[lt + 1] === "/") {
      const end = html.indexOf(">", lt);
      if (depth > 0) {
        depth--;
        if (depth === 0) {
          // Closing the outermost element ends one top-level segment.
          const stop = end === -1 ? html.length : end + 1;
          segs.push(html.slice(segStart, stop));
          segStart = stop;
        }
      }
      i = end === -1 ? html.length : end + 1;
      continue;
    }
    const c1 = html[lt + 1];
    const isName = (c1 >= "a" && c1 <= "z") || (c1 >= "A" && c1 <= "Z");
    if (!isName) {
      // A stray '<' is literal text — not a boundary.
      i = lt + 1;
      continue;
    }
    const { tag, selfClose, next } = parseOpenTag(html, lt);
    if (depth === 0) {
      // Any text before this element is its own top-level segment.
      if (lt > segStart) {
        segs.push(html.slice(segStart, lt));
        segStart = lt;
      }
    }
    const isVoid = selfClose || VOID.has(tag.toLowerCase());
    if (isVoid) {
      if (depth === 0) {
        segs.push(html.slice(segStart, next));
        segStart = next;
      }
    } else {
      depth++;
    }
    i = next;
  }
  // Trailing text (or an unterminated subtree) after the last boundary.
  if (segStart < html.length) segs.push(html.slice(segStart));
  return segs;
}

/**
 * Convert a block's trusted HTML string into a React node tree, replacing any
 * element whose tag name appears in `components`.
 *
 * With no `childMemoMap`, behavior is byte-identical to a single
 * `parseTrustedHtml` + convert pass — the closed-block call site memoizes on
 * `(html, components)`.
 *
 * Passing a `childMemoMap` opts into OPEN-block child reuse: the html is split
 * into top-level node segments and each is keyed by its exact substring. On a
 * hit the cached React node is reused (no re-parse, no re-serialize); only new /
 * changed trailing segments are parsed. The caller owns the map's lifetime and
 * must scope it per block.id and invalidate it when `components` changes (a hit
 * carries the React node built under the previous components map). Segment keys
 * carry their original document order via `keyOffset` so React keys stay stable.
 */
export function htmlToReact(
  html: string,
  components: Components,
  childMemoMap?: Map<string, ReactNode>,
  opts?: { decorators?: Decorator[]; urlTransform?: UrlTransform },
): ReactNode {
  // Build the walk context once (off when neither transform is supplied → the
  // text/URL branches behave byte-identically to before).
  const ctx: WalkCtx | undefined =
    opts && (opts.decorators || opts.urlTransform)
      ? { decorators: opts.decorators, urlTransform: opts.urlTransform }
      : undefined;
  if (!childMemoMap) return nodesToReact(parseTrustedHtml(html), components, "", ctx, []);
  const segs = topLevelSegments(html);
  const out: ReactNode[] = [];
  for (let idx = 0; idx < segs.length; idx++) {
    const seg = segs[idx];
    // Key the cache by index + segment text so identical segments at different
    // positions get distinct React keys (no collision) and a leading segment
    // stays a hit only while it keeps the same document index (append-only
    // growth — the common open-block case — preserves both).
    const cacheKey = idx + " " + seg;
    const hit = childMemoMap.get(cacheKey);
    if (hit !== undefined) {
      out.push(hit);
      continue;
    }
    // Each top-level segment starts a fresh ancestor chain (its own root nodes).
    const node = nodesToReact(parseTrustedHtml(seg), components, idx + ".", ctx, []);
    childMemoMap.set(cacheKey, node);
    out.push(node);
  }
  return out.length === 0 ? null : out.length === 1 ? out[0] : out;
}

/**
 * Build a SAFE `<a>` for use inside a {@link Decorator}'s `replace`. Decorator
 * output is a TRUSTED surface that does NOT pass through flux's attribute
 * sanitizer, and React renders a `javascript:` href without complaint — so this
 * runs `href` through {@link safeUrl} (the same scheme filter the core uses) and
 * spreads the remaining attributes verbatim. Prefer this over a hand-built
 * `<a>` whenever the href can come from model output.
 *
 * ```tsx
 * const decorators = [{
 *   match: /\$[\d.]+[BMK]/g,
 *   replace: (t) => wrapLink(t, { href: "/figures/" + t, className: "fig" }),
 * }];
 * ```
 */
export function wrapLink(
  text: ReactNode,
  attrs: { href: string } & Record<string, unknown>,
): ReactElement {
  const { href, ...rest } = attrs;
  return createElement("a", { ...rest, href: safeUrl(String(href)) }, text);
}

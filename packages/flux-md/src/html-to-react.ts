import { createElement, type ReactNode } from "react";
import type { Components } from "./types";

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

/** Replace a dangerous-scheme URL with "#". Mirrors the Rust `is_dangerous_scheme`:
 *  strip control chars (C0, DEL, C1 — matching Rust char::is_control),
 *  lowercase, then match. The strip affects only the probe, never output. */
function safeUrl(value: string): string {
  // Decode-STABLE probe: a value can be entity-decoded more than once before it
  // reaches the DOM, so peel layers to a fixpoint before the scheme check —
  // catches `javascript&#58;` and double-encoded `javascript&amp;#58;`. Only the
  // probe is decoded; the returned value is untouched (safe URLs stay verbatim).
  // Cap at 8 iterations: far beyond any legit URL (browsers entity-decode an
  // href once), and bounds the loop so a hostile value can't make it quadratic.
  let decoded = value;
  for (let i = 0, prev = ""; i < 8 && decoded !== prev; i++) {
    prev = decoded;
    decoded = decodeEntities(decoded);
  }
  // eslint-disable-next-line no-control-regex
  const probe = decoded.replace(/[\u0000-\u001f\u007f-\u009f]/g, "").replace(/^\s+/, "").toLowerCase();
  if (
    probe.startsWith("javascript:") ||
    probe.startsWith("vbscript:") ||
    probe.startsWith("data:text/html") ||
    probe.startsWith("data:text/javascript")
  ) {
    return "#";
  }
  return value;
}

type HNode =
  | { kind: "text"; text: string }
  | { kind: "el"; tag: string; attrs: Record<string, string | true>; children: HNode[] };

const NAMED_ENTITIES: Record<string, string> = {
  amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " ",
  copy: "©", reg: "®", hellip: "…", mdash: "—", ndash: "–",
};

/** Decode the (small, known) set of entities the core emits, plus numeric refs. */
export function decodeEntities(s: string): string {
  if (s.indexOf("&") === -1) return s;
  return s.replace(/&(#x[0-9a-fA-F]+|#\d+|[a-zA-Z][a-zA-Z0-9]*);/g, (m, body: string) => {
    if (body[0] === "#") {
      const code = body[1] === "x" || body[1] === "X"
        ? parseInt(body.slice(2), 16)
        : parseInt(body.slice(1), 10);
      if (Number.isNaN(code) || code < 0 || code > 0x10ffff) return m;
      try {
        return String.fromCodePoint(code);
      } catch {
        return m;
      }
    }
    const named = NAMED_ENTITIES[body];
    return named === undefined ? m : named;
  });
}

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

/** Parse one opening tag starting at `start` (the `<`). */
function parseOpenTag(html: string, start: number) {
  let i = start + 1;
  let j = i;
  while (j < html.length && /[a-zA-Z0-9-]/.test(html[j])) j++;
  // Preserve the tag's ORIGINAL case so an inline custom-component element (e.g.
  // `<Cite>`) dispatches to `components.Cite`. Standard elements the core emits
  // are already lowercase; the semantic checks below (VOID, `input`, close-tag
  // matching) lowercase as needed, so HTML behavior is unchanged.
  const tag = html.slice(i, j);
  i = j;
  const attrs: Record<string, string | true> = {};
  while (i < html.length) {
    const loopStart = i;
    while (i < html.length && /\s/.test(html[i])) i++;
    if (html[i] === ">") return { tag, attrs, selfClose: false, next: i + 1 };
    if (html[i] === "/" && html[i + 1] === ">") return { tag, attrs, selfClose: true, next: i + 2 };
    if (i >= html.length) break;
    let k = i;
    while (k < html.length && !/[\s=>/]/.test(html[k])) k++;
    const name = html.slice(i, k);
    i = k;
    while (i < html.length && /\s/.test(html[i])) i++;
    if (html[i] === "=") {
      i++;
      while (i < html.length && /\s/.test(html[i])) i++;
      let value = "";
      const q = html[i];
      if (q === '"' || q === "'") {
        i++;
        const e = html.indexOf(q, i);
        value = html.slice(i, e === -1 ? html.length : e);
        i = e === -1 ? html.length : e + 1;
      } else {
        let v = i;
        while (v < html.length && !/[\s>]/.test(html[v])) v++;
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

function attrsToProps(tag: string, attrs: Record<string, string | true>, key: string): Record<string, unknown> {
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
    if (URL_ATTRS.has(lower) && typeof value === "string") {
      props[ATTR_MAP[lower] ?? name] = safeUrl(value);
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

function nodesToReact(nodes: HNode[], components: Components, keyPrefix: string): ReactNode {
  const out: ReactNode[] = [];
  for (let idx = 0; idx < nodes.length; idx++) {
    const n = nodes[idx];
    if (n.kind === "text") {
      out.push(n.text);
      continue;
    }
    const key = keyPrefix + idx;
    const type = components[n.tag] ?? n.tag;
    const props = attrsToProps(n.tag, n.attrs, key);
    if (VOID.has(n.tag.toLowerCase())) {
      out.push(createElement(type, props));
    } else {
      out.push(createElement(type, props, nodesToReact(n.children, components, key + ".")));
    }
  }
  // `null` (not an empty array) for no children, so a self-closing / empty inline
  // component's `children` is nullish and a `{children ?? fallback}` override fires.
  return out.length === 0 ? null : out.length === 1 ? out[0] : out;
}

/**
 * Convert a block's trusted HTML string into a React node tree, replacing any
 * element whose tag name appears in `components`. Call this only for **closed**
 * blocks (open/streaming blocks have partial HTML); memoize on `(html,
 * components)` at the call site.
 */
export function htmlToReact(html: string, components: Components): ReactNode {
  return nodesToReact(parseTrustedHtml(html), components, "");
}

import type {
  Block,
  BlockComponentProps,
  CodeBlockData,
  HeadingData,
  ListData,
  MathBlockData,
  TableData,
} from "./types-core";

// Pure helpers duplicated from the JSX renderer / its CodeBlock so the
// framework-neutral DOM renderer carries no framework dependency. The JSX
// renderer is held byte-identical, so these are copies — match it exactly.

/** Decode the small entity set the core emits (amp last so `&amp;lt;` → `&lt;`).
 *  This is the simple ordered chain, not the numeric/named-entity decoder. */
function decodeEntities(s: string): string {
  return s
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, "&");
}

/** Decoded source text inside `<pre><code>…</code></pre>`. */
function decodeCodeText(html: string): string {
  const m = html.match(/<pre><code[^>]*>([\s\S]*?)<\/code><\/pre>/);
  return m ? decodeEntities(m[1]) : "";
}

/**
 * The LaTeX source for a MathBlock. Display math (`$$…$$` / `\[…\]`) renders as
 * `<div class="math math-display">…</div>`; a fenced ```math block renders as
 * `<pre><code>…</code></pre>`. Either way the body is the HTML-escaped LaTeX —
 * decode it back so a `components.MathBlock` override gets the raw source.
 */
function decodeMathText(html: string): string {
  const d = html.match(/<div class="math math-display">([\s\S]*?)<\/div>/);
  if (d) return decodeEntities(d[1]);
  return decodeCodeText(html);
}

/** Info-string language from a code block's `data-lang="…"`. */
export function extractLang(html: string): string {
  const m = html.match(/data-lang="([^"]+)"/);
  return m ? m[1] : "";
}

/** Strip the `<tag …>` open and trailing `</tag>` from a component block's HTML,
 *  leaving the inner (already-rendered markdown) HTML. Handles open (unclosed)
 *  blocks, where there is no close tag yet. */
function componentInnerHtml(html: string, tag: string): string {
  const gt = html.indexOf(">");
  if (gt < 0) return "";
  let inner = html.slice(gt + 1);
  const close = `</${tag}>`;
  if (inner.endsWith(close)) inner = inner.slice(0, -close.length);
  return inner.replace(/^\n/, "").replace(/\n$/, "");
}

/**
 * Convert sanitized HTML attribute pairs into a spreadable object, keeping the
 * HTML-form names (`class`, `for`) verbatim. This is the deliberate divergence
 * from the JSX renderer (which renames to `className`/`htmlFor` for a prop
 * spread): the DOM renderer applies them via `el.setAttribute(name, value)`,
 * which wants the literal HTML names.
 */
export function htmlAttrs(pairs: [string, string][]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of pairs) out[k] = v;
  return out;
}

/**
 * Build the props a block-kind / component-tag override receives — the same
 * shape the JSX renderer's block-kind props carry, with ONE deliberate
 * divergence: for `Component` blocks `attrs` stay in HTML form (`class`/`for`)
 * because DOM overrides apply them via `setAttribute` (see {@link htmlAttrs}).
 */
export function blockProps(block: Block): BlockComponentProps {
  const props: BlockComponentProps = {
    block,
    html: block.html,
    open: block.open,
    speculative: block.speculative,
  };
  const data = block.kind.data as
    | { lang?: string | null; code?: string; latex?: string; start?: number; ordered?: boolean; items?: { html: string }[]; tag?: string; attrs?: [string, string][] }
    | undefined;
  if (block.kind.type === "CodeBlock") {
    // Prefer the structured `code` (present when blockData is on) over the HTML
    // regex — it is the lossless decoded source. Fall back to the regex when off.
    props.text = data?.code ?? decodeCodeText(block.html);
    props.language = data?.lang ?? "";
    // Surface the typed convenience field only when the opt-in `code` is present.
    if (typeof data?.code === "string") {
      props.code = { lang: data.lang ?? null, code: data.code } as CodeBlockData;
    }
  } else if (block.kind.type === "MathBlock") {
    // Prefer the structured `latex` (present when blockData is on) over the regex.
    props.text = data?.latex ?? decodeMathText(block.html);
    if (typeof data?.latex === "string") {
      props.math = { latex: data.latex } as MathBlockData;
    }
  } else if (block.kind.type === "List") {
    // Structured list data is present only when blockData is on (the `start` key
    // rides the opt-in channel); surface it as the typed convenience field,
    // including the opt-in per-item HTML (`items`) for a keyed renderer.
    if (data && typeof data.start === "number") {
      props.list = { ordered: !!data.ordered, start: data.start, items: data.items } as ListData;
    }
  } else if (block.kind.type === "Component") {
    props.tag = data?.tag ?? "";
    props.attrs = htmlAttrs(data?.attrs ?? []);
    // An override replaces the `<tag>` wrapper, so it gets the *inner* HTML
    // (markdown already rendered) rather than the full wrapped block.
    props.html = componentInnerHtml(block.html, props.tag);
  } else if (block.kind.type === "Table") {
    // Structured data is present only when `blockData` is on (else `undefined`).
    // Pure data — identical for the DOM and JSX renderers, no name-form divergence.
    props.table = block.kind.data as TableData | undefined;
  } else if (block.kind.type === "Heading") {
    // When `blockData` is on, `kind.data` is the `{ level, text, id }` object;
    // when off it is the bare level `number`. Surface the rich object only — the
    // `typeof === "object"` guard keeps the off-path naked int out of `heading`.
    if (typeof block.kind.data === "object" && block.kind.data !== null) {
      props.heading = block.kind.data as HeadingData;
    }
  }
  return props;
}

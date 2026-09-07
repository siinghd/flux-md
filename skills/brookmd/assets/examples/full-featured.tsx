/**
 * Everything at once: hoisted overrides for both prop contracts, a Shiki
 * CodeBlock, KaTeX math, gated Mermaid, a CSV table toolbar off `blockData`,
 * decorators, `urlTransform`, `onLinkClick`, a DOMPurify `sanitize`, the opt-in
 * streaming caret, `stickToBottom` and `virtualize`.
 *
 * Every non-primitive prop below is defined at MODULE scope. That is the single
 * most important thing in this file: a fresh `components` / `decorators` /
 * `urlTransform` / `sanitize` identity on each render busts the per-block memo,
 * so every committed block re-renders on every patch (O(n^2) over a stream).
 */
import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { BrookMarkdown } from "brookmd/react";
import { safeUrl, wrapLink } from "brookmd/html-to-react";
import type {
  BlockComponentProps,
  Components,
  Decorator,
  ParserConfig,
  TableData,
  UrlTransform,
} from "brookmd";
import DOMPurify from "dompurify";
import katex from "katex";
import mermaid from "mermaid";
import { codeToHtml } from "shiki";

/* ── config ──────────────────────────────────────────────────────────────── */

const CONFIG: ParserConfig = {
  softBreaks: true,
  dirAuto: true,
  a11y: true,
  blockData: true, // powers the table toolbar + `props.math.latex` / `props.code.code`
  gfmMath: true,
  gfmFootnotes: true,
};

/* ── block-contract overrides (BlockComponentProps) ──────────────────────── */

/**
 * Shiki is async, so it can only run on a CLOSED block. While `open`, render
 * `props.text` (the decoded source, populated even mid-stream) as plain text —
 * never call an async highlighter per patch.
 */
const CodeBlock = memo(function CodeBlock(props: BlockComponentProps) {
  const { text = "", language = "text", open } = props;
  const [html, setHtml] = useState<string | null>(null);

  useEffect(() => {
    if (open) {
      setHtml(null);
      return;
    }
    let live = true;
    codeToHtml(text, { lang: language, theme: "github-dark" }).then(
      (out) => {
        if (live) setHtml(out);
      },
      () => {},
    );
    return () => {
      live = false;
    };
  }, [text, language, open]);

  if (html) return <div className="shiki-wrap" dangerouslySetInnerHTML={{ __html: html }} />;
  return (
    <pre className="shiki-plain" data-lang={language}>
      <code>{text}</code>
    </pre>
  );
});

/**
 * KaTeX from `props.text` (the decoded LaTeX). Half-typed LaTeX throws on almost
 * every patch, so typeset only once the block has closed.
 */
const MathBlock = memo(function MathBlock(props: BlockComponentProps) {
  const { text = "", open } = props;
  const rendered = useMemo(() => {
    if (open) return null;
    try {
      return katex.renderToString(text, { displayMode: true, throwOnError: false });
    } catch {
      return null;
    }
  }, [text, open]);

  if (!rendered) return <pre className="math-raw">{text}</pre>;
  return <div className="math math-display" dangerouslySetInnerHTML={{ __html: rendered }} />;
});

/** Mermaid only ever sees a complete diagram: `mermaid.render` throws on a partial one. */
const Mermaid = memo(function Mermaid(props: BlockComponentProps) {
  const { text = "", open } = props;
  const [svg, setSvg] = useState<string | null>(null);
  const idRef = useRef(`mmd-${Math.random().toString(36).slice(2)}`);

  useEffect(() => {
    if (open) {
      setSvg(null);
      return;
    }
    let live = true;
    mermaid
      .render(idRef.current, text)
      .then((r: { svg: string }) => {
        if (live) setSvg(r.svg);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, [text, open]);

  if (!svg) return <pre className="mermaid-source">{text}</pre>;
  return <div className="mermaid-svg" dangerouslySetInnerHTML={{ __html: svg }} />;
});

/** RFC-4180 CSV built from `cell.text` (plaintext) — never from the display HTML. */
function toCsv(table: TableData): string {
  const quote = (s: string) => (/[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
  const line = (cells: { text: string }[]) => cells.map((c) => quote(c.text)).join(",");
  return [line(table.headers), ...table.rows.map(line)].join("\n");
}

/**
 * A `Table` override driven entirely by `props.table` (`blockData: true`). It is
 * invoked on OPEN blocks too, so keep only UI state in React and DERIVE the rows
 * — the table keeps growing as rows stream in.
 */
const Table = memo(function Table(props: BlockComponentProps) {
  const table = props.table;
  const [copied, setCopied] = useState(false);

  // Guard: `blockData` may be off, or an open table may arrive before its data
  // is populated. Fall back to the block HTML rather than crashing.
  if (!table) {
    return <div dangerouslySetInnerHTML={{ __html: DOMPurify.sanitize(props.html) }} />;
  }

  const copy = () => {
    navigator.clipboard?.writeText(toCsv(table)).then(
      () => setCopied(true),
      () => {},
    );
  };

  return (
    <div className="table-wrap">
      <button type="button" onClick={copy}>
        {copied ? "Copied" : "Copy CSV"}
      </button>
      <table>
        <thead>
          <tr>
            {table.headers.map((h, i) => (
              <th key={i} style={{ textAlign: table.aligns[i] ?? undefined }}>
                <span dangerouslySetInnerHTML={{ __html: DOMPurify.sanitize(h.html) }} />
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {table.rows.map((row, r) => (
            <tr key={r}>
              {row.map((c, i) => (
                <td
                  key={i}
                  style={{ textAlign: table.aligns[i] ?? undefined }}
                  dangerouslySetInnerHTML={{ __html: DOMPurify.sanitize(c.html) }}
                />
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
});

/* ── tag-contract overrides (attributes + children, NO `block` prop) ─────── */

type TagProps = { children?: unknown; [attr: string]: unknown };

/** Lazy, non-shifting images. Element path: there is no `props.block` here. */
function Img(props: TagProps) {
  return (
    <img
      {...(props as Record<string, never>)}
      loading="lazy"
      decoding="async"
      style={{ maxWidth: "100%", height: "auto" }}
    />
  );
}

/** A `Thinking` tag can be matched by BOTH dispatchers — guard on `block`. */
function Thinking(props: Partial<BlockComponentProps> & TagProps) {
  const kids = props.children as ReactNode;
  if (!props.block) return <span className="thinking-inline">{kids}</span>;
  return <details className="thinking-block">{kids}</details>;
}

const COMPONENTS: Components = {
  CodeBlock,
  MathBlock,
  Mermaid,
  Table,
  img: Img,
  Thinking,
};

/* ── decorators / urlTransform / sanitize ────────────────────────────────── */

/**
 * Decorator output is TRUSTED — it does NOT pass through brookmd's attribute
 * sanitizer. Route every href through `safeUrl` (or use `wrapLink`, which does).
 */
const DECORATORS: Decorator[] = [
  {
    match: /\$[A-Z]{1,5}\b/g,
    replace: (text) => wrapLink(text, { href: `/ticker/${text.slice(1)}`, class: "ticker" }),
    skipInside: ["a", "code", "pre", "kbd"],
  },
];

/** Output is re-sanitized (`safeUrl(transform(safeUrl(v)))`), so it can't smuggle a scheme. */
const URL_TRANSFORM: UrlTransform = (url, ctx) => {
  if (ctx.attr === "src" && /^https?:/i.test(url)) {
    return `/img-proxy?u=${encodeURIComponent(url)}`;
  }
  return url;
};

/** Applied to every block's HTML before injection — INCLUDING the open tail. */
const sanitize = (html: string) => DOMPurify.sanitize(html);

/* ── the component ───────────────────────────────────────────────────────── */

export function RichAnswer({ stream }: { stream: AsyncIterable<string> }) {
  return (
    <div style={{ overflowY: "auto", scrollSnapType: "y proximity", height: "100%" }}>
      <BrookMarkdown
        stream={stream}
        streamConfig={CONFIG}
        components={COMPONENTS}
        decorators={DECORATORS}
        urlTransform={URL_TRANSFORM}
        sanitize={sanitize}
        // Blinking caret at the end of the open block (opt-in, from styles.css).
        className="brook-caret"
        stickToBottom
        // content-visibility:auto on CLOSED blocks — for very long documents.
        virtualize
        role="log"
        aria-live="polite"
        onLinkClick={(event, link) => {
          // One delegated listener on the `.brook-md` root. Streaming links with
          // no href yet (`<a data-brook-pending>`) never reach here.
          if (!/^https:\/\/(docs\.)?example\.com\//.test(safeUrl(link.href))) {
            event.preventDefault();
            showInterstitial(link.href, link.text);
          }
        }}
        onBlockError={(err, info) => console.error("block failed", info.kind, err)}
        onStreamError={(err) => console.error(err)}
      />
    </div>
  );
}

declare function showInterstitial(href: string, label: string): void;

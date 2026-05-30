import {
  createElement,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
} from "react";
import {
  FluxClient,
  FluxMarkdown,
  type BlockComponentProps,
  type Components,
  type HeadingData,
  type TableData,
} from "flux-md";
import DOMPurify from "dompurify";
import { streamDemoDoc } from "../streaming/demoDoc";

// Same sanitizer the chat uses; stable identity so the block memo doesn't churn.
const sanitizeHtml = (html: string) => DOMPurify.sanitize(html);

/* ──────────────────────────────────────────────────────────────────────────
   1. EnhancedTable — a `components.Table` override driven ENTIRELY by
   `props.table` (the structured `block.kind.data`). Column sort, text filter,
   and Copy-CSV are all computed from each cell's plaintext `.text`; cells are
   displayed via their inline `.html`. No HTML re-parse, no HAST tree — and it
   works while the table is still streaming because the override is invoked on
   open blocks too (`props.table.rows` grows as rows arrive) and only sort/
   filter UI state lives in React, with displayed rows DERIVED each render.
   ────────────────────────────────────────────────────────────────────────── */

type SortDir = "asc" | "desc";

// RFC-4180 CSV from DATA: quote any field containing a comma, quote, or newline,
// and double internal quotes. Built from cell.text — never the display HTML.
function toCsv(table: TableData): string {
  const quote = (s: string) => (/[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
  const line = (cells: { text: string }[]) => cells.map((c) => quote(c.text)).join(",");
  return [line(table.headers), ...table.rows.map(line)].join("\n");
}

// Numeric-aware comparison on plaintext: sort "9" before "18" when both numeric,
// else case-insensitive locale compare. Operates purely on `cell.text`.
function compareText(a: string, b: string): number {
  const na = Number(a);
  const nb = Number(b);
  if (a.trim() !== "" && b.trim() !== "" && !Number.isNaN(na) && !Number.isNaN(nb)) {
    return na - nb;
  }
  return a.localeCompare(b, undefined, { sensitivity: "base" });
}

function EnhancedTable(props: BlockComponentProps) {
  const table = props.table;

  // Sort/filter are the ONLY state — displayed rows are derived below, so the
  // table keeps reflecting new rows that stream in (BlockView is keyed by the
  // stable block id, so this state survives every streaming patch).
  const [sortCol, setSortCol] = useState<number | null>(null);
  const [sortDir, setSortDir] = useState<SortDir>("asc");
  const [filter, setFilter] = useState("");
  const [copied, setCopied] = useState(false);

  const rows = table?.rows ?? [];

  const viewRows = useMemo(() => {
    const q = filter.trim().toLowerCase();
    let out = q
      ? rows.filter((r) => r.some((c) => c.text.toLowerCase().includes(q)))
      : rows.slice();
    if (sortCol !== null) {
      out = out.slice().sort((a, b) => {
        const cmp = compareText(a[sortCol]?.text ?? "", b[sortCol]?.text ?? "");
        return sortDir === "asc" ? cmp : -cmp;
      });
    }
    return out;
  }, [rows, filter, sortCol, sortDir]);

  // Guard: an open table may arrive before its `kind.data` is populated (or
  // blockData could be off) — fall back to the sanitized display HTML so we
  // never crash, and the block still renders.
  if (!table) {
    return (
      <div
        className="flux-block flux-block-table"
        dangerouslySetInnerHTML={{ __html: sanitizeHtml(props.html) }}
      />
    );
  }

  const toggleSort = (col: number) => {
    if (sortCol === col) {
      setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortCol(col);
      setSortDir("asc");
    }
  };

  const copyCsv = async () => {
    const csv = toCsv(table);
    try {
      await navigator.clipboard.writeText(csv);
    } catch {
      // Clipboard may be unavailable (no focus / permissions) — surface the CSV
      // on the element so a smoke can still assert it was built from DATA.
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };

  const alignOf = (i: number) => table.aligns[i] ?? undefined;

  return (
    <div className="ds-table" data-flux-open={props.open ? "1" : undefined}>
      <div className="ds-table-toolbar">
        <input
          className="ds-filter"
          type="text"
          value={filter}
          placeholder="Filter rows…"
          spellCheck={false}
          aria-label="Filter table rows"
          onChange={(e) => setFilter(e.target.value)}
        />
        <span className="ds-rowcount">
          {viewRows.length}/{rows.length} rows
          {props.open && <span className="ds-live-dot" title="streaming" aria-hidden="true" />}
        </span>
        <button
          className="ds-csv-btn"
          onClick={copyCsv}
          data-csv={toCsv(table)}
          title="Copy the table as CSV, built from cell.text"
        >
          {copied ? "Copied ✓" : "Copy CSV"}
        </button>
      </div>
      <div className="ds-table-scroll">
        <table className="ds-grid">
          <thead>
            <tr>
              {table.headers.map((h, i) => {
                const active = sortCol === i;
                return (
                  <th
                    key={i}
                    style={{ textAlign: alignOf(i) }}
                    className={"ds-th" + (active ? " is-sorted" : "")}
                    aria-sort={active ? (sortDir === "asc" ? "ascending" : "descending") : "none"}
                    onClick={() => toggleSort(i)}
                    title="Click to sort by this column (from cell.text)"
                  >
                    <span dangerouslySetInnerHTML={{ __html: sanitizeHtml(h.html) }} />
                    <span className="ds-sort-caret" aria-hidden="true">
                      {active ? (sortDir === "asc" ? "▲" : "▼") : "↕"}
                    </span>
                  </th>
                );
              })}
            </tr>
          </thead>
          <tbody>
            {viewRows.map((row, r) => (
              <tr key={r}>
                {row.map((cell, c) => (
                  <td
                    key={c}
                    style={{ textAlign: alignOf(c) }}
                    dangerouslySetInnerHTML={{ __html: sanitizeHtml(cell.html) }}
                  />
                ))}
              </tr>
            ))}
            {viewRows.length === 0 && (
              <tr>
                <td className="ds-empty" colSpan={Math.max(1, table.headers.length)}>
                  No rows match “{filter}”.
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

/* ──────────────────────────────────────────────────────────────────────────
   2. HeadingAnchor — a `components.Heading` override. GFM byte-output does NOT
   put `id` attributes on `<hN>` (GitHub adds those in a post-pass), so we emit
   them ourselves from `props.heading.id` to create scroll targets the TOC links
   to. `props.heading.text` is the inline-stripped plaintext slug source.
   ────────────────────────────────────────────────────────────────────────── */

function HeadingAnchor(props: BlockComponentProps) {
  const h: HeadingData | undefined = props.heading;
  if (!h) {
    return (
      <div
        className="flux-block flux-block-heading"
        dangerouslySetInnerHTML={{ __html: sanitizeHtml(props.html) }}
      />
    );
  }
  const level = Math.min(Math.max(h.level, 1), 6);
  return createElement(
    `h${level}`,
    { id: h.id, className: "ds-heading" },
    h.text,
  );
}

// Module scope so the components-object identity is stable (a fresh object each
// render would bust flux-md's per-block memo and force every block to re-parse).
const COMPONENTS: Components = { Table: EnhancedTable, Heading: HeadingAnchor };

/* ──────────────────────────────────────────────────────────────────────────
   3. TableOfContents — built LIVE from the client's snapshot (not from a
   Heading override that registers during render, which would setState mid-
   render). We subscribe to the same external store FluxMarkdown uses, filter
   Heading blocks, and read each block's `kind.data` ({ level, text, id }).
   ────────────────────────────────────────────────────────────────────────── */

function useHeadings(client: FluxClient): HeadingData[] {
  const blocks = useSyncExternalStore(client.subscribe, client.getSnapshot, client.getSnapshot);
  return useMemo(() => {
    const out: HeadingData[] = [];
    for (const b of blocks) {
      if (b.kind.type !== "Heading") continue;
      const d = b.kind.data;
      if (d && typeof d === "object" && "id" in d) out.push(d as HeadingData);
    }
    return out;
  }, [blocks]);
}

function TableOfContents({ client }: { client: FluxClient }) {
  const headings = useHeadings(client);
  const minLevel = headings.length ? Math.min(...headings.map((h) => h.level)) : 1;

  const jump = useCallback((e: React.MouseEvent, id: string) => {
    e.preventDefault();
    const el = document.getElementById(id);
    if (el) el.scrollIntoView({ behavior: "smooth", block: "start" });
  }, []);

  return (
    <nav className="ds-toc" aria-label="Table of contents">
      <div className="ds-toc-title">
        Contents
        <span className="ds-toc-count">{headings.length}</span>
      </div>
      {headings.length === 0 ? (
        <p className="ds-toc-empty">Headings appear here as they stream…</p>
      ) : (
        <ul className="ds-toc-list">
          {headings.map((h, i) => (
            <li
              key={h.id + ":" + i}
              className="ds-toc-item"
              style={{ paddingLeft: 8 + (h.level - minLevel) * 14 }}
            >
              <a href={`#${h.id}`} className="ds-toc-link" onClick={(e) => jump(e, h.id)}>
                {h.text}
              </a>
            </li>
          ))}
        </ul>
      )}
    </nav>
  );
}

/* ──────────────────────────────────────────────────────────────────────────
   DataStudio — owns one blockData-enabled client, drives it from the canned
   doc via pipeFrom on a "Run / Replay" button (caller-owned client, not the
   one-shot `stream` prop, so React StrictMode's dev double-mount can't truncate
   the stream). Sidebar = live TOC; main = streamed markdown with the overrides.
   ────────────────────────────────────────────────────────────────────────── */

export function DataStudio() {
  // One blockData-enabled client for this view's lifetime. THE opt-in: turning
  // on `blockData` is what populates each block's structured `kind.data`.
  const [client] = useState(() => new FluxClient({ config: { blockData: true } }));
  const [running, setRunning] = useState(false);
  const [started, setStarted] = useState(false);
  const abortRef = useRef<AbortController | null>(null);

  const run = useCallback(async () => {
    abortRef.current?.abort();
    const ac = new AbortController();
    abortRef.current = ac;
    setRunning(true);
    setStarted(true);
    await client.whenReady();
    client.reset();
    try {
      await client.pipeFrom(streamDemoDoc(), { signal: ac.signal });
    } finally {
      if (!ac.signal.aborted) setRunning(false);
    }
  }, [client]);

  // Own the client's pool attachment. reattach() on (re)mount, destroy() on
  // unmount — mirroring the library's own `useFluxStream`. This matters under
  // React StrictMode (dev), whose double-mount destroys the SAME instance on the
  // simulated unmount, then remounts it: without reattach its patches would be
  // dropped and it'd render blank. reattach() is idempotent on first mount.
  useEffect(() => {
    client.reattach();
    return () => {
      abortRef.current?.abort();
      client.destroy();
    };
  }, [client]);

  return (
    <div className="ds">
      <div className="ds-head">
        <div className="ds-head-text">
          <h1 className="ds-title">Data Studio</h1>
          <p className="ds-sub">
            The table toolbar (sort · filter · CSV) and the live outline are built
            from <code>block.kind.data</code> — flux-md 0.10.0’s opt-in{" "}
            <code>{`{ blockData: true }`}</code> channel. <b>No HTML re-parsing</b>,
            and it all works <b>mid-stream</b>: hit Run and sort or filter while
            rows are still arriving.
          </p>
        </div>
        <button className="ds-run-btn" onClick={run} disabled={running}>
          {running ? "Streaming…" : started ? "Replay" : "Run demo"}
        </button>
      </div>

      <div className="ds-layout">
        <aside className="ds-aside">
          <TableOfContents client={client} />
          <p className="ds-note">
            Outline from <code>kind.type === "Heading"</code> →{" "}
            <code>{`{ level, text, id }`}</code>. Anchors scroll to ids this demo
            emits via a <code>Heading</code> override (GFM output has none).
          </p>
        </aside>

        <main className="ds-main">
          {!started ? (
            <div className="ds-placeholder">
              <p>
                Click <b>Run demo</b> to stream a markdown document with GFM
                tables, headings, and a code block. Watch the table become a
                sortable/filterable grid and the contents fill in — live.
              </p>
            </div>
          ) : (
            <FluxMarkdown client={client} components={COMPONENTS} sanitize={sanitizeHtml} />
          )}
        </main>
      </div>
    </div>
  );
}

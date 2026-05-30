import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { createElement, act } from "react";
import type { Block, FromWorker, ToWorker, WorkerLike, TableData, BlockComponentProps } from "../src/types";
import { FluxClient, FluxPool } from "../src/client";
import { FluxMarkdown } from "../src/react";

// PROOF: a Table block's structured `kind.data` is sufficient to build a
// sort/filter/transpose/chart/CSV toolbar from DATA — no HTML re-parse, no HAST.
// We drive a synthetic patch (a FakeWorker) carrying the exact wire shape the
// Rust core emits, render a real React `components.Table` override, and have it
// (a) sort rows by a column's `cell.text` and (b) emit CSV from `cell.text`.
// This is the consumer's actual use case, proven end-to-end through the renderer.

// Synchronous fake worker (same shape as the other suites): records posts and
// fires patch responses back through the registered listener.
class FakeWorker implements WorkerLike {
  sent: ToWorker[] = [];
  private listener: ((ev: { data: FromWorker }) => void) | null = null;
  postMessage(msg: ToWorker) {
    this.sent.push(msg);
  }
  addEventListener(_t: "message", l: (ev: { data: FromWorker }) => void) {
    this.listener = l;
  }
  terminate() {}
  fire(msg: FromWorker) {
    this.listener?.({ data: msg });
  }
}

let win: GlobalWindow;
beforeAll(() => {
  win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.window = win;
  g.navigator = win.navigator;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  (g as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

function makeClient() {
  const created: FakeWorker[] = [];
  const pool = new FluxPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 1);
  const client = new FluxClient({ pool });
  return { client, worker: () => created[0] };
}

function patch(committed: Block[], active: Block[], streamId = 1): FromWorker {
  return {
    type: "patch",
    streamId,
    patch: { newly_committed: committed, active },
    appendedBytes: 0,
    parseMicros: 0,
    retainedBytes: 0,
    wasmMemoryBytes: 0,
  };
}

async function mount(node: ReturnType<typeof createElement>) {
  const { createRoot } = await import("react-dom/client");
  const host = win.document.createElement("div");
  const root = createRoot(host as unknown as Element);
  await act(async () => {
    root.render(node);
  });
  return { host, root };
}

// The exact wire shape the Rust core emits under `setBlockData(true)` — captured
// verbatim from the real WASM boundary (`/tmp/probe.mjs`). `text` is the
// inline-stripped plaintext; `html` is the inline-rendered display markup.
const tableData: TableData = {
  headers: [
    { text: "Name", html: "<strong>Name</strong>" },
    { text: "Age", html: "Age" },
  ],
  rows: [
    [
      { text: "Charlie", html: "Charlie" },
      { text: "30", html: "30" },
    ],
    [
      { text: "Alice", html: "Alice" },
      { text: "20", html: '<a href="x" target="_blank" rel="noopener noreferrer nofollow">20</a>' },
    ],
    [
      { text: "Bob", html: "Bob" },
      { text: "25", html: "25" },
    ],
  ],
  aligns: ["left", "right"],
};

function tableBlock(id: number): Block {
  return {
    id,
    kind: { type: "Table", data: tableData },
    start: 0,
    end: 0,
    html: "<table><thead>...</thead><tbody>...</tbody></table>",
    open: false,
    speculative: false,
  };
}

// CSV from DATA: join each cell's plaintext with comma/newline, RFC-4180 quoting.
function toCsv(t: TableData): string {
  const quote = (s: string) => (/[",\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
  const line = (cells: { text: string }[]) => cells.map((c) => quote(c.text)).join(",");
  return [line(t.headers), ...t.rows.map(line)].join("\n");
}

test("React Table override reads kind.data and sorts rows + emits CSV from DATA (no HTML re-parse)", async () => {
  const { client, worker } = makeClient();
  client.append(""); // force worker creation so we can fire at its listener

  // Captured side-channel: what the override saw and computed from `props.table`.
  let seenTable: TableData | undefined;
  let sortedNames: string[] = [];
  let csv = "";

  // A `components.Table` override — the consumer's toolbar, working from DATA.
  function Table(props: BlockComponentProps) {
    seenTable = props.table; // the typed convenience field === block.kind.data
    const t = props.table;
    if (t) {
      // (a) SORT rows ascending by the "Name" column's plaintext.
      sortedNames = [...t.rows]
        .sort((a, b) => a[0].text.localeCompare(b[0].text))
        .map((r) => r[0].text);
      // (b) CSV straight from cell.text — no HTML, no HAST.
      csv = toCsv(t);
    }
    return createElement("div", { "data-testid": "toolbar" }, sortedNames.join("|"));
  }

  await mount(createElement(FluxMarkdown, { client, components: { Table } }));

  await act(async () => {
    worker().fire(patch([tableBlock(1)], []));
  });

  // The override received the structured data via the typed `table` field.
  expect(seenTable).toEqual(tableData);

  // (a) Sorted purely from `cell.text` — the toolbar's sort works from DATA.
  expect(sortedNames).toEqual(["Alice", "Bob", "Charlie"]);

  // (b) Exact CSV string from `cell.text` (note: header **Name** bold is NOT in
  // the plaintext — proves we used DATA, not the display HTML). The `20` cell's
  // anchor markup is likewise absent.
  expect(csv).toBe("Name,Age\nCharlie,30\nAlice,20\nBob,25");
});

test("props.table is undefined for a Table block parsed WITHOUT blockData (byte-identical-off)", async () => {
  const { client, worker } = makeClient();
  client.append("");

  let seen: { hadField: boolean; table: TableData | undefined } | null = null;
  function Table(props: BlockComponentProps) {
    seen = { hadField: "table" in props, table: props.table };
    return createElement("div", null, "x");
  }

  // A Table block as the core emits it with the flag OFF: `kind` has no `data`.
  const offBlock: Block = {
    id: 1,
    kind: { type: "Table" },
    start: 0,
    end: 0,
    html: "<table></table>",
    open: false,
    speculative: false,
  };

  await mount(createElement(FluxMarkdown, { client, components: { Table } }));
  await act(async () => {
    worker().fire(patch([offBlock], []));
  });

  expect(seen).not.toBeNull();
  // The field is present (we always set the branch) but carries `undefined`,
  // mirroring `kind.data === undefined` — a consumer reads `props.table` and
  // simply falls back to the display HTML.
  expect(seen!.table).toBeUndefined();
});

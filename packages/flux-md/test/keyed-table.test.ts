import { test, expect, beforeAll, beforeEach } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { createElement, act } from "react";
import type { Block, FromWorker, ToWorker, WorkerLike, TableData } from "../src/types";
import { FluxClient, FluxPool } from "../src/client";
import { FluxMarkdown } from "../src/react";
import { mountFluxMarkdown } from "../src/dom";
import { getParseCount, resetParseCount } from "../src/html-to-react";

// FEATURE: keyed Table renderer for the STREAMING TAIL (opt-in `blockData`).
// When a Table block is OPEN and carries structured `kind.data`, both renderers
// build a real `<table>` with keyed rows so only the growing TRAILING row is
// re-rendered each patch — committed rows keep their identity and their cells
// never re-tokenize. Closed tables stay on the full-HTML memo/fingerprint path.

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

beforeEach(() => resetParseCount());

// happy-dom's element types don't structurally match lib.dom's HTMLElement, so
// read inline `text-align` off the style object through `unknown` (test-only).
function textAlign(el: unknown): string {
  return (el as { style: { textAlign: string } }).style.textAlign;
}

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

// Build an OPEN Table block whose structured data has `rows` body rows; the LAST
// row is the still-growing trailing row. The HTML string is a faithful prefix —
// only its presence/shape matters (the keyed path reads `kind.data`, the closed
// fallback reads `html`).
// `rows` is the EXACT current cell content of every body row (committed rows
// keep byte-stable cells across patches; the trailing row's cells grow). This
// mirrors the real parser: committed cells never change, only the open tail does.
function openTable(id: number, rows: string[][]): Block {
  const cell = (s: string) => ({ text: s, html: s });
  const dataRows = rows.map((r) => r.map(cell));
  const data: TableData = {
    headers: [cell("Name"), cell("Age")],
    rows: dataRows,
    aligns: ["left", "right"],
  };
  // The block HTML grows with the data EXACTLY as the real parser re-renders the
  // full table each patch — so the renderer's fingerprint (block.html) changes
  // every tick and the open block is actually re-evaluated (as in production).
  const body = dataRows
    .map((r) => "<tr>" + r.map((c) => "<td>" + c.html + "</td>").join("") + "</tr>")
    .join("");
  return {
    id,
    kind: { type: "Table", data },
    start: 0,
    end: 0,
    html: `<table><thead><tr><th>Name</th><th>Age</th></tr></thead><tbody>${body}</tbody></table>`,
    open: true,
    speculative: false,
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

// ---------------------------------------------------------------------------
// React
// ---------------------------------------------------------------------------

test("React keyed table: open table with blockData renders real <table> with aligned keyed rows", async () => {
  const { client, worker } = makeClient();
  client.append("");
  const { host } = await mount(createElement(FluxMarkdown, { client }));

  await act(async () => {
    worker().fire(patch([], [openTable(1, [["Alice", "20"], ["Bob", "25"]])]));
  });

  const table = host.querySelector("table")!;
  expect(table).not.toBeNull();
  // Header cells.
  const ths = table.querySelectorAll("thead th");
  expect(Array.from(ths).map((t) => t.textContent)).toEqual(["Name", "Age"]);
  // Alignment from data.aligns is applied as inline text-align.
  expect(textAlign(ths[0])).toBe("left");
  expect(textAlign(ths[1])).toBe("right");
  // Body rows.
  const trs = table.querySelectorAll("tbody tr");
  expect(trs.length).toBe(2);
  expect(Array.from(trs[0].querySelectorAll("td")).map((t) => t.textContent)).toEqual(["Alice", "20"]);
  expect(Array.from(trs[1].querySelectorAll("td")).map((t) => t.textContent)).toEqual(["Bob", "25"]);
});

test("React keyed table: only the trailing row re-tokenizes across streaming patches", async () => {
  const { client, worker } = makeClient();
  client.append("");
  await mount(createElement(FluxMarkdown, { client }));

  // First patch: 1 body row (the open trailing row). Cells: 2 headers + 2 cells.
  await act(async () => {
    worker().fire(patch([], [openTable(1, [["Al", "2"]])]));
  });
  const afterFirst = getParseCount();
  expect(afterFirst).toBeGreaterThan(0); // header + first-row cells parsed

  // Grow the SAME single open row — BOTH its cells' html change. Header cells must
  // NOT re-parse; only the open row's 2 cells do (cell-level memo skips byte-stable
  // cells, and here both changed).
  resetParseCount();
  await act(async () => {
    worker().fire(patch([], [openTable(1, [["Alx", "2x"]])]));
  });
  expect(getParseCount()).toBe(2); // exactly the 2 cells of the open row; headers memoized

  // Add a SECOND row. The first row is now COMMITTED — its cell html is the SAME
  // bytes it had above (`Alx`/`2x`), so its cells are memoized (no re-parse); only
  // the new trailing row's 2 cells parse.
  resetParseCount();
  await act(async () => {
    worker().fire(patch([], [openTable(1, [["Alx", "2x"], ["Bo", "2"]])]));
  });
  expect(getParseCount()).toBe(2); // only the new trailing row (committed row 0 memoized)

  // Grow the trailing row again (committed row 0 unchanged): only its 2 cells parse.
  resetParseCount();
  await act(async () => {
    worker().fire(patch([], [openTable(1, [["Alx", "2x"], ["Boz", "25"]])]));
  });
  expect(getParseCount()).toBe(2); // still only the trailing row's 2 cells
});

test("React: a Table WITHOUT blockData falls back to the opaque-HTML path (no keyed table)", async () => {
  const { client, worker } = makeClient();
  client.append("");
  const offBlock: Block = {
    id: 1,
    kind: { type: "Table" }, // no `data`
    start: 0,
    end: 0,
    html: "<table><thead><tr><th>Name</th></tr></thead><tbody><tr><td>x</td></tr></tbody></table>",
    open: true,
    speculative: false,
  };
  const { host } = await mount(createElement(FluxMarkdown, { client }));
  await act(async () => {
    worker().fire(patch([], [offBlock]));
  });
  // Still renders a table (from the raw HTML), but via innerHTML — proven by the
  // tokenizer never running for this no-components fast path.
  expect(host.querySelector("table")).not.toBeNull();
  expect(getParseCount()).toBe(0);
});

// ---------------------------------------------------------------------------
// DOM
// ---------------------------------------------------------------------------

test("DOM keyed table: committed <tr> nodes keep identity; only the trailing row is replaced", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = win.document.createElement("div");
  const handle = mountFluxMarkdown(client, container as unknown as HTMLElement, { batch: false });
  const root = container.querySelector(".flux-md")!;

  // First: one open row.
  worker().fire(patch([], [openTable(1, [["Alice", "2"]])]));
  let tbody = root.querySelector("tbody")!;
  expect(tbody.children.length).toBe(1);

  // Grow the same open row: the trailing <tr> is replaced (its cells changed).
  const openV1 = tbody.children[0];
  worker().fire(patch([], [openTable(1, [["Alice", "20"]])]));
  tbody = root.querySelector("tbody")!;
  expect(tbody.children[0]).not.toBe(openV1); // open row replaced on growth

  // Add a second row: row 0 is now COMMITTED — its <tr> node must keep identity
  // forever; only the new trailing row appears as a fresh node.
  worker().fire(patch([], [openTable(1, [["Alice", "20"], ["Bob", "2"]])]));
  tbody = root.querySelector("tbody")!;
  expect(tbody.children.length).toBe(2);
  const committedRow = tbody.children[0];

  // Grow the trailing (2nd) row several times: committed row 0 stays the SAME node.
  for (const age of ["25", "250", "2500"]) {
    worker().fire(patch([], [openTable(1, [["Alice", "20"], ["Bob", age]])]));
    tbody = root.querySelector("tbody")!;
    expect(tbody.children[0]).toBe(committedRow); // committed row: same ref
    expect(tbody.children.length).toBe(2);
  }

  // Header cells carry the data alignment.
  const ths = root.querySelectorAll("thead th");
  expect(textAlign(ths[0])).toBe("left");
  expect(textAlign(ths[1])).toBe("right");

  // CLOSE the table: it leaves the keyed path and renders the full committed
  // HTML once (the fingerprint flips open→closed). The block node is rebuilt.
  const closed: Block = { ...openTable(1, [["Alice", "20"], ["Bob", "25"]]), open: false };
  worker().fire(patch([closed], []));
  const finalTable = root.querySelector("table")!;
  expect(finalTable.querySelectorAll("tbody tr").length).toBe(2);
  expect(finalTable.querySelector("tbody tr td")!.textContent).toBe("Alice");

  handle.destroy();
});

test("DOM: a Table WITHOUT blockData renders via the generic innerHTML path (no keyed manager)", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = win.document.createElement("div");
  const handle = mountFluxMarkdown(client, container as unknown as HTMLElement, { batch: false });
  const root = container.querySelector(".flux-md")!;

  const offBlock: Block = {
    id: 1,
    kind: { type: "Table" },
    start: 0,
    end: 0,
    html: "<table><tbody><tr><td>x</td></tr></tbody></table>",
    open: true,
    speculative: false,
  };
  worker().fire(patch([], [offBlock]));
  // The generic path wraps the raw HTML in a flux-block div.
  const block = root.querySelector(".flux-block-table")!;
  expect(block).not.toBeNull();
  expect(block.querySelector("table")).not.toBeNull();

  handle.destroy();
});

import { test, expect, beforeAll } from "bun:test";
import { existsSync, readFileSync } from "node:fs";

// End-to-end coverage of the REAL compiled WASM boundary. Every other test in
// this suite drives a FakeWorker with synthetic patches — none exercises the
// actual FluxParser, its serde shape, or the setGfm*/setComponentTags binding.
// A serde field rename or a changed #[wasm_bindgen(js_name)] would keep all the
// Rust tests green yet silently break JS; these assertions are the tripwire.
//
// src/wasm is git-ignored (built by `bun run build:wasm`, which CI runs before
// tests). On a fresh checkout it's absent, so we DYNAMICALLY import the glue and
// skip — a static import would fail collection and break `bun test` for anyone
// who hasn't built the WASM yet.

const wasmUrl = new URL("../src/wasm/flux_md_core_bg.wasm", import.meta.url);
const haveWasm = existsSync(wasmUrl);

if (!haveWasm) {
  // eslint-disable-next-line no-console
  console.warn(
    "[wasm-integration] src/wasm not built — run `bun run build:wasm` to enable the real-WASM tests; skipping.",
  );
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
let FluxParser: any;

beforeAll(async () => {
  if (!haveWasm) return;
  const glue = "../src/wasm/flux_md_core.js"; // variable specifier → resolved at runtime, not collection
  const mod = await import(glue);
  // The named `initSync` (NOT the async default export) compiles raw bytes
  // synchronously — no fetch shim / happy-dom needed in a bun test.
  mod.initSync({ module: readFileSync(wasmUrl) });
  FluxParser = mod.FluxParser;
});

// Parse a whole input in one append + finalize and return the final block set,
// deduped by stable id (finalize's closed version of a block wins).
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function parseAll(input: string, configure?: (p: any) => void) {
  const p = new FluxParser();
  configure?.(p);
  const a = p.append(input);
  const f = p.finalize();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const byId = new Map<number, any>();
  for (const b of [...a.newly_committed, ...a.active, ...f.newly_committed, ...f.active]) {
    byId.set(b.id, b);
  }
  return { append: a, blocks: [...byId.values()] };
}

test.skipIf(!haveWasm)("real WASM: append() returns the documented Patch shape with kind.data intact", () => {
  const { append, blocks } = parseAll("# Title\n\nHello\n");
  // Serde shape: exactly { newly_committed, active }.
  expect(Object.keys(append).sort()).toEqual(["active", "newly_committed"]);

  const h = blocks.find((b) => b.kind.type === "Heading");
  expect(h).toBeDefined();
  expect(h.kind.data).toBe(1); // Heading(u8) → data = level
  expect(h.html).toContain("<h1");
  expect(h.open).toBe(false); // closed after finalize
  expect(typeof h.id).toBe("number");
  expect(blocks.some((b) => b.kind.type === "Paragraph")).toBe(true);
});

test.skipIf(!haveWasm)("real WASM: a CodeBlock carries its info-string language across the boundary", () => {
  const { blocks } = parseAll("```js\nconst x = 1;\n```\n");
  const code = blocks.find((b) => b.kind.type === "CodeBlock");
  expect(code).toBeDefined();
  expect(code.kind.data?.lang).toBe("js");
});

test.skipIf(!haveWasm)("real WASM: setGfmMath toggles math output end-to-end (flag crosses the boundary)", () => {
  const off = parseAll("$x$\n");
  const on = parseAll("$x$\n", (p) => p.setGfmMath(true));
  expect(off.blocks.some((b) => b.html.includes("math-inline"))).toBe(false);
  expect(on.blocks.some((b) => b.html.includes("math-inline"))).toBe(true);
});

test.skipIf(!haveWasm)("real WASM: setA11y wraps a task-list checkbox in a <label>", () => {
  const off = parseAll("- [ ] todo\n");
  const on = parseAll("- [ ] todo\n", (p) => p.setA11y(true));
  expect(off.blocks.some((b) => b.html.includes("<label>"))).toBe(false);
  expect(on.blocks.some((b) => b.html.includes("<label><input"))).toBe(true);
});

test.skipIf(!haveWasm)("real WASM: setComponentTags renders an allowlisted tag as a Component block", () => {
  // Guards the recently-churned setComponentTags serde path specifically.
  const { blocks } = parseAll("<Thinking>\n\nhi **there**\n\n</Thinking>\n", (p) =>
    p.setComponentTags(["Thinking"]),
  );
  const comp = blocks.find((b) => b.kind.type === "Component");
  expect(comp).toBeDefined();
  expect(comp.kind.data?.tag).toBe("Thinking");
});

const TABLE_MD = "| **A** | B |\n|:--|:-:|\n| x | [y](z) |\n";

test.skipIf(!haveWasm)("real WASM: a Table carries structured kind.data when setBlockData is on", () => {
  // Guards the new setBlockData serde path across the real boundary: the
  // {headers,rows,aligns} of {text,html} cells must survive serde_wasm_bindgen.
  const { blocks } = parseAll(TABLE_MD, (p) => p.setBlockData(true));
  const table = blocks.find((b) => b.kind.type === "Table");
  expect(table).toBeDefined();
  const d = table.kind.data;
  expect(d).toBeDefined();
  // headers: text is inline-STRIPPED plaintext, html is inline-rendered display.
  expect(d.headers.map((c: { text: string }) => c.text)).toEqual(["A", "B"]);
  expect(d.headers[0].html).toBe("<strong>A</strong>");
  // aligns straight from the delimiter row.
  expect(d.aligns).toEqual(["left", "center"]);
  // rows: plaintext for logic; html for display (the link's full attrs preserved).
  expect(d.rows.length).toBe(1);
  expect(d.rows[0].map((c: { text: string }) => c.text)).toEqual(["x", "y"]);
  expect(d.rows[0][1].html).toContain('<a href="z"');
});

test.skipIf(!haveWasm)("real WASM: WITHOUT setBlockData a Table has no kind.data (byte-identical-off tripwire)", () => {
  // The default-off contract across the real serde boundary: `kind` is exactly
  // `{type:"Table"}` — no `data` key — so a non-user pays zero serde bytes.
  const { blocks } = parseAll(TABLE_MD);
  const table = blocks.find((b) => b.kind.type === "Table");
  expect(table).toBeDefined();
  expect(table.kind.data).toBeUndefined();
  expect(Object.keys(table.kind)).toEqual(["type"]);
});

const HEADING_MD = "## **Bold** & plain\n";

test.skipIf(!haveWasm)("real WASM: a Heading carries structured kind.data when setBlockData is on", () => {
  // Guards the Heading enrichment serde path across the real boundary: the
  // { level, text(plaintext), id(slug) } object must survive serde_wasm_bindgen.
  const { blocks } = parseAll(HEADING_MD, (p) => p.setBlockData(true));
  const h = blocks.find((b) => b.kind.type === "Heading");
  expect(h).toBeDefined();
  const d = h.kind.data;
  expect(d).toBeDefined();
  // data is the OBJECT (not the bare level) when on.
  expect(typeof d).toBe("object");
  expect(d.level).toBe(2);
  // text is inline-STRIPPED plaintext (the **bold** markup is gone).
  expect(d.text).toBe("Bold & plain");
  // id is the github-style slug of that plaintext.
  expect(d.id).toBe("bold-plain");
  // display html still carries the markup (data is additive, not a replacement).
  expect(h.html).toContain("<strong>Bold</strong>");
});

test.skipIf(!haveWasm)("real WASM: WITHOUT setBlockData a Heading's kind.data is the bare level int (byte-identical-off tripwire)", () => {
  // The default-off contract: `kind.data` is the naked level number, exactly as
  // before the carrier — a non-user sees no behavior change.
  const { blocks } = parseAll(HEADING_MD);
  const h = blocks.find((b) => b.kind.type === "Heading");
  expect(h).toBeDefined();
  expect(h.kind.data).toBe(2);
  expect(Object.keys(h.kind)).toEqual(["type", "data"]);
});

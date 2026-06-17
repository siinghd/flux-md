import { test, expect, beforeAll } from "bun:test";
import { existsSync } from "node:fs";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";

// Worker-free, synchronous server / static rendering (flux-md/server). Requires
// the compiled WASM (built by `bun run build:wasm`); skips when absent.
const wasmUrl = new URL("../src/wasm/flux_md_core_bg.wasm", import.meta.url);
const haveWasm = existsSync(wasmUrl);

// eslint-disable-next-line @typescript-eslint/no-explicit-any
let server: any;
beforeAll(async () => {
  if (!haveWasm) return;
  const mod = "../src/server"; // variable specifier: resolved at runtime, not collection
  server = await import(mod);
  await server.initFlux(); // Node path: reads the co-located .wasm off disk
});

test.skipIf(!haveWasm)("renderToString: worker-free sync HTML string", () => {
  const html = server.renderToString("# Title\n\nHello **world**\n");
  expect(html).toContain("<h1");
  expect(html).toContain("<strong>world</strong>");
  expect(server.isFluxReady()).toBe(true);
});

test.skipIf(!haveWasm)("renderToString: inline component tags emit a real element in the HTML string", () => {
  const html = server.renderToString('Buy <tik symbol="AAPL">A</tik> now\n', {
    config: { inlineComponentTags: ["tik"] },
  });
  expect(html).toContain('<tik symbol="AAPL">A</tik>');
});

test.skipIf(!haveWasm)("renderToString: a block component tag used inline does not eat the following table (P1)", () => {
  const html = server.renderToString("<tik>AAPL</tik> is up.\n\n| a |\n| --- |\n| 1 |\n", {
    config: { componentTags: ["tik"] },
  });
  expect(html).toContain("<table>");
  expect(html).toContain("is up.");
});

test.skipIf(!haveWasm)("FluxMarkdownStatic: emits the flux-md root and dispatches inline components", () => {
  const out = renderToStaticMarkup(
    createElement(server.FluxMarkdownStatic, {
      content: 'Buy <tik symbol="AAPL">**A**</tik> now\n',
      config: { inlineComponentTags: ["tik"] },
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      components: { tik: (p: any) => createElement("span", { className: "chip" }, p.children) },
    }),
  );
  expect(out).toContain('class="flux-md"');
  expect(out).toContain('<span class="chip"><strong>A</strong></span>');
});

test.skipIf(!haveWasm)("FluxMarkdownStatic: a block component override receives parsed children (P2)", () => {
  const out = renderToStaticMarkup(
    createElement(server.FluxMarkdownStatic, {
      content: "<Note>\nhello **world**\n</Note>\n",
      config: { componentTags: ["Note"] },
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      components: { Note: (p: any) => createElement("aside", { className: "note" }, p.children) },
    }),
  );
  expect(out).toContain('<aside class="note">');
  expect(out).toContain("<strong>world</strong>");
});

test.skipIf(!haveWasm)("FluxMarkdownStatic: no components → byte-identical innerHTML wrapper", () => {
  const out = renderToStaticMarkup(createElement(server.FluxMarkdownStatic, { content: "hi\n" }));
  expect(out).toBe('<div class="flux-md"><div class="flux-block flux-block-paragraph"><p>hi</p></div></div>');
});

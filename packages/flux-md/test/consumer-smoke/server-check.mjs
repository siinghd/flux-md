// Node ESM resolution + worker-FREE render against the BUILT, PACKED dist.
//
// Run with cwd = a throwaway consumer dir whose node_modules/ contains the
// extracted flux-md tarball plus react/react-dom/scheduler symlinks (run.sh sets
// this up). Proves: the published exports map resolves under Node native ESM,
// the `.js`-extensioned relative imports load, the wasm reads off disk via
// node:fs (no Worker, no fetch), and both the string + RSC render paths work.
import assert from "node:assert/strict";

const m = await import("flux-md/server");

for (const k of ["initFlux", "initFluxSync", "isFluxReady", "parseToBlocks", "renderToString"]) {
  assert.ok(typeof m[k] !== "undefined", `flux-md/server missing export: ${k}`);
}
// The React component moved to flux-md/server/react so the core entry stays
// React-free; assert it is NOT on the bare server entry.
assert.equal(typeof m.FluxMarkdownStatic, "undefined", "flux-md/server must not export the React FluxMarkdownStatic");
console.log("ok  - flux-md/server resolved via dist; exports:", Object.keys(m).join(", "));

await m.initFlux();                                  // reads ./wasm/*.wasm via node:fs — no Worker
assert.ok(m.isFluxReady(), "initFlux() did not mark the core ready");
console.log("ok  - initFlux() loaded wasm with no Worker");

const html = m.renderToString("# Hello **world**\n\n- a\n- b\n\n```js\nconst x = 1;\n```");
assert.match(html, /<h1[^>]*>/, "renderToString: no <h1>");
assert.match(html, /<strong[^>]*>world<\/strong>/, "renderToString: no inline emphasis");
assert.match(html, /<ul/, "renderToString: no list");
console.log("ok  - renderToString:", JSON.stringify(html.slice(0, 60)));

const mr = await import("flux-md/server/react");
assert.equal(typeof mr.FluxMarkdownStatic, "function", "flux-md/server/react missing FluxMarkdownStatic");
const { createElement } = await import("react");
const { renderToString } = await import("react-dom/server");
const rsc = renderToString(createElement(mr.FluxMarkdownStatic, { content: "## sub *em*" }));
assert.match(rsc, /<h2[^>]*>/, "FluxMarkdownStatic: no <h2>");
assert.match(rsc, /<em[^>]*>em<\/em>/, "FluxMarkdownStatic: no emphasis");
assert.match(rsc, /flux-md/, "FluxMarkdownStatic: no flux-md root class");
console.log("ok  - flux-md/server/react FluxMarkdownStatic (RSC):", JSON.stringify(rsc.slice(0, 60)));

console.log("\nNODE-ESM SERVER + RSC PATH OK (real exports map + built dist + wasm fs-read)");

// SSR cold-import tripwire — run in a FRESH process (`bun test/ssr-cold.mjs`),
// NOT under `bun test`. The in-process SSR suite (ssr.test.ts) can pass trivially
// because a sibling happy-dom suite imports the modules first, so bun's warm
// module cache means a re-import never re-runs top-level code. This script is the
// load-bearing proof: it strips every browser global, then cold-imports each
// entrypoint (top-level code runs for real) and exercises construct + render.
// A future top-level browser-global deref fails CI here, not silently in prod.
//
// Exit non-zero on any failure so CI catches it.

for (const g of [
  "window", "document", "navigator", "self", "Worker", "HTMLElement",
  "customElements", "requestAnimationFrame", "cancelAnimationFrame", "MutationObserver",
]) {
  delete globalThis[g];
}

let failures = 0;
const ok = (m) => console.log("  ok  " + m);
const fail = (m, e) => { failures++; console.error("  FAIL " + m + ": " + (e?.message || e)); };

// 1) Every public entrypoint imports with no browser env (top-level runs cold).
const entrypoints = [
  "../src/index.ts", "../src/client.ts", "../src/react.tsx", "../src/dom.ts",
  "../src/element.ts", "../src/vue.ts", "../src/svelte.ts", "../src/solid.tsx",
  "../src/hi.ts", "../src/html-to-react.ts", "../src/types.ts",
];
for (const ep of entrypoints) {
  try { await import(ep); ok("import " + ep); } catch (e) { fail("import " + ep, e); }
}

// 2) The original repro: new FluxClient() must not create a Worker on the server.
try {
  const { FluxClient } = await import("../src/index.ts");
  const c = new FluxClient();
  if (c.getSnapshot().length !== 0) throw new Error("expected empty snapshot");
  if (c.ready !== false) throw new Error("expected ready === false before first op");
  c.destroy(); // never-acquired client: no pool slot to free
  ok("new FluxClient() + getSnapshot() + destroy() (no Worker)");
} catch (e) { fail("new FluxClient()", e); }

// 3) React SSR of both modes renders the stable empty placeholder (hydrates clean).
try {
  const { createElement } = await import("react");
  const { renderToString } = await import("react-dom/server");
  const { FluxMarkdown } = await import("../src/react.tsx");
  const { FluxClient } = await import("../src/index.ts");
  const fromClient = renderToString(createElement(FluxMarkdown, { client: new FluxClient() }));
  async function* gen() { yield "# hi"; }
  const fromStream = renderToString(createElement(FluxMarkdown, { stream: gen() }));
  if (!fromClient.includes("flux-md")) throw new Error("client mode markup unexpected: " + fromClient);
  if (!fromStream.includes("flux-md")) throw new Error("stream mode markup unexpected: " + fromStream);
  ok("renderToString <FluxMarkdown client> + <FluxMarkdown stream>");
} catch (e) { fail("React renderToString", e); }

if (failures > 0) {
  console.error(`\nSSR cold-import tripwire: ${failures} failure(s)`);
  process.exit(1);
}
console.log("\nSSR cold-import tripwire: PASS (entrypoints + new FluxClient + renderToString, zero browser globals)");

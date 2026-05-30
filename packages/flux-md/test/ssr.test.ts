import { test, expect } from "bun:test";
import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { FluxClient } from "../src/client";
import { FluxMarkdown } from "../src/react";

/**
 * SSR-safety harness.
 *
 * What this proves: importing every public entrypoint, constructing a
 * `FluxClient`, and server-rendering each framework adapter does NOT throw when
 * no browser globals exist (`window`/`document`/`Worker`/…all undefined) — i.e.
 * flux-md is safe to load and render during server-side rendering. The original
 * bug was `new FluxClient()` eagerly calling `pool.acquire()` →
 * `new Worker(...)` in its constructor, throwing "Worker is not defined" on the
 * server (and during a React SSR render pass via `useFluxStream`'s
 * `useState(() => new FluxClient(...))` initializer). Plus a render-time
 * `document.createElement` in the Solid adapter.
 *
 * ## The bun shared-process subtlety (why we delete globals)
 *
 * `bun test` runs ALL test files in ONE process with ONE shared `globalThis`.
 * The sibling suites (react-stream, vue, solid, svelte, element, dom,
 * wasm-integration) install `window`/`document`/`navigator`/`HTMLElement`/… in
 * their `beforeAll` and never delete them. So by the time this file runs, a
 * leaked `document`/`Worker` may already be present — which would make a naive
 * "is it SSR?" check false. We therefore explicitly snapshot + DELETE the
 * relevant globals around each assertion (so `typeof document === "undefined"`
 * is genuinely true, exactly as on a real server), and restore them byte-exactly
 * in a `finally` — including restoring "was absent" as a delete, not as
 * `undefined` — so we never corrupt a sibling suite's leaked state (file order
 * under bun is not guaranteed).
 *
 * ## Module-cache caveat
 *
 * ESM modules evaluate once per process. A module a sibling suite already
 * imported is cached, so re-importing it under the server env will NOT re-run
 * its top-level code. That is fine: the import-safety assertion targets
 * top-level evaluation, which for these modules is import-safe regardless of env
 * (no top-level browser-global deref — cold-verified in a fresh process during
 * development). The construct/render assertions exercise CALL-TIME paths, which
 * read the live (now-deleted) globals — so the server env wraps the
 * construct/render calls, the load-bearing part.
 */

const BROWSER_GLOBALS = [
  "window",
  "document",
  "navigator",
  "Worker",
  "HTMLElement",
  "customElements",
  "requestAnimationFrame",
  "cancelAnimationFrame",
] as const;

function saveAndDelete(): { saved: Record<string, unknown>; had: Record<string, boolean> } {
  const g = globalThis as Record<string, unknown>;
  const saved: Record<string, unknown> = {};
  const had: Record<string, boolean> = {};
  for (const k of BROWSER_GLOBALS) {
    had[k] = k in g;
    saved[k] = g[k];
    delete g[k]; // make typeof === "undefined" even if a sibling suite leaked it
  }
  return { saved, had };
}

function restore(saved: Record<string, unknown>, had: Record<string, boolean>): void {
  const g = globalThis as Record<string, unknown>;
  for (const k of BROWSER_GLOBALS) {
    if (had[k]) g[k] = saved[k];
    else delete g[k]; // restore "was absent" as a delete, not as undefined
  }
}

/** Run `fn` with every browser global deleted; restore exactly afterward. */
function withServerEnv<T>(fn: () => T): T {
  const { saved, had } = saveAndDelete();
  try {
    return fn();
  } finally {
    restore(saved, had);
  }
}

/** Async sibling: the `finally` must run after the promise settles, not when the
 *  sync `fn` returns — so we `await` inside the try. */
async function withServerEnvAsync<T>(fn: () => Promise<T>): Promise<T> {
  const { saved, had } = saveAndDelete();
  try {
    return await fn();
  } finally {
    restore(saved, had);
  }
}

// --------------------------------------------------------------------------
// 1. Import-safety: every entrypoint loads without a browser env.
// --------------------------------------------------------------------------

test("every entrypoint imports without a browser env", async () => {
  await withServerEnvAsync(async () => {
    // Mirrors package.json `exports` exactly: ".", "./client", "./react",
    // "./dom", "./element", "./vue", "./svelte", "./solid", "./highlight"→hi,
    // "./types". A throw fails the test. (Under bun's module cache these may hit
    // a warm copy from a sibling suite; the load-bearing cold verification was
    // done in a fresh process during development. This stands as regression
    // documentation that a *first* server import would be safe.)
    await import("../src/index");
    await import("../src/client");
    await import("../src/react");
    await import("../src/dom");
    await import("../src/element");
    await import("../src/vue");
    await import("../src/svelte");
    await import("../src/solid");
    await import("../src/hi");
    await import("../src/types");
    // NOTE: do NOT import ../src/worker — it is intentionally out of the server
    // graph (worker-asset ref only); importing it directly is out of scope.
  });
});

// --------------------------------------------------------------------------
// 2. new FluxClient() is construct-safe + worker-free derived reads.
// --------------------------------------------------------------------------

test("new FluxClient() does not throw on the server and getSnapshot() === []", () => {
  withServerEnv(() => {
    const c = new FluxClient(); // MUST NOT throw "Worker is not defined"
    expect(c.getSnapshot()).toEqual([]);
    expect(c.getSnapshot()).toBe(c.getSnapshot()); // stable reference (no per-call alloc)
    expect(c.ready).toBe(false); // never-acquired status probe is safe (no acquire)
    expect(c.outline()).toEqual([]); // worker-free derived reads
    expect(c.toPlaintext()).toBe("");
    // subscribe is worker-free: registering + unsubscribing touches no Worker.
    const unsub = c.subscribe(() => {});
    unsub();
    // A never-acquired client must tear down without releasing a phantom slot.
    c.destroy();
  });
});

// --------------------------------------------------------------------------
// 3. React renderToString — both <FluxMarkdown client/> and <FluxMarkdown stream/>.
//    The stream path is the ORIGINAL repro: useFluxStream's
//    `useState(() => new FluxClient(...))` initializer runs during the render
//    pass and used to throw "Worker is not defined".
// --------------------------------------------------------------------------

test("React renderToString(<FluxMarkdown client/>) is worker-free and stable", () => {
  withServerEnv(() => {
    const client = new FluxClient(); // construct-safe (FILE 1)
    const html1 = renderToString(createElement(FluxMarkdown, { client }));
    const html2 = renderToString(createElement(FluxMarkdown, { client }));
    expect(html1).toBe(html2); // stable (empty snapshot both times)
    expect(html1).toContain('class="flux-md"');
    client.destroy(); // never-acquired destroy: no throw, no phantom release
  });
});

test("React renderToString(<FluxMarkdown stream/>) does not throw", () => {
  withServerEnv(() => {
    async function* gen() {
      yield "# hi\n";
    }
    // The async generator is never consumed during SSR (pipeFrom only runs in
    // the client-side effect), so no worker is created — empty snapshot → empty
    // root. This is the exact server-render path that used to crash.
    const html = renderToString(createElement(FluxMarkdown, { stream: gen() }));
    expect(html).toContain('class="flux-md"');
  });
});

// --------------------------------------------------------------------------
// 4. Vue server render.
// --------------------------------------------------------------------------

test("Vue renderToString of <FluxMarkdown> does not throw on the server", async () => {
  await withServerEnvAsync(async () => {
    const { createSSRApp } = await import("vue");
    const { renderToString: renderVue } = await import("vue/server-renderer");
    const adapter = await import("../src/vue");
    const client = new FluxClient();
    // setup() only does ref(null) + lifecycle registration (no DOM); onMounted
    // never fires on the server → emits an empty <div>.
    const html = await renderVue(createSSRApp(adapter.FluxMarkdown, { client }));
    expect(typeof html).toBe("string");
    expect(html).toContain("<div");
  });
});

// --------------------------------------------------------------------------
// 5. Solid server render — the FILE 2 regression guard.
//    Calling the component body under the server env must return the server
//    placeholder (undefined) and NOT throw "document is not defined". A body
//    call is deterministic and directly exercises line 66's typeof guard,
//    without wiring up solid-js/web's SSR runtime.
// --------------------------------------------------------------------------

test("Solid FluxMarkdown body is SSR-safe (returns server placeholder, no throw)", async () => {
  await withServerEnvAsync(async () => {
    const { FluxMarkdown: SolidFlux } = await import("../src/solid");
    const client = new FluxClient();
    let result: unknown;
    expect(() => {
      result = SolidFlux({ client });
    }).not.toThrow();
    expect(result).toBeUndefined(); // server placeholder (the FILE 2 guard)
  });
});

// --------------------------------------------------------------------------
// 6. Svelte — the adapter is a `use:` action, never invoked during Svelte SSR.
//    There is no component to render; the meaningful assertion is that the
//    module is server-safe and the action export exists but is not auto-run.
// --------------------------------------------------------------------------

test("Svelte action module is server-safe (action not invoked during SSR)", async () => {
  await withServerEnvAsync(async () => {
    const mod = await import("../src/svelte");
    expect(typeof mod.fluxMarkdown).toBe("function"); // defined, never auto-run
  });
});

// --------------------------------------------------------------------------
// 7. Hydration parity: the server snapshot must equal the client's INITIAL
//    snapshot so React/Vue/Solid markup matches on hydration. Both are the
//    stable empty [] from emptyBlockStore() — produced WITHOUT constructing a
//    worker (lazy acquire), so they match even on a client with no Worker yet.
// --------------------------------------------------------------------------

test("server snapshot equals the initial client snapshot (hydration parity)", () => {
  const serverSnap = withServerEnv(() => new FluxClient().getSnapshot());
  // Client side: no append → never acquires → no Worker needed; initial snapshot
  // is the same stable empty [].
  const clientSnap = new FluxClient().getSnapshot();
  expect(serverSnap).toEqual([]);
  expect(clientSnap).toEqual([]);
  expect(serverSnap).toEqual(clientSnap); // identical → no hydration mismatch
});

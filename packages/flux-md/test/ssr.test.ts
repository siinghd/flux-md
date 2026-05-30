import { test, expect } from "bun:test";
import { createElement } from "react";
import { renderToString } from "react-dom/server";
import { FluxClient } from "../src/client";
import { FluxMarkdown } from "../src/react";

/**
 * SSR-safety harness (in-process).
 *
 * What this proves: importing the flux entrypoints, constructing a `FluxClient`,
 * and React-server-rendering each `<FluxMarkdown>` mode does NOT throw when no
 * browser globals exist (`window`/`document`/`Worker`/… all undefined). The
 * original bug was `new FluxClient()` eagerly calling `pool.acquire()` →
 * `new Worker(...)` in its constructor, throwing "Worker is not defined" on the
 * server (and during a React SSR render via `useFluxStream`'s
 * `useState(() => new FluxClient(...))` initializer).
 *
 * ## Why no Vue/Solid/Svelte here — and why they live in `test:ssr-cold`
 *
 * `bun test` runs ALL files in ONE process with ONE `globalThis`. `@vue/runtime-dom`
 * (and peers) capture `const doc = typeof document !== "undefined" ? document : null`
 * ONCE at module load. If this file imported `vue` while `document` is deleted, it
 * would lock that cache to `null` for the WHOLE process — poisoning a sibling suite
 * (e.g. vue.test.ts) that mounts a real component later. So this file imports only
 * React (which does not cache `document` for server rendering) + the flux modules
 * (which guard every global). The cross-framework SSR proof runs in a dedicated
 * FRESH process (`bun test/ssr-cold.mjs`, the `test:ssr-cold` script) where importing
 * a framework under a stripped env can poison nothing.
 *
 * ## The shared-process subtlety (why we delete globals)
 *
 * A sibling suite may have leaked `document`/`Worker` into `globalThis`. We snapshot
 * + DELETE the relevant globals around each assertion (so `typeof document ===
 * "undefined"` is genuinely true, as on a real server) and restore them byte-exactly
 * in `finally` — restoring "was absent" as a delete, not `undefined` — so we never
 * corrupt a sibling suite's state (file order under bun is not guaranteed).
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
// 1. Import-safety: the flux entrypoints load without a browser env. (Framework
//    runtimes are intentionally excluded — see the header note; they are
//    cold-imported in a fresh process by test:ssr-cold.) Under bun's module
//    cache a sibling suite may have warm-loaded these; the load-bearing cold
//    proof is test:ssr-cold. This stands as in-process regression documentation.
// --------------------------------------------------------------------------

test("flux entrypoints import without a browser env", async () => {
  await withServerEnvAsync(async () => {
    await import("../src/index");
    await import("../src/client");
    await import("../src/react");
    await import("../src/dom");
    await import("../src/element");
    await import("../src/hi");
    await import("../src/types");
    // NOTE: do NOT import ../src/worker — it is intentionally out of the server
    // graph (worker-asset ref only). Vue/Solid/Svelte runtimes are cold-imported
    // by test:ssr-cold in a fresh process to avoid poisoning the shared process.
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
    const client = new FluxClient(); // construct-safe (lazy acquire)
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
// 4. Hydration parity: the server snapshot must equal the client's INITIAL
//    snapshot so the markup matches on hydration. Both are the stable empty []
//    from emptyBlockStore() — produced WITHOUT constructing a worker (lazy
//    acquire), so they match even on a client with no Worker yet.
// --------------------------------------------------------------------------

test("server snapshot equals the initial client snapshot (hydration parity)", () => {
  const serverSnap = withServerEnv(() => new FluxClient().getSnapshot());
  const clientSnap = new FluxClient().getSnapshot();
  expect(serverSnap).toEqual([]);
  expect(clientSnap).toEqual([]);
  expect(serverSnap).toEqual(clientSnap); // identical → no hydration mismatch
});

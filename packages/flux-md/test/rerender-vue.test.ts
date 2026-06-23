import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// Mirror of test/vue.test.ts's load-order dance: `@vue/runtime-dom` captures
// `document` at MODULE LOAD, and static imports hoist above beforeAll. We must
// register a live DOM FIRST, then DYNAMICALLY import vue + the adapter so
// runtime-dom captures the real document. Type-only imports above are erased.
//
// rAF is shimmed SYNCHRONOUS returning 0 (same reasoning as vue.test.ts): the
// FluxMarkdown component has no `batch` prop, so dom.ts batches via rAF and only
// re-arms when `frame === 0`; returning 0 flushes every patch synchronously
// inside `subscribe`.
let vue: typeof import("vue");
let adapter: typeof import("../src/vue");

beforeAll(async () => {
  const win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.window = win;
  for (const k of [
    "document", "HTMLElement", "Element", "Node", "Text", "Comment",
    "DocumentFragment", "ShadowRoot", "SVGElement", "MathMLElement",
  ]) {
    g[k] = (win as unknown as Record<string, unknown>)[k];
  }
  g.navigator = win.navigator;
  g.requestAnimationFrame = (cb: FrameRequestCallback) => {
    cb(0);
    return 0;
  };
  g.cancelAnimationFrame = () => {};

  vue = await import("vue");
  adapter = await import("../src/vue");
});

// Synchronous fake worker (same pattern as vue.test.ts / dom.test.ts).
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

const para = (id: number, html: string, open = false): Block => ({
  id, kind: { type: "Paragraph" }, start: 0, end: html.length, html, open, speculative: false,
});

function drive(worker: () => FakeWorker, msg: FromWorker) {
  worker().fire(msg);
}

// --------------------------------------------------------------------------
// MOUNT-ONCE (vue-no-mountcount)
//
// src/vue.ts is a THIN lifecycle glue over mountFluxMarkdown: it mounts once on
// onMounted and only remounts when one of the watched identities
// (client/components/sanitize/virtualize/stickToBottom) actually changes. An
// UNRELATED reactive value churning in the PARENT must NOT trigger a remount —
// the renderer (dom.ts keyed reconcile) and the store (client.ts applyPatch:
// committed blocks keep their object ref) own all diffing; the Vue layer must
// not tear down and rebuild the .flux-md root on every parent re-render.
//
// We mount FluxMarkdown inside a wrapper that owns a counter ref it bumps M
// times (each bump forces a parent re-render). The .flux-md root captured right
// after mount must be the SAME node every time, and the streamed content must
// survive untouched.
// --------------------------------------------------------------------------
test("vue-no-mountcount: unrelated parent re-renders never remount the renderer root", async () => {
  const { client, worker } = makeClient();
  client.append(""); // force worker creation so we can fire patches at it

  const host = document.createElement("div");

  // The wrapper owns an UNRELATED reactive value (counter); bumping it forces
  // the parent to re-render, but the client/components/etc identities passed to
  // FluxMarkdown never change.
  let bump: (() => void) | null = null;
  let readCounter: (() => number) | null = null;
  const wrapper = vue.defineComponent({
    setup() {
      const counter = vue.ref(0);
      bump = () => {
        counter.value += 1;
      };
      readCounter = () => counter.value;
      // Reference counter in the render fn so each bump genuinely re-renders the
      // parent; FluxMarkdown's props stay identical across renders.
      return () =>
        vue.h("div", { "data-count": counter.value }, [
          vue.h(adapter.FluxMarkdown, { client }),
        ]);
    },
  });

  const app = vue.createApp(wrapper);
  app.mount(host);

  // Stream content in, then capture the renderer root that owns it.
  drive(worker, patch([para(1, "<p>persisted</p>")], []));
  const root = host.querySelector(".flux-md");
  expect(root).not.toBeNull();
  expect(root!.textContent).toBe("persisted");
  expect(root!.children.length).toBe(1);

  // Bump the unrelated reactive value M times, each time forcing a parent
  // re-render via nextTick. mountFluxMarkdown must have run exactly once: the
  // SAME root node must persist (no teardown/remount on unrelated re-render).
  const M = 5;
  for (let i = 0; i < M; i++) {
    bump!();
    await vue.nextTick();
    // Parent actually re-rendered (the unrelated value propagated).
    expect((host.firstElementChild as HTMLElement).getAttribute("data-count")).toBe(String(i + 1));
    expect(readCounter!()).toBe(i + 1);
    // Same physical root node every time → mounted once, never rebuilt.
    expect(host.querySelector(".flux-md")).toBe(root);
  }

  // Content still present/correct after all the re-renders (committed block kept
  // its node; nothing was torn down).
  expect(host.querySelector(".flux-md")).toBe(root);
  expect(root!.textContent).toBe("persisted");
  expect(root!.children.length).toBe(1);

  // And the live stream still flows into the SAME root after the churn — a patch
  // after the re-renders lands without a remount.
  drive(worker, patch([para(2, "<p>after churn</p>")], []));
  expect(host.querySelector(".flux-md")).toBe(root);
  expect(root!.textContent).toBe("persistedafter churn");
  expect(root!.children.length).toBe(2);

  app.unmount();
});

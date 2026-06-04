import { test, expect, beforeAll, spyOn } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";
import type { DomComponents } from "../src/dom";

// `@vue/runtime-dom` captures `const doc = typeof document !== "undefined" ?
// document : null` at MODULE LOAD. Static imports are hoisted above beforeAll,
// so a statically-imported vue (directly or via ../src/vue) locks `doc` to null
// before our DOM exists. We therefore (1) register the DOM in beforeAll, then
// (2) dynamically import vue AND the adapter so runtime-dom captures the live
// document. Type-only imports above are erased and trigger no runtime eval.
//
// We also install a SYNCHRONOUS requestAnimationFrame that returns 0: the
// FluxMarkdown component has no `batch` prop, so the renderer batches via rAF.
// dom.ts re-arms only when `frame === 0`, and `flush()` resets it to 0 — so the
// shim MUST return 0, else the renderer never schedules a second flush.
// Returning 0 makes every patch flush synchronously inside `subscribe`.
let vue: typeof import("vue");
let adapter: typeof import("../src/vue");

beforeAll(async () => {
  const win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.window = win;
  // Vue's runtime-dom probes these constructors during mount/unmount
  // (normalizeContainer → ShadowRoot, resolveRootNamespace → SVGElement,
  // patchProp → Element/MathMLElement, etc.); the renderer needs document +
  // HTMLElement + Node.
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

  // DOM is live now → runtime-dom's load-time `doc` capture sees the real document.
  vue = await import("vue");
  adapter = await import("../src/vue");
});

// Synchronous fake worker (same pattern as dom.test.ts / pool.test.ts).
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

test("mounting FluxMarkdown renders a .flux-md root and a later patch lands in it", () => {
  const { client, worker } = makeClient();
  client.append(""); // force worker creation so we can fire at it
  const host = document.createElement("div");
  const app = vue.createApp(adapter.FluxMarkdown, { client });
  app.mount(host);

  // The component renders one <div> whose ref is the mount container; the
  // renderer appends its .flux-md root inside it on mount.
  const root = host.querySelector(".flux-md");
  expect(root).not.toBeNull();

  // A patch fired after mount flushes synchronously (rAF shim) into the root.
  drive(worker, patch([para(1, "<p>hello vue</p>")], []));
  expect(root!.children.length).toBe(1);
  expect(root!.textContent).toBe("hello vue");

  app.unmount();
});

test("changing the components prop identity remounts the renderer root", async () => {
  const { client, worker } = makeClient();
  client.append("");
  const host = document.createElement("div");

  const compsA: DomComponents = {};
  let setComps: ((c: DomComponents) => void) | null = null;
  const wrapper = vue.defineComponent({
    setup() {
      const comps = vue.ref<DomComponents>(compsA);
      setComps = (c) => {
        comps.value = c;
      };
      return () => vue.h(adapter.FluxMarkdown, { client, components: comps.value });
    },
  });
  const app = vue.createApp(wrapper);
  app.mount(host);

  drive(worker, patch([para(1, "<p>before</p>")], []));
  const before = host.querySelector(".flux-md");
  expect(before).not.toBeNull();
  expect(before!.textContent).toBe("before");

  // New components object identity → destroy + remount → a NEW root element.
  setComps!({ Paragraph: (p) => `<div class="mine">${p.html}</div>` });
  await vue.nextTick();

  const after = host.querySelector(".flux-md");
  expect(after).not.toBeNull();
  expect(after).not.toBe(before); // remounted: fresh root element
  // Remount's initial sync() repopulates from the snapshot synchronously, and
  // the new override is in effect for the existing block.
  expect(after!.querySelector(".mine")).not.toBeNull();
  expect(after!.textContent).toBe("before");

  app.unmount();
});

test("unmount tears down the renderer and NEVER calls client.destroy()", () => {
  const { client, worker } = makeClient();
  client.append("");
  const host = document.createElement("div");
  const destroySpy = spyOn(client, "destroy");

  const app = vue.createApp(adapter.FluxMarkdown, { client });
  app.mount(host);
  drive(worker, patch([para(1, "<p>live</p>")], []));
  expect(host.querySelector(".flux-md")).not.toBeNull();

  app.unmount(); // synchronous → onUnmounted runs → handle.destroy()
  // Ownership invariant: the adapter only ever calls handle.destroy(); the
  // caller owns the worker/stream, so client.destroy() must NOT be called.
  expect(destroySpy).not.toHaveBeenCalled();
  // handle.destroy() removed the renderer root.
  expect(host.querySelector(".flux-md")).toBeNull();

  // A patch after unmount must not resurrect or mutate anything.
  drive(worker, patch([para(2, "<p>after</p>")], []));
  expect(host.querySelector(".flux-md")).toBeNull();

  destroySpy.mockRestore();
});

// --------------------------------------------------------------------------
// useFluxMarkdownString — the controlled-string composable.
//
// It OWNS an internally-constructed FluxClient on the process-wide default pool
// (no pool injection), so the FakeWorker harness above can't drive it and a real
// setContent would spawn a real bun Worker. We therefore prove the WIRING only —
// the setContent diff/finalize semantics are client.ts's job and already tested.
// We stub FluxClient.prototype.setContent with a NO-OP (so nothing touches a
// Worker) and spy on destroy, then assert the composable calls them correctly.
// A prototype spy intercepts the composable's instance because the test imports
// FluxClient from the same module the composable imports.
// --------------------------------------------------------------------------

// Mount a wrapper that drives the composable through reactive refs, exposing them
// so a test can mutate content/streaming and observe the resulting setContent
// calls. Mirrors the "changing the components prop" wrapper pattern above.
function mountStringComposable(initial: { content: string; streaming?: boolean }) {
  const host = document.createElement("div");
  const content = vue.ref(initial.content);
  const streaming = vue.ref<boolean | undefined>(initial.streaming);
  const wrapper = vue.defineComponent({
    setup() {
      adapter.useFluxMarkdownString(
        () => content.value,
        () => ({ streaming: streaming.value }),
      );
      return () => vue.h("div");
    },
  });
  const app = vue.createApp(wrapper);
  app.mount(host);
  return { app, content, streaming };
}

test("useFluxMarkdownString feeds content on mount (done=false when streaming omitted)", () => {
  const setContentSpy = spyOn(FluxClient.prototype, "setContent").mockImplementation(() => {});
  const destroySpy = spyOn(FluxClient.prototype, "destroy");

  const { app } = mountStringComposable({ content: "# hi" });

  // onMounted ran → one feed, with the controlled string and done:false (streaming
  // omitted must NOT finalize — that is the O(n²) reparse trap).
  expect(setContentSpy).toHaveBeenCalledTimes(1);
  expect(setContentSpy).toHaveBeenLastCalledWith("# hi", { done: false });

  app.unmount();
  // The composable OWNS its client (unlike useFluxMarkdown) → it destroys it.
  expect(destroySpy).toHaveBeenCalledTimes(1);

  setContentSpy.mockRestore();
  destroySpy.mockRestore();
});

test("useFluxMarkdownString re-feeds on content growth and finalizes when streaming flips false", async () => {
  const setContentSpy = spyOn(FluxClient.prototype, "setContent").mockImplementation(() => {});
  const destroySpy = spyOn(FluxClient.prototype, "destroy");

  const { app, content, streaming } = mountStringComposable({ content: "# hi", streaming: true });

  expect(setContentSpy).toHaveBeenCalledTimes(1);
  expect(setContentSpy).toHaveBeenLastCalledWith("# hi", { done: false });

  // Content grows → non-immediate watch fires → another feed, still open.
  content.value = "# hi\nmore";
  await vue.nextTick();
  expect(setContentSpy).toHaveBeenCalledTimes(2);
  expect(setContentSpy).toHaveBeenLastCalledWith("# hi\nmore", { done: false });

  // streaming flips to false → watch fires → finalize via done:true.
  streaming.value = false;
  await vue.nextTick();
  expect(setContentSpy).toHaveBeenCalledTimes(3);
  expect(setContentSpy).toHaveBeenLastCalledWith("# hi\nmore", { done: true });

  app.unmount();
  expect(destroySpy).toHaveBeenCalledTimes(1);

  setContentSpy.mockRestore();
  destroySpy.mockRestore();
});

test("useFluxMarkdownString does NOT feed during SSR (setContent untouched on the server)", async () => {
  const setContentSpy = spyOn(FluxClient.prototype, "setContent").mockImplementation(() => {});
  const { renderToString } = await import("vue/server-renderer");

  const wrapper = vue.defineComponent({
    setup() {
      // setup() runs on the server: constructs the client (worker-free) but the
      // non-immediate watch and onMounted never fire there, so setContent — the
      // only worker-spawning call — must not be invoked.
      adapter.useFluxMarkdownString(
        () => "# server",
        () => ({ streaming: false }),
      );
      return () => vue.h("div");
    },
  });

  const html = await renderToString(vue.createSSRApp(wrapper));
  expect(typeof html).toBe("string");
  expect(setContentSpy).not.toHaveBeenCalled();

  setContentSpy.mockRestore();
});

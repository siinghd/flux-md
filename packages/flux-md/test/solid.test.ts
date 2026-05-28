import { test, expect, beforeAll, spyOn } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import { mountSolid, type FluxMarkdownProps } from "../src/solid";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// Register a DOM the same way test/dom.test.ts does (no GlobalRegistrator subpath
// in happy-dom 15.x). We deliberately do NOT install requestAnimationFrame, so
// the renderer's default batch falls to synchronous sync; we also pass
// `batch: false` explicitly to be safe.
beforeAll(() => {
  const win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  g.navigator = win.navigator;
});

// Synchronous fake worker (same pattern as pool.test.ts / dom.test.ts): records
// posts and lets the test fire patch responses through the registered listener.
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

// A cleanup collector standing in for Solid's onCleanup at the call site.
function makeCleanups() {
  const fns: Array<() => void> = [];
  return { register: (fn: () => void) => fns.push(fn), run: () => fns.forEach((f) => f()) };
}

test("mountSolid renders the client snapshot into the container", () => {
  const { client, worker } = makeClient();
  client.append(""); // force worker creation so we can fire at it
  const container = document.createElement("div");
  const cleanups = makeCleanups();

  const props: FluxMarkdownProps = { client, batch: false };
  mountSolid(() => props, container, cleanups.register);

  worker().fire(patch([para(1, "<p>hello</p>")], [para(2, "<p>tail", true)]));
  const root = container.querySelector(".flux-md")!;
  expect(root).not.toBeNull();
  expect(Array.from(root.children).map((c) => c.textContent)).toEqual(["hello", "tail"]);
});

test("forwards MountOptions (stickToBottom/virtualize) through to the renderer", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const cleanups = makeCleanups();

  const props: FluxMarkdownProps = { client, batch: false, stickToBottom: true, virtualize: true };
  mountSolid(() => props, container, cleanups.register);

  worker().fire(patch([para(1, "<p>a</p>")], [para(2, "<p>b", true)]));
  const root = container.querySelector(".flux-md")!;
  // stickToBottom: a scroll-snap anchor is pinned last.
  const last = root.children[root.children.length - 1] as HTMLElement;
  expect(last.className).toContain("flux-bottom-anchor");
  expect(last.style.scrollSnapAlign).toBe("end");
  // virtualize: the closed block gets content-visibility; the open tail does not.
  const closed = root.children[0] as HTMLElement;
  expect(closed.style.contentVisibility).toBe("auto");
});

test("cleanup runs handle.destroy (root removed) and never calls client.destroy", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const cleanups = makeCleanups();

  // Ownership invariant: the adapter must never tear down the client.
  const destroySpy = spyOn(client, "destroy");

  const props: FluxMarkdownProps = { client, batch: false };
  const handle = mountSolid(() => props, container, cleanups.register);
  const handleDestroySpy = spyOn(handle, "destroy");

  worker().fire(patch([para(1, "<p>before</p>")], []));
  expect(container.querySelector(".flux-md")).not.toBeNull();

  cleanups.run();

  // handle.destroy ran exactly once...
  expect(handleDestroySpy).toHaveBeenCalledTimes(1);
  // ...the renderer root is genuinely gone (spyOn calls through)...
  expect(container.querySelector(".flux-md")).toBeNull();
  // ...and the client was never destroyed by the adapter.
  expect(destroySpy).not.toHaveBeenCalled();

  // A later patch after cleanup must not resurrect or mutate the DOM.
  worker().fire(patch([para(2, "<p>after</p>")], []));
  expect(container.querySelector(".flux-md")).toBeNull();
  expect(container.children.length).toBe(0);
});

test("props are snapshotted once at mount (getProps read a single time)", () => {
  const { client } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const cleanups = makeCleanups();

  let reads = 0;
  const props: FluxMarkdownProps = { client, batch: false };
  mountSolid(() => { reads++; return props; }, container, cleanups.register);

  expect(reads).toBe(1);
});

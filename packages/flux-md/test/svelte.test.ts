import { test, expect, beforeAll, spyOn } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import { fluxMarkdown } from "../src/svelte";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// Register a DOM for this file. Mirror dom.test.ts exactly: deliberately do NOT
// install requestAnimationFrame, so mountFluxMarkdown's default batch collapses
// to synchronous sync() (dom.ts: `batch && typeof requestAnimationFrame ===
// "function"`) and a fired patch lands in the DOM immediately, no rAF flake.
beforeAll(() => {
  const win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  g.navigator = win.navigator;
});

// Synchronous fake worker (same pattern as pool.test.ts / dom.test.ts).
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

test("action mounts and streams patches into the host node", () => {
  const { client, worker } = makeClient();
  client.append(""); // force worker creation so we can fire at it
  const node = document.createElement("div");
  const action = fluxMarkdown(node, { client });

  drive(worker, patch([para(1, "<p>hello</p>")], []));
  const root = node.querySelector(".flux-md")!;
  expect(root).not.toBeNull();
  expect(root.textContent).toContain("hello");

  action.destroy!();
});

test("update with identical field values does NOT remount", () => {
  const { client, worker } = makeClient();
  client.append("");
  const node = document.createElement("div");
  const action = fluxMarkdown(node, { client });

  drive(worker, patch([para(1, "<p>first</p>")], []));
  const root = node.firstChild; // the .flux-md root created by this mount
  expect((root as HTMLElement).className).toBe("flux-md");

  // A fresh object literal with the SAME client (and absent options compare
  // undefined === undefined) must early-return: no destroy, same root element.
  action.update!({ client });
  expect(node.firstChild).toBe(root); // same root → no remount

  // The original mount is still live: a later patch still lands.
  drive(worker, patch([para(2, "<p>second</p>")], []));
  expect((root as HTMLElement).textContent).toContain("second");

  action.destroy!();
});

test("update with a changed client remounts", () => {
  const a = makeClient();
  a.client.append("");
  const node = document.createElement("div");
  const action = fluxMarkdown(node, { client: a.client });

  drive(a.worker, patch([para(1, "<p>from-a</p>")], []));
  const firstRoot = node.firstChild;
  expect((firstRoot as HTMLElement).textContent).toContain("from-a");

  // Different client → remount: old root destroyed, a new .flux-md takes over.
  const b = makeClient();
  b.client.append("");
  action.update!({ client: b.client });
  const secondRoot = node.firstChild;
  expect(secondRoot).not.toBe(firstRoot); // remounted

  drive(b.worker, patch([para(1, "<p>from-b</p>")], []));
  expect((secondRoot as HTMLElement).textContent).toContain("from-b");
  // Exactly one mount is live (old root was removed).
  expect(node.querySelectorAll(".flux-md").length).toBe(1);

  action.destroy!();
});

test("destroy tears down the mount and NEVER calls client.destroy (ownership invariant)", () => {
  const { client, worker } = makeClient();
  client.append("");
  const destroySpy = spyOn(client, "destroy");
  const node = document.createElement("div");
  const action = fluxMarkdown(node, { client });

  drive(worker, patch([para(1, "<p>x</p>")], []));
  expect(node.querySelector(".flux-md")).not.toBeNull();

  action.destroy!();
  // Mount torn down: its root is gone.
  expect(node.querySelector(".flux-md")).toBeNull();
  // The caller owns the client — the action must never destroy it.
  expect(destroySpy).not.toHaveBeenCalled();

  // A later patch must not resurrect the DOM.
  drive(worker, patch([para(2, "<p>after</p>")], []));
  expect(node.querySelector(".flux-md")).toBeNull();
});

test("remount on changed client also never destroys either client", () => {
  const a = makeClient();
  a.client.append("");
  const b = makeClient();
  b.client.append("");
  const aSpy = spyOn(a.client, "destroy");
  const bSpy = spyOn(b.client, "destroy");
  const node = document.createElement("div");
  const action = fluxMarkdown(node, { client: a.client });

  action.update!({ client: b.client }); // remount swaps clients
  action.destroy!();

  expect(aSpy).not.toHaveBeenCalled();
  expect(bSpy).not.toHaveBeenCalled();
});

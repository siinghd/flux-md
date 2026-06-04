import { test, expect, beforeAll, afterEach, spyOn } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import { fluxMarkdown, fluxMarkdownString } from "../src/svelte";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// A no-op Worker stub for the fluxMarkdownString tests below. Those tests use
// the action's OWNED client, which joins the DEFAULT pool — whose factory calls
// `new Worker(new URL("./worker.ts", import.meta.url))`. The first worker-bound
// op (setContent → append/finalize) would otherwise spawn a real WASM worker.
// The `new URL(...)` arg is harmless: this fake just records the construction.
// The caller-owned fluxMarkdown tests inject their own FluxPool, so this stub
// never affects them.
class FakeDefaultWorker {
  static instances: FakeDefaultWorker[] = [];
  constructor(..._args: unknown[]) {
    FakeDefaultWorker.instances.push(this);
  }
  postMessage() {}
  addEventListener() {}
  removeEventListener() {}
  terminate() {}
}

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
  g.Worker = FakeDefaultWorker as unknown as typeof Worker;
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

// --------------------------------------------------------------------------
// fluxMarkdownString — controlled-string action that OWNS its client.
// --------------------------------------------------------------------------

afterEach(() => {
  FakeDefaultWorker.instances = [];
});

test("string action: create constructs a client and feeds content done=false when streaming omitted", () => {
  const setContentSpy = spyOn(FluxClient.prototype, "setContent");
  try {
    const node = document.createElement("div");
    const action = fluxMarkdownString(node, { content: "# hi" });

    // mountFluxMarkdown is worker-free (getSnapshot + subscribe), so the root
    // mounts immediately.
    expect(node.querySelector(".flux-md")).not.toBeNull();

    // Exactly one setContent on create; stream left OPEN (done:false) because
    // `streaming` was omitted — done is NOT inferred from the absent flag.
    expect(setContentSpy).toHaveBeenCalledTimes(1);
    expect(setContentSpy.mock.calls[0]).toEqual(["# hi", { done: false }]);

    action.destroy!();
  } finally {
    setContentSpy.mockRestore();
  }
});

test("string action: streaming:false finalizes (done=true)", () => {
  const setContentSpy = spyOn(FluxClient.prototype, "setContent");
  try {
    const node = document.createElement("div");
    const action = fluxMarkdownString(node, { content: "done text", streaming: false });

    expect(setContentSpy.mock.calls[0]).toEqual(["done text", { done: true }]);

    action.destroy!();
  } finally {
    setContentSpy.mockRestore();
  }
});

test("string action: update re-feeds content on EVERY update (no early-return)", () => {
  const setContentSpy = spyOn(FluxClient.prototype, "setContent");
  try {
    const node = document.createElement("div");
    const action = fluxMarkdownString(node, { content: "a", streaming: true });
    expect(setContentSpy.mock.calls[0]).toEqual(["a", { done: false }]);

    action.update!({ content: "ab", streaming: true });
    expect(setContentSpy.mock.calls[1]).toEqual(["ab", { done: false }]);

    action.update!({ content: "abc", streaming: false });
    expect(setContentSpy.mock.calls[2]).toEqual(["abc", { done: true }]);

    expect(setContentSpy).toHaveBeenCalledTimes(3);

    action.destroy!();
  } finally {
    setContentSpy.mockRestore();
  }
});

test("string action: a mount-option change reuses the SAME client (baseline survives)", () => {
  const origSet = FluxClient.prototype.setContent;
  const seen = new Set<FluxClient>();
  const setContentSpy = spyOn(FluxClient.prototype, "setContent").mockImplementation(function (
    this: FluxClient,
    content: string,
    opts?: { done?: boolean },
  ) {
    seen.add(this);
    return origSet.call(this, content, opts);
  });
  try {
    const node = document.createElement("div");
    const action = fluxMarkdownString(node, { content: "x", components: {} });
    const firstRoot = node.firstChild;

    // Change a mount-option identity (a fresh components object) → remount.
    action.update!({ content: "x", components: {} });
    const secondRoot = node.firstChild;

    expect(secondRoot).not.toBe(firstRoot); // remounted
    expect(node.querySelectorAll(".flux-md").length).toBe(1); // exactly one live mount

    // The SAME client served both mounts — only one receiver ever.
    expect(seen.size).toBe(1);

    action.destroy!();
  } finally {
    setContentSpy.mockRestore();
  }
});

test("string action: identical mount-option identities do NOT remount", () => {
  const node = document.createElement("div");
  const components = {};
  const action = fluxMarkdownString(node, { content: "y", components });
  const root = node.firstChild;

  // Same components identity, only content changed → no remount, same root.
  action.update!({ content: "yy", components });
  expect(node.firstChild).toBe(root);

  action.destroy!();
});

test("string action: destroy OWNS the client — tears down mount AND destroys client (inverse of fluxMarkdown)", () => {
  const destroySpy = spyOn(FluxClient.prototype, "destroy");
  try {
    const node = document.createElement("div");
    const action = fluxMarkdownString(node, { content: "z", streaming: false });
    expect(node.querySelector(".flux-md")).not.toBeNull();

    action.destroy!();

    expect(node.querySelector(".flux-md")).toBeNull(); // mount torn down
    // OWNS the client → MUST destroy it (inverse of the caller-owned action).
    expect(destroySpy).toHaveBeenCalledTimes(1);
  } finally {
    destroySpy.mockRestore();
  }
});

test("string action: empty content + streaming omitted touches no Worker (setContent no-ops)", () => {
  const node = document.createElement("div");
  // content "" === client.lastContent and streaming omitted → setContent body
  // short-circuits (no append, no finalize) → no Worker is ever spawned.
  const action = fluxMarkdownString(node, { content: "" });
  expect(node.querySelector(".flux-md")).not.toBeNull();
  expect(FakeDefaultWorker.instances.length).toBe(0);
  action.destroy!();
});

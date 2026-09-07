import { test, expect, beforeAll, afterEach, spyOn } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { BrookClient, BrookPool } from "../src/client";
import { brookMarkdown, brookMarkdownString, tailBlockId } from "../src/svelte";
import { tailOpenBlockId } from "../src/dom";
import type { Block, FromWorker, LinkClickInfo, ToWorker, WorkerLike } from "../src/types";

// A no-op Worker stub for the brookMarkdownString tests below. Those tests use
// the action's OWNED client, which joins the DEFAULT pool — whose factory calls
// `new Worker(new URL("./worker.ts", import.meta.url))`. The first worker-bound
// op (setContent → append/finalize) would otherwise spawn a real WASM worker.
// The `new URL(...)` arg is harmless: this fake just records the construction.
// The caller-owned brookMarkdown tests inject their own BrookPool, so this stub
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
// install requestAnimationFrame, so mountBrookMarkdown's default batch collapses
// to synchronous sync() (dom.ts: `batch && typeof requestAnimationFrame ===
// "function"`) and a fired patch lands in the DOM immediately, no rAF flake.
beforeAll(() => {
  const win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  g.navigator = win.navigator;
  // Constructible click events for the delegated onLinkClick test.
  g.MouseEvent = win.MouseEvent;
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
  const pool = new BrookPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 1);
  const client = new BrookClient({ pool });
  return { client, worker: () => created[0] };
}

function patch(committed: Block[], active: Block[], streamId = 1): FromWorker {
  return {
    type: "patch",
    streamId,
    patch: JSON.stringify({ newly_committed: committed, active }),
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
  const action = brookMarkdown(node, { client });

  drive(worker, patch([para(1, "<p>hello</p>")], []));
  const root = node.querySelector(".brook-md")!;
  expect(root).not.toBeNull();
  expect(root.textContent).toContain("hello");

  action.destroy!();
});

test("update with identical field values does NOT remount", () => {
  const { client, worker } = makeClient();
  client.append("");
  const node = document.createElement("div");
  const action = brookMarkdown(node, { client });

  drive(worker, patch([para(1, "<p>first</p>")], []));
  const root = node.firstChild; // the .brook-md root created by this mount
  expect((root as HTMLElement).className).toBe("brook-md");

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
  const action = brookMarkdown(node, { client: a.client });

  drive(a.worker, patch([para(1, "<p>from-a</p>")], []));
  const firstRoot = node.firstChild;
  expect((firstRoot as HTMLElement).textContent).toContain("from-a");

  // Different client → remount: old root destroyed, a new .brook-md takes over.
  const b = makeClient();
  b.client.append("");
  action.update!({ client: b.client });
  const secondRoot = node.firstChild;
  expect(secondRoot).not.toBe(firstRoot); // remounted

  drive(b.worker, patch([para(1, "<p>from-b</p>")], []));
  expect((secondRoot as HTMLElement).textContent).toContain("from-b");
  // Exactly one mount is live (old root was removed).
  expect(node.querySelectorAll(".brook-md").length).toBe(1);

  action.destroy!();
});

test("destroy tears down the mount and NEVER calls client.destroy (ownership invariant)", () => {
  const { client, worker } = makeClient();
  client.append("");
  const destroySpy = spyOn(client, "destroy");
  const node = document.createElement("div");
  const action = brookMarkdown(node, { client });

  drive(worker, patch([para(1, "<p>x</p>")], []));
  expect(node.querySelector(".brook-md")).not.toBeNull();

  action.destroy!();
  // Mount torn down: its root is gone.
  expect(node.querySelector(".brook-md")).toBeNull();
  // The caller owns the client — the action must never destroy it.
  expect(destroySpy).not.toHaveBeenCalled();

  // A later patch must not resurrect the DOM.
  drive(worker, patch([para(2, "<p>after</p>")], []));
  expect(node.querySelector(".brook-md")).toBeNull();
});

test("remount on changed client also never destroys either client", () => {
  const a = makeClient();
  a.client.append("");
  const b = makeClient();
  b.client.append("");
  const aSpy = spyOn(a.client, "destroy");
  const bSpy = spyOn(b.client, "destroy");
  const node = document.createElement("div");
  const action = brookMarkdown(node, { client: a.client });

  action.update!({ client: b.client }); // remount swaps clients
  action.destroy!();

  expect(aSpy).not.toHaveBeenCalled();
  expect(bSpy).not.toHaveBeenCalled();
});

// --------------------------------------------------------------------------
// TAIL-BINDING (svelte-tail-finegrain)
// --------------------------------------------------------------------------

test("action: committed-block nodes keep identity across patches; only the open tail rebuilds", () => {
  const { client, worker } = makeClient();
  client.append("");
  const node = document.createElement("div");
  const action = brookMarkdown(node, { client });
  const root = node.querySelector(".brook-md")!;

  // Commit block 1, open block 2 as the tail.
  drive(worker, patch([para(1, "<p>committed</p>")], [para(2, "<p>t", true)]));
  const committedNode = root.children[0];
  const tailV1 = root.children[1];

  for (const t of ["<p>ta", "<p>tail</p>"]) {
    drive(worker, patch([], [para(2, t, true)]));
    expect(root.children[0]).toBe(committedNode); // committed: identity held
  }
  expect(root.children[1]).not.toBe(tailV1); // tail: rebuilt

  drive(worker, patch([para(2, "<p>tail final</p>")], []));
  expect(root.children[0]).toBe(committedNode);

  action.destroy!();
});

test("tailBlockId store tracks the open tail, stays stable on pure growth, stops on last unsubscribe", () => {
  const { client, worker } = makeClient();
  client.append("");
  const store = tailBlockId(client);

  const seen: Array<number | null> = [];
  const unsub = store.subscribe((v) => seen.push(v));
  expect(seen[seen.length - 1]).toBe(null); // initial: nothing open

  drive(worker, patch([para(1, "<p>c</p>")], [para(2, "<p>t", true)]));
  expect(seen[seen.length - 1]).toBe(2);

  // Pure tail growth keeps id 2 → Svelte's set is identity-checked, so NO new
  // emission lands (length unchanged).
  const lenBefore = seen.length;
  drive(worker, patch([], [para(2, "<p>tail more</p>", true)]));
  expect(seen.length).toBe(lenBefore); // stable id → no re-fire
  expect(tailOpenBlockId(client.getSnapshot())).toBe(2);

  // Commit the tail → emits null.
  drive(worker, patch([para(2, "<p>tail final</p>")], []));
  expect(seen[seen.length - 1]).toBe(null);

  // Last unsubscribe runs the readable stop fn (client.subscribe unsubscribed):
  // a later patch no longer pushes into a (re)subscribed store.
  unsub();
  const reSeen: Array<number | null> = [];
  const unsub2 = store.subscribe((v) => reSeen.push(v));
  // Fresh subscriber gets the current value once...
  expect(reSeen).toEqual([null]);
  drive(worker, patch([], [para(3, "<p>after", true)]));
  // ...and a re-subscribed store tracks again (stop/restart is transparent).
  expect(reSeen[reSeen.length - 1]).toBe(3);
  unsub2();
});

// --------------------------------------------------------------------------
// brookMarkdownString — controlled-string action that OWNS its client.
// --------------------------------------------------------------------------

afterEach(() => {
  FakeDefaultWorker.instances = [];
});

test("string action: create constructs a client and feeds content done=false when streaming omitted", () => {
  const setContentSpy = spyOn(BrookClient.prototype, "setContent");
  try {
    const node = document.createElement("div");
    const action = brookMarkdownString(node, { content: "# hi" });

    // mountBrookMarkdown is worker-free (getSnapshot + subscribe), so the root
    // mounts immediately.
    expect(node.querySelector(".brook-md")).not.toBeNull();

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
  const setContentSpy = spyOn(BrookClient.prototype, "setContent");
  try {
    const node = document.createElement("div");
    const action = brookMarkdownString(node, { content: "done text", streaming: false });

    expect(setContentSpy.mock.calls[0]).toEqual(["done text", { done: true }]);

    action.destroy!();
  } finally {
    setContentSpy.mockRestore();
  }
});

test("string action: update re-feeds content on EVERY update (no early-return)", () => {
  const setContentSpy = spyOn(BrookClient.prototype, "setContent");
  try {
    const node = document.createElement("div");
    const action = brookMarkdownString(node, { content: "a", streaming: true });
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
  const origSet = BrookClient.prototype.setContent;
  const seen = new Set<BrookClient>();
  const setContentSpy = spyOn(BrookClient.prototype, "setContent").mockImplementation(function (
    this: BrookClient,
    content: string,
    opts?: { done?: boolean },
  ) {
    seen.add(this);
    return origSet.call(this, content, opts);
  });
  try {
    const node = document.createElement("div");
    const action = brookMarkdownString(node, { content: "x", components: {} });
    const firstRoot = node.firstChild;

    // Change a mount-option identity (a fresh components object) → remount.
    action.update!({ content: "x", components: {} });
    const secondRoot = node.firstChild;

    expect(secondRoot).not.toBe(firstRoot); // remounted
    expect(node.querySelectorAll(".brook-md").length).toBe(1); // exactly one live mount

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
  const action = brookMarkdownString(node, { content: "y", components });
  const root = node.firstChild;

  // Same components identity, only content changed → no remount, same root.
  action.update!({ content: "yy", components });
  expect(node.firstChild).toBe(root);

  action.destroy!();
});

test("string action: destroy OWNS the client — tears down mount AND destroys client (inverse of brookMarkdown)", () => {
  const destroySpy = spyOn(BrookClient.prototype, "destroy");
  try {
    const node = document.createElement("div");
    const action = brookMarkdownString(node, { content: "z", streaming: false });
    expect(node.querySelector(".brook-md")).not.toBeNull();

    action.destroy!();

    expect(node.querySelector(".brook-md")).toBeNull(); // mount torn down
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
  const action = brookMarkdownString(node, { content: "" });
  expect(node.querySelector(".brook-md")).not.toBeNull();
  expect(FakeDefaultWorker.instances.length).toBe(0);
  action.destroy!();
});

// A settled link (the `data-brook-pending` streaming shape is covered in
// test/link-click.test.tsx, which owns the hook's filtering rules).
const LINK_HTML =
  '<p>See the <a href="https://example.com/q3" target="_blank" rel="noopener noreferrer nofollow">Earnings Call</a> today.</p>';

test("action spreads onLinkClick into the mount, and a changed handler remounts", () => {
  const { client, worker } = makeClient();
  client.append("");
  const node = document.createElement("div");
  const calls: LinkClickInfo[] = [];
  const first = (_e: MouseEvent, link: LinkClickInfo) => {
    calls.push(link);
  };
  const action = brookMarkdown(node, { client, onLinkClick: first });

  drive(worker, patch([para(1, LINK_HTML)], []));
  const a = node.querySelector("a[href]")!;
  a.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  expect(calls.length).toBe(1);
  expect(calls[0].href).toBe("https://example.com/q3");

  // Same handler identity → no remount (the option is part of the compare set).
  const root = node.firstChild;
  action.update!({ client, onLinkClick: first });
  expect(node.firstChild).toBe(root);

  // A NEW handler identity remounts against it.
  const later: LinkClickInfo[] = [];
  action.update!({ client, onLinkClick: (_e, link) => later.push(link) });
  expect(node.firstChild).not.toBe(root);
  node
    .querySelector("a[href]")!
    .dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true }));
  expect(later.length).toBe(1);
  expect(calls.length).toBe(1); // the replaced handler is gone

  action.destroy!();
});

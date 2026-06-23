import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import { defineFluxMarkdown } from "../src/element";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// Regression tests for two behavior-preserving fixes in src/element.ts:
//   #1 attributeChangedCallback early-returns when oldValue === newValue, so a
//      same-value setAttribute no longer tears down + reparses the client.
//   #2 `set components` / `set sanitize` early-return when value === current, so
//      a same-identity re-assign no longer remounts the renderer.
// Plus a node-stability check that a committed block's DOM node survives across
// later tail-growing patches (the keyed-reconcile reuse contract).
//
// This MIRRORS test/element.test.ts: the FakeWorker / CapturingWorker harness,
// the default-pool snapshot/recover dance, the external-client helper, the
// `patch`/`para` builders, and the `getClient()` identity hook the element
// exposes. Rendering is synchronous because beforeAll deliberately leaves
// requestAnimationFrame undefined (dom.ts falls to the sync path).

// The `<flux-markdown>` custom element's public surface.
type FluxEl = HTMLElement & {
  client?: FluxClient;
  components?: unknown;
  sanitize?: unknown;
  append(chunk: string): void;
  finalize(): void;
  getClient(): FluxClient | null;
};

// Synchronous fake worker (same pattern as element.test.ts / dom.test.ts).
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

// Workers the element's INTERNAL (self-owned) client creates via the default
// pool route through `new Worker(...)`. Capture every constructed fake so a test
// can drive the stream the element opened.
const defaultPoolWorkers: FakeWorker[] = [];
class CapturingWorker extends FakeWorker {
  constructor() {
    super();
    defaultPoolWorkers.push(this);
  }
}

beforeAll(() => {
  const win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  g.navigator = win.navigator;
  g.customElements = win.customElements;
  // No requestAnimationFrame: mountFluxMarkdown falls to synchronous sync, so
  // patches render immediately in tests.
  g.Worker = CapturingWorker as unknown;
  defineFluxMarkdown();
});

function patch(committed: Block[], active: Block[], streamId: number): FromWorker {
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

// External-client harness: an isolated FakeWorker-backed pool (like dom.test).
function makeExternalClient() {
  const created: FakeWorker[] = [];
  const pool = new FluxPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 1);
  const client = new FluxClient({ pool });
  return { client, worker: () => created[0] };
}

test("ATTR-NOOP: a same-value config setAttribute does not rebuild the self-owned client (fix #1)", () => {
  // Assert via the element's own client/root identity rather than default-pool
  // worker traffic: a self-owned client lands on the process-wide default pool,
  // which is warm across the suite, so `sent[]`-counting is order-dependent and
  // brittle. getClient()/root identity is the robust signal — and exactly what
  // fix #1 controls: a no-op config change must NOT tear the client down.
  const el = document.createElement("flux-markdown") as FluxEl;
  el.setAttribute("gfm-math", "true");
  el.setAttribute("markdown", "x"); // a persistent content source to re-resolve
  document.body.appendChild(el); // connect → self-owned client + mount

  const clientBefore = el.getClient();
  const rootBefore = el.querySelector(".flux-md");
  expect(clientBefore).not.toBeNull();
  expect(rootBefore).not.toBeNull();

  // No-op set: gfm-math is already "true" → attributeChangedCallback early-returns
  // (oldValue === newValue), so no teardown, no remount.
  el.setAttribute("gfm-math", "true");
  el.setAttribute("gfm-math", "true"); // twice for good measure

  expect(el.getClient()).toBe(clientBefore); // SAME client identity (no rebuild)
  expect(el.querySelector(".flux-md")).toBe(rootBefore); // SAME root node (no remount)

  // REAL change: the genuine-change path still rebuilds the client.
  el.setAttribute("gfm-math", "false");
  expect(el.getClient()).not.toBe(clientBefore); // client WAS rebuilt
  expect(el.getClient()).not.toBeNull();

  el.remove();
});

test("PROP-NOOP: re-assigning the same components/sanitize identity does not remount; a new identity does (fix #2)", () => {
  const { client, worker } = makeExternalClient();
  client.append(""); // force worker creation
  const sid = worker().sent[0].streamId;

  const el = document.createElement("flux-markdown") as FluxEl;
  el.client = client; // caller-owned
  document.body.appendChild(el); // connect → mount

  const components = { Paragraph: () => "<p>over</p>" };
  const sanitize = (h: string) => h;
  // First assignment is a real change (was undefined) → remount. Capture the
  // root AFTER it so subsequent same-identity assigns are pure no-ops.
  el.components = components as unknown as FluxEl["components"];
  el.sanitize = sanitize as unknown as FluxEl["sanitize"];

  worker().fire(patch([para(1, "<p>hello</p>")], [], sid));
  const rootBefore = el.querySelector(".flux-md");
  expect(rootBefore).not.toBeNull();

  // Re-assign the SAME object/fn identities several times: must not remount.
  el.components = components as unknown as FluxEl["components"];
  el.sanitize = sanitize as unknown as FluxEl["sanitize"];
  el.components = components as unknown as FluxEl["components"];
  el.sanitize = sanitize as unknown as FluxEl["sanitize"];
  el.components = components as unknown as FluxEl["components"];

  expect(el.querySelector(".flux-md")).toBe(rootBefore); // SAME root node (no remount)

  // A NEW components identity is a real change → remount (root replaced).
  const components2 = { Paragraph: () => "<p>over2</p>" };
  el.components = components2 as unknown as FluxEl["components"];
  const rootAfter = el.querySelector(".flux-md");
  expect(rootAfter).not.toBeNull();
  expect(rootAfter).not.toBe(rootBefore); // remount DID happen on a real change

  el.remove();
});

test("NODE-SURVIVES-PATCHES: a committed block's DOM node is reused across every later tail-growing patch", () => {
  const { client, worker } = makeExternalClient();
  client.append(""); // force worker creation
  const sid = worker().sent[0].streamId;

  const el = document.createElement("flux-markdown") as FluxEl;
  el.client = client;
  document.body.appendChild(el);

  // Commit block #1, with an active (open) tail.
  worker().fire(patch([para(1, "<p>committed one</p>")], [para(2, "<p>tail</p>", true)], sid));
  const root = el.querySelector(".flux-md")!;
  const committedNode = root.children[0];
  expect(committedNode).toBeTruthy();
  expect(committedNode.textContent).toContain("committed one");

  // Grow the tail across several patches: the committed block #1 is never
  // re-sent, so its node reference must stay identical the whole time.
  worker().fire(patch([], [para(2, "<p>tail growing</p>", true)], sid));
  expect(root.children[0]).toBe(committedNode);

  worker().fire(patch([], [para(2, "<p>tail growing more</p>", true)], sid));
  expect(root.children[0]).toBe(committedNode);

  // Commit #2 and open a new tail #3 — #1 still untouched.
  worker().fire(patch([para(2, "<p>tail final</p>")], [para(3, "<p>new tail</p>", true)], sid));
  expect(root.children[0]).toBe(committedNode);
  expect(committedNode.textContent).toContain("committed one");

  worker().fire(patch([], [para(3, "<p>new tail bigger</p>", true)], sid));
  expect(root.children[0]).toBe(committedNode);

  el.remove();
});

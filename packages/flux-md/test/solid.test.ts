import { test, expect, beforeAll, spyOn } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import {
  createFluxMarkdownString,
  mountSolid,
  setupFluxMarkdownString,
  setupTailBlockId,
  type FluxMarkdownProps,
} from "../src/solid";
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

test("mountSolid: committed-block nodes keep identity across patches; only the open tail rebuilds", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const cleanups = makeCleanups();

  const props: FluxMarkdownProps = { client, batch: false };
  mountSolid(() => props, container, cleanups.register);
  const root = container.querySelector(".flux-md")!;

  // Commit block 1, open block 2 as the tail.
  worker().fire(patch([para(1, "<p>committed</p>")], [para(2, "<p>t", true)]));
  const committedNode = root.children[0];
  const tailV1 = root.children[1];

  // Several pure tail-growth patches: block 1 is never re-sent, so its node must
  // be the SAME reference every time; the tail node IS rebuilt on growth.
  for (const tail of ["<p>ta", "<p>tai", "<p>tail</p>"]) {
    worker().fire(patch([], [para(2, tail, true)]));
    expect(root.children[0]).toBe(committedNode); // committed: identity held
  }
  expect(root.children[1]).not.toBe(tailV1); // tail: rebuilt

  // Commit the tail → block 1 STILL the same node (committed body never churns).
  worker().fire(patch([para(2, "<p>tail final</p>")], []));
  expect(root.children[0]).toBe(committedNode);

  cleanups.run();
});

test("setupTailBlockId tracks the open tail, no-ops on stable id, unsubscribes on cleanup", () => {
  const { client, worker } = makeClient();
  client.append("");
  const cleanups = makeCleanups();

  const tail = setupTailBlockId(client, cleanups.register);
  expect(tail()).toBe(null); // nothing open yet

  worker().fire(patch([para(1, "<p>c</p>")], [para(2, "<p>t", true)]));
  expect(tail()).toBe(2);

  // Pure tail growth keeps the same open id → the accessor stays === 2.
  worker().fire(patch([], [para(2, "<p>tail more</p>", true)]));
  expect(tail()).toBe(2);

  // Commit the tail → null.
  worker().fire(patch([para(2, "<p>tail final</p>")], []));
  expect(tail()).toBe(null);

  // Cleanup unsubscribes: a later patch no longer moves the accessor.
  cleanups.run();
  worker().fire(patch([], [para(3, "<p>after", true)]));
  expect(tail()).toBe(null);
});

// --------------------------------------------------------------------------
// createFluxMarkdownString — the controlled-string helper.
//
// We test the reactivity-free core `setupFluxMarkdownString` directly (the same
// strategy as `mountSolid`): it takes injected effect/cleanup registrars so the
// test drives them by hand — Solid's `createEffect` is a no-op under bun's
// server build (no client runtime to pump it), so relying on the real one
// firing would test nothing. A captured effect-runner lets us mutate
// content/streaming between runs and observe the resulting setContent calls.
// We spy on FluxClient.prototype.setContent with a NO-OP so nothing spawns a
// real bun Worker; the setContent diff/finalize semantics are client.ts's job
// and tested in setcontent.test.ts.
// --------------------------------------------------------------------------

// Captures the registered effect fn so the test can re-run it after changing
// what the accessors return, plus the cleanup collector from above.
function makeEffectRunner() {
  let effect: (() => void) | null = null;
  return {
    register: (fn: () => void) => { effect = fn; },
    run: () => effect?.(),
  };
}

test("createFluxMarkdownString feeds content on first effect (done=false when streaming omitted)", () => {
  const setContentSpy = spyOn(FluxClient.prototype, "setContent").mockImplementation(() => {});
  const destroySpy = spyOn(FluxClient.prototype, "destroy").mockImplementation(() => {});

  const effects = makeEffectRunner();
  const cleanups = makeCleanups();
  const client = setupFluxMarkdownString(() => "# hi", undefined, effects.register, cleanups.register);

  expect(client).toBeInstanceOf(FluxClient);
  // The body constructs but does not feed — setContent only runs inside the effect.
  expect(setContentSpy).toHaveBeenCalledTimes(0);

  effects.run();
  expect(setContentSpy).toHaveBeenCalledTimes(1);
  // streaming omitted → done:false (never inferred-done from an absent flag).
  expect(setContentSpy).toHaveBeenLastCalledWith("# hi", { done: false });

  setContentSpy.mockRestore();
  destroySpy.mockRestore();
});

test("createFluxMarkdownString re-feeds on content growth and finalizes when streaming flips false", () => {
  const setContentSpy = spyOn(FluxClient.prototype, "setContent").mockImplementation(() => {});
  const destroySpy = spyOn(FluxClient.prototype, "destroy").mockImplementation(() => {});

  let content = "# hi";
  let streaming: boolean | undefined = true;
  const effects = makeEffectRunner();
  const cleanups = makeCleanups();
  setupFluxMarkdownString(
    () => content,
    () => ({ streaming }),
    effects.register,
    cleanups.register,
  );

  // Initial feed: streaming:true → open.
  effects.run();
  expect(setContentSpy).toHaveBeenCalledTimes(1);
  expect(setContentSpy).toHaveBeenLastCalledWith("# hi", { done: false });

  // Content grows → effect re-runs → another feed, still open.
  content = "# hi\nmore";
  effects.run();
  expect(setContentSpy).toHaveBeenCalledTimes(2);
  expect(setContentSpy).toHaveBeenLastCalledWith("# hi\nmore", { done: false });

  // streaming flips to false → finalize via done:true.
  streaming = false;
  effects.run();
  expect(setContentSpy).toHaveBeenCalledTimes(3);
  expect(setContentSpy).toHaveBeenLastCalledWith("# hi\nmore", { done: true });

  setContentSpy.mockRestore();
  destroySpy.mockRestore();
});

test("createFluxMarkdownString reads config ONCE in the body (immutable) and OWNS the client (cleanup destroys)", () => {
  const setContentSpy = spyOn(FluxClient.prototype, "setContent").mockImplementation(() => {});
  const destroySpy = spyOn(FluxClient.prototype, "destroy").mockImplementation(() => {});

  let optionReads = 0;
  const getOptions = () => {
    optionReads++;
    return { config: { gfmMath: true }, streaming: true };
  };
  const effects = makeEffectRunner();
  const cleanups = makeCleanups();
  setupFluxMarkdownString(() => "x", getOptions, effects.register, cleanups.register);

  // Body read getOptions() exactly once (for config) before any effect ran.
  expect(optionReads).toBe(1);

  // The effect reads getOptions() again (for streaming) — so config is NOT a
  // change trigger but streaming IS tracked reactively.
  effects.run();
  expect(optionReads).toBe(2);
  expect(setContentSpy).toHaveBeenLastCalledWith("x", { done: false });

  // Ownership: this helper constructs the client, so cleanup destroys it.
  expect(destroySpy).not.toHaveBeenCalled();
  cleanups.run();
  expect(destroySpy).toHaveBeenCalledTimes(1);

  setContentSpy.mockRestore();
  destroySpy.mockRestore();
});

test("createFluxMarkdownString (public) wires Solid's createEffect/onCleanup", () => {
  // The public entry forwards to the core with Solid's real registrars. Under
  // bun's server build createEffect is a no-op, so we don't assert the feed
  // here (that's the core test above) — we prove the public function constructs
  // a client and returns it without throwing or touching a Worker. setContent is
  // stubbed defensively in case a client runtime ever pumps the effect.
  const setContentSpy = spyOn(FluxClient.prototype, "setContent").mockImplementation(() => {});
  const client = createFluxMarkdownString(() => "# hi", () => ({ streaming: true }));
  expect(client).toBeInstanceOf(FluxClient);
  expect(client.getSnapshot()).toEqual([]); // no patches, no Worker
  expect(client.ready).toBe(false); // never acquired a pool slot
  setContentSpy.mockRestore();
});

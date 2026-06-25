import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import { mountFluxMarkdown, tailOpenBlockId } from "../src/dom";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// Mirror test/dom.test.ts: register a DOM in this file only (no global preload).
// The default mount batch falls to synchronous sync unless requestAnimationFrame
// exists, so the RAF-BATCH test installs a controllable fake before mounting and
// restores globals afterward. The other tests pass `batch: false` explicitly.
beforeAll(() => {
  const win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  g.navigator = win.navigator;
});

// Synchronous fake worker (same pattern as dom.test.ts / pool.test.ts): records
// posts and lets the test fire patch responses back through the listener.
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

function drive(client: FluxClient, worker: () => FakeWorker, msg: FromWorker) {
  worker().fire(msg);
}

// --------------------------------------------------------------------------
// NODE-REUSE
// --------------------------------------------------------------------------

test("NODE-REUSE: committed block node is reused across tail growth; a changed block node is replaced", () => {
  const { client, worker } = makeClient();
  client.append(""); // force worker creation so we can fire at it
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false });
  const root = container.querySelector(".flux-md")!;

  // Commit block 1; open block 2 as the active tail.
  drive(client, worker, patch([para(1, "<p>committed</p>")], [para(2, "<p>t", true)]));
  const committedNode = root.children[0];
  const tailNodeV1 = root.children[1];
  expect(committedNode.outerHTML).toContain("committed");

  // Drive SEVERAL patches that only grow the active tail. Block 1 is never
  // re-sent, so its node must be the SAME reference every time (never rebuilt),
  // while the tail node IS rebuilt as its html changes.
  for (const tail of ["<p>ta", "<p>tai", "<p>tail", "<p>tail grows</p>"]) {
    drive(client, worker, patch([], [para(2, tail, true)]));
    expect(root.children[0]).toBe(committedNode); // committed node: same ref
  }
  expect(root.children[1]).not.toBe(tailNodeV1); // tail node: rebuilt on growth

  // Commit block 2: block 1 is STILL the same node.
  drive(client, worker, patch([para(2, "<p>tail final</p>")], []));
  expect(root.children[0]).toBe(committedNode);

  // A CHANGED block's node IS replaced. Re-open the tail (block 3) then send a
  // NEW patch for the SAME id with different html → its DOM node is swapped.
  drive(client, worker, patch([], [para(3, "<p>x</p>", true)]));
  const changingNode = root.children[2];
  drive(client, worker, patch([], [para(3, "<p>x changed</p>", true)]));
  expect(root.children[2]).not.toBe(changingNode); // changed → node replaced
  expect(root.children[2].textContent).toBe("x changed");

  handle.destroy();
});

// --------------------------------------------------------------------------
// TAIL-BINDING (open-block-id)
//
// The fine-grained "what may re-render next" signal that the framework adapters
// narrow their reactivity to. It is a pure derivation over the snapshot and must
// never change rendered output — the DOM here is identical to the NODE-REUSE
// test, we only additionally read handle.openBlockId().
// --------------------------------------------------------------------------

test("TAIL-BINDING: handle.openBlockId is the open tail id, null when committed, and stable across pure tail growth", () => {
  // Pure-function derivation: tail open id, null otherwise.
  expect(tailOpenBlockId([])).toBe(null);
  expect(tailOpenBlockId([para(1, "<p>a</p>")])).toBe(null); // closed tail
  expect(tailOpenBlockId([para(1, "<p>a</p>"), para(2, "<p>b", true)])).toBe(2); // open tail

  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false });
  const root = container.querySelector(".flux-md")!;

  // No patch yet → nothing open.
  expect(handle.openBlockId()).toBe(null);

  // Commit block 1, open block 2 as the tail → openBlockId is 2.
  drive(client, worker, patch([para(1, "<p>committed</p>")], [para(2, "<p>t", true)]));
  const committedNode = root.children[0];
  expect(handle.openBlockId()).toBe(2);

  // Pure tail growth keeps the SAME open id (2) every tick — the signal is
  // stable, so a fine-grained binding keyed off it never re-fires — while the
  // committed node keeps identity.
  for (const tail of ["<p>ta", "<p>tai", "<p>tail grows</p>"]) {
    drive(client, worker, patch([], [para(2, tail, true)]));
    expect(handle.openBlockId()).toBe(2);
    expect(root.children[0]).toBe(committedNode);
  }

  // Commit the tail → nothing open → null.
  drive(client, worker, patch([para(2, "<p>tail final</p>")], []));
  expect(handle.openBlockId()).toBe(null);
  expect(root.children[0]).toBe(committedNode); // committed body untouched

  handle.destroy();
});

// --------------------------------------------------------------------------
// RECONCILE-MINIMAL (dom-reconcile)
// --------------------------------------------------------------------------

test("RECONCILE-MINIMAL: at most one structural insert per new block, ZERO node moves on pure tail html growth, order preserved", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false });
  const root = container.querySelector(".flux-md")! as HTMLElement;

  // Wrap root.insertBefore and classify each call against the live DOM:
  //   - MOVE  = the node was ALREADY a child of root (a reorder/relocation).
  //   - PLACE = the node was detached (a new block node, or an in-place rebuild
  //             — happy-dom's Element.replaceWith routes through parent.insertBefore
  //             with a fresh node, so we can't see it as a separate API; but a
  //             rebuild does NOT move any *existing* node, which is what matters).
  // The minimality contract: append-only streaming NEVER relocates an existing
  // node, so MOVE must stay 0 forever. New blocks cost at most one PLACE each.
  let moves = 0;
  let places = 0;
  const realInsertBefore = root.insertBefore.bind(root);
  (root as unknown as { insertBefore: Node["insertBefore"] }).insertBefore = ((
    node: Node,
    ref: Node | null,
  ) => {
    if (node.parentNode === root) moves++;
    else places++;
    return realInsertBefore(node, ref);
  }) as Node["insertBefore"];

  // Establish a first tail node (a genuinely-new block → one structural place).
  drive(client, worker, patch([], [para(1, "<p>t", true)]));
  expect(root.children.length).toBe(1);
  expect(moves).toBe(0);
  expect(places).toBeLessThanOrEqual(1);

  // Stream M append-only growth patches on the SAME active tail id. Pure tail
  // html growth must move ZERO existing nodes: the rebuilt node replaces in
  // place (node.replaceWith) and the reconcile cursor matches it (cursor===want),
  // so root.insertBefore is never called to RELOCATE an attached node.
  const movesBeforeGrowth = moves;
  const M = 8;
  for (let i = 0; i < M; i++) {
    drive(client, worker, patch([], [para(1, `<p>tail step ${i}</p>`, true)]));
  }
  expect(moves - movesBeforeGrowth).toBe(0); // ZERO moves on pure tail growth
  expect(root.children.length).toBe(1);

  // Commit the open tail (block 1). Its fingerprint changes (open→closed), so
  // its node legitimately rebuilds once — that is a block CHANGE, not a move.
  drive(client, worker, patch([para(1, "<p>b1</p>")], []));
  expect(root.children.length).toBe(1);
  expect(root.children[0].textContent).toBe("b1");

  // Now append M genuinely-new, already-committed blocks one per patch, touching
  // no existing block. Each new block costs EXACTLY one structural place and
  // relocates ZERO existing nodes — the append-only ideal.
  const placesBeforeNew = places;
  const movesBeforeNew = moves;
  const newIds = [2, 3, 4, 5];
  for (const id of newIds) {
    const placesBefore = places;
    drive(client, worker, patch([para(id, `<p>b${id}</p>`)], []));
    expect(places - placesBefore).toBe(1); // exactly one insert for the new block
  }
  expect(places - placesBeforeNew).toBe(newIds.length); // one per new block, no more
  expect(moves - movesBeforeNew).toBe(0); // existing nodes never relocated

  // One more pure tail-growth burst with all 5 nodes present: still ZERO moves
  // (open a new tail, grow it twice).
  const movesBeforeGrow2 = moves;
  drive(client, worker, patch([], [para(6, "<p>b6", true)]));
  drive(client, worker, patch([], [para(6, "<p>b6 more", true)]));
  drive(client, worker, patch([], [para(6, "<p>b6 grown</p>", true)]));
  expect(moves - movesBeforeGrow2).toBe(0);

  // No existing node was EVER relocated across the whole append-only stream.
  expect(moves).toBe(0);

  // Final DOM order equals the snapshot order.
  const snapOrder = client.getSnapshot().map((b) => b.id);
  const domTexts = Array.from(root.children).map((c) => c.textContent);
  expect(snapOrder).toEqual([1, 2, 3, 4, 5, 6]);
  expect(domTexts).toEqual(["b1", "b2", "b3", "b4", "b5", "b6 grown"]);

  handle.destroy();
});

// --------------------------------------------------------------------------
// RAF-BATCH-COALESCE (raf-batch)
// --------------------------------------------------------------------------

test("RAF-BATCH-COALESCE: 3 patches schedule one frame and the drain reflects the latest snapshot; repeated fire/drain never drops", () => {
  // Controllable requestAnimationFrame: queue callbacks, drain() runs+clears.
  const g = globalThis as Record<string, unknown>;
  const prevRaf = g.requestAnimationFrame;
  const prevCaf = g.cancelAnimationFrame;
  let queued: Array<{ id: number; cb: FrameRequestCallback } | null> = [];
  let nextId = 1;
  let scheduledCount = 0;
  g.requestAnimationFrame = ((cb: FrameRequestCallback) => {
    const id = nextId++;
    queued.push({ id, cb });
    scheduledCount++;
    return id;
  }) as typeof requestAnimationFrame;
  g.cancelAnimationFrame = ((id: number) => {
    queued = queued.map((q) => (q && q.id === id ? null : q));
  }) as typeof cancelAnimationFrame;
  function drain() {
    const pending = queued;
    queued = [];
    for (const q of pending) q?.cb(performance.now());
  }

  try {
    const { client, worker } = makeClient();
    client.append("");
    const container = document.createElement("div");
    // batch defaults to ON; mount AFTER installing fake rAF so it is detected.
    const handle = mountFluxMarkdown(client, container, {});
    const root = container.querySelector(".flux-md")!;

    // The initial sync() ran synchronously at mount (no rAF). Reset the counter
    // so we measure only the patch-driven scheduling.
    scheduledCount = 0;

    // Fire 3 patches BEFORE draining. They must coalesce into exactly ONE
    // scheduled frame, and the DOM must still be empty (nothing flushed yet).
    drive(client, worker, patch([], [para(1, "<p>snap 1</p>", true)]));
    drive(client, worker, patch([], [para(1, "<p>snap 2</p>", true)]));
    drive(client, worker, patch([], [para(1, "<p>snap 3</p>", true)]));
    expect(scheduledCount).toBe(1); // coalesced: one rAF for three patches
    expect(root.children.length).toBe(0); // not flushed before the frame fires

    // Drain the frame: DOM reflects the 3rd (latest) snapshot only.
    drain();
    expect(root.children.length).toBe(1);
    expect(root.children[0].textContent).toBe("snap 3");

    // fire→drain→fire→drain: each drain syncs once, nothing dropped.
    scheduledCount = 0;
    drive(client, worker, patch([para(1, "<p>snap 3</p>")], [para(2, "<p>second open</p>", true)]));
    expect(scheduledCount).toBe(1);
    drain();
    expect(Array.from(root.children).map((c) => c.textContent)).toEqual(["snap 3", "second open"]);

    scheduledCount = 0;
    drive(client, worker, patch([para(2, "<p>second closed</p>")], [para(3, "<p>third</p>", true)]));
    expect(scheduledCount).toBe(1);
    drain();
    expect(Array.from(root.children).map((c) => c.textContent)).toEqual([
      "snap 3",
      "second closed",
      "third",
    ]);

    // A drain with no pending patches is a no-op (nothing scheduled, no change).
    scheduledCount = 0;
    drain();
    expect(scheduledCount).toBe(0);
    expect(root.children.length).toBe(3);

    handle.destroy();
  } finally {
    // Restore globals so the DOM-free / synchronous suites are unaffected.
    if (prevRaf === undefined) delete g.requestAnimationFrame;
    else g.requestAnimationFrame = prevRaf;
    if (prevCaf === undefined) delete g.cancelAnimationFrame;
    else g.cancelAnimationFrame = prevCaf;
  }
});

// --------------------------------------------------------------------------
// RENDER-PROBE (onRenderMetrics)
// --------------------------------------------------------------------------

test("RENDER-PROBE: committed block fires onRenderMetrics once; the rebuilt tail fires per patch", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");

  const samples: { id: number; renderCount: number; toggles: number; kind: string; ms: number }[] = [];
  const handle = mountFluxMarkdown(client, container, {
    batch: false,
    onRenderMetrics: (id, m) =>
      samples.push({ id, renderCount: m.renderCount, toggles: m.speculativeToggleCount, kind: m.kind, ms: m.lastRenderMs }),
  });

  // Commit block 1; open block 2 as the active tail (both build → fire once each).
  drive(client, worker, patch([para(1, "<p>committed</p>")], [para(2, "<p>t", true)]));

  // Grow the active tail several patches. Block 1 is never re-sent, so its node
  // is reused untouched → it must NEVER fire again. The tail rebuilds each patch.
  for (const tail of ["<p>ta", "<p>tai", "<p>tail</p>"]) {
    drive(client, worker, patch([], [para(2, tail, true)]));
  }

  const forId = (id: number) => samples.filter((s) => s.id === id);
  // Committed block fired EXACTLY once (initial build), never on a tail patch.
  expect(forId(1).length).toBe(1);
  expect(forId(1)[0].renderCount).toBe(1);
  expect(forId(1)[0].kind).toBe("Paragraph");
  // The tail fired on initial build + each rebuild (monotonic renderCount).
  expect(forId(2).map((s) => s.renderCount)).toEqual(forId(2).map((_, i) => i + 1));
  expect(forId(2).length).toBeGreaterThan(1);
  expect(Number.isFinite(forId(1)[0].ms)).toBe(true);

  // Aggregate rebuildCount advanced once per actual build/rebuild.
  expect(client.getMetrics().rebuildCount).toBe(samples.length);
  handle.destroy();
});

test("RENDER-PROBE: rebuildCount stays 0 with no hook (zero overhead)", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false });

  drive(client, worker, patch([para(1, "<p>committed</p>")], [para(2, "<p>t", true)]));
  for (const tail of ["<p>ta", "<p>tail</p>"]) {
    drive(client, worker, patch([], [para(2, tail, true)]));
  }
  expect(client.getMetrics().rebuildCount).toBe(0);
  handle.destroy();
});

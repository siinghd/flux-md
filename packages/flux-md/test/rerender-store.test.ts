import { test, expect } from "bun:test";
import { FluxClient, FluxPool, applyPatch, emptyBlockStore } from "../src/client";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// Regression tests for the store contract that prevents extra React re-renders:
// a COMMITTED block keeps the SAME object reference across every later patch, so
// the `blocksEqual` memo (BlockView) and the DOM keyed reconcile short-circuit
// and never rebuild a committed block — only the active tail churns. getSnapshot()
// must also be ref-stable while idle (for useSyncExternalStore) and the client
// must notify subscribers exactly once per patch, and reset() must not fire a
// no-op notify on an already-empty store.
//
// These mirror the sibling harnesses: applyPatch/emptyBlockStore are driven
// directly (client-store.test.ts) for the pure ref-stability contracts; the
// live FluxClient (emit/reset/subscribe) is driven over a FakeWorker through a
// FluxPool exactly as pool.test.ts / setcontent.test.ts do — no real Worker/WASM.

function blk(id: number, html: string, open = false): Block {
  return { id, kind: { type: "Paragraph" }, start: 0, end: html.length, html, open, speculative: false };
}

// A synchronous fake worker: records what was posted to it and lets the test
// fire responses back through the registered listener. No real Worker/WASM.
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

function setup() {
  const created: FakeWorker[] = [];
  const pool = new FluxPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 8);
  return { pool, created, client: new FluxClient({ pool }) };
}

// Wire the worker (lazy acquire) and return the discovered stream id + worker so
// the test can fire patches into the live client the way pool.test.ts does.
function wire(client: FluxClient, created: FakeWorker[]): { sid: number; w: FakeWorker } {
  client.append(""); // first worker-bound op → acquires + reveals the stream id
  const w = created[0];
  const sid = (w.sent[0] as { streamId: number }).streamId;
  return { sid, w };
}

const firePatch = (w: FakeWorker, sid: number, patch: { newly_committed: Block[]; active: Block[] }) =>
  w.fire({
    type: "patch",
    streamId: sid,
    patch,
    appendedBytes: 0,
    parseMicros: 0,
    retainedBytes: 0,
    wasmMemoryBytes: 0,
  });

// --------------------------------------------------------------------------
// COMMITTED-IDENTITY — the core no-re-render contract
// --------------------------------------------------------------------------

test("COMMITTED-IDENTITY: a committed block's reference is identical across every later snapshot; the tail gets a fresh ref when its html changes", () => {
  const store = emptyBlockStore();

  // Patch 1: commit b1, plus an active tail.
  const b1 = blk(1, "<p>first</p>");
  applyPatch(store, { newly_committed: [b1], active: [blk(2, "<p>act", true)] });
  expect(store.snapshot[0]).toBe(b1);
  const tail1 = store.snapshot[1];

  // Patch 2: commit b2; tail grows. b1 must NOT be re-sent and stays identical.
  const b2 = blk(2, "<p>act done</p>");
  applyPatch(store, { newly_committed: [b2], active: [blk(3, "<p>tail", true)] });
  expect(store.snapshot[0]).toBe(b1); // committed b1: SAME ref → memo skips
  expect(store.snapshot[1]).toBe(b2); // committed b2: SAME ref
  const tail2 = store.snapshot[2];

  // Patch 3: tail html changes; b1 and b2 stay frozen.
  applyPatch(store, { newly_committed: [], active: [blk(3, "<p>tail grown</p>", true)] });
  expect(store.snapshot[0]).toBe(b1); // STILL the original b1 object, 2 patches later
  expect(store.snapshot[1]).toBe(b2);
  const tail3 = store.snapshot[2];

  // Patch 4: another no-commit tail churn. Committed refs remain identical.
  applyPatch(store, { newly_committed: [], active: [blk(3, "<p>tail grown more</p>", true)] });
  expect(store.snapshot[0]).toBe(b1);
  expect(store.snapshot[1]).toBe(b2);

  // The active/tail block legitimately gets a FRESH reference each time its html
  // changes — that block (and only that block) must re-render.
  expect(tail2).not.toBe(tail1);
  expect(tail3).not.toBe(tail2);
  expect(store.snapshot[2]).not.toBe(tail3);
});

// --------------------------------------------------------------------------
// GETSNAPSHOT-STABLE — idle ref-stability for useSyncExternalStore
// --------------------------------------------------------------------------

test("GETSNAPSHOT-STABLE: getSnapshot() returns the same ref with no intervening patch, and a fresh ref after a real tail change", () => {
  const { client, created } = setup();
  const { sid, w } = wire(client, created);

  firePatch(w, sid, { newly_committed: [blk(1, "<p>one</p>")], active: [blk(2, "<p>tail", true)] });
  const s = client.getSnapshot();

  // Idle: repeated reads with NO patch must return the IDENTICAL array — else
  // useSyncExternalStore would tear / loop on every render.
  expect(client.getSnapshot()).toBe(s);
  expect(client.getSnapshot()).toBe(s);
  expect(client.getSnapshot()).toBe(s);

  // A genuine tail-changing patch reassigns the cached snapshot → new ref.
  firePatch(w, sid, { newly_committed: [], active: [blk(2, "<p>tail grown</p>", true)] });
  expect(client.getSnapshot()).not.toBe(s);
});

// --------------------------------------------------------------------------
// EMIT-ONCE-PER-PATCH (store-6) — one notify per patch, not per block
// --------------------------------------------------------------------------

test("EMIT-ONCE-PER-PATCH: a single patch with 3 committed blocks + an active tail notifies subscribers exactly once", () => {
  const { client, created } = setup();
  const { sid, w } = wire(client, created);

  let notifies = 0;
  client.subscribe(() => {
    notifies++;
  });

  firePatch(w, sid, {
    newly_committed: [blk(1, "<p>a</p>"), blk(2, "<p>b</p>"), blk(3, "<p>c</p>")],
    active: [blk(4, "<p>tail", true)],
  });

  expect(notifies).toBe(1); // ONE notify for the whole patch, not one per block
  // Sanity: the patch did land (4 blocks in the snapshot).
  expect(client.getSnapshot().map((b) => b.id)).toEqual([1, 2, 3, 4]);
});

// --------------------------------------------------------------------------
// RESET-NOOP-NO-EMIT (fix #3) — no no-op notify on an empty store; still emits
// when there was content to clear.
// --------------------------------------------------------------------------

test("RESET-NOOP-NO-EMIT: reset() on an empty store does NOT notify; reset() after content DOES notify", () => {
  const { client, created } = setup();

  let notifies = 0;
  client.subscribe(() => {
    notifies++;
  });

  // 1) reset() on a just-constructed (empty) store: must be a silent no-op.
  client.reset();
  expect(notifies).toBe(0); // no wasted notify → no wasted render pass
  expect(client.getSnapshot().length).toBe(0);

  // 2) Add content via a patch, then reset() again: the clear-the-view notify
  //    is preserved when there genuinely was content.
  const { sid, w } = wire(client, created);
  firePatch(w, sid, { newly_committed: [blk(1, "<p>x</p>")], active: [] });
  expect(notifies).toBe(1); // the patch itself emitted once
  expect(client.getSnapshot().length).toBe(1);

  client.reset();
  expect(notifies).toBe(2); // reset cleared real content → it MUST notify
  expect(client.getSnapshot().length).toBe(0);
});

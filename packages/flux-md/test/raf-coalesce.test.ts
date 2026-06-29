import { test, expect } from "bun:test";
import { FluxClient, FluxPool } from "../src/client";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// Opt-in frame coalescing at the store emit seam (client.ts `coalesce`): N
// synchronous patch emits in one frame collapse into ONE listener notification,
// while the stream-completion (finalize/done) patch flushes SYNCHRONOUSLY and a
// pending frame is cancelled by reset(). Mirrors the FakeWorker-over-FluxPool
// harness used by rerender-store.test.ts / pool.test.ts — no real Worker/WASM —
// plus a controllable requestAnimationFrame (as rerender-dom.test.ts uses) so the
// frame boundary is deterministic.

function blk(id: number, html: string, open = false): Block {
  return { id, kind: { type: "Paragraph" }, start: 0, end: html.length, html, open, speculative: false };
}

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

function setup(coalesce: boolean) {
  const created: FakeWorker[] = [];
  const pool = new FluxPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 8);
  const client = new FluxClient({ pool, coalesce });
  return { pool, created, client };
}

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
    patch: JSON.stringify(patch),
    appendedBytes: 0,
    parseMicros: 0,
    retainedBytes: 0,
    wasmMemoryBytes: 0,
  });

// Controllable requestAnimationFrame: queue callbacks, drain() runs + clears.
// Returns a restore() to reinstate whatever was there before (rerender-dom does
// the same so concurrent test files don't poison each other's rAF).
function installFakeRaf() {
  const g = globalThis as Record<string, unknown>;
  const prevRaf = g.requestAnimationFrame;
  const prevCaf = g.cancelAnimationFrame;
  let queued: Array<{ id: number; cb: FrameRequestCallback } | null> = [];
  let nextId = 1;
  let scheduled = 0;
  g.requestAnimationFrame = ((cb: FrameRequestCallback) => {
    const id = nextId++;
    queued.push({ id, cb });
    scheduled++;
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
  return {
    drain,
    pendingCount: () => queued.filter(Boolean).length,
    scheduledCount: () => scheduled,
    restore() {
      g.requestAnimationFrame = prevRaf;
      g.cancelAnimationFrame = prevCaf;
    },
  };
}

// --------------------------------------------------------------------------
// COALESCE-ONE-PER-FRAME — N intra-frame patches → exactly 1 notification
// --------------------------------------------------------------------------

test("COALESCE-ONE-PER-FRAME: 4 synchronous patches in one frame notify subscribers exactly once, with the latest snapshot", () => {
  const raf = installFakeRaf();
  try {
    const { client, created } = setup(true);
    const { sid, w } = wire(client, created);

    let notifies = 0;
    client.subscribe(() => {
      notifies++;
    });

    // Four tail-growing patches, all BEFORE any frame drains.
    firePatch(w, sid, { newly_committed: [], active: [blk(1, "<p>a</p>", true)] });
    firePatch(w, sid, { newly_committed: [], active: [blk(1, "<p>ab</p>", true)] });
    firePatch(w, sid, { newly_committed: [], active: [blk(1, "<p>abc</p>", true)] });
    firePatch(w, sid, { newly_committed: [], active: [blk(1, "<p>abcd</p>", true)] });

    // Nothing has fired yet — they only SCHEDULED a single frame.
    expect(notifies).toBe(0);
    expect(raf.scheduledCount()).toBe(1);

    raf.drain();

    expect(notifies).toBe(1); // ONE notify for the whole frame, not one per patch
    // ...and the subscriber sees the LATEST snapshot (coalescing is lossless).
    expect(client.getSnapshot()[0].html).toBe("<p>abcd</p>");
  } finally {
    raf.restore();
  }
});

// --------------------------------------------------------------------------
// DONE-FLUSHES-SYNC — the finalize/done patch must not be deferred a frame
// --------------------------------------------------------------------------

test("DONE-FLUSHES-SYNC: after finalize(), the terminal patch notifies synchronously (no frame needed) and cancels a pending frame", () => {
  const raf = installFakeRaf();
  try {
    const { client, created } = setup(true);
    const { sid, w } = wire(client, created);

    let notifies = 0;
    client.subscribe(() => {
      notifies++;
    });

    // A streaming patch → coalesced (scheduled, not yet fired).
    firePatch(w, sid, { newly_committed: [], active: [blk(1, "<p>stream</p>", true)] });
    expect(notifies).toBe(0);
    expect(raf.pendingCount()).toBe(1);

    // Completion: finalize() arms the sync flush; the terminal patch commits the
    // block and must notify IMMEDIATELY — no drain.
    client.finalize();
    firePatch(w, sid, { newly_committed: [blk(1, "<p>stream done</p>")], active: [] });

    expect(notifies).toBe(1); // delivered synchronously, before any frame
    expect(client.getSnapshot()[0].html).toBe("<p>stream done</p>");
    // The sync flush also cancelled the earlier pending frame, so a later drain
    // is a no-op — no duplicate, stale notification.
    expect(raf.pendingCount()).toBe(0);
    raf.drain();
    expect(notifies).toBe(1);
  } finally {
    raf.restore();
  }
});

test("DONE-FLUSHES-SYNC even when an append patch precedes the terminal patch (the `final` flag binds the sync flush to the real terminal patch)", () => {
  const raf = installFakeRaf();
  try {
    const { client, created } = setup(true);
    const { sid, w } = wire(client, created);

    let lastHtml = "";
    client.subscribe(() => {
      lastHtml = client.getSnapshot()[0]?.html ?? "";
    });

    // Real async order when append()+finalize() are issued in one task: the
    // worker emits the (still-open) APPEND patch first, then the terminal
    // finalize patch. The append patch must NOT steal the synchronous-completion
    // signal — that's what the one-shot finalizePending flag did, deferring the
    // actual terminal patch a frame.
    client.finalize();
    firePatch(w, sid, { newly_committed: [], active: [blk(1, "<p>stream</p>", true)] }); // intermediate, no `final`
    w.fire({
      type: "patch",
      streamId: sid,
      patch: JSON.stringify({ newly_committed: [blk(1, "<p>stream done</p>")], active: [] }),
      appendedBytes: 0, parseMicros: 0, retainedBytes: 0, wasmMemoryBytes: 0,
      final: true, // tagged at the source by worker-core.doFinalize()
    });

    // The terminal committed state was delivered SYNCHRONOUSLY — not parked in a frame.
    expect(lastHtml).toBe("<p>stream done</p>");
    expect(client.getSnapshot()[0].html).toBe("<p>stream done</p>");
    expect(raf.pendingCount()).toBe(0);
    raf.drain(); // nothing deferred to deliver
    expect(client.getSnapshot()[0].html).toBe("<p>stream done</p>");
  } finally {
    raf.restore();
  }
});

// --------------------------------------------------------------------------
// RESET-CANCELS-FRAME — reset() drops a pending coalesced frame
// --------------------------------------------------------------------------

test("RESET-CANCELS-FRAME: reset() cancels a pending coalesced frame; the dropped frame never fires", () => {
  const raf = installFakeRaf();
  try {
    const { client, created } = setup(true);
    const { sid, w } = wire(client, created);

    let notifies = 0;
    client.subscribe(() => {
      notifies++;
    });

    // Land content (its own coalesced frame) and DRAIN so reset() has real
    // content to clear (the empty-store reset is a deliberate no-op).
    firePatch(w, sid, { newly_committed: [blk(1, "<p>x</p>")], active: [blk(2, "<p>tail", true)] });
    raf.drain();
    expect(notifies).toBe(1);

    // A new tail patch schedules a frame that has NOT drained yet...
    firePatch(w, sid, { newly_committed: [], active: [blk(2, "<p>tail grown</p>", true)] });
    expect(raf.pendingCount()).toBe(1);

    // reset() clears real content (synchronous notify) AND cancels the pending
    // frame so it can't fire into the just-cleared store.
    client.reset();
    expect(notifies).toBe(2); // reset's clear-the-view notify is synchronous
    expect(raf.pendingCount()).toBe(0);
    expect(client.getSnapshot().length).toBe(0);

    // Draining now must be a no-op — the cancelled frame is gone.
    raf.drain();
    expect(notifies).toBe(2);
  } finally {
    raf.restore();
  }
});

// --------------------------------------------------------------------------
// DEFAULT-OFF — without the flag, emits stay synchronous (unchanged behavior)
// --------------------------------------------------------------------------

test("DEFAULT-OFF: with coalesce unset, each patch notifies synchronously (no rAF scheduled)", () => {
  const raf = installFakeRaf();
  try {
    const { client, created } = setup(false);
    const { sid, w } = wire(client, created);

    let notifies = 0;
    client.subscribe(() => {
      notifies++;
    });

    firePatch(w, sid, { newly_committed: [], active: [blk(1, "<p>a</p>", true)] });
    firePatch(w, sid, { newly_committed: [], active: [blk(1, "<p>ab</p>", true)] });

    expect(notifies).toBe(2); // one synchronous notify per patch, as before
    expect(raf.scheduledCount()).toBe(0); // default path never touches rAF
  } finally {
    raf.restore();
  }
});

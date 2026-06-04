import { test, expect } from "bun:test";
import { FluxClient, FluxPool } from "../src/client";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// A synchronous fake worker: records what was posted to it and lets the test
// fire responses back through the registered listener. No real Worker/WASM.
class FakeWorker implements WorkerLike {
  sent: ToWorker[] = [];
  terminated = false;
  private listener: ((ev: { data: FromWorker }) => void) | null = null;
  postMessage(msg: ToWorker) {
    this.sent.push(msg);
  }
  addEventListener(_t: "message", l: (ev: { data: FromWorker }) => void) {
    this.listener = l;
  }
  terminate() {
    this.terminated = true;
  }
  fire(msg: FromWorker) {
    this.listener?.({ data: msg });
  }
}

function makePool(cap: number) {
  const created: FakeWorker[] = [];
  const pool = new FluxPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, cap);
  return { pool, created };
}

test("one stream uses exactly one worker (lazy)", () => {
  const { pool, created } = makePool(8);
  pool.acquire(() => {});
  expect(created.length).toBe(1);
  expect(pool.workerCount).toBe(1);
});

test("each new stream gets its own worker until the cap", () => {
  const { pool, created } = makePool(3);
  for (let i = 0; i < 3; i++) pool.acquire(() => {});
  expect(created.length).toBe(3);
  expect(pool.workerCount).toBe(3);
});

test("past the cap, streams attach to the least-loaded worker", () => {
  const { pool } = makePool(2);
  const a = pool.acquire(() => {}); // worker0: 1
  const b = pool.acquire(() => {}); // worker1: 1
  const c = pool.acquire(() => {}); // cap hit → least-loaded (worker0): 2
  const d = pool.acquire(() => {}); // least-loaded (worker1): 2
  expect(pool.workerCount).toBe(2);
  // a&c share a worker; b&d share the other; the two workers differ.
  expect(a.pw).toBe(c.pw);
  expect(b.pw).toBe(d.pw);
  expect(a.pw).not.toBe(b.pw);
});

test("messages are demuxed to the owning stream's handler only", () => {
  const { pool, created } = makePool(1); // force both streams onto one worker
  const got1: FromWorker[] = [];
  const got2: FromWorker[] = [];
  const s1 = pool.acquire((m) => got1.push(m));
  const s2 = pool.acquire((m) => got2.push(m));
  expect(s1.pw).toBe(s2.pw);
  const w = created[0];

  const patch = (streamId: number): FromWorker => ({
    type: "patch", streamId, patch: { newly_committed: [], active: [] },
    appendedBytes: 0, parseMicros: 0, retainedBytes: 0, wasmMemoryBytes: 0,
  });
  w.fire(patch(s1.streamId));
  w.fire(patch(s2.streamId));
  w.fire(patch(s1.streamId));

  expect(got1.length).toBe(2);
  expect(got2.length).toBe(1);
});

test("ready is worker-level and not delivered to stream handlers", () => {
  const { pool, created } = makePool(1);
  const got: FromWorker[] = [];
  pool.acquire((m) => got.push(m));
  created[0].fire({ type: "ready" });
  expect(got.length).toBe(0); // handler sees patch/error, never ready
});

test("whenWorkerReady resolves on ready, and immediately for later siblings", async () => {
  const { pool, created } = makePool(1);
  const s1 = pool.acquire(() => {});
  let resolved = false;
  const p = pool.whenWorkerReady(s1.pw).then(() => (resolved = true));
  expect(resolved).toBe(false); // not ready yet
  created[0].fire({ type: "ready" });
  await p;
  expect(resolved).toBe(true);
  // A second stream on the now-ready worker resolves without another message.
  const s2 = pool.acquire(() => {});
  expect(s2.pw).toBe(s1.pw);
  await pool.whenWorkerReady(s2.pw); // resolves immediately
});

test("a fatal worker error rejects whenWorkerReady and notifies every live stream", async () => {
  const { pool, created } = makePool(1); // both streams share one worker
  const got1: FromWorker[] = [];
  const got2: FromWorker[] = [];
  const s1 = pool.acquire((m) => got1.push(m));
  const s2 = pool.acquire((m) => got2.push(m));
  expect(s1.pw).toBe(s2.pw);

  const ready = pool.whenWorkerReady(s1.pw);
  // Fatal WASM-init failure — carries no real streamId.
  created[0].fire({ type: "error", streamId: -1, message: "WASM boom", fatal: true });

  await expect(ready).rejects.toThrow("WASM boom");
  // Both live streams were notified, so each client's onError can fire.
  expect(got1.at(-1)).toMatchObject({ type: "error", fatal: true, message: "WASM boom" });
  expect(got2.at(-1)).toMatchObject({ type: "error", fatal: true, message: "WASM boom" });
  // A later readiness check on the doomed worker rejects immediately too.
  await expect(pool.whenWorkerReady(s1.pw)).rejects.toThrow("WASM boom");
});

test("a non-fatal (per-stream) error routes only to that stream's handler", () => {
  const { pool, created } = makePool(1);
  const got1: FromWorker[] = [];
  const got2: FromWorker[] = [];
  const s1 = pool.acquire((m) => got1.push(m));
  pool.acquire((m) => got2.push(m));
  created[0].fire({ type: "error", streamId: s1.streamId, message: "parse oops" });
  expect(got1.length).toBe(1);
  expect(got2.length).toBe(0);
});

test("FluxClient.onError receives worker errors (no console.error fallback)", () => {
  const { pool, created } = makePool(1);
  const errors: Array<{ message: string; fatal?: boolean }> = [];
  const c = new FluxClient({ pool, onError: (e) => errors.push(e) });
  c.append("x"); // wire the worker + discover the stream id
  const sid = (created[0].sent[0] as { streamId: number }).streamId;
  created[0].fire({ type: "error", streamId: sid, message: "parse oops" });
  expect(errors.length).toBe(1);
  expect(errors[0].message).toBe("parse oops");
  expect(errors[0].fatal).toBeUndefined();
});

test("FluxClient.whenReady rejects and onError fires on a fatal init failure", async () => {
  const { pool, created } = makePool(1);
  const errors: Array<{ message: string; fatal?: boolean }> = [];
  const c = new FluxClient({ pool, onError: (e) => errors.push(e) });
  const ready = c.whenReady();
  created[0].fire({ type: "error", streamId: -1, message: "no WASM", fatal: true });
  await expect(ready).rejects.toThrow("no WASM");
  expect(errors.length).toBe(1);
  expect(errors[0].fatal).toBe(true);
});

test("a throwing stream handler can't break the fatal fan-out or the message loop", () => {
  const { pool, created } = makePool(1); // both streams share one worker
  let bNotified = false;
  pool.acquire(() => {
    throw new Error("handler boom"); // stream a's handler always throws
  });
  pool.acquire((m) => {
    if (m.type === "error" && m.fatal) bNotified = true; // stream b
  });
  // a throws, but the dispatch boundary isolates it: the fire must not throw and
  // b must still receive the fatal notification.
  expect(() =>
    created[0].fire({ type: "error", streamId: -1, message: "boom", fatal: true }),
  ).not.toThrow();
  expect(bNotified).toBe(true);
});

test("a fatally-failed worker is not re-picked by a new stream", () => {
  const { pool, created } = makePool(2);
  const s1 = pool.acquire(() => {});
  created[0].fire({ type: "error", streamId: -1, message: "dead", fatal: true });
  // A new stream must NOT land on the dead worker (it would post into it and hang).
  const s2 = pool.acquire(() => {});
  expect(s2.pw).not.toBe(s1.pw);
  expect(s2.pw.failed).toBeNull();
});

test("pipeFrom reads a stream, appends decoded chunks, and finalizes", async () => {
  const { pool, created } = makePool(1);
  const c = new FluxClient({ pool });
  const enc = new TextEncoder();
  const stream = new ReadableStream<Uint8Array>({
    start(ctrl) {
      ctrl.enqueue(enc.encode("# Hi\n"));
      ctrl.enqueue(enc.encode("body text"));
      ctrl.close();
    },
  });
  await c.pipeFrom(stream);
  const sent = created[0].sent;
  const appends = sent
    .filter((m) => m.type === "append")
    .map((m) => (m as { chunk: string }).chunk)
    .join("");
  expect(appends).toContain("# Hi");
  expect(appends).toContain("body text");
  expect(sent.some((m) => m.type === "finalize")).toBe(true);
});

test("pipeFrom accepts a Response and finalizes an empty (null-body) one", async () => {
  const { pool, created } = makePool(1);
  const c = new FluxClient({ pool });
  // A Response-like with a null body (e.g. 204) → completed empty stream.
  await c.pipeFrom({ body: null } as unknown as Response);
  expect(created[0].sent.some((m) => m.type === "finalize")).toBe(true);
});

test("pipeFrom(AsyncIterable) appends each chunk in order and finalizes exactly once", async () => {
  const { pool, created } = makePool(1);
  const c = new FluxClient({ pool });
  async function* gen() {
    yield "a";
    yield "b";
    yield "c";
  }
  await c.pipeFrom(gen());
  const appends = created[0].sent
    .filter((m) => m.type === "append")
    .map((m) => (m as { chunk: string }).chunk);
  expect(appends).toEqual(["a", "b", "c"]);
  expect(created[0].sent.filter((m) => m.type === "finalize").length).toBe(1);
});

test("pipeFrom(AsyncIterable) with a pre-aborted signal appends nothing and never finalizes", async () => {
  const { pool, created } = makePool(1);
  const c = new FluxClient({ pool });
  const ac = new AbortController();
  ac.abort();
  async function* gen() {
    yield "a";
    yield "b";
  }
  await c.pipeFrom(gen(), { signal: ac.signal });
  // Lazy acquire: a pre-aborted pipeFrom returns before any worker-bound op, so
  // no worker is ever created — which trivially implies no append and no finalize.
  expect(created.length).toBe(0);
});

test("pipeFrom(AsyncIterable) aborted mid-stream stops appending and does not finalize", async () => {
  const { pool, created } = makePool(1);
  const c = new FluxClient({ pool });
  const ac = new AbortController();
  let openGate!: () => void;
  const gate = new Promise<void>((r) => (openGate = r));
  async function* gen() {
    yield "a";
    yield "b";
    await gate; // hold here until the test aborts + opens the gate
    yield "c";
  }
  const p = c.pipeFrom(gen(), { signal: ac.signal });
  await new Promise((r) => setTimeout(r, 0)); // let a, b append; loop now awaits the gate
  ac.abort();
  openGate();
  await p;
  const appends = created[0].sent
    .filter((m) => m.type === "append")
    .map((m) => (m as { chunk: string }).chunk);
  expect(appends).toEqual(["a", "b"]); // c is dropped by the post-abort guard
  expect(created[0].sent.some((m) => m.type === "finalize")).toBe(false);
});

test("pipeFrom(ReadableStream) aborted while stalled cancels the reader and does not finalize", async () => {
  const { pool, created } = makePool(1);
  const c = new FluxClient({ pool });
  const ac = new AbortController();
  let cancelled = false;
  const enc = new TextEncoder();
  const stream = new ReadableStream<Uint8Array>({
    start(ctrl) {
      ctrl.enqueue(enc.encode("a")); // one chunk, then stall (never close)
    },
    cancel() {
      cancelled = true;
    },
  });
  const p = c.pipeFrom(stream, { signal: ac.signal });
  await new Promise((r) => setTimeout(r, 0)); // "a" appends; read() now pends
  ac.abort();
  await p;
  expect(cancelled).toBe(true); // abort listener cancelled the reader
  const appends = created[0].sent
    .filter((m) => m.type === "append")
    .map((m) => (m as { chunk: string }).chunk)
    .join("");
  expect(appends).toContain("a");
  expect(created[0].sent.some((m) => m.type === "finalize")).toBe(false);
});

test("onBlock fires once per committed block in document order, not for the active tail", () => {
  const { pool, created } = makePool(1);
  const got: number[] = [];
  const c = new FluxClient({ pool, onBlock: (b) => got.push(b.id) });
  c.append("x");
  const sid = (created[0].sent[0] as { streamId: number }).streamId;
  const blk = (id: number): Block => ({
    id, kind: { type: "Paragraph" }, start: 0, end: 0, html: "<p></p>", open: false, speculative: false,
  });
  created[0].fire({
    type: "patch", streamId: sid,
    patch: { newly_committed: [blk(1), blk(2)], active: [blk(3)] },
    appendedBytes: 0, parseMicros: 0, retainedBytes: 0, wasmMemoryBytes: 0,
  });
  expect(got).toEqual([1, 2]); // committed in order; the active block (3) does not fire
});

test("reattach() re-sends config on the next append (the worker discards it on dispose)", () => {
  const { pool, created } = makePool(1);
  const c = new FluxClient({ pool, config: { gfmMath: true } });
  c.append("x"); // first message carries config
  c.destroy(); // posts dispose → the worker deletes the stored config
  c.reattach(); // StrictMode remount of the same client
  c.append("y"); // must re-send config, since the worker dropped it on dispose
  const withConfig = created[0].sent.filter(
    (m) => m.type === "append" && (m as { config?: unknown }).config !== undefined,
  );
  expect(withConfig.length).toBe(2); // the first append AND the post-reattach one
});

test("release frees the stream slot, sends dispose, keeps the worker warm", () => {
  const { pool, created } = makePool(4);
  const s = pool.acquire(() => {});
  expect(pool.workerCount).toBe(1);
  pool.release(s.streamId, s.pw);
  expect(created[0].sent).toContainEqual({ type: "dispose", streamId: s.streamId });
  expect(pool.workerCount).toBe(1); // worker stays alive
  // A subsequent stream reuses the warm (now-idle) worker rather than spawning.
  const s2 = pool.acquire(() => {});
  expect(s2.pw).toBe(s.pw);
  expect(pool.workerCount).toBe(1);
  // After release, messages for the freed stream are dropped (no handler).
  created[0].fire({
    type: "patch", streamId: s.streamId, patch: { newly_committed: [], active: [] },
    appendedBytes: 0, parseMicros: 0, retainedBytes: 0, wasmMemoryBytes: 0,
  });
  // (No throw = pass; the handler map no longer has streamId.)
});

test("send routes a message to the stream's worker", () => {
  const { pool, created } = makePool(2);
  const s = pool.acquire(() => {});
  pool.send(s.pw, { type: "append", streamId: s.streamId, chunk: "hi" });
  expect(created[0].sent).toContainEqual({ type: "append", streamId: s.streamId, chunk: "hi" });
});

test("disposeAll terminates every worker", () => {
  const { pool, created } = makePool(4);
  pool.acquire(() => {});
  pool.acquire(() => {});
  expect(created.length).toBe(2);
  pool.disposeAll();
  expect(created.every((w) => w.terminated)).toBe(true);
  expect(pool.workerCount).toBe(0);
});

test("simulates 50 streams over an 8-worker cap (~6 each)", () => {
  const { pool } = makePool(8);
  for (let i = 0; i < 50; i++) pool.acquire(() => {});
  expect(pool.workerCount).toBe(8);
});

test("parser config rides only on a stream's first message", () => {
  const { pool, created } = makePool(2);
  const c = new FluxClient({ pool, config: { unsafeHtml: true, gfmAlerts: false, gfmFootnotes: true } });
  c.append("a");
  c.append("b");
  c.finalize();
  const withCfg = created[0].sent.filter((m) => (m as any).config !== undefined);
  expect(withCfg.length).toBe(1); // exactly the first message
  expect((withCfg[0] as any).config).toEqual({ unsafeHtml: true, gfmAlerts: false, gfmFootnotes: true });
});

test("no config → no config field on any message (worker uses defaults)", () => {
  const { pool, created } = makePool(2);
  const c = new FluxClient({ pool });
  c.append("a");
  c.finalize();
  expect(created[0].sent.every((m) => (m as any).config === undefined)).toBe(true);
});

test("outline() and toPlaintext() derive from the streamed snapshot", () => {
  const { pool, created } = makePool(1);
  const c = new FluxClient({ pool });
  c.append("x"); // wire the worker + discover the stream id
  const sid = (created[0].sent[0] as { streamId: number }).streamId;

  const heading = (id: number, level: number, text: string): Block => ({
    id, kind: { type: "Heading", data: level }, start: 0, end: 0,
    html: `<h${level}>${text}</h${level}>`, open: false, speculative: false,
  });
  const para = (id: number, html: string): Block => ({
    id, kind: { type: "Paragraph" }, start: 0, end: 0, html, open: false, speculative: false,
  });

  created[0].fire({
    type: "patch", streamId: sid,
    patch: {
      newly_committed: [
        heading(1, 1, "Title"),
        para(2, "<p>Hello &amp; <strong>world</strong></p>"),
        heading(3, 2, "Sub"),
      ],
      active: [],
    },
    appendedBytes: 0, parseMicros: 0, retainedBytes: 0, wasmMemoryBytes: 0,
  });

  expect(c.outline()).toEqual([
    { level: 1, text: "Title", id: 1 },
    { level: 2, text: "Sub", id: 3 },
  ]);
  expect(c.toPlaintext()).toBe("Title\n\nHello & world\n\nSub");
});

test("default constructor still joins a pool and streams (no behavior change)", () => {
  const { pool, created } = makePool(2);
  const c = new FluxClient({ pool });
  c.append("hello");
  expect(created[0].sent[0]).toMatchObject({ type: "append", chunk: "hello" });
});

test("warm() eagerly creates a worker and resolves once it is ready", async () => {
  const { pool, created } = makePool(8);
  let warmed = false;
  const p = pool.warm().then(() => {
    warmed = true;
  });
  expect(created.length).toBe(1); // worker built immediately → WASM init starts now
  expect(warmed).toBe(false); // but warm() awaits readiness
  created[0].fire({ type: "ready" });
  await p;
  expect(warmed).toBe(true);
});

test("warm() reuses a live worker instead of stacking new ones", () => {
  const { pool, created } = makePool(8);
  pool.warm();
  pool.warm();
  expect(created.length).toBe(1);
});

test("the first stream attaches to the warm worker (init is not wasted)", () => {
  const { pool, created } = makePool(8);
  pool.warm();
  pool.acquire(() => {}); // first real stream
  expect(created.length).toBe(1); // reused the warm worker, no new one
  expect(pool.workerCount).toBe(1);
});

test("warm() on a pool whose only worker died fatally builds a fresh one", () => {
  const { pool, created } = makePool(8);
  const s = pool.acquire(() => {});
  created[0].fire({ type: "error", streamId: -1, message: "dead", fatal: true });
  expect(s.pw.failed).not.toBeNull();
  pool.warm(); // must not hand back the dead worker
  expect(created.length).toBe(2);
});

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

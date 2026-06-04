import { test, expect } from "bun:test";
import { FluxClient, FluxPool } from "../src/client";
import type { FromWorker, ToWorker, WorkerLike } from "../src/types";

// setContent is the controlled-string bridge: feed it the whole document each
// time and it diffs against the last value — prefix-extension → append the
// delta; divergence → reset + reparse. These drive a FluxClient over a fake
// worker and assert the exact message sequence it emits (no real Worker/WASM).

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

const appendedChunks = (w: FakeWorker) =>
  w.sent.filter((m): m is Extract<ToWorker, { type: "append" }> => m.type === "append").map((m) => m.chunk);
const msgTypes = (w: FakeWorker) => w.sent.map((m) => m.type);

test("prefix-extension appends only the new suffix (no reset)", () => {
  const { client, created } = setup();
  client.setContent("# A\n\n");
  client.setContent("# A\n\nbody");
  client.setContent("# A\n\nbody more");
  const w = created[0];
  expect(appendedChunks(w)).toEqual(["# A\n\n", "body", " more"]);
  expect(msgTypes(w)).not.toContain("reset");
});

test("an unchanged string is a no-op (no extra append)", () => {
  const { client, created } = setup();
  client.setContent("x");
  client.setContent("x");
  expect(appendedChunks(created[0])).toEqual(["x"]);
});

test("divergence resets then reparses the whole new string (reset precedes the reparse append)", () => {
  const { client, created } = setup();
  client.setContent("hello world");
  client.setContent("HELLO world"); // not a prefix-extension of "hello world"
  const w = created[0];
  expect(appendedChunks(w)).toEqual(["hello world", "HELLO world"]);
  const resetIdx = w.sent.findIndex((m) => m.type === "reset");
  const lastAppendIdx = w.sent.map((m) => m.type).lastIndexOf("append");
  expect(resetIdx).toBeGreaterThanOrEqual(0);
  expect(resetIdx).toBeLessThan(lastAppendIdx); // reset, THEN reparse
});

test("{ done: true } finalizes once and is idempotent for the same content", () => {
  const { client, created } = setup();
  client.setContent("final text", { done: true });
  client.setContent("final text", { done: true }); // same content + done → no second finalize
  const w = created[0];
  expect(w.sent.filter((m) => m.type === "finalize").length).toBe(1);
});

test("a content change after done reopens via reset+reparse (a finalized parser is terminal)", () => {
  const { client, created } = setup();
  client.setContent("a", { done: true }); // append "a" + finalize → parser now terminal
  client.setContent("ab", { done: true }); // reopen: must reset + reparse, NOT append "b"
  const w = created[0];
  // The delta must NOT be appended into the finalized (dead) parser — that would
  // be silently dropped. Reopen resets and reparses the whole new string.
  expect(appendedChunks(w)).toEqual(["a", "ab"]);
  const resetIdx = w.sent.findIndex((m) => m.type === "reset");
  const lastAppendIdx = w.sent.map((m) => m.type).lastIndexOf("append");
  expect(resetIdx).toBeGreaterThanOrEqual(0); // a reset was issued
  expect(resetIdx).toBeLessThan(lastAppendIdx); // reset, THEN reparse the new string
  expect(w.sent.filter((m) => m.type === "finalize").length).toBe(2);
});

test("reset() clears the diff baseline so the next setContent re-feeds the document", () => {
  const { client, created } = setup();
  client.setContent("abc");
  client.reset(); // manual reset — worker drops the parser
  client.setContent("abc"); // baseline cleared → must re-append the whole string
  expect(appendedChunks(created[0])).toEqual(["abc", "abc"]);
});

test("reattach() clears the baseline (StrictMode dev double-mount re-feeds)", () => {
  const { client, created } = setup();
  client.setContent("hello");
  client.destroy(); // simulated unmount: dispose drops the parser
  client.reattach(); // remount the SAME instance
  client.setContent("hello"); // baseline cleared → re-append
  expect(appendedChunks(created[0])).toEqual(["hello", "hello"]);
});

import { test, expect } from "bun:test";
import { WorkerCore, type ParserLike, type WorkerCoreDeps } from "../src/worker-core";
import type { FromWorker, Patch } from "../src/types-core";

// Regression guard for the WASM-readiness gate in the worker. The full suite
// drives a FakeWorker (pool) or FluxParser directly (wasm-integration) — neither
// exercises this state machine, which is the one that silently dropped a stream's
// first chunk(s) when an append arrived before WASM init() resolved (a parser
// got constructed against an uninitialized module → threw → chunk lost). These
// tests use a fake parser, no WASM, no Worker.

const EMPTY_PATCH: Patch = { newly_committed: [], active: [] };

class FakeParser implements ParserLike {
  calls: string[] = [];
  appended = "";
  append(chunk: string): string {
    this.calls.push(`append:${chunk}`);
    this.appended += chunk;
    return JSON.stringify(EMPTY_PATCH);
  }
  finalize(): string {
    this.calls.push("finalize");
    return JSON.stringify(EMPTY_PATCH);
  }
  free(): void {
    this.calls.push("free");
  }
  retainedBytes(): number {
    return this.appended.length;
  }
}

function harness(opts?: { makeParser?: () => ParserLike }) {
  const posted: FromWorker[] = [];
  const created: FakeParser[] = [];
  let makeCount = 0;
  const deps: WorkerCoreDeps = {
    makeParser: opts?.makeParser
      ? (() => {
          makeCount++;
          return opts.makeParser!();
        })
      : (() => {
          makeCount++;
          const p = new FakeParser();
          created.push(p);
          return p;
        }),
    post: (m) => posted.push(m),
    memBytes: () => 0,
    schedule: (fn) => fn(), // synchronous so flushes are deterministic in-test
  };
  return { core: new WorkerCore(deps), posted, created, makeCount: () => makeCount };
}

const patches = (posted: FromWorker[]) => posted.filter((m) => m.type === "patch");
const errors = (posted: FromWorker[]) => posted.filter((m) => m.type === "error");

test("buffers appends until ready, then drains EVERY chunk (incl. the first) — the dropped-chunk regression", () => {
  const h = harness();
  // Two appends arrive before init resolves.
  h.core.handle({ type: "append", streamId: 1, chunk: "# Heading\n\n" });
  h.core.handle({ type: "append", streamId: 1, chunk: "body" });

  // Nothing constructed, nothing emitted — input is held, not processed.
  expect(h.makeCount()).toBe(0);
  expect(patches(h.posted).length).toBe(0);
  expect(h.posted.some((m) => m.type === "ready")).toBe(false);

  h.core.markReady();

  // Exactly one parser, and it received the WHOLE buffered input (the first
  // chunk's "# Heading" is NOT lost), then one patch is emitted.
  expect(h.makeCount()).toBe(1);
  expect(h.created[0].appended).toBe("# Heading\n\nbody");
  expect(h.posted[0]).toEqual({ type: "ready" });
  expect(patches(h.posted).length).toBe(1);
  expect(errors(h.posted).length).toBe(0);
});

test("finalize before ready is deferred, then runs AFTER the buffered appends, in order", () => {
  const h = harness();
  h.core.handle({ type: "append", streamId: 1, chunk: "hi" });
  h.core.handle({ type: "finalize", streamId: 1 });

  expect(h.makeCount()).toBe(0); // deferred — no parser yet

  h.core.markReady();

  // append("hi") must precede finalize() on the same parser.
  expect(h.makeCount()).toBe(1);
  expect(h.created[0].calls).toEqual(["append:hi", "finalize"]);
  // A patch from the append-drain and a patch from finalize.
  expect(patches(h.posted).length).toBe(2);
});

test("an empty stream that finalizes before ready still emits a finalize patch on ready", () => {
  const h = harness();
  h.core.handle({ type: "finalize", streamId: 7 }); // no appends at all
  expect(h.makeCount()).toBe(0);

  h.core.markReady();

  expect(h.makeCount()).toBe(1);
  expect(h.created[0].calls).toEqual(["finalize"]);
  expect(patches(h.posted).length).toBe(1);
});

test("reset before ready clears buffered input AND cancels a pending finalize", () => {
  const h = harness();
  h.core.handle({ type: "append", streamId: 1, chunk: "x" });
  h.core.handle({ type: "finalize", streamId: 1 });
  h.core.handle({ type: "reset", streamId: 1 });

  h.core.markReady();

  // Nothing to drain: no parser created, only the "ready" message posted.
  expect(h.makeCount()).toBe(0);
  expect(patches(h.posted).length).toBe(0);
  expect(h.posted).toEqual([{ type: "ready" }]);
});

test("after ready, a normal append flushes and emits a patch", () => {
  const h = harness();
  h.core.markReady();
  h.core.handle({ type: "append", streamId: 2, chunk: "later" });

  expect(h.makeCount()).toBe(1);
  expect(h.created[0].appended).toBe("later");
  expect(patches(h.posted).length).toBe(1);
});

test("a parser-construction failure becomes a posted error, not an uncaught throw", () => {
  const h = harness({
    makeParser: () => {
      throw new Error("kaboom");
    },
  });
  h.core.markReady();
  // Should not throw out of handle(); the error is routed to the stream.
  expect(() => h.core.handle({ type: "append", streamId: 3, chunk: "z" })).not.toThrow();
  const errs = errors(h.posted);
  expect(errs.length).toBe(1);
  expect(errs[0]).toMatchObject({ type: "error", streamId: 3, message: "kaboom" });
});

// Mirrors the Rust core's terminal-finalize: once finalize() runs, the parser
// drops any further append. This is the contract that makes setContent's
// reopen-after-done REQUIRE a reset() (not a bare delta append) — see
// setcontent.test.ts "a content change after done reopens via reset+reparse".
class TerminalParser implements ParserLike {
  text = "";
  private done = false;
  append(c: string): string {
    if (!this.done) this.text += c;
    return JSON.stringify(EMPTY_PATCH);
  }
  finalize(): string {
    this.done = true;
    return JSON.stringify(EMPTY_PATCH);
  }
  free(): void {}
  retainedBytes(): number {
    return this.text.length;
  }
}

test("a finalized parser drops further appends (why setContent's reopen must reset, not append)", () => {
  let parser: TerminalParser | null = null;
  const core = new WorkerCore({
    makeParser: () => {
      parser = new TerminalParser();
      return parser;
    },
    post: () => {},
    memBytes: () => 0,
    schedule: (fn) => fn(), // synchronous flush
  });
  core.handle({ type: "append", streamId: 1, chunk: "a" });
  core.markReady(); // drains → parser.append("a")
  core.handle({ type: "finalize", streamId: 1 }); // parser.finalize() → terminal
  core.handle({ type: "append", streamId: 1, chunk: "b" }); // appended into the dead parser
  expect(parser!.text).toBe("a"); // "b" was dropped — appending after finalize is a no-op
});

test("two streams buffered before ready each create their own parser and keep their content", () => {
  const h = harness();
  h.core.handle({ type: "append", streamId: 1, chunk: "one" });
  h.core.handle({ type: "append", streamId: 2, chunk: "two" });
  expect(h.makeCount()).toBe(0);

  h.core.markReady();

  expect(h.makeCount()).toBe(2);
  expect(h.created.map((p) => p.appended).sort()).toEqual(["one", "two"]);
  expect(patches(h.posted).length).toBe(2);
});

test("emitted patches tag the terminal one with final:true and echo the stream epoch", () => {
  const h = harness();
  h.core.markReady();
  h.core.handle({ type: "append", streamId: 1, chunk: "x", epoch: 3 });
  h.core.handle({ type: "finalize", streamId: 1, epoch: 3 });
  const ps = patches(h.posted) as Array<{ final?: boolean; epoch?: number }>;
  expect(ps.length).toBe(2);
  expect(ps[0].final).toBe(false); // append patch
  expect(ps[1].final).toBe(true); // terminal patch
  expect(ps.every((p) => p.epoch === 3)).toBe(true);
});

test("a WebAssembly trap escalates to a fatal error; a plain Error stays per-stream recoverable", () => {
  // Plain Error from append → non-fatal.
  const plain = harness({
    makeParser: () =>
      ({
        append() {
          throw new Error("plain boom");
        },
        finalize: () => JSON.stringify(EMPTY_PATCH),
        free() {},
        retainedBytes: () => 0,
      }) as ParserLike,
  });
  plain.core.markReady();
  plain.core.handle({ type: "append", streamId: 1, chunk: "x" });
  const e1 = errors(plain.posted) as Array<{ fatal?: boolean }>;
  expect(e1.length).toBe(1);
  expect(e1[0].fatal).toBeFalsy();

  // WASM trap (RuntimeError) from append → fatal (the shared instance is poisoned).
  const trap = harness({
    makeParser: () =>
      ({
        append() {
          throw new WebAssembly.RuntimeError("unreachable");
        },
        finalize: () => JSON.stringify(EMPTY_PATCH),
        free() {},
        retainedBytes: () => 0,
      }) as ParserLike,
  });
  trap.core.markReady();
  trap.core.handle({ type: "append", streamId: 1, chunk: "x" });
  const e2 = errors(trap.posted) as Array<{ fatal?: boolean }>;
  expect(e2.length).toBe(1);
  expect(e2[0].fatal).toBe(true);
});

test("free() throwing on a poisoned instance does not escape reset() or dispose()", () => {
  const make = () =>
    ({
      append: () => JSON.stringify(EMPTY_PATCH),
      finalize: () => JSON.stringify(EMPTY_PATCH),
      free() {
        throw new WebAssembly.RuntimeError("free on poisoned instance");
      },
      retainedBytes: () => 0,
    }) as ParserLike;

  const h1 = harness({ makeParser: make });
  h1.core.markReady();
  h1.core.handle({ type: "append", streamId: 1, chunk: "x" }); // create the parser
  expect(() => h1.core.handle({ type: "reset", streamId: 1 })).not.toThrow();

  const h2 = harness({ makeParser: make });
  h2.core.markReady();
  h2.core.handle({ type: "append", streamId: 1, chunk: "x" });
  expect(() => h2.core.handle({ type: "dispose", streamId: 1 })).not.toThrow();
});

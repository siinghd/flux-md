import { test, expect, beforeAll, spyOn } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { createElement, act } from "react";
import type { Block, BlockComponentProps, FromWorker, ToWorker, WorkerLike } from "../src/types";
import { FluxClient, FluxPool } from "../src/client";
import { FluxMarkdown, useFluxStream, blocksEqual } from "../src/react";

// Synchronous fake worker (same shape as react-stream.test.tsx).
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

let win: GlobalWindow;
beforeAll(() => {
  win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.window = win;
  g.navigator = win.navigator;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  // The default pool builds `new Worker(...)`; the stream test uses a mocked
  // pipeFrom so it never actually drives a worker, but the constructor still
  // touches the default pool lazily — give it a fake so nothing real is built.
  g.Worker = class extends FakeWorker {} as unknown;
  (g as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

const tick = () => new Promise((r) => setTimeout(r, 0));

async function mount(node: ReturnType<typeof createElement>) {
  const { createRoot } = await import("react-dom/client");
  const host = win.document.createElement("div");
  const root = createRoot(host as unknown as Element);
  await act(async () => {
    root.render(node);
  });
  return { host, root };
}

// Build a committed Paragraph block (stable reference is supplied by the store;
// we hold our own reference here only to construct the patch).
function para(id: number, html: string, open: boolean): Block {
  return { id, kind: { type: "Paragraph" }, start: 0, end: 0, html, open, speculative: false };
}

const PATCH_META = { appendedBytes: 0, parseMicros: 0, retainedBytes: 0, wasmMemoryBytes: 0 } as const;

// react-3: RENDER-ONCE — a committed block's override renders EXACTLY ONCE across
// every later patch that only grows/replaces the OPEN tail, because the store
// keeps the committed block's object reference identical and memo(blocksEqual)
// short-circuits its re-render.
test("react-3: a committed block's override renders exactly once across tail-only patches", async () => {
  const w = new FakeWorker();
  const pool = new FluxPool(() => w, 1);
  const client = new FluxClient({ pool });
  client.append(""); // force worker creation + assign the stream id
  const sid = (w.sent[0] as { streamId: number }).streamId;

  // Per-id render log: an override pushes its block id on every render. The
  // committed block (id=1) must appear exactly once; the live tail (id=2) once
  // per distinct patch it appears in.
  const renders: number[] = [];
  const components = {
    Paragraph: (p: BlockComponentProps) => {
      renders.push(p.block.id);
      return createElement("p", null, p.block.id);
    },
  };

  const { host } = await mount(createElement(FluxMarkdown, { client, components }));

  // Patch 1: COMMIT block id=1, open tail id=2.
  await act(async () => {
    w.fire({
      type: "patch",
      streamId: sid,
      patch: { newly_committed: [para(1, "<p>one</p>", false)], active: [para(2, "<p>tw</p>", true)] },
      ...PATCH_META,
    });
  });
  expect(renders.filter((id) => id === 1).length).toBe(1); // committed rendered

  // Patches 2..4: id=1 stays committed (untouched); only the OPEN tail (id=2)
  // grows / is replaced with a fresh reference each patch.
  for (const html of ["<p>two</p>", "<p>two t</p>", "<p>two thr</p>"]) {
    await act(async () => {
      w.fire({
        type: "patch",
        streamId: sid,
        patch: { newly_committed: [], active: [para(2, html, true)] },
        ...PATCH_META,
      });
    });
  }

  // The committed block's override rendered EXACTLY ONCE across all tail patches.
  expect(renders.filter((id) => id === 1).length).toBe(1);
  // Sanity: the tail DID re-render (it changed every patch) — proves the harness
  // is actually flushing renders and the "once" above isn't a no-render artifact.
  expect(renders.filter((id) => id === 2).length).toBeGreaterThan(1);
  expect(host.innerHTML).toContain("1"); // committed block is still on screen
});

// react-3 (predicate): blocksEqual is the memo gate. Same field values → equal
// (skip re-render); a change in html / open / components identity → not equal.
test("react-3: blocksEqual is true for equal field values and false when html/open/components differ", () => {
  const comps = { Paragraph: (p: BlockComponentProps) => createElement("p", null, p.block.id) };
  const a = para(1, "<p>x</p>", false);
  // A DIFFERENT object with the SAME field values — mirrors what the renderer
  // sees when a committed block keeps its reference (here we go further: even a
  // distinct object with identical fields compares equal).
  const aClone = para(1, "<p>x</p>", false);

  expect(blocksEqual({ block: a, components: comps }, { block: aClone, components: comps })).toBe(true);

  // html differs
  expect(
    blocksEqual({ block: a, components: comps }, { block: para(1, "<p>y</p>", false), components: comps }),
  ).toBe(false);
  // open differs
  expect(
    blocksEqual({ block: a, components: comps }, { block: para(1, "<p>x</p>", true), components: comps }),
  ).toBe(false);
  // components identity differs (a fresh object busts the memo)
  expect(
    blocksEqual(
      { block: a, components: comps },
      { block: aClone, components: { ...comps } },
    ),
  ).toBe(false);
});

// react-7: USEFLUXSTREAM-STABLE — re-rendering with the SAME stream identity but a
// new onError must NOT re-pipe the stream (pipeFrom called once) and must NOT
// abort the in-flight pipe; the latest onError still wins (read through a ref).
test("react-7: same stream + new onError does not re-pipe or abort, and the latest onError wins", async () => {
  // Control pipeFrom: capture the abort signal and a reject handle so we can
  // force the source to error on demand, deterministically.
  let captured: { signal?: AbortSignal; reject: (e: Error) => void } | null = null;
  const pipeSpy = spyOn(FluxClient.prototype, "pipeFrom").mockImplementation(function (
    this: FluxClient,
    _source,
    opts?: { signal?: AbortSignal },
  ) {
    return new Promise<void>((_resolve, reject) => {
      captured = { signal: opts?.signal, reject };
    });
  });
  try {
    // A stable stream identity reused across re-renders.
    const stream: AsyncIterable<string> = {
      [Symbol.asyncIterator]() {
        return { next: () => Promise.resolve({ value: undefined as unknown as string, done: true }) };
      },
    };

    const aCalls: Error[] = [];
    const bCalls: Error[] = [];
    const a = (e: Error) => aCalls.push(e);
    const b = (e: Error) => bCalls.push(e);

    function Probe({ onError }: { onError: (e: Error) => void }) {
      useFluxStream(stream, { onError });
      return createElement("div", null, "ok");
    }

    const { root } = await mount(createElement(Probe, { onError: a }));
    await act(async () => {
      await tick();
    });
    expect(pipeSpy.mock.calls.length).toBe(1); // piped once on mount
    expect(captured).not.toBeNull();
    expect(captured!.signal?.aborted).toBe(false); // pipe is live

    // Re-render with the SAME stream identity but a NEW onError (b).
    await act(async () => {
      root.render(createElement(Probe, { onError: b }));
    });
    await act(async () => {
      await tick();
    });
    // The stream wasn't re-piped (onError identity change must not re-subscribe)
    // and the original pipe wasn't aborted.
    expect(pipeSpy.mock.calls.length).toBe(1);
    expect(captured!.signal?.aborted).toBe(false);

    // Now force the (still single) pipe to reject — the LATEST onError (b) must
    // fire, proving the ref read picks up the newest handler, not the one bound
    // at the pipe's start (a).
    const boom = new Error("source exploded");
    await act(async () => {
      captured!.reject(boom);
      await tick();
    });
    expect(aCalls.length).toBe(0); // the stale handler never fires
    expect(bCalls).toEqual([boom]); // the current handler fires, exactly once

    await act(async () => {
      root.unmount();
    });
  } finally {
    pipeSpy.mockRestore();
  }
});

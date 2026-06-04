import { test, expect, beforeAll, spyOn } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { createElement, act } from "react";
import type { FromWorker, ToWorker, WorkerLike } from "../src/types";
import { FluxClient, FluxPool } from "../src/client";
import { FluxMarkdown, useFluxStream, useFluxMarkdownString } from "../src/react";

// Synchronous fake worker (same shape as the other suites).
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

// Every worker the shared default pool builds, captured file-wide so the
// liveness test can find a client's stream by its (monotonic) streamId even
// when the pool reuses a worker created by an earlier test.
const allWorkers: FakeWorker[] = [];

let win: GlobalWindow;
beforeAll(() => {
  win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.window = win;
  g.navigator = win.navigator;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  // The default pool builds `new Worker(...)`; capture them. Most tests assert
  // on the client's own append/finalize (prototype spies) — robust to the
  // pool reusing workers — but the liveness test needs the worker to fire a
  // patch back through.
  g.Worker = class extends FakeWorker {
    constructor() {
      super();
      allWorkers.push(this);
    }
  } as unknown;
  (g as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

// Find the worker + streamId of the most recently opened stream (highest
// streamId across ANY message — append/reset/dispose all carry it, so this
// works even when a single-use stream was consumed away under StrictMode and no
// append survived).
function latestStream(): { worker: FakeWorker; streamId: number } {
  let best: { worker: FakeWorker; streamId: number } | null = null;
  for (const w of allWorkers) {
    for (const m of w.sent) {
      const sid = (m as { streamId?: number }).streamId;
      if (typeof sid === "number" && (best === null || sid > best.streamId)) {
        best = { worker: w, streamId: sid };
      }
    }
  }
  if (!best) throw new Error("no stream opened on any default-pool worker");
  return best;
}

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

test("<FluxMarkdown stream> pipes an AsyncIterable: append per chunk, then finalize once", async () => {
  const appendSpy = spyOn(FluxClient.prototype, "append");
  const finalizeSpy = spyOn(FluxClient.prototype, "finalize");
  try {
    async function* gen() {
      yield "# Hi\n";
      yield "body";
    }
    await mount(createElement(FluxMarkdown, { stream: gen() }));
    await act(async () => {
      await tick(); // let pipeFrom drain the generator
    });
    const appended = appendSpy.mock.calls.map((c) => c[0]).join("");
    expect(appended).toContain("# Hi");
    expect(appended).toContain("body");
    expect(finalizeSpy.mock.calls.length).toBe(1);
  } finally {
    appendSpy.mockRestore();
    finalizeSpy.mockRestore();
  }
});

test("useFluxStream destroys its owned client on unmount, and only then", async () => {
  let captured: FluxClient | null = null;
  function Probe({ stream }: { stream: AsyncIterable<string> }) {
    captured = useFluxStream(stream);
    return createElement("div", null, "ok");
  }
  async function* gen() {
    yield "x";
  }
  const { root } = await mount(createElement(Probe, { stream: gen() }));
  await act(async () => {
    await tick();
  });
  expect(captured).not.toBeNull();
  const destroySpy = spyOn(captured!, "destroy");
  expect(destroySpy).not.toHaveBeenCalled(); // alive while mounted
  await act(async () => {
    root.unmount();
  });
  expect(destroySpy).toHaveBeenCalledTimes(1); // destroyed exactly once, on unmount
});

test("<FluxMarkdown client> NEVER destroys the caller-owned client on unmount", async () => {
  const created: FakeWorker[] = [];
  const pool = new FluxPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 1);
  const client = new FluxClient({ pool });
  client.append(""); // force worker creation
  const destroySpy = spyOn(client, "destroy");
  const { root } = await mount(createElement(FluxMarkdown, { client }));
  await act(async () => {
    root.unmount();
  });
  expect(destroySpy).not.toHaveBeenCalled(); // ownership invariant
});

test("toggling between `client` and `stream` props does not violate the Rules of Hooks", async () => {
  const created: FakeWorker[] = [];
  const pool = new FluxPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 1);
  const caller = new FluxClient({ pool });
  caller.append("");
  async function* gen() {
    yield "a";
  }
  const { createRoot } = await import("react-dom/client");
  const host = win.document.createElement("div");
  const root = createRoot(host as unknown as Element);
  // Switch the SAME root across modes — a conditional hook would make React throw
  // 'rendered more/fewer hooks than during the previous render'.
  await act(async () => {
    root.render(createElement(FluxMarkdown, { client: caller }));
  });
  await act(async () => {
    root.render(createElement(FluxMarkdown, { stream: gen() }));
  });
  await act(async () => {
    await tick();
  });
  await act(async () => {
    root.render(createElement(FluxMarkdown, { client: caller }));
  });
  await act(async () => {
    root.unmount();
  });
  expect(true).toBe(true); // reaching here without a hooks-order throw is the assertion
});

test("React StrictMode double-mount never double-finalizes and never throws", async () => {
  const { StrictMode } = await import("react");
  const finalizeSpy = spyOn(FluxClient.prototype, "finalize");
  try {
    async function* gen() {
      yield "a";
      yield "b";
    }
    await mount(createElement(StrictMode, null, createElement(FluxMarkdown, { stream: gen() })));
    await act(async () => {
      await tick();
    });
    // The safety guarantee: a superseded effect run aborts WITHOUT finalizing,
    // so the stream finalizes at most once even under the dev double-mount.
    expect(finalizeSpy.mock.calls.length).toBeLessThanOrEqual(1);
  } finally {
    finalizeSpy.mockRestore();
  }
});

// LIVENESS (the regression the advisor caught): under StrictMode the dev
// double-mount destroys then remounts the SAME client; without reattach() its
// pool handler stays deleted and every patch is dropped → blank render. This
// asserts the client still RECEIVES patches after the StrictMode cycle.
test("React StrictMode: the owned client still receives patches (no blank render)", async () => {
  const { StrictMode } = await import("react");
  const { createRoot } = await import("react-dom/client");
  let client: FluxClient | null = null;
  function Probe({ stream }: { stream: AsyncIterable<string> }) {
    client = useFluxStream(stream);
    return createElement("div", null, "ok");
  }
  async function* gen() {
    yield "a";
  }
  const host = win.document.createElement("div");
  const root = createRoot(host as unknown as Element);
  await act(async () => {
    root.render(createElement(StrictMode, null, createElement(Probe, { stream: gen() })));
  });
  await act(async () => {
    await tick();
  });
  // Fire a patch back on the stream this client opened (highest streamId).
  const { worker: w, streamId: sid } = latestStream();
  await act(async () => {
    w.fire({
      type: "patch",
      streamId: sid,
      patch: {
        newly_committed: [
          { id: 1, kind: { type: "Paragraph" }, start: 0, end: 0, html: "<p>a</p>", open: false, speculative: false },
        ],
        active: [],
      },
      appendedBytes: 0, parseMicros: 0, retainedBytes: 0, wasmMemoryBytes: 0,
    });
  });
  // Without reattach() the StrictMode-destroyed handler would drop this patch
  // and the snapshot would stay empty (blank render). With it, the patch lands.
  expect(client!.getSnapshot().length).toBeGreaterThan(0);
  await act(async () => {
    root.unmount();
  });
});

test("#5: tag-level overrides apply to OPEN (streaming) blocks, not just settled ones", async () => {
  const created: FakeWorker[] = [];
  const pool = new FluxPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 1);
  const client = new FluxClient({ pool });
  client.append(""); // force worker creation + assign the stream id
  const sid = (created[0].sent[0] as { streamId: number }).streamId;
  // A tag-level <a> override that stamps a marker attribute.
  const components = { a: (p: Record<string, unknown>) => createElement("a", { ...p, "data-ovr": "1" }) };
  const { host } = await mount(createElement(FluxMarkdown, { client, components }));
  await act(async () => {
    created[0].fire({
      type: "patch",
      streamId: sid,
      patch: {
        newly_committed: [],
        active: [
          { id: 1, kind: { type: "Paragraph" }, start: 0, end: 0, html: '<p>see <a href="/x">link</a></p>', open: true, speculative: false },
        ],
      },
      appendedBytes: 0, parseMicros: 0, retainedBytes: 0, wasmMemoryBytes: 0,
    });
  });
  // Pre-#5, an OPEN block rendered via raw innerHTML so the override was invisible.
  expect(host.innerHTML).toContain("data-ovr");
});

test("#5: a supplied sanitize runs on component-rendered blocks (closes the bypass)", async () => {
  const created: FakeWorker[] = [];
  const pool = new FluxPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 1);
  const client = new FluxClient({ pool });
  client.append("");
  const sid = (created[0].sent[0] as { streamId: number }).streamId;
  const components = { a: (p: Record<string, unknown>) => createElement("a", p) };
  const sanitize = (html: string) => html.replace(/SECRET/g, "");
  const { host } = await mount(createElement(FluxMarkdown, { client, components, sanitize }));
  await act(async () => {
    created[0].fire({
      type: "patch",
      streamId: sid,
      patch: {
        newly_committed: [
          { id: 1, kind: { type: "Paragraph" }, start: 0, end: 0, html: "<p>SECRET data</p>", open: false, speculative: false },
        ],
        active: [],
      },
      appendedBytes: 0, parseMicros: 0, retainedBytes: 0, wasmMemoryBytes: 0,
    });
  });
  expect(host.innerHTML).not.toContain("SECRET"); // sanitize ran even with components
  expect(host.innerHTML).toContain("data"); // the rest survives
});

test("useFluxMarkdownString diffs a growing string into appends and finalizes when done", async () => {
  const appendSpy = spyOn(FluxClient.prototype, "append");
  const finalizeSpy = spyOn(FluxClient.prototype, "finalize");
  try {
    function Probe({ content, streaming }: { content: string; streaming: boolean }) {
      useFluxMarkdownString(content, { streaming });
      return createElement("div", null, "ok");
    }
    const { root } = await mount(createElement(Probe, { content: "# A\n", streaming: true }));
    await act(async () => {
      root.render(createElement(Probe, { content: "# A\nbody", streaming: false }));
    });
    const appended = appendSpy.mock.calls.map((c) => c[0]).join("");
    expect(appended).toContain("# A\n");
    expect(appended).toContain("body");
    expect(finalizeSpy.mock.calls.length).toBeGreaterThanOrEqual(1);
    await act(async () => {
      root.unmount();
    });
  } finally {
    appendSpy.mockRestore();
    finalizeSpy.mockRestore();
  }
});

test("useFluxMarkdownString: omitting `streaming` leaves it open; `streaming:false` finalizes", async () => {
  const finalizeSpy = spyOn(FluxClient.prototype, "finalize");
  try {
    function Probe({ content, streaming }: { content: string; streaming?: boolean }) {
      useFluxMarkdownString(content, streaming === undefined ? undefined : { streaming });
      return createElement("div", null, "ok");
    }
    // Omitted → never finalized (safe for a still-growing controlled string).
    const r1 = await mount(createElement(Probe, { content: "# A" }));
    expect(finalizeSpy.mock.calls.length).toBe(0);
    await act(async () => {
      r1.root.unmount();
    });
    // streaming:false → finalized, so the last block commits.
    finalizeSpy.mockClear();
    const r2 = await mount(createElement(Probe, { content: "# A", streaming: false }));
    expect(finalizeSpy.mock.calls.length).toBeGreaterThanOrEqual(1);
    await act(async () => {
      r2.root.unmount();
    });
  } finally {
    finalizeSpy.mockRestore();
  }
});

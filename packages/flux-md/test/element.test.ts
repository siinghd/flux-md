import { test, expect, beforeAll, spyOn } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import { defineFluxMarkdown, parseTriBool } from "../src/element";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// Synchronous fake worker (same pattern as dom.test.ts / pool.test.ts).
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

// Workers the element's INTERNAL (self-owned) client creates via the default
// pool route through `new Worker(...)`. Capture every constructed fake so a test
// can drive the stream the element opened.
const defaultPoolWorkers: FakeWorker[] = [];
class CapturingWorker extends FakeWorker {
  constructor() {
    super();
    defaultPoolWorkers.push(this);
  }
}

beforeAll(() => {
  const win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  g.navigator = win.navigator;
  // Custom-element registry (dom.test.ts omits this; the element needs it).
  g.customElements = win.customElements;
  // Self-owned clients build on getDefaultPool() → new Worker(...). Capture
  // them. Deliberately NO requestAnimationFrame, so mountFluxMarkdown falls to
  // synchronous sync (dom.ts line 99) — patches render immediately in tests.
  g.Worker = CapturingWorker as unknown;
  // Register AFTER customElements exists on globalThis (module top-level would
  // hit the SSR guard, since beforeAll runs after the import is evaluated).
  defineFluxMarkdown();
});

function patch(committed: Block[], active: Block[], streamId: number): FromWorker {
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

// The default pool keeps workers warm and reuses them across tests, so a new
// internal client may land on an already-created fake worker (no length delta)
// and the streamId counter is shared. Recover the worker + streamId a freshly
// connected element posted to by snapshotting `sent` lengths and finding the
// one that grew.
function snapshotSends(): number[] {
  return defaultPoolWorkers.map((w) => w.sent.length);
}
function recoverStream(snapshot: number[]): { worker: FakeWorker; sid: number } {
  for (let i = 0; i < defaultPoolWorkers.length; i++) {
    const w = defaultPoolWorkers[i];
    const prev = snapshot[i] ?? 0;
    if (w.sent.length > prev) {
      return { worker: w, sid: w.sent[prev].streamId };
    }
  }
  throw new Error("no worker received a new message");
}

// External-client harness: an isolated FakeWorker-backed pool (like dom.test).
function makeExternalClient() {
  const created: FakeWorker[] = [];
  const pool = new FluxPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 1);
  const client = new FluxClient({ pool });
  return { client, worker: () => created[0] };
}

test("tri-state bool attr parser: absent => undefined, truthy => true, falsy => false", () => {
  expect(parseTriBool(null)).toBeUndefined();
  expect(parseTriBool("")).toBe(true);
  expect(parseTriBool("true")).toBe(true);
  expect(parseTriBool("1")).toBe(true);
  expect(parseTriBool("false")).toBe(false);
  expect(parseTriBool("0")).toBe(false);
  // Unknown token falls back to library default (undefined => omit).
  expect(parseTriBool("yes")).toBeUndefined();
});

test("defineFluxMarkdown is idempotent and registers the tag", () => {
  expect(customElements.get("flux-markdown")).toBeDefined();
  // A second call must not throw (already-defined guard).
  expect(() => defineFluxMarkdown()).not.toThrow();
});

test("property-client mode: mounts on connect, NEVER destroys the caller-owned client", () => {
  const { client, worker } = makeExternalClient();
  client.append(""); // force worker creation so we can fire at it
  const sid = worker().sent[0].streamId;
  const destroySpy = spyOn(client, "destroy");

  const el = document.createElement("flux-markdown");
  el.client = client; // caller-owned
  document.body.appendChild(el); // connectedCallback → mount

  // Mounted into the element's light DOM (the element IS the container).
  expect(el.querySelector(".flux-md")).not.toBeNull();

  // Drive a patch through the external client's worker → renders synchronously.
  worker().fire(patch([para(1, "<p>hello</p>")], [], sid));
  const root = el.querySelector(".flux-md")!;
  expect(root.textContent).toContain("hello");

  el.remove(); // disconnectedCallback → handle.destroy(), but NOT client.destroy()
  // OWNERSHIP INVARIANT: a caller-owned client is never destroyed by the element.
  expect(destroySpy).not.toHaveBeenCalled();
  // The renderer's root was torn down.
  expect(el.querySelector(".flux-md")).toBeNull();

  destroySpy.mockRestore();
});

test("property-client mode honors a pre-upgrade (own-property) client assignment", () => {
  const { client, worker } = makeExternalClient();
  client.append("");
  const sid = worker().sent[0].streamId;

  // Assign `client` as an OWN property on a not-yet-upgraded element, then
  // upgrade by appending — the upgrade dance must re-route it through the setter.
  const el = document.createElement("flux-markdown") as HTMLElement & { client?: unknown };
  Object.defineProperty(el, "client", { value: client, writable: true, configurable: true });
  document.body.appendChild(el);

  worker().fire(patch([para(1, "<p>upgraded</p>")], [], sid));
  expect(el.querySelector(".flux-md")!.textContent).toContain("upgraded");
  el.remove();
});

test("self-owned mode via append()/finalize(): renders AND disconnect destroys the self-owned client", () => {
  const el = document.createElement("flux-markdown");
  document.body.appendChild(el); // no external client, no src/markdown/text → no client yet

  const snap = snapshotSends();
  el.append("# hi"); // lazily creates the internal client + mounts
  el.finalize();
  const { worker: w, sid } = recoverStream(snap);

  w.fire(patch([para(1, "<p>hi</p>")], [], sid));
  expect(el.querySelector(".flux-md")!.textContent).toContain("hi");

  const internal = el.getClient()!;
  const destroySpy = spyOn(internal, "destroy");
  el.remove(); // disconnect → self-owned client IS destroyed
  expect(destroySpy).toHaveBeenCalled();
  destroySpy.mockRestore();
});

test("self-owned mode from textContent: one-shot render on connect", () => {
  const snap = snapshotSends();
  const el = document.createElement("flux-markdown");
  el.textContent = "hello from text";
  document.body.appendChild(el); // connect → captures textContent → one-shot
  const { worker: w, sid } = recoverStream(snap);

  // append+finalize were posted to the internal worker for this stream.
  const mine = w.sent.filter((m) => m.streamId === sid);
  expect(mine.some((m) => m.type === "append" && (m as { chunk: string }).chunk === "hello from text")).toBe(true);
  expect(mine.some((m) => m.type === "finalize")).toBe(true);

  w.fire(patch([para(1, "<p>hello from text</p>")], [], sid));
  expect(el.querySelector(".flux-md")!.textContent).toContain("hello from text");

  el.remove();
});

test("self-owned client created with config from tri-state attributes", () => {
  const snap = snapshotSends();
  const el = document.createElement("flux-markdown");
  el.setAttribute("gfm-alerts", "false"); // turn OFF a default-on flag
  el.setAttribute("gfm-math", "true");
  el.setAttribute("component-tags", "Thinking, Callout");
  el.textContent = "x";
  document.body.appendChild(el);
  const { worker: w, sid } = recoverStream(snap);

  // Config rides this stream's first message (FIFO).
  const first = w.sent.find(
    (m) => m.streamId === sid && (m as { config?: unknown }).config !== undefined,
  ) as (ToWorker & { config: import("../src/types").ParserConfig }) | undefined;
  expect(first).toBeDefined();
  expect(first!.config.gfmAlerts).toBe(false);
  expect(first!.config.gfmMath).toBe(true);
  expect(first!.config.componentTags).toEqual(["Thinking", "Callout"]);

  el.remove();
});

// A fetch body whose chunks are delivered on demand: read() pends until push()
// or close() supplies the next result. Lets a test hold a stream "in flight".
function makeControllableStream() {
  const enc = new TextEncoder();
  const ready: Array<{ done: boolean; value?: Uint8Array }> = [];
  const waiters: Array<(r: { done: boolean; value?: Uint8Array }) => void> = [];
  const emit = (r: { done: boolean; value?: Uint8Array }) => {
    const w = waiters.shift();
    if (w) w(r);
    else ready.push(r);
  };
  return {
    push: (text: string) => emit({ done: false, value: enc.encode(text) }),
    close: () => emit({ done: true }),
    reader: {
      read: () =>
        new Promise<{ done: boolean; value?: Uint8Array }>((resolve) => {
          const r = ready.shift();
          if (r) resolve(r);
          else waiters.push(resolve);
        }),
    },
  };
}

const flush = () => new Promise((r) => setTimeout(r, 0));

test("rapid src switch aborts the prior fetch and never interleaves two streams into one parser", async () => {
  const streams = new Map<string, ReturnType<typeof makeControllableStream>>();
  const signals = new Map<string, AbortSignal | undefined>();
  const realFetch = (globalThis as Record<string, unknown>).fetch;
  (globalThis as Record<string, unknown>).fetch = (url: string, init?: { signal?: AbortSignal }) => {
    const s = makeControllableStream();
    streams.set(url, s);
    signals.set(url, init?.signal);
    return Promise.resolve({
      body: { getReader: () => s.reader },
      text: () => Promise.resolve(""),
    });
  };

  try {
    const snap = snapshotSends();
    const el = document.createElement("flux-markdown");
    el.setAttribute("src", "a.md");
    document.body.appendChild(el); // connect → streamFromSrc("a.md"), pends at first read
    await flush();
    const { worker: w, sid } = recoverStream(snap);

    // Switch src before A produced anything → must abort A, start B.
    el.setAttribute("src", "b.md");
    await flush();
    expect(signals.get("a.md")?.aborted).toBe(true);

    // A is superseded: its (late) chunk must NOT reach the parser.
    streams.get("a.md")!.push("AAA");
    streams.get("a.md")!.close();
    await flush();

    // B streams normally.
    streams.get("b.md")!.push("BBB");
    streams.get("b.md")!.close();
    await flush();

    const appends = w.sent
      .filter((m) => m.streamId === sid && m.type === "append")
      .map((m) => (m as { chunk: string }).chunk)
      .join("");
    expect(appends).toContain("BBB");
    expect(appends).not.toContain("AAA");

    el.remove();
  } finally {
    (globalThis as Record<string, unknown>).fetch = realFetch;
  }
});

test("switching from src to a markdown attribute supersedes the in-flight fetch", async () => {
  const streams = new Map<string, ReturnType<typeof makeControllableStream>>();
  const signals = new Map<string, AbortSignal | undefined>();
  const realFetch = (globalThis as Record<string, unknown>).fetch;
  (globalThis as Record<string, unknown>).fetch = (url: string, init?: { signal?: AbortSignal }) => {
    const s = makeControllableStream();
    streams.set(url, s);
    signals.set(url, init?.signal);
    return Promise.resolve({ body: { getReader: () => s.reader }, text: () => Promise.resolve("") });
  };

  try {
    const snap = snapshotSends();
    const el = document.createElement("flux-markdown");
    el.setAttribute("src", "a.md");
    document.body.appendChild(el); // connect → streamFromSrc("a.md"), pends at first read
    await flush();
    const { worker: w, sid } = recoverStream(snap);

    // Drop src and supply inline markdown instead → one-shot, must abort the fetch.
    el.removeAttribute("src");
    el.setAttribute("markdown", "# inline");
    await flush();
    expect(signals.get("a.md")?.aborted).toBe(true);

    // The stale fetch resolving late must NOT append into the one-shot stream.
    streams.get("a.md")!.push("AAA");
    streams.get("a.md")!.close();
    await flush();

    const appends = w.sent
      .filter((m) => m.streamId === sid && m.type === "append")
      .map((m) => (m as { chunk: string }).chunk)
      .join("");
    expect(appends).toContain("# inline");
    expect(appends).not.toContain("AAA");

    el.remove();
  } finally {
    (globalThis as Record<string, unknown>).fetch = realFetch;
  }
});

test("config attribute change while a caller-owned client is set is ignored (warns)", () => {
  const { client } = makeExternalClient();
  client.append("");
  const el = document.createElement("flux-markdown");
  el.client = client;
  document.body.appendChild(el);

  const warnSpy = spyOn(console, "warn").mockImplementation(() => {});
  el.setAttribute("gfm-math", "true"); // config change with external client
  expect(warnSpy).toHaveBeenCalled();
  warnSpy.mockRestore();
  el.remove();
});

import { test, expect, beforeAll, spyOn } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { BrookClient, BrookPool, __resetDefaultPool } from "../src/client";
import { defineBrookMarkdown, parseTriBool } from "../src/element";
import type { Block, FromWorker, LinkClickInfo, ToWorker, WorkerLike } from "../src/types";

// The `<brook-markdown>` custom element's public surface (not on lib.dom's HTMLElement).
type BrookEl = HTMLElement & {
  client?: BrookClient;
  onLinkClick?: (event: MouseEvent, link: LinkClickInfo) => void;
  finalize(): void;
  getClient(): BrookClient | null;
};

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
  // Constructible click events for the delegated `onLinkClick` tests (bun has
  // no MouseEvent of its own).
  g.MouseEvent = win.MouseEvent;
  // Custom-element registry (dom.test.ts omits this; the element needs it).
  g.customElements = win.customElements;
  // Self-owned clients build on getDefaultPool() → new Worker(...). Capture
  // them. Deliberately NO requestAnimationFrame, so mountBrookMarkdown falls to
  // synchronous sync (dom.ts line 99) — patches render immediately in tests.
  g.Worker = CapturingWorker as unknown;
  // Reset the process-wide default pool so this file's self-owned clients build
  // FRESH workers via THIS file's CapturingWorker (tracked in defaultPoolWorkers).
  // Without this, bun's shared test process may leave the pool warm with another
  // file's worker, and recoverStream() can't find the stream — a flaky,
  // file-order-dependent failure (the publish gate hit exactly this in CI).
  __resetDefaultPool();
  // Register AFTER customElements exists on globalThis (module top-level would
  // hit the SSR guard, since beforeAll runs after the import is evaluated).
  defineBrookMarkdown();
});

function patch(committed: Block[], active: Block[], streamId: number): FromWorker {
  return {
    type: "patch",
    streamId,
    patch: JSON.stringify({ newly_committed: committed, active }),
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
  const pool = new BrookPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 1);
  const client = new BrookClient({ pool });
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

test("defineBrookMarkdown is idempotent and registers the tag", () => {
  expect(customElements.get("brook-markdown")).toBeDefined();
  // A second call must not throw (already-defined guard).
  expect(() => defineBrookMarkdown()).not.toThrow();
});

test("property-client mode: mounts on connect, NEVER destroys the caller-owned client", () => {
  const { client, worker } = makeExternalClient();
  client.append(""); // force worker creation so we can fire at it
  const sid = worker().sent[0].streamId;
  const destroySpy = spyOn(client, "destroy");

  const el = document.createElement("brook-markdown") as BrookEl;
  el.client = client; // caller-owned
  document.body.appendChild(el); // connectedCallback → mount

  // Mounted into the element's light DOM (the element IS the container).
  expect(el.querySelector(".brook-md")).not.toBeNull();

  // Drive a patch through the external client's worker → renders synchronously.
  worker().fire(patch([para(1, "<p>hello</p>")], [], sid));
  const root = el.querySelector(".brook-md")!;
  expect(root.textContent).toContain("hello");

  el.remove(); // disconnectedCallback → handle.destroy(), but NOT client.destroy()
  // OWNERSHIP INVARIANT: a caller-owned client is never destroyed by the element.
  expect(destroySpy).not.toHaveBeenCalled();
  // The renderer's root was torn down.
  expect(el.querySelector(".brook-md")).toBeNull();

  destroySpy.mockRestore();
});

test("property-client mode honors a pre-upgrade (own-property) client assignment", () => {
  const { client, worker } = makeExternalClient();
  client.append("");
  const sid = worker().sent[0].streamId;

  // Assign `client` as an OWN property on a not-yet-upgraded element, then
  // upgrade by appending — the upgrade dance must re-route it through the setter.
  const el = document.createElement("brook-markdown") as HTMLElement & { client?: unknown };
  Object.defineProperty(el, "client", { value: client, writable: true, configurable: true });
  document.body.appendChild(el);

  worker().fire(patch([para(1, "<p>upgraded</p>")], [], sid));
  expect(el.querySelector(".brook-md")!.textContent).toContain("upgraded");
  el.remove();
});

test("self-owned mode via append()/finalize(): renders AND disconnect destroys the self-owned client", () => {
  const el = document.createElement("brook-markdown") as BrookEl;
  document.body.appendChild(el); // no external client, no src/markdown/text → no client yet

  const snap = snapshotSends();
  el.append("# hi"); // lazily creates the internal client + mounts
  el.finalize();
  const { worker: w, sid } = recoverStream(snap);

  w.fire(patch([para(1, "<p>hi</p>")], [], sid));
  expect(el.querySelector(".brook-md")!.textContent).toContain("hi");

  const internal = el.getClient()!;
  const destroySpy = spyOn(internal, "destroy");
  el.remove(); // disconnect → self-owned client IS destroyed
  expect(destroySpy).toHaveBeenCalled();
  destroySpy.mockRestore();
});

test("self-owned mode from textContent: one-shot render on connect", () => {
  const snap = snapshotSends();
  const el = document.createElement("brook-markdown");
  el.textContent = "hello from text";
  document.body.appendChild(el); // connect → captures textContent → one-shot
  const { worker: w, sid } = recoverStream(snap);

  // append+finalize were posted to the internal worker for this stream.
  const mine = w.sent.filter((m) => m.streamId === sid);
  expect(mine.some((m) => m.type === "append" && (m as { chunk: string }).chunk === "hello from text")).toBe(true);
  expect(mine.some((m) => m.type === "finalize")).toBe(true);

  w.fire(patch([para(1, "<p>hello from text</p>")], [], sid));
  expect(el.querySelector(".brook-md")!.textContent).toContain("hello from text");

  el.remove();
});

test("self-owned client created with config from tri-state attributes", () => {
  const snap = snapshotSends();
  const el = document.createElement("brook-markdown");
  el.setAttribute("gfm-alerts", "false"); // turn OFF a default-on flag
  el.setAttribute("gfm-math", "true");
  el.setAttribute("gfm-tagfilter", "true");
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
  expect(first!.config.gfmTagfilter).toBe(true);
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
    const el = document.createElement("brook-markdown");
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
    const el = document.createElement("brook-markdown");
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

test("public append() supersedes an in-flight src fetch (no interleave)", async () => {
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
    const el = document.createElement("brook-markdown");
    el.setAttribute("src", "a.md");
    document.body.appendChild(el); // connect → streamFromSrc("a.md"), pends at first read
    await flush();
    const { worker: w, sid } = recoverStream(snap);

    // Manually drive the stream — must abort the fetch and win.
    el.append("MANUAL");
    await flush();
    expect(signals.get("a.md")?.aborted).toBe(true);

    // The stale fetch resolving late must not interleave its chunk.
    streams.get("a.md")!.push("AAA");
    streams.get("a.md")!.close();
    await flush();

    const appends = w.sent
      .filter((m) => m.streamId === sid && m.type === "append")
      .map((m) => (m as { chunk: string }).chunk)
      .join("");
    expect(appends).toContain("MANUAL");
    expect(appends).not.toContain("AAA");

    el.remove();
  } finally {
    (globalThis as Record<string, unknown>).fetch = realFetch;
  }
});

test("config attribute change while a caller-owned client is set is ignored (warns)", () => {
  const { client } = makeExternalClient();
  client.append("");
  const el = document.createElement("brook-markdown") as BrookEl;
  el.client = client;
  document.body.appendChild(el);

  const warnSpy = spyOn(console, "warn").mockImplementation(() => {});
  el.setAttribute("gfm-math", "true"); // config change with external client
  expect(warnSpy).toHaveBeenCalled();
  warnSpy.mockRestore();
  el.remove();
});

// ---------------------------------------------------------------------------
// Renderer (MountOption) attributes + the delegated onLinkClick property
// ---------------------------------------------------------------------------

// A settled link, and the shape the core emits while a link's URL is still
// streaming (`data-brook-pending`, no href) — see test/pending-link.test.ts.
const LINK_HTML =
  '<p>See the <a href="https://example.com/q3" target="_blank" rel="noopener noreferrer nofollow">Earnings Call</a> today.</p>';

function clickIt(el: Element): boolean {
  const ev = new MouseEvent("click", { bubbles: true, cancelable: true });
  el.dispatchEvent(ev);
  return ev.defaultPrevented;
}

test("stick-to-bottom / virtualize attributes reach the renderer's mount options", () => {
  const { client, worker } = makeExternalClient();
  client.append("");
  const sid = worker().sent[0].streamId;

  const el = document.createElement("brook-markdown") as BrookEl;
  el.setAttribute("stick-to-bottom", "");
  el.setAttribute("virtualize", "");
  el.client = client;
  document.body.appendChild(el); // connect → mount reads both attributes

  worker().fire(patch([para(1, "<p>closed</p>")], [para(2, "<p>tail", true)], sid));
  const root = el.querySelector(".brook-md")!;

  // stick-to-bottom: the scroll-snap sentinel is pinned last.
  const last = root.children[root.children.length - 1] as HTMLElement;
  expect(last.className).toContain("brook-bottom-anchor");
  expect(last.style.scrollSnapAlign).toBe("end");

  // virtualize: the CLOSED block gets content-visibility, the open tail never does.
  expect((root.children[0] as HTMLElement).style.contentVisibility).toBe("auto");
  expect((root.children[1] as HTMLElement).style.contentVisibility).toBe("");

  el.remove();
});

test("no stick-to-bottom / virtualize attributes → renderer defaults (no sentinel, no deferral)", () => {
  const { client, worker } = makeExternalClient();
  client.append("");
  const sid = worker().sent[0].streamId;

  const el = document.createElement("brook-markdown") as BrookEl;
  el.client = client;
  document.body.appendChild(el);

  worker().fire(patch([para(1, "<p>closed</p>")], [], sid));
  const root = el.querySelector(".brook-md")!;
  expect(root.querySelector(".brook-bottom-anchor")).toBeNull();
  expect((root.children[0] as HTMLElement).style.contentVisibility).toBe("");

  el.remove();
});

test("toggling stick-to-bottom AFTER mount applies, with no config-immutability warning", () => {
  // A renderer option touches no ParserConfig, so it is honoured even with a
  // CALLER-owned client — where a config attribute would warn and be ignored.
  const { client, worker } = makeExternalClient();
  client.append("");
  const sid = worker().sent[0].streamId;

  const el = document.createElement("brook-markdown") as BrookEl;
  el.client = client;
  document.body.appendChild(el);
  worker().fire(patch([para(1, "<p>body</p>")], [], sid));
  expect(el.querySelector(".brook-bottom-anchor")).toBeNull();

  const warnSpy = spyOn(console, "warn").mockImplementation(() => {});
  el.setAttribute("stick-to-bottom", "");
  expect(warnSpy).not.toHaveBeenCalled();
  warnSpy.mockRestore();

  // Remounted with the option on, and the committed document is redrawn.
  const root = el.querySelector(".brook-md")!;
  expect(root.querySelector(".brook-bottom-anchor")).not.toBeNull();
  expect(root.textContent).toContain("body");

  // ...and removing the attribute takes it back off.
  el.removeAttribute("stick-to-bottom");
  expect(el.querySelector(".brook-bottom-anchor")).toBeNull();

  el.remove();
});

test("the .onLinkClick property is forwarded to the mount and fires on a link click", () => {
  const { client, worker } = makeExternalClient();
  client.append("");
  const sid = worker().sent[0].streamId;

  const calls: LinkClickInfo[] = [];
  const el = document.createElement("brook-markdown") as BrookEl;
  el.onLinkClick = (_e, link) => {
    calls.push(link);
  };
  el.client = client;
  document.body.appendChild(el);

  worker().fire(patch([para(1, LINK_HTML)], [], sid));
  const a = el.querySelector("a[href]") as HTMLAnchorElement;
  clickIt(a);

  expect(calls.length).toBe(1);
  expect(calls[0].href).toBe("https://example.com/q3");
  expect(calls[0].text).toBe("Earnings Call");
  expect(calls[0].element).toBe(a);

  // Re-assigning the property while connected remounts against the new handler.
  const later: LinkClickInfo[] = [];
  el.onLinkClick = (e, link) => {
    later.push(link);
    e.preventDefault();
  };
  expect(clickIt(el.querySelector("a[href]")!)).toBe(true);
  expect(later.length).toBe(1);
  expect(calls.length).toBe(1); // the replaced handler is gone

  el.remove();
});

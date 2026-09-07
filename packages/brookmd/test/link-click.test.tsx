import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { createElement, act } from "react";
import { BrookClient, BrookPool } from "../src/client";
import { BrookMarkdown } from "../src/react";
import { mountBrookMarkdown } from "../src/dom";
import type {
  Block,
  BlockComponentProps,
  FromWorker,
  LinkClickInfo,
  ToWorker,
  WorkerLike,
} from "../src/types";

// The delegated `onLinkClick` hook, in both renderers that own a root node: ONE
// listener on `.brook-md`, the anchor resolved from the event target. The design
// contract is as much about what it does NOT do — no per-anchor prop, no
// per-block work — so the React half also pins that a changing handler identity
// re-renders zero blocks.
//
// DOM registration mirrors test/dom.test.ts + test/rerender-react.test.tsx: no
// requestAnimationFrame, so mountBrookMarkdown falls to synchronous sync.
let win: GlobalWindow;
beforeAll(() => {
  win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.window = win;
  g.navigator = win.navigator;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  (g as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

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

function makeClient() {
  const created: FakeWorker[] = [];
  const pool = new BrookPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 1);
  const client = new BrookClient({ pool });
  return { client, worker: () => created[0] };
}

function patch(committed: Block[], active: Block[], streamId = 1): FromWorker {
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

// A settled link, and the shape the core emits for a link whose label has
// rendered but whose URL is still streaming: `data-brook-pending` and NO href
// (same fixture wire shape as test/pending-link.test.ts).
const LINK_HTML =
  '<p>See the <a href="https://example.com/q3" target="_blank" rel="noopener noreferrer nofollow">Earnings Call</a> today.</p>';
const PENDING_HTML =
  '<p>Check the <a data-brook-pending="" target="_blank" rel="noopener noreferrer nofollow">Pending Label</a></p>';

// A snapshot-only client stand-in for the React renderer (no worker needed: the
// tests never patch, they click). Built once per test so `subscribe`/
// `getSnapshot` keep stable identities for useSyncExternalStore.
function fakeClient(blocks: Block[]) {
  return {
    subscribe: (_fn: () => void) => () => {},
    getSnapshot: () => blocks,
  } as unknown as BrookClient;
}

// Dispatch a real bubbling, cancelable click and report whether a handler
// cancelled it. Used instead of `.click()` so `defaultPrevented` is observable.
// The parameter is structural because happy-dom's `Element` and lib.dom's are
// two distinct nominal types in this suite (the globals are happy-dom's, the
// compiler's are lib.dom's).
function click(el: { dispatchEvent(ev: unknown): unknown }): boolean {
  const ev = new win.MouseEvent("click", { bubbles: true, cancelable: true });
  el.dispatchEvent(ev);
  return ev.defaultPrevented;
}

// Query one anchor as the DOM type the hook hands back, so the identity
// assertions compare like with like.
function anchor(scope: { querySelector(sel: string): unknown }, sel = "a[href]"): HTMLAnchorElement {
  return scope.querySelector(sel) as HTMLAnchorElement;
}

async function mount(node: ReturnType<typeof createElement>) {
  const { createRoot } = await import("react-dom/client");
  const host = win.document.createElement("div");
  const root = createRoot(host as unknown as Element);
  await act(async () => {
    root.render(node);
  });
  return { host, root };
}

// ---------------------------------------------------------------------------
// React
// ---------------------------------------------------------------------------

test("React: onLinkClick fires once per click with the href, text and element", async () => {
  const calls: LinkClickInfo[] = [];
  const { host } = await mount(
    createElement(BrookMarkdown, {
      client: fakeClient([para(1, LINK_HTML), para(2, PENDING_HTML, true)]),
      onLinkClick: (_e, link) => {
        calls.push(link);
      },
    }),
  );
  const root = host.querySelector(".brook-md")!;
  const a = anchor(root);

  click(a);
  expect(calls.length).toBe(1);
  expect(calls[0].href).toBe("https://example.com/q3");
  expect(calls[0].text).toBe("Earnings Call");
  expect(calls[0].element).toBe(a);

  // A click that resolves to no anchor at all is ignored, and never double-fires.
  click(anchor(root, "p"));
  expect(calls.length).toBe(1); // a paragraph is not a link
});

test("React: a still-streaming (data-brook-pending) anchor is never reported", async () => {
  const calls: LinkClickInfo[] = [];
  const { host } = await mount(
    createElement(BrookMarkdown, {
      client: fakeClient([para(2, PENDING_HTML, true)]),
      onLinkClick: (_e, link) => {
        calls.push(link);
      },
    }),
  );
  const pending = anchor(host, "a[data-brook-pending]");
  expect(pending).not.toBeNull();
  expect(pending.hasAttribute("href")).toBe(false);

  click(pending);
  expect(calls.length).toBe(0);
});

test("React: preventDefault() in the handler cancels the navigation", async () => {
  const { host } = await mount(
    createElement(BrookMarkdown, {
      client: fakeClient([para(1, LINK_HTML)]),
      onLinkClick: (e) => e.preventDefault(),
    }),
  );
  expect(click(anchor(host))).toBe(true);
});

test("React: onLinkClick is delegated — a fresh handler identity re-renders NO blocks", async () => {
  // A block-kind override logs every render of its block. `onLinkClick` is never
  // threaded into BlockView, so changing its identity must leave the per-block
  // memo intact (unlike `components` / `decorators`, which do bust it).
  const renders: number[] = [];
  const components = {
    Paragraph: (p: BlockComponentProps) => {
      renders.push(p.block.id);
      return createElement("p", null, String(p.block.id));
    },
  };
  const client = fakeClient([para(1, LINK_HTML)]);

  const { root } = await mount(
    createElement(BrookMarkdown, { client, components, onLinkClick: () => {} }),
  );
  expect(renders.length).toBe(1);

  // Re-render with a BRAND NEW onLinkClick closure; everything else identical.
  await act(async () => {
    root.render(createElement(BrookMarkdown, { client, components, onLinkClick: () => {} }));
  });
  expect(renders.length).toBe(1); // the block memo held — zero re-renders

  // Sanity: the harness CAN observe a re-render (a fresh `components` identity
  // is the documented memo-buster), so the assertion above is not vacuous.
  await act(async () => {
    root.render(
      createElement(BrookMarkdown, { client, components: { ...components }, onLinkClick: () => {} }),
    );
  });
  expect(renders.length).toBe(2);
});

// ---------------------------------------------------------------------------
// DOM renderer
// ---------------------------------------------------------------------------

test("DOM: onLinkClick fires with the href/text/element and skips pending anchors", () => {
  const calls: LinkClickInfo[] = [];
  const { client, worker } = makeClient();
  client.append(""); // force worker creation so we can fire at it
  const container = document.createElement("div");
  const handle = mountBrookMarkdown(client, container, {
    batch: false,
    onLinkClick: (_e, link) => {
      calls.push(link);
    },
  });

  worker().fire(patch([para(1, LINK_HTML)], [para(2, PENDING_HTML, true)]));
  const root = container.querySelector(".brook-md")!;
  const a = anchor(root);

  click(a);
  expect(calls.length).toBe(1);
  expect(calls[0].href).toBe("https://example.com/q3");
  expect(calls[0].text).toBe("Earnings Call");
  expect(calls[0].element).toBe(a);

  // The streaming tail's pending anchor has no URL yet → never reported.
  click(anchor(root, "a[data-brook-pending]"));
  expect(calls.length).toBe(1);

  handle.destroy();
});

test("DOM: preventDefault() in the handler cancels the navigation", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountBrookMarkdown(client, container, {
    batch: false,
    onLinkClick: (e) => e.preventDefault(),
  });

  worker().fire(patch([para(1, LINK_HTML)], []));
  expect(click(anchor(container))).toBe(true);
  handle.destroy();
});

test("DOM: destroy() removes the delegated listener", () => {
  const calls: LinkClickInfo[] = [];
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountBrookMarkdown(client, container, {
    batch: false,
    onLinkClick: (_e, link) => {
      calls.push(link);
    },
  });

  worker().fire(patch([para(1, LINK_HTML)], []));
  // Hold the anchor: destroy() detaches the root, but a click still bubbles
  // within the detached tree — so a leaked listener would fire here.
  const a = anchor(container);
  click(a);
  expect(calls.length).toBe(1);

  handle.destroy();
  click(a);
  expect(calls.length).toBe(1); // unchanged: the listener is gone
});

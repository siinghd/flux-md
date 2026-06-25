import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { createElement, act, type ReactNode } from "react";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";
import { FluxClient, FluxPool } from "../src/client";
import { FluxMarkdown } from "../src/react";
import { mountFluxMarkdown } from "../src/dom";

// The keyed List renderer (item #5): when `blockData` is on, an OPEN list carries
// per-item inner `<li>` HTML in `kind.data.items`. The React renderer stamps one
// memoized `<li key={i}>` per item (so React reuses the unchanged items as the
// list streams) and the DOM renderer stamps a real `<li>` per item — both routing
// inner HTML through the same components/sanitize path the whole-block renderer
// uses. Off-path (no items) falls back to the opaque whole-block HTML.

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
  (g as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

function makeClient() {
  const created: FakeWorker[] = [];
  const pool = new FluxPool(() => {
    const w = new FakeWorker();
    created.push(w);
    return w;
  }, 1);
  const client = new FluxClient({ pool });
  return { client, worker: () => created[0] };
}

function patch(committed: Block[], active: Block[], streamId = 1): FromWorker {
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

// A List as the core emits it under setBlockData(true): kind.data carries
// `ordered`, `start`, and `items` (each `{ html }` = the inner <li> HTML).
function listBlock(
  id: number,
  ordered: boolean,
  start: number,
  items: string[],
  html: string,
  open = true,
): Block {
  return {
    id,
    kind: { type: "List", data: { ordered, start, items: items.map((h) => ({ html: h })) } },
    start: 0,
    end: 0,
    html,
    open,
    speculative: open,
  };
}

async function mountReact(node: ReactNode) {
  const { createRoot } = await import("react-dom/client");
  const host = win.document.createElement("div");
  const root = createRoot(host as unknown as Element);
  await act(async () => {
    root.render(node);
  });
  return { host, root };
}

test("React: an OPEN list with items renders one <li> per item (keyed)", async () => {
  const { client, worker } = makeClient();
  client.append("");
  const { host } = await mountReact(createElement(FluxMarkdown, { client }));

  await act(async () => {
    worker().fire(
      patch([], [listBlock(1, false, 1, ["a", "<strong>b</strong>"], "<ul>\n<li>a</li>\n<li><strong>b</strong></li>\n</ul>")]),
    );
  });

  const lis = host.querySelectorAll("li");
  expect(lis.length).toBe(2);
  expect(lis[0].textContent).toBe("a");
  expect(host.querySelector("li strong")?.textContent).toBe("b");
  // It is a real <ul>, not the opaque whole-block <div> innerHTML.
  expect(host.querySelector("ul")).not.toBeNull();
});

test("React: the ordered start attribute rides on the keyed <ol>", async () => {
  const { client, worker } = makeClient();
  client.append("");
  const { host } = await mountReact(createElement(FluxMarkdown, { client }));

  await act(async () => {
    worker().fire(
      patch([], [listBlock(1, true, 7, ["g", "h"], '<ol start="7">\n<li>g</li>\n<li>h</li>\n</ol>')]),
    );
  });

  const ol = host.querySelector("ol");
  expect(ol).not.toBeNull();
  expect(ol!.getAttribute("start")).toBe("7");
  expect(host.querySelectorAll("li").length).toBe(2);
});

test("React: unchanged item <li> nodes are reused as the list streams (keyed memo)", async () => {
  const { client, worker } = makeClient();
  client.append("");
  const { host } = await mountReact(createElement(FluxMarkdown, { client }));

  // Patch 1: two items.
  await act(async () => {
    worker().fire(patch([], [listBlock(1, false, 1, ["a", "b"], "<ul>\n<li>a</li>\n<li>b</li>\n</ul>")]));
  });
  const firstLi = host.querySelectorAll("li")[0];

  // Patch 2: same first two items + a new third item (the streamed tail grew).
  await act(async () => {
    worker().fire(
      patch([], [listBlock(1, false, 1, ["a", "b", "c"], "<ul>\n<li>a</li>\n<li>b</li>\n<li>c</li>\n</ul>")]),
    );
  });

  const lis = host.querySelectorAll("li");
  expect(lis.length).toBe(3);
  // The unchanged first <li> DOM node is the SAME element across the patch — the
  // memoized keyed item was reused, not re-created (the whole point of the path).
  expect(lis[0]).toBe(firstLi);
  expect(lis[2].textContent).toBe("c");
});

test("React: a tag-level inline override applies through item HTML (components-aware)", async () => {
  const { client, worker } = makeClient();
  client.append("");
  // An `a` override proves item inner HTML routes through htmlToReact(html,
  // components) — NOT a raw innerHTML hole.
  const A = (p: { href?: string; children?: ReactNode }) =>
    createElement("a", { "data-tagged": "1", href: p.href }, p.children);
  const { host } = await mountReact(createElement(FluxMarkdown, { client, components: { a: A } }));

  await act(async () => {
    worker().fire(
      patch([], [listBlock(1, false, 1, ['see <a href="/x">x</a>'], '<ul>\n<li>see <a href="/x">x</a></li>\n</ul>')]),
    );
  });

  const a = host.querySelector("li a");
  expect(a).not.toBeNull();
  expect(a!.getAttribute("data-tagged")).toBe("1");
});

test("React: a tag-level ul/li override keeps the whole-block path (keyed path skipped)", async () => {
  const { client, worker } = makeClient();
  client.append("");
  const Ul = (p: { children?: ReactNode }) => createElement("ul", { "data-mine": "1" }, p.children);
  const { host } = await mountReact(createElement(FluxMarkdown, { client, components: { ul: Ul } }));

  await act(async () => {
    worker().fire(patch([], [listBlock(1, false, 1, ["a", "b"], "<ul>\n<li>a</li>\n<li>b</li>\n</ul>")]));
  });

  // The user's <ul> override owns the wrapper — proving the keyed path stepped
  // aside so tag-level list overrides still control the element.
  expect(host.querySelector("ul[data-mine]")).not.toBeNull();
});

test("React: a closed list (no keyed path) renders via the opaque whole-block HTML", async () => {
  const { client, worker } = makeClient();
  client.append("");
  const { host } = await mountReact(createElement(FluxMarkdown, { client }));

  await act(async () => {
    worker().fire(
      patch([listBlock(1, false, 1, ["a"], "<ul>\n<li>a</li>\n</ul>", false)], []),
    );
  });

  // Still a correct list; committed blocks always use the whole-block HTML.
  expect(host.querySelectorAll("li").length).toBe(1);
  expect(host.querySelector("li")?.textContent).toBe("a");
});

test("React: sanitize is applied to each item's inner HTML", async () => {
  const { client, worker } = makeClient();
  client.append("");
  const sanitize = (h: string) => h.replace(/danger/g, "safe");
  const { host } = await mountReact(createElement(FluxMarkdown, { client, sanitize }));

  await act(async () => {
    worker().fire(
      patch([], [listBlock(1, false, 1, ["danger one", "two"], "<ul>\n<li>danger one</li>\n<li>two</li>\n</ul>")]),
    );
  });

  expect(host.querySelectorAll("li")[0].textContent).toBe("safe one");
});

test("DOM: an OPEN list with items renders one <li> per item, sanitize-aware", async () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = win.document.createElement("div");
  const sanitize = (h: string) => h.replace(/danger/g, "safe");
  const handle = mountFluxMarkdown(client, container as unknown as HTMLElement, { sanitize, batch: false });

  worker().fire(
    patch([], [listBlock(1, true, 3, ["danger a", "b"], '<ol start="3">\n<li>danger a</li>\n<li>b</li>\n</ol>')]),
  );

  const ol = container.querySelector("ol");
  expect(ol).not.toBeNull();
  expect(ol!.getAttribute("start")).toBe("3");
  const lis = container.querySelectorAll("li");
  expect(lis.length).toBe(2);
  expect(lis[0].textContent).toBe("safe a");
  handle.destroy();
});

test("DOM: a list parsed WITHOUT items falls back to the opaque whole-block HTML", async () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = win.document.createElement("div");
  const handle = mountFluxMarkdown(client, container as unknown as HTMLElement, { batch: false });

  // blockData off ⇒ no items channel ⇒ keyed path returns null ⇒ opaque HTML.
  const off: Block = {
    id: 1,
    kind: { type: "List", data: { ordered: false } },
    start: 0,
    end: 0,
    html: "<ul>\n<li>a</li>\n<li>b</li>\n</ul>",
    open: true,
    speculative: true,
  };
  worker().fire(patch([], [off]));

  expect(container.querySelectorAll("li").length).toBe(2);
  expect(container.querySelector("ul")).not.toBeNull();
  handle.destroy();
});

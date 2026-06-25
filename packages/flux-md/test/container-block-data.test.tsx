import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { createElement, act } from "react";
import type {
  Block,
  FromWorker,
  ToWorker,
  WorkerLike,
  ContainerData,
  BlockComponentProps,
} from "../src/types";
import { FluxClient, FluxPool } from "../src/client";
import { FluxMarkdown } from "../src/react";

// PROOF: a Blockquote / Alert block's structured `kind.data.nested` channel
// (opt-in `blockData`) lets the renderer paint the inner sub-blocks KEYED — so
// while the container streams, only its open last child re-renders. We drive
// synthetic patches (a FakeWorker) carrying the exact wire shape the Rust core
// emits, and assert (a) `props.container` reaches an override, (b) the default
// renderer builds a real `<blockquote>` / alert `<div>` wrapper with one keyed
// node per nested entry (alert title preserved), and (c) committed child nodes
// are REUSED across a streaming patch — the whole point of keying.

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

async function mount(node: ReturnType<typeof createElement>) {
  const { createRoot } = await import("react-dom/client");
  const host = win.document.createElement("div");
  const root = createRoot(host as unknown as Element);
  await act(async () => {
    root.render(node);
  });
  return { host, root };
}

// The wire shape the Rust core emits for an OPEN blockquote under blockData:
// `kind.data.nested` is the ordered per-paragraph HTML, and `html` is the
// full wrapper (the fragments are byte-present inside it).
function blockquoteBlock(id: number, paras: string[], open = true): Block {
  const nested = paras.map((html) => ({ html }));
  const data: ContainerData = { nested };
  return {
    id,
    kind: { type: "Blockquote", data },
    start: 0,
    end: 0,
    html: "<blockquote>\n" + paras.join("\n") + "\n</blockquote>",
    open,
    speculative: open,
  };
}

function alertBlock(id: number, paras: string[], open = true): Block {
  const nested = paras.map((html) => ({ html }));
  const data: ContainerData = { nested };
  const title = '<p class="markdown-alert-title">Note</p>';
  return {
    id,
    kind: { type: "Alert", data: { nested, kind: "note" } as unknown as ContainerData },
    start: 0,
    end: 0,
    html:
      '<div class="markdown-alert markdown-alert-note" data-alert="note" role="note">\n' +
      title +
      "\n" +
      paras.join("\n") +
      "\n</div>",
    open,
    speculative: open,
  };
}

test("Blockquote override receives props.container.nested (keyed structured data)", async () => {
  const { client, worker } = makeClient();
  client.append("");

  let seen: ContainerData | undefined;
  function Blockquote(props: BlockComponentProps) {
    seen = props.container;
    return createElement(
      "blockquote",
      { "data-testid": "bq" },
      (props.container?.nested ?? []).map((n, i) =>
        createElement("div", { key: i, dangerouslySetInnerHTML: { __html: n.html } }),
      ),
    );
  }

  await mount(createElement(FluxMarkdown, { client, components: { Blockquote } }));
  await act(async () => {
    worker().fire(patch([], [blockquoteBlock(1, ["<p>first</p>", "<p>second</p>"])]));
  });

  expect(seen).toEqual({ nested: [{ html: "<p>first</p>" }, { html: "<p>second</p>" }] });
});

test("default renderer paints an open blockquote with one KEYED node per nested paragraph", async () => {
  const { client, worker } = makeClient();
  client.append("");

  // A non-Blockquote component triggers the components (htmlToReact) path
  // without overriding the blockquote itself, so the default KeyedContainer runs.
  function Heading() {
    return createElement("h1", null, "x");
  }

  const { host } = await mount(createElement(FluxMarkdown, { client, components: { Heading } }));
  await act(async () => {
    worker().fire(patch([], [blockquoteBlock(1, ["<p>alpha</p>", "<p>beta</p>"])]));
  });

  const bq = host.querySelector("blockquote");
  expect(bq).not.toBeNull();
  const ps = bq!.querySelectorAll("p");
  expect(ps.length).toBe(2);
  expect(ps[0].textContent).toBe("alpha");
  expect(ps[1].textContent).toBe("beta");
});

test("default renderer keeps the alert title and wrapper attributes, keys the body", async () => {
  const { client, worker } = makeClient();
  client.append("");
  function Heading() {
    return createElement("h1", null, "x");
  }

  const { host } = await mount(createElement(FluxMarkdown, { client, components: { Heading } }));
  await act(async () => {
    worker().fire(patch([], [alertBlock(1, ["<p>body one</p>"])]));
  });

  const alert = host.querySelector("div.markdown-alert");
  expect(alert).not.toBeNull();
  expect(alert!.getAttribute("data-alert")).toBe("note");
  expect(alert!.getAttribute("role")).toBe("note");
  // Title preserved as the first child (it is the wrapper, never in `nested`).
  const title = alert!.querySelector("p.markdown-alert-title");
  expect(title).not.toBeNull();
  expect(title!.textContent).toBe("Note");
  // Body paragraph rendered from `nested`.
  const bodies = alert!.querySelectorAll("p:not(.markdown-alert-title)");
  expect(bodies.length).toBe(1);
  expect(bodies[0].textContent).toBe("body one");
});

test("committed nested child nodes are REUSED across a streaming patch (the keying win)", async () => {
  const { client, worker } = makeClient();
  client.append("");
  function Heading() {
    return createElement("h1", null, "x");
  }

  const { host } = await mount(createElement(FluxMarkdown, { client, components: { Heading } }));

  // Tick 1: first paragraph committed + an open second paragraph.
  await act(async () => {
    worker().fire(patch([], [blockquoteBlock(1, ["<p>committed</p>", "<p>open</p>"])]));
  });
  const firstP = host.querySelector("blockquote")!.querySelectorAll("p")[0];
  expect(firstP.textContent).toBe("committed");

  // Tick 2: same first paragraph (stable html), the open one grew + a new para.
  await act(async () => {
    worker().fire(
      patch([], [blockquoteBlock(1, ["<p>committed</p>", "<p>open more</p>", "<p>third</p>"])]),
    );
  });
  const ps2 = host.querySelector("blockquote")!.querySelectorAll("p");
  expect(ps2.length).toBe(3);
  // The committed paragraph's DOM node is the SAME instance (memoized per key
  // on stable html) — only the changed/new children re-rendered.
  expect(ps2[0]).toBe(firstP);
  expect(ps2[1].textContent).toBe("open more");
  expect(ps2[2].textContent).toBe("third");
});

test("no blockData → no props.container, default renderer uses the opaque-html path", async () => {
  const { client, worker } = makeClient();
  client.append("");

  let hadField = true;
  let container: ContainerData | undefined;
  function Blockquote(props: BlockComponentProps) {
    hadField = "container" in props && props.container !== undefined;
    container = props.container;
    return createElement("blockquote", null, "x");
  }

  // A Blockquote block as the core emits it with blockData OFF: no `data`.
  const offBlock: Block = {
    id: 1,
    kind: { type: "Blockquote" },
    start: 0,
    end: 0,
    html: "<blockquote>\n<p>x</p>\n</blockquote>",
    open: true,
    speculative: true,
  };

  await mount(createElement(FluxMarkdown, { client, components: { Blockquote } }));
  await act(async () => {
    worker().fire(patch([], [offBlock]));
  });

  expect(hadField).toBe(false);
  expect(container).toBeUndefined();
});

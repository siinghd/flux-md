import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import { mountFluxMarkdown } from "../src/dom";
import type { Block, ContainerData, FromWorker, ToWorker, WorkerLike } from "../src/types";

// PROOF (DOM renderer): an OPEN Blockquote / Alert with structured
// `kind.data.nested` (blockData on) is painted with one KEYED child node per
// inner sub-block instead of a single full-wrapper `innerHTML`. Each child's
// html is the SAME safe-allowlist fragment as the matching slice of `b.html`,
// so this is not a new innerHTML hole. A `sanitize` hook disables it (must run
// over the full string) and the opaque-html path takes over.

beforeAll(() => {
  const win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  g.navigator = win.navigator;
});

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

function blockquoteBlock(id: number, paras: string[], open = true): Block {
  const data: ContainerData = { nested: paras.map((html) => ({ html })) };
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
  const data = { nested: paras.map((html) => ({ html })), kind: "note" };
  return {
    id,
    kind: { type: "Alert", data: data as unknown as ContainerData },
    start: 0,
    end: 0,
    html:
      '<div class="markdown-alert markdown-alert-note" data-alert="note" role="note">\n' +
      '<p class="markdown-alert-title">Note</p>\n' +
      paras.join("\n") +
      "\n</div>",
    open,
    speculative: open,
  };
}

test("DOM: open blockquote paints one KEYED node per nested paragraph", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  // `components` present (without a Blockquote override) so the generic path runs.
  const handle = mountFluxMarkdown(client, container, { batch: false, components: { Heading: () => "<h1>x</h1>" } });
  const root = container.querySelector(".flux-md")!;

  worker().fire(patch([], [blockquoteBlock(1, ["<p>alpha</p>", "<p>beta</p>"])]));

  const bq = root.querySelector("blockquote");
  expect(bq).not.toBeNull();
  const ps = bq!.querySelectorAll("p");
  expect(ps.length).toBe(2);
  expect(ps[0].textContent).toBe("alpha");
  expect(ps[1].textContent).toBe("beta");
  handle.destroy();
});

test("DOM: open alert keeps its title + wrapper attrs, keys the body paragraphs", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false, components: { Heading: () => "<h1>x</h1>" } });
  const root = container.querySelector(".flux-md")!;

  worker().fire(patch([], [alertBlock(1, ["<p>body one</p>", "<p>body two</p>"])]));

  const alert = root.querySelector("div.markdown-alert");
  expect(alert).not.toBeNull();
  expect(alert!.getAttribute("data-alert")).toBe("note");
  expect(alert!.getAttribute("role")).toBe("note");
  const title = alert!.querySelector("p.markdown-alert-title");
  expect(title).not.toBeNull();
  expect(title!.textContent).toBe("Note");
  const bodies = alert!.querySelectorAll("p:not(.markdown-alert-title)");
  expect(bodies.length).toBe(2);
  expect(bodies[0].textContent).toBe("body one");
  expect(bodies[1].textContent).toBe("body two");
  handle.destroy();
});

test("DOM: a sanitize hook disables the keyed path (falls back to full-wrapper innerHTML)", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  let sanitizeCalls = 0;
  const handle = mountFluxMarkdown(client, container, {
    batch: false,
    components: { Heading: () => "<h1>x</h1>" },
    sanitize: (html) => {
      sanitizeCalls++;
      return html;
    },
  });
  const root = container.querySelector(".flux-md")!;

  worker().fire(patch([], [blockquoteBlock(1, ["<p>alpha</p>"])]));

  // The sanitizer ran over the full wrapper (the keyed path was skipped), and the
  // blockquote still renders correctly via the opaque-html path.
  expect(sanitizeCalls).toBeGreaterThan(0);
  const bq = root.querySelector("blockquote");
  expect(bq).not.toBeNull();
  expect(bq!.querySelector("p")!.textContent).toBe("alpha");
  handle.destroy();
});

test("DOM: a closed blockquote with blockData falls through to the opaque-html path", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false, components: { Heading: () => "<h1>x</h1>" } });
  const root = container.querySelector(".flux-md")!;

  // open=false → the keyed fast path is gated off; the full wrapper renders.
  worker().fire(patch([blockquoteBlock(1, ["<p>done</p>"], false)], []));

  const bq = root.querySelector("blockquote");
  expect(bq).not.toBeNull();
  expect(bq!.querySelector("p")!.textContent).toBe("done");
  handle.destroy();
});

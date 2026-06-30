import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import { mountFluxMarkdown } from "../src/dom";
import { safeUrl } from "../src/html-to-react";
import type { Block, Decorator, FromWorker, ToWorker, UrlTransform, WorkerLike } from "../src/types";

let win: GlobalWindow;
beforeAll(() => {
  win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.window = win;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  g.NodeFilter = win.NodeFilter;
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

function drive(client: FluxClient, worker: () => FakeWorker, msg: FromWorker) {
  client.append("");
  worker().fire(msg);
}

// Mount, push one committed paragraph, return the rendered block element.
function mountWith(html: string, options: Parameters<typeof mountFluxMarkdown>[2]) {
  const { client, worker } = makeClient();
  const container = win.document.createElement("div");
  const handle = mountFluxMarkdown(client, container as unknown as HTMLElement, { batch: false, ...options });
  drive(client, worker, patch([para(1, html)], []));
  const block = container.querySelector(".flux-block-paragraph")!;
  return { container, handle, block, client, worker };
}

const pctDecorators: Decorator[] = [
  {
    match: /\d+%/g,
    replace: (t) => {
      const mark = win.document.createElement("mark");
      mark.textContent = t;
      return mark;
    },
  },
];

// (b) wrap matched text
test("DOM: decorators wrap matched inline text", () => {
  const { block } = mountWith("<p>Revenue 50% YoY</p>", { decorators: pctDecorators });
  expect(block.innerHTML).toContain("<mark>50%</mark>");
  expect(block.textContent).toBe("Revenue 50% YoY");
});

// (c) skip inside a / code / pre
test("DOM: decorator skips text inside a / code by default", () => {
  const { block } = mountWith('<p>10% <a href="/y">20%</a> <code>30%</code></p>', { decorators: pctDecorators });
  expect(block.innerHTML).toContain("<mark>10%</mark>");
  expect(block.querySelector("a")!.innerHTML).toBe("20%"); // untouched
  expect(block.querySelector("code")!.innerHTML).toBe("30%"); // untouched
  expect(block.querySelectorAll("mark").length).toBe(1);
});

// (d) wrapLink-equivalent: a user Node with a javascript: href routed via safeUrl
test("DOM: a decorator Node with a javascript: href is neutralized via safeUrl", () => {
  const decs: Decorator[] = [
    {
      match: /LINK/g,
      replace: (t) => {
        const a = win.document.createElement("a");
        a.setAttribute("href", safeUrl("javascript:alert(1)"));
        a.textContent = t;
        return a;
      },
    },
  ];
  const { block } = mountWith("<p>see LINK now</p>", { decorators: decs });
  const a = block.querySelector("a")!;
  expect(a.getAttribute("href")).toBe("#");
  expect(block.innerHTML).not.toContain("javascript:");
});

// (e) urlTransform output re-sanitized
test("DOM: urlTransform output that returns a javascript: URL is re-sanitized away", () => {
  const evil: UrlTransform = () => "javascript:alert(1)";
  const { block } = mountWith('<p><a href="/safe">x</a></p>', { urlTransform: evil });
  expect(block.querySelector("a")!.getAttribute("href")).toBe("#");
  expect(block.innerHTML).not.toContain("javascript:");
});

test("DOM: urlTransform rewrites href/src/poster with ctx then re-sanitizes", () => {
  const seen: string[] = [];
  const tx: UrlTransform = (_u, ctx) => {
    seen.push(ctx.tag + ":" + ctx.attr);
    return "https://cdn.test/" + ctx.attr;
  };
  const { block } = mountWith('<p><a href="/a">x</a><img src="/b" alt="i"></p>', { urlTransform: tx });
  expect(block.querySelector("a")!.getAttribute("href")).toBe("https://cdn.test/href");
  expect(block.querySelector("img")!.getAttribute("src")).toBe("https://cdn.test/src");
  expect(seen).toContain("a:href");
  expect(seen).toContain("img:src");
});

// (f) /g multiple matches
test("DOM: a /g matcher wraps every match in one text node", () => {
  const { block } = mountWith("<p>10% and 20% and 30%</p>", { decorators: pctDecorators });
  expect(block.querySelectorAll("mark").length).toBe(3);
  expect(block.textContent).toBe("10% and 20% and 30%");
});

// O(n): a committed block's node is reused untouched (never re-walked) on a
// tail-only patch — the DOM analogue of the React render-once guarantee.
test("DOM: a committed decorated block's node is frozen across tail patches", () => {
  const { client, worker } = makeClient();
  const container = win.document.createElement("div");
  mountFluxMarkdown(client, container as unknown as HTMLElement, { batch: false, decorators: pctDecorators });
  // Commit id=1 (one match) + open tail id=2 (no match).
  drive(client, worker, patch([para(1, "<p>up 10%</p>")], [para(2, "<p>tw</p>", true)]));
  const committedNode = container.querySelector(".flux-block-paragraph")!;
  expect(committedNode.querySelector("mark")!.textContent).toBe("10%");

  // Grow the tail several times; the committed node identity must not change.
  for (const html of ["<p>two</p>", "<p>two t</p>", "<p>two three</p>"]) {
    worker().fire(patch([], [para(2, html, true)]));
  }
  const stillCommitted = container.querySelectorAll(".flux-block-paragraph")[0];
  expect(stillCommitted).toBe(committedNode); // same node object → never rebuilt/re-walked
  expect(committedNode.querySelectorAll("mark").length).toBe(1);
});

// Parity: streamed (grow open then commit) equals one-shot final HTML.
test("DOM: streamed-then-committed output equals one-shot committed output", () => {
  const full = "Up 10% and 20% done";
  const one = mountWith(`<p>${full}</p>`, { decorators: pctDecorators });

  const { client, worker } = makeClient();
  const container = win.document.createElement("div");
  mountFluxMarkdown(client, container as unknown as HTMLElement, { batch: false, decorators: pctDecorators });
  client.append("");
  for (let i = 1; i <= full.length; i++) {
    worker().fire(patch([], [para(1, `<p>${full.slice(0, i)}</p>`, true)]));
  }
  worker().fire(patch([para(1, `<p>${full}</p>`)], []));
  const streamedBlock = container.querySelector(".flux-block-paragraph")!;
  expect(streamedBlock.innerHTML).toBe(one.block.innerHTML);
});

import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import { mountFluxMarkdown } from "../src/dom";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// Register a DOM in this file only (no global preload). Bun shares globals
// across files in a run, but the other suites are DOM-free (html-to-react uses
// renderToStaticMarkup, client/pool touch no DOM), so a leaked `document` is
// harmless — verified by running the full suite. We deliberately do NOT install
// requestAnimationFrame, so `mountFluxMarkdown` with the default batch falls to
// synchronous sync; the tests that want batching pass `batch: false` explicitly.
beforeAll(() => {
  const win = new GlobalWindow();
  const g = globalThis as Record<string, unknown>;
  g.document = win.document;
  g.HTMLElement = win.HTMLElement;
  g.Node = win.Node;
  g.navigator = win.navigator;
});

// Synchronous fake worker (same pattern as pool.test.ts): records posts and
// lets the test fire patch responses back through the registered listener.
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
  // The client's append/finalize would post to the worker; the test drives the
  // store directly by firing `patch` messages at the worker's listener.
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

const para = (id: number, html: string, open = false): Block => ({
  id, kind: { type: "Paragraph" }, start: 0, end: html.length, html, open, speculative: false,
});

const code = (id: number, html: string, open: boolean): Block => ({
  id, kind: { type: "CodeBlock", data: { lang: "rust" } }, start: 0, end: html.length, html, open, speculative: false,
});

// The fake worker has no real stream; FluxClient.append() posts a message we
// don't need. We acquire the worker handle by triggering one append so the
// pool creates the worker, then fire patches at its listener.
function drive(client: FluxClient, worker: () => FakeWorker, msg: FromWorker) {
  worker().fire(msg);
}

test("a committed block's DOM node is the same element across later patches", () => {
  const { client, worker } = makeClient();
  client.append(""); // force worker creation so we can fire at it
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false });

  drive(client, worker, patch([para(1, "<p>first</p>")], [para(2, "<p>act", true)]));
  const root = container.querySelector(".flux-md")!;
  const firstNode = root.children[0];
  expect(firstNode.outerHTML).toContain("first");

  // Grow the active tail; block 1 is NOT re-sent → its node must be untouched.
  drive(client, worker, patch([], [para(2, "<p>active grown</p>", true)]));
  expect(root.children[0]).toBe(firstNode); // SAME element reference

  // Commit block 2; block 1 still untouched.
  drive(client, worker, patch([para(2, "<p>second</p>")], []));
  expect(root.children[0]).toBe(firstNode);

  handle.destroy();
});

test("streaming code block renders plain body; on commit it highlights (once)", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false });

  // Open: plain body (raw `.flux-code-body > div`, no highlight token spans),
  // streaming pill present, no copy button.
  drive(client, worker, patch([], [code(1, '<pre><code class="language-rust" data-lang="rust">fn ma', true)]));
  const root = container.querySelector(".flux-md")!;
  let node = root.children[0];
  expect(node.querySelector(".flux-code-streaming-pill")).not.toBeNull();
  expect(node.querySelector(".flux-code-body > div")).not.toBeNull(); // raw body
  expect(node.querySelector(".flux-code-body > pre")).toBeNull(); // not highlighted yet
  expect(node.querySelector(".t-kw")).toBeNull(); // highlighter did not run
  expect(node.querySelector(".flux-code-copy")).toBeNull();

  // Commit (open:false): highlighted <pre><code> appears, copy button present.
  drive(client, worker, patch([code(1, '<pre><code class="language-rust" data-lang="rust">fn main(){}</code></pre>', false)], []));
  node = root.children[0];
  const pre = node.querySelector(".flux-code-body > pre code");
  expect(pre).not.toBeNull();
  expect(node.querySelector(".flux-code-streaming-pill")).toBeNull();
  expect(node.querySelector(".flux-code-copy")).not.toBeNull();
  // Highlighter ran: `fn` is a rust keyword → wrapped in a token span.
  expect(node.querySelector(".t-kw")).not.toBeNull();

  handle.destroy();
});

test("block order is preserved as blocks commit incrementally", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false });
  const root = container.querySelector(".flux-md")!;

  drive(client, worker, patch([para(1, "<p>a</p>")], []));
  drive(client, worker, patch([para(2, "<p>b</p>"), para(3, "<p>c</p>")], [para(4, "<p>d", true)]));

  const texts = Array.from(root.children).map((c) => c.textContent);
  expect(texts).toEqual(["a", "b", "c", "d"]);

  handle.destroy();
});

test("sanitize is applied to the open/speculative tail", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const marker = "<!--SANITIZED-->";
  const handle = mountFluxMarkdown(client, container, {
    batch: false,
    sanitize: (html) => marker + html,
  });
  const root = container.querySelector(".flux-md")!;

  drive(client, worker, patch([], [para(1, "<p>streaming tail</p>", true)]));
  const node = root.children[0] as HTMLElement;
  expect(node.className).toContain("flux-open");
  expect(node.innerHTML).toContain("SANITIZED"); // sanitize ran on the open tail

  handle.destroy();
});

test("destroy() unsubscribes (later patches don't mutate the DOM) and removes root", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false });
  const root = container.querySelector(".flux-md")!;

  drive(client, worker, patch([para(1, "<p>before</p>")], []));
  expect(root.children.length).toBe(1);

  handle.destroy();
  expect(container.querySelector(".flux-md")).toBeNull(); // root removed

  // A later patch must not throw and must not resurrect/ mutate the DOM.
  drive(client, worker, patch([para(2, "<p>after</p>")], []));
  expect(container.querySelector(".flux-md")).toBeNull();
  expect(container.children.length).toBe(0);
});

test("stickToBottom keeps a scroll-snap anchor pinned last", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false, stickToBottom: true });
  const root = container.querySelector(".flux-md")!;

  drive(client, worker, patch([para(1, "<p>a</p>")], [para(2, "<p>b", true)]));
  const last = root.children[root.children.length - 1] as HTMLElement;
  expect(last.className).toContain("flux-bottom-anchor");
  expect(last.style.scrollSnapAlign).toBe("end");
  // Anchor stays last even after more blocks land.
  drive(client, worker, patch([para(2, "<p>b</p>")], [para(3, "<p>c", true)]));
  const stillLast = root.children[root.children.length - 1] as HTMLElement;
  expect(stillLast).toBe(last);
  expect(stillLast.className).toContain("flux-bottom-anchor");

  handle.destroy();
});

test("virtualize sets content-visibility on closed blocks, never on the open tail", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false, virtualize: true });
  const root = container.querySelector(".flux-md")!;

  // Block 1 closed → virtualized with the Paragraph estimate; block 2 open tail
  // → never deferred (heights change fastest there).
  drive(client, worker, patch([para(1, "<p>a</p>")], [para(2, "<p>b", true)]));
  const closed = root.children[0] as HTMLElement;
  const open = root.children[1] as HTMLElement;
  expect(closed.style.contentVisibility).toBe("auto");
  expect(closed.style.containIntrinsicSize).toBe("auto 80px"); // Paragraph
  expect(open.style.contentVisibility).toBe("");

  handle.destroy();
});

test("block-kind override replaces the whole block and receives props", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  let seen: { language?: string; text?: string; open?: boolean } | null = null;
  const handle = mountFluxMarkdown(client, container, {
    batch: false,
    components: {
      CodeBlock: (p) => {
        seen = { language: p.language, text: p.text, open: p.open };
        const el = document.createElement("div");
        el.className = "mine";
        el.textContent = p.language ?? "";
        return el;
      },
    },
  });
  const root = container.querySelector(".flux-md")!;

  drive(client, worker, patch([code(1, '<pre><code class="language-rust" data-lang="rust">fn main(){}</code></pre>', false)], []));
  const node = root.children[0] as HTMLElement;
  expect(node.className).toBe("mine");
  expect(node.textContent).toBe("rust");
  expect(seen!.language).toBe("rust");
  expect(seen!.text).toBe("fn main(){}");
  expect(seen!.open).toBe(false);
  // The default highlighter is suppressed when components.CodeBlock is given.
  expect(node.querySelector(".flux-code-block")).toBeNull();

  handle.destroy();
});

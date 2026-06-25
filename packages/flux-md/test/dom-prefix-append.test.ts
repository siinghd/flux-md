import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import { mountFluxMarkdown } from "../src/dom";
import type { Block, BlockKind, FromWorker, ToWorker, WorkerLike } from "../src/types";

// DOM-only suite (same setup as rerender-dom.test.ts): register happy-dom and a
// synchronous fake worker so the test can fire patch messages directly.
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

function block(
  id: number,
  html: string,
  kind: BlockKind = { type: "Paragraph" },
  open = true,
): Block {
  return { id, kind, start: 0, end: html.length, html, open, speculative: false };
}

function drive(client: FluxClient, worker: () => FakeWorker, msg: FromWorker) {
  worker().fire(msg);
}

// Mount a fresh client from a single final-snapshot block and read its root
// innerHTML — the canonical full-rebuild output the fast path must match.
function fullRebuildInnerHTML(b: Block): string {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false, highlightCode: false });
  drive(client, worker, patch([], [b]));
  const html = (container.querySelector(".flux-md") as HTMLElement).innerHTML;
  handle.destroy();
  return html;
}

// Count insertAdjacentHTML('beforeend', …) calls on every block node created
// under `root` by wrapping the prototype method for the duration of a run.
function withAppendSpy<T>(fn: (count: () => number) => T): T {
  const proto = (globalThis as unknown as { HTMLElement: { prototype: HTMLElement } }).HTMLElement
    .prototype as unknown as { insertAdjacentHTML: HTMLElement["insertAdjacentHTML"] };
  const real = proto.insertAdjacentHTML;
  let n = 0;
  proto.insertAdjacentHTML = function (this: HTMLElement, ...args: Parameters<HTMLElement["insertAdjacentHTML"]>) {
    if (args[0] === "beforeend") n++;
    return real.apply(this, args);
  } as HTMLElement["insertAdjacentHTML"];
  try {
    return fn(() => n);
  } finally {
    proto.insertAdjacentHTML = real;
  }
}

// --------------------------------------------------------------------------
// FAST PATH HITS — appended whole top-level siblings match the full rebuild.
// --------------------------------------------------------------------------

test("PREFIX-APPEND: paragraph-sibling growth splices the suffix and matches the full rebuild", () => {
  withAppendSpy((count) => {
    const { client, worker } = makeClient();
    client.append("");
    const container = document.createElement("div");
    const handle = mountFluxMarkdown(client, container, { batch: false });
    const root = container.querySelector(".flux-md") as HTMLElement;

    const steps = [
      "<p>a</p>\n",
      "<p>a</p>\n<p>b</p>\n",
      "<p>a</p>\n<p>b</p>\n<p>c</p>\n",
    ];
    drive(client, worker, patch([], [block(1, steps[0])]));
    const blockNode = root.children[0];
    const before = count();

    for (let i = 1; i < steps.length; i++) {
      drive(client, worker, patch([], [block(1, steps[i])]));
      // Same node: spliced in place, not rebuilt/replaced.
      expect(root.children[0]).toBe(blockNode);
    }
    // Each growth step took the append fast path.
    expect(count() - before).toBe(steps.length - 1);

    // Byte-identical to a fresh full rebuild of the final html.
    expect(root.innerHTML).toBe(fullRebuildInnerHTML(block(1, steps[steps.length - 1])));
    handle.destroy();
  });
});

test("PREFIX-APPEND: streaming code-fence lines (no highlighter, generic path) grow via append and match full rebuild", () => {
  // With highlightCode:false a CodeBlock renders through the generic
  // innerHTML path, so block-level sibling growth is append-eligible. Use a
  // shape whose growth is a pure prefix extension of WHOLE <pre> siblings.
  withAppendSpy((count) => {
    const { client, worker } = makeClient();
    client.append("");
    const container = document.createElement("div");
    const handle = mountFluxMarkdown(client, container, { batch: false, highlightCode: false });
    const root = container.querySelector(".flux-md") as HTMLElement;

    const kind: BlockKind = { type: "CodeBlock" };
    const steps = [
      "<pre><code>line1\n</code></pre>",
      "<pre><code>line1\n</code></pre><pre><code>line2\n</code></pre>",
      "<pre><code>line1\n</code></pre><pre><code>line2\n</code></pre><pre><code>line3\n</code></pre>",
    ];
    drive(client, worker, patch([], [block(1, steps[0], kind)]));
    const node = root.children[0];
    const before = count();
    for (let i = 1; i < steps.length; i++) {
      drive(client, worker, patch([], [block(1, steps[i], kind)]));
      expect(root.children[0]).toBe(node);
    }
    expect(count() - before).toBe(steps.length - 1);
    expect(root.innerHTML).toBe(
      fullRebuildInnerHTML(block(1, steps[steps.length - 1], kind)),
    );
    handle.destroy();
  });
});

// --------------------------------------------------------------------------
// FALL-BACK — partial trailing element / sanitizer / fingerprint change.
// --------------------------------------------------------------------------

test("PREFIX-APPEND: a streaming table with a partial trailing <tr> FALLS BACK to a full rebuild", () => {
  withAppendSpy((count) => {
    const { client, worker } = makeClient();
    client.append("");
    const container = document.createElement("div");
    const handle = mountFluxMarkdown(client, container, { batch: false });
    const root = container.querySelector(".flux-md") as HTMLElement;

    const kind: BlockKind = { type: "Table" };
    // Real streaming-table html keeps the <table>…</table> wrapper closed every
    // tick, so growth happens INSIDE an unclosed-at-the-boundary subtree — never
    // a depth-0 prefix extension. The fast path must not fire.
    const steps = [
      "<table><tbody><tr><td>1</td></tr></tbody></table>",
      "<table><tbody><tr><td>1</td></tr><tr><td>2</td></tr></tbody></table>",
    ];
    drive(client, worker, patch([], [block(1, steps[0], kind)]));
    const before = count();
    drive(client, worker, patch([], [block(1, steps[1], kind)]));
    expect(count() - before).toBe(0); // no append fast path
    // Still correct via full rebuild.
    expect(root.innerHTML).toBe(fullRebuildInnerHTML(block(1, steps[1], kind)));
    handle.destroy();
  });
});

test("PREFIX-APPEND: sanitizer present always falls back (suffix can't be independently sanitized)", () => {
  withAppendSpy((count) => {
    const { client, worker } = makeClient();
    client.append("");
    const container = document.createElement("div");
    const sanitize = (h: string) => h; // identity, but presence forces fallback
    const handle = mountFluxMarkdown(client, container, { batch: false, sanitize });
    const root = container.querySelector(".flux-md") as HTMLElement;

    drive(client, worker, patch([], [block(1, "<p>a</p>")]));
    const before = count();
    drive(client, worker, patch([], [block(1, "<p>a</p><p>b</p>")]));
    expect(count() - before).toBe(0); // sanitizer → no append
    expect(root.children[0].innerHTML).toBe("<p>a</p><p>b</p>");
    handle.destroy();
  });
});

test("PREFIX-APPEND: a mid-element prefix extension (text continues) FALLS BACK", () => {
  withAppendSpy((count) => {
    const { client, worker } = makeClient();
    client.append("");
    const container = document.createElement("div");
    const handle = mountFluxMarkdown(client, container, { batch: false });
    const root = container.querySelector(".flux-md") as HTMLElement;

    drive(client, worker, patch([], [block(1, "<p>hel")]));
    const before = count();
    // suffix "lo</p>" does not begin a new '<' sibling → fall back, full rebuild.
    drive(client, worker, patch([], [block(1, "<p>hello</p>")]));
    expect(count() - before).toBe(0);
    expect(root.innerHTML).toBe(fullRebuildInnerHTML(block(1, "<p>hello</p>")));
    handle.destroy();
  });
});

test("PREFIX-APPEND: open→closed commit falls back (fingerprint change), node rebuilt once", () => {
  withAppendSpy((count) => {
    const { client, worker } = makeClient();
    client.append("");
    const container = document.createElement("div");
    const handle = mountFluxMarkdown(client, container, { batch: false });
    const root = container.querySelector(".flux-md") as HTMLElement;

    drive(client, worker, patch([], [block(1, "<p>a</p>")]));
    const before = count();
    // Same html-prefix but now committed (open false): not eligible (open
    // differs) → rebuild, not append. Append count stays 0.
    drive(client, worker, patch([{ ...block(1, "<p>a</p><p>b</p>"), open: false }], []));
    expect(count() - before).toBe(0);
    expect(root.innerHTML).toBe(
      fullRebuildInnerHTML({ ...block(1, "<p>a</p><p>b</p>"), open: false }),
    );
    handle.destroy();
  });
});

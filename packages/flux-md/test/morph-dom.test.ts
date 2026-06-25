import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { FluxClient, FluxPool } from "../src/client";
import { mountFluxMarkdown } from "../src/dom";
import { morph, htmlToFragment } from "../src/morph";
import type { Block, FromWorker, ToWorker, WorkerLike } from "../src/types";

// Mirror dom.test.ts: register a DOM in this file only. We deliberately do NOT
// install requestAnimationFrame so the default batch falls to synchronous sync;
// every mount here passes `batch: false` explicitly anyway.
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

const para = (id: number, html: string, open = false): Block => ({
  id, kind: { type: "Paragraph" }, start: 0, end: html.length, html, open, speculative: false,
});

function drive(client: FluxClient, worker: () => FakeWorker, msg: FromWorker) {
  worker().fire(msg);
}

// --------------------------------------------------------------------------
// morph() unit behaviour
// --------------------------------------------------------------------------

test("morph: result outerHTML matches an innerHTML rebuild for arbitrary trees", () => {
  const cases = [
    "<p>hello</p>",
    "<p>hello world</p>",
    "<p>hello <strong>world</strong></p>",
    "<ul><li>one</li><li>two</li></ul>",
    "<ul><li>one</li><li>two</li><li>three</li></ul>",
    "<table><thead><tr><th>a</th><th>b</th></tr></thead><tbody><tr><td>1</td><td>2</td></tr></tbody></table>",
    "<p>a</p><blockquote><p>q</p></blockquote>",
  ];
  // Morph through the whole sequence in one live element; after each step it
  // must equal a fresh innerHTML build of the same HTML.
  const live = document.createElement("div");
  for (const html of cases) {
    morph(live, html);
    const reference = document.createElement("div");
    reference.innerHTML = html;
    expect(live.innerHTML).toBe(reference.innerHTML);
  }
});

test("morph: a growing text node keeps its node identity (in-place character data)", () => {
  const live = document.createElement("div");
  morph(live, "<p>hel</p>");
  const p = live.querySelector("p")!;
  const textNode = p.firstChild!;
  expect(textNode.nodeType).toBe(3);

  // Append-only token growth → same <p> element AND same text node, just updated.
  morph(live, "<p>hello</p>");
  expect(live.querySelector("p")).toBe(p); // element identity preserved
  expect(p.firstChild).toBe(textNode); // text node identity preserved
  expect(textNode.nodeValue).toBe("hello");
});

test("morph: id-keyed children are matched across an inserted sibling", () => {
  const live = document.createElement("div");
  morph(live, '<section id="a">A</section><section id="b">B</section>');
  const a = live.querySelector("#a")!;
  const b = live.querySelector("#b")!;

  // Insert a new keyed node between them: a and b must keep identity.
  morph(live, '<section id="a">A</section><section id="x">X</section><section id="b">B2</section>');
  expect(live.querySelector("#a")).toBe(a);
  expect(live.querySelector("#b")).toBe(b);
  expect(live.querySelector("#b")!.textContent).toBe("B2"); // morphed in place
  expect(live.querySelector("#x")).not.toBeNull();
});

test("htmlToFragment parses into a DocumentFragment", () => {
  const frag = htmlToFragment("<p>x</p><p>y</p>");
  expect(frag.childNodes.length).toBe(2);
  expect((frag.firstChild as Element).tagName).toBe("P");
});

// --------------------------------------------------------------------------
// dom.ts opt-in morphOpenBlocks integration
// --------------------------------------------------------------------------

test("morphOpenBlocks: morphed open-tail subtree equals the innerHTML-rebuilt subtree across a streaming sequence", () => {
  // Two mounts driven by the SAME patch sequence: one with morphOpenBlocks on,
  // one with the default rebuild path. The open tail's innerHTML must match at
  // every step (the morph is equivalent to a rebuild).
  const a = makeClient();
  const b = makeClient();
  a.client.append("");
  b.client.append("");
  const cA = document.createElement("div");
  const cB = document.createElement("div");
  const hA = mountFluxMarkdown(a.client, cA, { batch: false, morphOpenBlocks: true });
  const hB = mountFluxMarkdown(b.client, cB, { batch: false });
  const rootA = cA.querySelector(".flux-md")!;
  const rootB = cB.querySelector(".flux-md")!;

  const seq = [
    "<p>The </p>",
    "<p>The quick </p>",
    "<p>The quick <em>brown</em> </p>",
    "<p>The quick <em>brown</em> fox <strong>jumps</strong></p>",
  ];
  for (const html of seq) {
    drive(a.client, a.worker, patch([], [para(1, html, true)]));
    drive(b.client, b.worker, patch([], [para(1, html, true)]));
    const openA = rootA.children[0] as HTMLElement;
    const openB = rootB.children[0] as HTMLElement;
    expect(openA.innerHTML).toBe(openB.innerHTML); // morph == rebuild
    expect(openA.className).toBe(openB.className);
  }

  hA.destroy();
  hB.destroy();
});

test("morphOpenBlocks: the open block node keeps identity across growth (not rebuilt), and a committed prefix node is untouched", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false, morphOpenBlocks: true });
  const root = container.querySelector(".flux-md")!;

  // Commit a prefix block; open the tail.
  drive(client, worker, patch([para(1, "<p>committed</p>")], [para(2, "<p>The </p>", true)]));
  const committedNode = root.children[0];
  const openNode = root.children[1] as HTMLElement;
  const openTextNode = openNode.querySelector("p")!.firstChild!;

  // Grow the open tail several times. With morphing ON the open block node is
  // NOT replaced (default path WOULD replace it). The committed prefix node is
  // never re-sent so it must stay identical either way.
  for (const html of ["<p>The quick </p>", "<p>The quick brown </p>", "<p>The quick brown fox</p>"]) {
    drive(client, worker, patch([], [para(2, html, true)]));
    expect(root.children[0]).toBe(committedNode); // committed prefix: same node
    expect(root.children[1]).toBe(openNode); // open block: SAME node (morphed in place)
  }
  // The streaming text node identity is preserved (pure append growth).
  expect(openNode.querySelector("p")!.firstChild).toBe(openTextNode);
  expect(openNode.textContent).toBe("The quick brown fox");

  // On COMMIT (open→closed) the morph branch is skipped: a normal rebuild swaps
  // the node, while the committed prefix stays put.
  drive(client, worker, patch([para(2, "<p>The quick brown fox.</p>")], []));
  expect(root.children[0]).toBe(committedNode);
  expect(root.children[1]).not.toBe(openNode); // commit → rebuilt once

  handle.destroy();
});

test("morphOpenBlocks default OFF: open-tail node is rebuilt on growth (no behavior change)", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false }); // default
  const root = container.querySelector(".flux-md")!;

  drive(client, worker, patch([], [para(1, "<p>The </p>", true)]));
  const openV1 = root.children[0];
  drive(client, worker, patch([], [para(1, "<p>The quick</p>", true)]));
  expect(root.children[0]).not.toBe(openV1); // default path rebuilds the node
  expect(root.children[0].textContent).toBe("The quick");

  handle.destroy();
});

test("morphOpenBlocks: sanitize still runs on the morphed open tail", () => {
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const marker = "<!--SANITIZED-->";
  const handle = mountFluxMarkdown(client, container, {
    batch: false,
    morphOpenBlocks: true,
    sanitize: (html) => marker + html,
  });
  const root = container.querySelector(".flux-md")!;

  drive(client, worker, patch([], [para(1, "<p>one</p>", true)]));
  drive(client, worker, patch([], [para(1, "<p>one two</p>", true)]));
  const node = root.children[0] as HTMLElement;
  expect(node.className).toContain("flux-open");
  // The sanitizer's comment marker is morphed in just like a rebuild would add.
  expect(node.innerHTML).toContain("SANITIZED");
  expect(node.textContent).toContain("one two");

  handle.destroy();
});

test("morphOpenBlocks does not apply to code blocks (highlighter path unaffected)", () => {
  const code = (id: number, html: string, open: boolean): Block => ({
    id, kind: { type: "CodeBlock", data: { lang: "rust" } }, start: 0, end: html.length, html, open, speculative: false,
  });
  const { client, worker } = makeClient();
  client.append("");
  const container = document.createElement("div");
  const handle = mountFluxMarkdown(client, container, { batch: false, morphOpenBlocks: true });
  const root = container.querySelector(".flux-md")!;

  // Open code block uses the dedicated renderer (not the generic innerHTML path),
  // so the morph branch must NOT engage; the streaming pill / structure stay.
  drive(client, worker, patch([], [code(1, '<pre><code class="language-rust" data-lang="rust">fn ma', true)]));
  const v1 = root.children[0] as HTMLElement;
  expect(v1.querySelector(".flux-code-streaming-pill")).not.toBeNull();
  drive(client, worker, patch([], [code(1, '<pre><code class="language-rust" data-lang="rust">fn main', true)]));
  // Dedicated renderer rebuilds the node on growth (morph branch skipped).
  expect(root.children[0]).not.toBe(v1);
  expect((root.children[0] as HTMLElement).querySelector(".flux-code-streaming-pill")).not.toBeNull();

  handle.destroy();
});

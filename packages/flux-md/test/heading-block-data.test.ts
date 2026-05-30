import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { createElement, act } from "react";
import type {
  Block,
  FromWorker,
  ToWorker,
  WorkerLike,
  HeadingData,
  BlockComponentProps,
} from "../src/types";
import { FluxClient, FluxPool } from "../src/client";
import { FluxMarkdown } from "../src/react";

// PROOF: a Heading block's structured `kind.data` is sufficient to build a TABLE
// OF CONTENTS — nested by `level`, anchored by `id` — from DATA alone, with no
// HTML re-parse. We drive synthetic patches (a FakeWorker) carrying the exact
// wire shape the Rust core emits under `setBlockData(true)`, render a real React
// `components.Heading` override, and have it assemble a nested ToC whose anchor
// hrefs come straight from `props.heading.id` and whose labels come from
// `props.heading.text` (NOT the display HTML). This is the canonical consumer use
// case, proven end-to-end through the renderer.

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

// A heading block as the core emits it under `setBlockData(true)`: kind.data is
// the `{ level, text, id }` object. `text` is the inline-STRIPPED plaintext (note
// the **bold** in the markdown is gone), `id` the github-style slug. `html` is
// the display markup (markup intact) — which the ToC must NOT need to read.
function headingBlock(
  id: number,
  level: number,
  text: string,
  slug: string,
  html: string,
): Block {
  const data: HeadingData = { level, text, id: slug };
  return {
    id,
    kind: { type: "Heading", data },
    start: 0,
    end: 0,
    html,
    open: false,
    speculative: false,
  };
}

// A minimal ToC node the override builds purely from `props.heading`.
interface TocNode {
  level: number;
  text: string;
  href: string;
  children: TocNode[];
}

test("React Heading override builds a nested, anchored ToC from kind.data alone (no HTML re-parse)", async () => {
  const { client, worker } = makeClient();
  client.append(""); // force worker creation

  // The override pushes each heading's DATA into a flat list; the test nests it.
  const flat: { level: number; text: string; href: string }[] = [];
  const seen: (HeadingData | undefined)[] = [];

  function Heading(props: BlockComponentProps) {
    seen.push(props.heading); // the typed convenience field === block.kind.data
    const h = props.heading;
    if (h) {
      // Anchor + label come ONLY from DATA — never from props.html.
      flat.push({ level: h.level, text: h.text, href: `#${h.id}` });
    }
    return createElement("div", { id: h?.id }, h?.text ?? "");
  }

  await mount(createElement(FluxMarkdown, { client, components: { Heading } }));

  await act(async () => {
    worker().fire(
      patch(
        [
          headingBlock(1, 1, "Getting Started", "getting-started", "<h1><strong>Getting</strong> Started</h1>"),
          headingBlock(2, 2, "Install", "install", "<h2>Install</h2>"),
          headingBlock(3, 3, "From npm", "from-npm", "<h3>From npm</h3>"),
          headingBlock(4, 2, "Usage", "usage", "<h2>Usage</h2>"),
          headingBlock(5, 1, "API", "api", "<h1>API</h1>"),
        ],
        [],
      ),
    );
  });

  // Every heading override received the structured object (not the bare level).
  expect(seen.every((h) => h && typeof h === "object")).toBe(true);

  // The flat list is exactly the DATA — labels are PLAINTEXT (the H1's "Getting"
  // is NOT bold here, proving we used `text`, not the `<strong>` display HTML),
  // hrefs are the slugs.
  expect(flat).toEqual([
    { level: 1, text: "Getting Started", href: "#getting-started" },
    { level: 2, text: "Install", href: "#install" },
    { level: 3, text: "From npm", href: "#from-npm" },
    { level: 2, text: "Usage", href: "#usage" },
    { level: 1, text: "API", href: "#api" },
  ]);

  // Nest the flat list by `level` — a real ToC tree, built from DATA alone.
  const roots: TocNode[] = [];
  const stack: TocNode[] = [];
  for (const e of flat) {
    const node: TocNode = { level: e.level, text: e.text, href: e.href, children: [] };
    while (stack.length && stack[stack.length - 1].level >= e.level) stack.pop();
    if (stack.length) stack[stack.length - 1].children.push(node);
    else roots.push(node);
    stack.push(node);
  }

  // Two H1 roots; the first nests Install (→ From npm) and Usage.
  expect(roots.map((r) => r.text)).toEqual(["Getting Started", "API"]);
  expect(roots[0].children.map((c) => c.text)).toEqual(["Install", "Usage"]);
  expect(roots[0].children[0].children.map((c) => c.text)).toEqual(["From npm"]);
  expect(roots[0].children[0].href).toBe("#install");
  expect(roots[1].children).toEqual([]); // API has no sub-headings
});

test("outline() returns the right level + stable numeric id for both off (number) and on (object) kind.data", async () => {
  // outline() must accept `kind.data` as either the bare level number (blockData
  // off) or the { level, text, id } object (on), yielding the same numeric level.
  // OutlineEntry.id stays the stable numeric block id (non-breaking); the anchor
  // slug is reachable additively via kind.data.id, not through outline().
  const { client, worker } = makeClient();
  client.append("");
  await mount(createElement(FluxMarkdown, { client, components: {} }));

  // A mix: an OFF heading (data = bare number) and ON headings (data = object).
  const offHeading: Block = {
    id: 10,
    kind: { type: "Heading", data: 2 }, // blockData off ⇒ naked level int
    start: 0,
    end: 0,
    html: "<h2>Legacy</h2>",
    open: false,
    speculative: false,
  };
  await act(async () => {
    worker().fire(
      patch(
        [
          offHeading,
          headingBlock(11, 1, "Title", "title", "<h1>Title</h1>"),
          headingBlock(12, 3, "Deep", "deep", "<h3>Deep</h3>"),
        ],
        [],
      ),
    );
  });

  const ol = client.outline();
  expect(ol).toEqual([
    { level: 2, text: "Legacy", id: 10 },
    { level: 1, text: "Title", id: 11 },
    { level: 3, text: "Deep", id: 12 },
  ]);
  // Every level is a real number (the off-path object-as-number bug would make
  // these NaN/undefined).
  expect(ol.every((e) => typeof e.level === "number" && Number.isFinite(e.level))).toBe(true);
});

test("props.heading is undefined for a Heading parsed WITHOUT blockData (byte-identical-off)", async () => {
  const { client, worker } = makeClient();
  client.append("");

  let seen: { hadField: boolean; heading: HeadingData | undefined } | null = null;
  function Heading(props: BlockComponentProps) {
    seen = { hadField: "heading" in props, heading: props.heading };
    return createElement("div", null, "x");
  }

  // A Heading block as the core emits it with blockData OFF: data = bare level.
  const offBlock: Block = {
    id: 1,
    kind: { type: "Heading", data: 2 },
    start: 0,
    end: 0,
    html: "<h2>x</h2>",
    open: false,
    speculative: false,
  };

  await mount(createElement(FluxMarkdown, { client, components: { Heading } }));
  await act(async () => {
    worker().fire(patch([offBlock], []));
  });

  expect(seen).not.toBeNull();
  // The naked-int level must NOT be surfaced as `heading` (the typeof-object
  // guard): a consumer reads `props.heading`, finds it absent, and falls back.
  expect(seen!.heading).toBeUndefined();
});

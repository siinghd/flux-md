import { test, expect, beforeAll } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { createElement, act } from "react";
import type {
  Block,
  FromWorker,
  ToWorker,
  WorkerLike,
  BlockComponentProps,
} from "../src/types";
import { FluxClient, FluxPool } from "../src/client";
import { FluxMarkdown } from "../src/react";

// PROOF: the opt-in structured `kind.data` of CodeBlock / MathBlock / List is
// sufficient for a consumer to (a) build a copy-to-clipboard string from a code
// block's DECODED source, (b) re-render a math block's LaTeX, and (c) renumber a
// split ordered list — all from DATA, WITHOUT re-parsing (or entity-decoding) the
// rendered HTML. We drive synthetic patches (a FakeWorker) carrying the exact wire
// shape the Rust core emits under `setBlockData(true)`, render real React
// block-kind overrides, and assert each used `props.code` / `props.math` /
// `props.list` — never `props.html`.

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

// A CodeBlock as the core emits it under setBlockData(true): kind.data carries
// `lang` + the DECODED `code`. The HTML body is ENTITY-ESCAPED (`&lt;`, `&amp;`) —
// which the override must NOT need to decode itself.
function codeBlock(id: number, lang: string | null, code: string, html: string): Block {
  return {
    id,
    kind: { type: "CodeBlock", data: { lang, code } },
    start: 0,
    end: 0,
    html,
    open: false,
    speculative: false,
  };
}

function mathBlock(id: number, latex: string, html: string): Block {
  return {
    id,
    kind: { type: "MathBlock", data: { latex } },
    start: 0,
    end: 0,
    html,
    open: false,
    speculative: false,
  };
}

function listBlock(id: number, ordered: boolean, start: number, html: string): Block {
  return {
    id,
    kind: { type: "List", data: { ordered, start } },
    start: 0,
    end: 0,
    html,
    open: false,
    speculative: false,
  };
}

test("CodeBlock override builds a copy-to-clipboard string from props.code (no HTML decode)", async () => {
  const { client, worker } = makeClient();
  client.append("");

  let copied: { lang: string | null; code: string } | null = null;
  function CodeBlock(props: BlockComponentProps) {
    // The copy button's payload comes ONLY from props.code — never props.html
    // (which is entity-escaped and would need decoding).
    if (props.code) copied = { lang: props.code.lang, code: props.code.code ?? "" };
    // props.text is the same decoded source (drop-in compat).
    return createElement("pre", null, props.text ?? "");
  }

  await mount(createElement(FluxMarkdown, { client, components: { CodeBlock } }));

  // Source had `<` and `&` — the HTML escaped them; the structured `code` is the
  // raw source.
  const html = '<pre><code class="language-js" data-lang="js">const x = a &lt; b &amp;&amp; c;\n</code></pre>';
  await act(async () => {
    worker().fire(patch([codeBlock(1, "js", "const x = a < b && c;\n", html)], []));
  });

  expect(copied).not.toBeNull();
  expect(copied!.lang).toBe("js");
  // The clipboard string is the LOSSLESS decoded source, with literal `<`/`&` —
  // proving it came from DATA, not the escaped `&lt;`/`&amp;` in props.html.
  expect(copied!.code).toBe("const x = a < b && c;\n");
  expect(copied!.code).not.toContain("&lt;");
});

test("MathBlock override re-renders LaTeX from props.math (no HTML decode)", async () => {
  const { client, worker } = makeClient();
  client.append("");

  let rendered = "";
  function MathBlock(props: BlockComponentProps) {
    // A real override hands this to KaTeX; here we just capture it. It comes
    // ONLY from props.math.latex — never the escaped props.html body.
    if (props.math) rendered = props.math.latex;
    return createElement("div", null, "katex-output");
  }

  await mount(createElement(FluxMarkdown, { client, components: { MathBlock } }));

  const html = '<div class="math math-display">a &lt; b \\&amp; c</div>';
  await act(async () => {
    worker().fire(patch([mathBlock(1, "a < b \\& c", html)], []));
  });

  expect(rendered).toBe("a < b \\& c");
  expect(rendered).not.toContain("&lt;");
});

test("List override renumbers a split ordered list from props.list.start (no HTML attr parse)", async () => {
  const { client, worker } = makeClient();
  client.append("");

  const starts: number[] = [];
  function List(props: BlockComponentProps) {
    // The continued numbering comes from props.list.start — not by re-parsing the
    // `<ol start="N">` attribute out of props.html.
    if (props.list) starts.push(props.list.start ?? 0);
    return createElement("ol", null);
  }

  await mount(createElement(FluxMarkdown, { client, components: { List } }));

  await act(async () => {
    worker().fire(
      patch(
        [
          listBlock(1, true, 1, '<ol>\n<li>a</li>\n<li>b</li>\n</ol>'),
          listBlock(2, true, 7, '<ol start="7">\n<li>g</li>\n<li>h</li>\n</ol>'),
        ],
        [],
      ),
    );
  });

  // Two ordered lists; their starts come straight from DATA.
  expect(starts).toEqual([1, 7]);
});

test("props.code/math/list are undefined for blocks parsed WITHOUT blockData (byte-identical-off)", async () => {
  const { client, worker } = makeClient();
  client.append("");

  let seen: { code: unknown; math: unknown; list: unknown; text: unknown } | null = null;
  function CodeBlock(props: BlockComponentProps) {
    seen = { code: props.code, math: props.math, list: props.list, text: props.text };
    return createElement("pre", null, "x");
  }

  // A CodeBlock as the core emits it with blockData OFF: data = { lang } only (no
  // `code` key) — byte-identical to before the carrier.
  const offBlock: Block = {
    id: 1,
    kind: { type: "CodeBlock", data: { lang: "rust" } },
    start: 0,
    end: 0,
    html: '<pre><code class="language-rust" data-lang="rust">fn main() {}\n</code></pre>',
    open: false,
    speculative: false,
  };

  await mount(createElement(FluxMarkdown, { client, components: { CodeBlock } }));
  await act(async () => {
    worker().fire(patch([offBlock], []));
  });

  expect(seen).not.toBeNull();
  // The opt-in convenience fields are absent off-path...
  expect(seen!.code).toBeUndefined();
  expect(seen!.math).toBeUndefined();
  expect(seen!.list).toBeUndefined();
  // ...but props.text still works via the HTML regex fallback (drop-in compat).
  expect(seen!.text).toBe("fn main() {}\n");
});

import { test, expect, beforeAll, spyOn } from "bun:test";
import { GlobalWindow } from "happy-dom";
import { createElement, act, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { Block, BlockComponentProps, Decorator, FromWorker, ToWorker, UrlTransform, WorkerLike } from "../src/types";
import { FluxClient, FluxPool } from "../src/client";
import { FluxMarkdown, blocksEqual, __resetUnstableWarnings } from "../src/react";
import { wrapLink, htmlToReact } from "../src/html-to-react";

// ---------------------------------------------------------------------------
// Harness: a synchronous fake worker + a happy-dom mount (mirrors
// rerender-react.test.tsx) plus a no-worker renderToStaticMarkup helper.
// ---------------------------------------------------------------------------
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
  g.Worker = class extends FakeWorker {} as unknown;
  (g as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});

async function mount(node: ReturnType<typeof createElement>) {
  const { createRoot } = await import("react-dom/client");
  const host = win.document.createElement("div");
  const root = createRoot(host as unknown as Element);
  await act(async () => {
    root.render(node);
  });
  return { host, root };
}

const PATCH_META = { appendedBytes: 0, parseMicros: 0, retainedBytes: 0, wasmMemoryBytes: 0 } as const;
function para(id: number, html: string, open: boolean): Block {
  return { id, kind: { type: "Paragraph" }, start: 0, end: 0, html, open, speculative: false };
}
function fakeClient(blocks: Block[]) {
  return { subscribe: () => () => {}, getSnapshot: () => blocks } as unknown as FluxClient;
}
const renderStatic = (html: string, props: Record<string, unknown>) =>
  renderToStaticMarkup(
    createElement(FluxMarkdown, { client: fakeClient([para(1, html, false)]), ...props }),
  );

// HOISTED, stable decorator (the documented contract): wrap percentages in <mark>.
const pctDecorators: Decorator[] = [
  { match: /\d+%/g, replace: (t) => createElement("mark", null, t) },
];

// ---------------------------------------------------------------------------
// (b) decorators wrap matched text in a paragraph
// ---------------------------------------------------------------------------
test("decorators wrap matched inline text in a paragraph", () => {
  const out = renderStatic("<p>Revenue 50% YoY</p>", { decorators: pctDecorators });
  expect(out).toContain("<p>Revenue <mark>50%</mark> YoY</p>");
});

// ---------------------------------------------------------------------------
// (c) decorator does NOT fire inside <a> / <code> / <pre> (default skipInside)
// ---------------------------------------------------------------------------
test("decorator skips text inside a / code / pre by default", () => {
  const out = renderStatic(
    '<p>10% <a href="/y">20%</a> <code>30%</code></p>',
    { decorators: pctDecorators },
  );
  // The bare 10% is wrapped; the values inside <a> and <code> are NOT.
  expect(out).toContain("<mark>10%</mark>");
  expect(out).toContain('<a href="/y">20%</a>');
  expect(out).toContain("<code>30%</code>");
  // No <mark> ended up inside the link or the code span.
  expect(out).not.toContain("<a href=\"/y\"><mark>");
  expect(out).not.toContain("<code><mark>");
});

test("custom skipInside is honored (skip inside <strong>)", () => {
  const decs: Decorator[] = [
    { match: /\d+%/g, replace: (t) => createElement("mark", null, t), skipInside: ["strong"] },
  ];
  const out = renderStatic("<p>10% <strong>20%</strong></p>", { decorators: decs });
  expect(out).toContain("<mark>10%</mark>");
  expect(out).toContain("<strong>20%</strong>");
  expect(out).not.toContain("<strong><mark>");
});

// ---------------------------------------------------------------------------
// (d) wrapLink with a javascript: href is neutralized by safeUrl
// ---------------------------------------------------------------------------
test("wrapLink neutralizes a javascript: href via safeUrl (trusted-surface escape hatch)", () => {
  const decs: Decorator[] = [
    { match: /LINK/g, replace: (t) => wrapLink(t, { href: "javascript:alert(1)" }) },
  ];
  const out = renderStatic("<p>see LINK now</p>", { decorators: decs });
  expect(out).not.toContain("javascript:");
  expect(out).toContain('href="#"');
  // And a legitimate href survives.
  const decs2: Decorator[] = [
    { match: /LINK/g, replace: (t) => wrapLink(t, { href: "https://example.com/x" }) },
  ];
  expect(renderStatic("<p>LINK</p>", { decorators: decs2 })).toContain('href="https://example.com/x"');
});

// ---------------------------------------------------------------------------
// (e) urlTransform output that returns a javascript: URL is re-sanitized away
// ---------------------------------------------------------------------------
test("urlTransform output is re-sanitized: a javascript: result cannot reach the DOM", () => {
  const evil: UrlTransform = () => "javascript:alert(1)";
  const out = renderStatic('<p><a href="/safe">x</a></p>', { urlTransform: evil });
  expect(out).not.toContain("javascript:");
  expect(out).toContain('href="#"');
});

test("urlTransform rewrites href/src/poster with the correct ctx, then re-sanitizes", () => {
  const seen: Array<{ url: string; tag: string; attr: string }> = [];
  const tx: UrlTransform = (url, ctx) => {
    seen.push({ url, tag: ctx.tag, attr: ctx.attr });
    return "https://cdn.test/" + ctx.attr;
  };
  const out = renderStatic('<p><a href="/a">x</a><img src="/b" alt="i"></p>', { urlTransform: tx });
  expect(out).toContain('href="https://cdn.test/href"');
  expect(out).toContain('src="https://cdn.test/src"');
  expect(seen.find((s) => s.attr === "href")?.tag).toBe("a");
  expect(seen.find((s) => s.attr === "src")?.tag).toBe("img");
});

// ---------------------------------------------------------------------------
// (f) a /g matcher with multiple matches in one node wraps them all (lastIndex
//     reset per node)
// ---------------------------------------------------------------------------
test("a /g matcher wraps every match in one text node", () => {
  const out = renderStatic("<p>10% and 20% and 30%</p>", { decorators: pctDecorators });
  expect(out).toContain("<mark>10%</mark> and <mark>20%</mark> and <mark>30%</mark>");
  expect(out.match(/<mark>/g)?.length).toBe(3);
});

test("a string matcher (auto-global) wraps every literal occurrence", () => {
  const decs: Decorator[] = [{ match: "FY2024", replace: (t) => createElement("b", null, t) }];
  const out = renderStatic("<p>FY2024 vs FY2024</p>", { decorators: decs });
  expect(out.match(/<b>FY2024<\/b>/g)?.length).toBe(2);
});

test("two decorators compose without double-wrapping each other's output", () => {
  const decs: Decorator[] = [
    { match: /\d+%/g, replace: (t) => createElement("mark", null, t) },
    // The second matcher would match the digits inside the first match's text,
    // but it must only see the still-unmatched text, never <mark>'s output.
    { match: /\d+/g, replace: (t) => createElement("b", null, t) },
  ];
  const out = renderStatic("<p>up 10% by 5</p>", { decorators: decs });
  expect(out).toContain("<mark>10%</mark>"); // not <mark><b>10</b>%</mark>
  expect(out).toContain("<b>5</b>");
  expect(out).not.toContain("<mark><b>");
});

// ---------------------------------------------------------------------------
// (a) O(n): a COMMITTED block is decorated EXACTLY ONCE across many tail patches
// ---------------------------------------------------------------------------
test("a committed block is decorated exactly once across many streamed tail patches", async () => {
  const w = new FakeWorker();
  const pool = new FluxPool(() => w, 1);
  const client = new FluxClient({ pool });
  client.append("");
  const sid = (w.sent[0] as { streamId: number }).streamId;

  // Counting decorator (HOISTED here so its identity is stable across renders —
  // the prop object captured by the mounted element never changes). Only the
  // committed block (id=1) contains a match; the tail (id=2) never does, so every
  // replace call is attributable to the committed block.
  let replaceCalls = 0;
  const decorators: Decorator[] = [
    { match: /\d+%/g, replace: (t) => ((replaceCalls += 1), createElement("mark", null, t)) },
  ];

  await mount(createElement(FluxMarkdown, { client, decorators }));

  // Patch 1: COMMIT id=1 (one match: "10%"), open tail id=2 (no match).
  await act(async () => {
    w.fire({
      type: "patch",
      streamId: sid,
      patch: JSON.stringify({ newly_committed: [para(1, "<p>up 10%</p>", false)], active: [para(2, "<p>tw</p>", true)] }),
      ...PATCH_META,
    });
  });
  expect(replaceCalls).toBe(1);

  // Patches 2..5: id=1 stays committed (untouched ref); only the OPEN tail grows.
  for (const html of ["<p>two</p>", "<p>two t</p>", "<p>two thr</p>", "<p>two three</p>"]) {
    await act(async () => {
      w.fire({
        type: "patch",
        streamId: sid,
        patch: JSON.stringify({ newly_committed: [], active: [para(2, html, true)] }),
        ...PATCH_META,
      });
    });
  }

  // The committed block decorated EXACTLY ONCE — the block memo (blocksEqual)
  // short-circuits its re-render because (block ref, decorators identity) held.
  expect(replaceCalls).toBe(1);
});

// blocksEqual gate: a fresh decorators identity busts the memo (the footgun); a
// stable identity holds it.
test("blocksEqual is identity-sensitive to decorators and urlTransform", () => {
  const a = para(1, "<p>x 10%</p>", false);
  const decs: Decorator[] = [{ match: /\d+%/g, replace: (t) => createElement("mark", null, t) }];
  const tx: UrlTransform = (u) => u;
  // Same identities → equal (memo holds, no re-decorate).
  expect(blocksEqual({ block: a, decorators: decs, urlTransform: tx }, { block: a, decorators: decs, urlTransform: tx })).toBe(true);
  // Fresh decorators array → not equal (would re-decorate every committed block).
  expect(blocksEqual({ block: a, decorators: decs }, { block: a, decorators: [...decs] })).toBe(false);
  // Fresh urlTransform closure → not equal.
  expect(blocksEqual({ block: a, urlTransform: tx }, { block: a, urlTransform: (u) => u })).toBe(false);
});

// ---------------------------------------------------------------------------
// (a, cont.) an UNSTABLE decorators prop triggers the one-time dev warning
// ---------------------------------------------------------------------------
test("an unstable decorators prop identity fires a one-time dev warning", async () => {
  __resetUnstableWarnings();
  const warn = spyOn(console, "warn").mockImplementation(() => {});
  try {
    const client = fakeClient([para(1, "<p>10%</p>", false)]);
    const first: Decorator[] = [{ match: /\d+%/g, replace: (t) => createElement("mark", null, t) }];
    const { root } = await mount(createElement(FluxMarkdown, { client, decorators: first }));
    expect(warn.mock.calls.length).toBe(0); // stable on mount

    // Re-render with a DIFFERENT decorators identity → one warning.
    const second: Decorator[] = [{ match: /\d+%/g, replace: (t) => createElement("mark", null, t) }];
    await act(async () => {
      root.render(createElement(FluxMarkdown, { client, decorators: second }));
    });
    const decoratorWarnings = warn.mock.calls.filter((c) => String(c[0]).includes("`decorators`"));
    expect(decoratorWarnings.length).toBe(1);

    // A THIRD distinct identity must NOT warn again (one-time latch).
    const third: Decorator[] = [{ match: /\d+%/g, replace: (t) => createElement("mark", null, t) }];
    await act(async () => {
      root.render(createElement(FluxMarkdown, { client, decorators: third }));
    });
    const after = warn.mock.calls.filter((c) => String(c[0]).includes("`decorators`"));
    expect(after.length).toBe(1);
  } finally {
    warn.mockRestore();
    __resetUnstableWarnings();
  }
});

// ---------------------------------------------------------------------------
// (g) STREAMED (char-by-char) vs ONE-SHOT parity with decorators on
// ---------------------------------------------------------------------------
test("streamed char-by-char output equals one-shot output with decorators on", async () => {
  const decorators = pctDecorators;
  const full = "Up 10% and 20% done";

  // One-shot: a single committed patch with the whole paragraph.
  const oneClient = (() => {
    const w = new FakeWorker();
    const c = new FluxClient({ pool: new FluxPool(() => w, 1) });
    c.append("");
    return { c, w, sid: (w.sent[0] as { streamId: number }).streamId };
  })();
  const oneMount = await mount(createElement(FluxMarkdown, { client: oneClient.c, decorators }));
  await act(async () => {
    oneClient.w.fire({
      type: "patch",
      streamId: oneClient.sid,
      patch: JSON.stringify({ newly_committed: [para(1, `<p>${full}</p>`, false)], active: [] }),
      ...PATCH_META,
    });
  });

  // Streamed: grow the OPEN tail one char at a time, then commit the final block.
  const strClient = (() => {
    const w = new FakeWorker();
    const c = new FluxClient({ pool: new FluxPool(() => w, 1) });
    c.append("");
    return { c, w, sid: (w.sent[0] as { streamId: number }).streamId };
  })();
  const strMount = await mount(createElement(FluxMarkdown, { client: strClient.c, decorators }));
  for (let i = 1; i <= full.length; i++) {
    await act(async () => {
      strClient.w.fire({
        type: "patch",
        streamId: strClient.sid,
        patch: JSON.stringify({ newly_committed: [], active: [para(1, `<p>${full.slice(0, i)}</p>`, true)] }),
        ...PATCH_META,
      });
    });
  }
  await act(async () => {
    strClient.w.fire({
      type: "patch",
      streamId: strClient.sid,
      patch: JSON.stringify({ newly_committed: [para(1, `<p>${full}</p>`, false)], active: [] }),
      ...PATCH_META,
    });
  });

  expect(strMount.host.innerHTML).toBe(oneMount.host.innerHTML);
  expect(strMount.host.innerHTML).toContain("<mark>10%</mark>");
  expect(strMount.host.innerHTML).toContain("<mark>20%</mark>");
});

// ---------------------------------------------------------------------------
// Direct walker units (htmlToReact) — the byte-faithful no-op guarantees.
// ---------------------------------------------------------------------------
test("htmlToReact is byte-identical when no decorators/urlTransform are supplied", () => {
  const html = "<p>x <em>y</em> 10%</p>";
  const plain = renderToStaticMarkup(htmlToReact(html, {}) as ReactNode);
  const withEmptyOpts = renderToStaticMarkup(htmlToReact(html, {}, undefined, {}) as ReactNode);
  expect(withEmptyOpts).toBe(plain);
});

test("block-kind override still works alongside decorators (override wins, no crash)", () => {
  const out = renderToStaticMarkup(
    createElement(FluxMarkdown, {
      client: fakeClient([para(1, "<p>10%</p>", false)]),
      decorators: pctDecorators,
      components: { Paragraph: (p: BlockComponentProps) => createElement("section", null, p.block.id) },
    }),
  );
  expect(out).toContain("<section>1</section>");
});

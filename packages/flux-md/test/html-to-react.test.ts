import { test, expect, beforeEach } from "bun:test";
import { createElement, isValidElement, type ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import {
  htmlToReact,
  parseTrustedHtml,
  parseStyle,
  decodeEntities,
  getParseCount,
  resetParseCount,
} from "../src/html-to-react";
import { FluxMarkdown, blocksEqual } from "../src/react";
import type { Block, Components } from "../src/types";

const render = (node: unknown) => renderToStaticMarkup(node as ReactElement);

beforeEach(() => resetParseCount());

// ---------------------------------------------------------------------------
// Pure tokenizer / converter
// ---------------------------------------------------------------------------

test("round-trips simple inline markup", () => {
  expect(render(htmlToReact("<p>Hello <em>world</em> and <strong>more</strong></p>", {})))
    .toBe("<p>Hello <em>world</em> and <strong>more</strong></p>");
});

test("decodes the entities the core emits and re-escapes safely", () => {
  expect(decodeEntities("a &amp; b &lt; c &gt; d &quot;e&quot; &#39;f&#39; &#65;")).toBe(
    "a & b < c > d \"e\" 'f' A",
  );
  // Through the full pipeline, text is decoded then React re-escapes it.
  expect(render(htmlToReact("<p>a &amp; b &lt; c</p>", {}))).toBe("<p>a &amp; b &lt; c</p>");
});

test("handles void elements without children", () => {
  expect(render(htmlToReact('<p>a<br>b</p>', {}))).toBe("<p>a<br/>b</p>");
  expect(render(htmlToReact('<img src="x.png" alt="cat">', {}))).toBe('<img src="x.png" alt="cat"/>');
  expect(render(htmlToReact("<hr>", {}))).toBe("<hr/>");
});

test("maps class to className", () => {
  expect(render(htmlToReact('<pre><code class="language-rust">fn</code></pre>', {})))
    .toBe('<pre><code class="language-rust">fn</code></pre>');
});

test("converts a style string into a React style object", () => {
  expect(parseStyle("text-align:left;color:red")).toEqual({ textAlign: "left", color: "red" });
  expect(parseStyle("--my-var: 3px")).toEqual({ "--my-var": "3px" });
  // text-align style on a table cell survives the round-trip.
  const html = '<table><thead><tr><th style="text-align:left">H</th></tr></thead></table>';
  expect(render(htmlToReact(html, {}))).toContain('style="text-align:left"');
});

test("renders a task-list checkbox uncontrolled (no onChange warning)", () => {
  // `checked` becomes `defaultChecked`; React keeps it as `checked` in static markup.
  const out = render(htmlToReact('<li><input type="checkbox" checked disabled> done</li>', {}));
  expect(out).toContain('type="checkbox"');
  expect(out).toContain("checked");
  expect(out).toContain("disabled");
});

test("parses nested lists", () => {
  const html = "<ul><li>a<ul><li>b</li></ul></li><li>c</li></ul>";
  expect(render(htmlToReact(html, {}))).toBe(html);
});

test("treats a stray '<' as text", () => {
  expect(render(htmlToReact("<p>3 < 4</p>", {}))).toContain("3 ");
});

// ---------------------------------------------------------------------------
// Tag-level overrides
// ---------------------------------------------------------------------------

test("applies a tag-level override and passes attrs + children", () => {
  const tree = htmlToReact("<table><tbody><tr><td>1</td></tr></tbody></table>", {
    table: (p: any) => createElement("table", { ...p, className: "custom" }),
  }) as ReactElement;
  expect(isValidElement(tree)).toBe(true);
  const out = render(tree);
  expect(out).toContain('<table class="custom">');
  expect(out).toContain("<td>1</td>");
});

test("override receives parsed attributes (a → href/title)", () => {
  let seen: any = null;
  const comps: Components = {
    a: (p: any) => {
      seen = p;
      return createElement("a", { href: p.href, target: "_blank", rel: "noreferrer" }, p.children);
    },
  };
  const out = render(htmlToReact('<p><a href="https://x.io" title="t">link</a></p>', comps));
  expect(seen.href).toBe("https://x.io");
  expect(seen.title).toBe("t");
  expect(out).toContain('target="_blank"');
  expect(out).toContain('rel="noreferrer"');
});

test("string-valued override swaps the tag name", () => {
  expect(render(htmlToReact("<h1>Title</h1>", { h1: "h2" }))).toBe("<h2>Title</h2>");
});

// ---------------------------------------------------------------------------
// Inline custom components (the core emits a real <tik> element; the renderer
// dispatches it via components[tag] with sanitized attrs + parsed children)
// ---------------------------------------------------------------------------

test("dispatches an inline custom-component element to components[tag] with attrs + children", () => {
  const comps: Components = {
    tik: (p: any) => createElement("span", { className: "chip", "data-sym": p.symbol }, p.children),
  };
  const out = render(htmlToReact('<p>Buy <tik symbol="AAPL"><strong>A</strong></tik> now</p>', comps));
  expect(out).toContain('<span class="chip" data-sym="AAPL"><strong>A</strong></span>');
  expect(out).toContain("Buy ");
  expect(out).toContain(" now");
});

test("preserves tag case so a capitalized inline component dispatches", () => {
  const comps: Components = { Cite: (p: any) => createElement("cite", null, p.children) };
  expect(render(htmlToReact("<p>see <Cite>R1</Cite></p>", comps))).toContain("<cite>R1</cite>");
});

test("case preservation leaves standard elements (void / input) unchanged", () => {
  expect(render(htmlToReact("<p>a<br>b</p>", {}))).toBe("<p>a<br/>b</p>");
  expect(render(htmlToReact("<hr>", {}))).toBe("<hr/>");
  expect(render(htmlToReact('<table><tr><td>1</td></tr></table>', { td: "td" }))).toContain("<td>1</td>");
});

// ---------------------------------------------------------------------------
// FluxMarkdown dispatch (fake client, no worker)
// ---------------------------------------------------------------------------

function block(partial: Partial<Block> & { html: string; kind: Block["kind"] }): Block {
  return {
    id: 1,
    start: 0,
    end: partial.html.length,
    open: false,
    speculative: false,
    ...partial,
  } as Block;
}

function fakeClient(blocks: Block[]) {
  return {
    subscribe: (_fn: () => void) => () => {},
    getSnapshot: () => blocks,
  } as any;
}

const para = (html: string, open = false) =>
  block({ kind: { type: "Paragraph" }, html, open });

test("no components prop → byte-identical innerHTML wrapper, parser untouched", () => {
  const out = render(createElement(FluxMarkdown, { client: fakeClient([para("<p>hi</p>")]) }));
  expect(out).toBe('<div class="flux-md"><div class="flux-block flux-block-paragraph"><p>hi</p></div></div>');
  expect(getParseCount()).toBe(0);
});

test("empty components object also takes the fast path", () => {
  render(createElement(FluxMarkdown, { client: fakeClient([para("<p>hi</p>")]), components: {} }));
  expect(getParseCount()).toBe(0);
});

test("closed block + components → override applied via parser (parsed once)", () => {
  const out = render(
    createElement(FluxMarkdown, {
      client: fakeClient([para("<p>hi</p>")]),
      components: { p: (p: any) => createElement("p", { ...p, className: "x" }) },
    }),
  );
  expect(out).toContain('<p class="x">hi</p>');
  expect(getParseCount()).toBe(1);
});

test("#5: open block + components → override applies to the streaming tail too (parsed once)", () => {
  // The open tail's HTML is well-formed (the parser speculatively closes it), and
  // parseTrustedHtml auto-closes anything unterminated at EOF — so a design-system
  // override now styles the streaming block instead of waiting for it to commit.
  const out = render(
    createElement(FluxMarkdown, {
      client: fakeClient([para("<p>partial", true)]),
      components: { p: (p: any) => createElement("p", { ...p, className: "x" }) },
    }),
  );
  expect(out).toContain('class="x"'); // override applies mid-stream (was deferred pre-#5)
  expect(out).toContain("flux-open"); // still flagged as the open tail
  expect(getParseCount()).toBe(1); // parsed once (was 0 — innerHTML — pre-#5)
});

test("CodeBlock with no override → dedicated highlighter renderer", () => {
  const b = block({
    kind: { type: "CodeBlock", data: { lang: "rust" } },
    html: '<pre><code class="language-rust" data-lang="rust">fn main(){}</code></pre>',
  });
  const out = render(createElement(FluxMarkdown, { client: fakeClient([b]) }));
  expect(out).toContain("flux-code-block");
  expect(out).toContain("flux-code-lang");
});

test("CodeBlock (closed) renders a copy button with an accessible label", () => {
  const b = block({
    kind: { type: "CodeBlock", data: { lang: "rust" } },
    html: '<pre><code class="language-rust" data-lang="rust">fn main(){}</code></pre>',
  });
  const out = render(createElement(FluxMarkdown, { client: fakeClient([b]) }));
  expect(out).toContain("flux-code-copy");
  expect(out).toContain('aria-label="Copy code"');
  expect(out).toContain('type="button"');
  // No streaming pill on a closed block.
  expect(out).not.toContain("flux-code-streaming-pill");
});

test("CodeBlock (open / streaming) hides the copy button until close", () => {
  const b = block({
    kind: { type: "CodeBlock", data: { lang: "rust" } },
    html: '<pre><code class="language-rust" data-lang="rust">fn ma',
    open: true,
  });
  const out = render(createElement(FluxMarkdown, { client: fakeClient([b]) }));
  expect(out).toContain("flux-code-streaming-pill");
  expect(out).not.toContain("flux-code-copy");
  expect(out).not.toContain("Copy code");
});

test("CodeBlock block-kind override wins and receives text + language", () => {
  let props: any = null;
  const b = block({
    kind: { type: "CodeBlock", data: { lang: "rust" } },
    html: '<pre><code class="language-rust" data-lang="rust">fn main(){}</code></pre>',
  });
  render(
    createElement(FluxMarkdown, {
      client: fakeClient([b]),
      components: {
        CodeBlock: (p: any) => {
          props = p;
          return createElement("div", { className: "mine" }, p.language);
        },
      },
    }),
  );
  expect(props.language).toBe("rust");
  expect(props.text).toBe("fn main(){}");
  expect(props.open).toBe(false);
});

test("CodeBlock + tag-level code override bypasses the highlighter", () => {
  const b = block({
    kind: { type: "CodeBlock", data: { lang: "rust" } },
    html: '<pre><code class="language-rust" data-lang="rust">fn</code></pre>',
  });
  const out = render(
    createElement(FluxMarkdown, {
      client: fakeClient([b]),
      components: { code: (p: any) => createElement("code", { ...p, className: "hl" }) },
    }),
  );
  expect(out).not.toContain("flux-code-block"); // highlighter bypassed
  expect(out).toContain('class="hl"');
});

test("block-kind Table override replaces the whole block", () => {
  const b = block({
    kind: { type: "Table" },
    html: "<table><tbody><tr><td>1</td></tr></tbody></table>",
  });
  const out = render(
    createElement(FluxMarkdown, {
      client: fakeClient([b]),
      components: { Table: (p: any) => createElement("div", { className: "grid" }, p.block.kind.type) },
    }),
  );
  expect(out).toContain('<div class="grid">Table</div>');
});

test("Alert block-kind override receives the alert type via kind.data", () => {
  let props: any = null;
  const b = block({
    kind: { type: "Alert", data: { kind: "warning" } },
    html: '<div class="markdown-alert markdown-alert-warning" data-alert="warning"><p class="markdown-alert-title">Warning</p><p>be careful</p></div>',
  });
  const out = render(
    createElement(FluxMarkdown, {
      client: fakeClient([b]),
      components: {
        Alert: (p: any) => createElement("aside", { className: "my-" + p.block.kind.data.kind }, p.block.kind.data.kind),
      },
    }),
  );
  expect(out).toContain('<aside class="my-warning">warning</aside>');
});

test("Alert with no override renders the GitHub-compatible HTML", () => {
  const b = block({
    kind: { type: "Alert", data: { kind: "note" } },
    html: '<div class="markdown-alert markdown-alert-note" data-alert="note"><p class="markdown-alert-title">Note</p><p>x</p></div>',
  });
  const out = render(createElement(FluxMarkdown, { client: fakeClient([b]) }));
  expect(out).toContain('class="markdown-alert markdown-alert-note"');
  expect(out).toContain("flux-block-alert");
});

test("MathBlock ($$ display) override receives decoded LaTeX as text", () => {
  let props: any = null;
  const b = block({
    kind: { type: "MathBlock" },
    html: '<div class="math math-display">E = mc^2 \\text{ where } a &lt; b</div>',
  });
  render(
    createElement(FluxMarkdown, {
      client: fakeClient([b]),
      components: { MathBlock: (p: any) => ((props = p), createElement("div", null, p.text)) },
    }),
  );
  // Display-math HTML is `<div class="math math-display">`; the override must
  // still get the raw LaTeX (entities decoded back).
  expect(props.text).toBe("E = mc^2 \\text{ where } a < b");
});

test("MathBlock (```math fence) override still receives decoded code text", () => {
  let props: any = null;
  const b = block({
    kind: { type: "MathBlock" },
    html: '<pre><code class="language-math" data-lang="math">\\frac{1}{2}</code></pre>',
  });
  render(
    createElement(FluxMarkdown, {
      client: fakeClient([b]),
      components: { MathBlock: (p: any) => ((props = p), createElement("div", null, p.text)) },
    }),
  );
  expect(props.text).toBe("\\frac{1}{2}");
});

test("MathBlock with no override renders the math-display markup", () => {
  const b = block({
    kind: { type: "MathBlock" },
    html: '<div class="math math-display">x^2</div>',
  });
  const out = render(createElement(FluxMarkdown, { client: fakeClient([b]) }));
  expect(out).toContain('class="math math-display"');
  expect(out).toContain("x^2");
});

// ---------------------------------------------------------------------------
// Virtualization (content-visibility for long docs)
// ---------------------------------------------------------------------------

test("virtualize wraps closed blocks with content-visibility, sized per kind", () => {
  const out = render(createElement(FluxMarkdown, { client: fakeClient([para("<p>hi</p>")]), virtualize: true }));
  expect(out).toContain("content-visibility:auto");
  expect(out).toContain("contain-intrinsic-size:auto 80px"); // Paragraph estimate
});

test("virtualize uses the right per-kind estimate (code block ~300px)", () => {
  const cb = block({
    kind: { type: "CodeBlock", data: { lang: "rust" } },
    html: '<pre><code class="language-rust" data-lang="rust">fn</code></pre>',
  });
  const out = render(createElement(FluxMarkdown, { client: fakeClient([cb]), virtualize: true }));
  expect(out).toContain("contain-intrinsic-size:auto 300px");
});

test("virtualize never defers the streaming tail (open or speculative)", () => {
  const open = render(createElement(FluxMarkdown, { client: fakeClient([para("<p>partial", true)]), virtualize: true }));
  expect(open).not.toContain("content-visibility");
  const spec = block({ kind: { type: "Paragraph" }, html: "<p>x</p>", open: false, speculative: true });
  const out = render(createElement(FluxMarkdown, { client: fakeClient([spec]), virtualize: true }));
  expect(out).not.toContain("content-visibility");
});

test("no virtualize prop → no wrapper (default output unchanged)", () => {
  const out = render(createElement(FluxMarkdown, { client: fakeClient([para("<p>hi</p>")]) }));
  expect(out).not.toContain("content-visibility");
  expect(out).toBe('<div class="flux-md"><div class="flux-block flux-block-paragraph"><p>hi</p></div></div>');
});

test("virtualize wraps block-kind overrides and dedicated renderers too", () => {
  const b = block({ kind: { type: "Table" }, html: "<table><tbody><tr><td>1</td></tr></tbody></table>" });
  const out = render(
    createElement(FluxMarkdown, {
      client: fakeClient([b]),
      virtualize: true,
      components: { Table: (p: any) => createElement("div", { className: "grid" }, "T" + p.block.kind.type) },
    }),
  );
  expect(out).toContain("content-visibility:auto");
  expect(out).toContain('<div class="grid">TTable</div>');
});

test("stickToBottom renders a scroll-snap sentinel as the last child", () => {
  const out = render(createElement(FluxMarkdown, { client: fakeClient([para("<p>hi</p>")]), stickToBottom: true }));
  expect(out).toContain("scroll-snap-align:end");
  // Sentinel is the final element of .flux-md.
  expect(out.endsWith('class="flux-bottom-anchor"></div></div>')).toBe(true);
});

test("no stickToBottom → no sentinel (default unchanged)", () => {
  const out = render(createElement(FluxMarkdown, { client: fakeClient([para("<p>hi</p>")]) }));
  expect(out).not.toContain("scroll-snap-align");
  expect(out).not.toContain("flux-bottom-anchor");
});

// ---------------------------------------------------------------------------
// Memoization gate
// ---------------------------------------------------------------------------

test("blocksEqual skips re-render (and thus re-parse) only when nothing changed", () => {
  const comps: Components = { p: "p" };
  const a = para("<p>x</p>");
  const same = { block: { ...a }, components: comps };
  expect(blocksEqual({ block: a, components: comps }, same)).toBe(true);
  // changed html → must re-render
  expect(blocksEqual({ block: a, components: comps }, { block: para("<p>y</p>"), components: comps }))
    .toBe(false);
  // changed components reference → must re-render
  expect(blocksEqual({ block: a, components: comps }, { block: a, components: { p: "p" } })).toBe(false);
  // open-state flip → must re-render
  expect(blocksEqual({ block: a, components: comps }, { block: para("<p>x</p>", true), components: comps }))
    .toBe(false);
});

// ---------------------------------------------------------------------------
// Security (htmlToReact is exported — must be safe even on untrusted HTML)
// ---------------------------------------------------------------------------

test("neutralizes javascript: / vbscript: / data:text/html URLs", () => {
  const cases = [
    '<a href="javascript:alert(1)">x</a>',
    '<a href="javascript&#58;alert(1)">x</a>', // entity colon (decoded by parser)
    '<a href="VBScript:msgbox(1)">x</a>',
    '<a href="data:text/html,<script>">x</a>',
    '<img src="javascript:alert(1)">',
  ];
  for (const html of cases) {
    const out = render(htmlToReact(html, {}));
    expect(out).not.toContain("javascript:");
    expect(out).not.toContain("vbscript:");
    expect(out).not.toContain("data:text/html");
    expect(out).toContain('="#"');
  }
});

test("strips C1 control chars in the scheme (parity with the Rust filter)", () => {
  // &#x85; = U+0085 (NEL), a C1 control. Rust's is_control strips it; the JS
  // probe must too so the two filters agree.
  for (const html of [
    '<a href="java&#x85;script:alert(1)">x</a>',
    '<a href="java&#133;script:alert(1)">x</a>', // decimal 133 = U+0085
  ]) {
    const out = render(htmlToReact(html, {}));
    expect(out).not.toContain("javascript:");
    expect(out).toContain('="#"');
  }
});

test("neutralizes double / triple entity-encoded dangerous schemes (decode-stable)", () => {
  for (const html of [
    '<a href="javascript&#58;alert(1)">x</a>', // single-encoded colon
    '<a href="javascript&amp;#58;alert(1)">x</a>', // double-encoded
    '<a href="javascript&amp;amp;#58;alert(1)">x</a>', // triple-encoded
  ]) {
    const out = render(htmlToReact(html, {}));
    expect(out).not.toContain("javascript:");
    expect(out).toContain('="#"');
  }
});

test("self-closing / empty inline component yields nullish children (so `children ?? x` fires)", () => {
  let received: unknown = "unset";
  const comps: Components = {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    tik: (p: any) => {
      received = p.children;
      return createElement("span", null, p.children ?? p.symbol);
    },
  };
  const out = render(htmlToReact('<p><tik symbol="AAPL"></tik></p>', comps));
  expect(received == null).toBe(true); // null, not an empty array
  expect(out).toContain("<span>AAPL</span>");
});

test("drops inline event-handler attributes", () => {
  const out = render(htmlToReact('<a href="/x" onclick="alert(1)" onmouseover="alert(2)">x</a>', {}));
  expect(out).not.toContain("onclick");
  expect(out).not.toContain("onmouseover");
  expect(out).not.toContain("alert");
  expect(out).toContain('href="/x"'); // legit attr preserved
});

test("preserves legitimate URLs", () => {
  expect(render(htmlToReact('<a href="https://example.com/a?b=1">x</a>', {})))
    .toContain('href="https://example.com/a?b=1"');
  expect(render(htmlToReact('<img src="/img/cat.png" alt="c">', {})))
    .toContain('src="/img/cat.png"');
  expect(render(htmlToReact('<a href="mailto:a@b.com">x</a>', {})))
    .toContain('href="mailto:a@b.com"');
});

// ---------------------------------------------------------------------------
// Robustness: the converter must not throw on the shapes the core emits
// ---------------------------------------------------------------------------

test("never throws on assorted real block HTML", () => {
  const samples = [
    "<h2>Heading</h2>",
    "<blockquote>\n<p>quote</p>\n</blockquote>",
    "<ol><li>one</li><li>two</li></ol>",
    "<p>text with <code>inline</code> and a <a href=\"/x\">link</a></p>",
    '<table><thead><tr><th style="text-align:right">n</th></tr></thead><tbody><tr><td style="text-align:right">1</td></tr></tbody></table>',
    "<hr>",
    "<p><del>struck</del> and <em>em</em></p>",
    "",
  ];
  for (const s of samples) {
    expect(() => render(htmlToReact(s, { table: "table", a: "a" }))).not.toThrow();
  }
  expect(parseTrustedHtml("<p>ok</p>").length).toBe(1);
});

test("terminates on malformed / adversarial markup (no hang)", () => {
  // These never come out of the core (it escapes < and balances tags), but the
  // tokenizer must still make forward progress on every one of them.
  const evil = [
    "<p>3 < 4</p>",
    "<<<<",
    "<p / / />",
    "<a href=>x</a>",
    "<div class=\"unterminated>text",
    "</p></p></p>",
    "<p><<>>",
    "<b><i>unclosed",
    "<!-- comment never closed",
    "<" .repeat(500),
    "<p>" + "x".repeat(5000) + "</p>",
  ];
  for (const s of evil) {
    expect(() => parseTrustedHtml(s)).not.toThrow();
    expect(() => render(htmlToReact(s, {}))).not.toThrow();
  }
});

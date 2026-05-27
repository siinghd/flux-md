import { useEffect, useMemo, useState } from "react";
import { FluxClient, FluxMarkdown, type BlockComponentProps, type Components } from "flux-md";

/**
 * Self-contained showcase of `<FluxMarkdown components={...} />`. It streams a
 * fixed markdown sample through a real worker-backed FluxClient and renders it
 * with both override namespaces:
 *   - tag-level  `a` / `table`  (replace an HTML element wherever it appears)
 *   - block-kind `CodeBlock`     (a copy-button code block)
 *
 * Kept out of the 5-stream perf board on purpose: the override path adds a
 * little main-thread work that would skew the throughput numbers.
 */

const SAMPLE = `## Custom components, live

Links get an override that opens in a new tab and adds a glyph:
see the [flux-md repo](https://md.hsingh.app) and the [CommonMark spec](https://spec.commonmark.org/0.31/).

> [!TIP]
> GitHub alerts (\`> [!NOTE]\`, \`[!WARNING]\`, …) render as styled callouts.

> [!WARNING]
> They stay overridable: pass \`components.Alert\` to render your own.

Streaming markdown is hard to get right[^why], and footnotes[^fn] are the latest addition — which is exactly why it's worth doing right[^why].

[^why]: Re-parsing the whole document per token melts the main thread.
[^fn]: GFM footnotes — references render inline, the section lands at finalize.

| Approach | Re-parse | Thread |
| --- | :---: | ---: |
| flux-md | tail only | worker |
| conventional | full | main |

\`\`\`ts
// The CodeBlock override adds a copy button.
export async function* stream(res: Response) {
  const reader = res.body!.getReader();
  for (;;) {
    const { value, done } = await reader.read();
    if (done) return;
    yield new TextDecoder().decode(value);
  }
}
\`\`\`
`;

function CopyCodeBlock({ text, language, open }: BlockComponentProps) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="demo-code">
      <div className="demo-code-bar">
        <span className="demo-code-lang">{language || "text"}</span>
        {open && <span className="demo-code-live">streaming…</span>}
        <button
          type="button"
          className="demo-code-copy"
          aria-label={copied ? "Code copied to clipboard" : "Copy code to clipboard"}
          onClick={() => {
            navigator.clipboard?.writeText(text ?? "");
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1200);
          }}
        >
          {copied ? "✓ copied" : "copy"}
        </button>
      </div>
      <pre className="demo-code-pre">
        <code>{text}</code>
      </pre>
    </div>
  );
}

export function ComponentsDemo() {
  // Create the client inside the effect (not useMemo) so create/destroy are
  // paired per effect-run — correct under React StrictMode's double-invoke.
  const [client, setClient] = useState<FluxClient | null>(null);

  const components: Components = useMemo(
    () => ({
      a: (p: any) => (
        <a {...p} target="_blank" rel="noreferrer">
          {p.children}
          <span aria-hidden="true"> ↗</span>
        </a>
      ),
      table: (p: any) => (
        <div className="demo-table-wrap">
          <table {...p} className="demo-table" />
        </div>
      ),
      CodeBlock: CopyCodeBlock,
    }),
    [],
  );

  useEffect(() => {
    const c = new FluxClient({ config: { gfmFootnotes: true } });
    setClient(c);
    let cancelled = false;
    (async () => {
      await c.whenReady();
      if (cancelled) return;
      // Stream it in small chunks so the "streaming…" pill is visible.
      let pos = 0;
      const chunk = 12;
      while (pos < SAMPLE.length && !cancelled) {
        c.append(SAMPLE.slice(pos, pos + chunk));
        pos += chunk;
        await new Promise((r) => setTimeout(r, 16));
      }
      if (!cancelled) c.finalize();
    })();
    return () => {
      cancelled = true;
      c.destroy();
    };
  }, []);

  return (
    <details className="demo-components">
      <summary>
        🎨 Custom components demo — <code>components={`{ a, table, CodeBlock }`}</code>
      </summary>
      <div className="demo-components-body">
        {client && <FluxMarkdown client={client} components={components} />}
      </div>
    </details>
  );
}

import { memo } from "react";

/**
 * Default mermaid block — renders the diagram source verbatim in a code-like
 * container. flux-md stays zero-dep and does not ship the Mermaid runtime:
 * override this slot via `components.Mermaid` to render to SVG with your own
 * Mermaid build (typically `mermaid.run` over the closed-block source text).
 */

interface Props {
  html: string;
  open: boolean;
}

function MermaidImpl({ html, open }: Props) {
  return (
    <div className={"flux-mermaid-block" + (open ? " flux-streaming" : "")}>
      <div className="flux-mermaid-header">
        <span className="flux-mermaid-lang">mermaid</span>
        {open && <span className="flux-code-streaming-pill">streaming</span>}
      </div>
      <div className="flux-mermaid-body" dangerouslySetInnerHTML={{ __html: html }} />
    </div>
  );
}

export const Mermaid = memo(MermaidImpl);

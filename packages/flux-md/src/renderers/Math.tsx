import { memo } from "react";

/**
 * Default math block — emits the LaTeX inside a `<div class="math
 * math-display">` (or `<span class="math math-inline">` for inline). flux-md
 * stays zero-dep, so it does not ship KaTeX/MathJax: bring your own typesetter
 * (run it over the rendered `.math` nodes once a block closes), or override
 * this slot via `components.MathBlock` to render the LaTeX yourself.
 */

interface Props {
  html: string;
  open: boolean;
}

function MathImpl({ html, open }: Props) {
  return (
    <div className={"flux-math-block" + (open ? " flux-streaming" : "")}>
      <div className="flux-math-header">
        <span className="flux-math-lang">math</span>
        {open && <span className="flux-code-streaming-pill">streaming</span>}
      </div>
      <div className="flux-math-body" dangerouslySetInnerHTML={{ __html: html }} />
    </div>
  );
}

export const MathBlock = memo(MathImpl);

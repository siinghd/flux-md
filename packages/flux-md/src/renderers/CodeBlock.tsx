import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { highlight } from "../hi";
import { extractLang } from "../block-props";

/**
 * Deferred-highlighting code block. Open (streaming) blocks render plain;
 * the moment the parser commits the block (open=false), we run our in-house
 * tokenizer on the source and swap in highlighted HTML. Highlighting is
 * memoized on html identity so closed blocks never re-tokenize.
 */

function decodeText(html: string): string {
  const m = html.match(/<pre><code[^>]*>([\s\S]*?)<\/code><\/pre>/);
  if (!m) return "";
  return m[1]
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, "&");
}


interface Props {
  html: string;
  open: boolean;
}

function CodeBlockImpl({ html, open }: Props) {
  const lang = extractLang(html) || "text";
  // Decode once: highlighter and copy handler share the same source.
  const text = useMemo(() => (open ? "" : decodeText(html)), [html, open]);
  const highlighted = useMemo(() => {
    if (!text) return null;
    return highlight(text, lang);
  }, [text, lang]);

  const [copied, setCopied] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Reset "Copied" if the block re-opens or its content changes underneath us.
  useEffect(() => {
    if (open) setCopied(false);
  }, [open, html]);

  useEffect(() => {
    return () => {
      if (timerRef.current !== null) clearTimeout(timerRef.current);
    };
  }, []);

  const onCopy = useCallback(() => {
    const write = (typeof navigator !== "undefined" && navigator.clipboard && navigator.clipboard.writeText)
      ? navigator.clipboard.writeText.bind(navigator.clipboard)
      : null;
    if (!write || !text) return;
    write(text).then(
      () => {
        setCopied(true);
        if (timerRef.current !== null) clearTimeout(timerRef.current);
        timerRef.current = setTimeout(() => setCopied(false), 1500);
      },
      // Permission denied / blocked: stay silent, leave button usable.
      () => {},
    );
  }, [text]);

  return (
    <div className={"flux-code-block" + (open ? " flux-streaming" : "")}>
      <div className="flux-code-header">
        <span className="flux-code-lang">{lang}</span>
        {open ? (
          <span className="flux-code-streaming-pill">streaming</span>
        ) : (
          <button
            type="button"
            className="flux-code-copy"
            onClick={onCopy}
            aria-label={copied ? "Copied" : "Copy code"}
            aria-live="polite"
          >
            {copied ? (
              <>
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M20 6 9 17l-5-5" />
                </svg>
                <span>Copied</span>
              </>
            ) : (
              <>
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <rect x="9" y="9" width="11" height="11" rx="2" />
                  <path d="M5 15V5a2 2 0 0 1 2-2h10" />
                </svg>
                <span>Copy</span>
              </>
            )}
          </button>
        )}
      </div>
      <div className="flux-code-body">
        {highlighted ? (
          // tabIndex=0 + role/label so keyboard users can scroll long code and
          // screen readers announce the region with its language.
          <pre tabIndex={0} role="region" aria-label={`${lang} code`}>
            <code dangerouslySetInnerHTML={{ __html: highlighted }} />
          </pre>
        ) : (
          <div tabIndex={0} role="region" aria-label={`${lang} code`} dangerouslySetInnerHTML={{ __html: html }} />
        )}
      </div>
    </div>
  );
}

export const CodeBlock = memo(CodeBlockImpl);

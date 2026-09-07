/**
 * Minimal React usage: hand `<BrookMarkdown>` a stream and it owns everything
 * (client, worker, pipe, cleanup). `stream` accepts a `Response`, a
 * `ReadableStream<Uint8Array>`, or an `AsyncIterable<string>`.
 *
 * Setup once in your app entry:
 *   import "brookmd/styles.css";
 * Vite users must also add `optimizeDeps: { exclude: ["brookmd"] }`.
 */
import { useEffect, useState } from "react";
import { BrookMarkdown } from "brookmd/react";
import { getDefaultPool } from "brookmd/client";

export function Answer({ prompt }: { prompt: string }) {
  const [stream, setStream] = useState<Response | null>(null);

  // Boot the worker + WASM before the first token so the first patch is not
  // waiting on WASM init. Cheap, idempotent, browser-only.
  useEffect(() => {
    getDefaultPool().warm();
  }, []);

  useEffect(() => {
    const ac = new AbortController();
    fetch("/api/chat", {
      method: "POST",
      body: JSON.stringify({ prompt }),
      signal: ac.signal,
    }).then(setStream, () => {});
    return () => ac.abort();
  }, [prompt]);

  if (!stream) return <div className="brook-md" />;

  // A new `stream` identity supersedes the previous one: the old pipe is
  // aborted, the parser is reset, the new stream is piped.
  return <BrookMarkdown stream={stream} onStreamError={(e) => console.error(e)} />;
}

/**
 * The same thing driven by an async generator of SSE deltas, which is the shape
 * most hand-rolled LLM clients produce.
 */
export function AnswerFromDeltas({ deltas }: { deltas: AsyncIterable<string> }) {
  return <BrookMarkdown stream={deltas} streamConfig={{ softBreaks: true, gfmMath: true }} />;
}

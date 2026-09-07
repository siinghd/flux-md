/**
 * Next.js App Router (Turbopack or webpack, verified on Next 16).
 *
 * No `transpilePackages` and no asset/loader config: brookmd ships compiled ESM
 * and references its worker + .wasm with `new URL(asset, import.meta.url)`,
 * which Next resolves itself. The Vite `optimizeDeps` workaround does NOT apply.
 *
 * Two requirements:
 *   1. `"use client"` — `<BrookMarkdown>` uses hooks and spawns a Web Worker on
 *      mount. It is still SSR-safe (it renders an empty shell on the server),
 *      but it cannot be a Server Component.
 *   2. Open the stream in CLIENT code. A Response / ReadableStream /
 *      AsyncIterable is not serializable, so a Server Component cannot pass one
 *      as a prop ("Only plain objects can be passed to Client Components").
 *      Pass a URL or the messages from the server and fetch on the client.
 *
 * `brookmd/styles.css` is global CSS — App Router can import it anywhere; the
 * Pages Router only allows it from `pages/_app`.
 */
"use client";

import { useCallback, useEffect, useState } from "react";
import { BrookMarkdown } from "brookmd/react";
import { getDefaultPool } from "brookmd/client";
import type { Components, ParserConfig } from "brookmd";

const CONFIG: ParserConfig = { softBreaks: true, dirAuto: true, a11y: true };
const COMPONENTS: Components = {};

export default function Answer({ endpoint, prompt }: { endpoint: string; prompt: string }) {
  const [stream, setStream] = useState<ReadableStream<Uint8Array> | null>(null);
  const [dead, setDead] = useState(false);

  useEffect(() => {
    // Browser-only; safe here because effects never run on the server.
    getDefaultPool().warm();
  }, []);

  const start = useCallback(async () => {
    setDead(false);
    const res = await fetch(endpoint, { method: "POST", body: JSON.stringify({ prompt }) });
    setStream(res.body);
  }, [endpoint, prompt]);

  useEffect(() => {
    void start();
  }, [start]);

  if (dead) return <p>Rendering failed. Reload the page.</p>;
  if (!stream) return <div className="brook-md" />;

  return (
    <BrookMarkdown
      stream={stream}
      streamConfig={CONFIG}
      components={COMPONENTS}
      className="brook-caret"
      stickToBottom
      onStreamError={(err) => {
        // `fatal` marks a dead worker (view frozen); a transient death heals
        // invisibly via the client's `recovery` option and never lands here.
        if ((err as Error & { fatal?: boolean }).fatal) setDead(true);
      }}
    />
  );
}

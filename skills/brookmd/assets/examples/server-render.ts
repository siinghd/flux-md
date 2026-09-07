/**
 * Worker-free, synchronous rendering of FINISHED markdown — Node SSR, a React
 * Server Component, a build step, or an edge runtime.
 *
 * `brookmd/server` is React-free: it imports no framework, so it works with no
 * `react` installed. The React server component lives in `brookmd/server/react`.
 */
import { initBrook, initBrookSync, isBrookReady, parseToBlocks, renderToString } from "brookmd/server";
import type { Block, ParserConfig } from "brookmd/types";

const CONFIG: ParserConfig = { softBreaks: true, gfmMath: true, blockData: true };

/** Node: `initBrook()` reads the package's co-located .wasm off disk. Idempotent. */
export async function renderMarkdown(md: string): Promise<string> {
  await initBrook();
  return renderToString(md, { config: CONFIG });
}

/** Edge / no filesystem: supply the bytes yourself, once at cold start. */
export function bootOnEdge(wasmBytes: BufferSource): void {
  if (!isBrookReady()) initBrookSync(wasmBytes);
}

/** Need the blocks (table of contents, per-block markup, custom renderer)? */
export async function outlineOf(md: string): Promise<{ level: number; text: string }[]> {
  await initBrook();
  const blocks: Block[] = parseToBlocks(md, { config: CONFIG });
  return blocks
    .filter((b) => b.kind.type === "Heading")
    .map((b) => {
      const data = b.kind.data as { level: number; text: string } | number | undefined;
      return typeof data === "object" && data !== null
        ? { level: data.level, text: data.text }
        : { level: typeof data === "number" ? data : 1, text: "" };
    });
}

/**
 * Assembling `parseToBlocks` output into a document string yourself? Use the
 * same join rule `renderToString` uses, or the output stops being byte-identical
 * to a one-shot reference render: a `Block.html` never ends with a newline, so
 * insert one before a block only when the output does not already end with one,
 * and terminate the document with a single newline.
 */
export function assemble(blocks: Block[]): string {
  let out = "";
  for (const b of blocks) {
    if (out.length > 0 && !out.endsWith("\n")) out += "\n";
    out += b.html;
  }
  if (out.length > 0 && !out.endsWith("\n")) out += "\n";
  return out;
}

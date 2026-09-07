/**
 * Framework-free: `BrookClient` + `mountBrookMarkdown`. No React, no Vue — the
 * `brookmd/client` and `brookmd/dom` subpaths pull no framework peer at all.
 *
 * The DOM renderer's `components` map has ONE contract: block-kind (and
 * component-tag) keys only, values `(props) => HTMLElement | string`. There is
 * no lowercase tag-level override path here (that is React-only).
 */
import "brookmd/styles.css";
import { BrookClient, getDefaultPool } from "brookmd/client";
import { mountBrookMarkdown, type MountHandle, type MountOptions } from "brookmd/dom";
import type { BlockComponentProps } from "brookmd/types";

// Hoisted: a stable identity keeps the renderer off the rebuild path.
const OPTIONS: MountOptions = {
  components: {
    // Block contract: `props.text` is the decoded source, `props.open` is true
    // while the fence is still streaming.
    Mermaid: (props: BlockComponentProps): HTMLElement => {
      const el = document.createElement("pre");
      el.className = props.open ? "mermaid-source brook-streaming" : "mermaid-source";
      el.textContent = props.text ?? "";
      return el;
    },
  },
  className: "brook-caret",
  stickToBottom: true,
  virtualize: true,
  role: "log",
  ariaLive: "polite",
  // "wavefront" (the default) colours the frozen prefix and leaves the line
  // being typed plain; "eager" recolours the tail on every patch.
  streamingHighlight: "wavefront",
  // One DOM write per animation frame; finalize() always flushes synchronously.
  batch: true,
  // Morph a growing block's subtree in place so selection/focus in the tail survives.
  morphOpenBlocks: true,
};

export async function renderChat(container: HTMLElement, url: string): Promise<MountHandle> {
  // Boot WASM before the first token.
  getDefaultPool().warm();

  const client = new BrookClient({
    config: { softBreaks: true, gfmMath: true, blockData: true },
    // Default true: one transparent re-feed if the worker dies (e.g. a stale
    // hashed worker URL after a deploy). A second death is terminal.
    recovery: true,
    onError: (err) => {
      if (err.fatal) container.classList.add("brook-dead");
      console.error("brookmd:", err.message);
    },
  });

  const handle = mountBrookMarkdown(client, container, OPTIONS);

  // pipeFrom accepts a Response, a ReadableStream<Uint8Array>, or an
  // AsyncIterable<string>; it calls finalize() at the end (an abort does not).
  const ac = new AbortController();
  await client.pipeFrom(await fetch(url, { signal: ac.signal }), { signal: ac.signal });

  return {
    destroy() {
      ac.abort();
      handle.destroy();
      client.destroy(); // caller-owned client: the mount never destroys it
    },
    refresh: handle.refresh,
    openBlockId: handle.openBlockId,
  };
}

/**
 * Already holding a growing string instead of a stream? Skip the append loop and
 * call `setContent` — the same primitive the framework helpers wrap. Pass
 * `{ done: true }` once the text is final so the last block commits.
 */
export function renderControlledString(container: HTMLElement) {
  const client = new BrookClient();
  const handle = mountBrookMarkdown(client, container, OPTIONS);
  return {
    update(fullText: string, done: boolean) {
      client.setContent(fullText, { done });
    },
    destroy() {
      handle.destroy();
      client.destroy();
    },
  };
}

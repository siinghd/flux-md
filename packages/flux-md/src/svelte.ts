import type { ActionReturn } from "svelte/action";
import type { FluxClient } from "./client";
import { mountFluxMarkdown, type DomComponents, type MountOptions } from "./dom";

/**
 * Svelte action that mounts a streaming {@link FluxClient} into the host node.
 * Plain `.ts` — no `.svelte` compile step — so `use:` works unchanged in
 * Svelte 4 and 5. The action owns only lifecycle: it mounts on creation and
 * tears the mount down on destroy. The caller keeps ownership of the client
 * (the worker/stream); the action never calls `client.destroy()`.
 *
 * ```svelte
 * <div use:fluxMarkdown={{ client, stickToBottom: true }} />
 * ```
 */
export interface FluxMarkdownParams {
  client: FluxClient;
  components?: DomComponents;
  sanitize?: (h: string) => string;
  virtualize?: boolean;
  stickToBottom?: boolean;
}

export function fluxMarkdown(
  node: HTMLElement,
  params: FluxMarkdownParams,
): ActionReturn<FluxMarkdownParams> {
  let { client, ...options } = params;
  let handle = mountFluxMarkdown(client, node, options as MountOptions);

  return {
    update(next: FluxMarkdownParams) {
      // Svelte fires update on every params change, even when nothing the mount
      // depends on moved (a fresh object literal with identical field values).
      // Remount only when an input the renderer reads actually changed identity;
      // otherwise the live mount keeps streaming untouched.
      if (
        next.client === client &&
        next.components === options.components &&
        next.sanitize === options.sanitize &&
        next.virtualize === options.virtualize &&
        next.stickToBottom === options.stickToBottom
      ) {
        return;
      }
      handle.destroy();
      ({ client, ...options } = next);
      handle = mountFluxMarkdown(client, node, options as MountOptions);
    },
    destroy() {
      // Only the mount is torn down. The caller owns the client.
      handle.destroy();
    },
  };
}

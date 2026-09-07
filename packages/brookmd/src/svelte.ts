import type { ActionReturn } from "svelte/action";
import { readable, type Readable } from "svelte/store";
import { BrookClient } from "./client";
import type { ParserConfig } from "./types-core";
import {
  mountBrookMarkdown,
  tailOpenBlockId,
  type DomComponents,
  type LinkClickInfo,
  type MountOptions,
} from "./dom";

/**
 * Svelte action that mounts a streaming {@link BrookClient} into the host node.
 * Plain `.ts` — no `.svelte` compile step — so `use:` works unchanged in
 * Svelte 4 and 5. The action owns only lifecycle: it mounts on creation and
 * tears the mount down on destroy. The caller keeps ownership of the client
 * (the worker/stream); the action never calls `client.destroy()`.
 *
 * ```svelte
 * <div use:brookMarkdown={{ client, stickToBottom: true }} />
 * ```
 */
export interface BrookMarkdownParams {
  client: BrookClient;
  components?: DomComponents;
  sanitize?: (h: string) => string;
  virtualize?: boolean;
  stickToBottom?: boolean;
  /** Delegated link-click hook — ONE listener on the renderer root, never per
   *  anchor (see `MountOptions.onLinkClick`). */
  onLinkClick?: (event: MouseEvent, link: LinkClickInfo) => void;
}

export function brookMarkdown(
  node: HTMLElement,
  params: BrookMarkdownParams,
): ActionReturn<BrookMarkdownParams> {
  let { client, ...options } = params;
  let handle = mountBrookMarkdown(client, node, options as MountOptions);

  return {
    update(next: BrookMarkdownParams) {
      // Svelte fires update on every params change, even when nothing the mount
      // depends on moved (a fresh object literal with identical field values).
      // Remount only when an input the renderer reads actually changed identity;
      // otherwise the live mount keeps streaming untouched.
      if (
        next.client === client &&
        next.components === options.components &&
        next.sanitize === options.sanitize &&
        next.virtualize === options.virtualize &&
        next.stickToBottom === options.stickToBottom &&
        next.onLinkClick === options.onLinkClick
      ) {
        return;
      }
      handle.destroy();
      ({ client, ...options } = next);
      handle = mountBrookMarkdown(client, node, options as MountOptions);
    },
    destroy() {
      // Only the mount is torn down. The caller owns the client.
      handle.destroy();
    },
  };
}

/**
 * A fine-grained Svelte `Readable` store of the streaming **tail** block id — the
 * one block that may still re-render — driven by the client's own subscribe loop.
 * The store sets a new value only when the tail id changes, so a `$tail`
 * subscription or `derived(tail, …)` re-evaluates *only* for the tail, never for
 * the committed body. Subscribing renders nothing: {@link brookMarkdown} draws the
 * document; this mirrors `MountHandle.openBlockId` through Svelte's primitive for
 * any extra tail-scoped work. The client subscription is owned by the store and
 * torn down when the last subscriber leaves (Svelte's `readable` stop fn).
 *
 * ```svelte
 * const tail = tailBlockId(client); // $tail is the open block id, or null
 * ```
 */
export function tailBlockId(client: BrookClient): Readable<number | null> {
  return readable<number | null>(tailOpenBlockId(client.getSnapshot()), (set) => {
    // `set` is identity-checked by Svelte's store, so pure tail-html growth that
    // keeps the same open id never re-fires subscribers. The returned stop fn
    // unsubscribes from the client when no one is listening.
    const unsubscribe = client.subscribe(() => set(tailOpenBlockId(client.getSnapshot())));
    return unsubscribe;
  });
}

/**
 * Controlled-string sibling of {@link brookMarkdown}: instead of taking a
 * caller-owned client, this action OWNS a single {@link BrookClient} (constructed
 * from `config`) and drives it from a CONTROLLED full string — the bridge for
 * Svelte UIs that hold a streaming message as one growing `content` prop rather
 * than feeding the client by hand. Each update passes the whole document-so-far
 * and {@link BrookClient.setContent} diffs it: a prefix-extension appends only the
 * delta; any divergence resets and reparses.
 *
 * ```svelte
 * <div use:brookMarkdownString={{ content, streaming: !done }} />
 * ```
 *
 * Pass `streaming: false` once the content is final to finalize the stream and
 * commit its last block (only then does a finished code fence highlight + show
 * its copy button). When `streaming` is omitted or `true` the stream is left
 * OPEN — right for a still-growing string, but a *complete static* string keeps
 * its last block in the streaming state until you pass `{ streaming: false }`.
 * (Inferring "done" from an absent flag is deliberately avoided — it would
 * re-finalize on every token and trip an O(n²) reparse.)
 *
 * SSR-safe by construction: a Svelte action runs ONLY in the browser, and the
 * `BrookClient` constructor is worker-free — the first worker is spawned lazily by
 * `setContent`, which only runs here (never during a server render).
 *
 * Lifecycle differs from {@link brookMarkdown}: this action constructs the client
 * once (a later `config` change is ignored, like a created-once instance) and
 * `destroy()`s it on teardown — it OWNS the client. The mount-option reconcile
 * (`components`/`sanitize`/`virtualize`/`stickToBottom`/`onLinkClick`) matches
 * `brookMarkdown`,
 * but the remount reuses the SAME client so its `setContent` diff baseline
 * survives.
 */
export interface BrookMarkdownStringParams extends Omit<BrookMarkdownParams, "client"> {
  /** The full document-so-far. Diffed against the prior value on every update. */
  content: string;
  /** Leave the stream open while true/omitted; `false` finalizes (commits the tail). */
  streaming?: boolean;
  /** Per-stream parser flags. Applied once at construction; later changes are ignored. */
  config?: ParserConfig;
}

/** Strip the action-only inputs (`content`/`streaming`/`config`), leaving the
 *  fields {@link mountBrookMarkdown} reads — so they never leak into the mount. */
function mountOptionsOf(p: BrookMarkdownStringParams): Omit<BrookMarkdownParams, "client"> {
  const { content: _c, streaming: _s, config: _cfg, ...rest } = p;
  void _c;
  void _s;
  void _cfg;
  return rest;
}

export function brookMarkdownString(
  node: HTMLElement,
  params: BrookMarkdownStringParams,
): ActionReturn<BrookMarkdownStringParams> {
  // This action OWNS the client — construct it once from `config` (a later
  // `config` change is ignored, mirroring the created-once React hook). The
  // content/streaming diff baseline lives INSIDE the client (setContent), so we
  // keep no outer copy; only the mount-option fields are tracked for the remount
  // comparison.
  let options = mountOptionsOf(params);
  const client = new BrookClient({ config: params.config });
  let handle = mountBrookMarkdown(client, node, options as MountOptions);
  // First worker-bound op: spawns the lazy Worker — browser-only, never SSR.
  client.setContent(params.content, { done: params.streaming === false });

  return {
    update(next: BrookMarkdownStringParams) {
      // Content/streaming are the primary changing inputs, so reconcile them on
      // EVERY update — setContent self-no-ops when the string is unchanged, so
      // this is cheap. (Unlike brookMarkdown, we cannot early-return: that would
      // swallow content updates.)
      client.setContent(next.content, { done: next.streaming === false });

      // Then reconcile mount options exactly like brookMarkdown: remount only when
      // a field the renderer reads actually changed identity, and reuse the SAME
      // client so its setContent diff baseline (lastContent) survives the remount.
      if (
        next.components === options.components &&
        next.sanitize === options.sanitize &&
        next.virtualize === options.virtualize &&
        next.stickToBottom === options.stickToBottom &&
        next.onLinkClick === options.onLinkClick
      ) {
        return;
      }
      handle.destroy();
      options = mountOptionsOf(next);
      handle = mountBrookMarkdown(client, node, options as MountOptions);
    },
    destroy() {
      // This action OWNS the client (unlike brookMarkdown) — tear down the mount
      // AND destroy the client so its pool slot is freed.
      handle.destroy();
      client.destroy();
    },
  };
}

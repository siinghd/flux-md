import { defineComponent, h, onMounted, onUnmounted, ref, watch } from "vue";
import type { PropType, Ref } from "vue";
import { FluxClient } from "./client";
import type { ParserConfig } from "./types-core";
import { mountFluxMarkdown, type DomComponents, type MountHandle, type MountOptions } from "./dom";

/**
 * Vue 3 bindings for {@link mountFluxMarkdown}. Thin lifecycle glue: mount the
 * framework-neutral DOM renderer on `onMounted`, tear it down on `onUnmounted`.
 *
 * The renderer owns all subscribe/diffing; this layer never re-implements it
 * and — per the renderer's contract — never calls `client.destroy()` (the
 * caller owns the worker/stream). Shipped as plain `.ts` (no SFC compiler in
 * the pipeline) via `defineComponent` + `h()`.
 */

/** Everything `mountFluxMarkdown` accepts, plus the client to subscribe to. */
export type UseFluxMarkdownOptions = { client: FluxClient } & MountOptions;

/**
 * Composable that mounts the renderer into a container ref. Returns
 * `{ container }` — bind it as the `ref` of the element you want filled.
 *
 * `getOpts` must read its fields lazily (e.g. `() => ({ client: props.client,
 * ... })`) so the watcher sees live prop identities. We watch the five
 * identities individually — `[client, components, sanitize, virtualize,
 * stickToBottom]` — rather than a freshly-composed object, which would change
 * identity every call and remount on every patch. On any of those changing we
 * destroy and remount; `batch`/`highlightCode` still flow through to the mount
 * but are intentionally not remount triggers.
 */
export function useFluxMarkdown(getOpts: () => UseFluxMarkdownOptions): {
  container: Ref<HTMLElement | null>;
} {
  const container = ref<HTMLElement | null>(null);
  let handle: MountHandle | null = null;

  function mount(): void {
    if (!container.value) return;
    const { client, ...mountOptions } = getOpts();
    handle = mountFluxMarkdown(client, container.value, mountOptions);
  }

  function teardown(): void {
    // handle.destroy() is the ONLY teardown — it unsubscribes and removes the
    // renderer root. The caller owns client.destroy(); we never call it.
    handle?.destroy();
    handle = null;
  }

  onMounted(mount);

  watch(
    [
      () => getOpts().client,
      () => getOpts().components,
      () => getOpts().sanitize,
      () => getOpts().virtualize,
      () => getOpts().stickToBottom,
    ],
    () => {
      // Only after the initial onMounted has run does `handle` exist; before
      // that the watcher firing (it won't, being lazy) would no-op anyway.
      teardown();
      mount();
    },
  );

  // Vue auto-stops this watcher when the owning component unmounts, so a manual
  // stop is unnecessary; we only need to drop the renderer.
  onUnmounted(teardown);

  return { container };
}

/**
 * Component wrapper around {@link useFluxMarkdown}. Renders a single `<div>`
 * whose ref is the mount container.
 */
export const FluxMarkdown = defineComponent({
  name: "FluxMarkdown",
  props: {
    client: { type: Object as PropType<FluxClient>, required: true },
    components: { type: Object as PropType<DomComponents>, default: undefined },
    sanitize: { type: Function as PropType<(html: string) => string>, default: undefined },
    virtualize: { type: Boolean, default: undefined },
    stickToBottom: { type: Boolean, default: undefined },
  },
  setup(props) {
    // Read props inside the getter so the watch tracks their live identities;
    // destructuring here would snapshot them and the watcher would never fire.
    const { container } = useFluxMarkdown(() => ({
      client: props.client,
      components: props.components,
      sanitize: props.sanitize,
      virtualize: props.virtualize,
      stickToBottom: props.stickToBottom,
    }));
    return () => h("div", { ref: container });
  },
});

/**
 * Own a {@link FluxClient} driven by a CONTROLLED full string — the Vue analogue
 * of React's `useFluxMarkdownString`, for UIs that hold a streaming message as a
 * single growing string (a `ref`/computed) rather than as a stream. Pass a getter
 * for the whole document-so-far; on every change {@link FluxClient.setContent}
 * diffs it and does the minimal work (prefix-extension appends only the delta;
 * any divergence resets and reparses).
 *
 * Pass `streaming: false` (via `getOptions`) once the content is final to
 * finalize the stream and commit its last block. If `streaming` is omitted or
 * `true` the stream is left OPEN — inferring "done" from an absent flag is
 * deliberately avoided (it would re-finalize on every token for callers that
 * grow the string without the flag — an O(n²) reparse trap). `config` is read
 * once at construction and is immutable thereafter, so it is not a change
 * trigger.
 *
 * **Returns the owned client** — a deliberate divergence from {@link useFluxMarkdown}
 * (which returns `{ container }`). Mirroring React's hook, this composes with the
 * component as `<FluxMarkdown :client="client" />` (and lets you read
 * `outline()` / `getMetrics()` off it). The client is created in the composable
 * body (constructor is worker-free → SSR-safe) and destroyed on unmount.
 *
 * SSR-safety: `setContent` is what spawns a Worker (via `append`), so it is
 * called ONLY in `onMounted` and a NON-immediate `watch` — never during the
 * server render path (`setup` constructs the client but neither lifecycle hook
 * nor the non-immediate watch fires on the server).
 */
export function useFluxMarkdownString(
  getContent: () => string,
  getOptions?: () => { config?: ParserConfig; streaming?: boolean },
): FluxClient {
  // One client per composable instance. Constructor is worker-free, so this is
  // safe to run in setup() during SSR; config is read once and is immutable.
  const client = new FluxClient({ config: getOptions?.()?.config });

  // Reconcile the parser to the controlled string. setContent diffs internally,
  // so this is correct whether `content` grows by a token or is swapped wholesale.
  // `streaming === false` (never `!streaming`) → only an explicit false finalizes;
  // an absent/true flag leaves the stream open.
  const apply = (): void => {
    client.setContent(getContent(), { done: getOptions?.()?.streaming === false });
  };

  // Initial feed + every change. NOT { immediate: true }: an immediate watch runs
  // in setup() — i.e. during SSR — and would spawn a Worker on the server. The
  // initial feed is onMounted (client-only); the watch covers later changes.
  onMounted(apply);
  watch([getContent, () => getOptions?.()?.streaming], apply);

  // This composable OWNS the client (unlike useFluxMarkdown, which takes one), so
  // it destroys it here. Vue auto-stops the watcher on unmount.
  onUnmounted(() => client.destroy());

  return client;
}

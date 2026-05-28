import { defineComponent, h, onMounted, onUnmounted, ref, watch } from "vue";
import type { PropType, Ref } from "vue";
import type { FluxClient } from "./client";
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

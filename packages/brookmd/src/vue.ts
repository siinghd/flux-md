import { defineComponent, h, onMounted, onUnmounted, ref, shallowRef, watch } from "vue";
import type { DefineComponent, PropType, Ref } from "vue";
import { BrookClient } from "./client";
import type { ParserConfig } from "./types-core";
import {
  mountBrookMarkdown,
  tailOpenBlockId,
  type DomComponents,
  type LinkClickInfo,
  type MountHandle,
  type MountOptions,
} from "./dom";

/**
 * Vue 3 bindings for {@link mountBrookMarkdown}. Thin lifecycle glue: mount the
 * framework-neutral DOM renderer on `onMounted`, tear it down on `onUnmounted`.
 *
 * The renderer owns all subscribe/diffing; this layer never re-implements it
 * and — per the renderer's contract — never calls `client.destroy()` (the
 * caller owns the worker/stream). Shipped as plain `.ts` (no SFC compiler in
 * the pipeline) via `defineComponent` + `h()`.
 */

/** Everything `mountBrookMarkdown` accepts, plus the client to subscribe to. */
export type UseBrookMarkdownOptions = { client: BrookClient } & MountOptions;

/**
 * Composable that mounts the renderer into a container ref. Returns
 * `{ container }` — bind it as the `ref` of the element you want filled.
 *
 * `getOpts` must read its fields lazily (e.g. `() => ({ client: props.client,
 * ... })`) so the watcher sees live prop identities. We watch the six
 * identities individually — `[client, components, sanitize, virtualize,
 * stickToBottom, onLinkClick]` — rather than a freshly-composed object, which
 * would change identity every call and remount on every patch. On any of those
 * changing we destroy and remount; `batch`/`highlightCode` still flow through to
 * the mount but are intentionally not remount triggers.
 */
export function useBrookMarkdown(getOpts: () => UseBrookMarkdownOptions): {
  container: Ref<HTMLElement | null>;
} {
  const container = ref<HTMLElement | null>(null);
  let handle: MountHandle | null = null;

  function mount(): void {
    if (!container.value) return;
    const { client, ...mountOptions } = getOpts();
    handle = mountBrookMarkdown(client, container.value, mountOptions);
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
      () => getOpts().onLinkClick,
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
 * A fine-grained `Ref` to the streaming **tail** block id — the one block that
 * may still re-render — driven by Vue's reactivity. Subscribes to the client
 * once and writes a `shallowRef` only when the tail id changes, so a `computed`
 * or `watch` keyed off it re-evaluates *only* for the tail, never for the
 * committed body. Reading it renders nothing: {@link useBrookMarkdown} draws the
 * document; this mirrors {@link MountHandle.openBlockId} through Vue's primitive
 * for any extra tail-scoped work the caller schedules. Auto-unsubscribes on the
 * owning component's unmount.
 */
export function useTailBlockId(client: BrookClient): Ref<number | null> {
  const tail = shallowRef<number | null>(tailOpenBlockId(client.getSnapshot()));
  // A shallowRef assignment only triggers when the value actually changes, so
  // pure tail-html growth that keeps the same open id never re-fires watchers.
  const unsubscribe = client.subscribe(() => {
    tail.value = tailOpenBlockId(client.getSnapshot());
  });
  onUnmounted(unsubscribe);
  return tail;
}

/** Public props of the {@link BrookMarkdown} Vue component. */
export interface BrookMarkdownVueProps {
  client: BrookClient;
  components?: DomComponents;
  sanitize?: (html: string) => string;
  virtualize?: boolean;
  stickToBottom?: boolean;
  /** Delegated link-click hook (one listener on the root; see `MountOptions`). */
  onLinkClick?: (event: MouseEvent, link: LinkClickInfo) => void;
}

/**
 * Component wrapper around {@link useBrookMarkdown}. Renders a single `<div>`
 * whose ref is the mount container.
 *
 * The return type is annotated with an explicit, single-type-argument
 * `DefineComponent<BrookMarkdownVueProps>` instead of letting `tsc` inline
 * `defineComponent`'s inferred type. The inferred form bakes the *build-time*
 * Vue version's `DefineComponent` arity into the emitted `.d.ts`, which breaks
 * consumers on an older Vue within the declared `vue >=3` peer range (TS2707).
 * A single explicit type arg is portable across all of Vue 3.x.
 */
export const BrookMarkdown: DefineComponent<BrookMarkdownVueProps> = defineComponent({
  name: "BrookMarkdown",
  props: {
    client: { type: Object as PropType<BrookClient>, required: true },
    components: { type: Object as PropType<DomComponents>, default: undefined },
    sanitize: { type: Function as PropType<(html: string) => string>, default: undefined },
    virtualize: { type: Boolean, default: undefined },
    stickToBottom: { type: Boolean, default: undefined },
    // Declared as a real prop (not an emit listener): Vue resolves a declared
    // `onX` prop from `props` before it ever reaches `attrs`.
    onLinkClick: {
      type: Function as PropType<(event: MouseEvent, link: LinkClickInfo) => void>,
      default: undefined,
    },
  },
  setup(props) {
    // Read props inside the getter so the watch tracks their live identities;
    // destructuring here would snapshot them and the watcher would never fire.
    const { container } = useBrookMarkdown(() => ({
      client: props.client,
      components: props.components,
      sanitize: props.sanitize,
      virtualize: props.virtualize,
      stickToBottom: props.stickToBottom,
      onLinkClick: props.onLinkClick,
    }));
    return () => h("div", { ref: container });
  },
});

/**
 * Own a {@link BrookClient} driven by a CONTROLLED full string — the Vue analogue
 * of React's `useBrookMarkdownString`, for UIs that hold a streaming message as a
 * single growing string (a `ref`/computed) rather than as a stream. Pass a getter
 * for the whole document-so-far; on every change {@link BrookClient.setContent}
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
 * **Returns the owned client** — a deliberate divergence from {@link useBrookMarkdown}
 * (which returns `{ container }`). Mirroring React's hook, this composes with the
 * component as `<BrookMarkdown :client="client" />` (and lets you read
 * `outline()` / `getMetrics()` off it). The client is created in the composable
 * body (constructor is worker-free → SSR-safe) and destroyed on unmount.
 *
 * SSR-safety: `setContent` is what spawns a Worker (via `append`), so it is
 * called ONLY in `onMounted` and a NON-immediate `watch` — never during the
 * server render path (`setup` constructs the client but neither lifecycle hook
 * nor the non-immediate watch fires on the server).
 */
export function useBrookMarkdownString(
  getContent: () => string,
  getOptions?: () => { config?: ParserConfig; streaming?: boolean },
): BrookClient {
  // One client per composable instance. Constructor is worker-free, so this is
  // safe to run in setup() during SSR; config is read once and is immutable.
  const client = new BrookClient({ config: getOptions?.()?.config });

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

  // This composable OWNS the client (unlike useBrookMarkdown, which takes one), so
  // it destroys it here. Vue auto-stops the watcher on unmount.
  onUnmounted(() => client.destroy());

  return client;
}

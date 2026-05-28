import { onCleanup, onMount, type JSX } from "solid-js";
import type { FluxClient } from "./client";
import { mountFluxMarkdown, type MountHandle, type MountOptions } from "./dom";

/**
 * Solid binding for the framework-neutral DOM renderer ({@link mountFluxMarkdown}).
 *
 * Deliberately thin lifecycle glue: it mounts the renderer once on `onMount` and
 * tears it down on `onCleanup`. There is **no** `createEffect` — the DOM renderer
 * owns its own `client.subscribe` loop and patches the container directly, so
 * re-running mount on signal changes would thrash (double-subscribe, rebuild the
 * tree). Props are read once as a non-reactive snapshot at mount time.
 *
 * Ownership: unmount calls `handle.destroy()` (unsubscribe + remove the renderer
 * root) and never `client.destroy()`. The caller owns the worker/stream.
 */

export interface FluxMarkdownProps extends MountOptions {
  client: FluxClient;
  class?: string;
  style?: JSX.CSSProperties | string;
}

/**
 * Mount the DOM renderer and register its teardown — the testable core, free of
 * JSX so it runs under any toolchain. `getProps` is read once (snapshot), the
 * handle is returned so callers/tests can observe `destroy`, and the teardown is
 * handed to `registerCleanup` (Solid's `onCleanup` at the call site).
 */
export function mountSolid(
  getProps: () => FluxMarkdownProps,
  container: HTMLElement,
  registerCleanup: (fn: () => void) => void,
): MountHandle {
  const p = getProps();
  // Explicit field copy (not rest-spread): keeps `client`/`class`/`style` out of
  // MountOptions and threads `batch`/`highlightCode` straight through.
  const handle = mountFluxMarkdown(p.client, container, {
    components: p.components,
    sanitize: p.sanitize,
    virtualize: p.virtualize,
    stickToBottom: p.stickToBottom,
    highlightCode: p.highlightCode,
    batch: p.batch,
  });
  registerCleanup(() => handle.destroy());
  return handle;
}

/**
 * The container `<div>` the DOM renderer mounts into. We do not set
 * `class="flux-md"`: the renderer appends its own `.flux-md` root inside it.
 *
 * Authored imperatively rather than with a JSX literal: a JSX literal makes
 * bun's transform inject an automatic-runtime import (`jsxDEV` from
 * `solid-js/jsx-dev-runtime`) that Solid does not provide (Solid compiles JSX
 * via dom-expressions, not a runtime), which breaks importing this module under
 * bun. A real DOM node is a valid Solid `JSX.Element`; under a Solid build this
 * is equivalent to `<div ref={container} class={props.class} style={props.style} />`.
 */
export function FluxMarkdown(props: FluxMarkdownProps): JSX.Element {
  const container = document.createElement("div");
  if (props.class) container.className = props.class;
  if (typeof props.style === "string") container.setAttribute("style", props.style);
  else if (props.style)
    for (const [k, v] of Object.entries(props.style)) container.style.setProperty(k, String(v));
  // Snapshot props once on mount; the renderer drives itself from here on.
  onMount(() => mountSolid(() => props, container, onCleanup));
  return container;
}

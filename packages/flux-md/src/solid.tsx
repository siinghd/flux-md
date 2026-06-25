import { createEffect, createSignal, onCleanup, onMount, type Accessor, type JSX } from "solid-js";
import { FluxClient } from "./client";
import type { ParserConfig } from "./types-core";
import { mountFluxMarkdown, tailOpenBlockId, type MountHandle, type MountOptions } from "./dom";

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
 * A fine-grained accessor for the streaming **tail** block id — the one block
 * that may still re-render — driven by Solid's own reactivity. Subscribes to the
 * client once and updates a `createSignal` only when the tail id changes, so a
 * downstream `createMemo`/effect that reads it re-evaluates *only* for the tail,
 * never for the committed body. Reading it renders nothing: the DOM is owned by
 * {@link mountFluxMarkdown}; this is a scheduling/diagnostic signal that mirrors
 * {@link MountHandle.openBlockId} through Solid's primitive.
 *
 * Registrars are injected (like {@link mountSolid}) so the testable core runs
 * under any toolchain; the public {@link createTailBlockId} wires Solid's
 * `onCleanup`.
 */
export function setupTailBlockId(
  client: FluxClient,
  registerCleanup: (fn: () => void) => void,
): Accessor<number | null> {
  const [tail, setTail] = createSignal<number | null>(tailOpenBlockId(client.getSnapshot()));
  // setTail no-ops when the value is unchanged (Solid's default equality), so
  // pure tail-html growth that keeps the same open id never re-fires downstream.
  const unsubscribe = client.subscribe(() => setTail(tailOpenBlockId(client.getSnapshot())));
  registerCleanup(unsubscribe);
  return tail;
}

/**
 * Own a fine-grained tail-block-id accessor for `client`, wired to Solid's
 * `onCleanup`. Pair it with `<FluxMarkdown client={client} />`: the component
 * draws the document, this accessor narrows any extra reactive work you key off
 * the live tail (e.g. a "streaming…" affordance) to just the open block.
 */
export function createTailBlockId(client: FluxClient): Accessor<number | null> {
  return setupTailBlockId(client, onCleanup);
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
  // SSR: this renderer is client-only and imperative (creates/owns its own DOM
  // node, no hydration expected). Solid runs component bodies on the server, so
  // guard the document access; the browser path is byte-identical (document is
  // always defined there). onMount never fires on the server anyway.
  if (typeof document === "undefined") return undefined as unknown as JSX.Element;
  const container = document.createElement("div");
  if (props.class) container.className = props.class;
  if (typeof props.style === "string") container.setAttribute("style", props.style);
  else if (props.style)
    for (const [k, v] of Object.entries(props.style)) container.style.setProperty(k, String(v));
  // Snapshot props once on mount; the renderer drives itself from here on.
  onMount(() => mountSolid(() => props, container, onCleanup));
  return container;
}

/**
 * Wire a controlled string to a freshly-constructed {@link FluxClient}, free of
 * Solid's reactive runtime so it runs (and is tested) under any toolchain. The
 * registrars are injected: the public {@link createFluxMarkdownString} passes
 * Solid's real `createEffect` / `onCleanup`; tests pass hand-rolled stand-ins
 * (mirroring how {@link mountSolid} takes `registerCleanup`).
 *
 * Ownership DIFFERS from {@link mountSolid}: this constructs the client and so
 * `registerCleanup`s `client.destroy()` — it OWNS the worker/stream. `config` is
 * read ONCE here (the constructor treats it as immutable); `getContent()` and
 * `streaming` are read INSIDE the effect so the effect tracks them reactively.
 */
export function setupFluxMarkdownString(
  getContent: () => string,
  getOptions: (() => { config?: ParserConfig; streaming?: boolean }) | undefined,
  registerEffect: (fn: () => void) => void,
  registerCleanup: (fn: () => void) => void,
): FluxClient {
  // One client per helper instance. Constructor is worker-free → SSR-safe; the
  // worker is spawned lazily by the first setContent → append, which only runs
  // inside the effect below. config is read once and is immutable thereafter.
  const client = new FluxClient({ config: getOptions?.()?.config });

  // Reconcile the parser to the controlled string. setContent diffs internally,
  // so this is correct whether `content` grows by a token or is swapped wholesale.
  // `streaming === false` (never `!streaming`) → only an explicit false finalizes;
  // an absent/true flag leaves the stream open (inferring "done" from an absent
  // flag would re-finalize on every token — an O(n²) reparse trap).
  registerEffect(() => {
    client.setContent(getContent(), { done: getOptions?.()?.streaming === false });
  });

  // This helper OWNS the client (unlike the client-based bindings above), so it
  // destroys it on cleanup — freeing its pool slot.
  registerCleanup(() => client.destroy());

  return client;
}

/**
 * Own a {@link FluxClient} driven by a CONTROLLED full string — the Solid
 * analogue of React's `useFluxMarkdownString`, for UIs that hold a streaming
 * message as a single growing string (a signal/memo) rather than as a stream.
 * Pass an accessor for the whole document-so-far; on every change
 * {@link FluxClient.setContent} diffs it and does the minimal work (a
 * prefix-extension appends only the delta; any divergence resets and reparses).
 *
 * Pass `streaming: false` (via `getOptions`) once the content is final to
 * finalize the stream and commit its last block (only then does a finished code
 * fence highlight + show its copy button). If `streaming` is omitted or `true`
 * the stream is left OPEN. `config` is read once at construction and is
 * immutable, so it is not a change trigger.
 *
 * **Returns the owned client** — pass it to `<FluxMarkdown client={client} />`
 * (and read `outline()` / `getMetrics()` off it). The client is constructed in
 * the body (constructor is worker-free → SSR-safe) and destroyed on cleanup.
 *
 * SSR-safety: `setContent` is what spawns a Worker (via `append`), so it runs
 * ONLY inside a `createEffect` — Solid does not run user effects during
 * `renderToString`, so nothing touches a Worker on the server render path (the
 * body only constructs the worker-free client).
 */
export function createFluxMarkdownString(
  getContent: () => string,
  getOptions?: () => { config?: ParserConfig; streaming?: boolean },
): FluxClient {
  return setupFluxMarkdownString(getContent, getOptions, createEffect, onCleanup);
}

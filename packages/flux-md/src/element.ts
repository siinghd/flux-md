import { FluxClient } from "./client";
import { mountFluxMarkdown, type DomComponents, type MountHandle } from "./dom";
import type { ParserConfig } from "./types-core";

/**
 * `<flux-markdown>` custom element — thin lifecycle glue over
 * {@link mountFluxMarkdown}. It owns no diffing: connect mounts the DOM
 * renderer into the element itself (LIGHT DOM, so the host app's markdown CSS
 * reaches the content), disconnect tears the mount down. It never reimplements
 * subscribe/patch.
 *
 * Two usage modes:
 *   - **Caller-owned client** (`el.client = myClient`): the element subscribes
 *     and mounts but NEVER destroys the client — the caller owns the
 *     worker/stream lifecycle.
 *   - **Self-owned client** (`markdown`/`src`/`textContent` attrs, or
 *     `el.append()`): the element lazily creates an internal client from its
 *     config attributes and destroys it on disconnect.
 *
 * Not auto-registered (SSR-unsafe): call {@link defineFluxMarkdown} from
 * browser code.
 */

// Tri-state attribute parse: absent => undefined (omit, library default);
// ""/"true"/"1" => true; "false"/"0" => false. Tri-state is the only way to
// turn OFF a flag whose library default is on (autolinks, alerts). Exported so
// it is directly unit-testable.
export function parseTriBool(value: string | null): boolean | undefined {
  if (value === null) return undefined;
  if (value === "" || value === "true" || value === "1") return true;
  if (value === "false" || value === "0") return false;
  return undefined;
}

const CONFIG_ATTRS = [
  "gfm-autolinks",
  "gfm-alerts",
  "gfm-footnotes",
  "gfm-math",
  "dir-auto",
  "a11y",
  "unsafe-html",
];

export function defineFluxMarkdown(tag = "flux-markdown"): void {
  // SSR-safe: no custom-element registry => nothing to define.
  if (typeof customElements === "undefined") return;
  // Idempotent: a tag may only be defined once.
  if (customElements.get(tag)) return;

  // The class is defined lazily INSIDE the function: at module-evaluation time
  // `HTMLElement` may not exist (SSR / pre-DOM). Referencing it only after the
  // guards above keeps the module import side-effect-free.
  class FluxMarkdownElement extends HTMLElement {
    static get observedAttributes(): string[] {
      return ["markdown", "src", "component-tags", ...CONFIG_ATTRS];
    }

    #client: FluxClient | null = null;
    #ownsClient = false;
    #components: DomComponents | undefined = undefined;
    #sanitize: ((html: string) => string) | undefined = undefined;
    #handle: MountHandle | null = null;
    #connected = false;
    // In-flight `src` fetch supersession. A self-owned client is REUSED across
    // src changes (not torn down), so two concurrent #streamFromSrc runs would
    // capture the same client and reset() even reuses the worker streamId — an
    // identity guard alone can't separate them. Each run captures the current
    // #srcSeq; a newer src (or teardown) bumps it and aborts the fetch, so a
    // stale run bails before interleaving its chunks into the parser.
    #srcSeq = 0;
    #srcAbort: AbortController | null = null;

    // --- Accessor properties (objects/functions can't be attributes) ---------

    get client(): FluxClient | null {
      return this.#client;
    }
    set client(value: FluxClient | null) {
      if (value === this.#client) return;
      // Switching to a caller-owned client: tear down any internal client we own
      // first, then adopt the new one without owning it.
      this.#teardownClient();
      this.#client = value;
      this.#ownsClient = false;
      if (this.#connected) this.#remount();
    }

    get components(): DomComponents | undefined {
      return this.#components;
    }
    set components(value: DomComponents | undefined) {
      if (value === this.#components) return; // no-op re-assign: don't remount
      this.#components = value;
      if (this.#connected) this.#remount();
    }

    get sanitize(): ((html: string) => string) | undefined {
      return this.#sanitize;
    }
    set sanitize(value: ((html: string) => string) | undefined) {
      if (value === this.#sanitize) return; // no-op re-assign: don't remount
      this.#sanitize = value;
      if (this.#connected) this.#remount();
    }

    // --- Self-owned-client methods -------------------------------------------

    append(chunk: string): void {
      // Manual drive supersedes any in-flight `src` fetch (mixing the two is out
      // of contract; this makes the manual stream win predictably instead of
      // interleaving a late fetch chunk into it).
      this.#cancelSrcStream();
      this.#ensureClient();
      this.#client!.append(chunk);
    }

    finalize(): void {
      // Only meaningful for a self-owned stream; a no-op if no client yet.
      this.#cancelSrcStream();
      this.#client?.finalize();
    }

    reset(): void {
      // Keep config; just clear the current stream's blocks. Also abandon any
      // in-flight `src` fetch so it can't append into the freshly-reset stream.
      this.#cancelSrcStream();
      this.#client?.reset();
    }

    getClient(): FluxClient | null {
      return this.#client;
    }

    // --- Lifecycle -----------------------------------------------------------

    connectedCallback(): void {
      // Guard double-connect; allow reconnect-after-move.
      if (this.#connected) return;
      this.#connected = true;

      // Property-upgrade dance: a framework may set `el.client`/`components`/
      // `sanitize` BEFORE the element is upgraded, leaving an own data property
      // that shadows the accessor. Capture, delete, re-assign through the setter.
      this.#upgradeProperty("client");
      this.#upgradeProperty("components");
      this.#upgradeProperty("sanitize");

      // Mount synchronously if we already have a client (caller-owned, or one a
      // pre-connect append() created). append/finalize are postMessage and the
      // config rides the first message FIFO, so no whenReady await is needed.
      this.#mountIfReady();

      // Resolve initial content for self-owned mode only (no caller client).
      if (!this.#client || this.#ownsClient) {
        this.#resolveInitialContent();
      }
    }

    attributeChangedCallback(name: string, oldValue: string | null, newValue: string | null): void {
      // attributeChangedCallback fires before connectedCallback for attributes
      // present at upgrade; ignore until connected so config reads happen once.
      if (!this.#connected) return;
      // setAttribute fires this on EVERY set, including setting an attribute to
      // its current value (common when a host framework re-applies the same
      // attrs on re-render). A no-op value change must not tear down the client
      // and reparse the whole document — only a genuine change proceeds.
      // (Attribute removal yields null, distinct from an empty string.)
      if (oldValue === newValue) return;

      if (name === "markdown" || name === "src") {
        // One-shot content source change — only for a self-owned client. A
        // caller-owned client is driven by its owner, not by our attributes.
        if (!this.#client || this.#ownsClient) {
          this.#resolveInitialContent();
        }
        return;
      }

      // A config / component-tags change. ParserConfig is immutable per stream.
      if (this.#client && !this.#ownsClient) {
        // eslint-disable-next-line no-console
        console.warn(
          "<flux-markdown>: config attributes are ignored while a caller-owned `client` is set (ParserConfig is immutable per stream).",
        );
        return;
      }
      // Self-owned: rebuild the client with fresh config, then re-render.
      if (this.#ownsClient) {
        this.#teardownClient();
        this.#mountIfReady();
        this.#resolveInitialContent();
      }
    }

    disconnectedCallback(): void {
      this.#connected = false;
      // Stop any in-flight `src` fetch before we (maybe) destroy its client.
      this.#cancelSrcStream();
      // ALWAYS tear down the mount (the only teardown path for the renderer).
      this.#handle?.destroy();
      this.#handle = null;
      // Destroy the client ONLY if we created it. A caller-owned client's
      // worker/stream lifecycle belongs to the caller — never destroy it here.
      if (this.#ownsClient) {
        this.#client?.destroy();
        this.#client = null;
        this.#ownsClient = false;
      }
    }

    // --- Internals -----------------------------------------------------------

    #upgradeProperty(prop: "client" | "components" | "sanitize"): void {
      if (Object.prototype.hasOwnProperty.call(this, prop)) {
        const value = (this as unknown as Record<string, unknown>)[prop];
        delete (this as unknown as Record<string, unknown>)[prop];
        (this as unknown as Record<string, unknown>)[prop] = value;
      }
    }

    // Build a ParserConfig from the current config attributes. Read ONCE, at
    // client creation — config is immutable per stream.
    #readConfig(): ParserConfig | undefined {
      const cfg: ParserConfig = {};
      let any = false;
      const set = (attr: string, key: keyof ParserConfig): void => {
        const v = parseTriBool(this.getAttribute(attr));
        if (v !== undefined) {
          (cfg as Record<string, unknown>)[key] = v;
          any = true;
        }
      };
      set("gfm-autolinks", "gfmAutolinks");
      set("gfm-alerts", "gfmAlerts");
      set("gfm-footnotes", "gfmFootnotes");
      set("gfm-math", "gfmMath");
      set("dir-auto", "dirAuto");
      set("a11y", "a11y");
      set("unsafe-html", "unsafeHtml");

      const tags = this.getAttribute("component-tags");
      if (tags !== null) {
        const list = tags.split(/[\s,]+/).filter(Boolean);
        if (list.length > 0) {
          cfg.componentTags = list;
          any = true;
        }
      }
      return any ? cfg : undefined;
    }

    // Lazily create the internal client from config attributes (self-owned).
    #ensureClient(): void {
      if (this.#client) return;
      this.#client = new FluxClient({ config: this.#readConfig() });
      this.#ownsClient = true;
      this.#mountIfReady();
    }

    // Mount once a client exists and we're connected. Idempotent.
    #mountIfReady(): void {
      if (!this.#connected || !this.#client || this.#handle) return;
      this.#handle = mountFluxMarkdown(this.#client, this, {
        components: this.#components,
        sanitize: this.#sanitize,
      });
    }

    // Destroy the current mount and remount against the current client+options.
    // Used when a property changes while connected.
    #remount(): void {
      this.#handle?.destroy();
      this.#handle = null;
      this.#mountIfReady();
    }

    // Tear down only the client side (mount stays / is handled by the caller).
    // Destroys the client only if self-owned, then clears it and the mount so
    // the next mount targets a fresh client.
    #teardownClient(): void {
      // A swap/destroy abandons the current client; stop feeding it from src.
      this.#cancelSrcStream();
      this.#handle?.destroy();
      this.#handle = null;
      if (this.#ownsClient) this.#client?.destroy();
      this.#client = null;
      this.#ownsClient = false;
    }

    // Resolve the initial content of a self-owned stream from the attributes,
    // in priority order: `src` (fetch+stream) > `markdown` (one-shot) >
    // textContent (one-shot). A caller-owned client never reaches here.
    #resolveInitialContent(): void {
      // Single chokepoint: every content-source resolution supersedes any
      // in-flight `src` fetch. This covers the src→markdown / src→textContent
      // transitions too — #oneShot reuses (resets + finalizes) the same client,
      // so without this a still-pending fetch would append into the finished
      // stream. (#streamFromSrc bumps again; the extra bump is harmless.)
      this.#cancelSrcStream();
      const src = this.getAttribute("src");
      if (src) {
        void this.#streamFromSrc(src);
        return;
      }
      const markdown = this.getAttribute("markdown");
      if (markdown !== null) {
        this.#oneShot(markdown);
        return;
      }
      // textContent-as-initial-markdown: capture, clear, then feed. Capture
      // BEFORE the mount appended its `.flux-md` root would pollute the text;
      // mount happened in connectedCallback, so read only our own text nodes.
      const text = this.#captureSourceText();
      if (text.trim().length > 0) this.#oneShot(text);
    }

    // Read the raw markdown the host put between the tags, ignoring the
    // renderer's `.flux-md` root (and any other element children).
    #captureSourceText(): string {
      let text = "";
      for (const node of Array.from(this.childNodes)) {
        if (node.nodeType === 3 /* Text */) {
          text += node.textContent ?? "";
          node.parentNode?.removeChild(node);
        }
      }
      return text;
    }

    // One-shot: reset the stream (in case content changed), feed it, finalize.
    #oneShot(markdown: string): void {
      this.#ensureClient();
      this.#client!.reset();
      this.#client!.append(markdown);
      this.#client!.finalize();
    }

    // Abort any in-flight `src` fetch and invalidate its read loop, so it can
    // no longer append into a client we're about to reuse, swap, or destroy.
    #cancelSrcStream(): void {
      this.#srcSeq++;
      this.#srcAbort?.abort();
      this.#srcAbort = null;
    }

    // Fetch a URL and stream its body. TextDecoder with {stream:true} carries a
    // multibyte sequence that straddles a chunk boundary into the next decode.
    async #streamFromSrc(src: string): Promise<void> {
      // Supersede any prior in-flight src, then tag this run with a fresh token.
      this.#cancelSrcStream();
      const token = this.#srcSeq;
      const abort = new AbortController();
      this.#srcAbort = abort;

      this.#ensureClient();
      this.#client!.reset();
      const owned = this.#client!;
      // True while THIS run is still the active stream: not superseded by a
      // newer src, and the client wasn't swapped/destroyed out from under us.
      const current = () => this.#srcSeq === token && this.#client === owned;

      try {
        const res = await fetch(src, { signal: abort.signal });
        if (!current()) return;
        const body = res.body;
        if (!body) {
          const text = await res.text();
          if (!current()) return;
          owned.append(text);
          owned.finalize();
          return;
        }
        const reader = body.getReader();
        const decoder = new TextDecoder();
        for (;;) {
          const { done, value } = await reader.read();
          if (!current()) return;
          if (done) break;
          if (value) owned.append(decoder.decode(value, { stream: true }));
        }
        owned.append(decoder.decode()); // flush any trailing partial sequence
        owned.finalize();
      } catch (err) {
        // A supersede/disconnect aborts the fetch — intentional, not an error.
        if (abort.signal.aborted || !current()) return;
        // eslint-disable-next-line no-console
        console.error("<flux-markdown>: failed to stream src", src, err);
      }
    }
  }

  customElements.define(tag, FluxMarkdownElement);
}

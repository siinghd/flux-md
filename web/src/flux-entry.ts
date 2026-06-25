// Self-hosted flux-md browser bundle entry for isobox chatbot.
//
// Importing this module:
//   - registers the <flux-markdown> custom element (light DOM),
//   - re-exports FluxClient + defineFluxMarkdown as ESM named exports,
//   - mirrors both onto window (window.FluxClient / window.defineFluxMarkdown)
//     so a plain <script type="module"> page can use them without bundling.
//
// The flux-md styles are imported so Vite emits a co-located CSS asset; the
// page should also <link> it (see report) for polished, themed markdown.
import { defineFluxMarkdown } from "flux-md/element";
import { FluxClient } from "flux-md/client";
import "flux-md/styles.css";

// Auto-register on import so consumers just need <flux-markdown> in the DOM.
defineFluxMarkdown();

declare global {
  interface Window {
    FluxClient: typeof FluxClient;
    defineFluxMarkdown: typeof defineFluxMarkdown;
  }
}

if (typeof window !== "undefined") {
  window.FluxClient = FluxClient;
  window.defineFluxMarkdown = defineFluxMarkdown;
}

export { FluxClient, defineFluxMarkdown };

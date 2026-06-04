/**
 * flux-md: zero-dep streaming markdown for the browser.
 *
 * Public surface:
 *   - FluxClient: owns one Web Worker + Rust parser per stream
 *   - FluxMarkdown: React component that subscribes to a FluxClient
 *   - Block / Patch / BlockKind types
 *   - highlight: optional in-house syntax highlighter
 *
 * Typical use (React + a Vite-like bundler):
 *
 *     import { FluxClient, FluxMarkdown } from "flux-md";
 *     const client = new FluxClient();
 *     // ... in your component: <FluxMarkdown client={client} />
 *     // ... wherever your tokens land: client.append(deltaText);
 *     client.finalize();
 */
export { FluxClient, FluxPool, getDefaultPool } from "./client";
export { FluxMarkdown, useFluxStream, useFluxMarkdownString } from "./react";
export { highlight, supportedLangs } from "./hi";
export { htmlToReact, parseTrustedHtml } from "./html-to-react";
export type {
  Block,
  BlockKind,
  BlockKindTag,
  BlockComponentProps,
  Components,
  Patch,
  FromWorker,
  ToWorker,
  WorkerLike,
  ParserConfig,
  Align,
  TableCell,
  TableData,
  HeadingData,
  CodeBlockData,
  MathBlockData,
  ListData,
} from "./types";

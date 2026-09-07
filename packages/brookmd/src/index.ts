/**
 * brookmd: zero-dep streaming markdown for the browser.
 *
 * Public surface:
 *   - BrookClient: owns one Web Worker + Rust parser per stream
 *   - BrookMarkdown: React component that subscribes to a BrookClient
 *   - Block / Patch / BlockKind types
 *   - highlight / registerLanguage: optional in-house syntax highlighter
 *
 * Typical use (React + a Vite-like bundler):
 *
 *     import { BrookClient, BrookMarkdown } from "brookmd";
 *     const client = new BrookClient();
 *     // ... in your component: <BrookMarkdown client={client} />
 *     // ... wherever your tokens land: client.append(deltaText);
 *     client.finalize();
 */
export { BrookClient, BrookPool, getDefaultPool, sourceFingerprint } from "./client";
export type { PersistableSnapshot } from "./client";
export { BrookMarkdown, useBrookStream, useBrookMarkdownString } from "./react";
export { highlight, registerLanguage, supportedLangs } from "./hi";
export type { LanguageDef } from "./hi";
export { htmlToReact, parseTrustedHtml, safeUrl, wrapLink } from "./html-to-react";
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
  NestedBlock,
  ContainerData,
  Decorator,
  UrlTransform,
  LinkClickInfo,
  BrookNode,
} from "./types";

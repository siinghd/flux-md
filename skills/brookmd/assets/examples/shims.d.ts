/**
 * Ambient stubs so these examples typecheck without installing the optional
 * third-party packages they demonstrate. Nothing here is part of brookmd's API
 * — copy the example, install the real package, and delete the shim.
 */

// brookmd's dev-only warnings read `process.env.NODE_ENV` behind a
// `typeof process !== "undefined"` guard. This project typechecks against the
// package SOURCE (see tsconfig `paths`) with no `@types/node`, so declare it.
declare const process: { env: { NODE_ENV?: string } };

// Side-effect CSS imports (`import "brookmd/styles.css"`) are a bundler concern;
// TypeScript needs a declaration for them.
declare module "*.css";

declare module "@ai-sdk/react" {
  export const useChat: any;
}

declare module "dompurify" {
  const DOMPurify: { sanitize(html: string, config?: any): string };
  export default DOMPurify;
}

declare module "katex" {
  const katex: any;
  export default katex;
}

declare module "mermaid" {
  const mermaid: any;
  export default mermaid;
}

declare module "shiki" {
  export const codeToHtml: (
    code: string,
    options: { lang: string; theme: string },
  ) => Promise<string>;
}

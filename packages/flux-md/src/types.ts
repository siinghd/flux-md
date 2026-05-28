// Public type surface, split so framework-neutral consumers (`flux-md/client`,
// `flux-md/dom`) typecheck without resolving react: the neutral types live in
// ./types-core, the lone React-coupled `Components` type in ./types-react.
// Re-exported here so `flux-md/types`, index.ts, and every existing import see
// the identical surface as before.
export * from "./types-core";
export * from "./types-react";

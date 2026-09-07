import type { ComponentType } from "react";
import type { BlockComponentProps, BlockKindTag } from "./types-core";

/**
 * Override map for {@link BrookMarkdown}. Keys are either lowercase HTML tag
 * names (`table`, `a`, `code`, `h1`… — applied inside a
 * block's HTML) or capitalized block-kind names (`BlockKindTag`, e.g.
 * `CodeBlock`, `Table` — replace the whole block renderer). Values are a React
 * component or an HTML tag string.
 *
 * ## Two prop contracts — read this before writing an override
 *
 * The same map is consulted by two dispatchers, and they pass different props:
 *
 * - **Block contract.** A block-kind key, or a `componentTags` tag matched at
 *   block level, receives {@link BlockComponentProps} — `block`, `html`, `open`,
 *   `speculative` (plus `tag`/`attrs`/`children` for component tags).
 * - **Tag contract.** The SAME key is also matched by *element name* while
 *   converting a block's HTML to React — which is how `a`/`code`/`table`
 *   overrides work, and also how an `inlineComponentTags` chip, or a component
 *   tag nested inside a list item / blockquote, is rendered. That call passes
 *   the element's attributes and `children` only: **there is no `block` prop.**
 *
 * So a component registered for a tag that can appear in both positions must not
 * assume `block` exists:
 *
 * ```tsx
 * const Thinking = ({ block, children }: any) =>
 *   block ? <Panel data={block.kind.data}>{children}</Panel> : <span>{children}</span>;
 * ```
 *
 * Block-kind keys are typed to {@link BlockComponentProps} below so the mismatch
 * is a compile error rather than a runtime `undefined` deref; brookmd also
 * refuses to dispatch a raw element whose name collides with a block-kind key,
 * and wraps every block in an error boundary so a throwing override costs one
 * block instead of the document.
 */
export type Components = {
  [K in BlockKindTag]?: ComponentType<BlockComponentProps> | string;
} & {
  [tag: string]: ComponentType<any> | string | undefined;
};

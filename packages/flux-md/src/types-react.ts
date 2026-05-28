import type { ComponentType } from "react";

/**
 * Override map for {@link FluxMarkdown}. Keys are either lowercase HTML tag
 * names (`table`, `a`, `code`, `h1`… — react-markdown style, applied inside a
 * block's HTML) or capitalized block-kind names (`BlockKindTag`, e.g.
 * `CodeBlock`, `Table` — replace the whole block renderer). Values are a React
 * component or an HTML tag string.
 *
 * Tag-level components receive the element's parsed attributes (with
 * `class`→`className`, `style` as an object) plus `children`. Block-kind
 * components receive `BlockComponentProps`. There is no `node` prop.
 */
export type Components = Record<string, ComponentType<any> | string>;

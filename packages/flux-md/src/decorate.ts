// Shared, framework-neutral inline-text matcher for the `decorators` API.
//
// THE ONE place the "split an inline text node by the user's decorators into a
// list of [text | match] segments" logic lives, so the React walker
// (html-to-react.ts) and the DOM TreeWalker (dom.ts) can NEVER drift. It is pure
// string + RegExp logic — no browser globals, no React — so it is SSR-cold-import
// safe and reusable by future Vue/Svelte/Solid bindings.
//
// Design invariants (the security/perf review corrections):
//   - The matcher feeds the ORIGINAL text-node string only; a later decorator
//     never re-matches inside an earlier decorator's REPLACEMENT (no double-wrap).
//   - `replace` is treated as PURE: no global match counter, so a value streamed
//     char-by-char produces the identical final output as one-shot rendering.
//   - A `/g` (or non-global) user RegExp is normalized to a FRESH global regex
//     per call, so `lastIndex` never carries across text nodes.
//   - Matching is per-text-node: a value split by inline markup (e.g.
//     `$2.<em>5</em>B`) is two separate text nodes and will not match across them.

import type { Decorator } from "./types-core";

/** Ancestor tags inside which decoration is skipped by default. */
export const DEFAULT_SKIP: readonly string[] = ["a", "code", "pre", "kbd"];

/** A run of the original text, or one matched span the caller turns into a node. */
export type DecorateSegment =
  | { type: "text"; text: string }
  | { type: "match"; decorator: Decorator; matchText: string; groups: string[] };

// Escape a literal string so it can be embedded in a RegExp source.
function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// Build a FRESH global RegExp for one matcher. A user RegExp is cloned with its
// flags normalized (drop sticky `y`, ensure `g`) so a single `exec` loop finds
// every match in the node and never inherits a stale `lastIndex`. A string
// matcher becomes a global literal-escaped pattern.
function toGlobalRegExp(match: RegExp | string): RegExp {
  if (typeof match === "string") return new RegExp(escapeRegExp(match), "g");
  let flags = match.flags.replace("y", "");
  if (!flags.includes("g")) flags += "g";
  return new RegExp(match.source, flags);
}

// True when any ancestor tag matches the decorator's skip set (case-insensitive).
function skippedByAncestor(ancestors: string[], skip: readonly string[]): boolean {
  for (let i = 0; i < ancestors.length; i++) {
    const a = ancestors[i].toLowerCase();
    for (let j = 0; j < skip.length; j++) {
      if (a === skip[j].toLowerCase()) return true;
    }
  }
  return false;
}

// Split ONE text string by ONE decorator into text/match segments. Returns the
// single unchanged text segment when nothing matches (the common fast path).
function splitOne(text: string, dec: Decorator, out: DecorateSegment[]): boolean {
  const re = toGlobalRegExp(dec.match);
  let last = 0;
  let matched = false;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    const matchText = m[0];
    // Zero-width match: advance to guarantee forward progress (no infinite loop).
    if (matchText.length === 0) {
      re.lastIndex++;
      continue;
    }
    matched = true;
    if (m.index > last) out.push({ type: "text", text: text.slice(last, m.index) });
    // `groups` is documented as string[]: map an unmatched optional group to "".
    const groups: string[] = [];
    for (let i = 1; i < m.length; i++) groups.push(m[i] ?? "");
    out.push({ type: "match", decorator: dec, matchText, groups });
    last = m.index + matchText.length;
  }
  if (!matched) return false;
  if (last < text.length) out.push({ type: "text", text: text.slice(last) });
  return true;
}

/**
 * Split one inline text node's string into segments per the active decorators.
 * Decorators apply in order; each one only re-scans the still-TEXT segments left
 * by the previous ones (so a match is never decorated twice). `ancestors` is the
 * chain of enclosing tag names — a decorator whose `skipInside` (default
 * {@link DEFAULT_SKIP}) intersects it is not applied here.
 *
 * Returns `null` when nothing matched at all, so the caller can take the
 * zero-allocation fast path of emitting the original text node unchanged.
 */
export function decorateSegments(
  text: string,
  decorators: Decorator[],
  ancestors: string[],
): DecorateSegment[] | null {
  let segments: DecorateSegment[] | null = null; // built lazily on first match
  for (let d = 0; d < decorators.length; d++) {
    const dec = decorators[d];
    if (skippedByAncestor(ancestors, dec.skipInside ?? DEFAULT_SKIP)) continue;
    const source: DecorateSegment[] = segments ?? [{ type: "text", text }];
    let changedThisDecorator = false;
    const next: DecorateSegment[] = [];
    for (let i = 0; i < source.length; i++) {
      const seg = source[i];
      if (seg.type !== "text") {
        next.push(seg);
        continue;
      }
      const before = next.length;
      if (splitOne(seg.text, dec, next)) {
        changedThisDecorator = true;
      } else {
        next.length = before; // splitOne pushed nothing on no-match
        next.push(seg);
      }
    }
    if (changedThisDecorator) segments = next;
  }
  return segments;
}

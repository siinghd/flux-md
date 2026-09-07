import { test, expect } from "bun:test";
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";

// Class contract between the renderers and the shipped theme.
//
// `src/styles.css` is opt-in (`import "brookmd/styles.css"`), so a `brook-*`
// class that a renderer emits but the theme never mentions is not a crash — it
// is silently unstyled output in every app that imported the theme expecting
// "good-looking by default". This test is the tripwire: emit a new class, style
// it, or the suite goes red.
//
// Plain string scanning on purpose — no DOM, no CSS parser, no build step (the
// file is copied verbatim to dist by scripts/build.mjs).

const srcDir = join(import.meta.dir, "..", "src");
const css = readFileSync(join(srcDir, "styles.css"), "utf8");

/** CSS with comments removed, so a class named only in prose never counts. */
const cssCode = css.replace(/\/\*[\s\S]*?\*\//g, "");

/** Every file that can put a class on a rendered node. */
const emitters = [
  join(srcDir, "react.tsx"),
  join(srcDir, "dom.ts"),
  join(srcDir, "server-react.tsx"),
  ...readdirSync(join(srcDir, "renderers"))
    .filter((f) => f.endsWith(".tsx") || f.endsWith(".ts"))
    .map((f) => join(srcDir, "renderers", f)),
];

/**
 * Classes the theme is not expected to carry a rule for:
 *  - `brook-block-<kind>` is a per-kind family generated from the block kind at
 *    runtime, so it is open-ended by construction (the theme styles the shared
 *    `brook-block` instead, plus whichever kinds want a tweak).
 *  - the root modifiers are the caller's switches, not emitted decoration:
 *    `brook-md` is the scope every rule already hangs off, `brook-dark` /
 *    `brook-light` force a palette, `brook-caret` opts into the caret, and
 *    `brook-deferred` is documented as a hook for the app to style itself.
 */
const ROOT_MODIFIERS = new Set(["brook-md", "brook-dark", "brook-light", "brook-caret", "brook-deferred"]);

/**
 * Every `brook-*` token the emitters put on a node, as it must appear in a
 * selector: `.brook-code-block` for a class, `[data-brook-pending]` for the
 * streaming-link attribute. Read as text (not shelled out to grep) because
 * several of these sources carry raw NUL bytes that truncate a byte scan.
 */
function emittedTokens(): string[] {
  const found = new Set<string>();
  for (const file of emitters) {
    const text = readFileSync(file, "utf8");
    for (const m of text.matchAll(/(data-)?brook-[a-z0-9-]+/g)) {
      const [token, dataPrefix] = m;
      if (dataPrefix) {
        found.add(`[${token}]`);
        continue;
      }
      if (token.startsWith("brook-block-")) continue;
      if (ROOT_MODIFIERS.has(token)) continue;
      found.add(`.${token}`);
    }
  }
  return [...found].sort();
}

/** True when `sel` (`.foo` or `[data-foo]`) is used as a selector in the theme. */
function hasSelector(sel: string): boolean {
  const literal = sel.replace(/[.[\]]/g, (c) => `\\${c}`);
  return new RegExp(`${literal}(?![A-Za-z0-9_-])`).test(cssCode);
}

/** Split a selector list on top-level commas only — `:is(a, b)` is one selector. */
function splitSelectors(list: string): string[] {
  const out: string[] = [];
  let depth = 0;
  let cur = "";
  for (const ch of list) {
    if (ch === "(") depth++;
    else if (ch === ")") depth--;
    if (ch === "," && depth === 0) {
      out.push(cur.trim());
      cur = "";
    } else cur += ch;
  }
  if (cur.trim()) out.push(cur.trim());
  return out;
}

test("the renderers emit no brook-* class the shipped theme leaves unstyled", () => {
  const emitted = emittedTokens();
  // Guard the guard: a regex that silently matched nothing would pass forever.
  expect(emitted.length).toBeGreaterThan(10);
  expect(emitted).toContain(".brook-code-block");
  expect(emitted).toContain(".brook-block");
  expect(emitted).toContain("[data-brook-pending]");

  const unstyled = emitted.filter((sel) => !hasSelector(sel));
  expect(unstyled).toEqual([]);
});

test("the fenced-block chrome is styled end to end", () => {
  // Named explicitly (not just via the sweep above) so deleting a rule fails
  // with the name of the thing that lost its styling.
  for (const chrome of ["code", "math", "mermaid"]) {
    for (const part of ["block", "header", "lang", "body"]) {
      const sel = `.brook-${chrome}-${part}`;
      expect([sel, hasSelector(sel)]).toEqual([sel, true]);
    }
  }
  expect(hasSelector(".brook-code-copy")).toBe(true);
  expect(hasSelector(".brook-code-streaming-pill")).toBe(true);
  // The copy button's "copied" state rides on aria-label in both adapters.
  expect(cssCode).toContain('.brook-code-copy[aria-label="Copied"]');
  // The scroll sentinel must take no space: no height AND no inherited gap.
  expect(/\.brook-bottom-anchor\s*\{[^}]*height:\s*0[^}]*margin:\s*0/.test(cssCode)).toBe(true);
});

test("the opt-in caret is scoped to .brook-caret and only paints open blocks", () => {
  const at = cssCode.indexOf(".brook-md.brook-caret");
  expect(at).toBeGreaterThan(-1);
  // Every caret selector must be gated on BOTH the opt-in root class and the
  // open-block marker, or settled content would grow a permanent cursor.
  const selectors = splitSelectors(cssCode.slice(at, cssCode.indexOf("{", at)));
  expect(selectors.length).toBeGreaterThan(3);
  for (const sel of selectors) {
    expect(sel).toContain(".brook-caret");
    expect(sel).toContain(".brook-open");
    expect(sel).toContain("::after");
    // Fences show the streaming pill instead; the caret must never land inside
    // one (nor on the `pre` a code block falls back to on the generic path).
    expect(sel).not.toContain("brook-code");
    expect(sel).not.toContain("brook-math");
    expect(sel).not.toContain("brook-mermaid");
  }
  expect(cssCode).toContain("@keyframes brook-caret-blink");
});

test("prefers-reduced-motion disables both animations", () => {
  // Both animations are applied through a custom property, so the reduced-motion
  // block switches them off by redefining those two properties on the root.
  expect(cssCode).toContain("animation: var(--brook-caret-anim)");
  expect(cssCode).toContain("animation: var(--brook-pill-anim)");
  expect(cssCode).toContain("@keyframes brook-pill-pulse");

  const at = cssCode.indexOf("@media (prefers-reduced-motion: reduce)");
  expect(at).toBeGreaterThan(-1);
  const block = cssCode.slice(at, cssCode.indexOf("\n}", cssCode.indexOf("{", at)));
  expect(block).toContain("--brook-caret-anim: none");
  expect(block).toContain("--brook-pill-anim: none");
});

test("every custom property the theme uses is defined for light and dark", () => {
  const defined = new Set([...cssCode.matchAll(/(--brook-[a-z0-9-]+)\s*:/g)].map((m) => m[1]));
  const used = new Set([...cssCode.matchAll(/var\((--brook-[a-z0-9-]+)/g)].map((m) => m[1]));
  for (const name of used) expect([name, defined.has(name)]).toEqual([name, true]);

  // Palette vars that differ per mode must be restated in BOTH forced-dark
  // shapes: the `prefers-color-scheme` block and the explicit `.brook-dark`
  // escape hatch. (`.brook-light` needs none — it falls through to the base
  // `.brook-md` block by opting out of the media query.)
  const media = cssCode.slice(
    cssCode.indexOf("@media (prefers-color-scheme: dark)"),
    cssCode.indexOf("}\n}", cssCode.indexOf("@media (prefers-color-scheme: dark)")),
  );
  const forced = cssCode.slice(cssCode.indexOf(".brook-md.brook-dark {"));
  for (const name of ["--brook-alert-tip", "--brook-alert-important", "--brook-alert-warning", "--brook-alert-caution"]) {
    expect([name, media.includes(`${name}:`)]).toEqual([name, true]);
    expect([name, forced.slice(0, forced.indexOf("}")).includes(`${name}:`)]).toEqual([name, true]);
  }
});

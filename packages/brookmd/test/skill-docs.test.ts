// Agent-skill contract gate.
//
// `skills/brookmd/` is what `npx skills add siinghd/brookmd` installs into a
// coding agent, and `llms.txt` is what an agent reads when it lands on the repo
// or the demo site. Both are documentation that no compiler checks — so this
// test is the compiler: it fails when the skill's frontmatter is malformed,
// when a relative link points at a file that does not exist, when an example
// or reference imports a `brookmd/<subpath>` that the package does not export,
// or when a public-facing doc names another rendering library (a standing
// project rule: brookmd is described on its own terms only).
//
// The examples themselves are typechecked by CI (`tsc -p
// skills/brookmd/assets/examples/tsconfig.json`), which covers symbol names and
// prop types; this file covers everything tsc cannot see.

import { describe, expect, test } from "bun:test";
import { existsSync, readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";

const ROOT = path.resolve(import.meta.dir, "../../..");
const SKILL_DIR = path.join(ROOT, "skills", "brookmd");
const SKILL_MD = path.join(SKILL_DIR, "SKILL.md");
const PKG = JSON.parse(readFileSync(path.join(ROOT, "packages/brookmd/package.json"), "utf8")) as {
  exports: Record<string, unknown>;
};

function walk(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) walk(full, out);
    else out.push(full);
  }
  return out;
}

const skillFiles = existsSync(SKILL_DIR) ? walk(SKILL_DIR) : [];
const skillMarkdown = skillFiles.filter((f) => f.endsWith(".md"));
const skillCode = skillFiles.filter((f) => /\.(ts|tsx|js|mjs|html)$/.test(f));

/** The public docs that must exist and must obey the rules below. */
const PUBLIC_DOCS = [
  SKILL_MD,
  path.join(ROOT, "llms.txt"),
  path.join(ROOT, "web/public/llms.txt"),
  path.join(ROOT, "README.md"),
  path.join(ROOT, "packages/brookmd/README.md"),
  path.join(ROOT, "web/public/llms-full.txt"),
];

describe("skills/brookmd — installable agent skill", () => {
  test("SKILL.md exists with valid frontmatter (name + description)", () => {
    expect(existsSync(SKILL_MD)).toBe(true);
    const src = readFileSync(SKILL_MD, "utf8");
    const m = /^---\r?\n([\s\S]*?)\r?\n---\r?\n/.exec(src);
    expect(m, "SKILL.md must start with a YAML frontmatter block").toBeTruthy();
    const fm = m![1];
    const name = /^name:\s*(.+)$/m.exec(fm)?.[1]?.trim();
    expect(name).toBe("brookmd");
    // The skills CLI requires lowercase-hyphen names.
    expect(name).toMatch(/^[a-z0-9]+(?:-[a-z0-9]+)*$/);
    // `description:` may be a scalar or a folded block (`>-`); either way the
    // key must exist and carry non-empty text on the following lines.
    const descIdx = fm.search(/^description:/m);
    expect(descIdx, "frontmatter needs a description").toBeGreaterThanOrEqual(0);
    const descBody = fm
      .slice(descIdx)
      .replace(/^description:\s*(>-?|\|-?)?/, "")
      .trim();
    expect(descBody.length).toBeGreaterThan(40);
  });

  test("the skill has references and typed examples", () => {
    expect(existsSync(path.join(SKILL_DIR, "references"))).toBe(true);
    expect(existsSync(path.join(SKILL_DIR, "assets/examples/tsconfig.json"))).toBe(true);
    expect(skillMarkdown.length).toBeGreaterThan(1);
    expect(skillCode.length).toBeGreaterThan(0);
  });

  test("every relative link in the skill resolves to a real file", () => {
    const broken: string[] = [];
    for (const file of skillMarkdown) {
      const src = readFileSync(file, "utf8");
      // [text](target) — skip absolute URLs, mailto, and in-page anchors.
      for (const m of src.matchAll(/\]\(([^)\s]+)(?:\s+"[^"]*")?\)/g)) {
        const target = m[1];
        if (/^(?:[a-z]+:|#)/i.test(target)) continue;
        const clean = target.split("#")[0];
        if (!clean) continue;
        const abs = path.resolve(path.dirname(file), clean);
        if (!existsSync(abs)) broken.push(`${path.relative(ROOT, file)} -> ${target}`);
      }
    }
    expect(broken).toEqual([]);
  });

  test("every `brookmd/<subpath>` the skill mentions is a real package export", () => {
    const exportsSet = new Set(Object.keys(PKG.exports).map((k) => k.replace(/^\.\/?/, "")));
    const unknown = new Set<string>();
    for (const file of [...skillMarkdown, ...skillCode, ...PUBLIC_DOCS.filter(existsSync)]) {
      const src = readFileSync(file, "utf8");
      for (const m of src.matchAll(/["'`]brookmd\/([A-Za-z0-9_./-]+)["'`]/g)) {
        const sub = m[1];
        if (!exportsSet.has(sub)) unknown.add(`${path.relative(ROOT, file)}: brookmd/${sub}`);
      }
    }
    expect([...unknown]).toEqual([]);
  });

  test("public docs describe brookmd on its own terms (no other-library names)", () => {
    // Deliberately specific tokens: bare words like "marked" or "remark" are
    // ordinary English and would false-positive.
    const forbidden = /\b(?:streamdown|react-markdown|markdown-it|micromark|remark-[a-z]+|rehype(?:-[a-z]+)?|hast|mdast|pulldown-cmark|comrak|marked\.js|showdown|snarkdown)\b/i;
    const hits: string[] = [];
    for (const file of [...skillMarkdown, ...skillCode, ...PUBLIC_DOCS.filter(existsSync)]) {
      const lines = readFileSync(file, "utf8").split("\n");
      lines.forEach((line, i) => {
        if (forbidden.test(line)) hits.push(`${path.relative(ROOT, file)}:${i + 1}: ${line.trim()}`);
      });
    }
    expect(hits).toEqual([]);
  });

  test("llms.txt exists at the repo root and on the demo site, and they match", () => {
    const a = path.join(ROOT, "llms.txt");
    const b = path.join(ROOT, "web/public/llms.txt");
    expect(existsSync(a)).toBe(true);
    expect(existsSync(b)).toBe(true);
    expect(readFileSync(a, "utf8")).toBe(readFileSync(b, "utf8"));
    expect(readFileSync(a, "utf8")).toMatch(/^# brookmd/m);
  });

  test("the root README advertises the one-line skill install", () => {
    const readme = readFileSync(path.join(ROOT, "README.md"), "utf8");
    expect(readme).toContain("npx skills add siinghd/brookmd");
  });

  test("Claude Code plugin manifests point at the skill", () => {
    const plugin = path.join(ROOT, ".claude-plugin/plugin.json");
    const market = path.join(ROOT, ".claude-plugin/marketplace.json");
    expect(existsSync(plugin)).toBe(true);
    expect(existsSync(market)).toBe(true);
    const p = JSON.parse(readFileSync(plugin, "utf8")) as { name?: string };
    expect(p.name).toBe("brookmd");
    JSON.parse(readFileSync(market, "utf8")); // must be valid JSON
  });
});

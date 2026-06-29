// packages/flux-md/scripts/build.mjs
//
// Structure-preserving compiled-dist build for flux-md.
//
// Why this shape (NOT a single inlined bundle): flux-md's core design relies on
// the CONSUMER's bundler re-emitting the Web Worker + the .wasm asset from the
// web-standard `new Worker(new URL("./worker.js", import.meta.url))` and
// `new URL("./wasm/...wasm", import.meta.url)` references. So dist/ mirrors src/
// 1:1 as ESM .js + .d.ts, the worker stays a standalone dist/worker.js, and the
// wasm binary + glue + styles.css are copied verbatim. The published package is
// pre-compiled JS instead of raw .ts/.tsx (Socket "unusual packaging" fix) while
// keeping every downstream resolution identical to the raw-source behaviour.
//
// Three steps plain tsc/esbuild SILENTLY OMIT — each a hard downstream break if
// skipped (see test/consumer-smoke):
//   1. rewrite the lone `./worker.ts` -> `./worker.js` URL literal in client.js
//   2. add explicit `.js` to every relative import/export specifier in .js + .d.ts
//      (Node native-ESM has no extension probing; nodenext type-resolution needs it)
//   3. copy src/wasm/* (incl. the 179 KB .wasm + the hand-written glue, which tsc
//      ignores because allowJs is off) and styles.css into dist
//
// Run: `node scripts/build.mjs` (paths anchored to this file). Requires esbuild
// (^0.28) + typescript (^5.6) — both hoisted in the bun workspace.

import { build } from "esbuild";
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import {
  cpSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync,
} from "node:fs";
import path from "node:path";

const require = createRequire(import.meta.url);
const pkgRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const srcDir = path.join(pkgRoot, "src");
const distDir = path.join(pkgRoot, "dist");

// --------------------------------------------------------------------------
// 0. clean
rmSync(distDir, { recursive: true, force: true });
mkdirSync(distDir, { recursive: true });

// --------------------------------------------------------------------------
// 1. enumerate entries: every src/**/*.ts(x) EXCEPT *.d.ts and the wasm/ dir
//    (wasm/*.js is hand-written glue copied verbatim; it has no .ts entries).
function walk(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) {
      if (path.relative(srcDir, full) === "wasm") continue; // copied as-is
      walk(full, out);
    } else if (/\.tsx?$/.test(name) && !/\.d\.ts$/.test(name)) {
      out.push(full);
    }
  }
  return out;
}
const entryPoints = walk(srcDir);

// --------------------------------------------------------------------------
// 2. transpile per-file (NO bundle) -> dist, preserving structure via outbase.
//    bundle:false => esbuild does NOT resolve/inline imports, so frameworks
//    (react/vue/svelte/solid-js, react/jsx-runtime) and ./wasm/* + node:* stay
//    external for free, and worker.ts emits as its own standalone dist/worker.js.
await build({
  entryPoints,
  outbase: srcDir,
  outdir: distDir,
  bundle: false,
  format: "esm",
  platform: "neutral",
  target: "es2022",
  jsx: "automatic",        // react.tsx + renderers/*.tsx -> react/jsx-runtime (external)
  jsxImportSource: "react",
  loader: { ".ts": "ts", ".tsx": "tsx" },
  logLevel: "info",
});

// --------------------------------------------------------------------------
// 3. copy assets verbatim: wasm dir (glue + .wasm + .d.ts) and styles.css.
cpSync(path.join(srcDir, "wasm"), path.join(distDir, "wasm"), { recursive: true });
cpSync(path.join(srcDir, "styles.css"), path.join(distDir, "styles.css"));

// --------------------------------------------------------------------------
// 4. emit .d.ts for every entry (incl .tsx) with tsc --emitDeclarationOnly.
//    Reuse the project tsconfig, flip emit on, silence style-only lints so a
//    dist build never blocks on noUnusedLocals/Parameters.
const tscBin = require.resolve("typescript/bin/tsc");
const tscArgs = [
  tscBin,
  "-p", path.join(pkgRoot, "tsconfig.json"),
  "--noEmit", "false",
  "--declaration",
  "--emitDeclarationOnly",
  "--rootDir", srcDir,
  "--outDir", distDir,
  "--noUnusedLocals", "false",
  "--noUnusedParameters", "false",
];
const tsc = spawnSync(process.execPath, tscArgs, { stdio: "inherit", cwd: pkgRoot });
if (tsc.status !== 0) {
  throw new Error(`tsc declaration emit failed (exit ${tsc.status})`);
}

// --------------------------------------------------------------------------
// 5. post-process: add explicit .js to relative import specifiers in .js + .d.ts,
//    and rewrite the worker URL string in client.js.
// Known real extensions we must NOT append .js to. Crucially this is an explicit
// allowlist, NOT "the basename contains a dot" — a relative module named e.g.
// "./types.v2" has a dotted basename but is NOT extensioned and DOES need ".js".
const KNOWN_EXT = /\.(js|mjs|cjs|css|wasm|json|node)$/;
function addJsExt(spec) {
  if (!/^\.\.?\//.test(spec)) return spec;            // bare/builtin: untouched
  const last = spec.split("/").pop();
  if (KNOWN_EXT.test(last)) return spec;              // already a real asset/module ext
  return spec + ".js";
}
function rewriteSpecifiers(code) {
  // a) `from "..."` (import ... from, export ... from, export * from)
  code = code.replace(
    /(\bfrom\s*)(["'])(\.\.?\/[^"']+)\2/g,
    (_m, kw, q, spec) => kw + q + addJsExt(spec) + q,
  );
  // b) side-effect `import "..."` and dynamic `import("...")`
  code = code.replace(
    /(\bimport\s*\(?\s*)(["'])(\.\.?\/[^"']+)\2/g,
    (_m, kw, q, spec) => kw + q + addJsExt(spec) + q,
  );
  return code;
}
function processTree(dir) {
  for (const name of readdirSync(dir)) {
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) {
      if (path.relative(distDir, full) === "wasm") continue; // glue is verbatim
      processTree(full);
    } else if (/\.js$/.test(name) || /\.d\.ts$/.test(name)) {
      writeFileSync(full, rewriteSpecifiers(readFileSync(full, "utf8")));
    }
  }
}
processTree(distDir);

// worker URL rewrite (client.js only): ./worker.ts -> ./worker.js
const clientPath = path.join(distDir, "client.js");
let client = readFileSync(clientPath, "utf8");
const workerRe = /(new URL\(\s*["'])\.\/worker\.ts(["'])/g;
const hits = (client.match(workerRe) || []).length;
if (hits !== 1) {
  throw new Error(`client.js: expected exactly 1 './worker.ts' URL, found ${hits}`);
}
client = client.replace(workerRe, "$1./worker.js$2");
writeFileSync(clientPath, client);

// --------------------------------------------------------------------------
// 6. assert the dist contract.
function assert(cond, msg) { if (!cond) throw new Error("build assert: " + msg); }
const read = (p) => readFileSync(path.join(distDir, p), "utf8");
assert(statSync(path.join(distDir, "worker.js")).isFile(), "dist/worker.js missing");
assert(statSync(path.join(distDir, "wasm/flux_md_core_bg.wasm")).isFile(), "wasm binary missing");
assert(statSync(path.join(distDir, "wasm/flux_md_core.js")).isFile(), "wasm glue missing");
assert(statSync(path.join(distDir, "styles.css")).isFile(), "styles.css missing");
assert(statSync(path.join(distDir, "index.d.ts")).isFile(), "index.d.ts missing");
assert(statSync(path.join(distDir, "client.d.ts")).isFile(), "client.d.ts missing");
assert(
  read("client.js").includes('new URL("./worker.js"') ||
  read("client.js").includes("new URL('./worker.js'"),
  "worker URL not rewritten to ./worker.js",
);
assert(!/new URL\(\s*["']\.\/worker\.ts/.test(read("client.js")), "stale ./worker.ts in client.js");
assert(read("worker.js").includes("./wasm/flux_md_core_bg.wasm"), "worker wasm ref lost");
// no extensionless relative specifier should survive in the entry barrel:
assert(!/\bfrom\s*["']\.\/(client|react|hi|html-to-react)["']/.test(read("index.js")),
  "index.js still has an extensionless relative import");

// flux-md/server is the documented React-FREE entry — a non-React consumer must
// be able to import renderToString/parseToBlocks/initFlux without react installed.
// An eager `react` import here (directly, or transitively via ./react.js /
// ./html-to-react.js) would break that, so fail the build if one reappears. The
// React server component lives in dist/server-react.js (flux-md/server/react).
assert(statSync(path.join(distDir, "server.js")).isFile(), "dist/server.js missing");
assert(statSync(path.join(distDir, "server-react.js")).isFile(), "dist/server-react.js missing");
const serverJs = read("server.js");
assert(!/\bfrom\s*["']react(\/|["'])/.test(serverJs), "dist/server.js imports react (must stay React-free)");
assert(!/\bfrom\s*["']\.\/(react|html-to-react)\.js["']/.test(serverJs),
  "dist/server.js imports ./react or ./html-to-react (pulls react transitively — must stay React-free)");
assert(/\bfrom\s*["']react["']/.test(read("server-react.js")), "dist/server-react.js should import react");

// FAIL-LOUD coverage for the regex-vs-parser gaps (the post-process is the only
// thing standing between source and a broken Node-ESM dist). Re-scan the FINAL
// dist and throw on anything the rewriter could have silently missed:
//   (1) a quoted relative specifier left without a known extension, and
//   (2) a relative dynamic import() with a template/computed arg we cannot fix.
function scanTree(dir, visit) {
  for (const name of readdirSync(dir)) {
    const full = path.join(dir, name);
    if (statSync(full).isDirectory()) {
      if (path.relative(distDir, full) === "wasm") continue; // verbatim glue
      scanTree(full, visit);
    } else if (/\.js$/.test(name) || /\.d\.ts$/.test(name)) {
      visit(full, readFileSync(full, "utf8"));
    }
  }
}
const specRe = /(?:\bfrom\s*|\bimport\s*\(?\s*)(["'])(\.\.?\/[^"']+)\1/g;
const templateImportRe = /\bimport\s*\(\s*`[^`]*\.\.?\//; // relative path inside a template literal
scanTree(distDir, (file, code) => {
  let m;
  while ((m = specRe.exec(code))) {
    const spec = m[2];
    const last = spec.split("/").pop();
    assert(KNOWN_EXT.test(last),
      `extensionless relative specifier ${JSON.stringify(spec)} in ${path.relative(distDir, file)}`);
  }
  assert(!templateImportRe.test(code),
    `unfixable relative template-literal dynamic import in ${path.relative(distDir, file)} (add an explicit ./x.js literal)`);
});
console.log("flux-md dist build OK ->", distDir);

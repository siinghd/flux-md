# Shipping brookmd

The repo is git-initialized, CI is wired (`.github/workflows/ci.yml`), and the
npm package is publish-ready. The steps below are the ones that need **your**
credentials/accounts — they are intentionally not automated.

## Status (already done)

- ✅ `git init` + `.gitignore` (excludes `target/`, `node_modules/`, `web/dist/`)
- ✅ CI: Rust suite (enforces CommonMark **652/652** + GFM floors) and the
  WASM-build + JS package job (typecheck, component/pool/store tests, web build)
- ✅ `packages/brookmd/package.json`: `publishConfig.access=public`,
  `prepublishOnly` (rebuilds WASM), `repository`/`homepage`/`bugs`
- ✅ `npm pack --dry-run` verified: the tarball is `dist/` (compiled ESM + `.d.ts`)
  including `dist/wasm/`, `dist/worker.js` and `dist/styles.css`, plus
  `README.md` / `CHANGELOG.md` — see `files` in `packages/brookmd/package.json`.
- ✅ npm name `brookmd` is **available** (registry returns 404).

## 1–2. Repo + push — ✅ DONE

Public repo: **https://github.com/siinghd/brookmd** (branch `main`). First CI
run is **green** (both the Rust and WASM/JS jobs). `repository`/`bugs` URLs in
`packages/brookmd/package.json` point at it.

## 3. Publish to npm

```bash
cd packages/brookmd       # run from HERE — prepublishOnly does `cd ../.. && bun run build:wasm`
npm login                 # or set NPM_TOKEN in CI for automated release
npm publish               # prepublishOnly rebuilds the WASM first
```

## Distribution note (not a blocker)

Since 0.17.0 the package is distributed as **compiled, non-minified ESM** —
`main`/`exports` point at `dist/*.js` with `.d.ts` types beside them, plus the
compiled `dist/wasm/`; no raw `.ts`/`.tsx` ships. That is what removed the
`transpilePackages` requirement on Next.js, which is now verified (App Router,
Turbopack *and* webpack, dev and build).

The worker + WASM use the web-standard `new URL(asset, import.meta.url)` pattern,
so they resolve in any bundler with asset-module support (Vite, webpack 5,
Rollup, Parcel) — not just Vite. Vite additionally needs
`optimizeDeps: { exclude: ["brookmd"] }` (its pre-bundler hoists the wasm-bindgen
glue and breaks the relative `.wasm` lookup). Nothing is inlined and the worker is
never a Blob URL — the consumer smoke test fails the build if either changes. If a
real consumer hits a bundler without `new URL` asset support (e.g. raw no-bundler
ESM, or older esbuild used directly), the fallback is an inline-everything build
(base64 WASM + Blob-URL worker) — kept on the shelf, not built speculatively.

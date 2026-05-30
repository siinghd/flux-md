/**
 * Playwright smoke for the Data Studio demo (flux-md 0.10.0 `blockData`).
 *
 * Proves, through a real browser, that rich UI is built from `block.kind.data`:
 *   1. EnhancedTable renders from the structured table data and SORTS by a
 *      column's plaintext when its header is clicked (asc → desc).
 *   2. The text filter narrows the visible rows.
 *   3. The Copy-CSV button carries a CSV string built from cell.text.
 *   4. The live Table of Contents renders one entry per streamed heading, each
 *      anchoring to an `id` that scrolls into view.
 *
 * Self-contained: it launches the built `vite preview` server, drives the page,
 * and tears everything down. Run AFTER `bun run build:web`:
 *
 *     node web/data-studio.smoke.mjs
 *
 * The default Playwright chromium (chromium-1223) isn't installed here, so we
 * resolve a chromium binary that IS on disk and pass it as `executablePath`.
 */
import { chromium } from "playwright";
import { spawn } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { homedir } from "node:os";

const WEB_DIR = dirname(fileURLToPath(import.meta.url));
const PORT = 4173;
const BASE = `http://127.0.0.1:${PORT}/`;

function findChromium() {
  // Prefer Playwright's own resolution; fall back to scanning the cache for any
  // installed chromium (full or headless_shell), newest first.
  try {
    const p = chromium.executablePath();
    if (p && existsSync(p)) return undefined; // default works — let Playwright pick
  } catch {
    /* fall through to manual scan */
  }
  const cache = join(homedir(), ".cache", "ms-playwright");
  if (!existsSync(cache)) return undefined;
  const dirs = readdirSync(cache)
    .filter((d) => d.startsWith("chromium"))
    .sort()
    .reverse();
  for (const d of dirs) {
    for (const bin of ["chrome-linux/chrome", "chrome-linux/headless_shell"]) {
      const p = join(cache, d, bin);
      if (existsSync(p)) return p;
    }
  }
  return undefined;
}

async function waitForServer(url, ms = 15000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try {
      const r = await fetch(url);
      if (r.ok) return;
    } catch {
      /* not up yet */
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`preview server did not come up at ${url}`);
}

function assert(cond, msg) {
  if (!cond) throw new Error("ASSERT FAILED: " + msg);
  console.log("  ✓ " + msg);
}

const preview = spawn("npx", ["vite", "preview", "--host", "127.0.0.1", "--port", String(PORT)], {
  cwd: WEB_DIR,
  stdio: "ignore",
});

let browser;
let failed = false;
try {
  await waitForServer(BASE);

  const executablePath = findChromium();
  browser = await chromium.launch(executablePath ? { executablePath } : {});
  const page = await browser.newPage();
  await page.goto(BASE, { waitUntil: "networkidle" });

  // Switch to the Data Studio tab and run the canned stream.
  await page.getByRole("tab", { name: "Data Studio" }).click();
  await page.getByRole("button", { name: /Run demo|Replay/ }).click();

  // The FIRST `.ds-table` is the "Benchmarks" grid — scope every query to that
  // element handle (the doc has a second table later, and `:first-of-type` is
  // element-type based, not class based, so we resolve the handle directly).
  await page.waitForSelector(".ds-table .ds-grid tbody tr", { timeout: 15000 });
  const tableEl = await page.$(".ds-table"); // Playwright `$` returns the first match
  assert(!!tableEl, "first EnhancedTable rendered");

  // Helpers scoped to the first table's element handle.
  const rowCount = () => tableEl.$$eval(".ds-grid tbody tr", (trs) => trs.length);
  const isOpen = () => tableEl.evaluate((el) => el.getAttribute("data-flux-open") === "1");
  const waitRows = async (n) => {
    for (let i = 0; i < 150 && (await rowCount()) < n; i++) await new Promise((r) => setTimeout(r, 100));
  };

  const headerTexts = await tableEl.$$eval(".ds-grid thead th", (ths) =>
    ths.map((t) => t.textContent.replace(/[▲▼↕]/g, "").trim()),
  );
  const insertCol = headerTexts.findIndex((h) => /Insert/.test(h));
  assert(insertCol >= 0, `found the "Insert (ns)" column (headers: ${headerTexts.join(", ")})`);

  const colValues = async (col) =>
    tableEl.$$eval(
      ".ds-grid tbody tr",
      (trs, c) => trs.map((tr) => tr.children[c]?.textContent.trim()).filter((v) => v != null),
      col,
    );
  const clickHeader = () => tableEl.$$eval(".ds-grid thead th", (ths, c) => ths[c].click(), insertCol);

  // --- 1. MID-STREAM SORT: the moment the grid has ≥2 rows (the table block is
  // still OPEN, more rows + the rest of the doc still arriving), sort ascending.
  // Because the view is DERIVED from the growing `kind.data`, the sort must stay
  // correct as the remaining rows stream into the already-sorted table.
  await waitRows(2);
  const openWhenSorted = await isOpen();
  await clickHeader(); // sort ascending while streaming
  console.log(`  · sorted while table still streaming (open): ${openWhenSorted}`);

  // Let the rest of the rows stream in, then assert the column is still sorted.
  await waitRows(5);
  const asc = (await colValues(insertCol)).map(Number);
  const ascSorted = [...asc].sort((a, b) => a - b);
  assert(asc.length >= 5, `benchmark table accrued ${asc.length} rows`);
  assert(
    JSON.stringify(asc) === JSON.stringify(ascSorted),
    `sort applied mid-stream stays correct as rows arrive: ${asc.join(",")}`,
  );

  // Sort descending (second click on the same header).
  await clickHeader();
  const desc = (await colValues(insertCol)).map(Number);
  const descSorted = [...asc].sort((a, b) => b - a);
  assert(JSON.stringify(desc) === JSON.stringify(descSorted), `column sorts descending: ${desc.join(",")}`);

  // --- 2. FILTER: typing narrows the rows by cell plaintext.
  const before = (await colValues(0)).length;
  const filterInput = await tableEl.$(".ds-filter");
  await filterInput.fill("Rope");
  for (let i = 0; i < 50 && (await rowCount()) >= before; i++) await new Promise((r) => setTimeout(r, 100));
  const after = (await colValues(0)).length;
  assert(after > 0 && after < before, `filter "Rope" narrows ${before} → ${after} rows`);
  await filterInput.fill("");

  // --- 3. CSV: the Copy-CSV button carries a CSV built from cell.text.
  const csv = await tableEl.$eval(".ds-csv-btn", (b) => b.getAttribute("data-csv"));
  assert(typeof csv === "string" && csv.split("\n").length >= 2, "Copy-CSV button has multi-row CSV from DATA");
  assert(/Insert/.test(csv.split("\n")[0]), "CSV header row is built from header cell.text");

  // --- 4. TOC: one link per streamed heading, anchoring to a real id target.
  // Wait for the stream to finish (the Run button leaves the "Streaming…"
  // state) so the full outline has accrued — proving it's driven live.
  await page.waitForFunction(
    () => {
      const b = document.querySelector(".ds-run-btn");
      return b && !/Streaming/.test(b.textContent);
    },
    { timeout: 20000 },
  );
  const tocCount = await page.$$eval(".ds-toc-link", (a) => a.length);
  assert(tocCount >= 5, `TOC rendered ${tocCount} heading links from kind.data`);

  const firstHref = await page.getAttribute(".ds-toc-link", "href");
  const id = firstHref.replace(/^.*#/, "");
  const hasTarget = await page.evaluate((i) => !!document.getElementById(i), id);
  assert(hasTarget, `first TOC link "#${id}" has a matching heading id to scroll to`);

  // --- 5. The descending sort applied mid-stream still holds now that the doc
  // has finished streaming — the view is DERIVED from kind.data, not a snapshot.
  const finalCol = (await colValues(insertCol)).map(Number);
  const stillDesc = JSON.stringify(finalCol) === JSON.stringify([...finalCol].sort((a, b) => b - a));
  assert(stillDesc, `mid-stream sort persists after stream completes: ${finalCol.join(",")}`);

  console.log("\nDATA STUDIO SMOKE: PASS");
} catch (err) {
  failed = true;
  console.error("\nDATA STUDIO SMOKE: FAIL\n" + (err?.stack || err));
} finally {
  if (browser) await browser.close();
  preview.kill("SIGTERM");
}
process.exit(failed ? 1 : 0);

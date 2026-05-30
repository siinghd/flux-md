//! Cost benchmark for the opt-in structured Table `kind.data` channel
//! (`setBlockData` / `with_block_data`). Run on demand, never in CI.
//!
//!   cargo run --release --example bench_table_data
//!
//! What it measures (honest, OFF vs ON):
//!   1. PARSE-TIME delta — stream a table-heavy doc through `StreamParser` in
//!      small chunks (the real hot path) with `block_data` OFF then ON. Reports
//!      best/median ms, MB/s, and the ON/OFF ratio. Swept across chunk sizes and
//!      two table widths/lengths so any super-linear growth shows up as a curve,
//!      not an average. Plain-text cells and markup-heavy cells are reported
//!      separately (markup is the honest worst case for the strip pass).
//!   2. PAYLOAD-SIZE delta — serialize with `serde_json` (a proxy for the
//!      `serde_wasm_bindgen`/postMessage payload, NOT the exact transferred byte
//!      count). Two numbers:
//!        (a) final per-table bytes (the finalized document snapshot), and
//!        (b) CUMULATIVE streamed bytes across every patch incl. speculative —
//!            the core emits `kind.data` on every patch, so the active table's
//!            data is re-shipped each append. The OFF path already re-ships the
//!            active table's html each append, so the fair delta is what `data/`
//!            adds ON TOP of bytes already in flight.
//!
//! The cost of the cell plaintext pass (`strip_inline_html`) specifically is
//! isolated in a crate-internal `#[ignore]`d test in `src/render.rs` (it is
//! `pub(crate)`, unreachable from here). Run it with:
//!   cargo test --release strip_pass_cost -- --ignored --nocapture

use std::time::{Duration, Instant};

use flux_md_core::{Block, StreamParser};

// ---- corpora ---------------------------------------------------------------

/// A GFM table streamed row-by-row, plain-text cells (cheap strip path: nothing
/// to strip). `cols` columns, grown until the doc reaches `target` bytes.
fn table_plain(target: usize, cols: usize) -> String {
    let mut s = String::with_capacity(target + 64);
    // header + alignment row
    s.push('|');
    for c in 0..cols {
        s.push_str(&format!(" Col {c} |"));
    }
    s.push_str("\n|");
    for _ in 0..cols {
        s.push_str(" --- |");
    }
    s.push('\n');
    let mut i = 0usize;
    while s.len() < target {
        s.push('|');
        for c in 0..cols {
            s.push_str(&format!(" Person {i} v{c} {} |", (i * 7 + c) % 1000));
        }
        s.push('\n');
        i += 1;
    }
    s
}

/// Same shape, but every cell carries inline markup (bold, italic, code, link).
/// This is where `strip_inline_html` does real work — more tags, longer html.
fn table_markup(target: usize, cols: usize) -> String {
    let mut s = String::with_capacity(target + 64);
    s.push('|');
    for c in 0..cols {
        s.push_str(&format!(" **Col {c}** |"));
    }
    s.push_str("\n|");
    for _ in 0..cols {
        s.push_str(" --- |");
    }
    s.push('\n');
    let mut i = 0usize;
    while s.len() < target {
        s.push('|');
        for c in 0..cols {
            s.push_str(&format!(
                " **Item {i}** with *em* and `code{c}` and a [link](https://example.com/{i}/{c}) |"
            ));
        }
        s.push('\n');
        i += 1;
    }
    s
}

// ---- parse-time ------------------------------------------------------------

/// Stream `input` in `chunk`-byte UTF-8-safe pieces, finalize, touch BOTH the
/// html AND the structured `kind.data` so the ON-path build/strip work cannot be
/// optimized away.
fn run_once(input: &str, chunk: usize, block_data: bool) -> Duration {
    let bytes = input.as_bytes();
    let t0 = Instant::now();
    let mut p = StreamParser::new()
        .with_gfm_autolinks(true)
        .with_block_data(block_data);
    let mut i = 0;
    while i < bytes.len() {
        let mut e = (i + chunk).min(bytes.len());
        while e < bytes.len() && (bytes[e] & 0xC0) == 0x80 {
            e += 1;
        }
        p.append(&input[i..e]);
        i = e;
    }
    p.finalize();
    let total = touch_blocks(p.all_blocks());
    std::hint::black_box(total);
    t0.elapsed()
}

/// Sum html length AND, for table-data blocks, every cell's text+html length, so
/// the optimizer can't elide the work that produced `kind.data`.
fn touch_blocks<'a>(blocks: impl Iterator<Item = &'a Block>) -> usize {
    use flux_md_core::BlockKind;
    let mut total = 0usize;
    for b in blocks {
        total += b.html.len();
        if let BlockKind::Table(Some(td)) = &b.kind {
            for h in &td.headers {
                total += h.text.len() + h.html.len();
            }
            for row in &td.rows {
                for cell in row.iter() {
                    total += cell.text.len() + cell.html.len();
                }
            }
        }
    }
    total
}

fn bench_pair(name: &str, input: &str, chunk: usize) {
    // best of 7 after a warm-up, both flag states.
    run_once(input, chunk, false);
    let off = best_median(input, chunk, false);
    run_once(input, chunk, true);
    let on = best_median(input, chunk, true);
    let mb = input.len() as f64 / 1e6;
    let off_mbps = mb / off.0.as_secs_f64();
    let on_mbps = mb / on.0.as_secs_f64();
    let ratio = on.0.as_secs_f64() / off.0.as_secs_f64();
    println!(
        "{name:22} chunk={chunk:>4}  OFF best {:>7.2} ms ({:>6.1} MB/s)  ON best {:>7.2} ms ({:>6.1} MB/s)  ON/OFF x{ratio:>4.2}  (med OFF {:>7.2} / ON {:>7.2})",
        off.0.as_secs_f64() * 1e3,
        off_mbps,
        on.0.as_secs_f64() * 1e3,
        on_mbps,
        off.1.as_secs_f64() * 1e3,
        on.1.as_secs_f64() * 1e3,
    );
}

fn best_median(input: &str, chunk: usize, block_data: bool) -> (Duration, Duration) {
    let mut runs: Vec<Duration> = (0..7).map(|_| run_once(input, chunk, block_data)).collect();
    runs.sort();
    (runs[0], runs[runs.len() / 2])
}

// ---- payload size ----------------------------------------------------------

/// Final per-document table-data bytes: serialize the finalized blocks OFF vs ON
/// and report the size of the single (one big) table block each way, plus the
/// delta attributable purely to `data/`.
struct PayloadReport {
    off_table_bytes: usize,
    on_table_bytes: usize,
    cum_off_bytes: usize,
    cum_on_bytes: usize,
    rows: usize,
}

/// Drive a fresh parser, accumulating the `serde_json` byte size of every patch
/// (newly_committed + active) across the whole stream — the cumulative bytes the
/// bridge would serialize and post. Then report the finalized table's size.
fn payload_report(input: &str, chunk: usize, block_data: bool, take_final: bool) -> (usize, usize, usize) {
    let bytes = input.as_bytes();
    let mut p = StreamParser::new()
        .with_gfm_autolinks(true)
        .with_block_data(block_data);
    let mut cumulative = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        let mut e = (i + chunk).min(bytes.len());
        while e < bytes.len() && (bytes[e] & 0xC0) == 0x80 {
            e += 1;
        }
        let patch = p.append(&input[i..e]);
        cumulative += patch_bytes(&patch);
        i = e;
    }
    let patch = p.finalize();
    cumulative += patch_bytes(&patch);

    let (final_table_bytes, rows) = if take_final {
        // the finalized table block (largest single block in the doc)
        let mut best = 0usize;
        let mut rows = 0usize;
        for b in p.all_blocks() {
            let n = serde_json::to_string(b).unwrap().len();
            if n > best {
                best = n;
                rows = table_rows(b);
            }
        }
        (best, rows)
    } else {
        (0, 0)
    };
    (final_table_bytes, cumulative, rows)
}

fn patch_bytes(patch: &flux_md_core::Patch) -> usize {
    let mut n = 0usize;
    for b in &patch.newly_committed {
        n += serde_json::to_string(b).unwrap().len();
    }
    for b in &patch.active {
        n += serde_json::to_string(b).unwrap().len();
    }
    n
}

fn table_rows(b: &Block) -> usize {
    use flux_md_core::BlockKind;
    match &b.kind {
        BlockKind::Table(Some(td)) => td.rows.len(),
        _ => 0,
    }
}

fn payload(name: &str, input: &str, chunk: usize) -> PayloadReport {
    let off = payload_report(input, chunk, false, true);
    let on = payload_report(input, chunk, true, true);
    let r = PayloadReport {
        off_table_bytes: off.0,
        on_table_bytes: on.0,
        cum_off_bytes: off.1,
        cum_on_bytes: on.1,
        rows: on.2,
    };
    let final_delta = r.on_table_bytes as i64 - r.off_table_bytes as i64;
    let cum_delta = r.cum_on_bytes as i64 - r.cum_off_bytes as i64;
    let per_row = if r.rows > 0 { final_delta as f64 / r.rows as f64 } else { 0.0 };
    let cum_ratio = r.cum_on_bytes as f64 / r.cum_off_bytes as f64;
    println!(
        "{name:22} rows={:>5}  final-table OFF {:>8} B / ON {:>8} B  (data adds {:>+8} B, {:>5.1} B/row)  |  cumulative-streamed OFF {:>10} B / ON {:>10} B  (x{cum_ratio:.2}, +{} B)",
        r.rows, r.off_table_bytes, r.on_table_bytes, final_delta, per_row, r.cum_off_bytes, r.cum_on_bytes, cum_delta,
    );
    r
}

fn main() {
    println!("flux-md-core Table kind.data cost bench (best of 7, release)\n");

    // Two widths × two lengths so super-linear behavior is visible as a curve.
    let plain_small = table_plain(50_000, 4);
    let plain_large = table_plain(200_000, 4);
    let plain_wide = table_plain(200_000, 10);
    let markup_large = table_markup(200_000, 4);
    let markup_wide = table_markup(200_000, 10);

    println!("== PARSE-TIME (OFF vs ON; ratio is the cost multiplier) ==\n");
    for &chunk in &[16usize, 64, 256, 1024] {
        bench_pair("plain  50K x4col", &plain_small, chunk);
        bench_pair("plain 200K x4col", &plain_large, chunk);
        bench_pair("plain 200K x10col", &plain_wide, chunk);
        bench_pair("markup 200K x4col", &markup_large, chunk);
        bench_pair("markup 200K x10col", &markup_wide, chunk);
        println!();
    }

    println!("== PAYLOAD SIZE (serde_json bytes; proxy for postMessage payload) ==\n");
    // Payload shape is chunk-independent for the FINAL table; cumulative depends
    // on chunk (more appends => more re-ships). Show a fine chunk (worst case for
    // cumulative) and a coarse chunk.
    for &chunk in &[64usize, 1024] {
        println!("-- chunk={chunk} --");
        payload("plain 200K x4col", &plain_large, chunk);
        payload("plain 200K x10col", &plain_wide, chunk);
        payload("markup 200K x4col", &markup_large, chunk);
        payload("markup 200K x10col", &markup_wide, chunk);
        println!();
    }
}

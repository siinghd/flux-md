//! Streaming throughput micro-benchmark for the parser core (run on demand,
//! never in CI). It feeds representative documents through `StreamParser` in
//! small chunks — the real streaming hot path — and reports MB/s.
//!
//!   cargo run --release --example bench
//!
//! Scenarios:
//!   mixed      — typical LLM markdown (headings, lists, code, table, quotes)
//!   big_code   — one huge fenced block (the O(n²) uncommitted-tail worst case)
//!   ref_heavy  — many link reference definitions (exercises ref-table cloning)
//!   math       — inline + display math (the new feature), with gfmMath on

use std::time::{Duration, Instant};

use flux_md_core::StreamParser;

fn mixed_doc(target: usize) -> String {
    let unit = "## Section heading\n\nSome **bold** and *italic* prose with a \
[link](https://example.com/path) and `inline code`, plus a bare \
https://example.org/x autolink.\n\n\
- first item\n- second item with `code`\n- third item\n\n\
1. one\n2. two\n\n\
```rust\nfn main() {\n    let x = 1 + 2;\n    println!(\"{x}\");\n}\n```\n\n\
| name | value |\n|:-----|------:|\n| a | 1 |\n| b | 2 |\n\n\
> a block quote with some **emphasis** inside it\n\n";
    let mut s = String::with_capacity(target + unit.len());
    while s.len() < target {
        s.push_str(unit);
    }
    s
}

fn big_code(target: usize) -> String {
    let mut s = String::with_capacity(target + 16);
    s.push_str("```rust\n");
    let line = "    let result = compute(alpha, beta, gamma); // a line of code\n";
    while s.len() < target {
        s.push_str(line);
    }
    s.push_str("```\n");
    s
}

// One huge open `$$…$$` display-math block — the math analogue of `big_code`.
// A multi-line aligned environment is exactly the long-block shape LLMs emit.
fn big_math(target: usize) -> String {
    let mut s = String::with_capacity(target + 16);
    s.push_str("$$\n\\begin{aligned}\n");
    let line = "x_{n+1} &= \\frac{1}{2}\\left(x_n + \\frac{a}{x_n}\\right) \\\\\n";
    while s.len() < target {
        s.push_str(line);
    }
    s.push_str("\\end{aligned}\n$$\n");
    s
}

// One huge GFM table streamed row-by-row — now that tables render incrementally,
// this is the shape to watch for an O(n²) re-render-all-rows-per-append cost.
fn big_table(target: usize) -> String {
    let mut s = String::with_capacity(target + 32);
    s.push_str("| Name | Age | City | Score |\n| --- | --- | --- | --- |\n");
    let mut i = 0;
    while s.len() < target {
        s.push_str(&format!("| Person {i} | {} | Town {i} | {} |\n", 20 + (i % 60), i * 7 % 1000));
        i += 1;
    }
    s
}

fn ref_heavy(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!("Paragraph {i} cites [topic {i}][r{i}] and more text here.\n\n"));
    }
    for i in 0..n {
        s.push_str(&format!("[r{i}]: https://example.com/page/{i} \"Title number {i}\"\n"));
    }
    s
}

fn long_paragraph(target: usize) -> String {
    // One paragraph, no blank lines — a model writing a long explanation without
    // breaks. Like an open code fence, it never commits, so the whole tail is
    // re-rendered each append (but inline rendering can't be cached the same way,
    // since late delimiters can change earlier output).
    let mut s = String::with_capacity(target + 32);
    let unit = "and then the system processes each token in sequence which means ";
    while s.len() < target {
        s.push_str(unit);
    }
    s.push('\n');
    s
}

fn emphasis_paragraph(target: usize) -> String {
    // One unbroken paragraph with emphasis/code/links sprinkled in. The cache
    // can't commit past an early construct, so this stays O(n²) — included to
    // report that the long-paragraph fix is honest about its coverage (plain
    // paragraphs O(n); construct-laden ones unchanged).
    let mut s = String::with_capacity(target + 64);
    let mut i = 0usize;
    while s.len() < target {
        s.push_str("some words of ordinary explanation continue here and ");
        if i % 5 == 0 {
            s.push_str("**bold** and *italic* and `code` ");
        }
        i += 1;
    }
    s.push('\n');
    s
}

fn math_doc(target: usize) -> String {
    let unit = "The mass-energy relation $E = mc^2$ and the inline \\(a_1 + b_2\\) form.\n\n\
$$\n\\sum_{i=1}^{n} x_i = \\frac{n(n+1)}{2}\n$$\n\n\
Then \\[ \\int_0^1 f(x)\\,dx \\] as a display block, with $\\alpha$ trailing.\n\n";
    let mut s = String::with_capacity(target + unit.len());
    while s.len() < target {
        s.push_str(unit);
    }
    s
}

/// Stream `input` through a fresh parser in `chunk`-byte pieces (split on UTF-8
/// boundaries), finalize, and touch the output so nothing is optimized away.
fn run_once(input: &str, chunk: usize, math: bool) -> Duration {
    let bytes = input.as_bytes();
    let t0 = Instant::now();
    let mut p = StreamParser::new()
        .with_gfm_autolinks(true)
        .with_gfm_alerts(true)
        .with_gfm_math(math);
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
    let total: usize = p.all_blocks().map(|b| b.html.len()).sum();
    std::hint::black_box(total);
    t0.elapsed()
}

fn bench(name: &str, input: &str, chunk: usize, math: bool) {
    // Warm up, then report best AND median over several runs — a gap between
    // them flags noise, so a small regression in a fast case isn't mistaken for
    // signal.
    run_once(input, chunk, math);
    let mut runs: Vec<Duration> = (0..7).map(|_| run_once(input, chunk, math)).collect();
    runs.sort();
    let best = runs[0];
    let median = runs[runs.len() / 2];
    let mb = input.len() as f64 / 1e6;
    println!(
        "{name:14} {:>9} B  chunk={chunk:>4}  best {:>8.2} ms  median {:>8.2} ms  {:>7.1} MB/s",
        input.len(),
        best.as_secs_f64() * 1e3,
        median.as_secs_f64() * 1e3,
        mb / best.as_secs_f64(),
    );
}

// One huge open blockquote — a model "explaining its reasoning" in a `>` quote
// or a `> [!NOTE]` alert with many lines. Blockquote re-parses its inner each
// append, so this catches the analogue of the long-paragraph shape inside a
// resumable container.
fn big_blockquote(target: usize) -> String {
    let mut s = String::with_capacity(target + 32);
    let unit = "> a continuation line with some **emphasis** and `code` here, plus more prose to bulk it up.\n";
    while s.len() < target {
        s.push_str(unit);
    }
    s
}

// A long flat unordered list — the shape an LLM emits when producing a 100+
// item bulleted breakdown. List items are a resumable container, so each
// append re-parses the whole growing tail.
fn big_list(target: usize) -> String {
    let mut s = String::with_capacity(target + 32);
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("- item {i} with some **bold** and a `bit of code` for flavor\n"));
        i += 1;
    }
    s
}

// A `> [!NOTE]` alert with many lines of content — exercises the GFM alert
// render path (a Blockquote variant with inner markdown).
fn big_alert(target: usize) -> String {
    let mut s = String::from("> [!NOTE]\n");
    while s.len() < target {
        s.push_str("> a continuation line of the note body with **bold** and a [link](https://example.com) thrown in.\n");
    }
    s
}

// Many small paragraphs separated by blank lines — the shape of a long prose
// reply with frequent paragraph breaks. Each paragraph commits quickly; this
// stresses the commit + active-vec churn path, not any single-block cache.
fn many_paragraphs(target: usize) -> String {
    let mut s = String::with_capacity(target + 64);
    let unit = "A short paragraph of explanation with one **bold** word and an `inline` snippet.\n\nAnd a second paragraph here for variety, ending with a [link](https://example.com).\n\n";
    while s.len() < target {
        s.push_str(unit);
    }
    s
}

fn main() {
    println!("flux-md-core streaming bench (best of 7, release)\n");
    let mixed = mixed_doc(200_000);
    let code = big_code(200_000);
    let refs = ref_heavy(2_000);
    let math = math_doc(200_000);
    let mathblk = big_math(200_000);
    let para = long_paragraph(200_000);
    let emph = emphasis_paragraph(200_000);
    let table = big_table(200_000);
    // Resumable containers (blockquote / list / alert) are still O(n²) when their
    // inner grows as a single open block: every append re-scans + re-renders the
    // whole growing inner. Sized down to 50 KB so the bench completes in seconds
    // and the quadratic curve is still visible — at 200 KB these would dominate
    // the runtime. See CHANGELOG / known-limitations for the planned fix.
    let quote = big_blockquote(50_000);
    let list = big_list(50_000);
    let alert = big_alert(50_000);
    let manyp = many_paragraphs(200_000);

    // Small chunks = many appends = many tail re-parses (the demanding case).
    // The intermediate sizes (64/128/512) matter: a scenario that's fast at 16
    // and 256 can still hide an O(n²/chunk) curve between them (the table-cache
    // fix in 0.5.2 was the proof). Print every shape × every chunk so the next
    // regression can't slip in unnoticed.
    for &chunk in &[16usize, 64, 128, 256, 512, 1024] {
        bench("mixed", &mixed, chunk, false);
        bench("big_code", &code, chunk, false);
        bench("long_paragraph", &para, chunk, false);
        bench("emphasis_para", &emph, chunk, false);
        bench("big_table", &table, chunk, false);
        bench("big_blockquote", &quote, chunk, false);
        bench("big_list", &list, chunk, false);
        bench("big_alert", &alert, chunk, false);
        bench("many_paragraphs", &manyp, chunk, false);
        bench("ref_heavy", &refs, chunk, false);
        bench("math", &math, chunk, true);
        bench("big_math", &mathblk, chunk, true);
        println!();
    }
}

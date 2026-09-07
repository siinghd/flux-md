//! Deterministic complexity gate — bounds the streaming parser's *work* (not
//! wall-clock time) across a size span, per document shape, per metric.
//!
//! Three counters (see `brook_md_core::perf`), because streaming cost goes
//! quadratic in three distinct ways:
//!
//! - `scanned`  — slow-path tail re-scan bytes. Every cliff we shipped and then
//!   fixed by hand (0.18.2 ref-def runs, 0.18.3 nested lists, 0.18.4 blockquote
//!   contents) was a tail that stopped committing, so `reparse_tail` re-scanned
//!   a growing suffix each append.
//! - `rendered` — bytes entering the inline renderer. Catches cache-INTERNAL
//!   quadratics the scan counter is blind to: a cache that stays armed but
//!   re-inline-renders a growing region every append (open list item bodies,
//!   table partial rows, pinned container paragraph cuts).
//! - `emitted`  — HTML bytes crossing the `append`/`finalize` patch boundary.
//!   Informational only (printed, never asserted): re-emitting the full open
//!   block per append is the current wire contract, so this is inherently
//!   O(n²/chunk) for any giant single open block.
//!
//! Each shape declares an expectation per gated metric:
//!
//! - `Linear`         — must stay ~O(n); ratio across the span ≤ 4x linear.
//! - `KnownQuadratic` — documented O(n²), the open fix-campaign target list
//!   (named by hunt group key). Guarded against regressing PAST quadratic
//!   (an accidental O(n³)). When a group is fixed, flip it to `Linear`.
//! - `Untracked`      — the metric cannot see this shape's work (wall-only
//!   cost, e.g. memcpy/allocator churn); printed, not asserted.
//!
//! Deterministic: counts work, so it gates in CI without flaking on noisy
//! shared runners. Run with:
//!
//!   cargo test --release --features perf_counters --test scaling -- --nocapture
//!
//! Without the feature the whole file compiles to nothing.

#![cfg(feature = "perf_counters")]

use brook_md_core::{perf, StreamParser};
use std::time::Instant;

// ---- harness ---------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Expect {
    Linear,
    KnownQuadratic,
    /// Declared vocabulary for future entries whose cliff no counter can see
    /// (wall-only): printed, never asserted. Currently every registered shape
    /// classifies as Linear or KnownQuadratic on both gated metrics.
    #[allow(dead_code)]
    Untracked,
}

/// Builder options a shape needs beyond the always-on base set
/// (autolinks + alerts + math, the richest common streaming configuration).
#[derive(Clone, Copy, Default)]
struct Opts {
    footnotes: bool,
    block_data: bool,
    component_tags: &'static [&'static str],
    unsafe_html: bool,
    /// Giant-word shapes are linear exactly when extended autolinks are off (a
    /// future `@` legitimately binds an alnum run right-to-left, so with
    /// autolinks on the commit cut is semantically pinned).
    no_autolinks: bool,
    /// Wire delta mode (`WIRE.md` §11): active re-emits count only their
    /// `append` tails toward `emitted`. See `wire_delta_emitted_is_linear`.
    wire_delta: bool,
    /// Safe raw-HTML sanitizer, allow-all (`set_html_sanitize(true, [], [])`).
    html_sanitize: bool,
    /// Block-level raw HTML (`set_block_html`) — only bites with `html_sanitize`.
    block_html: bool,
}

struct Shape {
    name: &'static str,
    gen: fn(usize) -> String,
    opts: Opts,
    chunk: usize,
    small: usize,
    large: usize,
    scanned: Expect,
    rendered: Expect,
}

struct Work {
    scanned: u64,
    rendered: u64,
    emitted: u64,
    wall_ms: f64,
}

/// Stream `md` in `chunk`-byte pieces (UTF-8 safe), finalize, and return all
/// work counters plus wall time. Small chunks = many appends = the most
/// demanding case for an incremental parser.
fn measure(md: &str, chunk: usize, o: Opts) -> Work {
    perf::reset();
    let bytes = md.as_bytes();
    let mut p = StreamParser::new()
        .with_gfm_autolinks(!o.no_autolinks)
        .with_gfm_alerts(true)
        .with_gfm_math(true)
        .with_gfm_footnotes(o.footnotes)
        .with_block_data(o.block_data)
        .with_unsafe_html(o.unsafe_html)
        .with_wire_delta(o.wire_delta)
        .with_block_html(o.block_html);
    if o.html_sanitize {
        p = p.with_html_sanitize(true, Vec::new(), Vec::new());
    }
    if !o.component_tags.is_empty() {
        p = p.with_component_tags(o.component_tags.iter().map(|s| s.to_string()).collect());
    }
    let start = Instant::now();
    let mut i = 0;
    while i < bytes.len() {
        let mut e = (i + chunk).min(bytes.len());
        while e < bytes.len() && (bytes[e] & 0xC0) == 0x80 {
            e += 1;
        }
        p.append(&md[i..e]);
        i = e;
    }
    p.finalize();
    let wall_ms = start.elapsed().as_secs_f64() * 1e3;
    Work {
        scanned: perf::scanned_bytes().max(1),
        rendered: perf::rendered_bytes().max(1),
        emitted: perf::emitted_bytes().max(1),
        wall_ms,
    }
}

// ---- document-shape generators (size-parametric) --------------------------

fn repeat_to(unit: &str, target: usize) -> String {
    let mut s = String::with_capacity(target + unit.len());
    while s.len() < target {
        s.push_str(unit);
    }
    s
}

fn mixed(target: usize) -> String {
    repeat_to(
        "## Section heading\n\nSome **bold** and *italic* prose with a \
[link](https://example.com/path) and `inline code`.\n\n\
- first item\n- second item with `code`\n- third item\n\n\
1. one\n2. two\n\n\
```rust\nfn main() { let x = 1 + 2; }\n```\n\n\
| name | value |\n|:-----|------:|\n| a | 1 |\n| b | 2 |\n\n\
> a block quote with some **emphasis** inside it\n\n",
        target,
    )
}

fn many_paragraphs(target: usize) -> String {
    repeat_to(
        "A short paragraph of explanation with one **bold** word and an `inline` snippet.\n\n\
And a second paragraph here for variety, ending with a [link](https://example.com).\n\n",
        target,
    )
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

fn big_list(target: usize) -> String {
    let mut s = String::with_capacity(target + 32);
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("- item {i} with some **bold** and a `bit of code` for flavor\n"));
        i += 1;
    }
    s
}

fn nested_loose_list(target: usize) -> String {
    // The 0.18.3 flicker shape: loose outer bullets with 2-space nested subs.
    let mut s = String::with_capacity(target + 32);
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("- item {i}\n  - sub a\n  - sub b\n  - sub c\n\n"));
        i += 1;
    }
    s
}

fn big_blockquote(target: usize) -> String {
    repeat_to(
        "> a continuation line with some **emphasis** and `code` here, plus more prose.\n",
        target,
    )
}

fn quote_many_paras(target: usize) -> String {
    // A prose blockquote whose body is MANY short inner paragraphs (each blank
    // `>` line closes one) — the ContainerCache shape whose committed-paras data
    // channel re-emits per append. Kept linear by the Rc-shared committed entries.
    let mut s = String::with_capacity(target + 32);
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("> para {i} with some **bold** prose here\n>\n"));
        i += 1;
    }
    s
}

fn bq_lazy_continuation(target: usize) -> String {
    // One `>` line, then marker-less lazy paragraph-continuation lines forever
    // (CommonMark laziness). The container cache used to bail on every lazy
    // line, so the never-committing quote re-scanned its whole tail per append
    // (O(n²)); the cache now glues lazy lines exactly like `blockquote_inner`.
    let mut s = String::from("> the quoted paragraph starts here\n");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("lazy continuation line {i} with plain prose words\n"));
        i += 1;
    }
    s
}

fn quote_ref_defs(target: usize) -> String {
    // A blockquote hosting a growing run of link-reference definitions. The
    // recursive container-block cache used to refuse any container holding a
    // `]:` (document-global scoping), so the quote armed NO cache and the whole
    // growing tail re-scanned per append (O(n²), 44 s @ 512 KB). The nested
    // parser now consumes def lines natively (its own def-run commit), and the
    // outer full reparse re-derives the global ref table whenever the container
    // closes/commits.
    let mut s = String::with_capacity(target + 64);
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("> [r{i}]: https://example.com/page/{i} \"Title {i}\"\n"));
        i += 1;
    }
    s
}

fn quote_footnote_defs(target: usize) -> String {
    // Same shape with `[^label]:` defs. With footnotes OFF (this harness),
    // `[^f0]:` is a plain link-ref def whose label happens to start with `^` —
    // it must stream linearly like `quote_ref_defs`. (With footnotes ON the
    // container cache still bails on `[^` — document-global numbering — and
    // that flavor remains a known-quadratic follow-up.)
    let mut s = String::with_capacity(target + 64);
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("> [^f{i}]: https://example.com/note/{i} \"Note {i}\"\n"));
        i += 1;
    }
    s
}

fn quote_depth_growing(target: usize) -> String {
    // Ever-deepening nested blockquotes: line k carries k `>` markers. The
    // recursive container-block cache spent one nested parser per level, capped at
    // MAX_CONTAINER_DEPTH; past the cap the innermost parser full-reparsed its
    // growing tail every append — worse than quadratic. Now linear via the
    // iterative DeepQuoteCache (fold each settled shallower level's opener once,
    // single open parser for the deepest line, a heap stack instead of nested
    // parsers → no shadow-stack cost), byte-identical to the recursive path.
    let mut s = String::with_capacity(target + 4096);
    let mut k = 1usize;
    while s.len() < target {
        for _ in 0..k {
            s.push_str("> ");
        }
        s.push_str(&format!("level {k} prose with **bold**\n"));
        k += 1;
    }
    s
}

fn big_alert(target: usize) -> String {
    // The 0.18.4 shape: a `> [!NOTE]` alert with structured inner blocks. The
    // recursive container-block cache renders the `>`-stripped inner through a
    // nested StreamParser, so it now streams linearly instead of re-parsing the
    // whole growing alert body every append.
    let mut s = String::from("> [!NOTE]\n");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("> - point {i} with **bold**\n> - point {} more\n", i + 1));
        i += 2;
    }
    s
}

fn blockquote_with_list(target: usize) -> String {
    // A plain `>` blockquote whose body is a list — the other structured-inner
    // container shape the recursive container-block cache makes linear.
    let mut s = String::with_capacity(target + 32);
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("> - point {i} with **bold** and `code`\n"));
        i += 1;
    }
    s
}

fn big_table(target: usize) -> String {
    let mut s = String::from("| Name | Age | City | Score |\n| --- | --- | --- | --- |\n");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("| Person {i} | {} | Town {i} | {} |\n", 20 + (i % 60), i * 7 % 1000));
        i += 1;
    }
    s
}

fn big_code(target: usize) -> String {
    let mut s = String::from("```rust\n");
    let line = "    let result = compute(alpha, beta, gamma); // a line of code\n";
    while s.len() < target {
        s.push_str(line);
    }
    s.push_str("```\n");
    s
}

/// A fence whose OPENER is indented, so §4.5 makes every body line shed those
/// columns. The de-indent is per-line work at fold time; if a cache ever
/// re-derived it by rescanning the accumulated body per append, this shape goes
/// quadratic while flush-left `big_code` stays flat.
fn indented_big_code(target: usize) -> String {
    let mut s = String::from("   ```rust\n");
    let line = "   let result = compute(alpha, beta, gamma); // a line of code\n";
    while s.len() < target {
        s.push_str(line);
    }
    s.push_str("   ```\n");
    s
}

fn big_math(target: usize) -> String {
    let mut s = String::from("$$\n\\begin{aligned}\n");
    let line = "x_{n+1} &= \\frac{1}{2}\\left(x_n + \\frac{a}{x_n}\\right) \\\\\n";
    while s.len() < target {
        s.push_str(line);
    }
    s.push_str("\\end{aligned}\n$$\n");
    s
}

// ---- fix-campaign generators (one per verified O(n²) hunt group) -----------

/// open-block-html-reemit: a giant never-closing fence. Cache-hit appends
/// re-materialize the whole open block's HTML (memcpy + Block clone) — a
/// wall-only cliff both work counters are blind to; `emitted` shows it.
fn unclosed_fence(target: usize) -> String {
    let mut s = String::from("```rust\n");
    while s.len() < target {
        s.push_str("let result = compute(alpha, beta, gamma); // never closes\n");
    }
    s
}

/// commit-cut-pinned-no-boundary: a single enormous word — zero inter-word
/// boundary candidates, so the paragraph cache never arms and every append
/// full-rescans + re-renders the whole tail.
fn one_giant_word(target: usize) -> String {
    "a".repeat(target)
}

/// uncached-open-block-kinds (FIXED): an open ComponentBlock had no
/// incremental cache arm in `reparse_tail`, so its growing body full-rescanned
/// every append; it now streams via ComponentBlockCache (recursive nested
/// parser, like the container shapes).
fn component_block_open(target: usize) -> String {
    let mut s = String::from("<Chart>\n");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("data point {i} with **bold** and `code` in the body line\n"));
        i += 1;
    }
    s // close tag never arrives
}

/// component-open-blank-line: the blank-line twin of `component_block_open`.
/// That shape's body is one unbroken run of single lines, so it never ends an
/// append on a blank line and its `scanned` sits at a constant 249 bytes —
/// structurally blind to the bail this pins. Here the body is PARAGRAPHS, so
/// the buffer ends `\n\n` once per paragraph, which is where the open-tail
/// disagreement between the nested parser's forced-open commits and the full
/// rescan's settled render lives. That used to drop the cache and rebuild a
/// fresh nested parser over the whole body every time — O(body) per paragraph.
///
/// Each unit is exactly `CHUNK` bytes and ends in the blank line (the header
/// line is folded into the first unit), so EVERY append boundary is a `\n\n`
/// boundary and the shape exercises the path deterministically instead of
/// depending on where the chunk grid happens to fall.
fn component_multi_para(target: usize) -> String {
    component_paras(target, |i| format!("para {i} with **bold** and words"))
}

/// Same shape with the inline constructs a real `<Thinking>` / `<Callout>` body
/// is full of: backticks, brackets, `$` and `<`. Every one of those is an
/// `open_tail` TRIGGER byte, so a sensitivity heuristic that latches on the
/// trigger alone (the `open_item_ot_sensitive` scheme the list cache uses)
/// leaves this variant quadratic while `component_multi_para` looks fixed.
fn component_multi_para_rich(target: usize) -> String {
    component_paras(target, |i| {
        format!("para {i} `code` [link](https://e.com/p{i}) $x$ and 3 < 4")
    })
}

/// Shared body builder for the two shapes above: `<Chart>` that never closes,
/// then paragraphs padded to exactly `CHUNK` bytes including their blank line.
fn component_paras(target: usize, body: fn(usize) -> String) -> String {
    const HEAD: &str = "<Chart>\n";
    let mut s = String::from(HEAD);
    let mut i = 0usize;
    while s.len() < target {
        // First unit absorbs the open-tag line so the grid starts aligned.
        let unit = if i == 0 { CHUNK - HEAD.len() } else { CHUNK };
        let mut para = body(i);
        para.truncate(unit - 2);
        // Pad inside the last word — trailing spaces would be a hard break.
        while para.len() < unit - 2 {
            para.push('x');
        }
        para.push_str("\n\n");
        s.push_str(&para);
        i += 1;
    }
    s // close tag never arrives
}

/// The SETTLED-BODY-PREFIX shape: a `<Chart>` that never closes whose body is
/// thousands of TINY paragraphs, so the nested parser commits one sub-block per
/// four source bytes. `component_multi_para` has the same failure mode but at
/// CHUNK-sized paragraphs, where the open block's own per-append `Block.html`
/// re-emit (the documented O(n²/chunk) memcpy floor — see the `emitted` column)
/// swamps the signal; here the sub-block COUNT, not the byte count, dominates,
/// so re-walking the committed sub-blocks per append stands out against that
/// floor instead of hiding under it.
///
/// What it pins: the wrapper assembler must fold each committed sub-block into
/// its settled prefix exactly ONCE and re-emit that prefix as a single
/// contiguous copy. Rebuilding the body from `all_blocks()` instead measured
/// 7.5x this shape's wall time at 256 KB (1671 ms vs 222 ms) while both work
/// counters stayed flat and green — the assembler scans nothing and inline-
/// renders nothing, it only allocates and memcpys.
fn component_tiny_paras(target: usize) -> String {
    let mut s = String::from("<Chart>\n");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("w{i}\n\n"));
        i += 1;
    }
    s // close tag never arrives
}

/// The LINEAR control twin of [`component_tiny_paras`]: byte-for-byte the same
/// paragraphs, minus the `<Chart>` opener line. Without the wrapper each
/// paragraph is an ordinary top-level block that commits and is emitted exactly
/// once, so the same bytes, the same block count and the same rendered html
/// stream in O(new bytes) — the runner's weather hits both twins identically.
fn tiny_paras_toplevel(target: usize) -> String {
    let mut s = String::new();
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("w{i}\n\n"));
        i += 1;
    }
    s
}

/// A single giant ATX heading line, still growing (no newline) — streams via
/// HeadingCache (the paragraph cache's settled-prefix scheme in `<hN>`).
fn heading_words(target: usize) -> String {
    let mut s = String::from("# ");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("word{i} "));
        i += 1;
    }
    s
}

/// Same shape with inline constructs: pre-cache this went ~cubic in wall time
/// (quadratic re-scan × superlinear whole-line inline re-render).
fn heading_emphasis(target: usize) -> String {
    let mut s = String::from("# ");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("*word{i}* and **bold{i}** "));
        i += 1;
    }
    s
}

/// A thematic-break line still growing — RuleCache (constant `<hr>`).
fn growing_rule(target: usize) -> String {
    "-".repeat(target)
}

/// A code fence whose OPENER line (info string) grows without a newline —
/// FenceInfoCache (output frozen once the first info word settles).
fn fence_giant_info(target: usize) -> String {
    let mut s = String::from("```rust ");
    while s.len() < target {
        s.push_str("attr ");
    }
    s
}

/// The linear CONTROL twin of [`fence_giant_info`]: the same `` ```rust `` +
/// `attr `-run bytes, cut into CLOSED fences with a bounded 1 KB opener line.
///
/// A giant open fence opener has no same-bytes linear twin — every other open
/// block re-emits its growing `Block.html` per append (the O(n²/chunk) wire
/// contract), and closing the opener turns the rest of the bytes into fence
/// CONTENT of an open block, which is that same floor. Bounding each opener
/// instead keeps the per-byte work identical (same opener parse, same info
/// split, same escaping) while making the document provably O(n): each fence
/// commits after a fixed number of appends, so a cache that re-derived its
/// opener per append would cost this control a constant factor, not a growth
/// term. That is exactly what a control twin has to be — a yardstick for what n
/// bytes of this stream cost on this runner right now.
fn fence_closed_info_runs(target: usize) -> String {
    let mut s = String::new();
    while s.len() < target {
        let start = s.len();
        s.push_str("```rust ");
        while s.len() - start < 1024 {
            s.push_str("attr ");
        }
        s.push_str("\n```\n\n");
    }
    s
}

/// A blockquote whose FIRST line never completes. Once its content diverges from
/// every alert marker the container cache arms mid-line, so the slow-path tail
/// reparse is gone (scanned linear); its open paragraph still re-renders each
/// append (rendered quadratic — the ContainerCache commits per complete line).
fn quote_giant_line(target: usize) -> String {
    let mut s = String::from("> ");
    while s.len() < target {
        s.push_str("prose without any newline at all ");
    }
    s
}

/// Same mid-line arm, single giant CJK line (no spaces): same scanned-linear /
/// rendered-quadratic profile as `quote_giant_line`.
fn bq_cjk_one_line(target: usize) -> String {
    let mut s = String::from("> ");
    while s.len() < target {
        s.push_str("漢字の行が続く");
    }
    s
}

/// html-empty-partial-blank-close (FIXED): open raw-HTML block with 64-byte
/// lines so every chunk=128 append ends precisely at a line boundary (empty
/// trailing partial). An empty partial used to vacuously pass the type-6/7
/// blank-line close test, dropping the HtmlBlockCache (and refusing to re-arm)
/// on every such append -> O(n²); now it stays armed and streams linearly.
fn html_block_aligned(opener: &str, target: usize) -> String {
    let mut s = String::with_capacity(target + 64);
    let mut open = String::from(opener);
    while open.len() < 63 {
        open.push(' ');
    }
    open.push('\n');
    s.push_str(&open);
    let line = "abcdefghij klmnopqrst uvwxyz0123 456789ABCD EFGHIJKLMN OPQRSTUV\n"; // 64 bytes
    while s.len() < target {
        s.push_str(line);
    }
    s
}

/// BLOCK-level raw HTML under the sanitizer (`block_html`): a long open type-6
/// `<div>` whose body is markup, so every append walks real TAGS instead of a
/// per-byte escape map. The `HtmlBlockCache` runs its THIRD mode here — folding
/// at token boundaries with a carried open-element stack — because a per-LINE
/// fold is impossible once a tag may span lines. Without it every append
/// re-sanitizes the whole growing block: O(n²), and `rendered` sees it (every
/// byte entering `render::fold_block_html` is counted).
///
/// 64-byte body lines, so a 128-byte append boundary always lands on a line
/// start — the ALIGNED twin of the ragged shapes below.
fn block_html_aligned(target: usize) -> String {
    let mut s = String::from("<div class=\"wrap\">\n");
    let line = "<span class=\"c\">abcdefghij</span> plain tail text 012345678<br>\n";
    assert_eq!(line.len(), 64, "aligned body line must be 64 bytes");
    while s.len() < target {
        s.push_str(line);
    }
    s
}

/// RAGGED twin of [`block_html_aligned`]: an ODD 37-byte line, coprime with the
/// 128-byte append chunk (`gcd(128, 37) = 1`), so the boundary visits EVERY
/// offset within a line — parking the buffer mid-tag-name, mid-attribute-name,
/// inside a quoted attribute value, and between a tag's `<` and its first
/// letter, in turn. Each park leaves a still-STREAMING `<…` that the fold must
/// refuse to consume and must re-examine on the next append, which is exactly
/// where a token-boundary fold can silently degrade into re-sanitizing the whole
/// block (the gcd lesson from the ragged paragraph shapes). Must stay linear.
fn block_html_ragged(target: usize) -> String {
    let mut s = String::from("<div class=\"wrap\">\n");
    let line = "<i class=\"q\">abc</i> defgh ijklm<br>\n";
    assert_eq!(line.len(), 37, "ragged body line must be an odd 37 bytes");
    while s.len() < target {
        s.push_str(line);
    }
    s
}

/// Type-7 ragged twin (the opener is an arbitrary complete tag alone on its
/// line, not a known block-level one) — same fold, different arm-time gate.
/// 27-byte lines: odd, coprime with the 128-byte chunk.
fn block_html_type7_ragged(target: usize) -> String {
    let mut s = String::from("<mytag class=\"wrap\">\n");
    let line = "<b>abcd</b> efghijklmn opq\n";
    assert_eq!(line.len(), 27, "type-7 ragged body line must be an odd 27 bytes");
    while s.len() < target {
        s.push_str(line);
    }
    s
}

fn html_type6_aligned(target: usize) -> String {
    html_block_aligned("<div class=\"wrap\">", target)
}

fn html_type7_aligned(target: usize) -> String {
    html_block_aligned("<mytag class=\"wrap\">", target)
}

/// footnote-global-state (FIXED) — four member shapes, streamed with
/// `gfm_footnotes` ON. All used to cliff via document-global footnote state:
///   a1: a NO-blank run of single-line defs scans as ONE raw block, so the
///       pure-def-tail commit never advanced past it (ctr ratio 248 pre-fix);
///       fixed by committing the run up to its last def-opener line.
///   a2: blank-separated defs — the pre-existing >=2 def-block commit branch;
///       linear before, guarded here against regressing.
///   c:  ONE paragraph with thousands of distinct refs — the numbering
///       pre-pass re-collected the WHOLE cache region per append:
///       wall-quadratic (199x) but counter-BLIND pre-fix. The incremental
///       per-cache numbering (`RegionFnNums`) is itself counter-instrumented
///       now, so this entry really gates the class.
///   g:  flat list where every item carries a distinct ref — the list cache
///       used to line_bail on any `[^` (ctr ratio 160 pre-fix); now only
///       genuine def lines bail.
fn a1_def_run_noblank(target: usize) -> String {
    let mut s = String::from("Intro paragraph citing a note.[^f0]\n\n");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("[^f{i}]: footnote text number {i} with some words\n"));
        i += 1;
    }
    s
}

fn a2_def_run_blank(target: usize) -> String {
    let mut s = String::from("Intro paragraph citing a note.[^f0]\n\n");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("[^f{i}]: footnote text number {i} with some words\n\n"));
        i += 1;
    }
    s
}

fn c_many_refs_one_para(target: usize) -> String {
    let mut s = String::new();
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("word{i} [^c{i}] and"));
        s.push(if i % 8 == 7 { '\n' } else { ' ' });
        i += 1;
    }
    s
}

fn g_big_list_refs(target: usize) -> String {
    let mut s = String::new();
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("- item {i} cites a source[^g{i}] here\n"));
        i += 1;
    }
    s
}

/// open-list-item-body-rerender (FIXED): a quoted list item whose body is a
/// growing table — the open item never folds, and `fold_item_body` used to
/// re-render it whole every append (scan-counter-blind). The OpenItemStream
/// (nested parser) makes an armed item body stream in O(new bytes), and the
/// speculative fold is now counted, so both metrics pin this class:
/// ContainerBlockCache recursing into a nested ListCache recursing into an
/// OpenItemStream + TableCache.
fn quoted_list_table(target: usize) -> String {
    let mut s = String::from("> - intro\n>   | a | b |\n>   | --- | --- |\n");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!(">   | cell {i} | value {i} |\n"));
        i += 1;
    }
    s
}

/// ONE list item with an ever-growing plain-prose multi-line body — the
/// wall-only half of the open-item cliff.
fn list_item_plain_body(target: usize) -> String {
    let mut s = String::from("- intro line for the single item\n");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!(
            "  plain continuation prose line {i} with several ordinary words here\n"
        ));
        i += 1;
    }
    s
}

/// ONE list item whose body is an ever-growing table (the nested stream's own
/// TableCache makes the per-append work O(new bytes)).
fn list_growing_table(target: usize) -> String {
    let mut s = String::from("- intro\n  | a | b |\n  | --- | --- |\n");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("  | cell {i} | value {i} |\n"));
        i += 1;
    }
    s
}

/// ONE list item holding an ever-growing open fence. Pins the open-item
/// stream's kind-aware `open_tail` sensitivity: the fence opener's backticks
/// must NOT poison the settled-append fast path (code bodies never
/// inline-render, so they are exempt from the trigger-byte scan).
fn list_open_fence(target: usize) -> String {
    let mut s = String::from("- item with a growing fence\n  ```rust\n");
    while s.len() < target {
        s.push_str("  let x = compute(alpha, beta); // fence line\n");
    }
    s
}

/// ONE ever-growing directly-loose item (§5.3): blank-separated sub-paragraphs
/// inside a single item. The blank used to hard-bail the list cache.
fn loose_subs_one_item(target: usize) -> String {
    let mut s = String::from("- topic intro\n");
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("\n  sub paragraph {i} with a few words of detail\n"));
        i += 1;
    }
    s
}

/// Every item directly loose (§5.3): an interior blank between the item's two
/// paragraphs used to hard-bail the list cache — and the re-armed cache
/// re-bailed every append (counter ~247x).
fn staircase_blank_flap(target: usize) -> String {
    let mut s = String::with_capacity(target + 64);
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!(
            "- step {i} heading text\n\n  step {i} detail paragraph with words\n"
        ));
        i += 1;
    }
    s
}

/// table-partial-row-rerender (FIXED): the trailing table row never gets its
/// newline; the speculative partial-row render used to re-split + re-render
/// the whole growing partial every append (scan-counter-blind; `rendered` saw
/// it). The PartialRowCache freezes settled cells at each unescaped `|` and
/// commits the open cell's settled inline prefix, so only new bytes are
/// examined per append.
fn growing_last_cell(target: usize) -> String {
    let mut s = String::from("| a | b |\n| --- | --- |\n| x | y |\n| last | ");
    while s.len() < target {
        s.push_str("word word word ");
    }
    s.push_str("|\n");
    s
}

/// One header/alignment/data row with thousands of columns — the other
/// partial-row flavor (growth across cells rather than inside one).
fn wide_one_row(target: usize) -> String {
    let cols = (target / 21).max(4);
    let mut s = String::with_capacity(target + 64);
    for i in 0..cols {
        s.push_str("| h");
        s.push_str(&i.to_string());
        s.push(' ');
    }
    s.push_str("|\n");
    for _ in 0..cols {
        s.push_str("| --- ");
    }
    s.push_str("|\n");
    for i in 0..cols {
        s.push_str("| c");
        s.push_str(&i.to_string());
        s.push(' ');
    }
    s.push_str("|\n");
    s
}

/// resolve-delimiters-replace-range (FIXED): emphasis-pair-dense paragraph —
/// each resolved pair used to splice via `replace_range`, O(pairs × slice)
/// inside one inline render; edits now apply in a single forward-pass rebuild.
/// Counter-linear guard: the render-side win itself is wall-only.
fn strikethrough_one_para(target: usize) -> String {
    let mut s = String::new();
    let mut i = 0usize;
    while s.len() < target {
        s.push_str(&format!("w{i} ~~struck {i}~~ mid "));
        if i % 8 == 7 {
            s.push('\n');
        }
        i += 1;
    }
    s
}

/// delimiter-stack-mod3-rescan (FIXED — per-render via openers_bottom AND
/// streaming via the Mod3TailCache): a lone `**` opener permanently blocked by
/// the mod-3 rule used to make every later `*` closer re-walk the whole
/// delimiter stack (O(stack²) per render, so streaming went ~cubic in wall). The
/// bounded scan makes one render linear; the paragraph cut stays semantically
/// pinned (the unpaired can-open `**` is `earliest_open`) so no ParagraphCache
/// can arm — but while the `**` is the sole opener the paragraph is all-literal,
/// so the Mod3TailCache extends the escaped body by only the appended bytes and
/// streaming is linear on both metrics.
fn mod3_soup(target: usize) -> String {
    let mut s = String::from("a**b");
    while s.len() < target {
        s.push_str("c* ");
    }
    s
}

/// dollar-math-eof-rescan (FIXED — per-render via precomputed closer tables AND
/// streaming via the DollarTailCache): every `$` is a valid opener whose
/// candidate closers are all invalid, so each opener used to scan to EOF (O(n²)
/// inside one render). Memoized next-valid-closer lookup makes one render linear;
/// the paragraph cut stays semantically pinned (a future closer could pair back)
/// so no ParagraphCache can arm — but the whole open block is a single
/// speculative math span whose escaped body just grows, so the DollarTailCache
/// extends it in O(new bytes) and streaming is linear on both metrics.
fn dollar_soup(target: usize) -> String {
    let mut s = String::with_capacity(target + 8);
    while s.len() < target {
        s.push_str("$x ");
    }
    s
}

/// compute-cut-pair-overlap-scan (FIXED): thousands of resolved emphasis pairs
/// made `compute_cut`'s per-candidate pair-overlap scan O(candidates × pairs);
/// now a single ascending sweep with a running max. Counter-linear guard.
fn em_pairs_one_para(target: usize) -> String {
    repeat_to("*em* ", target)
}

/// commit-cut-pinned-no-boundary (PARTIALLY FIXED): a single space-free
/// paragraph of completed entities had zero boundary candidates, pinning the
/// commit cut at 0 (counter-visible ~250x). Synthetic boundary candidates
/// inside inert runs let the cut advance every ~2 KB.
fn entity_soup(target: usize) -> String {
    repeat_to("&amp;", target)
}

/// One giant space-free "word" with periodic non-autolinkable bytes (`;`).
/// Synthetic candidates may only sit on bytes an extended autolink can neither
/// contain nor start (a future `@` binds alnum runs backwards), so this is the
/// autolinks-on-safe flavor of the giant-word shape.
fn punctuated_giant_word(target: usize) -> String {
    repeat_to("abcdefghij;", target)
}

/// crlf-cache-bail (FIXED): CRLF line endings used to bail every incremental
/// cache, so ordinary CRLF streams full-rescanned each append; ingest
/// normalization (`\r\n`/lone `\r` -> `\n` before the buffer) makes them take
/// the exact same fast paths as LF. One twin per confirmed member family.
fn crlf_big_list(target: usize) -> String {
    big_list(target).replace('\n', "\r\n")
}

fn crlf_mixed(target: usize) -> String {
    mixed(target).replace('\n', "\r\n")
}

fn crlf_big_code(target: usize) -> String {
    big_code(target).replace('\n', "\r\n")
}

fn crlf_big_table(target: usize) -> String {
    big_table(target).replace('\n', "\r\n")
}

fn crlf_nested_loose_list(target: usize) -> String {
    nested_loose_list(target).replace('\n', "\r\n")
}

fn crlf_blockquote_with_list(target: usize) -> String {
    blockquote_with_list(target).replace('\n', "\r\n")
}

fn crlf_alert_with_list(target: usize) -> String {
    big_alert(target).replace('\n', "\r\n")
}

/// list-interior-blank-loose-bail (FIXED): indented code with a legal interior
/// blank every 20 lines used to permanently disarm the IndentedCodeCache
/// (bail + re-arm walk died on the same blank), so the never-committing
/// region re-scanned every append (counter ~248x). Blanks now fold as body
/// lines, at arm time too.
fn indented_code_blanks(target: usize) -> String {
    let mut s = String::with_capacity(target + 64);
    let mut i = 0usize;
    while s.len() < target {
        s.push_str("    let value = compute(alpha, beta); // indented code line\n");
        i += 1;
        if i % 20 == 0 {
            s.push('\n');
        }
    }
    s
}

// ---- the per-shape table ----------------------------------------------------

/// Default span for shapes cheap enough to run big: 16x. Linear work grows
/// ~16x across it; quadratic ~256x. Chunk 128 keeps appends frequent enough to
/// expose any O(n²/chunk) curve while finishing fast in CI.
const SMALL: usize = 16 * 1024;
const LARGE: usize = 256 * 1024;
const CHUNK: usize = 128;

/// Span for the known-quadratic shapes: 8x (8 KB → 64 KB unless noted), the
/// small-span pattern that keeps documented-O(n²) shapes affordable in CI.
const Q_SMALL: usize = 8 * 1024;
const Q_LARGE: usize = 64 * 1024;

/// Per-metric gate limits, span-relative:
/// - Linear: ratio ≤ span × 4 — 4x headroom absorbs cache re-arm constants and
///   small-N noise while staying 4x below the quadratic floor (span²). The
///   shipped cliffs were 100x–2900x, far past it. (span 16 → limit 64,
///   identical to the historical gate.)
/// - KnownQuadratic: ratio < span² × 2.5 — trips only on worse-than-quadratic
///   (e.g. an accidental cubic); the quadratic itself is the documented limit,
///   not a failure. (span 8 → limit 160, identical to the historical guard.)
fn linear_limit(span: f64) -> f64 {
    span * 4.0
}
fn quad_limit(span: f64) -> f64 {
    span * span * 2.5
}


/// Complete raw-HTML anchors in prose and table rows — the streaming shape a
/// backend emitting `<a href="…">label</a>` (instead of markdown links)
/// produces. Each tag completes quickly, so the open-tail suppression scan
/// settles in O(1) per tag and blocks commit normally: must stay linear.
fn raw_html_anchors(target: usize) -> String {
    let mut s = String::new();
    let mut i = 0;
    while s.len() < target {
        s.push_str("Result reported <a href=\"https://platform.example.com/company/");
        s.push_str(&i.to_string());
        s.push_str("/transcript\">source</a> today.\n\n| co | src |\n| -- | --- |\n| acme | <a href=\"https://data.example.com/row/");
        s.push_str(&i.to_string());
        s.push_str("\">row</a> |\n\n");
        i += 1;
    }
    s
}

/// ONE never-closing raw tag with an unboundedly growing quoted attribute —
/// the raw-HTML twin of the `[link](growing-url…` tail pin. The failed-`<`
/// unstable mark (which PRE-DATES the open-tail tag suppression) pins the
/// paragraph cut at the `<`; while the tag streams to EOF inside its unclosed
/// quoted value the render is a constant `<p></p>`, so the RawTagTailCache
/// extends it in O(1) (see the shape entry below).
fn raw_tag_growing_attr(target: usize) -> String {
    let mut s = String::from("<a href=\"https://example.com/");
    while s.len() < target {
        s.push_str("aaaaaaaa");
    }
    s
}

fn shapes() -> Vec<Shape> {
    let base = Opts::default();
    let lin = |name: &'static str, gen: fn(usize) -> String, rendered: Expect| Shape {
        name,
        gen,
        opts: base,
        chunk: CHUNK,
        small: SMALL,
        large: LARGE,
        scanned: Expect::Linear,
        rendered,
    };
    // Sanitizer + block-level raw HTML (the third HtmlBlockCache mode). Both
    // metrics must stay linear: the fold consumes each token exactly once.
    let block_html_shape = |name: &'static str, gen: fn(usize) -> String| Shape {
        name,
        gen,
        opts: Opts { html_sanitize: true, block_html: true, ..Opts::default() },
        chunk: CHUNK,
        small: SMALL,
        large: LARGE,
        scanned: Expect::Linear,
        rendered: Expect::Linear,
    };
    // A known-quadratic fix-campaign entry, named by hunt group key. `scanned`/
    // `rendered` reflect which metric actually sees the cliff (measured, not
    // assumed): a metric that stays linear on a wall-only cliff is declared
    // Linear so the gate at least pins the visible half.
    let quad = |name: &'static str,
                gen: fn(usize) -> String,
                opts: Opts,
                scanned: Expect,
                rendered: Expect| Shape {
        name,
        gen,
        opts,
        chunk: CHUNK,
        small: Q_SMALL,
        large: Q_LARGE,
        scanned,
        rendered,
    };
    use Expect::{KnownQuadratic, Linear};
    let footnotes = Opts { footnotes: true, ..base };
    let block_data = Opts { block_data: true, ..base };
    let chart_tag = Opts { component_tags: &["Chart"], ..base };
    // A `block_data = true` twin of a cached linear shape (same span/chunk).
    let bd = |name: &'static str, gen: fn(usize) -> String| Shape {
        name,
        gen,
        opts: block_data,
        chunk: CHUNK,
        small: SMALL,
        large: LARGE,
        scanned: Expect::Linear,
        rendered: Expect::Linear,
    };
    let mut v = vec![
        // -- shapes that MUST stay linear (commit regularly or have a cache) --
        lin("mixed", mixed, Linear),
        lin("many_paragraphs", many_paragraphs, Linear),
        // Word-wrapped prose with INDENTED continuation lines -> ParagraphCache.
        // Was O(n²) (see `indented_wrapped_para`): `is_boundary` proposed no
        // commit candidate anywhere in the paragraph, so the cut pinned at 0 and
        // every append re-rendered it whole. Both gated counters see it, so this
        // is the deterministic witness; `indented_continuation_para_is_wall_linear`
        // is the wall-time second one.
        lin("indented_wrapped_para", indented_wrapped_para, Linear),
        // Same prose on a line length that does NOT divide the append size, so
        // boundaries regularly park the buffer on a TRANSIENT paragraph end (a
        // whitespace-only unterminated last line). Was O(n²) on its own: the
        // incremental path dropped its cache there and paid a full re-scan +
        // re-render, one append in seven. `trailing_open_ws_line` suspends instead.
        lin("indented_wrapped_para_ragged", indented_wrapped_para_ragged, Linear),
        // The same transient-end shape one container deep. An open list item
        // hosts a nested StreamParser, so its paragraph inherits the suspension
        // through the very same `try_incremental_paragraph` — this pins that
        // inheritance (212x scanned / 209x rendered without it).
        lin("indented_list_item_ragged", indented_list_item_ragged, Linear),
        // Proven-safe pin: `ContainerCache` never consults `ParaEnd`, so a
        // quoted indented-continuation paragraph has no transient verdict to
        // drop on. Flat with AND without the suspension — registered so a future
        // refactor that routes containers through the paragraph path can't
        // silently inherit the cliff.
        lin("indented_blockquote_ragged", indented_blockquote_ragged, Linear),
        lin("big_list", big_list, Linear), // flat list -> ListCache (incremental)
        lin("big_blockquote", big_blockquote, Linear), // prose quote -> ContainerCache
        lin("quote_many_paras", quote_many_paras, Linear), // multi-para quote -> ContainerCache
        // Structured-inner containers -> ContainerBlockCache (recursive nested
        // parser, incremental). Was O(n²) (the 0.18.4 flicker fix bailed to a
        // full reparse every append); now streams linearly.
        lin("alert_with_list", big_alert, Linear),
        lin("blockquote_with_list", blockquote_with_list, Linear),
        // Nested loose list -> ListCache (multi-line item bodies, incremental).
        // Was O(n²) (the 0.18.3 flicker fix bailed the list cache to a full
        // reparse on every nested sub-bullet); now streams linearly.
        lin("nested_loose_list", nested_loose_list, Linear),
        lin("big_table", big_table, Linear),
        lin("big_code", big_code, Linear),
        lin("indented_big_code", indented_big_code, Linear),
        lin("big_math", big_math, Linear),
        // Open HTML block (types 6/7) with newline-aligned appends ->
        // HtmlBlockCache (incremental). Was O(n²) (hunt group
        // html-empty-partial-blank-close): a zero-byte trailing partial
        // vacuously passed the blank-line close test, so the cache dropped and
        // never re-armed whenever an append ended exactly at `\n`.
        lin("html_type6_aligned", html_type6_aligned, Linear),
        // Raw anchors under unsafe_html: engages the open-tail incomplete-tag
        // suppression scan on every tail — must stay linear (albany shape).
        Shape {
            name: "raw_html_anchors",
            gen: raw_html_anchors,
            opts: Opts { unsafe_html: true, ..Opts::default() },
            chunk: CHUNK,
            small: SMALL,
            large: LARGE,
            scanned: Expect::Linear,
            rendered: Expect::Linear,
        },
        lin("html_type7_aligned", html_type7_aligned, Linear),
        // BLOCK-level raw HTML through the sanitizer (`block_html`): the
        // HtmlBlockCache's token-boundary fold. `rendered` counts every byte
        // entering `fold_block_html`, so a cache that re-sanitized the whole
        // growing block per append shows up here as O(n²) — measured 96x/94x
        // (scanned/rendered) over the 16x span before the fold existed.
        block_html_shape("block_html_aligned", block_html_aligned),
        block_html_shape("block_html_ragged", block_html_ragged),
        block_html_shape("block_html_type7_ragged", block_html_type7_ragged),
        // CRLF twins (hunt group crlf-cache-bail, FIXED via ingest
        // normalization) — must cost the same as their LF originals.
        lin("crlf_big_list", crlf_big_list, Linear),
        lin("crlf_mixed", crlf_mixed, Linear),
        lin("crlf_big_code", crlf_big_code, Linear),
        lin("crlf_big_table", crlf_big_table, Linear),
        lin("crlf_nested_loose_list", crlf_nested_loose_list, Linear),
        lin("crlf_blockquote_with_list", crlf_blockquote_with_list, Linear),
        lin("crlf_alert_with_list", crlf_alert_with_list, Linear),
        // Inline-engine wave (FIXED): emphasis-edit forward rebuild,
        // openers_bottom, compute_cut sweep, synthetic inert-run boundaries.
        lin("strikethrough_para", strikethrough_one_para, Linear),
        lin("em_pairs_para", em_pairs_one_para, Linear),
        lin("entity_soup", entity_soup, Linear),
        lin("punctuated_giant_word", punctuated_giant_word, Linear),
        // Giant word with autolinks OFF is linear post-fix; with autolinks ON
        // the pin is semantic — see the known-quadratic entry below.
        Shape {
            name: "giant_word_no_autolinks",
            gen: one_giant_word,
            opts: Opts { no_autolinks: true, ..base },
            chunk: CHUNK,
            small: SMALL,
            large: LARGE,
            scanned: Linear,
            rendered: Linear,
        },
        // Table partial-row shapes (hunt group table-partial-row-rerender,
        // FIXED via the PartialRowCache) — the partial-row path self-counts
        // into `scanned`, so both metrics now pin this class.
        lin("growing_last_cell", growing_last_cell, Linear),
        lin("wide_one_row", wide_one_row, Linear),
        // Open list ITEM with an ever-growing body -> ListCache + OpenItemStream
        // (nested parser, incremental; hunt group open-list-item-body-rerender,
        // FIXED). The speculative fold is now counted (perf::add_scan).
        lin("list_item_plain_body", list_item_plain_body, Linear),
        lin("list_growing_table", list_growing_table, Linear),
        lin("list_open_fence", list_open_fence, Linear),
        lin("quoted_list_table", quoted_list_table, Linear),
        // Legal interior blank lines no longer disarm the caches (hunt group
        // list-interior-blank-loose-bail, FIXED): a list item's interior blank
        // stays in-item (§5.3 looseness via item_directly_loose through the
        // one-time rebuild_loose), and indented-code blanks fold as body lines.
        lin("loose_subs_one_item", loose_subs_one_item, Linear),
        lin("staircase_blank_flap", staircase_blank_flap, Linear),
        lin("indented_code_with_interior_blanks", indented_code_blanks, Linear),
        // Container defs + laziness (hunt groups global-defs-inside-container,
        // FIXED, and the lazy half of container-stack-churn-lazy, FIXED): the
        // nested parser consumes quote-hosted def runs natively, and lazy
        // marker-less continuation lines glue exactly like blockquote_inner.
        lin("quote_ref_defs", quote_ref_defs, Linear),
        lin("quote_footnote_defs", quote_footnote_defs, Linear),
        lin("bq_lazy_continuation", bq_lazy_continuation, Linear),
        // `block_data` twins of the cached shapes (hunt groups
        // blockdata-disables-container-cache + blockdata-per-append-rebuild,
        // FIXED): the structured `kind.data` channel must never disarm an
        // incremental cache or change its scan profile. The container-block
        // cache used to bail outright under `block_data` (247x counter on an
        // alert/quote with a structured body — it now owns the nested
        // `ContainerData` channel); the armed caches used to rebuild their full
        // data payload per append (fence/indented whole-body entity decode,
        // deep-cloned list items / container paras / table headers) — counter-
        // linear but a 3–87x wall multiplier, fixed by raw-slice derivation +
        // `Rc`-shared committed entries. The wall half can't gate
        // deterministically; these twins pin the arm/disarm + scan profile.
        bd("blockdata_alert", big_alert),
        bd("blockdata_blockquote", blockquote_with_list),
        bd("blockdata_quote_paras", quote_many_paras),
        bd("blockdata_big_list", big_list),
        bd("blockdata_nested_list", nested_loose_list),
        bd("blockdata_big_table", big_table),
        bd("blockdata_big_code", big_code),
        bd("blockdata_indented_big_code", indented_big_code),
        bd("blockdata_big_math", big_math),
        // Footnote shapes (hunt group footnote-global-state, FIXED): def-run
        // tails commit up to the last def opener, the per-cache footnote
        // numbering extends over only NEW bytes (`RegionFnNums`, self-counted
        // into `scanned`), the committed footnote tables are Rc-shared (no
        // per-append map clones), and the list cache streams footnote REFS
        // (only genuine def lines bail).
        Shape {
            name: "fn_a1_def_run_noblank",
            gen: a1_def_run_noblank,
            opts: footnotes,
            chunk: CHUNK,
            small: SMALL,
            large: LARGE,
            scanned: Linear,
            rendered: Linear,
        },
        Shape {
            name: "fn_a2_def_run_blank",
            gen: a2_def_run_blank,
            opts: footnotes,
            chunk: CHUNK,
            small: SMALL,
            large: LARGE,
            scanned: Linear,
            rendered: Linear,
        },
        Shape {
            name: "fn_c_many_refs_one_para",
            gen: c_many_refs_one_para,
            opts: footnotes,
            chunk: CHUNK,
            small: SMALL,
            large: LARGE,
            scanned: Linear,
            rendered: Linear,
        },
        Shape {
            name: "fn_g_big_list_refs",
            gen: g_big_list_refs,
            opts: footnotes,
            chunk: CHUNK,
            small: SMALL,
            large: LARGE,
            scanned: Linear,
            rendered: Linear,
        },
        // -- the 17 verified O(n²) hunt groups (fix campaign; flip to Linear
        //    as each lands) ---------------------------------------------------
        quad("open-block-html-reemit", unclosed_fence, base, Linear, Linear), // wall-only (memcpy); emitted shows it
        // commit-cut-pinned-no-boundary: FIXED on BOTH metrics via the
        // AlnumTailCache. With extended autolinks ON the `aaaa…` run has no
        // boundary candidate (a future `@`/`.` could bind it right-to-left into an
        // autolink), so the cut pins at 0 and no ParagraphCache can arm. But a
        // pure-ASCII-alnum run can neither open a construct nor complete an
        // autolink (which needs `http://`/`www.`/`@` punctuation the run lacks),
        // and escape_html leaves alnum unchanged — so the render is fixed at `<p>`
        // + escape_html(body) + `</p>` and the cache extends the escaped body by
        // only the appended bytes (O(new)). The guard drops to the byte-identical
        // full path the instant a non-alnum byte settles the run. Inert-run
        // flavors (entity_soup, punctuated_giant_word, autolinks-off) are linear
        // above by the synthetic-boundary path.
        quad("giant-word-autolinks-pin", one_giant_word, base, Linear, Linear),
        // raw-tag-tail-pin: FIXED on BOTH metrics via the RawTagTailCache. The
        // failed-`<` unstable mark (pre-dating the open-tail tag suppression) pins
        // the cut at the `<`, so no ParagraphCache can arm. But while the tag
        // streams to EOF inside its unclosed quoted attr value, the 0.20.3
        // suppression emits nothing — the paragraph render is the CONSTANT
        // `<p></p>` — so the cache extension is O(1): it only checks the appended
        // bytes for the value-closing quote or a newline. The guard drops to the
        // byte-identical full path the instant the quote closes (the tag can
        // complete or gain attrs) or a newline splits the line. Engaged only under
        // sanitize/unsafe HTML (in escape mode the `<` is visible `&lt;…` text, a
        // separate out-of-scope pin).
        quad(
            "raw-tag-tail-pin",
            raw_tag_growing_attr,
            Opts { unsafe_html: true, ..Opts::default() },
            Linear,
            Linear,
        ),
        // uncached-open-block-kinds: FIXED — the five member shapes are pinned
        // linear here; the two first-line-incomplete container flavors remain
        // known-quadratic below.
        Shape {
            name: "component_block_open",
            gen: component_block_open,
            opts: chart_tag,
            chunk: CHUNK,
            small: SMALL,
            large: LARGE,
            scanned: Linear,
            rendered: Linear,
        },
        // component-open-blank-line: FIXED — the blank-ending appends are served
        // from the cache's SETTLED nested twin instead of dropping the cache, so
        // the body streams in O(new bytes) whether or not it has blank lines.
        Shape {
            name: "component_multi_para",
            gen: component_multi_para,
            opts: chart_tag,
            chunk: CHUNK,
            small: SMALL,
            large: LARGE,
            scanned: Linear,
            rendered: Linear,
        },
        Shape {
            name: "component_multi_para_rich",
            gen: component_multi_para_rich,
            opts: chart_tag,
            chunk: CHUNK,
            small: SMALL,
            large: LARGE,
            scanned: Linear,
            rendered: Linear,
        },
        lin("heading_words", heading_words, Linear),
        lin("heading_emphasis", heading_emphasis, Linear),
        lin("growing_rule", growing_rule, Linear),
        lin("fence_giant_info", fence_giant_info, Linear),
        // html-empty-partial-blank-close: FIXED — promoted to the linear
        // html_type6/7_aligned shapes above.
        // footnote-global-state: FIXED — promoted to the four fn_* linear
        // shapes below (def-run commit cut + incremental per-cache footnote
        // numbering + list-cache `[^` bail narrowed to genuine def lines).
        // rendered measured sub-linear (defs emit no HTML) — gate it Linear.
        // global-defs-inside-container: FIXED — quote_ref_defs,
        // quote_footnote_defs and bq_lazy_continuation promoted to linear
        // shapes below (footnotes-ON quote-hosted defs remain a follow-up).
        // open-list-item-body-rerender: FIXED — quoted_list_table and the
        // list_* shapes promoted to linear below.
        // table-partial-row-rerender: FIXED — growing_last_cell + wide_one_row
        // promoted to linear shapes below.
        // resolve-delimiters-replace-range: FIXED — strikethrough_para promoted
        // to a linear shape above (the render-side win is wall-only).
        // compute-cut-pair-overlap-scan: FIXED — em_pairs_para promoted above.
        // crlf-cache-bail: FIXED — promoted to the seven crlf_* linear twins above.
        // blockdata-per-append-rebuild: FIXED — the armed caches derive the data
        // channel from the raw source / Rc-shared committed entries; promoted to
        // the blockdata_* linear twins above.
        // container-first-line-pin(+cjk): SCANNED fixed — both container caches
        // now arm MID-LINE once the partial first line can no longer become an
        // alert marker (`first_line_alert_undecided`), so the O(n²) slow-path tail
        // reparse (block scan + classify + render_block of the growing quote every
        // append) is gone. RENDERED stays quadratic: the plain ContainerCache
        // commits inner content only at COMPLETE-line boundaries — the trailing
        // partial line is rendered speculatively then truncated back each append
        // (so a later lazy continuation / blank close can still reinterpret it),
        // so a first line that NEVER completes keeps its whole open paragraph in
        // the re-rendered region. Wall-only inline re-render (same shape as the
        // other open-tail pins); the costly block-level rescan is what arming cut.
        quad("container-first-line-pin", quote_giant_line, base, Linear, KnownQuadratic),
        quad("container-first-line-pin-cjk", bq_cjk_one_line, base, Linear, KnownQuadratic),
        // list-interior-blank-loose-bail: FIXED — indented_code_with_interior_
        // blanks + loose_subs_one_item + staircase_blank_flap promoted above.
    ];
    // Shapes too expensive per byte for the 8 KB → 64 KB span (super-quadratic
    // wall); same 8x span at smaller sizes.
    // delimiter-stack-mod3-rescan: FIXED on BOTH metrics via the Mod3TailCache.
    // The `a**bc* c* …` soup is one open paragraph whose lone `**` (can-open AND
    // can-close, so the mod-3 rule is live) every later single `*` is blocked
    // from closing (`2 + 1 ≡ 0 (mod 3)`, neither length a multiple of 3). The
    // `**` stays an unpaired can-open run, so `compute_cut`'s `earliest_open`
    // pins the cut at 0 and no ParagraphCache can arm. But while the `**` is the
    // sole opener no `*` in the body can pair, so the paragraph renders as
    // literal escaped text — `<p>` + escape_html(body) + `</p>` — and escape_html
    // is a context-free per-byte map, so the Mod3TailCache extends the settled
    // body by only the appended bytes (O(new)); the slow-path tail rescan
    // (scanned) and whole-paragraph re-render (rendered) are both gone. The guard
    // drops to the byte-identical full path the moment a byte could restructure
    // the render — a `*` run of length ≥ 2 (a `**` closer pairs the leading one,
    // `2 + 2 ≢ 0` → `<strong>`), a single `*` a non-space follows (it could
    // open), a newline, or any construct/entity/non-ASCII byte; a `*` on the
    // chunk edge is held pending so the 3-byte-period soup never forces a drop.
    // Small span retained from its former super-quadratic days; it now sits
    // comfortably linear (~7x over the 8x span).
    v.push(Shape {
        name: "delimiter-stack-mod3-rescan",
        gen: mod3_soup,
        opts: base,
        chunk: CHUNK,
        small: 1024,
        large: 8 * 1024,
        scanned: Linear,
        rendered: Linear,
    });
    // dollar-math-eof-rescan: FIXED on BOTH metrics. The `$x $x …` soup is one
    // open, unclosed single-`$` inline-math span from the paragraph start to EOF
    // (every inner `$` is space-preceded, so none is a valid closer; the opener
    // could still pair forward, so the commit cut is genuinely pinned at 0 and no
    // ParagraphCache can arm). While the span stays open its render is fixed —
    // `<span class="math math-inline">` + escape_html(body) + `</span>` — and
    // escape_html is a context-free per-byte map, so the DollarTailCache extends
    // the escaped body by only the appended bytes (O(new)); the slow-path tail
    // rescan (scanned) and whole-paragraph re-render (rendered) are both gone.
    // The guard drops to the byte-identical full path the moment a newline, a
    // valid `$` closer, or a `$$` run appears (real math closes once per formula
    // → amortized linear). Small span retained from its former super-quadratic
    // days; it now sits comfortably linear (~7x over the 8x span).
    v.push(Shape {
        name: "dollar-math-eof-rescan",
        gen: dollar_soup,
        opts: base,
        chunk: CHUNK,
        small: 1024,
        large: 8 * 1024,
        scanned: Linear,
        rendered: Linear,
    });
    // blockdata-disables-container-cache: FIXED — the ContainerBlockCache owns
    // `block_data` now (nested `ContainerData` per committed inner block);
    // promoted to the blockdata_alert / blockdata_blockquote linear twins above.
    // container-depth-growth-pin: FIXED on BOTH metrics via the iterative
    // DeepQuoteCache. The recursive ContainerBlockCache spent one nested parser per
    // level (capped at MAX_CONTAINER_DEPTH so the recursive `append` call chain
    // can't overflow the WASM shadow stack); past the cap the innermost parser
    // re-scanned + re-rendered its whole growing nested-quote tail every append
    // (worse than n²). The DeepQuoteCache folds each settled shallower level's
    // `<blockquote>`+paragraph opener EXACTLY once and keeps ONE open parser for the
    // deepest line, extending in O(new bytes) with a heap `String` stack (never
    // nested parsers → no shadow-stack cost). Byte-identical to the recursive path:
    // it is armed only on the pure single-step-deepening prose staircase (top level,
    // block_data + footnotes off) and BAILS to that path — the unchanged baseline,
    // incl. its depth-(MAX_CONTAINER_DEPTH+100) render truncation — on any deviation
    // (a line not exactly one deeper, a non-prose/alert content byte, a lazy/blank/
    // shallower line, an as-yet-content-less deeper marker, or that depth bound).
    v.push(Shape {
        name: "container-depth-growth-pin",
        gen: quote_depth_growing,
        opts: base,
        chunk: CHUNK,
        small: 2 * 1024,
        large: 16 * 1024,
        scanned: Linear,
        rendered: Linear,
    });
    v
}

// ---- the gate ---------------------------------------------------------------

fn check(
    shape: &str,
    metric: &str,
    ratio: f64,
    span: f64,
    expect: Expect,
    failures: &mut Vec<String>,
) {
    match expect {
        Expect::Linear => {
            if ratio > linear_limit(span) {
                failures.push(format!(
                    "{shape}: {metric} work grew {ratio:.1}x across a {span}x size span \
(limit {:.0}x) — superlinear regression",
                    linear_limit(span)
                ));
            }
        }
        Expect::KnownQuadratic => {
            if ratio > quad_limit(span) {
                failures.push(format!(
                    "{shape}: {metric} work grew {ratio:.1}x across a {span}x size span — \
WORSE than the documented quadratic limit ({:.0}x)",
                    quad_limit(span)
                ));
            }
        }
        Expect::Untracked => {}
    }
}

fn tag(e: Expect) -> &'static str {
    match e {
        Expect::Linear => "lin",
        Expect::KnownQuadratic => "n²",
        Expect::Untracked => "-",
    }
}

#[test]
fn streaming_complexity_gate() {
    let mut failures = Vec::new();
    println!(
        "{:36} {:>5}  {:>12} {:>12} {:>12}  {:>9} {:>9}",
        "shape (span)", "", "scanned", "rendered", "emitted", "wall-S ms", "wall-L ms"
    );
    for s in shapes() {
        let span = (s.large / s.small) as f64;
        let w_small = measure(&(s.gen)(s.small), s.chunk, s.opts);
        let w_large = measure(&(s.gen)(s.large), s.chunk, s.opts);
        let r_scan = w_large.scanned as f64 / w_small.scanned as f64;
        let r_rend = w_large.rendered as f64 / w_small.rendered as f64;
        let r_emit = w_large.emitted as f64 / w_small.emitted as f64;
        println!(
            "{:36} (x{:>2})  {:>7.1} [{}] {:>7.1} [{}] {:>9.1} [i]  {:>9.1} {:>9.1}",
            s.name,
            span,
            r_scan,
            tag(s.scanned),
            r_rend,
            tag(s.rendered),
            r_emit,
            w_small.wall_ms,
            w_large.wall_ms,
        );
        check(s.name, "scanned", r_scan, span, s.scanned, &mut failures);
        check(s.name, "rendered", r_rend, span, s.rendered, &mut failures);
        // `emitted` is informational only — full-active-block re-emission per
        // append is the current wire contract (see perf module docs).
    }
    assert!(failures.is_empty(), "complexity regression(s):\n  {}", failures.join("\n  "));
}

/// Pins the exact 0.18.2 regression: a paragraph immediately followed by a long
/// run of link-reference definitions used to stall `committed_offset`, so the
/// whole growing def run re-scanned every append (235 KB @ chunk 256 = 59 s).
/// The fix made it linear; this guards it deterministically.
/// Wire delta mode retires the emitted-bytes re-emit floor: for shapes whose
/// active-block html grows prefix-stably (fences, lists, tables, quotes —
/// the realistic long-block streams), `emitted` must now scale LINEARLY.
/// This is the gate for `WIRE.md` §11's whole reason to exist; before delta
/// mode these shapes' emitted ratio tracked span² (the documented floor).
/// Shapes whose rendered html legitimately rewrites its prefix mid-stream
/// (late delimiter resolution) are excluded — for them full re-emits are the
/// verified-correct behavior, not a regression.
#[test]
fn wire_delta_emitted_is_linear() {
    let shapes: [(&str, fn(usize) -> String); 7] = [
        ("unclosed_fence", unclosed_fence),
        ("big_code", big_code),
        ("big_list", big_list),
        ("big_table", big_table),
        ("big_blockquote", big_blockquote),
        ("many_paragraphs", many_paragraphs),
        ("mixed", mixed),
    ];
    let opts = Opts { wire_delta: true, ..Opts::default() };
    let mut failures = Vec::new();
    for (name, gen) in shapes {
        let w_small = measure(&gen(SMALL), CHUNK, opts);
        let w_large = measure(&gen(LARGE), CHUNK, opts);
        let span = (LARGE / SMALL) as f64;
        let ratio = w_large.emitted as f64 / w_small.emitted as f64;
        println!("wire-delta emitted  {name:20} ratio {ratio:>7.1} (span x{span})");
        if ratio > linear_limit(span) {
            failures.push(format!(
                "{name}: emitted {ratio:.1}x for {span}x input (limit {:.0}x)",
                linear_limit(span)
            ));
        }
    }
    assert!(failures.is_empty(), "wire-delta emitted regression(s):\n  {}", failures.join("\n  "));
}

// ---- wall-clock guard harness ---------------------------------------------

/// The ratios one full wall-guard measurement pass produces.
///
/// Wall guards exist because the work counters are blind to allocation-class
/// quadratics: an armed cache that re-derives a growing region every append
/// scans nothing extra and inline-renders nothing extra — it just allocates and
/// memcpys — so `scanned`/`rendered` stay flat and green while wall goes O(n²).
/// Two shipped regressions were caught only this way. They stay.
///
/// But they are also the only gate here that can flake: wall time on a shared
/// runner carries the runner's weather, and raw growth-over-a-span carries it
/// UNDAMPED (a 29.1x reading on a shape whose quiet value is 18.5x once turned a
/// commit with zero Rust changes red, while the deterministic counter gate for
/// the same shape passed in the same job).
///
/// So every wall guard also measures a CONTROL twin — same sizes, same span,
/// back to back on the same runner, on a shape that does equivalent LINEAR work
/// — and gates on a ratio the weather cancels out of:
///
/// * `vs_control` — worst per-size `shape / control`. The sharpest form, and the
///   one to use whenever the twin is the SAME document minus the single property
///   under test (the flush-prose twins, the escaped-HTML twin): ~1x when fixed,
///   30-300x when broken, and a runner hiccup lands on both twins.
/// * `growth_vs_control` — the shape's growth across the span divided by the
///   control's growth across the same span. For a twin that is necessarily a
///   DIFFERENT document (the fence opener has no same-bytes linear twin), the
///   per-size ratio carries an arbitrary per-byte constant; dividing the growths
///   cancels it. ~1x when fixed, ~(observed broken growth / span) when broken.
/// * `growth` — raw shape growth across the span. Noise-fragile by construction,
///   so it is kept only as a SECONDARY catch for a catastrophic regression, with
///   a limit far from both the quiet value and the runner's weather.
///
/// A limit of [`f64::INFINITY`] leaves a field printed but not gated.
#[derive(Clone, Copy)]
struct WallRatios {
    vs_control: f64,
    growth_vs_control: f64,
    growth: f64,
}

impl WallRatios {
    fn within(&self, limits: &WallRatios) -> bool {
        self.vs_control < limits.vs_control
            && self.growth_vs_control < limits.growth_vs_control
            && self.growth < limits.growth
    }

    /// Elementwise better (smaller) of two passes — see [`WallGuard::measure`].
    fn better(&self, other: &WallRatios) -> WallRatios {
        WallRatios {
            vs_control: self.vs_control.min(other.vs_control),
            growth_vs_control: self.growth_vs_control.min(other.growth_vs_control),
            growth: self.growth.min(other.growth),
        }
    }
}

/// One wall-clock guard: a SHAPE and its CONTROL twin, measured at the same
/// sizes and interleaved size by size so a slow stretch of runner hits both.
struct WallGuard {
    label: &'static str,
    /// Column header for the control's millisecond column (e.g. `"flush"`).
    control_label: &'static str,
    sizes: &'static [usize],
    /// Samples per size; the best (least-noisy, never the worst) is kept.
    runs: usize,
    shape: fn(usize) -> String,
    control: fn(usize) -> String,
    opts: Opts,
    control_opts: Opts,
    limits: WallRatios,
}

impl WallGuard {
    /// One full measurement pass: best-of-`runs` wall time for the shape and for
    /// its control at every size, printed per size, reduced to [`WallRatios`].
    fn pass(&self) -> WallRatios {
        let best = |md: &str, o: Opts| {
            (0..self.runs).map(|_| measure(md, CHUNK, o).wall_ms).fold(f64::INFINITY, f64::min)
        };
        let mut walls = Vec::with_capacity(self.sizes.len());
        let mut controls = Vec::with_capacity(self.sizes.len());
        let mut worst_vs_control = 0.0f64;
        for &n in self.sizes {
            let wall = best(&(self.shape)(n), self.opts);
            let control = best(&(self.control)(n), self.control_opts);
            let vs = wall / control;
            worst_vs_control = worst_vs_control.max(vs);
            let each = walls.last().map(|prev| wall / *prev as f64).unwrap_or(0.0);
            println!(
                "{} {:>4} KB  wall {:>8.2} ms  (x{each:.1} vs previous size)  {} {:>8.2} ms  \
                 (x{vs:.1} vs control)",
                self.label,
                n / 1024,
                wall,
                self.control_label,
                control
            );
            walls.push(wall);
            controls.push(control);
        }
        let growth = walls[walls.len() - 1] / walls[0];
        let control_growth = controls[controls.len() - 1] / controls[0];
        WallRatios {
            vs_control: worst_vs_control,
            growth_vs_control: growth / control_growth,
            growth,
        }
    }

    /// Retry-once damping: measure, and if any GATED ratio would fail, re-measure
    /// the whole set once (fresh best-of-`runs`) and keep the better value of
    /// each. One retry only — a real quadratic reproduces on both passes, a
    /// scheduler hiccup does not. The retry costs nothing on a green run.
    fn measure(&self) -> WallRatios {
        let first = self.pass();
        if first.within(&self.limits) {
            return first;
        }
        println!(
            "{}: pass 1 breaches a limit (vs-control {:.1}x/{:.1}, growth-vs-control {:.1}x/{:.1}, \
             growth {:.1}x/{:.1}) — RE-MEASURING the full set once and asserting on the better \
             run; runner weather does not reproduce, a quadratic does",
            self.label,
            first.vs_control,
            self.limits.vs_control,
            first.growth_vs_control,
            self.limits.growth_vs_control,
            first.growth,
            self.limits.growth,
        );
        let second = self.pass();
        first.better(&second)
    }
}

/// The growing-OPENER-LINE shape, gated on WALL time — deliberately, because no
/// counter here can see its failure mode.
///
/// `fence_giant_info` streams a code fence whose opener line (`` ```rust `` + a
/// giant info tail) never gets its newline, so `FenceInfoCache` stays armed for
/// thousands of appends. That cache's contract is that everything it re-emits is
/// FROZEN — O(new bytes) per append. The way to break it is to re-derive some
/// growing part of the opener line (e.g. the info string's `meta`) on every
/// append: pure allocation + memcpy, entirely INSIDE the cache. `scanned` and
/// `rendered` count units of parser work the cache legitimately does not do, so
/// both stay flat and green while wall time goes quadratic; `emitted` is
/// informational and also flat (the frozen HTML is small). Wall time is the only
/// signal that moves, so this asserts on it.
///
/// Crude by construction: wall is noisy. Damped four ways (see [`WallRatios`]):
/// best of 5 runs per size, an 8x span (64 KB → 512 KB opener line) where linear
/// ≈ 8x and quadratic ≈ 64x, a retry-once re-measure, and above all the
/// [`fence_closed_info_runs`] control twin.
///
/// The PRIMARY gate is `growth_vs_control` — this shape's growth over the span
/// divided by the linear control's growth over the same span, which is ~1.0 when
/// the cache stays frozen and ~6x when it does not (the pinned regression grew
/// 50.5x-64x where the control grows ~8x). Limit 3x: 2.5x above the quiet value,
/// 2x below the broken one, and a runner that slows one twin down slows the
/// other with it. The raw growth stays as a SECONDARY catch at 48x — halfway to
/// the quadratic floor, so it only fires on a catastrophe and never on weather
/// (it read 8.9x quiet).
#[test]
fn fence_opener_line_growth_is_wall_linear() {
    const SIZES: [usize; 4] = [64 * 1024, 128 * 1024, 256 * 1024, 512 * 1024];
    let limits = WallRatios { vs_control: f64::INFINITY, growth_vs_control: 3.0, growth: 48.0 };
    let r = WallGuard {
        label: "fence_giant_info opener",
        control_label: "closed fences",
        sizes: &SIZES,
        runs: 5,
        shape: fence_giant_info,
        control: fence_closed_info_runs,
        opts: Opts::default(),
        control_opts: Opts::default(),
        limits,
    }
    .measure();
    let span = (SIZES[SIZES.len() - 1] / SIZES[0]) as f64; // 8x
    println!(
        "fence_giant_info growth {:.1}x vs the linear control's growth = {:.1}x (limit {:.0}); \
         raw wall ratio {:.1} (span x{span:.0}, limit {:.0}); per-size cost vs control \
         {:.1}x (informational — a different document, so the constant is arbitrary)",
        r.growth,
        r.growth_vs_control,
        limits.growth_vs_control,
        r.growth,
        limits.growth,
        r.vs_control,
    );
    assert!(
        r.growth_vs_control < limits.growth_vs_control,
        "growing fence opener line grew {:.1}x its LINEAR control's growth across the same \
         {span:.0}x span (limit {:.0}x; ~1x when frozen). Something re-derives a growing part of \
         the opener line per append — FenceInfoCache must stay frozen.",
        r.growth_vs_control,
        limits.growth_vs_control,
    );
    assert!(
        r.growth < limits.growth,
        "growing fence opener line went superlinear in WALL time: {:.1}x for {span:.0}x input \
         (limit {:.0}x; linear ≈ {span:.0}x, quadratic ≈ {:.0}x). Something re-derives a growing \
         part of the opener line per append — FenceInfoCache must stay frozen.",
        r.growth,
        limits.growth,
        span * span,
    );
}

/// A paragraph whose continuation lines are INDENTED — the wrapped-prose shape
/// an LLM/chat backend emits all day. Isolates `is_boundary`'s indent-led-line
/// rule: every word start sits after a space whose own predecessor is a space
/// or the line's `\n`, so without that rule the paragraph yields ZERO commit
/// candidates (`synth_boundary` can't rescue it either — its inert runs need
/// `SYNTH_GAP` space-free bytes and every line has a space), the cut pins at 0
/// and every append re-renders the whole accumulated paragraph.
///
/// Two deliberate shape choices, both needed to measure only the cut:
/// * ONE space-free token per line. A second token would sit after a single
///   inter-word space and hand the OLD rule a candidate for free.
/// * A 32-byte line and no preamble, so every 128-byte append boundary lands
///   exactly on a line start. A boundary landing INSIDE a leading indent leaves
///   the buffer ending in a whitespace-only incomplete line, which the block
///   scanner reads as a blank line (it genuinely ends the paragraph — the
///   one-shot render agrees), so `paragraph_ends_before_eof` bails the
///   incremental path into a full re-scan + re-render. That is a SECOND,
///   independent O(n²) — one append in `indent period` — and it is not what
///   this shape guards; see the indented-continuation note in `parser.rs`.
fn indented_wrapped_para(target: usize) -> String {
    let mut s = String::new();
    while s.len() < target {
        s.push_str(" the-quick-brown-fox-jumps-over\n"); // exactly 32 bytes
    }
    s
}

/// The FLUSH control twin of [`indented_wrapped_para`]: byte-for-byte the same
/// stream minus the indent, so its line-leading word is a candidate under the
/// pre-existing after-a-`\n` rule and its cut has always advanced. Same length,
/// same line count, same 32-byte alignment — so any wall-time gap between the
/// two is the indent, and nothing else.
fn flush_wrapped_para(target: usize) -> String {
    let mut s = String::new();
    while s.len() < target {
        s.push_str("the-quick-brown-fox-jumps-overs\n"); // exactly 32 bytes
    }
    s
}

/// The same wrapped-prose stream on an ODD line length, which is what makes an
/// append boundary land on the leading indent. Boundaries sit at multiples of
/// `128 mod L` within a line, so they only ever visit offsets that are multiples
/// of `gcd(128, L)`: any even `L` skips offset 1 entirely and never parks the
/// buffer on a bare `" "`. With `L = 7` the cycle covers every offset, so one
/// append in seven ends the buffer on a whitespace-only UNTERMINATED line.
///
/// That is a real (but transient) paragraph end — the block scanner splits the
/// line off — so `paragraph_ends_before_eof` fires and the incremental paragraph
/// path used to DROP its cache and pay a full re-scan + re-render for it.
/// Quadratic on its own, even with the commit cut advancing;
/// `trailing_open_ws_line` now suspends the cache across it instead.
fn indented_wrapped_para_ragged(target: usize) -> String {
    let mut s = String::new();
    while s.len() < target {
        s.push_str(" words\n"); // 7 bytes, odd -> gcd(128, 7) = 1
    }
    s
}

/// Flush control twin of [`indented_wrapped_para_ragged`] — same length, same
/// line count, same misalignment, no indent. Its trailing partial line always
/// carries content, so it never reaches the transient end.
fn flush_wrapped_para_ragged(target: usize) -> String {
    let mut s = String::new();
    while s.len() < target {
        s.push_str("wordsx\n"); // 7 bytes
    }
    s
}

/// A LIST ITEM whose open paragraph is word-wrapped with an extra indent past
/// the content column — the same transient-end shape as
/// [`indented_wrapped_para_ragged`], one container deep. An open item hosts a
/// nested `StreamParser`, so its paragraph runs through the very same
/// `try_incremental_paragraph`, and it inherits the suspension for free: with
/// the suspension disabled this measures 212x scanned / 209x rendered across a
/// 16x span (limit 64) and 22x its own flush control's wall time.
///
/// 9-byte lines: odd, so the 128-byte append boundary visits every offset and
/// regularly parks the nested buffer on a whitespace-only unterminated line.
fn indented_list_item_ragged(target: usize) -> String {
    let mut s = String::from("- first item line\n");
    while s.len() < target {
        s.push_str("   words\n"); // 2 stripped to the content column + 1 indent
    }
    s
}

/// Flush control twin of [`indented_list_item_ragged`]: same length, same line
/// count, same misalignment, but the nested paragraph's lines start flush at the
/// content column, so the nested buffer never ends whitespace-only.
fn flush_list_item_ragged(target: usize) -> String {
    let mut s = String::from("- first item line\n");
    while s.len() < target {
        s.push_str("  wordsx\n"); // exactly the content column, no extra indent
    }
    s
}

/// A BLOCKQUOTE of the same indented wrapped prose. Registered as a linear pin
/// for the proven-safe case: `ContainerCache` keeps its own stripped
/// `inner_buffer`/`inner_cut` and never consults [`ParaEnd`], so no transient
/// verdict exists for it to drop on — measured flat (15.9x / 15.9x over a 16x
/// span) both with and without the paragraph suspension.
fn indented_blockquote_ragged(target: usize) -> String {
    let mut s = String::from("> first line of the quote\n");
    while s.len() < target {
        s.push_str(">  words\n"); // 9 bytes; `> ` stripped, leaving ` words`
    }
    s
}

/// Wall-time twin of the `rendered` counter gate for the indented-continuation
/// paragraph (see [`indented_wrapped_para`]): best of 3 per size over an 8x span
/// (64 KB → 512 KB), against the [`flush_wrapped_para`] control twin.
///
/// Growth over the span is a blunt instrument HERE: one giant open paragraph
/// re-emits its whole `Block.html` every append (the documented wire contract —
/// see the `emitted` counter), which is an O(n²/chunk) memcpy floor no commit-cut
/// fix can remove, and it dominates the 512 KB sample — the quiet reading is
/// already 18.5x of a 24x limit, which is what made this the guard that flaked
/// (29.1x on a contended runner, with the counter gate for the same shape green
/// in the same job). So the PRIMARY assertion is the flush control: indented
/// prose must cost about what the byte-identical FLUSH prose costs, ~1x when the
/// cut advances and 60x+ when it pinned at 0. Raw growth stays as a SECONDARY
/// catch, moved out to 48x — halfway to the quadratic floor.
#[test]
fn indented_continuation_para_is_wall_linear() {
    const SIZES: [usize; 4] = [64 * 1024, 128 * 1024, 256 * 1024, 512 * 1024];
    let limits = WallRatios { vs_control: 4.0, growth_vs_control: f64::INFINITY, growth: 48.0 };
    let r = WallGuard {
        label: "indented_wrapped_para",
        control_label: "flush",
        sizes: &SIZES,
        runs: 3,
        shape: indented_wrapped_para,
        control: flush_wrapped_para,
        opts: Opts::default(),
        control_opts: Opts::default(),
        limits,
    }
    .measure();
    println!(
        "indented_wrapped_para worst cost vs flush prose {:.1}x (limit {:.0}); raw wall ratio \
         {:.1} (span x8, limit {:.0}; {:.1}x the flush control's own growth)",
        r.vs_control, limits.vs_control, r.growth, limits.growth, r.growth_vs_control,
    );
    assert!(
        r.vs_control < limits.vs_control,
        "indented continuation lines cost {:.1}x what the identical FLUSH prose costs (limit \
         {:.0}x). The paragraph commit cut stopped advancing — `is_boundary` must keep proposing \
         the first word of an indent-led line.",
        r.vs_control,
        limits.vs_control,
    );
    let span = (SIZES[SIZES.len() - 1] / SIZES[0]) as f64; // 8x
    assert!(
        r.growth < limits.growth,
        "indented continuation lines went superlinear in WALL time: {:.1}x for {span:.0}x input \
         (limit {:.0}x; linear ≈ {span:.0}x, the open paragraph's re-emit floor ≈ 18x, quadratic \
         ≈ {:.0}x). The paragraph commit cut stopped advancing — `is_boundary` must keep \
         proposing the first word of an indent-led line.",
        r.growth,
        limits.growth,
        span * span,
    );
}

/// Wall/control twin of [`indented_continuation_para_is_wall_linear`] for the
/// CHUNK-MISALIGNED line length (see [`indented_wrapped_para_ragged`]), where an
/// append boundary regularly parks the buffer on a transient paragraph end. Same
/// method, same limits; the sharp assertion is again the flush-prose control,
/// which was 63x when the cache was dropped there instead of suspended.
#[test]
fn indented_para_ragged_chunks_is_wall_linear() {
    const SIZES: [usize; 4] = [64 * 1024, 128 * 1024, 256 * 1024, 512 * 1024];
    let limits = WallRatios { vs_control: 4.0, growth_vs_control: f64::INFINITY, growth: 48.0 };
    let r = WallGuard {
        label: "indented_ragged",
        control_label: "flush",
        sizes: &SIZES,
        runs: 3,
        shape: indented_wrapped_para_ragged,
        control: flush_wrapped_para_ragged,
        opts: Opts::default(),
        control_opts: Opts::default(),
        limits,
    }
    .measure();
    println!(
        "indented_ragged worst cost vs flush prose {:.1}x (limit {:.0}); raw wall ratio {:.1} \
         (span x8, limit {:.0}; {:.1}x the flush control's own growth)",
        r.vs_control, limits.vs_control, r.growth, limits.growth, r.growth_vs_control,
    );
    assert!(
        r.vs_control < limits.vs_control,
        "chunk-misaligned indented continuation lines cost {:.1}x what the identical FLUSH prose \
         costs (limit {:.0}x). An append boundary landing on a line's leading indent must SUSPEND \
         the paragraph cache (`trailing_open_ws_line`), not drop it into a full re-scan + \
         re-render.",
        r.vs_control,
        limits.vs_control,
    );
    let span = (SIZES[SIZES.len() - 1] / SIZES[0]) as f64; // 8x
    assert!(
        r.growth < limits.growth,
        "chunk-misaligned indented continuation lines went superlinear in WALL time: {:.1}x for \
         {span:.0}x input (limit {:.0}x; linear ≈ {span:.0}x, the open paragraph's re-emit floor \
         ≈ 12x, quadratic ≈ {:.0}x).",
        r.growth,
        limits.growth,
        span * span,
    );
}

/// Wall/control twin of [`indented_para_ragged_chunks_is_wall_linear`] one
/// container deep (see [`indented_list_item_ragged`]). The nested parser inside
/// an open list item reaches the transient paragraph end exactly as the
/// top-level one does; this pins that it keeps inheriting the suspension.
#[test]
fn indented_list_item_ragged_is_wall_linear() {
    const SIZES: [usize; 4] = [64 * 1024, 128 * 1024, 256 * 1024, 512 * 1024];
    let limits = WallRatios { vs_control: 4.0, growth_vs_control: f64::INFINITY, growth: 48.0 };
    let r = WallGuard {
        label: "indented_list_item",
        control_label: "flush",
        sizes: &SIZES,
        runs: 3,
        shape: indented_list_item_ragged,
        control: flush_list_item_ragged,
        opts: Opts::default(),
        control_opts: Opts::default(),
        limits,
    }
    .measure();
    println!(
        "indented_list_item worst cost vs flush prose {:.1}x (limit {:.0}); raw wall ratio {:.1} \
         (span x8, limit {:.0}; {:.1}x the flush control's own growth)",
        r.vs_control, limits.vs_control, r.growth, limits.growth, r.growth_vs_control,
    );
    assert!(
        r.vs_control < limits.vs_control,
        "an open list item's indented continuation lines cost {:.1}x what the identical FLUSH \
         item costs (limit {:.0}x). The item's NESTED parser must keep suspending its paragraph \
         cache on a transient end (`ParaEnd::OpenBlank`), not dropping it.",
        r.vs_control,
        limits.vs_control,
    );
    let span = (SIZES[SIZES.len() - 1] / SIZES[0]) as f64; // 8x
    assert!(
        r.growth < limits.growth,
        "an open list item's indented continuation lines went superlinear in WALL time: {:.1}x \
         for {span:.0}x input (limit {:.0}x; linear ≈ {span:.0}x, the open item's re-emit floor ≈ \
         15x, quadratic ≈ {:.0}x).",
        r.growth,
        limits.growth,
        span * span,
    );
}

#[test]
fn ref_def_run_is_linear() {
    let small = measure(&ref_heavy(250), CHUNK, Opts::default());
    let large = measure(&ref_heavy(4000), CHUNK, Opts::default()); // 16x more defs
    let ratio = large.scanned as f64 / small.scanned as f64;
    println!(
        "ref_heavy  small={} large={} ratio={ratio:.1} (x16 defs)",
        small.scanned, large.scanned
    );
    assert!(
        ratio < linear_limit(16.0),
        "ref-def run regressed to superlinear: {ratio:.1}x work for 16x defs (limit {:.0}x)",
        linear_limit(16.0)
    );
}

/// Wall-time twin of the `rendered` gate for BLOCK-level raw HTML, against an
/// ESCAPE control: the identical stream measured with `block_html` OFF, which
/// folds per LINE through the long-standing escaped path. Same bytes, same line
/// count, same misalignment — so the gap between the two IS the token fold.
///
/// Growth ratio alone is blunt here for the same reason it is for the indented
/// paragraph: one giant open block re-emits its whole `Block.html` every append
/// (the documented wire contract — see the `emitted` column), an O(n²/chunk)
/// memcpy floor that dominates the 256 KB sample and that no fold can remove.
/// So the ONLY assertion is the control comparison: sanitizing block HTML must
/// cost about what escaping the same bytes costs (the raw growth ratio is
/// printed, not gated — the escaped control grows just as fast). Measured ~1x with the token fold;
/// the naive path (cache refuses to arm, full reparse + full re-sanitize per
/// append) measured 247x on BOTH work counters over the 16x span and 11.2 s vs
/// 49 ms of wall at 256 KB.
#[test]
fn block_html_sanitize_is_wall_linear() {
    // 64 KB up: below that the per-append fixed costs swamp the fold and the
    // ratio is pure noise (a 32 KB sample swung 1.9x–5.1x run to run).
    const SIZES: [usize; 3] = [64 * 1024, 128 * 1024, 256 * 1024];
    // Raw growth stays ungated on BOTH twins (see the note above): the escaped
    // control grows faster than the sanitized shape on the same stream.
    let limits = WallRatios {
        vs_control: 6.0,
        growth_vs_control: f64::INFINITY,
        growth: f64::INFINITY,
    };
    let r = WallGuard {
        label: "block_html_ragged",
        control_label: "escaped",
        sizes: &SIZES,
        runs: 5,
        shape: block_html_ragged,
        control: block_html_ragged,
        opts: Opts { html_sanitize: true, block_html: true, ..Opts::default() },
        // The control twin: sanitizer engaged, block HTML still escaped.
        control_opts: Opts { html_sanitize: true, ..Opts::default() },
        limits,
    }
    .measure();
    println!(
        "block_html_ragged worst cost vs escaped control {:.1}x (limit {:.0})",
        r.vs_control, limits.vs_control
    );
    println!(
        "block_html_ragged raw wall growth {:.1}x over a 4x span, {:.2}x the ESCAPED control's \
         growth on the same stream (informational — the open block's per-append full re-emit is \
         an O(n\u{b2}/chunk) memcpy floor both twins pay)",
        r.growth, r.growth_vs_control,
    );
    assert!(
        r.vs_control < limits.vs_control,
        "sanitizing block HTML costs {:.1}x what ESCAPING the same bytes costs (limit {:.0}x). \
         The HtmlBlockCache token fold must consume each token exactly once — re-sanitizing the \
         growing block per append measured 247x on both work counters and 121x this control's \
         wall time.",
        r.vs_control,
        limits.vs_control,
    );
}

/// The open WRAPPER BODY's settled prefix, gated on WALL time — deliberately,
/// for the same reason the fence-opener guard is: no counter here can see the
/// failure mode. A container / component cache that re-derives its already
/// committed sub-blocks every append scans nothing extra and inline-renders
/// nothing extra (the nested parser froze that html once, at commit); it only
/// walks the block list and memcpys each fragment out of its own allocation.
/// `scanned` and `rendered` stay flat and green while wall goes O(n²/chunk) in
/// the SUB-BLOCK COUNT — a second, independent quadratic riding on top of the
/// re-emit floor.
///
/// [`component_tiny_paras`] maximises that count per byte (one committed
/// paragraph per four source bytes) and [`tiny_paras_toplevel`] is the same
/// paragraphs unwrapped, which stream linearly — the SAME document minus the
/// single property under test, so the runner's weather cancels.
///
/// The PRIMARY gate is `growth_vs_control`: the shape's growth across the 8x
/// span divided by the linear control's growth across it. Limit 3x sits 2.5x
/// above the quiet value (1.2x with the prefix folded once) and 1.6x below the
/// re-walking one (4.7x). `vs_control` backs it up at 12x (quiet 3.6x, broken
/// 26.5x) — note that ratio legitimately CLIMBS with size even when correct,
/// because the shape still pays the open block's per-append `Block.html`
/// re-emit and the control does not; it is a bound on the constant, not a
/// linearity claim. Raw `growth` stays ungated for that same reason: the floor
/// alone makes it superlinear on a healthy tree.
#[test]
fn wrapper_body_prefix_is_wall_linear() {
    const SIZES: [usize; 4] = [32 * 1024, 64 * 1024, 128 * 1024, 256 * 1024];
    let opts = Opts { component_tags: &["Chart"], ..Opts::default() };
    let limits =
        WallRatios { vs_control: 12.0, growth_vs_control: 3.0, growth: f64::INFINITY };
    let r = WallGuard {
        label: "component_tiny_paras",
        control_label: "unwrapped",
        sizes: &SIZES,
        runs: 3,
        shape: component_tiny_paras,
        control: tiny_paras_toplevel,
        // Identical parser config on both twins — only the `<Chart>` line differs.
        opts,
        control_opts: opts,
        limits,
    }
    .measure();
    let span = (SIZES[SIZES.len() - 1] / SIZES[0]) as f64; // 8x
    println!(
        "component_tiny_paras growth {:.1}x vs the linear control's growth = {:.1}x (limit \
         {:.0}); worst per-size cost vs control {:.1}x (limit {:.0}); raw wall growth {:.1}x \
         (span x{span:.0}, ungated — the open block's per-append re-emit is an \
         O(n\u{b2}/chunk) memcpy floor only this twin pays)",
        r.growth,
        r.growth_vs_control,
        limits.growth_vs_control,
        r.vs_control,
        limits.vs_control,
        r.growth,
    );
    assert!(
        r.growth_vs_control < limits.growth_vs_control,
        "an open component's body grew {:.1}x its LINEAR control's growth across the same \
         {span:.0}x span (limit {:.0}x). The wrapper assembler must fold each COMMITTED inner \
         sub-block into its settled prefix exactly once — rebuilding the body from \
         `all_blocks()` every append measured 4.7x here with both work counters flat.",
        r.growth_vs_control,
        limits.growth_vs_control,
    );
    assert!(
        r.vs_control < limits.vs_control,
        "an open component's body of tiny paragraphs costs {:.1}x what the SAME paragraphs cost \
         unwrapped (limit {:.0}x). The wrapper assembler must fold each COMMITTED inner \
         sub-block into its settled prefix exactly once — re-walking them per append measured \
         26.5x this control.",
        r.vs_control,
        limits.vs_control,
    );
}

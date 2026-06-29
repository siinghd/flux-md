//! Incremental streaming parser.

use std::collections::HashMap;
use std::rc::Rc;

use crate::blocks::Block;
use crate::render::{
    blockquote_inner, classify, collect_footnote_defs, collect_footnote_refs,
    is_fence_close_line, is_footnote_def_block, item_body, normalize_label,
    parse_alignments, push_code_fence_open, push_table_cell, render_block,
    render_footnote_section, resolve_footnote_ids, split_table_cells, Enrichment, LinkRef,
    RenderOpts,
};
use crate::blocks::{BlockKind, ContainerData, ListItemData, NestedBlock, TableCell, TableData};
use crate::scanner::{
    count_table_columns, detect_html_block_open, html_block_line_closes, indent_cols, is_blank_line,
    is_setext_underline, is_table_delimiter_row, line_end, line_slice, parse_link_ref_def, scan,
    scan_marker, would_start_other_block, MarkerScan, RawBlock, RawBlockKind, ScanCtx,
};

/// True when a *stripped* container (blockquote/alert) inner line is NOT plain
/// paragraph prose — it starts or implies a block the container cache can't
/// render: a list / nested quote / heading / fence / thematic break / HTML /
/// math / component (`would_start_other_block`), a setext underline (`===`/`---`),
/// a table delimiter row (`| --- |`), or indented code (≥4 cols). Such content
/// must bail to the full reparse — otherwise the streamed blockquote/alert shows
/// it as escaped paragraph text until finalize (a structural flicker).
fn container_inner_breaks_paragraph(stripped: &[u8], ctx: ScanCtx<'_>) -> bool {
    would_start_other_block(stripped, 0, ctx)
        // any list marker, including an ordered list starting at a number other
        // than 1 (which `would_start_other_block` rejects because it cannot
        // *interrupt* a paragraph — yet it starts a list at the top of a body).
        || scan_marker(stripped).is_some()
        || is_setext_underline(stripped, 0).is_some()
        || is_table_delimiter_row(stripped)
        // a link reference definition produces no visible output; the cache would
        // otherwise render it as a literal paragraph.
        || parse_link_ref_def(stripped, 0).is_some()
        || indent_cols(stripped) >= 4
}
use crate::inline::{render_inline, render_inline_boundary};
use crate::url::escape_html;

/// Collect link reference definitions from `text` into `refs`, recursing into
/// block quotes and list items (definitions are document-wide, §4.7). `ctx`
/// keeps the block split identical to the render-time scan (e.g. a `$$…$$`
/// math fence stays one block instead of being mis-read).
/// Max container-nesting depth for the link-reference-definition sweep. This
/// recursion descends into blockquote/list inner content during `append`, so —
/// like the renderer's [`render::MAX_RENDER_DEPTH`] — it must be bounded or an
/// adversarial `">".repeat(10_000)` overflows the WASM shadow stack (an
/// uncatchable trap). 100 is far beyond any real document and well under the
/// 256 KB stack; a link reference nested >100 containers deep is meaningless.
const MAX_REF_DEPTH: usize = 100;

fn collect_refs(text: &str, refs: &mut HashMap<String, LinkRef>, ctx: ScanCtx, depth: usize) {
    if depth >= MAX_REF_DEPTH {
        return;
    }
    let bytes = text.as_bytes();
    for raw in scan(text, ctx) {
        match &raw.kind {
            RawBlockKind::LinkRefDefinition => {
                if let Some(((label, url, title), _)) = parse_link_ref_def(bytes, raw.range.start) {
                    refs.entry(normalize_label(&label)).or_insert(LinkRef { url, title });
                }
            }
            RawBlockKind::Blockquote => {
                let inner = blockquote_inner(&text[raw.range.clone()]);
                collect_refs(&inner, refs, ctx, depth + 1);
            }
            RawBlockKind::List { .. } => {
                // Re-split the list into items and recurse into each body.
                let slice = &text[raw.range.clone()];
                for item in split_list_items(slice) {
                    if let Some(body) = item_body(item.as_bytes()) {
                        collect_refs(&body, refs, ctx, depth + 1);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Resolve every footnote-ref placeholder token in a fully-produced `Block`
/// (its `html` plus any opt-in `kind.data` channel HTML) in document order.
///
/// `occ` is the running per-label occurrence map; it is advanced by exactly the
/// tokens in `block.html` (the canonical document-order stream). The structured
/// `kind.data` HTML carries the SAME tokens (same content, same order) duplicated
/// for the keyed renderer, so it is resolved from a CLONE of `occ` snapshotted
/// at the start of this block — that yields suffixes byte-identical to `html`
/// without double-counting occurrences. (Cell `text` is inline-stripped, so it
/// never contains a token.)
fn resolve_block_footnotes(block: &mut Block, occ: &mut HashMap<String, usize>) {
    // Snapshot for the data channel BEFORE html advances `occ`.
    let data_seed = occ.clone();

    let mut new_html = String::with_capacity(block.html.len());
    resolve_footnote_ids(&block.html, occ, &mut new_html);
    block.html = new_html;

    // Resolve the structured data channel (if any) from the snapshot, replaying
    // the same document order so the ids match `html` exactly.
    let resolve_one = |s: &str| -> String {
        let mut seed = data_seed.clone();
        let mut o = String::with_capacity(s.len());
        resolve_footnote_ids(s, &mut seed, &mut o);
        o
    };
    match &mut block.kind {
        BlockKind::Table(Some(td)) => {
            for cell in &mut td.headers {
                cell.html = resolve_one(&cell.html);
            }
            for row in &mut td.rows {
                let resolved: Vec<TableCell> = row
                    .iter()
                    .map(|c| TableCell { text: c.text.clone(), html: resolve_one(&c.html) })
                    .collect();
                *row = Rc::new(resolved);
            }
        }
        BlockKind::List { items, .. } => {
            for it in items {
                it.html = resolve_one(&it.html);
            }
        }
        BlockKind::Blockquote(Some(cd)) => {
            for nb in &mut cd.nested {
                nb.html = resolve_one(&nb.html);
            }
        }
        BlockKind::Alert { nested: Some(cd), .. } => {
            for nb in &mut cd.nested {
                nb.html = resolve_one(&nb.html);
            }
        }
        _ => {}
    }
}

/// Split a list slice into its item slices (by lines that begin a sibling
/// marker at the list's own indentation). A light re-implementation used only
/// for ref-def harvesting; rendering does its own item splitting.
fn split_list_items(slice: &str) -> Vec<&str> {
    use crate::scanner::{indent_cols, line_end, scan_marker};
    let bytes = slice.as_bytes();
    let mut starts = Vec::new();
    let mut pos = 0;
    let mut edge = 0usize;
    let mut cur_ci = 0usize;
    while pos < bytes.len() {
        let le = line_end(bytes, pos);
        let line = &bytes[pos..le];
        let is_blank = line.iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
        if !is_blank {
            let ind = indent_cols(line);
            if starts.is_empty() {
                if let Some(m) = scan_marker(line) {
                    edge = m.marker_indent;
                    cur_ci = m.content_indent;
                    starts.push(pos);
                }
            } else if ind < cur_ci && ind <= edge + 3 {
                if let Some(m) = scan_marker(line) {
                    cur_ci = m.content_indent;
                    starts.push(pos);
                }
            }
        }
        pos = le;
    }
    let mut items = Vec::new();
    for i in 0..starts.len() {
        let s = starts[i];
        let e = if i + 1 < starts.len() { starts[i + 1] } else { slice.len() };
        items.push(&slice[s..e]);
    }
    items
}

pub struct StreamParser {
    buffer: String,
    committed_offset: usize,
    committed_blocks: Vec<Block>,
    active_blocks: Vec<Block>,
    next_id: u64,
    finalized: bool,
    /// Reference definitions harvested from the *committed* region only — these
    /// are permanent (first definition wins, §4.7). Definitions in the still
    /// growing tail are recomputed fresh on every reparse so a partially typed
    /// definition (e.g. a URL mid-stream) never gets locked in.
    // `Rc` so each reparse shares the committed table with `RenderOpts` in O(1)
    // instead of cloning it per append (mutated in place via `Rc::make_mut` once
    // the render's `Rc` clone has been dropped — see `reparse_tail`).
    committed_refs: Rc<HashMap<String, LinkRef>>,
    /// Footnote numbering/defs from the *committed* region (permanent), mirroring
    /// `committed_refs`. `next_footnote` is the next number to assign; the tail
    /// continues from here so committed `<sup>N</sup>` numbers stay stable.
    committed_footnotes: HashMap<String, usize>,
    committed_footnote_defs: HashMap<String, String>,
    /// Total references per label in the committed region — seeds the tail's
    /// occurrence counter so repeated-reference ids stay unique across commits.
    committed_footnote_occurrences: HashMap<String, usize>,
    next_footnote: usize,
    unsafe_html: bool,
    gfm_autolinks: bool,
    gfm_alerts: bool,
    gfm_footnotes: bool,
    gfm_math: bool,
    dir_auto: bool,
    a11y: bool,
    /// Opt-in structured `kind.data` channel for Table blocks (`setBlockData`).
    /// Off by default — when off, Table serializes as `{"type":"Table"}` (no
    /// `data` key) and output is byte-identical to before.
    block_data: bool,
    /// Opt-in allowlist of custom "component" tag names (e.g. `Thinking`,
    /// `Callout`). A `<Tag>…</Tag>` whose name is listed is parsed as a container
    /// whose inner content is markdown, and dispatched to a React component —
    /// safely, without `unsafe_html`. Empty by default (no component tags).
    component_tags: Vec<Box<str>>,
    /// Opt-in allowlist of INLINE component tag names (e.g. `tik`, `cite`). An
    /// allowlisted `<tik>…</tik>` (or self-closing `<tik/>`) anywhere in inline
    /// content — paragraphs, headings, table cells, list items — renders as a
    /// real custom element (markdown inner, sanitized attrs) so a JSX/DOM layer
    /// dispatches it via `components[tag]`. Empty by default.
    inline_component_tags: Vec<Box<str>>,
    /// Opt-in safe raw-HTML sanitizer. When `html_sanitize` is on (engaged via a
    /// configured allow/drop list), inline raw HTML renders SAFELY without full
    /// `unsafe_html`: `html_allowlist` empty = allow all tags except a built-in
    /// dangerous set; non-empty = only those render (others escaped); `html_drop`
    /// tags are removed entirely; comments dropped; every rendered tag's
    /// attributes are sanitized. All off by default (output unchanged).
    html_sanitize: bool,
    html_allowlist: Vec<Box<str>>,
    html_drop: Vec<Box<str>>,
    /// Fast path for a long open code/math fence at the tail (see [`FenceCache`]).
    fence_cache: Option<FenceCache>,
    /// Fast path for a long open paragraph at the tail (see [`ParagraphCache`]).
    para_cache: Option<ParagraphCache>,
    /// Fast path for a long open GFM table at the tail (see [`TableCache`]).
    table_cache: Option<TableCache>,
    /// Fast path for a long open blockquote / alert at the tail (see [`ContainerCache`]).
    container_cache: Option<ContainerCache>,
    /// Fast path for a long open tight, flat list at the tail (see [`ListCache`]).
    list_cache: Option<ListCache>,
    /// Fast path for a long open indented-code block at the tail (see [`IndentedCodeCache`]).
    indented_cache: Option<IndentedCodeCache>,
    /// Fast path for a long open raw-HTML block at the tail (see [`HtmlBlockCache`]).
    html_cache: Option<HtmlBlockCache>,
}

#[derive(Default)]
pub struct Patch {
    pub newly_committed: Vec<Block>,
    pub active: Vec<Block>,
}

/// How an open fence's closing line is recognized. The cache MUST match the
/// scanner's predicate exactly, or streamed and one-shot output diverge.
#[derive(Clone, Copy)]
enum FenceClose {
    /// Code fence: a line that is *only* a closing fence (``` / ~~~), per
    /// `is_fence_close_line`.
    CodeFence,
    /// Display-math fence: a line *containing* this closer substring (`$$` or
    /// `\]`), mirroring the scanner's `scan_math_block`.
    MathCloser(&'static [u8]),
}

/// Incremental render state for a single open fence — a code fence or a
/// display-math fence — at the tail. Streaming a long fenced block is otherwise
/// O(n²): every append re-scans and re-escapes the whole growing body. With this
/// cache, an append only escapes the newly arrived complete lines and re-escapes
/// the (short) trailing partial line, so the block stays O(total bytes). It
/// applies only to the plain case: the cache bails to the full renderer the
/// moment a new line looks like the closer or contains a `\r` (so CRLF and
/// close/whitespace trimming keep their exact behavior). Cleared whenever the
/// tail is no longer this open fence.
struct FenceCache {
    /// Absolute byte offset of the fence opener line in `buffer`.
    start: usize,
    /// Stable id of the fence block (preserved across appends and the eventual close).
    id: u64,
    /// Classified kind (CodeBlock / MathBlock / Mermaid — all render identically).
    kind: BlockKind,
    /// Opening tag — `<pre><code…>` or `<div class="math math-display">`.
    opener_html: String,
    /// Closing tag — `</code></pre>` or `</div>`.
    closer_html: &'static str,
    /// How the closing line is detected (code-fence rule vs math closer substring).
    close: FenceClose,
    /// Math fences trim surrounding whitespace of the body; code fences don't.
    trim_body: bool,
    /// Escaped HTML of the complete body lines, joined by `\n`, no trailing `\n`.
    escaped_lines: String,
    /// Absolute offset just past the last complete body line's `\n`.
    lines_upto: usize,
}

/// True if `needle` occurs anywhere in `haystack` (used for the math closer).
fn line_contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Incremental render state for a single open paragraph at the tail. Streaming
/// a long paragraph is otherwise O(n²) — the whole growing, uncommitted
/// paragraph is re-`render_inline`d each append. Unlike code, inline output is
/// not prefix-stable (a late `*` can emphasize earlier text; a code span or
/// link spans inter-word spaces). So this cache commits only a *plain* prefix:
/// text up to the last top-level inter-word boundary that precedes the first
/// space-spanning-construct character. That prefix is final (it contains no
/// construct that future input can reach), so it's rendered once and only the
/// short active tail is re-rendered. Long plain paragraphs (the realistic
/// O(n²) trigger) become O(n); a paragraph whose constructs start early keeps
/// today's behavior (no regression, no speedup).
struct ParagraphCache {
    /// Absolute byte offset of the paragraph start in `buffer`.
    start: usize,
    /// Stable id of the paragraph block.
    id: u64,
    /// Absolute offset; `buffer[start..cut]` is committed (plain, construct-free)
    /// and rendered into `committed_inner`. Always at a clean word/line boundary.
    cut: usize,
    /// Rendered inline HTML of `buffer[start..cut]`.
    committed_inner: String,
    /// Footnote occurrence map for the FROZEN prefix (`committed_inner`). Seeded
    /// from the committed occurrence counts at arm time, advanced when a settled
    /// segment's placeholder tokens are resolved into `committed_inner`. The
    /// speculative active tail resolves from a CLONE. Unused when footnotes off.
    fn_occ: HashMap<String, usize>,
}

/// Incremental render state for a single open GFM table at the tail. Streaming
/// a long table is otherwise O(n²) — `render_table` re-walks every row on every
/// append, normalizing cell counts and re-rendering inline content. Each body
/// row's HTML is self-contained (it depends only on the row's own bytes, the
/// header's column count, the alignments, and the committed link-ref/footnote
/// tables — none of which change while the table is open), so once a row is
/// rendered into the cache it's stable. The cache stores the pre-rendered
/// prefix (`<table>…<thead>…</thead>` plus the `<tbody>` opener and every
/// completed `<tr>`) and extends it by the newly-arrived complete rows; the
/// trailing partial row is re-rendered each append (it is short).
///
/// Footnote-aware: the cache renders each row's `[^x]` ref as an
/// occurrence-INDEPENDENT placeholder token and resolves the `id="fnref-…"`
/// suffix into the frozen prefix in document order from the committed occurrence
/// baseline (advancing `fn_occ`), so a streamed footnote table stays O(new bytes)
/// and is byte-identical to the one-shot render.
struct TableCache {
    /// Absolute byte offset of the table's header line in `buffer`.
    start: usize,
    /// Stable id of the table block (preserved across appends and the eventual close).
    id: u64,
    /// Pre-rendered HTML prefix: `<table dir?><thead>…</thead>` and, once any
    /// body row exists, `<tbody>` followed by every completed `<tr>…</tr>`.
    /// No trailing `</tbody></table>`.
    cached_prefix: String,
    /// Absolute offset just past the last complete cached body row's `\n`. The
    /// next complete line at this offset is the next row to fold into the cache.
    lines_upto: usize,
    /// Header column count (locked at the delimiter row).
    ncol: usize,
    /// Per-column alignment (parsed once from the delimiter row).
    aligns: Vec<Option<&'static str>>,
    /// `true` once we've emitted `<tbody>` into `cached_prefix` (after the first
    /// committed body row). The trailing partial-row path emits its own `<tbody>`
    /// when speculatively rendering the very first row of the body.
    tbody_opened: bool,
    /// Structured `kind.data` channel (only populated when `block_data` is on):
    /// the header cells (locked once, parallel to the `<thead>` in
    /// `cached_prefix`). Empty + unused when off.
    header_cells: Vec<TableCell>,
    /// Structured channel: the committed body-row cells, pushed at the exact
    /// step a row's `<tr>` is folded into `cached_prefix` so DATA never diverges
    /// from HTML. The speculative trailing partial row is NOT stored here (it is
    /// rebuilt fresh each append, mirroring `partial_html`). Empty when off.
    /// Each row is `Rc`-shared so re-emitting the full table per patch costs an
    /// O(rows) refcount bump, not an O(cells) `String` deep clone.
    body_cells: Vec<Rc<Vec<TableCell>>>,
    /// Footnote occurrence map for the FROZEN prefix (`cached_prefix` + the
    /// `body_cells` data). Seeded from the committed occurrence counts at arm
    /// time and advanced (via `resolve_footnote_ids`) each time a row's
    /// placeholder tokens are resolved into the prefix — so the frozen ids are
    /// computed once, in document order, and never re-touched. The speculative
    /// trailing partial row resolves from a CLONE of this (doesn't advance it).
    /// Unused (empty) when footnotes are off.
    fn_occ: HashMap<String, usize>,
}

/// Incremental render state for a single open GFM blockquote / alert at the
/// tail whose inner is one growing paragraph. Long resumable containers are
/// otherwise O(n²) — every append re-runs `blockquote_inner` + `scan` + the
/// full inline render over the whole growing inner. This cache wraps the
/// paragraph-cache pattern with a `>`-stripped inner buffer: each new
/// `> ` line is stripped once into `inner_buffer`, and only the unsettled
/// inline tail is re-rendered per append.
///
/// Handles a multi-paragraph inner — each blank `>` line closes the current
/// paragraph (rendered once into `committed_paras_html`) and starts a new one.
/// The wrapper (blockquote / alert div + title) is unchanged. The cache
/// bails (full path takes over) on any of:
///   - a line without a `>` marker (lazy continuation or end-of-container),
///   - a `\r` byte in any processed line (CRLF input — full path handles it).
///
/// Footnote-aware, mirroring `TableCache`: inner `[^x]` refs render as
/// placeholder tokens and are resolved into the frozen content in document
/// order (closed paragraphs advance `fn_occ`; the open paragraph's settled
/// prefix advances `inner_fn_occ`, discarded on close so re-rendering the
/// closed paragraph whole never double-counts).
struct ContainerCache {
    /// Absolute byte offset of the container's first line in `buffer`.
    start: usize,
    /// Stable id of the container block (preserved across appends and the close).
    id: u64,
    /// Container variant — drives wrapper HTML + line accounting (Alert skips
    /// the `[!KIND]` marker line; Blockquote starts from the first line).
    kind: ContainerCacheKind,
    /// Wrapper opener that always appears: `<blockquote dir?>\n` for blockquote,
    /// or `<div class="...">\n<p class="...title">Title</p>\n` for an alert.
    wrapper_open: String,
    /// Body paragraph opener: `<p dir?>` — emitted only when the current
    /// paragraph has content. An empty current paragraph must produce no
    /// `<p></p>` (matches the full renderer's per-sub-block contract).
    body_p_open: String,
    /// Body paragraph closer plus the `\n` that the full renderer emits after
    /// each sub-block: `</p>\n`.
    body_p_close: String,
    /// Wrapper closer: `</blockquote>` or `</div>`.
    wrapper_close: String,
    /// Pre-rendered HTML of every fully-closed inner paragraph, each in the
    /// shape `<p dir?>{inline}</p>\n`. Closed paragraphs never re-render
    /// (each blank `>` line costs one final `render_inline` and one push).
    committed_paras_html: String,
    /// Structured `kind.data` channel (only populated when `block_data` is on):
    /// each fully-closed inner paragraph's own HTML (`<p dir?>{inline}</p>`, no
    /// trailing `\n`), pushed in lock-step with `committed_paras_html` so a
    /// keyed override gets one stable, memoizable entry per committed paragraph.
    /// The still-open current paragraph is appended fresh each patch (mirroring
    /// `partial_html` in the table cache). Empty + unused when off.
    committed_paras: Vec<NestedBlock>,
    /// Stripped inner content of the CURRENT (still-open) paragraph, one
    /// `\n`-terminated line per processed source line. Cleared on close.
    inner_buffer: String,
    /// Absolute buffer offset just past the last `\n` we've stripped into
    /// `inner_buffer`. The next complete line at this offset is the next
    /// candidate to fold.
    lines_upto: usize,
    /// Position in `inner_buffer`; bytes in `[0..inner_cut]` are the settled
    /// prefix whose rendered HTML lives in `committed_inner_html` and is
    /// never re-rendered again. Resets to 0 when the current paragraph closes.
    inner_cut: usize,
    /// Rendered inline HTML of `inner_buffer[0..inner_cut]`. Cleared on close.
    committed_inner_html: String,
    /// Footnote occurrence map after all CLOSED paragraphs (the truly frozen
    /// state in `committed_paras_html`). Seeded from the committed occurrence
    /// counts at arm time; advanced ONLY when a paragraph closes (it is then
    /// re-rendered whole). The current open paragraph never advances this map.
    /// Unused when footnotes off.
    fn_occ: HashMap<String, usize>,
    /// Footnote occurrence map for the current OPEN paragraph's settled prefix
    /// (`committed_inner_html`). Reset to `fn_occ.clone()` when a paragraph
    /// starts; advanced as the inline boundary commits segments into
    /// `committed_inner_html`. The active tail resolves from a CLONE of this.
    /// Because the open paragraph is re-rendered whole on close (advancing the
    /// persistent `fn_occ`), this sub-map is discarded then — no double-count.
    inner_fn_occ: HashMap<String, usize>,
}

#[derive(Clone, Copy)]
enum ContainerCacheKind {
    Blockquote,
    Alert(crate::blocks::AlertKind),
}

/// Incremental render state for a single open *flat* list at the tail — the
/// LLM-emit shape where every line is a same-family marker (no continuation,
/// no nesting). Handles both tight and loose lists; a tight list whose
/// siblings end up separated by a blank line flips to loose mid-stream and
/// re-renders its prior items with the loose `<p>` wrapper. The cache bails
/// (full path takes over) on any of:
///   - a non-blank line that isn't a sibling marker (continuation, paragraph,
///     end-of-list — the full path handles lazy lines and multi-block items),
///   - a line whose `marker_indent` exceeds the list's `edge + 3` (nested),
///   - a line of a different marker family / delimiter,
///   - a `\r` byte (CRLF — full path handles).
///
/// Inside the cache, each new sibling line renders directly as `<li>…</li>`
/// (tight inline `<li>{inline}</li>`, or loose `<li>\n<p>{inline}</p></li>`,
/// GFM task-list `[ ]`/`[x]` checkbox prefix supported), folded into a single
/// cached HTML buffer. Subsequent appends do O(new bytes); the one-time
/// tight→loose rebuild is O(items so far).
///
/// Footnote-aware, like `TableCache` / `ContainerCache`: each item's `[^x]`
/// renders as a placeholder token and is resolved into the frozen prefix in
/// document order (advancing `fn_occ`); the tight→loose rebuild replays from
/// `fn_occ_base` so the re-rendered loose items keep the same ids.
struct ListCache {
    /// Absolute byte offset of the list's first line in `buffer`.
    start: usize,
    /// Stable id of the list block.
    id: u64,
    /// Ordered vs. unordered — locked at the first marker.
    ordered: bool,
    /// The ordered-list start number (the `start="N"` HTML attribute; `1` for an
    /// unordered list). Folded onto the active block's `BlockKind::List { start }`
    /// when `block_data` is on, so the streamed `kind.data` matches the full path.
    start_num: u32,
    /// Marker family + delimiter (`b'-'`/`b'*'`/`b'+'` for bullets,
    /// `b'.'`/`b')'` for ordered). A sibling must match.
    delim: u8,
    /// `content_indent` of the first item — the column where its content starts.
    /// A later marker is a SIBLING only if `marker_indent < content_indent`; a
    /// marker at or past the content column begins a NESTED sub-list, which this
    /// flat-list cache can't render — so it bails to the full reparse instead of
    /// flattening the sub-list (CommonMark §5.2 / §5.3).
    content_indent: usize,
    /// `<ul>` / `<ol start=N>` opener + `\n`, frozen at arm time. Kept separate
    /// from item HTML so the tight→loose rebuild only touches items.
    opener_html: String,
    /// Pre-rendered HTML: opener + every fully-cached `<li>…</li>\n`. No
    /// trailing `</ul>` / `</ol>`.
    cached_prefix: String,
    /// Absolute offset just past the last cached complete item line's `\n`
    /// (or past any blanks the lookahead consumed when transitioning loose).
    lines_upto: usize,
    /// `true` once any two siblings are separated by a blank line (§5.3).
    /// Sticky — never flips back; new items render with the loose `<p>` wrap.
    loose: bool,
    /// Source spans `(start, end)` of every cached item line in `buffer`.
    /// `end` is the byte just before that line's `\n` (so `&buffer[s..e]` is
    /// the line content). Used to re-render on the tight→loose transition.
    items: Vec<(usize, usize)>,
    /// Per-item inner `<li>` HTML for the opt-in `kind.data` channel — one entry
    /// per `items` span, parallel to `cached_prefix`. Empty unless `block_data` is
    /// on; surfaced on the active block's `BlockKind::List { items }` so the keyed
    /// renderer reuses unchanged item nodes while the list streams.
    item_html: Vec<ListItemData>,
    /// Footnote occurrence map for the FROZEN prefix (`cached_prefix` +
    /// `item_html`). Seeded from the committed occurrence counts at arm time and
    /// advanced when an item's placeholder tokens are resolved into the prefix.
    /// On a tight→loose rebuild the map is re-derived from the baseline by
    /// replaying every item in order. The speculative trailing item resolves from
    /// a CLONE. Unused when footnotes are off.
    fn_occ: HashMap<String, usize>,
    /// The baseline occurrence map captured at arm time (committed counts), kept
    /// so the tight→loose rebuild can reset `fn_occ` and replay every item.
    fn_occ_base: HashMap<String, usize>,
}

/// Incremental render state for a single open *indented-code* block at the tail —
/// the streaming shape where every line is ≥4-column-indented content with no
/// interior blank line. Streaming such a block is otherwise O(n²): every append
/// re-strips and re-escapes the whole growing body (`render_indented_code`).
/// With this cache an append only strips+escapes the newly-arrived complete
/// lines and re-renders the (short) trailing partial line.
///
/// The cache bails (full path takes over) the instant the simple pattern breaks:
///   - a newly-complete line that dedents (indent < 4) — it ends the block,
///   - a blank line — the full path owns interior-blank accounting (the block
///     range stops at the last content line, blanks may or may not be absorbed),
///   - a `\r` byte (CRLF) in any processed line.
/// The mirror of `render_indented_code`: each line strips up to 4 columns of
/// leading indent (one tab counts as enough and is consumed whole), the body is
/// the stripped lines joined by `\n`, trailing whitespace trimmed, then a single
/// `\n`, wrapped in `<pre><code>…</code></pre>`.
struct IndentedCodeCache {
    /// Absolute byte offset of the block's first line in `buffer`.
    start: usize,
    /// Stable id of the code block (preserved across appends and the close).
    id: u64,
    /// Escaped HTML of the complete stripped body lines, joined by `\n`, no
    /// trailing `\n`. Whitespace bytes survive `escape_html` unchanged, so
    /// trimming this matches trimming the decoded source.
    escaped_lines: String,
    /// Absolute offset just past the last complete body line's `\n`.
    lines_upto: usize,
}

/// Incremental render state for a single open *raw-HTML* block at the tail.
/// Streaming a long HTML block is otherwise O(n²): `render_html_block` re-escapes
/// (or re-copies) the whole growing slice on every append. The block's output is
/// a pure function of its contiguous source slice — `<pre><code>` + escaped slice
/// + `</code></pre>` when escaping, or the trailing-newline-trimmed slice + `\n`
/// in `unsafe_html` pass-through — so completed lines fold into `cached_prefix`
/// once and only the short trailing partial is re-processed per append.
///
/// `html_type` (1–7, from [`crate::scanner::detect_html_block_open`]) drives the
/// close detection, which MUST match `scan_html_block` exactly: a completed line
/// (or the partial) satisfying the type-specific closer (types 1–5, via the
/// shared [`crate::scanner::html_block_line_closes`]) or a blank line (types 6/7)
/// ends the block, so the cache bails there and the full path commits it. A `\r`
/// byte also bails (CRLF routes through the full renderer in both modes).
struct HtmlBlockCache {
    /// Absolute byte offset of the block's first line in `buffer`.
    start: usize,
    /// Stable id of the HTML block (preserved across appends and the close).
    id: u64,
    /// HTML-block type 1–7 (locked at arm time). Drives the close condition.
    html_type: u8,
    /// When `true`, raw HTML passes through verbatim (`unsafe_html` and the
    /// sanitizer is off); when `false`, the slice is escaped into `<pre><code>`.
    /// Locked at arm time from the parser's options.
    pass_through: bool,
    /// Pre-rendered prefix of the completed lines: for pass-through, the raw
    /// source bytes verbatim (including their `\n`); for the escaped path, their
    /// `escape_html` output (newlines survive escaping unchanged). No closer.
    cached_prefix: String,
    /// Absolute offset just past the last complete folded line's `\n`.
    lines_upto: usize,
}

impl StreamParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            committed_offset: 0,
            committed_blocks: Vec::new(),
            active_blocks: Vec::new(),
            next_id: 0,
            finalized: false,
            committed_refs: Rc::new(HashMap::new()),
            committed_footnotes: HashMap::new(),
            committed_footnote_defs: HashMap::new(),
            committed_footnote_occurrences: HashMap::new(),
            next_footnote: 1,
            unsafe_html: false,
            gfm_autolinks: false,
            gfm_alerts: false,
            gfm_footnotes: false,
            gfm_math: false,
            dir_auto: false,
            a11y: false,
            block_data: false,
            component_tags: Vec::new(),
            inline_component_tags: Vec::new(),
            html_sanitize: false,
            html_allowlist: Vec::new(),
            html_drop: Vec::new(),
            fence_cache: None,
            para_cache: None,
            table_cache: None,
            container_cache: None,
            list_cache: None,
            indented_cache: None,
            html_cache: None,
        }
    }

    /// Allow raw HTML to pass through unescaped. Default is `false` (escape).
    /// Required for full CommonMark spec compliance. **Do not enable for
    /// untrusted input** — it bypasses XSS protection.
    pub fn with_unsafe_html(mut self, on: bool) -> Self {
        self.unsafe_html = on;
        self
    }

    /// Enable GFM extended autolinks (bare `www.`/`http(s)://`/`ftp://` URLs in
    /// text become links). Off by default (strict CommonMark).
    pub fn with_gfm_autolinks(mut self, on: bool) -> Self {
        self.gfm_autolinks = on;
        self
    }

    pub fn set_gfm_autolinks(&mut self, on: bool) {
        self.gfm_autolinks = on;
    }

    /// Enable GitHub alerts (`> [!NOTE]` → styled callout). Off by default
    /// (strict CommonMark renders a plain blockquote).
    pub fn with_gfm_alerts(mut self, on: bool) -> Self {
        self.gfm_alerts = on;
        self
    }

    pub fn set_gfm_alerts(&mut self, on: bool) {
        self.gfm_alerts = on;
    }

    /// Enable GFM footnotes (`[^1]` + `[^1]:` → footnote section). Off by
    /// default. References render speculatively; the section is emitted at
    /// finalize (see the footnote streaming notes in the README).
    pub fn with_gfm_footnotes(mut self, on: bool) -> Self {
        self.gfm_footnotes = on;
        self
    }

    pub fn set_gfm_footnotes(&mut self, on: bool) {
        self.gfm_footnotes = on;
    }

    /// Enable math: `$…$` / `\(…\)` inline and `$$…$$` / `\[…\]` display math.
    /// Off by default so `$` in ordinary prose (and currency like `$5`) stays
    /// literal. Inline uses the pandoc rule for `$` (the opener has a non-space
    /// to its right, the closer a non-space to its left and no digit after it),
    /// so `$5 and $10` is not treated as math. The HTML carries the LaTeX in
    /// `<span class="math math-inline">` / `<div class="math math-display">` for
    /// KaTeX (bring your own renderer — flux-md stays zero-dep).
    pub fn with_gfm_math(mut self, on: bool) -> Self {
        self.gfm_math = on;
        self
    }

    pub fn set_gfm_math(&mut self, on: bool) {
        self.gfm_math = on;
    }

    /// Emit `dir="auto"` on block-level text elements (`<p>`, `<h1>`–`<h6>`,
    /// `<blockquote>`, `<ul>`/`<ol>`/`<li>`, `<table>`) so the browser detects
    /// each block's text direction independently (LTR/RTL) via the Unicode bidi
    /// algorithm — correct for documents that mix English with Arabic/Hebrew.
    /// Off by default (strict-CommonMark output has no `dir`); code blocks never
    /// get it (code is always LTR).
    pub fn with_dir_auto(mut self, on: bool) -> Self {
        self.dir_auto = on;
        self
    }

    pub fn set_dir_auto(&mut self, on: bool) {
        self.dir_auto = on;
    }

    /// Opt-in accessibility markup that deviates from strict GFM byte-output:
    /// `<label>`-wrap a task-list checkbox with its text, and `scope="col"` on
    /// table header cells. Off by default (CommonMark/GFM output unchanged).
    pub fn with_a11y(mut self, on: bool) -> Self {
        self.a11y = on;
        self
    }

    pub fn set_a11y(&mut self, on: bool) {
        self.a11y = on;
    }

    /// Enable the opt-in structured `kind.data` channel for Table blocks: a Table
    /// then carries `{ headers, rows, aligns }` (per-cell `{ text, html }`) so a
    /// consumer can build a sort/filter/transpose/chart/CSV toolbar from DATA
    /// without re-parsing the HTML. Off by default (Table serializes as
    /// `{"type":"Table"}`, no `data` key — byte-identical output).
    pub fn with_block_data(mut self, on: bool) -> Self {
        self.block_data = on;
        self
    }

    pub fn set_block_data(&mut self, on: bool) {
        self.block_data = on;
    }

    /// Set the opt-in component-tag allowlist (e.g. `["Thinking", "Callout"]`).
    /// A `<Tag>…</Tag>` whose name is listed renders as a component with markdown
    /// inner content. Names are matched exactly (case-sensitively). Empty = off.
    pub fn with_component_tags(mut self, tags: Vec<String>) -> Self {
        self.component_tags = tags.into_iter().map(String::into_boxed_str).collect();
        self
    }

    pub fn set_component_tags(&mut self, tags: Vec<String>) {
        self.component_tags = tags.into_iter().map(String::into_boxed_str).collect();
    }

    /// Set the opt-in INLINE component-tag allowlist (e.g. `["tik", "cite"]`).
    /// An allowlisted `<tik>…</tik>` (or self-closing `<tik/>`) in inline content
    /// renders as a custom element whose inner is markdown and whose attributes
    /// are sanitized — XSS-safe without `unsafe_html`. Separate from
    /// `component_tags` (block containers): list a tag here for inline chips
    /// (tickers, citations, @mentions); put it in both lists to allow both
    /// positions. Names are matched exactly (case-sensitively). Empty = off.
    pub fn with_inline_component_tags(mut self, tags: Vec<String>) -> Self {
        self.inline_component_tags = tags.into_iter().map(String::into_boxed_str).collect();
        self
    }

    pub fn set_inline_component_tags(&mut self, tags: Vec<String>) {
        self.inline_component_tags = tags.into_iter().map(String::into_boxed_str).collect();
    }

    /// Engage the safe raw-HTML sanitizer and set its allow/drop lists. When on,
    /// inline raw HTML renders sanitized (no `unsafe_html` needed): `allow` empty
    /// = allow all non-dangerous tags; non-empty = only those (others escaped);
    /// `drop` tags are removed entirely; comments dropped; attributes sanitized.
    pub fn set_html_sanitize(&mut self, on: bool, allow: Vec<String>, drop: Vec<String>) {
        self.html_sanitize = on;
        self.html_allowlist = allow.into_iter().map(String::into_boxed_str).collect();
        self.html_drop = drop.into_iter().map(String::into_boxed_str).collect();
    }

    pub fn with_html_sanitize(mut self, on: bool, allow: Vec<String>, drop: Vec<String>) -> Self {
        self.set_html_sanitize(on, allow, drop);
        self
    }

    pub fn set_unsafe_html(&mut self, on: bool) {
        self.unsafe_html = on;
    }

    pub fn append(&mut self, chunk: &str) -> Patch {
        if self.finalized {
            return Patch::default();
        }
        self.buffer.push_str(chunk);
        self.reparse_tail(false)
    }

    pub fn finalize(&mut self) -> Patch {
        if self.finalized {
            return Patch::default();
        }
        self.finalized = true;
        self.reparse_tail(true)
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    pub fn all_blocks(&self) -> impl Iterator<Item = &Block> {
        self.committed_blocks.iter().chain(self.active_blocks.iter())
    }

    pub fn retained_bytes(&self) -> usize {
        let mut n = self.buffer.len();
        for b in &self.committed_blocks {
            n += b.html.len();
        }
        for b in &self.active_blocks {
            n += b.html.len();
        }
        n
    }

    fn reparse_tail(&mut self, finalizing: bool) -> Patch {
        // Fast paths: extend a long open code/math fence / paragraph at the tail
        // in O(new bytes) instead of re-scanning + re-rendering the whole tail.
        if !finalizing {
            if let Some(patch) = self.try_incremental_fence() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_paragraph() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_table() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_container() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_list() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_indented() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_html() {
                return patch;
            }
        }

        let tail_start = self.committed_offset;
        let tail = &self.buffer[tail_start..];

        let ctx = ScanCtx {
            math: self.gfm_math,
            component_tags: &self.component_tags,
            inline_component_tags: &self.inline_component_tags,
        };
        let raw_blocks = scan(tail, ctx);

        // Pre-pass: build the ref table for this render. The committed table is
        // shared into opts by an O(1) `Rc` clone (never copied per append);
        // tail definitions are collected fresh each reparse (so a half-typed
        // definition in the growing tail can't get stuck). Committed wins at
        // lookup time (first-definition-wins).
        let committed_refs = Rc::clone(&self.committed_refs);
        let mut tail_refs = HashMap::new();
        collect_refs(tail, &mut tail_refs, ctx, 0);

        // Renderable blocks: skip link-ref defs (no output) and, when footnotes
        // are on, footnote definitions (collected into the section instead).
        let gfm_footnotes = self.gfm_footnotes;
        let is_footnote_def = |slice: &str| gfm_footnotes && is_footnote_def_block(slice);
        let renderable: Vec<&RawBlock> = raw_blocks
            .iter()
            .filter(|r| !matches!(r.kind, RawBlockKind::LinkRefDefinition))
            .filter(|r| !is_footnote_def(&tail[r.range.clone()]))
            .collect();

        // Footnote numbering pre-pass: committed numbers (permanent) + tail
        // references continuing from `next_footnote`, in document order over the
        // renderable (non-def) content only.
        let mut fn_nums = self.committed_footnotes.clone();
        let mut fn_next = self.next_footnote;
        if gfm_footnotes {
            for raw in &renderable {
                collect_footnote_refs(&tail[raw.range.clone()], &mut fn_nums, &mut fn_next);
            }
        }

        let opts = RenderOpts {
            unsafe_html: self.unsafe_html,
            committed_refs,
            tail_refs,
            in_link: false,
            // Base = false (one-shot CommonMark: incomplete link → literal).
            // Overridden to true PER-BLOCK below, only for the final block when it
            // abuts buffer EOF and is not blank-line-closed — so `one_shot_open`
            // (full rescan, single append, no finalize) agrees byte-for-byte with
            // the streaming-cache `streamed_open`. At finalize (`!finalizing` is
            // false) every block stays false → committed output is literal.
            open_tail: false,
            gfm_autolinks: self.gfm_autolinks,
            gfm_alerts: self.gfm_alerts,
            gfm_math: self.gfm_math,
            dir_auto: self.dir_auto,
            a11y: self.a11y,
            block_data: self.block_data,
            gfm_footnotes,
            footnotes: fn_nums.clone(),
            // Seed the per-label occurrence counter from the committed counts so
            // ref ids stay unique across the commit boundary.
            footnote_occ: std::cell::RefCell::new(self.committed_footnote_occurrences.clone()),
            // Full-reparse body renders footnote refs as placeholder tokens (when
            // footnotes are on) so it agrees byte-for-byte with the cache path;
            // each produced block's html is then resolved in document order
            // (seeded from the committed occurrence map) just below.
            footnote_placeholder: gfm_footnotes,
            component_tags: self.component_tags.clone(),
            inline_component_tags: self.inline_component_tags.clone(),
            html_sanitize: self.html_sanitize,
            html_allowlist: self.html_allowlist.clone(),
            html_drop: self.html_drop.clone(),
        };

        // Parity load-bearer for speculative open-tail links. `one_shot_open(md)`
        // (single append, no finalize) renders the open tail through THIS full
        // rescan; for `one_shot_open == streamed_open` the rescan's FINAL block
        // must get `open_tail=true` under the SAME condition the streaming tail
        // caches fire: it is the last renderable block, it abuts buffer EOF, and
        // the buffer is not closed by a trailing blank line (which would settle
        // the block). At finalize (`finalizing`) this stays false everywhere, so
        // every incomplete link degrades to literal → committed byte-parity with
        // a one-shot complete-literal render.
        let buffer_ends_blank =
            self.buffer.ends_with("\n\n") || self.buffer.ends_with("\r\n\r\n");
        let last_idx = renderable.len().wrapping_sub(1);

        let mut produced: Vec<Block> = Vec::with_capacity(renderable.len());
        for (bi, raw) in renderable.iter().enumerate() {
            let mut kind = classify(&raw.kind, &tail[raw.range.clone()], self.gfm_alerts);
            let mut html = String::with_capacity(64);
            // Per-block open_tail: the final block that abuts buffer EOF and is
            // not blank-line-closed. Clone opts with the flag set only for it.
            let block_open_tail = !finalizing
                && bi == last_idx
                && tail_start + raw.range.end == self.buffer.len()
                && !buffer_ends_blank;
            let block_opts;
            let block_opts_ref: &RenderOpts = if block_open_tail {
                block_opts = RenderOpts { open_tail: true, ..opts.clone() };
                &block_opts
            } else {
                &opts
            };
            // render_block returns Some(Enrichment) only for a top-level block
            // with an opt-in payload (Table, Heading) when block_data is on —
            // fold it onto the matching `Option` carrier field. Off ⇒ None ⇒ kind
            // unchanged (byte-identical wire).
            match render_block(tail, raw, block_opts_ref, &mut html) {
                Some(Enrichment::Table(td)) => kind = BlockKind::Table(Some(td)),
                Some(Enrichment::Heading(h)) => {
                    kind = BlockKind::Heading { level: h.level, rich: Some(h) }
                }
                // CodeBlock keeps its classified `lang`; only `code` is folded on.
                Some(Enrichment::CodeBlock(code)) => {
                    if let BlockKind::CodeBlock { lang, .. } = kind {
                        kind = BlockKind::CodeBlock { lang, code: Some(code) };
                    }
                }
                Some(Enrichment::MathBlock(md)) => kind = BlockKind::MathBlock(Some(md)),
                // List keeps its classified `ordered`; `start` + per-item `items`
                // (inner `<li>` HTML) are folded on for the keyed renderer.
                Some(Enrichment::List(start, items)) => {
                    if let BlockKind::List { ordered, .. } = kind {
                        kind = BlockKind::List { ordered, start: Some(start), items };
                    }
                }
                Some(Enrichment::Blockquote(cd)) => kind = BlockKind::Blockquote(Some(cd)),
                // Alert keeps its classified `kind`; only `nested` is folded on.
                Some(Enrichment::Alert(cd)) => {
                    if let BlockKind::Alert { kind: ak, .. } = kind {
                        kind = BlockKind::Alert { kind: ak, nested: Some(cd) };
                    }
                }
                None => {}
            }
            produced.push(Block {
                id: 0,
                kind,
                start: tail_start + raw.range.start,
                end: tail_start + raw.range.end,
                html,
                open: false,
                speculative: false,
            });
        }

        // Resolve the placeholder-token footnote ids in every produced block, in
        // document order, seeded from the committed occurrence counts. The token
        // replay is the SOLE source of truth for advancing the occurrence map, so
        // after resolving all blocks `total_occ` holds the exact per-label
        // reference count (committed + tail) used for backref generation. (When
        // footnotes are off, no tokens exist and this is a cheap byte-copy.)
        //
        // `occ_after_block[i]` snapshots the occurrence map after resolving the
        // first i+1 produced blocks; the committed-region advance is read from it
        // once `to_commit` is known, so the persistent committed occurrence map is
        // advanced by exactly the COMMITTED blocks' real refs (never by `[^x]`
        // inside code spans / escaped text, which emit no token). `produced` is a
        // handful of top-level blocks (not proportional to table rows / list
        // items), so the per-block snapshot clone is cheap.
        let mut total_occ = self.committed_footnote_occurrences.clone();
        let mut occ_after_block: Vec<HashMap<String, usize>> = Vec::new();
        if gfm_footnotes {
            occ_after_block.reserve(produced.len());
            for block in &mut produced {
                resolve_block_footnotes(block, &mut total_occ);
                occ_after_block.push(total_occ.clone());
            }
        }
        // Definition bodies render at finalize, AFTER the total_occ snapshot, with
        // placeholder mode OFF (the live path): a def-body `[^x]` continues the
        // occurrence sequence past total_occ (matching the historical behavior)
        // and is intentionally not counted into the section's backref total.
        let mut fn_defs = self.committed_footnote_defs.clone();
        if gfm_footnotes {
            let defs_opts = RenderOpts {
                footnote_placeholder: false,
                footnote_occ: std::cell::RefCell::new(total_occ.clone()),
                ..opts.clone()
            };
            collect_footnote_defs(tail, &mut fn_defs, &defs_opts);
        }

        // `buffer_ends_blank` is computed above (for the per-block open_tail gate).
        let last_is_open_fence = renderable.last().map_or(false, |b| {
            matches!(
                b.kind,
                RawBlockKind::CodeFence { terminated: false, .. }
                    | RawBlockKind::MathFence { terminated: false }
                    | RawBlockKind::ComponentBlock { terminated: false, .. }
            )
        });
        // A trailing list, block quote, indented code, or open HTML block can
        // *resume* after a blank line (loose lists, lazy continuations, code
        // with interior blanks), so a single blank is not a safe commit
        // boundary for it — keep it speculative until a following block proves
        // it's closed (or we finalize). Otherwise streamed loose lists/code get
        // split where one-shot parsing keeps them whole.
        let is_resumable = |k: &RawBlockKind| {
            matches!(
                k,
                RawBlockKind::List { .. }
                    | RawBlockKind::Blockquote
                    | RawBlockKind::IndentedCode
                    | RawBlockKind::HtmlBlock { closed: false }
            )
        };
        let last_is_resumable_container = renderable.last().map_or(false, |b| is_resumable(&b.kind));
        let commit_all = finalizing
            || (buffer_ends_blank && !last_is_open_fence && !last_is_resumable_container);
        let n = renderable.len();
        let final_line_start = tail.rfind('\n').map_or(0, |i| i + 1);
        // A block whose start sits on the buffer's still-growing FINAL line (no
        // terminating newline yet) is only PROVISIONALLY classified: `#x`, `</p`,
        // or a lone `*` look like a Heading / type-6 HTML block / new list bullet
        // now, but dissolve into a lazy continuation of the previous block once
        // the line completes (`#hashtag`, `</pre>`, `*emph*`). Committing the
        // block BEFORE such a transient would freeze a split the FINALIZED
        // one-shot parse never makes. Bounded: it clears the moment any `\n`
        // arrives. (The Paragraph guard below is the special case where the
        // provisional block already classifies as a Paragraph; this generalizes
        // it to Heading/HtmlBlock/List/… block starts.)
        let last_starts_final_line = !finalizing
            && n >= 2
            && tail_start + renderable[n - 1].range.end == self.buffer.len()
            && !self.buffer.ends_with('\n')
            && !self.buffer.ends_with('\r')
            && renderable[n - 1].range.start >= final_line_start;
        // …and the line just before that final line must be non-blank, so the
        // final line can actually be a lazy continuation of it. A blank line
        // closes the previous block (`para\n\n#x` is two real paragraphs), and
        // holding `para` back across it would re-scan it every append — O(n²).
        let prev_line_nonblank = final_line_start > 0 && {
            let before = &tail[..final_line_start - 1];
            let prev_start = before.rfind('\n').map_or(0, |i| i + 1);
            !before[prev_start..].trim().is_empty()
        };
        let to_commit = if produced.is_empty() {
            0
        } else if commit_all {
            produced.len()
        } else if renderable[n - 1].range.end < raw_blocks.last().map_or(0, |r| r.range.end) {
            // The last renderable block is followed by a trailing run of
            // (non-renderable) link-ref / footnote definitions. A definition only
            // parses at a block boundary, so the renderable block is CLOSED — it
            // can't grow or merge backward — and must commit. Otherwise it never
            // becomes "the last block" (the defs aren't renderable), so it stays
            // speculative forever, stalling `committed_offset` and re-scanning the
            // whole growing def run on every append (the ref_heavy O(n²) cliff).
            produced.len()
        } else if n >= 2
            && ((matches!(renderable[n - 1].kind, RawBlockKind::Paragraph)
                && is_resumable(&renderable[n - 2].kind))
                || (last_starts_final_line
                    && prev_line_nonblank
                    && (matches!(renderable[n - 2].kind, RawBlockKind::Paragraph)
                        || is_resumable(&renderable[n - 2].kind))))
        {
            // A resumable container immediately followed by a paragraph may
            // still be mid-parse — the "paragraph" could be a partial list
            // marker or a lazy continuation that merges back into the
            // container once more bytes arrive — OR the trailing block is a
            // provisional marker on the unterminated final line that may lazily
            // continue a continuable penultimate. Keep both uncommitted.
            n - 2
        } else {
            produced.len() - 1
        };

        for block in &mut produced {
            let reuse = self
                .active_blocks
                .iter()
                .find(|prev| prev.start == block.start && prev.kind.tag() == block.kind.tag())
                .map(|prev| prev.id);
            block.id = reuse.unwrap_or_else(|| {
                let id = self.next_id;
                self.next_id += 1;
                id
            });
        }

        let mut new_active: Vec<Block> = produced.split_off(to_commit);
        let mut newly_committed: Vec<Block> = produced;

        for b in &mut newly_committed {
            b.open = false;
            b.speculative = false;
        }
        for b in &mut new_active {
            b.open = !finalizing;
            b.speculative = !finalizing;
        }

        // Advance committed_offset to the end of the last RAW block (which
        // may be a LinkRefDefinition we filtered out). This way ref defs
        // don't get re-scanned on the next append.
        let last_raw_end_to_commit = if renderable.is_empty() && !finalizing {
            // The tail is a pure run of non-renderable definition blocks (link-ref
            // and/or footnote defs) — it produces nothing renderable, so `to_commit`
            // is 0 and committed_offset would never advance, leaving the whole run
            // re-scanned and re-collected every append (O(n²) for a long reference
            // section). Commit every completed def but the last: a def's title can
            // arrive on the following line, so the trailing def stays speculative
            // until a later block proves it complete. (At finalize, the `commit_all`
            // walk below commits the whole run.) Routes through the
            // `last_raw_end_to_commit > 0` block so ref/footnote tables stay correct.
            if raw_blocks.len() >= 2 {
                raw_blocks[raw_blocks.len() - 2].range.end
            } else {
                0
            }
        } else if commit_all || raw_blocks.len() > to_commit.saturating_add(0) {
            // Walk the raw_blocks and find the boundary corresponding to our
            // commit decision. Concretely: after committing `to_commit`
            // renderable blocks, also include any trailing ref defs.
            let mut renderable_idx = 0;
            let mut boundary = 0;
            for raw in &raw_blocks {
                // Footnote defs are non-renderable too (when on), so the walk must
                // skip them exactly like link-ref defs or the index diverges.
                let non_renderable = matches!(raw.kind, RawBlockKind::LinkRefDefinition)
                    || is_footnote_def(&tail[raw.range.clone()]);
                if non_renderable {
                    if renderable_idx <= to_commit && commit_all {
                        boundary = raw.range.end;
                    } else if renderable_idx < to_commit {
                        boundary = raw.range.end;
                    }
                } else {
                    if renderable_idx < to_commit {
                        boundary = raw.range.end;
                        renderable_idx += 1;
                    } else if renderable_idx == to_commit && commit_all {
                        boundary = raw.range.end;
                        renderable_idx += 1;
                    } else {
                        break;
                    }
                }
            }
            boundary
        } else {
            0
        };
        if last_raw_end_to_commit > 0 {
            // The region [tail_start, new offset) just became permanent — fold its
            // (now-stable) footnote definitions into the committed tables and lock
            // in footnote numbers. The *link-ref* fold is deferred to the end of
            // this method: it mutates `committed_refs` via `Rc::make_mut`, which
            // must run after `opts` (which holds the shared `Rc` clone) is dropped,
            // so the table is mutated in place rather than copied.
            let committed_slice = &self.buffer[tail_start..tail_start + last_raw_end_to_commit];
            if gfm_footnotes {
                collect_footnote_refs(
                    committed_slice,
                    &mut self.committed_footnotes,
                    &mut self.next_footnote,
                );
                // BELT-AND-SUSPENDERS: advance the persistent committed occurrence
                // map from the RESOLVED token replay of the committed blocks, NOT
                // from `count_footnote_refs` over the raw committed slice. The raw
                // scan counts `[^x]` inside code spans and escaped `\[^x\]`, which
                // emit no ref token → it would over-count, shifting every later
                // suffix and breaking backrefs. `occ_after_block[to_commit-1]` is
                // the map after replaying exactly the committed blocks' real refs,
                // so seed == tokens by construction. `to_commit == 0` (nothing
                // renderable committed, only def blocks) leaves the map unchanged.
                if to_commit > 0 {
                    if let Some(snap) = occ_after_block.get(to_commit - 1) {
                        self.committed_footnote_occurrences = snap.clone();
                    }
                }
                // Committed def bodies render with placeholder mode OFF (they are
                // stored permanently and re-emitted in the section verbatim), with
                // the occurrence counter seeded PAST the committed refs so a
                // def-body `[^x]` continues the sequence (mirrors the historical
                // live path + the finalize def path).
                let commit_defs_opts = RenderOpts {
                    footnote_placeholder: false,
                    footnote_occ: std::cell::RefCell::new(
                        self.committed_footnote_occurrences.clone(),
                    ),
                    ..opts.clone()
                };
                collect_footnote_defs(
                    committed_slice,
                    &mut self.committed_footnote_defs,
                    &commit_defs_opts,
                );
            }
            self.committed_offset = tail_start + last_raw_end_to_commit;
        }

        // At finalize, emit the footnote section as a final block (once).
        if finalizing && gfm_footnotes {
            let section = render_footnote_section(&fn_nums, &fn_defs, &total_occ, opts.dir());
            if !section.is_empty() {
                let id = self.next_id;
                self.next_id += 1;
                newly_committed.push(Block {
                    id,
                    kind: BlockKind::Html,
                    start: self.buffer.len(),
                    end: self.buffer.len(),
                    html: section,
                    open: false,
                    speculative: false,
                });
            }
        }

        for b in newly_committed.iter().cloned() {
            self.committed_blocks.push(b);
        }
        self.active_blocks = new_active.clone();

        // Arm (or disarm) the tail fast-path caches. They apply only when the
        // entire tail is now a single open block whose kind streams cheaply —
        // an open code/math fence or an open paragraph — so subsequent appends
        // take the O(new bytes) path instead of re-rendering the whole tail.
        self.fence_cache = None;
        self.para_cache = None;
        self.table_cache = None;
        self.container_cache = None;
        self.list_cache = None;
        self.indented_cache = None;
        self.html_cache = None;
        if !finalizing && new_active.len() == 1 {
            let raw = renderable[to_commit];
            let start = tail_start + raw.range.start;
            let gap_blank = self.buffer.as_bytes()[self.committed_offset..start]
                .iter()
                .all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
            if gap_blank {
                match &raw.kind {
                    RawBlockKind::CodeFence { terminated: false, info, .. } => {
                        self.fence_cache = build_code_fence_cache(
                            &self.buffer,
                            start,
                            info,
                            new_active[0].id,
                            new_active[0].kind.clone(),
                        );
                    }
                    RawBlockKind::MathFence { terminated: false } => {
                        self.fence_cache = build_math_fence_cache(
                            &self.buffer,
                            start,
                            new_active[0].id,
                            new_active[0].kind.clone(),
                        );
                    }
                    RawBlockKind::Paragraph => {
                        self.para_cache = build_paragraph_cache(
                            &self.buffer,
                            start,
                            new_active[0].id,
                            &opts,
                            &self.committed_footnote_occurrences,
                        );
                    }
                    // Footnotes stay ARMED: the cache renders refs as
                    // occurrence-INDEPENDENT placeholder tokens (see
                    // `RenderOpts::footnote_placeholder`) and resolves them into
                    // the frozen prefix in document order from the committed
                    // occurrence baseline, so the cache vs. full-reparse boundary
                    // is byte-identical at O(new bytes) per append.
                    RawBlockKind::Table => {
                        self.table_cache = build_table_cache(
                            &self.buffer,
                            start,
                            new_active[0].id,
                            &opts,
                            &self.committed_footnote_occurrences,
                        );
                    }
                    RawBlockKind::Blockquote => {
                        self.container_cache = build_container_cache(
                            &self.buffer,
                            start,
                            new_active[0].id,
                            &new_active[0].kind,
                            &opts,
                            &self.committed_footnote_occurrences,
                        );
                    }
                    RawBlockKind::List { ordered, start: list_start_num } => {
                        self.list_cache = build_list_cache(
                            &self.buffer,
                            start,
                            new_active[0].id,
                            *ordered,
                            *list_start_num,
                            &opts,
                            &self.committed_footnote_occurrences,
                        );
                    }
                    RawBlockKind::IndentedCode => {
                        self.indented_cache =
                            build_indented_cache(&self.buffer, start, new_active[0].id);
                    }
                    RawBlockKind::HtmlBlock { closed: false } => {
                        self.html_cache =
                            build_html_cache(&self.buffer, start, new_active[0].id, &opts);
                    }
                    _ => {}
                }
            }
        }

        // Fold the just-committed link-ref definitions into the permanent table.
        // Deferred to here so `opts`'s shared `Rc` clone is dropped first — then
        // `Rc::make_mut` mutates the committed table in place (no per-append copy).
        drop(opts);
        if last_raw_end_to_commit > 0 {
            let committed_slice = &self.buffer[tail_start..tail_start + last_raw_end_to_commit];
            // The fold must mutate in place (no copy) to stay O(n): `opts` (the
            // only other `Rc` holder) was just dropped, so the count is 1. If a
            // future change stashes a clone of the committed table, this fires in
            // tests before the silent O(n²) regression ships.
            debug_assert_eq!(Rc::strong_count(&self.committed_refs), 1);
            collect_refs(committed_slice, Rc::make_mut(&mut self.committed_refs), ctx, 0);
        }

        Patch { newly_committed, active: new_active }
    }

    /// O(new bytes) extension of a long open code/math fence at the tail. Returns
    /// the patch directly on a cache hit; `None` falls through to the full reparse
    /// (and drops the cache) when the tail is no longer this plain open fence.
    fn try_incremental_fence(&mut self) -> Option<Patch> {
        let mut cache = self.fence_cache.take()?;
        // The fence must still be the tail: only whitespace may sit between the
        // committed boundary and the opener (normally they're equal).
        if cache.start < self.committed_offset
            || self.buffer.as_bytes()[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        let close = cache.close; // Copy, so the body push below can borrow cache.
        let bytes = self.buffer.as_bytes();
        let end = bytes.len();
        // Append newly-arrived complete lines to the cached body.
        let mut pos = cache.lines_upto;
        while pos < end {
            match bytes[pos..end].iter().position(|&b| b == b'\n') {
                None => break, // a partial line; handled below
                Some(r) => {
                    let content_end = pos + r;
                    let next = pos + r + 1;
                    // A closing line or CRLF: defer to the full renderer, which
                    // gets the close / whitespace-trim / `\r` handling exactly right.
                    let is_close = match close {
                        FenceClose::CodeFence => is_fence_close_line(&bytes[pos..next]),
                        FenceClose::MathCloser(c) => line_contains(&bytes[pos..content_end], c),
                    };
                    if bytes[pos..content_end].contains(&b'\r') || is_close {
                        return None;
                    }
                    if !cache.escaped_lines.is_empty() {
                        cache.escaped_lines.push('\n');
                    }
                    escape_html(
                        std::str::from_utf8(&bytes[pos..content_end]).unwrap_or(""),
                        &mut cache.escaped_lines,
                    );
                    cache.lines_upto = next;
                    pos = next;
                }
            }
        }
        // The trailing partial line is re-escaped each append (it is short).
        let partial = &bytes[cache.lines_upto..end];
        let partial_is_close = match close {
            FenceClose::CodeFence => is_fence_close_line(partial),
            FenceClose::MathCloser(c) => line_contains(partial, c),
        };
        if partial.contains(&b'\r') || partial_is_close {
            return None;
        }
        // Assemble the block HTML directly from the cached pieces — no clone of
        // the (growing) escaped body. For code: opener + body[+ "\n" + partial]
        // + "\n" + close. For math: opener + trim_end(body[+ partial]) + close
        // (math trims the body's surrounding whitespace; leading whitespace is
        // already dropped at arm time via the body-start skip).
        let mut html = String::with_capacity(
            cache.opener_html.len() + cache.escaped_lines.len() + partial.len() + 32,
        );
        html.push_str(&cache.opener_html);
        let body_start = html.len();
        html.push_str(&cache.escaped_lines);
        let lines_nonempty = !cache.escaped_lines.is_empty();
        if !partial.is_empty() {
            if lines_nonempty {
                html.push('\n');
            }
            escape_html(std::str::from_utf8(partial).unwrap_or(""), &mut html);
        }
        if cache.trim_body {
            // Whitespace bytes survive escape_html unchanged, so trimming the
            // escaped output equals trimming the source body.
            let trimmed = html.trim_end_matches([' ', '\t', '\n', '\r']).len();
            html.truncate(trimmed.max(body_start));
        } else if lines_nonempty || !partial.is_empty() {
            html.push('\n');
        }
        // Opt-in structured channel: recover the decoded source from the just-
        // assembled, already-trimmed HTML body (`html[body_start..]`, before the
        // closer) by inverting `escape_html`. This makes the streamed `kind.data`
        // byte-identical to the full path / `decodeCodeText`/`decodeMathText`, with
        // no parallel raw-source state in the cache. Off (or for a Mermaid fence,
        // which carries no enrichment) ⇒ the frozen `cache.kind` is reused as-is.
        let kind = if self.block_data {
            let src = crate::render::unescape_html_body(&html[body_start..]);
            match &cache.kind {
                BlockKind::CodeBlock { lang, .. } => {
                    BlockKind::CodeBlock { lang: lang.clone(), code: Some(src) }
                }
                BlockKind::MathBlock(_) => {
                    BlockKind::MathBlock(Some(crate::blocks::MathBlockData { latex: src }))
                }
                other => other.clone(),
            }
        } else {
            cache.kind.clone()
        };
        html.push_str(cache.closer_html);
        let block = Block {
            id: cache.id,
            kind,
            start: cache.start,
            end,
            html,
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.fence_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block] })
    }

    /// O(new bytes) extension of a long open indented-code block at the tail.
    /// Folds each newly-complete ≥4-indent line into the cached body and
    /// re-renders only the trailing partial. Returns `None` (dropping the cache)
    /// the moment the block ends or is no longer the sole open tail — a dedent,
    /// a blank line, or a `\r` — and the full reparse takes over.
    fn try_incremental_indented(&mut self) -> Option<Patch> {
        let mut cache = self.indented_cache.take()?;
        // The block must still be the tail: only whitespace may sit between the
        // committed boundary and the opener (normally they're equal).
        if cache.start < self.committed_offset
            || self.buffer.as_bytes()[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        let bytes = self.buffer.as_bytes();
        let end = bytes.len();
        // Append newly-arrived complete lines to the cached body.
        let mut pos = cache.lines_upto;
        while pos < end {
            match bytes[pos..end].iter().position(|&b| b == b'\n') {
                None => break, // a partial line; handled below
                Some(r) => {
                    let content_end = pos + r;
                    let next = pos + r + 1;
                    // A dedent / blank / CRLF line: defer to the full renderer,
                    // which owns interior-blank accounting and the exact block end.
                    if bytes[pos..content_end].contains(&b'\r')
                        || !indented_code_line(&bytes[pos..content_end])
                    {
                        return None;
                    }
                    if !cache.escaped_lines.is_empty() {
                        cache.escaped_lines.push('\n');
                    }
                    push_indented_content(&bytes[pos..content_end], &mut cache.escaped_lines);
                    cache.lines_upto = next;
                    pos = next;
                }
            }
        }
        // The trailing partial line is re-rendered each append (it is short). An
        // all-whitespace partial contributes nothing (it is a blank-so-far line
        // the full renderer would not yet absorb); a partial that already dedents
        // (content before column 4) ends the block — bail.
        let partial = &bytes[cache.lines_upto..end];
        if partial.contains(&b'\r') {
            return None;
        }
        let partial_blank = partial.iter().all(|&b| matches!(b, b' ' | b'\t'));
        if !partial_blank && !indented_code_line(partial) {
            return None;
        }
        // Assemble: <pre><code> + trim_end(body[+ "\n" + partial]) + "\n" +
        // </code></pre>. Whitespace survives escape_html unchanged, so trimming
        // the escaped output equals trimming the decoded source — exactly what
        // render_indented_code does.
        let mut html = String::with_capacity(
            cache.escaped_lines.len() + partial.len() + 32,
        );
        html.push_str("<pre><code>");
        let body_start = html.len();
        html.push_str(&cache.escaped_lines);
        if !partial_blank {
            if !cache.escaped_lines.is_empty() {
                html.push('\n');
            }
            push_indented_content(partial, &mut html);
        }
        let trimmed = html.trim_end_matches([' ', '\t', '\n', '\r']).len();
        html.truncate(trimmed.max(body_start));
        // Opt-in structured channel: the decoded source is the trimmed body + "\n",
        // recovered by inverting escape_html — byte-identical to the full path.
        let kind = if self.block_data {
            let mut code = crate::render::unescape_html_body(&html[body_start..]);
            code.push('\n');
            BlockKind::CodeBlock { lang: None, code: Some(code) }
        } else {
            BlockKind::CodeBlock { lang: None, code: None }
        };
        html.push('\n');
        html.push_str("</code></pre>");
        let block = Block {
            id: cache.id,
            kind,
            start: cache.start,
            end,
            html,
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.indented_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block] })
    }

    /// O(new bytes) extension of a long open raw-HTML block at the tail. Folds
    /// each newly-complete line into the cached prefix (pass-through or escaped)
    /// and re-processes only the trailing partial. Returns `None` (dropping the
    /// cache) the moment the block's type-specific close condition is met (so the
    /// full reparse closes + commits it), or on a `\r`.
    fn try_incremental_html(&mut self) -> Option<Patch> {
        let mut cache = self.html_cache.take()?;
        // The pass-through decision must still hold (options don't change mid-
        // stream, but stay defensive: a changed setting voids the cache).
        if cache.pass_through != (self.unsafe_html && !self.html_sanitize) {
            return None;
        }
        // The block must still be the tail (only whitespace before the opener).
        if cache.start < self.committed_offset
            || self.buffer.as_bytes()[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        let html_type = cache.html_type;
        let bytes = self.buffer.as_bytes();
        let end = bytes.len();
        let mut pos = cache.lines_upto;
        while pos < end {
            match bytes[pos..end].iter().position(|&b| b == b'\n') {
                None => break, // a partial line; handled below
                Some(r) => {
                    let content_end = pos + r;
                    let next = pos + r + 1;
                    let line = &bytes[pos..next];
                    // The closing line (types 1–5) or a blank line (types 6/7)
                    // ends the block — defer to the full renderer to close + commit
                    // it. A `\r` also bails (CRLF goes through the full path).
                    if html_block_closes_here(line, html_type, &bytes[pos..content_end]) {
                        return None;
                    }
                    fold_html_line(&bytes[pos..next], cache.pass_through, &mut cache.cached_prefix);
                    cache.lines_upto = next;
                    pos = next;
                }
            }
        }
        // The trailing partial line is re-processed each append (it is short). It
        // ends the block iff it satisfies the close condition — bail then.
        let partial = &bytes[cache.lines_upto..end];
        if html_block_closes_here(partial, html_type, partial) {
            return None;
        }
        let mut html = String::with_capacity(cache.cached_prefix.len() + partial.len() + 32);
        if cache.pass_through {
            // Pass-through: prefix + partial verbatim, trailing newlines trimmed,
            // then a single `\n` (matches render_html_block's pass-through).
            html.push_str(&cache.cached_prefix);
            html.push_str(std::str::from_utf8(partial).unwrap_or(""));
            let trimmed = html.trim_end_matches(['\n', '\r']).len();
            html.truncate(trimmed);
            html.push('\n');
        } else {
            // Escaped: <pre><code> + escape_html(prefix + partial) + </code></pre>.
            // The prefix is already escaped; only the partial needs escaping now.
            html.push_str("<pre><code>");
            html.push_str(&cache.cached_prefix);
            escape_html(std::str::from_utf8(partial).unwrap_or(""), &mut html);
            html.push_str("</code></pre>");
        }
        let block = Block {
            id: cache.id,
            kind: BlockKind::Html,
            start: cache.start,
            end,
            html,
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.html_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block] })
    }

    /// Inline-render options for a streaming tail render. Reference + footnote
    /// tables come from the committed region (an open block defines none of its
    /// own); footnote numbers continue from the committed count over
    /// `footnote_region`, mirroring the full path's pre-pass.
    fn build_inline_opts(&self, footnote_region: &str) -> RenderOpts {
        let mut footnotes = self.committed_footnotes.clone();
        if self.gfm_footnotes {
            let mut next = self.next_footnote;
            collect_footnote_refs(footnote_region, &mut footnotes, &mut next);
        }
        RenderOpts {
            unsafe_html: self.unsafe_html,
            // O(1) Rc share of the committed table; an open paragraph defines no
            // refs of its own, so there are no tail refs to layer.
            committed_refs: Rc::clone(&self.committed_refs),
            tail_refs: HashMap::new(),
            in_link: false,
            // This opts backs the streaming tail caches (paragraph / table /
            // container / list), which render the still-open, abuts-EOF active
            // tail. Speculate incomplete open-tail links here so a streaming
            // `[label](url…` shows an inert `<a>` instead of flashing the URL.
            open_tail: true,
            gfm_autolinks: self.gfm_autolinks,
            gfm_alerts: self.gfm_alerts,
            gfm_math: self.gfm_math,
            dir_auto: self.dir_auto,
            a11y: self.a11y,
            block_data: self.block_data,
            gfm_footnotes: self.gfm_footnotes,
            footnotes,
            footnote_occ: std::cell::RefCell::new(self.committed_footnote_occurrences.clone()),
            // Cache fold + builders render footnote refs as placeholder tokens
            // when footnotes are on (occurrence-independent → safe to freeze);
            // the caller resolves them on commit (frozen prefix) or per-append
            // from a clone (speculative tail). No-op when footnotes are off.
            footnote_placeholder: self.gfm_footnotes,
            component_tags: self.component_tags.clone(),
            inline_component_tags: self.inline_component_tags.clone(),
            html_sanitize: self.html_sanitize,
            html_allowlist: self.html_allowlist.clone(),
            html_drop: self.html_drop.clone(),
        }
    }

    /// O(new bytes) extension of a long open paragraph at the tail. Commits the
    /// blocker-free plain prefix once and re-renders only the short active tail.
    /// Returns `None` (dropping the cache) whenever the paragraph has ended or
    /// is no longer the sole tail block — the full reparse then handles it.
    fn try_incremental_paragraph(&mut self) -> Option<Patch> {
        let mut cache = self.para_cache.take()?;
        let ctx = ScanCtx {
            math: self.gfm_math,
            component_tags: &self.component_tags,
            inline_component_tags: &self.inline_component_tags,
        };
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        // The paragraph must still be the tail (only whitespace before it) and
        // must still run to EOF (no blank line / interrupting block / setext
        // underline appeared after the committed cut).
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
            || paragraph_ends_before_eof(bytes, cache.cut, ctx)
        {
            return None;
        }
        let mut content_end = len;
        while content_end > cache.start && matches!(bytes[content_end - 1], b'\n' | b'\r') {
            content_end -= 1;
        }
        if content_end < cache.cut {
            return None;
        }
        let opts = self.build_inline_opts(&self.buffer[cache.start..content_end]);
        // Render the active region and learn how far of it is now settled — past
        // closed emphasis / code spans / inline links, but not an unpaired opener
        // or unclosed construct. `boundary_rel` is relative to the active slice.
        let mut active = String::new();
        let boundary_rel =
            render_inline_boundary(&self.buffer[cache.cut..content_end], &opts, &mut active);
        let new_cut = cache.cut + boundary_rel;
        if new_cut > cache.cut {
            // Commit [cut..new_cut] by rendering that segment on its own — a clean
            // boundary guarantees it equals its slice of the full render — then
            // re-render the now-shorter active tail. Resolve the just-committed
            // segment's footnote tokens into `committed_inner`, advancing the
            // frozen-prefix occurrence map (resolve-on-commit; never re-touched).
            let mut seg = String::new();
            render_inline(&self.buffer[cache.cut..new_cut], &opts, &mut seg);
            resolve_footnote_ids(&seg, &mut cache.fn_occ, &mut cache.committed_inner);
            cache.cut = new_cut;
            active.clear();
            render_inline(&self.buffer[cache.cut..content_end], &opts, &mut active);
        }
        // Resolve the speculative active tail per-append from a CLONE of the
        // frozen-prefix occurrence map (does NOT advance persistent state). No-op
        // byte-copy when footnotes are off (no tokens present).
        if self.gfm_footnotes {
            let mut occ = cache.fn_occ.clone();
            let mut resolved = String::with_capacity(active.len());
            resolve_footnote_ids(&active, &mut occ, &mut resolved);
            active = resolved;
        }
        // Assemble in a single buffer with 1× memcpy of `committed_inner` (was
        // 2× via an intermediate `inner` String). Matches `render_paragraph`'s
        // `<p…>` opener and trailing trim.
        let mut html = String::with_capacity(
            cache.committed_inner.len() + active.len() + opts.dir().len() + 8,
        );
        html.push_str("<p");
        html.push_str(opts.dir());
        html.push('>');
        let body_start = html.len();
        html.push_str(&cache.committed_inner);
        html.push_str(&active);
        while html.len() > body_start
            && matches!(
                html.as_bytes()[html.len() - 1],
                b' ' | b'\t' | b'\n' | b'\r'
            )
        {
            html.pop();
        }
        html.push_str("</p>");
        let block = Block {
            id: cache.id,
            kind: BlockKind::Paragraph,
            start: cache.start,
            end: len,
            html,
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.para_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block] })
    }

    /// O(new bytes) extension of a long open GFM table at the tail. Folds each
    /// newly-complete body row into the cached prefix; speculatively renders
    /// the trailing partial line as the last row. Returns `None` (dropping the
    /// cache) whenever the table has ended (blank line, interrupting block, or
    /// a `\r` line that the full path handles) or is no longer the sole tail
    /// block — the full reparse then handles it.
    fn try_incremental_table(&mut self) -> Option<Patch> {
        let mut cache = self.table_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let end = bytes.len();
        // Must still be at the tail (only whitespace before it).
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        let ctx = ScanCtx {
            math: self.gfm_math,
            component_tags: &self.component_tags,
            inline_component_tags: &self.inline_component_tags,
        };
        // Build inline opts once for the whole append: the same shared RenderOpts
        // backs cached-row rendering and the speculative partial-row render. The
        // open table's OWN cells may carry the first reference to a footnote
        // label, so the footnote-numbering pre-pass must see the table content
        // (mirrors the full path, which numbers refs over every renderable block).
        let opts = self.build_inline_opts(&self.buffer[cache.start..end]);

        // Fold every newly-complete body row into the cache. A blank/interrupting
        // line bails: the table has ended, full reparse takes over so the block
        // boundary updates correctly.
        let mut pos = cache.lines_upto;
        while pos < end {
            let r = match bytes[pos..end].iter().position(|&b| b == b'\n') {
                None => break, // trailing partial line — handled below
                Some(r) => r,
            };
            let content_end = pos + r;
            let next = pos + r + 1;
            // The cache stores LF-only state; CRLF rows route through the full
            // renderer (same fallback strategy as `FenceCache`).
            if bytes[pos..content_end].contains(&b'\r') {
                return None;
            }
            if is_blank_line(bytes, pos) || would_start_other_block(bytes, pos, ctx) {
                return None;
            }
            let line_str = std::str::from_utf8(&bytes[pos..content_end]).unwrap_or("");
            let cells = split_table_cells(line_str);
            if !cache.tbody_opened {
                cache.cached_prefix.push_str("<tbody>");
                cache.tbody_opened = true;
            }
            // Render the row into a scratch buffer (placeholder tokens when
            // footnotes on), then resolve its tokens into `cached_prefix`,
            // advancing the cache-local occurrence map. Once folded the row is
            // never re-rendered (frozen-prefix invariant). The data-channel cells
            // resolve from a CLONE captured before the row advances `fn_occ`.
            let data_seed = cache.fn_occ.clone();
            let mut row_html = String::with_capacity(line_str.len() + 16);
            row_html.push_str("<tr>");
            let mut row: Vec<TableCell> = Vec::new();
            for i in 0..cache.ncol {
                let cell = push_table_cell(
                    "td",
                    cells.get(i).map(String::as_str).unwrap_or(""),
                    cache.aligns.get(i),
                    &opts,
                    &mut row_html,
                );
                if let Some(c) = cell {
                    row.push(c);
                }
            }
            row_html.push_str("</tr>");
            resolve_footnote_ids(&row_html, &mut cache.fn_occ, &mut cache.cached_prefix);
            // Structured channel: fold this committed row's cells in lock-step
            // with its `<tr>` — once folded it's never re-rendered (HTML invariant).
            if opts.block_data {
                if opts.gfm_footnotes {
                    let mut occ = data_seed;
                    for c in &mut row {
                        let mut o = String::with_capacity(c.html.len());
                        resolve_footnote_ids(&c.html, &mut occ, &mut o);
                        c.html = o;
                    }
                }
                cache.body_cells.push(Rc::new(row));
            }
            cache.lines_upto = next;
            pos = next;
        }

        // Speculatively render the trailing partial line (no `\n`) as a row, if
        // it's non-empty and not blank. The full renderer treats a final
        // newline-less line as the last row, so we must too. The partial is short
        // (≤ one row's worth), so re-rendering it each append is O(row).
        let partial = &bytes[cache.lines_upto..end];
        let mut partial_html = String::new();
        // Structured channel: the speculative partial row's cells, built parallel
        // to `partial_html` and NOT folded into `cache.body_cells` (mirrors how
        // `partial_html` is not folded into `cached_prefix`).
        let mut partial_row: Option<Vec<TableCell>> = None;
        if !partial.is_empty() && !is_blank_line(bytes, cache.lines_upto) {
            if partial.contains(&b'\r') {
                return None;
            }
            let line_str = std::str::from_utf8(partial).unwrap_or("");
            let cells = split_table_cells(line_str);
            let mut raw_partial = String::with_capacity(line_str.len() + 16);
            raw_partial.push_str("<tr>");
            let mut row: Vec<TableCell> = Vec::new();
            for i in 0..cache.ncol {
                let cell = push_table_cell(
                    "td",
                    cells.get(i).map(String::as_str).unwrap_or(""),
                    cache.aligns.get(i),
                    &opts,
                    &mut raw_partial,
                );
                if let Some(c) = cell {
                    row.push(c);
                }
            }
            raw_partial.push_str("</tr>");
            // Resolve the speculative partial row from a CLONE of the frozen-prefix
            // occurrence map (does NOT advance it). Byte-copy when footnotes off.
            let mut occ = cache.fn_occ.clone();
            resolve_footnote_ids(&raw_partial, &mut occ, &mut partial_html);
            if opts.block_data {
                if opts.gfm_footnotes {
                    let mut docc = cache.fn_occ.clone();
                    for c in &mut row {
                        let mut o = String::with_capacity(c.html.len());
                        resolve_footnote_ids(&c.html, &mut docc, &mut o);
                        c.html = o;
                    }
                }
                partial_row = Some(row);
            }
        }

        // Assemble final HTML: cached_prefix [+ "<tbody>" if first row is partial]
        // + partial_html + "</tbody>" (if any body row at all) + "</table>".
        let need_tbody_for_partial = !cache.tbody_opened && !partial_html.is_empty();
        let mut html = String::with_capacity(
            cache.cached_prefix.len() + partial_html.len() + 32,
        );
        html.push_str(&cache.cached_prefix);
        if need_tbody_for_partial {
            html.push_str("<tbody>");
        }
        html.push_str(&partial_html);
        if cache.tbody_opened || need_tbody_for_partial {
            html.push_str("</tbody>");
        }
        html.push_str("</table>");

        // Structured channel: assemble TableData = header + committed body rows +
        // the speculative partial row (if any), exactly mirroring the HTML the
        // consumer renders. emit-on-every-patch so DATA never lags HTML.
        let kind = if opts.block_data {
            // O(rows) Rc refcount bumps, not an O(cells) String deep clone.
            let mut rows = cache.body_cells.clone();
            if let Some(row) = partial_row {
                rows.push(Rc::new(row));
            }
            BlockKind::Table(Some(TableData {
                headers: cache.header_cells.clone(),
                rows,
                aligns: cache.aligns.clone(),
            }))
        } else {
            BlockKind::Table(None)
        };

        let block = Block {
            id: cache.id,
            kind,
            start: cache.start,
            end,
            html,
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.table_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block] })
    }

    /// O(new bytes) extension of a long open blockquote / alert at the tail.
    /// Strips the `>` marker from new lines into `inner_buffer` for the open
    /// paragraph, runs the paragraph-cache-style inline-boundary commit on
    /// its inner, and re-renders only the unsettled tail. A blank `>` line
    /// closes the current paragraph into `committed_paras_html` (rendered
    /// once, never re-rendered) and starts a fresh one. Returns `None`
    /// (dropping the cache) on a non-`>` line (lazy continuation or
    /// end-of-container) or `\r`.
    fn try_incremental_container(&mut self) -> Option<Patch> {
        let mut cache = self.container_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let end = bytes.len();
        // Tail-only check (same as the other caches).
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }

        // Inline opts — built once, shared by the close-paragraph render and the
        // per-append boundary pass. The open container's inner may carry the first
        // reference to a footnote label, so the numbering pre-pass scans the whole
        // container region (the `>` markers don't break `[^label]` matching), so
        // in-container refs get the same number as the full path assigns.
        let opts = self.build_inline_opts(&self.buffer[cache.start..end]);

        // Fold every newly-complete `> `-marker line. A blank `>` line closes
        // the current paragraph (rendered once into `committed_paras_html`)
        // and starts a fresh one; any other line is folded into the current
        // paragraph's `inner_buffer`. Bails on `\r` or a non-`>` line.
        let mut pos = cache.lines_upto;
        while pos < end {
            let r = match bytes[pos..end].iter().position(|&b| b == b'\n') {
                None => break, // trailing partial — handled below
                Some(r) => r,
            };
            let content_end = pos + r;
            let next = pos + r + 1;
            if bytes[pos..content_end].contains(&b'\r') {
                return None;
            }
            let stripped = strip_blockquote_marker(&bytes[pos..content_end])?;
            if stripped.iter().all(|&b| matches!(b, b' ' | b'\t')) {
                // Blank `>` line → close the current paragraph (if any).
                // Consecutive blanks collapse: nothing to push when the
                // current paragraph is empty.
                if !cache.inner_buffer.is_empty() {
                    close_container_paragraph(&mut cache, &opts);
                }
                cache.lines_upto = next;
                pos = next;
                continue;
            }
            // This cache renders inner content as PLAIN PARAGRAPHS only. If a line
            // would start a different block (a list, nested blockquote, heading,
            // fence, thematic break, HTML, …), bail to the full reparse, which
            // renders the inner block structure — otherwise the streamed
            // blockquote/alert shows the inner list/quote as escaped paragraph
            // text until finalize (a structural flicker).
            if container_inner_breaks_paragraph(stripped, opts.scan_ctx()) {
                return None;
            }
            let stripped_str = std::str::from_utf8(stripped).ok()?;
            cache.inner_buffer.push_str(stripped_str);
            cache.inner_buffer.push('\n');
            cache.lines_upto = next;
            pos = next;
        }

        // Speculatively extract the trailing partial line's stripped content,
        // if it already has a `>` marker. The partial extends the open inner
        // paragraph by ≤ one line — we push it onto `inner_buffer` for the
        // boundary + render passes, then truncate it back so future appends
        // see the same committed state.
        let partial = &bytes[cache.lines_upto..end];
        let mut partial_pushed = 0usize;
        if !partial.is_empty() {
            if partial.contains(&b'\r') {
                return None;
            }
            if let Some(stripped) = strip_blockquote_marker(partial) {
                // A leading `>` with only whitespace after it is the prefix of
                // a maybe-blank inner line — stay safe and render with what we
                // have committed so far.
                if !stripped.is_empty()
                    && !stripped.iter().all(|&b| matches!(b, b' ' | b'\t'))
                {
                    // Same guard as the committed lines: a partial inner line that
                    // already looks like a block start (e.g. `> -`) must not render
                    // as paragraph text — bail to the full reparse.
                    if container_inner_breaks_paragraph(stripped, opts.scan_ctx()) {
                        return None;
                    }
                    let stripped_str = std::str::from_utf8(stripped).ok()?;
                    cache.inner_buffer.push_str(stripped_str);
                    partial_pushed = stripped_str.len();
                }
            } else {
                // No `>` marker yet on the partial — could still become one as
                // more bytes arrive (e.g. just `>` or leading spaces). Render
                // with what we have committed so far.
            }
        }
        let post_partial_len = cache.inner_buffer.len();
        let committed_inner_end = post_partial_len - partial_pushed;

        // Render boundary on the full active region (committed-tail + partial)
        // for the CURRENT paragraph only. Closed paragraphs are fully settled
        // in `committed_paras_html` and never re-rendered.
        let mut active_html = String::new();
        let boundary_rel = render_inline_boundary(
            &cache.inner_buffer[cache.inner_cut..],
            &opts,
            &mut active_html,
        );
        let new_cut = (cache.inner_cut + boundary_rel).min(committed_inner_end);
        if new_cut > cache.inner_cut {
            let mut seg = String::new();
            render_inline(&cache.inner_buffer[cache.inner_cut..new_cut], &opts, &mut seg);
            // Resolve the just-settled segment into the open paragraph's frozen
            // prefix, advancing its (discard-on-close) occurrence sub-map.
            resolve_footnote_ids(&seg, &mut cache.inner_fn_occ, &mut cache.committed_inner_html);
            cache.inner_cut = new_cut;
            active_html.clear();
            render_inline(&cache.inner_buffer[cache.inner_cut..], &opts, &mut active_html);
        }
        // Resolve the speculative active tail from a CLONE of the open paragraph's
        // occurrence sub-map (does NOT advance it). Byte-copy when footnotes off.
        if self.gfm_footnotes {
            let mut occ = cache.inner_fn_occ.clone();
            let mut resolved = String::with_capacity(active_html.len());
            resolve_footnote_ids(&active_html, &mut occ, &mut resolved);
            active_html = resolved;
        }

        // Assemble in a single buffer with 1× memcpy of every committed
        // paragraph and `committed_inner_html`. Trailing whitespace is trimmed
        // in-place against the CURRENT paragraph's content only; an empty
        // current paragraph has its `<p>` opener backed out so the output
        // matches the full renderer (no `<p></p>`).
        let mut html = String::with_capacity(
            cache.wrapper_open.len()
                + cache.committed_paras_html.len()
                + cache.body_p_open.len()
                + cache.committed_inner_html.len()
                + active_html.len()
                + cache.body_p_close.len()
                + cache.wrapper_close.len(),
        );
        html.push_str(&cache.wrapper_open);
        html.push_str(&cache.committed_paras_html);
        let body_p_start = html.len();
        html.push_str(&cache.body_p_open);
        let body_content_start = html.len();
        html.push_str(&cache.committed_inner_html);
        html.push_str(&active_html);
        // Trim trailing whitespace from the current paragraph's content.
        while html.len() > body_content_start
            && matches!(
                html.as_bytes()[html.len() - 1],
                b' ' | b'\t' | b'\n' | b'\r'
            )
        {
            html.pop();
        }
        // Structured channel: the current (still-open) paragraph's own HTML, if it
        // has content — captured from the just-assembled bytes so it is
        // byte-identical to the wrapper's last `<p>…</p>`. Built before the
        // wrapper close / opener-backout so the slice is exactly the open paragraph.
        let open_para_html: Option<String> = if opts.block_data && html.len() > body_content_start {
            let mut p = String::with_capacity(html.len() - body_p_start + 4);
            p.push_str(&html[body_p_start..]);
            p.push_str("</p>");
            Some(p)
        } else {
            None
        };
        if html.len() == body_content_start {
            // Empty current paragraph → back out the `<p>` opener (matches
            // the full renderer, which emits no body sub-block for an empty
            // inner — true whether or not closed paragraphs precede it).
            html.truncate(body_p_start);
        } else {
            html.push_str(&cache.body_p_close);
        }
        html.push_str(&cache.wrapper_close);

        // Drop the speculative partial bytes so the cache's committed state is
        // unchanged for the next append.
        cache.inner_buffer.truncate(committed_inner_end);

        // Assemble the opt-in `nested` channel: the stable committed paragraphs
        // (O(paras) clone of cheap entries) plus the current open paragraph.
        // emit-on-every-patch so DATA never lags HTML, mirroring the table cache.
        let container_data = if opts.block_data {
            let mut nested = cache.committed_paras.clone();
            if let Some(p) = open_para_html {
                nested.push(NestedBlock { html: p });
            }
            Some(ContainerData { nested })
        } else {
            None
        };
        let kind = match cache.kind {
            ContainerCacheKind::Blockquote => BlockKind::Blockquote(container_data),
            ContainerCacheKind::Alert(ak) => BlockKind::Alert { kind: ak, nested: container_data },
        };
        let block = Block {
            id: cache.id,
            kind,
            start: cache.start,
            end,
            html,
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.container_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block] })
    }

    /// O(new bytes) extension of a long open flat list at the tail. Each
    /// newly-complete sibling line renders directly as `<li>…</li>` (tight or
    /// loose, per `cache.loose`) folded into `cached_prefix`; the trailing
    /// partial-marker line renders speculatively. A blank line between two
    /// siblings flips the list to loose (§5.3) and triggers a one-time
    /// O(items so far) rebuild — sticky once set. The cache bails on a
    /// non-sibling-marker line, foreign family / over-edge marker, or `\r`.
    fn try_incremental_list(&mut self) -> Option<Patch> {
        let mut cache = self.list_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let end = bytes.len();
        // Tail-only check.
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        // The open list's OWN items may carry the first reference to a footnote
        // label, so the numbering pre-pass scans the whole list region (mirrors
        // the full path).
        let opts = self.build_inline_opts(&self.buffer[cache.start..end]);

        // Helper: a marker line `line` qualifies as a SIBLING of this list. A
        // marker indented to/past the first item's content column nests a
        // sub-list (it is NOT a sibling) — returning false there makes the caller
        // bail to the full reparse, which renders the nesting correctly. Using
        // `<= edge + 3` here flattened 2-space-indented sub-bullets into siblings.
        let sibling_match = |m: &MarkerScan, cache: &ListCache| {
            m.ordered == cache.ordered
                && m.delim == cache.delim
                && m.marker_indent < cache.content_indent
        };

        // Fold every newly-complete sibling line into `cached_prefix`. Any
        // unrecoverable shape drops the cache so the full reparse handles
        // nested / lazy / multi-block items.
        let mut pos = cache.lines_upto;
        'outer: while pos < end {
            let r = match bytes[pos..end].iter().position(|&b| b == b'\n') {
                None => break, // trailing partial — handled below
                Some(r) => r,
            };
            let content_end = pos + r;
            let next = pos + r + 1;
            if bytes[pos..content_end].contains(&b'\r') {
                return None;
            }
            let line = &bytes[pos..content_end];

            if line.iter().all(|&b| matches!(b, b' ' | b'\t')) {
                // Blank between siblings = loose (§5.3). Look ahead past
                // further blanks for a sibling-marker line, transition if
                // needed, then resume the outer loop at that marker.
                let mut look = next;
                loop {
                    if look >= end {
                        // Trailing blanks only. Stay armed; the next chunk
                        // brings either more content (we'll re-scan) or
                        // finalize (full path takes over).
                        break 'outer;
                    }
                    let r2 = match bytes[look..end].iter().position(|&b| b == b'\n') {
                        None => {
                            // Trailing non-blank without `\n` — a partial line
                            // after one or more blank lines. If it's already a
                            // sibling marker, the list IS loose: that decision
                            // is settled by the blank+marker pair even though
                            // the marker body isn't complete. Skip past the
                            // blanks and let the partial section render the
                            // trailing marker.
                            let tail = &bytes[look..end];
                            if tail.contains(&b'\r') {
                                return None;
                            }
                            if tail.iter().all(|&b| matches!(b, b' ' | b'\t')) {
                                // Only whitespace; no marker visible yet.
                                break 'outer;
                            }
                            let m = scan_marker(tail)?;
                            if !sibling_match(&m, &cache) {
                                return None;
                            }
                            if !cache.loose {
                                rebuild_loose(&mut cache, bytes, &opts)?;
                            }
                            cache.lines_upto = look;
                            break 'outer;
                        }
                        Some(r2) => r2,
                    };
                    let look_end = look + r2;
                    let look_next = look + r2 + 1;
                    if bytes[look..look_end].contains(&b'\r') {
                        return None;
                    }
                    let look_line = &bytes[look..look_end];
                    if look_line.iter().all(|&b| matches!(b, b' ' | b'\t')) {
                        look = look_next;
                        continue;
                    }
                    let m = scan_marker(look_line)?;
                    if !sibling_match(&m, &cache) {
                        return None;
                    }
                    if !cache.loose {
                        rebuild_loose(&mut cache, bytes, &opts)?;
                    }
                    fold_item_line(
                        look_line,
                        &m,
                        true,
                        &opts,
                        &mut cache.cached_prefix,
                        Some(&mut cache.item_html),
                        &mut cache.fn_occ,
                    )?;
                    cache.lines_upto = look_next;
                    cache.items.push((look, look_end));
                    pos = look_next;
                    continue 'outer;
                }
            }

            let m = scan_marker(line)?;
            if !sibling_match(&m, &cache) {
                return None;
            }
            let loose = cache.loose;
            fold_item_line(
                line,
                &m,
                loose,
                &opts,
                &mut cache.cached_prefix,
                Some(&mut cache.item_html),
                &mut cache.fn_occ,
            )?;
            cache.lines_upto = next;
            cache.items.push((pos, content_end));
            pos = next;
        }

        // Speculatively render the trailing partial line as an item. Three
        // shapes are valid: empty (no partial), all whitespace including `\n`
        // (trailing blanks after a settled item — emit nothing; cache armed),
        // or a sibling marker line (render in the cache's current style). The
        // partial item's inner HTML rides on the active block's `items` too (so
        // the keyed renderer sees the streaming tail item), but is NOT folded into
        // the cache's committed `item_html` — it may be revised next chunk.
        let partial = &bytes[cache.lines_upto..end];
        let mut partial_html = String::new();
        let mut partial_item: Vec<ListItemData> = Vec::new();
        if !partial.is_empty() {
            if partial.contains(&b'\r') {
                return None;
            }
            if partial.iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n')) {
                // Trailing blank(s) / whitespace; emit nothing.
            } else {
                let m = scan_marker(partial)?;
                if !sibling_match(&m, &cache) {
                    return None;
                }
                // Speculative trailing item: resolve from a CLONE of the
                // frozen-prefix occurrence map (does NOT advance it).
                let mut partial_occ = cache.fn_occ.clone();
                if fold_item_line(
                    partial,
                    &m,
                    cache.loose,
                    &opts,
                    &mut partial_html,
                    Some(&mut partial_item),
                    &mut partial_occ,
                )
                .is_none()
                {
                    return None;
                }
            }
        }

        let close = if cache.ordered { "</ol>" } else { "</ul>" };
        let mut html = String::with_capacity(
            cache.cached_prefix.len() + partial_html.len() + close.len(),
        );
        html.push_str(&cache.cached_prefix);
        html.push_str(&partial_html);
        html.push_str(close);

        // Opt-in structured channel: surface the per-item inner HTML (committed
        // items + the speculative trailing item) on the active block so the keyed
        // renderer reuses unchanged item nodes mid-stream. Off ⇒ empty (omitted on
        // the wire, byte-identical). The committed `cache.item_html` is borrowed by
        // reference + the trailing partial appended without disturbing the cache.
        let items: Vec<ListItemData> = if self.block_data {
            let mut v = Vec::with_capacity(cache.item_html.len() + partial_item.len());
            v.extend_from_slice(&cache.item_html);
            v.append(&mut partial_item);
            v
        } else {
            Vec::new()
        };
        let block = Block {
            id: cache.id,
            // Opt-in structured channel: fold the start number on when block_data
            // is on (matches the full path); off ⇒ `start: None` (byte-identical).
            kind: BlockKind::List {
                ordered: cache.ordered,
                start: if self.block_data { Some(cache.start_num) } else { None },
                items,
            },
            start: cache.start,
            end,
            html,
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.list_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block] })
    }
}

/// Render one flat list item from its raw line bytes. Mirrors the single-
/// paragraph branch of `render_list_item` (GFM task-list `[ ] ` / `[x] `
/// checkbox prefix supported), in either tight or loose form:
///   tight: `<li dir?>{checkbox?}{inline}</li>`
///   loose: `<li dir?>{checkbox?}\n<p dir?>{inline}</p></li>`
/// (`render_list` emits the trailing `\n` after each item; the cache also
/// pushes that `\n`, so byte layout is identical in both branches.)
/// Returns `None` on any invalid-UTF-8 path so the cache can bail; on success
/// returns `Some((lo, hi))` — the byte range *within `out`* of this item's inner
/// `<li>` HTML (the bytes between `<li…>` and `</li>`), so the cache can surface it
/// as `ListItemData` for the keyed renderer without a second render.
fn render_item_line(
    line: &[u8],
    m: &MarkerScan,
    loose: bool,
    opts: &RenderOpts,
    out: &mut String,
) -> Option<(usize, usize)> {
    let content_bytes = &line[m.content_byte..];
    let content_str = std::str::from_utf8(content_bytes).ok()?;
    // Trim trailing whitespace to match `render_list_item`'s `body_trimmed`.
    let trimmed = content_str.trim_end_matches(|c: char| matches!(c, '\n' | '\r' | ' ' | '\t'));

    // GFM task list: a body opening with `[ ] ` / `[x] ` (case-insensitive `x`)
    // becomes a disabled checkbox + remainder.
    let (checkbox, rest) = {
        let b = trimmed.as_bytes();
        if b.len() >= 4 && b[0] == b'[' && b[2] == b']' && b[3] == b' ' {
            match b[1] {
                b' ' => (Some(false), &trimmed[4..]),
                b'x' | b'X' => (Some(true), &trimmed[4..]),
                _ => (None, trimmed),
            }
        } else {
            (None, trimmed)
        }
    };

    // An empty body short-circuits to `<li></li>` in both tight and loose —
    // matches `render_list_item`'s `sub.is_empty()` branch, which never enters
    // the `<p>` wrap path. A pure-checkbox item keeps the wrapper / checkbox
    // but still skips the `<p>` (the scanner sees no paragraph to wrap).
    if rest.is_empty() && checkbox.is_none() {
        let lo = out.len();
        out.push_str("<li></li>");
        // Inner span is empty, just past the 4-byte `<li>` opener.
        return Some((lo + 4, lo + 4));
    }

    // a11y: mirror `render_list_item`'s `<label>` wrap — ONLY the tight,
    // non-empty task item (the single-paragraph inline shape) qualifies, so the
    // cached and full-reparse paths stay byte-identical.
    let wrap_label = opts.a11y && checkbox.is_some() && !loose && !rest.is_empty();

    out.push_str("<li");
    out.push_str(opts.dir());
    out.push('>');
    // Inner-HTML span starts just past the `<li…>` opening tag.
    let inner_lo = out.len();
    if wrap_label {
        out.push_str("<label>");
    }
    if let Some(checked) = checkbox {
        out.push_str(if checked {
            "<input type=\"checkbox\" checked disabled> "
        } else {
            "<input type=\"checkbox\" disabled> "
        });
    }
    if loose && !rest.is_empty() {
        // Mirrors the loose branch in `render_list_item`: leading `\n` after
        // any checkbox, then `<p dir?>{inline}</p>`, no trailing `\n` before
        // `</li>` (a trailing newline normalizes to a stray space pre-`</li>`).
        out.push('\n');
        out.push_str("<p");
        out.push_str(opts.dir());
        out.push('>');
        render_inline(rest, opts, out);
        out.push_str("</p>");
    } else if !rest.is_empty() {
        render_inline(rest, opts, out);
    }
    if wrap_label {
        out.push_str("</label>");
    }
    // Inner-HTML span ends just before `</li>`.
    let inner_hi = out.len();
    out.push_str("</li>");
    Some((inner_lo, inner_hi))
}

/// Tight→loose one-time rebuild. Re-renders `cached_prefix` from the source
/// spans in `cache.items`, each item now wrapped in `<p>…</p>`. Sets
/// `cache.loose`. O(items so far) — paid once per list, never again. Spans
/// were validated by `scan_marker` when they were appended; the only way
/// rendering can fail here is invalid UTF-8 inside a span, which means
/// `scan_marker` saw non-ASCII before the content byte (impossible because
/// markers are ASCII). Returns `None` on the impossible path so the caller
/// bails for safety.
fn rebuild_loose(cache: &mut ListCache, bytes: &[u8], opts: &RenderOpts) -> Option<()> {
    cache.loose = true;
    cache.cached_prefix.clear();
    cache.cached_prefix.push_str(&cache.opener_html);
    // Rebuild the keyed-renderer item HTML in lockstep (the loose `<p>`-wrapped
    // inner differs from the tight inline form), so `item_html` stays parallel.
    cache.item_html.clear();
    // Reset the frozen-prefix occurrence map to the arm-time baseline and replay
    // every item, so the re-rendered loose items get the SAME footnote ids in the
    // same document order as the tight items they replace.
    cache.fn_occ = cache.fn_occ_base.clone();
    // Borrow `items` separately so `cached_prefix`/`item_html` can be mutated.
    let spans = std::mem::take(&mut cache.items);
    for &(s, e) in &spans {
        let line = &bytes[s..e];
        let m = scan_marker(line)?;
        if fold_item_line(
            line,
            &m,
            true,
            opts,
            &mut cache.cached_prefix,
            Some(&mut cache.item_html),
            &mut cache.fn_occ,
        )
        .is_none()
        {
            cache.items = spans;
            return None;
        }
    }
    cache.items = spans;
    Some(())
}

/// Strip the CommonMark blockquote marker (`>` with optional one space, after
/// up to 3 leading spaces) from a line's bytes. Returns the content portion,
/// or `None` if the line doesn't carry a `>` marker (lazy continuation or
/// end-of-blockquote — the full path handles those).
fn strip_blockquote_marker(line: &[u8]) -> Option<&[u8]> {
    let mut i = 0;
    let mut indent = 0;
    while i < line.len() && line[i] == b' ' && indent < 3 {
        i += 1;
        indent += 1;
    }
    if i >= line.len() || line[i] != b'>' {
        return None;
    }
    i += 1;
    // CommonMark: a single optional space after `>` (not a tab, not multiple).
    if i < line.len() && line[i] == b' ' {
        i += 1;
    }
    Some(&line[i..])
}

/// Build the incremental cache for an open code fence at `start`, walking its
/// body once. Returns `None` (no caching) if the body isn't plain — any `\r`
/// or fence-looking line — so those keep going through the full renderer.
fn build_code_fence_cache(
    buffer: &str,
    start: usize,
    info: &str,
    id: u64,
    kind: BlockKind,
) -> Option<FenceCache> {
    let bytes = buffer.as_bytes();
    let end = bytes.len();
    // Body begins after the opener line's newline; bail if it hasn't arrived.
    let nl = bytes[start..end].iter().position(|&b| b == b'\n')?;
    let body_start = start + nl + 1;
    let mut escaped_lines = String::new();
    let mut lines_upto = body_start;
    let mut pos = body_start;
    while pos < end {
        match bytes[pos..end].iter().position(|&b| b == b'\n') {
            None => break,
            Some(r) => {
                let content_end = pos + r;
                let next = pos + r + 1;
                if bytes[pos..content_end].contains(&b'\r') || is_fence_close_line(&bytes[pos..next]) {
                    return None;
                }
                if !escaped_lines.is_empty() {
                    escaped_lines.push('\n');
                }
                escape_html(
                    std::str::from_utf8(&bytes[pos..content_end]).unwrap_or(""),
                    &mut escaped_lines,
                );
                lines_upto = next;
                pos = next;
            }
        }
    }
    if bytes[lines_upto..end].contains(&b'\r') || is_fence_close_line(&bytes[lines_upto..end]) {
        return None;
    }
    let mut opener_html = String::new();
    push_code_fence_open(info, &mut opener_html);
    Some(FenceCache {
        start,
        id,
        kind,
        opener_html,
        closer_html: "</code></pre>",
        close: FenceClose::CodeFence,
        trim_body: false,
        escaped_lines,
        lines_upto,
    })
}

/// Build the incremental cache for an open display-math fence (`$$…$$` / `\[…\]`)
/// at `start`, walking its body once. Returns `None` (no caching) when the body
/// is still all-whitespace, contains a `\r`, or already shows the matching
/// closer — those keep going through the full renderer, which gets the
/// whitespace-trim and single-line cases exactly right. Mirrors the scanner's
/// `scan_math_block`: the body begins right after the `$$`/`\[` delimiter (math
/// content may follow it on the opener line) and a line *containing* the closer
/// substring ends the block.
fn build_math_fence_cache(buffer: &str, start: usize, id: u64, kind: BlockKind) -> Option<FenceCache> {
    let bytes = buffer.as_bytes();
    let end = bytes.len();
    // Opener after ≤3 spaces of indent (the scanner guarantees ≤3).
    let mut p = start;
    let mut indent = 0;
    while p < end && bytes[p] == b' ' && indent < 3 {
        p += 1;
        indent += 1;
    }
    let closer: &'static [u8] = if bytes[p..end].starts_with(b"$$") {
        b"$$"
    } else if bytes[p..end].starts_with(b"\\[") {
        b"\\]"
    } else {
        return None;
    };
    // Body starts right after the delimiter; skip leading whitespace (math trims
    // the body's leading whitespace). If it's all-whitespace so far, arm later.
    let mut body_start = p + 2;
    while body_start < end && matches!(bytes[body_start], b' ' | b'\t' | b'\n' | b'\r') {
        body_start += 1;
    }
    if body_start >= end {
        return None;
    }
    let mut escaped_lines = String::new();
    let mut lines_upto = body_start;
    let mut pos = body_start;
    while pos < end {
        match bytes[pos..end].iter().position(|&b| b == b'\n') {
            None => break,
            Some(r) => {
                let content_end = pos + r;
                let next = pos + r + 1;
                if bytes[pos..content_end].contains(&b'\r') || line_contains(&bytes[pos..content_end], closer) {
                    return None;
                }
                if !escaped_lines.is_empty() {
                    escaped_lines.push('\n');
                }
                escape_html(
                    std::str::from_utf8(&bytes[pos..content_end]).unwrap_or(""),
                    &mut escaped_lines,
                );
                lines_upto = next;
                pos = next;
            }
        }
    }
    if bytes[lines_upto..end].contains(&b'\r') || line_contains(&bytes[lines_upto..end], closer) {
        return None;
    }
    Some(FenceCache {
        start,
        id,
        kind,
        opener_html: "<div class=\"math math-display\">".to_string(),
        closer_html: "</div>",
        close: FenceClose::MathCloser(closer),
        trim_body: true,
        escaped_lines,
        lines_upto,
    })
}

/// Arm the table cache for the open table at `start`, pre-rendering the
/// `<thead>` once. The body grows incrementally via `try_incremental_table`.
/// Returns `None` (no caching) if the header or delimiter lines aren't fully
/// present yet, if either contains a `\r` (CRLF tables route through the full
/// path), or if column counts disagree (the scanner shouldn't have produced
/// a Table block in that case, but the guard is cheap).
fn build_table_cache(
    buffer: &str,
    start: usize,
    id: u64,
    opts: &RenderOpts,
    fn_base: &HashMap<String, usize>,
) -> Option<TableCache> {
    let bytes = buffer.as_bytes();
    let end = bytes.len();
    // Header line.
    let header_nl = bytes[start..end].iter().position(|&b| b == b'\n')?;
    let header_end = start + header_nl;
    if bytes[start..header_end].contains(&b'\r') {
        return None;
    }
    let header_line = std::str::from_utf8(&bytes[start..header_end]).ok()?;
    // Delimiter line.
    let delim_start = header_end + 1;
    if delim_start >= end {
        return None;
    }
    let delim_nl = bytes[delim_start..end].iter().position(|&b| b == b'\n')?;
    let delim_end = delim_start + delim_nl;
    if bytes[delim_start..delim_end].contains(&b'\r') {
        return None;
    }
    let delim_line = std::str::from_utf8(&bytes[delim_start..delim_end]).ok()?;
    let body_start = delim_end + 1;

    let aligns = parse_alignments(delim_line);
    let header_cells = split_table_cells(header_line);
    let ncol = header_cells.len();
    if ncol == 0 || ncol != count_table_columns(delim_line.as_bytes()) {
        return None;
    }

    // Pre-render `<table dir?><thead><tr>…</tr></thead>` exactly as
    // `render_table` would. Cells use the same `push_table_cell` so inline
    // markup in headers (e.g. `**bold**`) renders byte-identical to the
    // full path.
    let mut raw_prefix = String::with_capacity(64 + ncol * 32);
    raw_prefix.push_str("<table");
    raw_prefix.push_str(opts.dir());
    raw_prefix.push_str("><thead><tr>");
    // Structured channel: capture the header cells at the exact step the `<th>`s
    // are written, from the same `push_table_cell` (so DATA matches HTML).
    let mut td_header_cells: Vec<TableCell> = Vec::new();
    for i in 0..ncol {
        let cell = push_table_cell(
            "th",
            header_cells.get(i).map(String::as_str).unwrap_or(""),
            aligns.get(i),
            opts,
            &mut raw_prefix,
        );
        if let Some(c) = cell {
            td_header_cells.push(c);
        }
    }
    raw_prefix.push_str("</tr></thead>");

    // Resolve the header's placeholder footnote tokens into the frozen prefix
    // from the committed occurrence baseline, advancing the cache-local map. The
    // header-cell DATA is resolved from a CLONE so its ids match the HTML without
    // double-counting (same content, same order).
    let mut fn_occ = fn_base.clone();
    let mut cached_prefix = String::with_capacity(raw_prefix.len());
    resolve_footnote_ids(&raw_prefix, &mut fn_occ, &mut cached_prefix);
    if opts.block_data && opts.gfm_footnotes {
        let mut occ = fn_base.clone();
        for cell in &mut td_header_cells {
            let mut o = String::with_capacity(cell.html.len());
            resolve_footnote_ids(&cell.html, &mut occ, &mut o);
            cell.html = o;
        }
    }

    Some(TableCache {
        start,
        id,
        cached_prefix,
        lines_upto: body_start,
        ncol,
        aligns,
        tbody_opened: false,
        header_cells: td_header_cells,
        body_cells: Vec::new(),
        fn_occ,
    })
}

/// Close the current paragraph: render its inline once (settled — it will
/// receive no more bytes) into `committed_paras_html` as `<p dir?>{inline}</p>\n`,
/// matching `render_paragraph` + the trailing `\n` that `render_blockquote` /
/// `render_alert` emit after each sub-block. Callers must ensure `inner_buffer`
/// is non-empty (consecutive blank `>` lines must skip this).
fn close_container_paragraph(cache: &mut ContainerCache, opts: &RenderOpts) {
    let trimmed = cache.inner_buffer.trim_end_matches(|c: char| c == '\n' || c == '\r');
    let mut tmp = String::with_capacity(trimmed.len());
    render_inline(trimmed, opts, &mut tmp);
    let raw_text =
        tmp.trim_end_matches(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r');
    // The paragraph is settled — re-render-from-scratch resolves its placeholder
    // footnote tokens, advancing the PERSISTENT closed-paras occurrence map once.
    // Reuse the resolved text for both the HTML and the data entry (byte-identical,
    // no double-count). No-op copy when footnotes off.
    let mut final_text = String::with_capacity(raw_text.len());
    resolve_footnote_ids(raw_text, &mut cache.fn_occ, &mut final_text);
    cache.committed_paras_html.push_str(&cache.body_p_open);
    cache.committed_paras_html.push_str(&final_text);
    cache.committed_paras_html.push_str(&cache.body_p_close);
    // Structured channel: record this just-closed paragraph's own HTML (no
    // trailing `\n` separator), in lock-step with `committed_paras_html`, so the
    // keyed `nested` data carries one stable entry per committed paragraph.
    if opts.block_data {
        let mut html = String::with_capacity(cache.body_p_open.len() + final_text.len() + 4);
        html.push_str(&cache.body_p_open);
        html.push_str(&final_text);
        html.push_str("</p>");
        cache.committed_paras.push(NestedBlock { html });
    }
    cache.inner_buffer.clear();
    cache.inner_cut = 0;
    cache.committed_inner_html.clear();
    // Next paragraph's open-prefix occurrence map starts from the now-advanced
    // closed-paras baseline.
    cache.inner_fn_occ = cache.fn_occ.clone();
}

/// Arm the container cache for an open blockquote / alert at `start`. Returns
/// `None` if the first inner line isn't fully present yet (so we can't safely
/// commit to a kind — Blockquote vs. Alert is a first-line decision) or if
/// the block kind isn't a Blockquote / Alert. The first cache call processes
/// the existing lines; subsequent appends only fold new bytes.
fn build_container_cache(
    buffer: &str,
    start: usize,
    id: u64,
    block_kind: &BlockKind,
    opts: &RenderOpts,
    fn_base: &HashMap<String, usize>,
) -> Option<ContainerCache> {
    let bytes = buffer.as_bytes();
    let end = bytes.len();
    // Require at least one complete line so the Blockquote / Alert distinction
    // is settled (a partial first line could later become `[!NOTE]`).
    let first_nl = bytes[start..end].iter().position(|&b| b == b'\n')?;
    let first_line_end = start + first_nl;
    if bytes[start..first_line_end].contains(&b'\r') {
        return None;
    }
    // Body `<p>` opener / closer — emitted only when the inner has content
    // (an empty body must not produce `<p></p>`, matching the full renderer).
    let mut body_p_open = String::with_capacity(16);
    body_p_open.push_str("<p");
    body_p_open.push_str(opts.dir());
    body_p_open.push('>');
    let body_p_close = String::from("</p>\n");
    let (kind, wrapper_open, wrapper_close, lines_upto) = match block_kind {
        BlockKind::Blockquote(_) => {
            let mut w = String::with_capacity(32);
            w.push_str("<blockquote");
            w.push_str(opts.dir());
            w.push_str(">\n");
            (ContainerCacheKind::Blockquote, w, String::from("</blockquote>"), start)
        }
        BlockKind::Alert { kind: ak, .. } => {
            let mut w = String::with_capacity(96);
            w.push_str("<div class=\"markdown-alert markdown-alert-");
            w.push_str(ak.class());
            w.push_str("\" data-alert=\"");
            w.push_str(ak.class());
            w.push_str("\" role=\"note\"");
            w.push_str(opts.dir());
            w.push_str(">\n<p class=\"markdown-alert-title\"");
            w.push_str(opts.dir());
            w.push('>');
            w.push_str(ak.title());
            w.push_str("</p>\n");
            // Alert: skip past the `[!KIND]` marker line — body starts on line 2.
            (ContainerCacheKind::Alert(*ak), w, String::from("</div>"), first_line_end + 1)
        }
        _ => return None,
    };
    // Don't arm for a container whose committed inner content already has BLOCK
    // structure (a list, nested blockquote, heading, fence, …): this cache only
    // renders plain paragraphs, so arming would re-arm-then-bail every append.
    // Let the full reparse own it.
    {
        let mut p = lines_upto;
        while p < end {
            let r = match bytes[p..end].iter().position(|&b| b == b'\n') {
                None => break, // trailing partial — settled by try_incremental_container
                Some(r) => r,
            };
            if let Some(stripped) = strip_blockquote_marker(&bytes[p..p + r]) {
                if !stripped.iter().all(|&b| matches!(b, b' ' | b'\t'))
                    && container_inner_breaks_paragraph(stripped, opts.scan_ctx())
                {
                    return None;
                }
            }
            p += r + 1;
        }
    }
    Some(ContainerCache {
        start,
        id,
        kind,
        wrapper_open,
        body_p_open,
        body_p_close,
        wrapper_close,
        committed_paras_html: String::new(),
        committed_paras: Vec::new(),
        inner_buffer: String::new(),
        lines_upto,
        inner_cut: 0,
        committed_inner_html: String::new(),
        fn_occ: fn_base.clone(),
        inner_fn_occ: fn_base.clone(),
    })
}

/// Arm the list cache for the open flat list at `start`. Requires the first
/// line to be complete (so the marker family / delimiter / edge are settled —
/// a partial first line could still grow into a foreign family). First
/// incremental call processes any existing sibling lines; subsequent appends
/// only fold new bytes. The list starts tight and flips to loose later if a
/// blank line appears between siblings.
fn build_list_cache(
    buffer: &str,
    start: usize,
    id: u64,
    ordered: bool,
    list_start_num: u32,
    opts: &RenderOpts,
    fn_base: &HashMap<String, usize>,
) -> Option<ListCache> {
    let bytes = buffer.as_bytes();
    let end = bytes.len();
    let first_nl = bytes[start..end].iter().position(|&b| b == b'\n')?;
    if bytes[start..start + first_nl].contains(&b'\r') {
        return None;
    }
    let first_line = &bytes[start..start + first_nl];
    let m = scan_marker(first_line)?;
    if m.ordered != ordered {
        return None;
    }
    // Pre-render the opener — matches the prefix `render_list` emits before
    // the first item. `<ul dir?>\n` / `<ol dir? start="N">\n`.
    let mut opener_html = String::with_capacity(64);
    if ordered {
        opener_html.push_str("<ol");
        opener_html.push_str(opts.dir());
        if list_start_num != 1 {
            opener_html.push_str(" start=\"");
            opener_html.push_str(&list_start_num.to_string());
            opener_html.push('"');
        }
        opener_html.push('>');
    } else {
        opener_html.push_str("<ul");
        opener_html.push_str(opts.dir());
        opener_html.push('>');
    }
    opener_html.push('\n');
    let cached_prefix = opener_html.clone();
    Some(ListCache {
        start,
        id,
        ordered,
        start_num: list_start_num,
        delim: m.delim,
        content_indent: m.content_indent,
        opener_html,
        cached_prefix,
        lines_upto: start,
        loose: false,
        items: Vec::new(),
        item_html: Vec::new(),
        fn_occ: fn_base.clone(),
        fn_occ_base: fn_base.clone(),
    })
}

/// Fold one item line into `cached_prefix` and, when `block_data` is on, capture
/// its inner `<li>` HTML into `item_html` (the keyed-renderer channel). Mirrors
/// the raw `render_item_line` + `push('\n')` the cache did before, so byte layout
/// is unchanged. Returns `None` (so the caller can truncate + bail) on the
/// invalid-UTF-8 path. `out`/`html_sink` are passed separately so the trailing
/// (speculative) item can capture into a scratch buffer without committing it to
/// the cache's `item_html`.
fn fold_item_line(
    line: &[u8],
    m: &MarkerScan,
    loose: bool,
    opts: &RenderOpts,
    out: &mut String,
    html_sink: Option<&mut Vec<ListItemData>>,
    occ: &mut HashMap<String, usize>,
) -> Option<()> {
    // Render the item into a scratch buffer (placeholder footnote tokens when on),
    // resolve its tokens into `out` advancing `occ`, and resolve the inner `<li>`
    // span for the keyed-renderer DATA from a CLONE taken at item start (same
    // content, same order → matching ids, no double-count). When footnotes are
    // off this is a token-free byte copy.
    let mut tmp = String::new();
    let (lo, hi) = render_item_line(line, m, loose, opts, &mut tmp)?;
    let data_seed = occ.clone();
    resolve_footnote_ids(&tmp, occ, out);
    if let Some(sink) = html_sink {
        if opts.block_data {
            let mut seed = data_seed;
            let mut inner = String::with_capacity(hi - lo);
            resolve_footnote_ids(&tmp[lo..hi], &mut seed, &mut inner);
            sink.push(ListItemData { html: inner });
        }
    }
    out.push('\n');
    Some(())
}

/// Arm the paragraph cache for the open paragraph at `start`, rendering its
/// initial settled prefix once. `None` if nothing is committable yet (the very
/// first construct/word boundary hasn't settled, or the paragraph is still short).
fn build_paragraph_cache(
    buffer: &str,
    start: usize,
    id: u64,
    opts: &RenderOpts,
    fn_base: &HashMap<String, usize>,
) -> Option<ParagraphCache> {
    let bytes = buffer.as_bytes();
    let mut content_end = bytes.len();
    while content_end > start && matches!(bytes[content_end - 1], b'\n' | b'\r') {
        content_end -= 1;
    }
    let mut tmp = String::new();
    let cut = start + render_inline_boundary(&buffer[start..content_end], opts, &mut tmp);
    if cut <= start {
        return None;
    }
    // Render the settled prefix with placeholder tokens (when footnotes on), then
    // resolve them into `committed_inner` from the committed occurrence baseline,
    // advancing the cache-local `fn_occ` map. The frozen prefix never re-renders.
    let mut raw = String::new();
    render_inline(&buffer[start..cut], opts, &mut raw);
    let mut fn_occ = fn_base.clone();
    let mut committed_inner = String::with_capacity(raw.len());
    resolve_footnote_ids(&raw, &mut fn_occ, &mut committed_inner);
    Some(ParagraphCache { start, id, cut, committed_inner, fn_occ })
}

/// True iff `line` (content only, no terminator) is an indented-code line:
/// ≥4 columns of leading whitespace (one tab counts as 4) followed by content.
/// Mirrors the per-line gate in `scan_indented_code`.
fn indented_code_line(line: &[u8]) -> bool {
    let mut indent = 0usize;
    let mut i = 0usize;
    while i < line.len() {
        match line[i] {
            b' ' => {
                indent += 1;
                i += 1;
            }
            b'\t' => {
                indent += 4;
                i += 1;
            }
            _ => break,
        }
        if indent >= 4 {
            break;
        }
    }
    indent >= 4 && i < line.len() && !matches!(line[i], b'\n' | b'\r')
}

/// Strip up to 4 columns of leading indent from `line` (content only) — one tab
/// is consumed whole and stops the strip — then escape the remainder into `out`.
/// Mirrors the per-line stripping in `render_indented_code`.
fn push_indented_content(line: &[u8], out: &mut String) {
    let mut i = 0usize;
    let mut consumed = 0usize;
    while i < line.len() && consumed < 4 {
        match line[i] {
            b' ' => {
                consumed += 1;
                i += 1;
            }
            b'\t' => {
                i += 1;
                break;
            }
            _ => break,
        }
    }
    escape_html(std::str::from_utf8(&line[i..]).unwrap_or(""), out);
}

/// True iff this `line` ends the open HTML block of `html_type` — the
/// type-specific closer (types 1–5, via the shared scanner predicate, on the
/// content slice `content`) or a blank line (types 6/7), or a `\r` anywhere
/// (CRLF defers to the full path). MUST match `scan_html_block`'s loop exactly.
fn html_block_closes_here(line: &[u8], html_type: u8, content: &[u8]) -> bool {
    if line.contains(&b'\r') {
        return true;
    }
    if html_block_line_closes(line, html_type) {
        return true;
    }
    // Types 6/7 end on a blank line (which is not part of the block).
    matches!(html_type, 6 | 7) && content.iter().all(|&b| matches!(b, b' ' | b'\t'))
}

/// Fold one complete HTML-block source line (terminator included) into the
/// cached prefix: verbatim for pass-through, `escape_html`d otherwise. Newlines
/// pass through `escape_html` unchanged, so the escaped prefix keeps line breaks.
fn fold_html_line(line: &[u8], pass_through: bool, out: &mut String) {
    let s = std::str::from_utf8(line).unwrap_or("");
    if pass_through {
        out.push_str(s);
    } else {
        escape_html(s, out);
    }
}

/// Arm the indented-code cache for the open block at `start`, walking its body
/// lines once. Returns `None` (no caching) if a line dedents, is blank, or
/// contains a `\r` — those keep going through the full renderer, which gets the
/// interior-blank accounting and exact block end right.
fn build_indented_cache(buffer: &str, start: usize, id: u64) -> Option<IndentedCodeCache> {
    let bytes = buffer.as_bytes();
    let end = bytes.len();
    let mut escaped_lines = String::new();
    let mut lines_upto = start;
    let mut pos = start;
    while pos < end {
        match bytes[pos..end].iter().position(|&b| b == b'\n') {
            None => break,
            Some(r) => {
                let content_end = pos + r;
                let next = pos + r + 1;
                if bytes[pos..content_end].contains(&b'\r')
                    || !indented_code_line(&bytes[pos..content_end])
                {
                    return None;
                }
                if !escaped_lines.is_empty() {
                    escaped_lines.push('\n');
                }
                push_indented_content(&bytes[pos..content_end], &mut escaped_lines);
                lines_upto = next;
                pos = next;
            }
        }
    }
    // The trailing partial must not already dedent (else the block has ended and
    // the full path owns it); an all-whitespace partial is fine (blank-so-far).
    let partial = &bytes[lines_upto..end];
    if partial.contains(&b'\r') {
        return None;
    }
    let partial_blank = partial.iter().all(|&b| matches!(b, b' ' | b'\t'));
    if !partial_blank && !indented_code_line(partial) {
        return None;
    }
    Some(IndentedCodeCache { start, id, escaped_lines, lines_upto })
}

/// Arm the raw-HTML-block cache for the open block at `start`, walking its body
/// lines once. Returns `None` (no caching) if the block already meets its close
/// condition (a closing line / a blank line for types 6–7) or any line carries a
/// `\r` — those keep going through the full renderer.
fn build_html_cache(buffer: &str, start: usize, id: u64, opts: &RenderOpts) -> Option<HtmlBlockCache> {
    let bytes = buffer.as_bytes();
    let end = bytes.len();
    let (_, html_type) = detect_html_block_open(bytes, start)?;
    let pass_through = opts.unsafe_html && !opts.html_sanitize;
    let mut cached_prefix = String::new();
    let mut lines_upto = start;
    let mut pos = start;
    while pos < end {
        match bytes[pos..end].iter().position(|&b| b == b'\n') {
            None => break,
            Some(r) => {
                let content_end = pos + r;
                let next = pos + r + 1;
                let line = &bytes[pos..next];
                if html_block_closes_here(line, html_type, &bytes[pos..content_end]) {
                    return None;
                }
                fold_html_line(line, pass_through, &mut cached_prefix);
                lines_upto = next;
                pos = next;
            }
        }
    }
    let partial = &bytes[lines_upto..end];
    if html_block_closes_here(partial, html_type, partial) {
        return None;
    }
    Some(HtmlBlockCache { start, id, html_type, pass_through, cached_prefix, lines_upto })
}

/// True if the open paragraph beginning before `cut` actually ends somewhere in
/// `[cut, EOF)` — a blank line, an interrupting block start, or a setext
/// underline (which would change the block's kind). The line containing `cut`
/// is a continuation (it began as paragraph text), so it's skipped.
fn paragraph_ends_before_eof(bytes: &[u8], cut: usize, ctx: ScanCtx) -> bool {
    let len = bytes.len();

    // Phase 1: re-check the line containing `cut` if it has just completed.
    if cut < len && cut > 0 && bytes[cut - 1] != b'\n' {
        if bytes[cut..len].contains(&b'\n') {
            let mut s = cut - 1;
            while s > 0 && bytes[s - 1] != b'\n' {
                s -= 1;
            }
            let cur_line_start = s;
            let next = line_end(bytes, cur_line_start);
            if next > cur_line_start && bytes[next - 1] == b'\n' {
                if is_blank_line(bytes, cur_line_start)
                    || is_setext_underline(bytes, cur_line_start).is_some()
                    || would_start_other_block(bytes, cur_line_start, ctx)
                {
                    return true;
                }
                if is_table_delimiter_row(line_slice(bytes, cur_line_start)) {
                    let prev = prev_line_start(bytes, cur_line_start);
                    if prev != cur_line_start
                        && forms_table_header(bytes, prev, cur_line_start)
                    {
                        return true;
                    }
                }
            }
        }
    }

    let mut pos = cut;
    if pos < len && (pos == 0 || bytes[pos - 1] != b'\n') {
        while pos < len && bytes[pos] != b'\n' {
            pos += 1;
        }
        if pos < len {
            pos += 1;
        }
    }
    // Spot a paragraph turning into a GFM table — a `|---|` delimiter row under a
    // matching header line. Like a setext underline, that retroactively changes
    // the block's kind, so the fast-path must bail and let the full scan re-form
    // it as a table (which then streams its rows incrementally). Track the
    // previous line forward so the check is O(1) per line; only a delimiter row
    // ever consults the header (rare), so a plain (single-line) paragraph pays
    // nothing — no per-append backward scan.
    let mut prev: Option<usize> = None;
    while pos < len {
        if is_blank_line(bytes, pos)
            || is_setext_underline(bytes, pos).is_some()
            || would_start_other_block(bytes, pos, ctx)
        {
            return true;
        }
        if is_table_delimiter_row(line_slice(bytes, pos)) {
            let header = prev.unwrap_or_else(|| prev_line_start(bytes, pos));
            if header != pos && forms_table_header(bytes, header, pos) {
                return true;
            }
        }
        prev = Some(pos);
        pos = line_end(bytes, pos);
    }
    false
}

/// Start of the line immediately before `pos` (which must be a line start), or 0.
fn prev_line_start(bytes: &[u8], pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut s = pos - 1; // the '\n' terminating the previous line
    while s > 0 && bytes[s - 1] != b'\n' {
        s -= 1;
    }
    s
}

/// True if the line at `header` followed by the delimiter line at `delim` forms a
/// GFM table (header has a `|` and their column counts match) — mirrors the gate
/// in `scan_table`. Caller has already confirmed `delim` is a delimiter row.
fn forms_table_header(bytes: &[u8], header: usize, delim: usize) -> bool {
    let h = line_slice(bytes, header);
    h.contains(&b'|') && count_table_columns(h) == count_table_columns(line_slice(bytes, delim))
}

impl Default for StreamParser {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(dead_code)]
fn extract_link_ref(slice: &str) -> Option<(String, String, Option<String>)> {
    let bytes = slice.as_bytes();
    let mut i = 0;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    if bytes.get(i) != Some(&b'[') {
        return None;
    }
    i += 1;
    let label_start = i;
    let mut depth = 1;
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    if depth != 0 {
        return None;
    }
    let label = std::str::from_utf8(&bytes[label_start..i]).ok()?.to_string();
    i += 1; // ]
    if bytes.get(i) != Some(&b':') {
        return None;
    }
    i += 1;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    let url_start = i;
    let url: String;
    if bytes.get(i) == Some(&b'<') {
        i += 1;
        let s = i;
        while i < bytes.len() && bytes[i] != b'>' && bytes[i] != b'\n' {
            i += 1;
        }
        url = std::str::from_utf8(&bytes[s..i]).ok()?.to_string();
        if bytes.get(i) == Some(&b'>') {
            i += 1;
        }
    } else {
        let s = i;
        while i < bytes.len() && !matches!(bytes[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        url = std::str::from_utf8(&bytes[s..i]).ok()?.to_string();
    }
    if url.is_empty() {
        return None;
    }
    let _ = url_start;
    // Optional title.
    let mut title: Option<String> = None;
    let save = i;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
        i += 1;
    }
    if i < bytes.len() && bytes[i] == b'\n' {
        i += 1;
        while i < bytes.len() && matches!(bytes[i], b' ' | b'\t') {
            i += 1;
        }
    }
    if i < bytes.len() && matches!(bytes[i], b'"' | b'\'' | b'(') {
        let close = match bytes[i] {
            b'"' => b'"',
            b'\'' => b'\'',
            _ => b')',
        };
        i += 1;
        let ts = i;
        while i < bytes.len() && bytes[i] != close {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
            } else {
                i += 1;
            }
        }
        if i < bytes.len() && bytes[i] == close {
            title = Some(std::str::from_utf8(&bytes[ts..i]).ok()?.to_string());
        } else {
            // invalid title; ignore.
            let _ = save;
        }
    }
    Some((label, url, title))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &str) -> String {
        let mut p = StreamParser::new();
        p.append(src);
        p.finalize();
        let mut out = String::new();
        for b in p.all_blocks() {
            out.push_str(&b.html);
        }
        out
    }

    fn render_unsafe(src: &str) -> String {
        let mut p = StreamParser::new().with_unsafe_html(true);
        p.append(src);
        p.finalize();
        let mut out = String::new();
        for b in p.all_blocks() {
            out.push_str(&b.html);
        }
        out
    }

    #[test]
    fn single_paragraph_stays_active_until_blank_line() {
        let mut p = StreamParser::new();
        let patch = p.append("Hello world");
        assert_eq!(patch.active.len(), 1);
        assert_eq!(patch.active[0].kind.tag(), "Paragraph");
        assert!(patch.active[0].open);
        let patch = p.append("\n\n");
        assert_eq!(patch.newly_committed.len(), 1);
        assert_eq!(patch.active.len(), 0);
    }

    #[test]
    fn id_is_stable_across_appends() {
        let mut p = StreamParser::new();
        p.append("Hello ");
        let first_id = p.active_blocks[0].id;
        p.append("world");
        let second_id = p.active_blocks[0].id;
        assert_eq!(first_id, second_id);
    }

    #[test]
    fn unclosed_code_block_renders_speculatively() {
        let mut p = StreamParser::new();
        let patch = p.append("```rust\nfn main() {\n  println!(\"hi\");\n");
        assert_eq!(patch.active.len(), 1);
        assert!(patch.active[0].html.contains("</code></pre>"));
        let patch = p.append("}\n```\n\n");
        assert_eq!(patch.newly_committed.len(), 1);
    }

    #[test]
    fn link_with_javascript_url_is_sanitized() {
        let html = render("[click](javascript:alert(1))\n\n");
        assert!(!html.contains("javascript:"), "html was: {}", html);
        assert!(html.contains("href=\"#\""));
    }

    #[test]
    fn html_text_is_escaped_in_safe_mode() {
        let html = render("<script>alert(1)</script>\n\n");
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn html_text_passes_through_in_unsafe_mode() {
        let html = render_unsafe("<div>raw</div>\n\n");
        assert!(html.contains("<div>raw</div>"), "html: {}", html);
    }

    #[test]
    fn link_reference_definition_resolves_later_use() {
        let html = render("[foo]: /url \"title\"\n\nSee [foo].\n\n");
        assert!(html.contains("href=\"/url\""), "html: {}", html);
        assert!(html.contains("title=\"title\""));
    }

    #[test]
    fn entity_decoded_named() {
        let html = render("Hello &amp; goodbye.\n\n");
        assert!(html.contains("Hello &amp; goodbye."), "html: {}", html);
    }

    #[test]
    fn entity_decoded_numeric() {
        let html = render("&#65;&#x42;.\n\n");
        assert!(html.contains("AB."), "html: {}", html);
    }

    #[test]
    fn setext_h1_via_equals_underline() {
        let html = render("Big title\n=========\n\n");
        assert!(html.contains("<h1>Big title</h1>"), "{}", html);
    }

    #[test]
    fn indented_code_block() {
        let html = render("    fn main() {\n        println!(\"hi\");\n    }\n\n");
        assert!(html.contains("fn main()"));
        assert!(!html.contains("    fn main()"));
    }

    #[test]
    fn table_with_alignment() {
        let html = render("| L | C | R |\n|:--|:-:|--:|\n| a | b | c |\n\n");
        assert!(html.starts_with("<table>"));
        assert!(html.contains("text-align:left"));
    }

    #[test]
    fn task_list_checkboxes() {
        let html = render("- [x] done\n- [ ] todo\n\n");
        assert!(html.contains("checkbox\" checked disabled"));
    }

    #[test]
    fn blockquote_renders_inner_blocks() {
        let html = render("> # Inside\n> a quote\n\n");
        assert!(html.contains("<blockquote>"));
        assert!(html.contains("<h1>Inside</h1>"));
    }

    // Parity tests pass even if the cache silently never engages (the full
    // renderer would just run every time). These assert it *does* fire, so a
    // regression that disables it can't hide.
    #[test]
    fn paragraph_cache_engages_for_plain_text() {
        let md = "the quick brown fox jumps over the lazy dog again and again here ".repeat(4);
        let mut p = StreamParser::new();
        let mut buf = [0u8; 4];
        for ch in md.chars() {
            p.append(ch.encode_utf8(&mut buf));
        }
        let cache = p.para_cache.as_ref().expect("paragraph cache should arm for plain text");
        assert!(cache.cut > cache.start, "cache should have committed a plain prefix");
        assert!(!cache.committed_inner.is_empty());
    }

    #[test]
    fn code_fence_cache_engages() {
        let mut p = StreamParser::new();
        let mut buf = [0u8; 4];
        for ch in "```rust\nfn a() {}\nfn b() {}\nlet x = 1;\n".chars() {
            p.append(ch.encode_utf8(&mut buf));
        }
        assert!(p.fence_cache.is_some(), "code-fence cache should arm for an open fence");
    }

    #[test]
    fn math_fence_cache_engages() {
        let mut p = StreamParser::new().with_gfm_math(true);
        let mut buf = [0u8; 4];
        for ch in "$$\n\\begin{aligned}\na &= b \\\\\nc &= d\n".chars() {
            p.append(ch.encode_utf8(&mut buf));
        }
        assert!(p.fence_cache.is_some(), "math-fence cache should arm for an open $$ block");
    }

    #[test]
    fn component_tags_config_is_stored() {
        let p = StreamParser::new().with_component_tags(vec!["Thinking".into(), "Callout".into()]);
        assert_eq!(p.component_tags.len(), 2);
        assert_eq!(&*p.component_tags[0], "Thinking");
        assert_eq!(&*p.component_tags[1], "Callout");
        // Default is empty (feature off).
        assert!(StreamParser::new().component_tags.is_empty());
    }
}

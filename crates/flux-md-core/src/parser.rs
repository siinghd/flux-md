//! Incremental streaming parser.

use std::collections::HashMap;
use std::rc::Rc;

use crate::blocks::Block;
use crate::render::{
    blockquote_inner, classify, collect_footnote_defs, collect_footnote_refs,
    count_footnote_refs, is_fence_close_line, is_footnote_def_block, item_body, normalize_label,
    parse_alignments, push_code_fence_open, push_table_cell, render_block,
    render_footnote_section, split_table_cells, LinkRef, RenderOpts,
};
use crate::blocks::BlockKind;
use crate::scanner::{
    count_table_columns, is_blank_line, is_setext_underline, is_table_delimiter_row, line_end,
    line_slice, parse_link_ref_def, scan, scan_marker, would_start_other_block, MarkerScan,
    RawBlock, RawBlockKind, ScanCtx,
};
use crate::inline::{render_inline, render_inline_boundary};
use crate::url::escape_html;

/// Collect link reference definitions from `text` into `refs`, recursing into
/// block quotes and list items (definitions are document-wide, §4.7). `ctx`
/// keeps the block split identical to the render-time scan (e.g. a `$$…$$`
/// math fence stays one block instead of being mis-read).
fn collect_refs(text: &str, refs: &mut HashMap<String, LinkRef>, ctx: ScanCtx) {
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
                collect_refs(&inner, refs, ctx);
            }
            RawBlockKind::List { .. } => {
                // Re-split the list into items and recurse into each body.
                let slice = &text[raw.range.clone()];
                for item in split_list_items(slice) {
                    if let Some(body) = item_body(item.as_bytes()) {
                        collect_refs(&body, refs, ctx);
                    }
                }
            }
            _ => {}
        }
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
    /// Opt-in allowlist of custom "component" tag names (e.g. `Thinking`,
    /// `Callout`). A `<Tag>…</Tag>` whose name is listed is parsed as a container
    /// whose inner content is markdown, and dispatched to a React component —
    /// safely, without `unsafe_html`. Empty by default (no component tags).
    component_tags: Vec<Box<str>>,
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
/// Disarmed when footnotes are on: cell-level `[^x]` occurrence ids would
/// diverge across the cache vs. full-reparse boundary (the cache renders each
/// row once; the full path re-renders the whole tail each append). The full
/// path stays O(n²) in that combination — rare enough to defer to a later fix.
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
/// Disarmed when footnotes are on, mirroring `TableCache`: cell-level
/// `[^x]` occurrence ids would diverge across the cache vs. full-reparse
/// boundary (the cache renders each inner-prefix once; the full path
/// re-renders the whole inner each append).
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
/// Disarmed when `gfmFootnotes` is on, like `TableCache` / `ContainerCache`.
struct ListCache {
    /// Absolute byte offset of the list's first line in `buffer`.
    start: usize,
    /// Stable id of the list block.
    id: u64,
    /// Ordered vs. unordered — locked at the first marker.
    ordered: bool,
    /// Marker family + delimiter (`b'-'`/`b'*'`/`b'+'` for bullets,
    /// `b'.'`/`b')'` for ordered). A sibling must match.
    delim: u8,
    /// `marker_indent` of the first item — siblings must have
    /// `marker_indent <= edge + 3` (CommonMark §5.2).
    edge: usize,
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
            component_tags: Vec::new(),
            fence_cache: None,
            para_cache: None,
            table_cache: None,
            container_cache: None,
            list_cache: None,
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
        }

        let tail_start = self.committed_offset;
        let tail = &self.buffer[tail_start..];

        let ctx = ScanCtx { math: self.gfm_math, component_tags: &self.component_tags };
        let raw_blocks = scan(tail, ctx);

        // Pre-pass: build the ref table for this render. The committed table is
        // shared into opts by an O(1) `Rc` clone (never copied per append);
        // tail definitions are collected fresh each reparse (so a half-typed
        // definition in the growing tail can't get stuck). Committed wins at
        // lookup time (first-definition-wins).
        let committed_refs = Rc::clone(&self.committed_refs);
        let mut tail_refs = HashMap::new();
        collect_refs(tail, &mut tail_refs, ctx);

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
            gfm_autolinks: self.gfm_autolinks,
            gfm_alerts: self.gfm_alerts,
            gfm_math: self.gfm_math,
            dir_auto: self.dir_auto,
            gfm_footnotes,
            footnotes: fn_nums.clone(),
            // Seed the per-label occurrence counter from the committed counts so
            // ref ids stay unique across the commit boundary.
            footnote_occ: std::cell::RefCell::new(self.committed_footnote_occurrences.clone()),
            component_tags: self.component_tags.clone(),
        };

        let mut produced: Vec<Block> = Vec::with_capacity(renderable.len());
        for raw in &renderable {
            let kind = classify(&raw.kind, &tail[raw.range.clone()], self.gfm_alerts);
            let mut html = String::with_capacity(64);
            render_block(tail, raw, &opts, &mut html);
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

        // After the body render, the occurrence counter holds the *total*
        // references per label (committed + tail) — capture it before def
        // content is rendered (which would otherwise perturb it). Then collect
        // the definitions for the section.
        let total_occ = opts.footnote_occ.borrow().clone();
        let mut fn_defs = self.committed_footnote_defs.clone();
        if gfm_footnotes {
            collect_footnote_defs(tail, &mut fn_defs, &opts);
        }

        let buffer_ends_blank = self.buffer.ends_with("\n\n") || self.buffer.ends_with("\r\n\r\n");
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
        let to_commit = if produced.is_empty() {
            0
        } else if commit_all {
            produced.len()
        } else if n >= 2
            && matches!(renderable[n - 1].kind, RawBlockKind::Paragraph)
            && is_resumable(&renderable[n - 2].kind)
        {
            // A resumable container immediately followed by a paragraph may
            // still be mid-parse — the "paragraph" could be a partial list
            // marker or a lazy continuation that merges back into the
            // container once more bytes arrive. Keep both uncommitted.
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
                count_footnote_refs(committed_slice, &mut self.committed_footnote_occurrences);
                collect_footnote_defs(committed_slice, &mut self.committed_footnote_defs, &opts);
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
                        self.para_cache =
                            build_paragraph_cache(&self.buffer, start, new_active[0].id, &opts);
                    }
                    // Footnotes are disabled for the table / container caches
                    // (see their doc comments): the per-`[^x]` occurrence counter
                    // would diverge across the cache vs. full-reparse boundary.
                    RawBlockKind::Table if !self.gfm_footnotes => {
                        self.table_cache =
                            build_table_cache(&self.buffer, start, new_active[0].id, &opts);
                    }
                    RawBlockKind::Blockquote if !self.gfm_footnotes => {
                        self.container_cache = build_container_cache(
                            &self.buffer,
                            start,
                            new_active[0].id,
                            &new_active[0].kind,
                            &opts,
                        );
                    }
                    RawBlockKind::List { ordered, start: list_start_num }
                        if !self.gfm_footnotes =>
                    {
                        self.list_cache = build_list_cache(
                            &self.buffer,
                            start,
                            new_active[0].id,
                            *ordered,
                            *list_start_num,
                            &opts,
                        );
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
            collect_refs(committed_slice, Rc::make_mut(&mut self.committed_refs), ctx);
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
        html.push_str(cache.closer_html);
        let block = Block {
            id: cache.id,
            kind: cache.kind.clone(),
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
            gfm_autolinks: self.gfm_autolinks,
            gfm_alerts: self.gfm_alerts,
            gfm_math: self.gfm_math,
            dir_auto: self.dir_auto,
            gfm_footnotes: self.gfm_footnotes,
            footnotes,
            footnote_occ: std::cell::RefCell::new(self.committed_footnote_occurrences.clone()),
            component_tags: self.component_tags.clone(),
        }
    }

    /// O(new bytes) extension of a long open paragraph at the tail. Commits the
    /// blocker-free plain prefix once and re-renders only the short active tail.
    /// Returns `None` (dropping the cache) whenever the paragraph has ended or
    /// is no longer the sole tail block — the full reparse then handles it.
    fn try_incremental_paragraph(&mut self) -> Option<Patch> {
        let mut cache = self.para_cache.take()?;
        let ctx = ScanCtx { math: self.gfm_math, component_tags: &self.component_tags };
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
            // re-render the now-shorter active tail.
            let mut seg = String::new();
            render_inline(&self.buffer[cache.cut..new_cut], &opts, &mut seg);
            cache.committed_inner.push_str(&seg);
            cache.cut = new_cut;
            active.clear();
            render_inline(&self.buffer[cache.cut..content_end], &opts, &mut active);
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
        let ctx = ScanCtx { math: self.gfm_math, component_tags: &self.component_tags };
        // Build inline opts once for the whole append: the same shared RenderOpts
        // backs cached-row rendering and the speculative partial-row render. Cells
        // never define link refs / footnote defs themselves, so the open table
        // contributes nothing to the footnote_region — `&""` is fine.
        let opts = self.build_inline_opts("");

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
            cache.cached_prefix.push_str("<tr>");
            for i in 0..cache.ncol {
                push_table_cell(
                    "td",
                    cells.get(i).map(String::as_str).unwrap_or(""),
                    cache.aligns.get(i),
                    &opts,
                    &mut cache.cached_prefix,
                );
            }
            cache.cached_prefix.push_str("</tr>");
            cache.lines_upto = next;
            pos = next;
        }

        // Speculatively render the trailing partial line (no `\n`) as a row, if
        // it's non-empty and not blank. The full renderer treats a final
        // newline-less line as the last row, so we must too. The partial is short
        // (≤ one row's worth), so re-rendering it each append is O(row).
        let partial = &bytes[cache.lines_upto..end];
        let mut partial_html = String::new();
        if !partial.is_empty() && !is_blank_line(bytes, cache.lines_upto) {
            if partial.contains(&b'\r') {
                return None;
            }
            let line_str = std::str::from_utf8(partial).unwrap_or("");
            let cells = split_table_cells(line_str);
            partial_html.push_str("<tr>");
            for i in 0..cache.ncol {
                push_table_cell(
                    "td",
                    cells.get(i).map(String::as_str).unwrap_or(""),
                    cache.aligns.get(i),
                    &opts,
                    &mut partial_html,
                );
            }
            partial_html.push_str("</tr>");
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

        let block = Block {
            id: cache.id,
            kind: BlockKind::Table,
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

        // Inline opts — built once, shared by the close-paragraph render and
        // the per-append boundary pass. `&""` for the footnote-region pre-pass
        // matches the paragraph-cache convention (the open container's inner
        // can't define refs / footnote defs).
        let opts = self.build_inline_opts("");

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
            cache.committed_inner_html.push_str(&seg);
            cache.inner_cut = new_cut;
            active_html.clear();
            render_inline(&cache.inner_buffer[cache.inner_cut..], &opts, &mut active_html);
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

        let kind = match cache.kind {
            ContainerCacheKind::Blockquote => BlockKind::Blockquote,
            ContainerCacheKind::Alert(ak) => BlockKind::Alert { kind: ak },
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
        let opts = self.build_inline_opts("");

        // Helper: a marker line `line` qualifies as a sibling of this list.
        let sibling_match = |m: &MarkerScan, cache: &ListCache| {
            m.ordered == cache.ordered
                && m.delim == cache.delim
                && m.marker_indent <= cache.edge + 3
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
                    let cached_len_before = cache.cached_prefix.len();
                    if render_item_line(look_line, &m, true, &opts, &mut cache.cached_prefix)
                        .is_none()
                    {
                        cache.cached_prefix.truncate(cached_len_before);
                        return None;
                    }
                    cache.cached_prefix.push('\n');
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
            let cached_len_before = cache.cached_prefix.len();
            if render_item_line(line, &m, cache.loose, &opts, &mut cache.cached_prefix).is_none() {
                cache.cached_prefix.truncate(cached_len_before);
                return None;
            }
            cache.cached_prefix.push('\n');
            cache.lines_upto = next;
            cache.items.push((pos, content_end));
            pos = next;
        }

        // Speculatively render the trailing partial line as an item. Three
        // shapes are valid: empty (no partial), all whitespace including `\n`
        // (trailing blanks after a settled item — emit nothing; cache armed),
        // or a sibling marker line (render in the cache's current style).
        let partial = &bytes[cache.lines_upto..end];
        let mut partial_html = String::new();
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
                if render_item_line(partial, &m, cache.loose, &opts, &mut partial_html).is_none() {
                    return None;
                }
                partial_html.push('\n');
            }
        }

        let close = if cache.ordered { "</ol>" } else { "</ul>" };
        let mut html = String::with_capacity(
            cache.cached_prefix.len() + partial_html.len() + close.len(),
        );
        html.push_str(&cache.cached_prefix);
        html.push_str(&partial_html);
        html.push_str(close);

        let block = Block {
            id: cache.id,
            kind: BlockKind::List { ordered: cache.ordered },
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
/// Returns `None` on any invalid-UTF-8 path so the cache can bail.
fn render_item_line(
    line: &[u8],
    m: &MarkerScan,
    loose: bool,
    opts: &RenderOpts,
    out: &mut String,
) -> Option<()> {
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
        out.push_str("<li></li>");
        return Some(());
    }

    out.push_str("<li");
    out.push_str(opts.dir());
    out.push('>');
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
    out.push_str("</li>");
    Some(())
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
    for &(s, e) in &cache.items {
        let line = &bytes[s..e];
        let m = scan_marker(line)?;
        render_item_line(line, &m, true, opts, &mut cache.cached_prefix)?;
        cache.cached_prefix.push('\n');
    }
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
fn build_table_cache(buffer: &str, start: usize, id: u64, opts: &RenderOpts) -> Option<TableCache> {
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
    let mut cached_prefix = String::with_capacity(64 + ncol * 32);
    cached_prefix.push_str("<table");
    cached_prefix.push_str(opts.dir());
    cached_prefix.push_str("><thead><tr>");
    for i in 0..ncol {
        push_table_cell(
            "th",
            header_cells.get(i).map(String::as_str).unwrap_or(""),
            aligns.get(i),
            opts,
            &mut cached_prefix,
        );
    }
    cached_prefix.push_str("</tr></thead>");

    Some(TableCache {
        start,
        id,
        cached_prefix,
        lines_upto: body_start,
        ncol,
        aligns,
        tbody_opened: false,
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
    let final_text =
        tmp.trim_end_matches(|c: char| c == ' ' || c == '\t' || c == '\n' || c == '\r');
    cache.committed_paras_html.push_str(&cache.body_p_open);
    cache.committed_paras_html.push_str(final_text);
    cache.committed_paras_html.push_str(&cache.body_p_close);
    cache.inner_buffer.clear();
    cache.inner_cut = 0;
    cache.committed_inner_html.clear();
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
        BlockKind::Blockquote => {
            let mut w = String::with_capacity(32);
            w.push_str("<blockquote");
            w.push_str(opts.dir());
            w.push_str(">\n");
            (ContainerCacheKind::Blockquote, w, String::from("</blockquote>"), start)
        }
        BlockKind::Alert { kind: ak } => {
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
    Some(ContainerCache {
        start,
        id,
        kind,
        wrapper_open,
        body_p_open,
        body_p_close,
        wrapper_close,
        committed_paras_html: String::new(),
        inner_buffer: String::new(),
        lines_upto,
        inner_cut: 0,
        committed_inner_html: String::new(),
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
        delim: m.delim,
        edge: m.marker_indent,
        opener_html,
        cached_prefix,
        lines_upto: start,
        loose: false,
        items: Vec::new(),
    })
}

/// Arm the paragraph cache for the open paragraph at `start`, rendering its
/// initial settled prefix once. `None` if nothing is committable yet (the very
/// first construct/word boundary hasn't settled, or the paragraph is still short).
fn build_paragraph_cache(buffer: &str, start: usize, id: u64, opts: &RenderOpts) -> Option<ParagraphCache> {
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
    let mut committed_inner = String::new();
    render_inline(&buffer[start..cut], opts, &mut committed_inner);
    Some(ParagraphCache { start, id, cut, committed_inner })
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

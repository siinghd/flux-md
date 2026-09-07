//! Incremental streaming parser.

use std::collections::HashMap;
use std::rc::Rc;

use crate::blocks::Block;
use crate::render::{
    alert_head, blockquote_inner, chop_trailing_blank_lines, classify, collect_footnote_defs,
    collect_footnote_refs, cr,
    collect_footnote_refs_overlay, extend_footnote_refs, is_fence_close_line,
    is_footnote_def_block, item_body, item_directly_loose, last_footnote_def_opener,
    fence_indent, fold_block_html, html_block_sanitizes, normalize_label, parse_alignments,
    push_code_fence_open, push_html_closers, push_table_cell,
    push_lazy_line, push_table_cell_open, push_tagfiltered, render_block, render_footnote_section,
    render_item_body, resolve_footnote_ids, resolve_footnote_ids_overlay, split_cols,
    split_table_cells,
    trim_trailing_newlines, Enrichment, HtmlPolicy, LinkRef, RenderOpts,
};
use crate::blocks::{BlockKind, ContainerData, ListItemData, NestedBlock, TableCell, TableData};
use crate::scanner::{
    component_inner_range, component_open_tag, count_table_columns, detect_html_block_open,
    html_block_line_closes, indent_cols, is_blank_line, is_clean_close_tag, is_setext_underline,
    is_table_delimiter_row, line_end, line_slice, parse_link_ref_def, scan, scan_marker,
    strip_indent, would_start_other_block, MarkerScan, RawBlock, RawBlockKind, ScanCtx,
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
        || scan_marker(stripped, ctx).is_some()
        || is_setext_underline(stripped, 0).is_some()
        || is_table_delimiter_row(stripped)
        // a link reference definition produces no visible output; the cache would
        // otherwise render it as a literal paragraph.
        || parse_link_ref_def(stripped, 0).is_some()
        || indent_cols(stripped) >= 4
}

/// While an open blockquote's FIRST inner line is still incomplete (no `\n`
/// yet), can it still resolve to a GitHub alert? `render::alert_head` makes a
/// blockquote an alert iff its `>`-stripped first line, `str::trim`-med, is
/// exactly `[!KIND]` (KIND ∈ NOTE/TIP/IMPORTANT/WARNING/CAUTION, upper-case).
/// So the partial `stripped` content is still "undecided" only while — after
/// its leading whitespace — it is a prefix of some marker (still being typed),
/// or a completed marker trailed by whitespace alone. Outside that (≤12-byte)
/// window the alert is impossible and a Blockquote cache may arm mid-line. A
/// completed marker is normally already classified as an Alert (which bails on a
/// missing newline separately); the trailing-whitespace arm keeps the predicate
/// safe if consulted for it anyway. Non-UTF-8 stays undecided (keeps bailing).
fn first_line_alert_undecided(stripped: &[u8]) -> bool {
    const MARKERS: [&str; 5] = ["[!NOTE]", "[!TIP]", "[!IMPORTANT]", "[!WARNING]", "[!CAUTION]"];
    let core = match std::str::from_utf8(stripped) {
        Ok(s) => s.trim_start(),
        Err(_) => return true,
    };
    if core.is_empty() {
        return true;
    }
    MARKERS.iter().any(|m| {
        m.starts_with(core)
            || core.strip_prefix(m).is_some_and(|rest| rest.chars().all(char::is_whitespace))
    })
}
use crate::inline::{
    open_tag_streaming_quote, render_inline, render_inline_boundary, render_inline_para,
    render_inline_para_boundary,
};
use crate::url::{escape_attr, escape_html, sanitize_attrs};

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

/// Max recursion depth for the [`ContainerBlockCache`] nested-parser fast path.
/// Each level owns a real [`StreamParser`] (a heavier stack frame than the
/// render recursion), so this is set well below [`MAX_REF_DEPTH`]: past it the
/// container falls back to the full reparse (itself bounded by
/// `render::MAX_RENDER_DEPTH`). No real document nests structured containers
/// anywhere near this deep.
const MAX_CONTAINER_DEPTH: usize = 24;

fn collect_refs(
    text: &str,
    refs: &mut HashMap<String, LinkRef>,
    ctx: ScanCtx,
    alerts: bool,
    depth: usize,
) {
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
                // Align with `render_alert`: an alert's body starts on line 2
                // (the `[!KIND]` marker line is the title, not body content).
                // Scanning the title line as body would glue a first-body-line
                // definition onto the title "paragraph" here while the render
                // side's body scan treats it as a definition — the def would be
                // dropped from output yet never collected (a swallowed def).
                let inner_body = if alerts && alert_head(&inner).is_some() {
                    match inner.find('\n') {
                        Some(nl) => &inner[nl + 1..],
                        None => "",
                    }
                } else {
                    &inner[..]
                };
                collect_refs(inner_body, refs, ctx, alerts, depth + 1);
            }
            RawBlockKind::List { .. } => {
                // Re-split the list into items and recurse into each body.
                let slice = &text[raw.range.clone()];
                for item in split_list_items(slice, ctx) {
                    if let Some(body) = item_body(item.as_bytes(), ctx) {
                        collect_refs(&body, refs, ctx, alerts, depth + 1);
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
/// The occurrence count is layered: `base` is the (Rc-shared, never mutated)
/// committed occurrence map and `over` the running per-reparse overlay,
/// advanced by exactly the tokens in `block.html` (the canonical document-order
/// stream) — so a reparse never clones the growing committed map. The
/// structured `kind.data` HTML carries the SAME tokens (same content, same
/// order) duplicated for the keyed renderer, so it is resolved from a CLONE of
/// the (small) overlay snapshotted at the start of this block — that yields
/// suffixes byte-identical to `html` without double-counting occurrences. (Cell
/// `text` is inline-stripped, so it never contains a token.)
fn resolve_block_footnotes(
    block: &mut Block,
    base: &HashMap<String, usize>,
    over: &mut HashMap<String, usize>,
) {
    // Snapshot for the data channel BEFORE html advances the overlay.
    let data_seed = over.clone();

    let mut new_html = String::with_capacity(block.html.len());
    resolve_footnote_ids_overlay(&block.html, base, over, &mut new_html);
    block.html = new_html;

    // Resolve the structured data channel (if any) from the snapshot, replaying
    // the same document order so the ids match `html` exactly.
    let resolve_one = |s: &str| -> String {
        let mut seed = data_seed.clone();
        let mut o = String::with_capacity(s.len());
        resolve_footnote_ids_overlay(s, base, &mut seed, &mut o);
        o
    };
    // The `Rc`-shared payload entries are freshly produced here (strong count 1
    // on the full-reparse path), so `Rc::make_mut` / re-wrapping never clones a
    // cache-frozen entry.
    match &mut block.kind {
        BlockKind::Table(Some(td)) => {
            for cell in Rc::make_mut(&mut td.headers) {
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
                // Only `html` is re-resolved; the source offset carries through.
                *it = Rc::new(ListItemData { html: resolve_one(&it.html), start: it.start });
            }
        }
        BlockKind::Blockquote(Some(cd)) => {
            for nb in &mut cd.nested {
                *nb = Rc::new(NestedBlock { html: resolve_one(&nb.html) });
            }
        }
        BlockKind::Alert { nested: Some(cd), .. } => {
            for nb in &mut cd.nested {
                *nb = Rc::new(NestedBlock { html: resolve_one(&nb.html) });
            }
        }
        _ => {}
    }
}

/// Split a list slice into its item slices (by lines that begin a sibling
/// marker at the list's own indentation). A light re-implementation used only
/// for ref-def harvesting; rendering does its own item splitting.
fn split_list_items<'a>(slice: &'a str, ctx: ScanCtx<'_>) -> Vec<&'a str> {
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
                if let Some(m) = scan_marker(line, ctx) {
                    edge = m.marker_indent;
                    cur_ci = m.content_indent;
                    starts.push(pos);
                }
            } else if ind < cur_ci && ind <= edge + 3 {
                if let Some(m) = scan_marker(line, ctx) {
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
    /// Keep each committed block's rendered `html` in `committed_blocks` after
    /// the block has been emitted (as `newly_committed`) in a patch. ON by
    /// default: the uniffi / C bindings and the one-shot `allBlocks` SSR
    /// primitive read the whole rendered document back out of the parser.
    ///
    /// A pure STREAMING consumer — the npm worker — reads each committed block
    /// exactly once, off the patch, and never calls [`Self::all_blocks`]; there
    /// the retained html pins the entire rendered document for the life of the
    /// stream on behalf of nobody. With this OFF the html `String` is released
    /// at commit, so retention is the source buffer plus the open tail (see
    /// [`Self::all_blocks`] for what that costs, and [`Self::retained_bytes`],
    /// which reflects it). Only the html payload drops — block metadata stays,
    /// and an opt-in `kind.data` payload ([`Self::set_block_data`]) is untouched.
    ///
    /// NEVER propagated to a NESTED parser ([`Self::make_nested_parser`] and the
    /// component cache's inline twin): the container / open-item assemblers read
    /// their inner parser's COMMITTED html AFTER the commit — the open-item one
    /// on every append, the wrapper one exactly once per block (into its
    /// [`WrappedBodyCache`]) — so the html has to survive the commit either way.
    retain_committed_html: bool,
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
    // `Rc` (like `committed_refs`) so each reparse / tail cache shares the
    // committed tables with `RenderOpts` in O(1) instead of cloning per append;
    // folded via `Rc::make_mut` at commit time.
    committed_footnotes: Rc<HashMap<String, usize>>,
    committed_footnote_defs: Rc<HashMap<String, String>>,
    /// Total references per label in the committed region — seeds the tail's
    /// occurrence counter so repeated-reference ids stay unique across commits.
    committed_footnote_occurrences: Rc<HashMap<String, usize>>,
    next_footnote: usize,
    unsafe_html: bool,
    gfm_autolinks: bool,
    gfm_alerts: bool,
    /// GFM "Disallowed Raw HTML" (tagfilter): in `unsafe_html` pass-through,
    /// the nine disallowed tags render with their `<` escaped. Off by default
    /// and NOT implied by `unsafe_html` (strict CommonMark wants `<script>`
    /// verbatim there).
    gfm_tagfilter: bool,
    gfm_footnotes: bool,
    gfm_math: bool,
    dir_auto: bool,
    /// Treat a list marker followed by 6+ columns of SPACE padding as the item's
    /// text instead of an indented code block. Off by default (strict CommonMark).
    lenient_lists: bool,
    /// Render a soft line break as `<br>` (the "GitHub comment" convention). Off by
    /// default (strict CommonMark: a soft break is whitespace).
    soft_breaks: bool,
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
    /// Extend the sanitizer to BLOCK-level raw HTML (see
    /// [`StreamParser::set_block_html`]). Only meaningful with `html_sanitize`
    /// on, and only for HTML block types 6/7. Off by default (block raw HTML
    /// stays escaped, byte-identical to before).
    block_html: bool,
    /// Opt-in URL-scheme un-blocklist (see [`StreamParser::set_allow_schemes`]).
    /// BARE scheme names, no colon (`["file"]`), matched case-insensitively.
    /// Empty by default — every default-blocked scheme stays blocked.
    allow_schemes: Vec<Box<str>>,
    /// Fast path for a long open code/math fence at the tail (see [`FenceCache`]).
    fence_cache: Option<FenceCache>,
    /// Fast path for a long open paragraph at the tail (see [`ParagraphCache`]).
    para_cache: Option<ParagraphCache>,
    /// Fast path for a long open GFM table at the tail (see [`TableCache`]).
    table_cache: Option<TableCache>,
    /// Fast path for a long open blockquote / alert whose inner content is PLAIN
    /// PARAGRAPHS (see [`ContainerCache`]).
    container_cache: Option<ContainerCache>,
    /// Fast path for a long open blockquote / alert whose inner content has BLOCK
    /// structure — a list, nested quote, heading, table, fence (see
    /// [`ContainerBlockCache`]). Renders the `>`-stripped inner via a recursive
    /// nested `StreamParser` so structured containers stream in O(total bytes)
    /// instead of re-parsing the whole growing tail every append.
    container_block_cache: Option<ContainerBlockCache>,
    /// Iterative fast path for a monotonically-deepening nested-blockquote
    /// staircase — the shape the recursive [`ContainerBlockCache`] streams
    /// worse-than-quadratically. Byte-identical to that path but with no per-level
    /// call-stack recursion (see [`DeepQuoteCache`]). Top-level, `block_data` off.
    deep_quote_cache: Option<DeepQuoteCache>,
    /// True only for a NESTED parser owned by a [`ContainerBlockCache`]. When set,
    /// every block this parser produces (committed AND active) renders with
    /// `open_tail = true`, matching `render_blockquote` / `render_alert`, which
    /// propagate the open container's `open_tail` to ALL inner sub-blocks. The
    /// top-level parser leaves this `false` (open_tail is per-block, last-only).
    force_open_tail: bool,
    /// Container-nesting depth of THIS parser (0 = top level; a nested
    /// container-block parser is +1). Bounds the recursive
    /// [`ContainerBlockCache`] so an adversarial `">".repeat(N)` can't overflow
    /// the WASM shadow stack — past [`MAX_CONTAINER_DEPTH`] we fall back to the
    /// (depth-bounded) full reparse.
    container_depth: usize,
    /// Fast path for a long open tight, flat list at the tail (see [`ListCache`]).
    list_cache: Option<ListCache>,
    /// Fast path for a long open indented-code block at the tail (see [`IndentedCodeCache`]).
    indented_cache: Option<IndentedCodeCache>,
    /// Fast path for a long open raw-HTML block at the tail (see [`HtmlBlockCache`]).
    html_cache: Option<HtmlBlockCache>,
    /// A chunk ended in a bare `\r` whose line ending is not yet decided: the
    /// next appended byte tells whether it was `\r\n` (one `\n`) or a lone `\r`
    /// (also one `\n`, per CommonMark). The `\r` is held OUT of `buffer` until
    /// then so a `\r|\n` split across appends stays chunk-independent;
    /// `finalize` flushes a still-pending `\r` as `\n`. See [`Self::ingest`].
    pending_cr: bool,
    /// Fast path for a long open component block (`<Tag>` whose close hasn't
    /// arrived) at the tail (see [`ComponentBlockCache`]). Like the structured
    /// container cache, the markdown body streams through a recursive nested
    /// `StreamParser` in O(new bytes).
    component_cache: Option<ComponentBlockCache>,
    /// Fast path for a still-growing single-line ATX heading at the tail (see
    /// [`HeadingCache`] — the paragraph cache's settled-prefix scheme in an
    /// `<hN>` wrapper).
    heading_cache: Option<HeadingCache>,
    /// Fast path for a still-growing thematic-break line at the tail (see
    /// [`RuleCache`] — the output is a constant `<hr>`).
    rule_cache: Option<RuleCache>,
    /// Fast path for a code fence whose OPENING line (the info string) is still
    /// growing (see [`FenceInfoCache`] — output depends only on the settled
    /// first info word, so it is frozen).
    fence_info_cache: Option<FenceInfoCache>,
    /// Fast path for a paragraph that is one open, unclosed inline-math span
    /// opened by an unmatched single `$` at the paragraph start — the `$x $x …`
    /// soup where the commit cut is pinned at 0 (a future closer could pair
    /// back) so no [`ParagraphCache`] can arm (see [`DollarTailCache`]).
    dollar_tail_cache: Option<DollarTailCache>,
    /// Fast path for a paragraph that is one maximal pure-ASCII-alphanumeric run
    /// at the paragraph start — the `aaaa…` giant word. With extended autolinks
    /// on, no byte in the run is a boundary candidate (a future `@`/`.` could
    /// bind the whole run right-to-left into an autolink), so the commit cut
    /// pins at 0 and no [`ParagraphCache`] can arm (see [`AlnumTailCache`]).
    alnum_tail_cache: Option<AlnumTailCache>,
    /// Fast path for a paragraph that is one never-closing raw open tag whose
    /// unclosed quoted attribute value grows to EOF — `<a href="…` under
    /// sanitize/unsafe HTML. The failed-`<` unstable mark pins the cut at the
    /// `<`, so no [`ParagraphCache`] can arm (see [`RawTagTailCache`]).
    raw_tag_tail_cache: Option<RawTagTailCache>,
    /// Fast path for a paragraph opening `…**…` whose lone `**` run (can-open
    /// AND can-close, so the mod-3 rule is live) is permanently unpaired — the
    /// `a**bc* c* …` soup where every later single `*` is a mod-3-blocked
    /// closer. The unpaired can-open `**` pins the commit cut at 0, so no
    /// [`ParagraphCache`] can arm (see [`Mod3TailCache`]).
    mod3_tail_cache: Option<Mod3TailCache>,
    /// Opt-in wire delta mode (`WIRE.md` §11): when on, each patch's
    /// `active_deltas` is populated so re-emitted active blocks serialize as
    /// verified splices (`html_delta`) against their previous emit instead of
    /// their full `html`, making total emitted bytes O(n) for a block that
    /// grows across many appends. Off by default (wire byte-identical to
    /// pre-delta releases).
    wire_delta: bool,
    /// The `active` array as last emitted (id, html, UTF-16 length of html),
    /// maintained only in `finish_patch` and only when `wire_delta` is on —
    /// deliberately independent of `active_blocks`, which internal fast paths
    /// mutate between emits. This is the base the next patch's deltas splice
    /// against, and mirrors exactly the state a conforming consumer holds.
    last_wire_active: Vec<(u64, String, usize)>,
}

#[derive(Default)]
pub struct Patch {
    pub newly_committed: Vec<Block>,
    pub active: Vec<Block>,
    /// Wire-delta metadata for `active`, populated only when the parser was
    /// configured with [`StreamParser::set_wire_delta`]: either empty (delta
    /// mode off — every active block serializes with its full `html`) or
    /// exactly `active.len()` entries, where `Some(d)` means entry `i`
    /// serializes as a splice against the previous emit of the same block id
    /// (`html_delta` instead of `html` — see `WIRE.md` §11). The parser
    /// guarantees a `Some` delta's base block id was present in the previous
    /// patch's `active` array.
    pub active_deltas: Vec<Option<HtmlDelta>>,
}

/// A verified splice of an active block's `html` against the previous emit of
/// the same block id: the first `keep_bytes` bytes (equivalently the first
/// `keep_units` UTF-16 code units) of the previous `html` are byte-identical
/// and are kept; everything after is replaced by `html[keep_bytes..]`. Both
/// counts always fall on character boundaries. Produced by comparison, never
/// by structural inference, so a delta can never reconstruct wrong bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HtmlDelta {
    /// Kept prefix length in UTF-8 bytes (for byte-oriented consumers).
    pub keep_bytes: usize,
    /// Kept prefix length in UTF-16 code units (for JS/JVM-style consumers).
    pub keep_units: usize,
}

/// Length of the shared byte prefix of `a` and `b`, 8 bytes per step (the same
/// SWAR shape as `scanner::line_end`); the wire delta path runs this over the
/// previous emit on every append, so it is deliberately word-at-a-time.
fn common_prefix_bytes(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 8 <= n {
        let x = u64::from_le_bytes(a[i..i + 8].try_into().unwrap());
        let y = u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
        let d = x ^ y;
        if d != 0 {
            return i + (d.trailing_zeros() >> 3) as usize;
        }
        i += 8;
    }
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

/// UTF-16 code-unit length of `s` — the unit JS/JVM consumers slice by.
fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
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
    /// Absolute byte offset where the fence BODY begins in `buffer` (past the
    /// opener line for code; past the delimiter + leading whitespace for math).
    /// The assembled HTML body is exactly `escape_html(buffer[body_start..end])`
    /// plus the trailing trim, so the opt-in `kind.data` source derives from this
    /// RAW slice directly — no per-append whole-body entity decode.
    body_start: usize,
    /// Columns of indentation on the OPENER line, which every body line sheds
    /// (§4.5 — see [`crate::render::fence_indent`]). 0 for math fences and for
    /// the overwhelmingly common flush-left code fence, which is the only case
    /// where the folds below stay pure `escape_html`.
    fence_indent: usize,
    /// Escaped HTML of the complete body lines, joined by `\n`, no trailing `\n`.
    escaped_lines: String,
    /// RAW (unescaped) twin of `escaped_lines`, de-indented identically —
    /// `Some` iff `fence_indent > 0`. When the opener is flush left the decoded
    /// source is just the buffer slice `[body_start..end]`, so nothing is kept;
    /// once lines are being de-indented that slice no longer matches the HTML,
    /// and re-deriving it per append would be an O(body) rescan (an O(n²) wall),
    /// so the de-indented raw text folds in line-by-line right beside the
    /// escaped one.
    decoded_lines: Option<String>,
    /// Whether ≥1 complete body line has been folded in. Drives the `\n` line
    /// JOIN separator — NOT `!escaped_lines.is_empty()`, which would swallow a
    /// LEADING blank body line (` ```\n\nx ` must keep its `\n` before `x`).
    has_body_line: bool,
    /// Absolute offset just past the last complete body line's `\n`.
    lines_upto: usize,
}

/// True if `needle` occurs anywhere in `haystack` (used for the math closer).
fn line_contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.len() <= haystack.len() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Incremental footnote-NUMBERING state for a tail cache's region. The full
/// path's numbering pre-pass re-collects `[^label]` refs over the WHOLE region
/// per append (O(region), so a ref-heavy open block streams O(n²) wall even
/// when the byte counter looks linear). First-occurrence numbering over an
/// append-only region is prefix-stable, so each cache keeps this state and
/// extends it over only the NEW bytes (see [`extend_footnote_refs`] for the
/// exact region-edge classification rules this relies on). The committed layer
/// is Rc-shared through `RenderOpts::footnotes`; region-local labels live in
/// the `tail` overlay, Rc-shared through `RenderOpts::tail_footnotes` — O(1)
/// per append, no map clones. Inert (empty, never extended) when footnotes off.
struct RegionFnNums {
    /// label → number for labels first referenced inside the region.
    tail: Rc<HashMap<String, usize>>,
    /// Next number to assign (continues committed + `tail`). Frozen relative to
    /// the parser's `next_footnote` while the cache is armed (no commits).
    next: usize,
    /// Absolute buffer offset; numbering over `[region_start..upto)` is settled.
    upto: usize,
    /// Label numbered speculatively because its `]` was the region's final byte
    /// (a `:` arriving next would make it a def opener, not a ref). Retracted at
    /// the start of the next extension, which re-classifies from the same `[`.
    spec: Option<String>,
}

impl RegionFnNums {
    fn new(region_start: usize, next: usize) -> Self {
        Self { tail: Rc::new(HashMap::new()), next, upto: region_start, spec: None }
    }

    /// Extend the numbering over `buffer[upto..region_end)`. `committed` is the
    /// parser's committed numbering table (the other first-wins layer).
    fn extend(&mut self, buffer: &str, region_end: usize, committed: &HashMap<String, usize>) {
        if let Some(label) = self.spec.take() {
            // The cache is the sole Rc holder between appends (per-append
            // RenderOpts clones are dropped with the patch), so this mutates in
            // place. The speculative label was the last number assigned and
            // nothing was assigned after it — pop it and re-classify below.
            Rc::make_mut(&mut self.tail).remove(&label);
            self.next -= 1;
        }
        if self.upto >= region_end {
            return;
        }
        // Deterministic complexity probe (mirrors the `reparse_tail` counter):
        // this scan is cache-internal, so without it a numbering-pre-pass
        // regression (`upto` stalling → whole-region re-scans) would be
        // counter-invisible even though the wall goes quadratic.
        #[cfg(feature = "perf_counters")]
        crate::perf::add_scan(region_end - self.upto);
        let (upto, spec) = extend_footnote_refs(
            &buffer[..region_end],
            self.upto,
            committed,
            Rc::make_mut(&mut self.tail),
            &mut self.next,
        );
        self.upto = upto;
        self.spec = spec;
    }
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
    /// Incremental footnote NUMBERING over the paragraph region (see
    /// [`RegionFnNums`]). Inert when footnotes are off.
    fn_nums: RegionFnNums,
}

/// Incremental render state for an open paragraph that is one speculative,
/// still-unclosed single-`$` inline-math span running from the paragraph start
/// to EOF — the `$x $x $x …` soup (a streaming LLM formula, currency-ish prose).
///
/// The commit cut is genuinely pinned at the opener: any `$` appended later with
/// a non-space to its left is a valid closer, and the first `$` would then pair
/// forward across the ENTIRE run, so nothing before EOF is ever final. No
/// [`ParagraphCache`] can arm (its cut would be 0), so today every append
/// re-scans + re-renders the whole growing paragraph — O(n²) on both counters.
///
/// But while the span stays open its rendered form is fixed: `<span class="math
/// math-inline">` + `escape_html(body)` + `</span>`, where `body` is every byte
/// after the opener. `escape_html` is a context-free per-byte map, so `body`
/// extends by appending `escape_html(new bytes)` — O(new bytes) per append. The
/// guard (see [`dollar_span_stays_open`] / [`build_dollar_tail_cache`])
/// engages ONLY while the span is provably still open and the paragraph is still
/// a single tail line; the moment a valid closer, a newline, or a `$$` run
/// appears the cache drops and the full reparse takes over (byte-identical). At
/// finalize (open_tail off) the fast paths are skipped, so the span resolves to
/// literal `$`s exactly as the one-shot oracle does.
struct DollarTailCache {
    /// Absolute byte offset of the opening `$` (== the paragraph start).
    start: usize,
    /// Stable id of the paragraph block.
    id: u64,
    /// Absolute offset; `math == escape_html(buffer[start + 1 .. scanned])` and
    /// `buffer[start .. scanned]` is newline-free with no valid closer. Advances
    /// by the appended bytes each engagement.
    scanned: usize,
    /// Escaped inline-math body rendered so far (`escape_html(buffer[start+1..scanned])`).
    math: String,
}

/// Incremental render state for an open paragraph that is one maximal run of
/// pure ASCII alphanumerics from the paragraph start to EOF — the `aaaa…` giant
/// word (a bare token, a base64/hex blob, a long identifier a stream hasn't yet
/// spaced).
///
/// With extended autolinks on the commit cut is genuinely pinned at 0: no alnum
/// byte is a boundary candidate ([`synth_boundary`] excludes them because a
/// future `@`/`.` could bind the whole run right-to-left into an email/`www.`
/// autolink), and there is no inter-word space, so [`compute_cut`] returns 0 and
/// no [`ParagraphCache`] can arm — every append re-scans + re-renders the whole
/// growing paragraph (O(n²) on both counters).
///
/// But a pure-alnum run can NEVER complete an autolink (`try_ext_autolink` needs
/// `http://`/`www.`, `try_ext_email` needs `@` — all require punctuation the run
/// lacks) nor open any other inline construct, and `escape_html` leaves every
/// alnum byte unchanged. So while the run stays pure-alnum its render is fixed —
/// `<p>` + `escape_html(body)` + `</p>` — and the escaped body extends by only
/// the appended bytes (O(new)). The guard (see [`alnum_run_stays_open`]) drops
/// to the byte-identical full path the instant a non-alnum byte appears (a
/// space, a `.`/`@`/`:` that could settle or trigger a construct, a newline) —
/// so one retro re-render per settling byte, amortized linear.
struct AlnumTailCache {
    /// Absolute byte offset of the run start (== the paragraph start).
    start: usize,
    /// Stable id of the paragraph block.
    id: u64,
    /// Absolute offset; `buffer[start .. scanned]` is all ASCII alphanumeric and
    /// `body == escape_html(buffer[start .. scanned])`. Advances by the appended
    /// bytes each engagement.
    scanned: usize,
    /// Escaped paragraph body rendered so far (`escape_html(buffer[start..scanned])`;
    /// for pure alnum this equals the bytes themselves).
    body: String,
}

/// Incremental render state for an open paragraph that is one never-closing raw
/// open tag whose unclosed quoted attribute value grows to EOF — `<a href="…`
/// under sanitize/unsafe HTML (a backend streaming `<a href="…">` instead of a
/// markdown link, before the closing quote / `>` arrives).
///
/// The failed-`<` unstable mark (which pre-dates the 0.20.3 open-tail tag
/// suppression) pins the commit cut at the `<`, so no [`ParagraphCache`] can arm
/// and the growing suffix re-renders every append. But while the tag streams to
/// EOF inside its quoted value, [`inline_html_streams_to_eof`] suppresses it —
/// the paragraph renders to the CONSTANT `<p></p>` (see
/// [`open_tag_streaming_quote`]). So the fast path is O(1) per append: it only
/// tracks whether appended bytes keep the value unclosed. The guard (see
/// [`raw_tag_stays_open`]) drops to the byte-identical full path the instant the
/// value's matching quote closes (the tag can now complete or gain attrs) or a
/// newline splits the single-line paragraph. Only engaged when raw HTML is
/// active (sanitize/unsafe); in escape mode the `<` renders as visible `&lt;…`
/// text, a different (still-pinned, out-of-scope) shape.
struct RawTagTailCache {
    /// Absolute byte offset of the opening `<` (== the paragraph start).
    start: usize,
    /// Stable id of the paragraph block.
    id: u64,
    /// Absolute offset; `buffer[start .. scanned]` is a newline-free open tag
    /// streaming to EOF inside an unclosed `quote`-delimited attribute value.
    /// Advances by the appended bytes each engagement.
    scanned: usize,
    /// The open attribute value's delimiter (`b'"'` or `b'\''`). A matching quote
    /// in the appended bytes closes the value and drops the cache.
    quote: u8,
}

/// Incremental render state for an open paragraph opening `X**Y…` whose single
/// `**` run is permanently unpaired — the `a**bc* c* c* …` soup (a lone `**`
/// followed by a growing run of mod-3-blocked single-`*` closers).
///
/// The leading `**` sits between two non-`*` non-space bytes, so it can both
/// OPEN and CLOSE — which keeps the CommonMark mod-3 rule live: every later
/// single `*` is right-flanking-only (a non-space to its left, a space to its
/// right) and tries to close it, but `2 + 1 ≡ 0 (mod 3)` with neither length a
/// multiple of 3 blocks the pairing. So the `**` stays on the delimiter stack
/// as an unpaired can-open run; [`compute_cut`]'s `earliest_open` pins the
/// commit cut at 0 and no [`ParagraphCache`] can arm — today every append
/// re-scans + re-renders the whole growing paragraph (O(n²) on both counters).
///
/// But while the `**` stays the sole opener, no `*` in the body can pair (the
/// closers are can-close-only, and the `**` is mod-3-blocked against every one
/// of them), so the whole paragraph renders as literal escaped text — `<p>` +
/// `escape_html(body)` (trailing whitespace stripped) + `</p>` — and
/// `escape_html` is a context-free per-byte map, so the settled body extends by
/// only the appended bytes. The guard (see [`mod3_body_scan`] /
/// [`build_mod3_tail_cache`]) settles a byte only when it is provably literal —
/// an ASCII-alnum/space/tab text byte, or a single `*` decidably closer-only
/// (a non-`*` follows and it is whitespace) — and drops to the byte-identical
/// full path the moment a byte could restructure the render: a `*` run of
/// length ≥ 2 (a second `**` pairs the leading one, `2 + 2 ≢ 0 (mod 3)` →
/// `<strong>`), a single `*` that could open (a non-space follows it), a
/// newline, or any construct/entity/non-ASCII byte. A `*` run abutting the
/// chunk edge is left PENDING (its length/flanking undecided until the next
/// append) — held out of the settled body but rendered literally in the open
/// view, exactly as the full path renders a trailing `*` at EOF.
struct Mod3TailCache {
    /// Absolute byte offset of the paragraph start (== `body`'s first byte).
    start: usize,
    /// Stable id of the paragraph block.
    id: u64,
    /// Absolute offset; `buffer[start .. settled]` is all provably-literal and
    /// `body == escape_html(buffer[start .. settled])`. `buffer[settled .. len]`
    /// is the PENDING trailing `*` run (0 or 1 byte — a run reaching length 2
    /// drops). Advances by the newly-settled bytes each engagement.
    settled: usize,
    /// Escaped paragraph body settled so far (`escape_html(buffer[start..settled])`;
    /// for the literal soup this equals the bytes themselves).
    body: String,
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
    /// Per-column alignment (parsed once from the delimiter row). `Rc`-shared so
    /// the per-patch `TableData` emit is a refcount bump, not a `Vec` clone.
    aligns: Rc<Vec<Option<&'static str>>>,
    /// `true` once we've emitted `<tbody>` into `cached_prefix` (after the first
    /// committed body row). The trailing partial-row path emits its own `<tbody>`
    /// when speculatively rendering the very first row of the body.
    tbody_opened: bool,
    /// Structured `kind.data` channel (only populated when `block_data` is on):
    /// the header cells (locked once, parallel to the `<thead>` in
    /// `cached_prefix`), `Rc`-shared so the per-patch emit is a refcount bump.
    /// Empty + unused when off.
    header_cells: Rc<Vec<TableCell>>,
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
    /// Incremental footnote NUMBERING over the table region (see
    /// [`RegionFnNums`]). Inert when footnotes are off.
    fn_nums: RegionFnNums,
    /// Incremental state for the trailing newline-less partial row, so the
    /// growing partial is split + rendered in O(new bytes) instead of whole
    /// per append. `None` until the first partial byte arrives (and always
    /// `None` when `block_data` is on — that path re-renders whole).
    partial: Option<PartialRowCache>,
}

/// Two-level incremental render state for the TRAILING newline-less partial
/// row of an open [`TableCache`] table. Without it, every append re-splits and
/// re-inline-renders the whole growing partial line — O(n²/chunk) once the
/// line stops getting `\n`s (one giant last cell, or one enormously wide row).
///
/// Level 1 — settled cells: a cell closed by an unescaped `|` can never
/// change (earlier bytes are immutable, and the cells before any later `|`
/// are the same whether that pipe turns out to be the row's trailing
/// decoration or another delimiter), so each is split + rendered exactly once
/// into `html`; only bytes at `scanned..` are examined per append. The scan
/// mirrors `split_table_cells` char-for-char: leading whitespace plus one
/// leading `|` are decoration, `\` swallows itself and makes the next char
/// literal, an unescaped `|` closes a cell, and each cell is trimmed.
///
/// Level 2 — the still-open last cell: `render_inline_boundary` (the
/// paragraph-cache primitive) commits the settled inline prefix of the cell's
/// content into `cell_committed` and re-renders only the short unsettled tail
/// per append.
///
/// Footnote-aware with the same cascading discipline as the row cache: `occ`
/// advances per frozen cell, `cell_occ` (re-seeded from `occ`) advances per
/// committed inline segment and is discarded when the cell closes, and the
/// speculative active tail resolves from a clone.
///
/// Not used when `block_data` is on: the structured `TableCell {text, html}`
/// channel needs a full `strip_inline_html` pass over the growing cell each
/// append, so that path keeps the whole-partial re-render.
struct PartialRowCache {
    /// `lines_upto` this state belongs to; a mismatch (a row completed since)
    /// resets the whole sub-cache.
    line_start: usize,
    /// Absolute offset of the first byte not yet fed through the split scan.
    scanned: usize,
    /// Split-level escape state at `scanned` (`\` seen; next char is literal).
    esc: bool,
    /// Still consuming the line's leading whitespace / one leading `|`.
    leading: bool,
    /// Absolute offset of the last non-whitespace char seen (drives the
    /// trailing-`|`-decoration emulation at emit time).
    last_nonws: Option<usize>,
    /// Cells closed (frozen) so far. Can exceed `ncol`; cells past `ncol` are
    /// counted but not rendered (the renderer drops them).
    ncells: usize,
    /// Absolute offset just past the last `|` the scan consumed as a cell
    /// delimiter (or as the leading decoration pipe).
    frozen_end: usize,
    /// Rendered `<td>…</td>` HTML of the frozen cells, footnotes resolved.
    html: String,
    /// Footnote occurrences advanced through `html` (seeded from the row
    /// cache's `fn_occ` clone at arm time).
    occ: HashMap<String, usize>,
    /// Escape-processed, left-trimmed content of the OPEN (last) cell —
    /// exactly what `split_table_cells` would accumulate for it.
    cellbuf: String,
    /// `cellbuf` length through its last non-whitespace char (right trim).
    trim_len: usize,
    /// Resolved HTML of the settled inline prefix of the open cell.
    cell_committed: String,
    /// Settled inline boundary — byte offset into the open cell's content.
    cell_cut: usize,
    /// Content length `cell_cut` was last computed against. The boundary
    /// contract only covers *extensions* of the analyzed input; the
    /// trailing-`|` emulation can shrink the content, which resets level 2.
    cell_len: usize,
    /// Footnote occurrences advanced through `cell_committed` (seeded from
    /// `occ`; discarded whenever the open cell closes or resets).
    cell_occ: HashMap<String, usize>,
}

impl PartialRowCache {
    fn new(line_start: usize, fn_occ: &HashMap<String, usize>) -> Self {
        PartialRowCache {
            line_start,
            scanned: line_start,
            esc: false,
            leading: true,
            last_nonws: None,
            ncells: 0,
            frozen_end: line_start,
            html: String::new(),
            occ: fn_occ.clone(),
            cellbuf: String::new(),
            trim_len: 0,
            cell_committed: String::new(),
            cell_cut: 0,
            cell_len: 0,
            cell_occ: fn_occ.clone(),
        }
    }

    /// Append one escape-processed char to the open cell, trimming
    /// incrementally: leading whitespace is skipped while the cell is empty,
    /// and `trim_len` remembers the length through the last non-whitespace
    /// char (the emit slices to it — `.trim()` equivalence).
    fn push_cell_char(&mut self, ch: char) {
        if self.cellbuf.is_empty() && ch.is_whitespace() {
            return;
        }
        self.cellbuf.push(ch);
        if !ch.is_whitespace() {
            self.trim_len = self.cellbuf.len();
        }
    }
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
    /// `partial_html` in the table cache). `Rc`-shared so the per-patch re-emit
    /// is O(paras) refcount bumps, not O(para bytes) `String` clones. Empty +
    /// unused when off.
    committed_paras: Vec<Rc<NestedBlock>>,
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
    /// Footnote occurrence OVERLAY (layered over `fn_occ`, which is never
    /// cloned) for the current OPEN paragraph's settled prefix
    /// (`committed_inner_html`). Cleared when a paragraph starts; advanced as
    /// the inline boundary commits segments into `committed_inner_html`; holds
    /// only the open paragraph's own labels. The active tail resolves from a
    /// CLONE of this (small) overlay. Because the open paragraph is re-rendered
    /// whole on close (advancing the persistent `fn_occ`), the overlay is
    /// discarded then — no double-count.
    inner_fn_occ: HashMap<String, usize>,
    /// Incremental footnote NUMBERING over the container region (marker-included
    /// raw slice, mirroring the full path's pre-pass; see [`RegionFnNums`]).
    /// Inert when footnotes are off.
    fn_nums: RegionFnNums,
}

#[derive(Clone, Copy)]
enum ContainerCacheKind {
    Blockquote,
    Alert(crate::blocks::AlertKind),
}

/// Settled prefix of one wrapper's assembled body (see [`assemble_wrapped_body`]).
///
/// The two wrapper caches ([`ContainerBlockCache`], [`ComponentBlockCache`]) rebuild
/// their open block's HTML every append. Without this the rebuild walked the nested
/// parser's whole block list and re-`push_str`'d every already-COMMITTED sub-block's
/// html out of its own heap allocation — O(body) of scattered small copies per
/// append, i.e. O(n²/chunk) over a body that commits many sub-blocks (a component
/// whose body is paragraphs commits one per blank line).
///
/// A nested parser's `committed_blocks` is append-only and its html is frozen at
/// commit, so the wrapper opener plus every committed sub-block is a stable PREFIX:
/// folded here exactly once, then re-emitted as ONE contiguous copy. Only the
/// speculative `active_blocks` tail is rebuilt per append.
///
/// NOTHING speculative ever enters `settled` — the active tail and the wrapper
/// closer are appended to the freshly returned `String` instead — so there is no
/// truncate-back mark to keep, and the buffer's capacity is reused across appends.
///
/// One of these belongs to each nested parser: the component cache alternates
/// between `inner` and its `force_open_tail`-off SETTLED twin, whose committed html
/// deliberately differs, so a single shared prefix would be wrong.
struct WrappedBodyCache {
    /// `wrapper_open`, then the body-leading `cr()` and each committed sub-block's
    /// html followed by its own `cr()` — byte-identical to the prefix
    /// [`assemble_wrapped_body`] used to rebuild from scratch.
    settled: String,
    /// How many of the nested parser's `committed_blocks` are already folded in.
    /// Also the "has a block been emitted yet" flag the body-leading `cr()` needs.
    blocks: usize,
}

impl WrappedBodyCache {
    /// Seed the prefix with the (immutable, never-empty) wrapper opener.
    fn new(wrapper_open: &str) -> Self {
        WrappedBodyCache { settled: String::from(wrapper_open), blocks: 0 }
    }
}

/// Incremental render state for an open blockquote / alert whose inner content
/// has BLOCK STRUCTURE (a list, nested quote, heading, table, fence, …) — the
/// shape the plain-paragraph [`ContainerCache`] bails on. The whole container is
/// one atomic, never-committing block, so the full reparse re-scans its growing
/// tail every append (O(n²)); this cache renders the `>`-stripped inner through
/// a RECURSIVE nested [`StreamParser`] that streams in O(new bytes).
///
/// Byte-parity rests on `render_blockquote` / `render_alert` (render.rs) being
/// the SAME `scan()` + `render_block()` engine run on the `>`-stripped inner: a
/// fresh `StreamParser` fed that identical stripped inner reproduces the inner
/// HTML exactly (chunk-independence), and `force_open_tail` makes its committed
/// sub-blocks match the full path's all-blocks-open_tail propagation. Link-ref
/// definitions inside the container are handled NATIVELY by the nested parser
/// (its own def-run commit keeps them O(new bytes)); document-global resolution
/// is preserved because the container only ever commits through a full outer
/// reparse, which re-collects refs from source. The cache BAILS to the full
/// reparse (correct, just O(n²)) on anything the nested parser can't reproduce
/// identically: a `\r` (CRLF), a non-`>` lazy continuation line, or a footnote
/// ref/def (`[^` with footnotes on — numbering is document-global).
///
/// `block_data` is owned here too: the nested `ContainerData` channel emits one
/// `NestedBlock { html }` per inner block, which matches `render_blockquote` /
/// `render_alert` byte-for-byte because those build their `nested` fragments at
/// exactly the per-sub-block boundaries this cache joins with `\n` (the same
/// invariant the wrapper HTML parity rests on).
struct ContainerBlockCache {
    /// Absolute byte offset of the container's first line in `buffer`.
    start: usize,
    /// Stable id of the container block (preserved across appends and the close).
    id: u64,
    /// Container variant — only drives which wrapper strings were precomputed.
    kind: ContainerCacheKind,
    /// Wrapper closer: `</blockquote>` or `</div>`.
    wrapper_close: String,
    /// `assemble_wrapped_body`'s `lead_when_empty`: true for a blockquote, whose
    /// body-leading `\n` is unconditional (`<blockquote>\n</blockquote>` for an
    /// empty quote); false for an alert, whose title line already ends in `\n`
    /// (so the `cr()` is a no-op either way).
    body_leading_nl: bool,
    /// The recursive parser rendering the `>`-stripped inner markdown. Fed only
    /// the per-append delta; its `all_blocks()` are the inner sub-blocks.
    inner: Box<StreamParser>,
    /// Settled prefix of `inner`'s assembled body (see [`WrappedBodyCache`]), so
    /// an append re-emits the committed sub-blocks as one contiguous copy instead
    /// of walking them. SEEDED with the wrapper opener WITHOUT the conditional
    /// body-leading `\n`: `<blockquote dir?>` for a blockquote, or the full
    /// `<div …>\n<p …title>Title</p>\n` for an alert.
    body: WrappedBodyCache,
    /// Absolute outer-buffer offset already turned into fed inner content.
    fed_outer: usize,
    /// True when mid inner-line: the current line's `>` prefix was already
    /// consumed and partial content fed; continuation bytes feed raw (no prefix).
    mid_line: bool,
    /// Last byte fed to `inner` (for catching a `[^` marker split across two
    /// feeds when footnotes are on). `None` until the first non-empty feed.
    last_fed: Option<u8>,
    /// Whether the outer parser has footnotes on (then a `[^` inner marker BAILS,
    /// since the nested parser runs with footnotes off).
    footnotes: bool,
    /// Structured `kind.data` channel (only populated when the OUTER parser's
    /// `block_data` is on — the nested parser itself runs with it off): one
    /// `Rc`-shared `NestedBlock` per COMMITTED inner sub-block, folded exactly
    /// once when the nested parser commits it (committed blocks never change).
    /// The still-open inner blocks are appended fresh per emit, mirroring the
    /// table cache's committed-rows-then-partial pattern. Empty + unused when off.
    committed_nested: Vec<Rc<NestedBlock>>,
}

/// Deepest cache depth [`DeepQuoteCache`] renders before it hands the tail back to
/// the full reparse. It equals the byte-parity boundary of the recursive
/// [`ContainerBlockCache`] path it replaces: that path caches
/// [`MAX_CONTAINER_DEPTH`] nested-parser levels (each consuming NO render budget),
/// then the innermost full-reparses, whose `render_block` recursion truncates at
/// [`render::MAX_RENDER_DEPTH`] (100) — so the streamed staircase renders exactly
/// `MAX_CONTAINER_DEPTH + 100` blockquotes deep, then escapes the remainder.
/// `DeepQuoteCache` reproduces that byte-for-byte up to this bound and BAILS the
/// instant a deeper level would cross it, so the (adversarial, past-real-document)
/// truncation regime stays on the original path unchanged.
const DEEP_QUOTE_MAX_DEPTH: usize = MAX_CONTAINER_DEPTH + 100;

/// Iterative fast path for a monotonically-DEEPENING nested-blockquote staircase
/// (line k carries exactly k `> ` markers, each level a single plain-prose
/// paragraph) — the one container shape the recursive [`ContainerBlockCache`]
/// streams worse-than-quadratically. That path spends one nested [`StreamParser`]
/// per level (capped at [`MAX_CONTAINER_DEPTH`] to bound the recursive-`append`
/// call stack against the WASM shadow stack); past the cap the innermost parser
/// re-scans and re-renders its whole growing tail every append.
///
/// This cache instead folds each settled shallower level's opener EXACTLY once
/// into `settled` and keeps ONE open parser for the deepest line, so it extends in
/// O(new bytes) with NO per-level call-stack recursion (a heap `String` + a single
/// parser, never nested parsers — so arbitrary depth costs no shadow stack). It is
/// byte-identical to the recursive path: each level is `render_blockquote`'s
/// `<blockquote>` wrapper around one prose paragraph rendered with the same engine
/// and `force_open_tail`.
///
/// It BAILS (drops itself → the full reparse re-arms the recursive path, i.e. the
/// unchanged baseline) on ANY deviation from the pure single-step deepening shape:
/// a line not exactly one level deeper, a non-prose first content byte (anything
/// that could open a different block or an alert), a lazy/blank/shallower line, a
/// `\r`, a footnote marker (with footnotes on), or a depth reaching
/// [`DEEP_QUOTE_MAX_DEPTH`] (past which the baseline render-truncates — preserved
/// by handing back). Only armed at the top level with `block_data` off (the
/// recursive path owns the nested `ContainerData` channel).
struct DeepQuoteCache {
    /// Absolute offset of the outermost `>` of the staircase's first line.
    start: usize,
    /// Stable id of the outer blockquote block (preserved across appends).
    id: u64,
    /// `<blockquote{dir}>` — the same wrapper opener for every level (the alert
    /// path is excluded; a level whose content is an alert marker bails).
    wrapper_open: String,
    /// Frozen openers for the SETTLED levels `1..open_depth`: each is
    /// `wrapper_open + "\n" + <prose-paragraph html> + "\n"`, folded exactly once
    /// when the level settles (a strictly-deeper line arrives; committed levels
    /// never change under `force_open_tail`).
    settled: String,
    /// Depth (number of `> ` markers) of the current deepest line; `0` before the
    /// first line is classified. `settled` holds `open_depth - 1` levels.
    open_depth: usize,
    /// Renders the deepest line's content (markers stripped), OPEN
    /// (`force_open_tail`), matching `render_blockquote`'s inner sub-block. Reset
    /// to a fresh parser each time the deepest level settles.
    open: Box<StreamParser>,
    /// Whether the deepest line has received its terminating `\n` (drives the
    /// trailing `\n` after its `<p>` and whether the next byte opens a new level).
    open_complete: bool,
    /// Absolute offset of the next unconsumed byte (into `buffer`).
    fed_upto: usize,
}

/// A content byte that is safe to treat as the start of a plain-prose paragraph
/// line inside a blockquote: an ASCII letter. Everything else may open a different
/// block or an alert (`[`, `#`, `>`, `-`/`*`/`+`, digits, fences, `<`, indent, …)
/// or change block structure, so [`DeepQuoteCache`] bails and lets the full path
/// render it. Deliberately narrow — bails only over-drop (never mis-render).
fn dq_prose_start(b: u8) -> bool {
    b.is_ascii_alphabetic()
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
    /// `marker_indent` of the first item (the list's left edge). A sibling marker
    /// must be indented `≤ edge + 3` (deeper than that is a continuation, not a
    /// new item), mirroring `render_list`'s split.
    edge: usize,
    /// `content_indent` of the CURRENT (open) item — the boundary that decides
    /// whether a following line nests (≥ `cur_ci`) or is a shallower sibling /
    /// continuation. Updated each time a sibling opens, so items with differing
    /// content columns classify exactly like the full path's `cur_ci`.
    cur_ci: usize,
    /// `<ul>` / `<ol start=N>` opener + `\n`, frozen at arm time. Kept separate
    /// from item HTML so the tight→loose rebuild only touches items.
    opener_html: String,
    /// Pre-rendered HTML: opener + every fully-cached `<li>…</li>\n`. No
    /// trailing `</ul>` / `</ol>`. Holds only CLOSED items (a later sibling
    /// marker has settled their full multi-line body); the open (last) item and
    /// any trailing partial marker are re-rendered speculatively each append.
    cached_prefix: String,
    /// Absolute offset just past the last COMPLETE line the boundary scan has
    /// classified. Everything from here to EOF is the trailing partial line.
    lines_upto: usize,
    /// Absolute byte offset of the OPEN (last, not-yet-closed) item's first line.
    /// The open item — possibly a multi-line body with nested sub-lists — spans
    /// `[open_item_start..lines_upto]`; it is committed (folded into
    /// `cached_prefix`) only when a later sibling marker closes it.
    open_item_start: usize,
    /// `true` when the last classified complete line was blank. A following
    /// sibling marker then makes the list loose (§5.3, "blank between siblings");
    /// a following nested line is an INTERIOR blank — the item stays in the
    /// cache (see `item_blank`), and §5.3 "directly loose" is settled by the
    /// precise inter-block gap test before the append renders.
    prev_blank: bool,
    /// `true` once the OPEN item has an interior blank line followed by more
    /// in-item content. While the list is still tight this triggers the §5.3
    /// "directly contains two blocks separated by a blank" test each append
    /// (via the nested stream's block gaps, or `item_directly_loose` on the
    /// span) — a confirmed gap flips the whole list loose via `rebuild_loose`,
    /// exactly when the full path's per-append recompute would. Reset per item.
    item_blank: bool,
    /// `true` while the OPEN item is still EMPTY (its marker line had no
    /// content). Mirrors `scan_list`'s `cur_empty`: an empty item cannot gain
    /// content across a blank line (§5.2) — the list ENDS there, so the cache
    /// bails to the full reparse for that shape.
    item_empty: bool,
    /// Incremental renderer for the OPEN item's multi-line body (see
    /// [`OpenItemStream`]). Armed lazily once the body exceeds
    /// [`OPEN_ITEM_STREAM_MIN`]; `None` for short items (the per-append
    /// `fold_item_body` is cheap there). Reset whenever a sibling closes the
    /// item.
    open_stream: Option<Box<OpenItemStream>>,
    /// `true` when the open item's body cannot stream through a nested parser
    /// (a lazy continuation line needs `item_body`'s lazy re-indent, or the arm
    /// failed) — the per-append fold owns the item until it closes. Reset per
    /// item.
    stream_disabled: bool,
    /// `true` once any two siblings are separated by a blank line (§5.3).
    /// Sticky — never flips back; new items render with the loose `<p>` wrap.
    loose: bool,
    /// Source spans `(start, end)` of every CLOSED item in `buffer`. `end` is the
    /// next sibling marker's line start (exclusive), so `&buffer[s..e]` is the
    /// item's whole multi-line body including nested sub-lines and trailing blank
    /// lines — fed to `item_body` + `render_item_body` to re-render on the
    /// tight→loose transition (byte-identical to the full path).
    items: Vec<(usize, usize)>,
    /// Per-item inner `<li>` HTML for the opt-in `kind.data` channel — one entry
    /// per `items` span, parallel to `cached_prefix`. Empty unless `block_data` is
    /// on; surfaced on the active block's `BlockKind::List { items }` so the keyed
    /// renderer reuses unchanged item nodes while the list streams. `Rc`-shared
    /// (mirroring `TableData::rows`) so the per-patch re-emit of all committed
    /// items is O(items) refcount bumps, not O(item bytes) `String` clones.
    item_html: Vec<Rc<ListItemData>>,
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
    /// Incremental footnote NUMBERING over the list region (see
    /// [`RegionFnNums`]). Inert when footnotes are off.
    fn_nums: RegionFnNums,
}

/// Body-size threshold (bytes) past which the OPEN list item's multi-line body
/// streams through a nested parser ([`OpenItemStream`]). Below it the per-append
/// `fold_item_body` re-render is cheap; arming per tiny item would just churn
/// allocations (a flat list opens a new item every line).
const OPEN_ITEM_STREAM_MIN: usize = 1024;

/// Incremental renderer for the OPEN (last) list item's multi-line body — the
/// missing recursion level of the [`ListCache`]: closed items fold once, but the
/// open item (a growing table / fence / sub-list / plain multi-line paragraph)
/// was re-rendered whole by `fold_item_body` on EVERY append, i.e. O(item body)
/// per append = O(n²/chunk) for a single ever-growing item.
///
/// The [`ContainerBlockCache`] pattern one level down: the item's body is
/// de-indented incrementally — byte-identical to [`item_body`]'s output (the
/// marker line's content byte, then `strip_cols(line, content_indent)` per
/// deeper/blank line) — and fed to a recursive nested [`StreamParser`]. The
/// `<li>` inner is assembled from the nested blocks exactly as
/// `render_item_body` would render them:
///   - loose item, or a non-paragraph block: the nested block's `.html`
///     verbatim (same `render_block` engine, `force_open_tail` matching the
///     open list's whole-list `open_tail`),
///   - tight paragraph: `render_inline(trim_trailing_newlines(slice))` from the
///     nested SOURCE (never the `<p>`-wrapped html — its trailing-whitespace
///     trim differs). The trailing (growing) paragraph keeps a settled-prefix
///     cut (`render_inline_boundary`, the `ParagraphCache` pattern) so plain
///     prose bodies are O(new bytes); settled interior paragraphs render once
///     (memoized).
///
/// Byte-parity guards:
///   - trailing whitespace (blank lines / a content line's final spaces) is
///     HELD BACK from the feed — `render_item_body` truncates it before
///     scanning, so the nested buffer must end at the last non-whitespace byte,
///   - a lazy continuation line (shallow, non-blank) needs `item_body`'s lazy
///     re-indent, which can't be re-fed — the stream is dropped for this item
///     (`stream_disabled`) and the per-append fold takes over (today's cost),
///   - the checkbox prefix `[x] ` is stripped from the feed and re-emitted by
///     the assembly (mirroring `render_item_body`),
///   - CRLF / a `]:` def line already bail the whole list cache before any
///     line is fed; a footnote REF `[^x]` streams through the cache's fold
///     (placeholder tokens) but the nested parser runs with footnotes OFF, so
///     `[^` anywhere in the open item's body disables the STREAM only (the
///     arm/feed scoping check below — the fold owns the item then),
///   - the item's CLOSE (a sibling marker) still folds the full span through
///     `fold_item_body` — the committed prefix bytes are produced by the same
///     code as before, the stream only serves the speculative open view.
///
/// The nested blocks also drive the §5.3 "directly loose" test: a blank line
/// sitting in a gap BETWEEN two nested top-level blocks makes the item (and so
/// the list) loose; a blank inside one block (a fence body) does not. Gaps
/// between committed nested blocks are frozen — checked once.
struct OpenItemStream {
    /// The recursive parser rendering the item's de-indented body.
    inner: Box<StreamParser>,
    /// Absolute outer-buffer offset of the item's marker line (identity: the
    /// stream belongs to exactly one open item).
    item_start: usize,
    /// The item's `content_indent` — the `strip_cols` width for body lines.
    ci: usize,
    /// GFM task-list state parsed from the body's first 4 bytes (stripped from
    /// the feed; the assembly re-emits the checkbox).
    task: Option<bool>,
    /// Absolute outer offset consumed so far (fed or held).
    fed_outer: usize,
    /// De-indented trailing whitespace not yet fed (see the hold-back rule).
    held_ws: String,
    /// True when mid source line: the line's leading indent was consumed and
    /// some content fed; continuation bytes feed raw.
    mid_line: bool,
    /// Settled-prefix cut state for the trailing tight paragraph: inner-buffer
    /// offsets + the rendered settled prefix (`ParagraphCache` pattern, minus
    /// the `<p>` wrapper and its output trim).
    para_start: usize,
    para_cut: usize,
    para_settled: String,
    /// Once-rendered interior tight paragraphs, keyed by their (frozen)
    /// inner-buffer span.
    tight_memo: HashMap<(usize, usize), String>,
    /// Consecutive committed nested-block pairs whose gap was already checked
    /// for a §5.3 blank (frozen — never re-checked).
    gap_pairs_done: usize,
    /// Sticky half of the `open_tail`-sensitivity test (see
    /// [`open_item_ot_sensitive`]): a COMMITTED nested block whose html the
    /// assembly serves frozen carries a byte that can open an
    /// `open_tail`-speculative inline construct. Once true, settled
    /// (buffer-ends-blank) appends fall back to the one-shot fold — the frozen
    /// renders are open-tail variants only.
    sens_committed: bool,
    /// Committed nested blocks already trigger-scanned (frozen kind + span —
    /// scanned once, amortized O(n)).
    sens_scanned: usize,
}

/// Incremental render state for a single open *indented-code* block at the tail —
/// the streaming shape where every line is ≥4-column-indented content. Streaming
/// such a block is otherwise O(n²): every append re-strips and re-escapes the
/// whole growing body (`render_indented_code`).
/// With this cache an append only strips+escapes the newly-arrived complete
/// lines and re-renders the (short) trailing partial line.
///
/// Interior blank lines fold like content lines (strip ≤4 columns, keep the
/// remainder — exactly `render_indented_code`'s per-line strip): a blank between
/// two chunks is part of the block (§4.4), while blanks at the tail are trimmed
/// by the assembly's trailing-whitespace trim, matching the full path whose
/// block range stops at the last content line.
///
/// The cache bails (full path takes over) the instant the simple pattern breaks:
///   - a newly-complete NON-BLANK line that dedents (indent < 4) — it ends the
///     block,
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
    /// RAW (unescaped) twin of `escaped_lines`, extended in lock-step — only when
    /// `block_data` is on (empty + unused otherwise). The opt-in `kind.data.code`
    /// assembles from this directly, so the per-append emit is a memcpy instead of
    /// an O(body) entity decode of the assembled HTML. Interior blank lines fold
    /// into both twins at the same points, so the trailing trims stay identical.
    decoded_lines: String,
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
///
/// THIRD MODE — `sanitize` (`block_html` + the sanitizer, types 6/7). The output
/// is no longer a per-byte map of the source, so a per-LINE fold is impossible:
/// a tag legitimately spans lines. The fold moves to TOKEN boundaries instead
/// (`render::fold_block_html`), carrying the offset after the last fully
/// consumed token/text run (`tokens_upto`) and the open-element stack there
/// (`open_tags`). Per append only the new complete tokens are sanitized; only
/// the speculative-closer suffix (bounded by `MAX_BLOCK_HTML_DEPTH`) and the
/// trailing partial are regenerated. Re-sanitizing the whole block per append is
/// the O(n²) this mode exists to avoid — `tests/scaling.rs` pins it.
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
    /// When `true` (pass-through + `gfm_tagfilter`), each folded line and the
    /// re-processed partial run through the GFM tagfilter — per NEW line only,
    /// so filtering stays O(new bytes) and byte-identical to the full path.
    tagfilter: bool,
    /// When `true` (`block_html` + the sanitizer + type 6/7), the block renders
    /// through the raw-HTML sanitizer and the fold is at TOKEN boundaries: see
    /// `tokens_upto` / `open_tags`. Locked at arm time, re-validated per append.
    sanitize: bool,
    /// Pre-rendered prefix of the completed lines: for pass-through, the raw
    /// source bytes verbatim (including their `\n`); for the escaped path, their
    /// `escape_html` output (newlines survive escaping unchanged). In sanitize
    /// mode, the sanitized output of everything up to `tokens_upto`. No closer.
    cached_prefix: String,
    /// Absolute offset just past the last complete folded line's `\n`. Drives
    /// close detection in every mode; drives the FOLD in all but sanitize mode.
    lines_upto: usize,
    /// Sanitize mode only: absolute offset just past the last fully consumed
    /// token / settled text byte. Everything from here to the buffer end is a
    /// still-streaming `<…` (suppressed until it completes).
    tokens_upto: usize,
    /// Sanitize mode only: the open-element stack at `tokens_upto` (outermost
    /// first, source spelling), regenerated into speculative closers per append.
    open_tags: Vec<String>,
}

/// Incremental render state for a single open COMPONENT block (`<Tag>` whose
/// matching close line hasn't arrived) at the tail. The component is one atomic,
/// never-committing block whose body is markdown, so the full reparse re-scans +
/// re-renders the whole growing body every append (O(n²) — the streaming
/// `<Chart>` shape). This is [`ContainerBlockCache`] without the `>`-stripping:
/// the body bytes past the open tag's `>` feed a recursive nested
/// [`StreamParser`] RAW, and the active block reassembles as
/// `<Tag attrs>` + inner sub-blocks + `</Tag>` — byte-identical to
/// `render_component` (same wrapper builder, same `\n` joins, same
/// `!sub.is_empty()` leading newline).
///
/// The close / fence / same-tag-nesting line classification mirrors
/// `scan_component_block` exactly, so the cache BAILS to the full reparse at the
/// precise line where the scanner would terminate the block. It also bails on
/// the scoping markers the nested parser can't reproduce (`[^` when footnotes
/// are on; `]:` link-ref defs — both document-global), on `\r`, and on a
/// blank-line-terminated buffer (the full rescan then renders the whole
/// component with `open_tail = false`, which the nested parser's frozen
/// `force_open_tail` commits can't match).
struct ComponentBlockCache {
    /// Absolute byte offset of the component's open-tag line in `buffer`.
    start: usize,
    /// Stable id of the component block (preserved across appends and the close).
    id: u64,
    /// Component tag name (allowlisted; drives the close/nesting line tracking).
    tag: String,
    /// Frozen `BlockKind::Component { tag, attrs }` from arm time — the open tag
    /// line is complete, so the sanitized attrs can never change.
    kind: BlockKind,
    /// Wrapper closer `</Tag>`.
    wrapper_close: String,
    /// The recursive parser rendering the raw body markdown. Fed only the
    /// per-append delta; its `all_blocks()` are the inner sub-blocks.
    inner: Box<StreamParser>,
    /// Settled prefix of `inner`'s assembled body (see [`WrappedBodyCache`]),
    /// seeded with the wrapper opener `<Tag attrs>` (built with the same
    /// `sanitize_attrs` + `escape_attr` as `render_component`).
    body: WrappedBodyCache,
    /// Absolute outer-buffer offset already fed to `inner` (everything past the
    /// open tag's `>` feeds raw — the body IS the inner markdown).
    fed_upto: usize,
    /// Absolute offset of the first not-yet-classified line. The close/fence/
    /// nesting scan is line-based; complete lines classify exactly once.
    lines_upto: usize,
    /// Same-tag nesting depth, mirroring `scan_component_block` (starts at 1;
    /// the outer block terminates when a clean close line brings it to 0).
    depth: usize,
    /// Code-fence toggle state, mirroring `scan_component_block` (a `</Tag>`
    /// line inside a ``` fence is content, not a close).
    in_fence: bool,
    /// Last byte fed to `inner` (for catching a `[^` / `]:` marker split across
    /// two feeds). `None` until the first non-empty feed.
    last_fed: Option<u8>,
    /// Whether the outer parser has footnotes on (then a `[^` body marker BAILS,
    /// since the nested parser runs with footnotes off).
    footnotes: bool,
    /// SETTLED twin of `inner`: the same body with the same flags, but with
    /// `force_open_tail` OFF, so every block IT froze at commit time froze with
    /// `open_tail = false`. Serves the appends where the outer buffer ends on a
    /// blank line — there the full rescan renders the open component, and all
    /// its sub-blocks, settled, which `inner`'s forced-open commits can never
    /// match. Built and fed LAZILY, only on the appends that actually read it,
    /// so each catch-up feed spans just the bytes since the previous
    /// blank-ending append and the pair costs O(body) over the whole stream.
    /// Stays `None` for a body that never contains a blank line.
    settled: Option<Box<StreamParser>>,
    /// The SETTLED twin's own body prefix. Kept separate from `body` because the
    /// two twins' committed html deliberately differs (`force_open_tail`), so one
    /// shared prefix would emit the wrong bytes on the append after a swap.
    settled_body: WrappedBodyCache,
    /// Absolute outer-buffer offset already fed to `settled`. Starts at the body
    /// start, like `fed_upto`, and lags it between blank-ending appends.
    settled_fed: usize,
}

/// Incremental render state for a still-growing single-line ATX heading at the
/// tail — the giant-`# …` streaming shape. A heading has no cache arm otherwise,
/// so every append re-scans + re-`render_inline`s the whole growing line
/// (O(n²); with early-open emphasis the re-render itself is superlinear, going
/// ~cubic in wall time). This is the [`ParagraphCache`] settled-prefix scheme in
/// an `<hN>` wrapper: `render_inline_boundary` commits the construct-free prefix
/// once and only the short active tail re-renders per append.
///
/// The heading-specific part is the CONTENT WINDOW: `render_heading` strips the
/// leading `#`s + whitespace (frozen once content exists — the line only grows
/// at its end) and, per append, the trailing spaces plus an optional closing-`#`
/// run (only when preceded by space/tab). The trim is recomputed each append
/// (O(trailing run)); if it ever reaches back into the frozen prefix the cache
/// bails. Any newline ends the single-line heading — bail, full path owns it.
struct HeadingCache {
    /// Absolute byte offset of the heading's first byte (its indent/`#`s).
    start: usize,
    /// Stable id of the heading block.
    id: u64,
    /// ATX level (1–6), locked once content exists.
    level: u8,
    /// Absolute offset of the first content byte (past `#`s + following ws).
    content_start: usize,
    /// Absolute offset; `buffer[content_start..cut]` is committed (settled) and
    /// rendered into `committed_inner`. Always ≤ the current trim target.
    cut: usize,
    /// Rendered inline HTML of `buffer[content_start..cut]`.
    committed_inner: String,
    /// Buffer length after the last processed append — only bytes past this are
    /// checked for the newline that ends the heading.
    scanned_upto: usize,
    /// Footnote occurrence map for the frozen prefix (see [`ParagraphCache`]).
    fn_occ: HashMap<String, usize>,
    /// Incremental footnote NUMBERING over the heading region (see
    /// [`RegionFnNums`]). Inert when footnotes are off.
    fn_nums: RegionFnNums,
}

/// Incremental state for a still-growing thematic-break line at the tail
/// (`"-".repeat(n)` streamed). The output is a constant `<hr>`; the cache just
/// validates that appended bytes keep the line a same-char break (`scan_hr`:
/// only the rule char, spaces, tabs) and re-emits the block without re-scanning
/// the whole line. Any other byte — including the newline that completes the
/// line — bails to the full path.
struct RuleCache {
    /// Absolute byte offset of the rule line's first byte.
    start: usize,
    /// Stable id of the rule block.
    id: u64,
    /// The break character (`-`/`*`/`_`), locked at arm time.
    ch: u8,
    /// Buffer length after the last processed append (only new bytes validate).
    scanned_upto: usize,
}

/// Incremental state for a code fence whose OPENING line is still growing — the
/// giant-info-string shape ("```rust " + huge attr tail, no newline yet).
/// [`FenceCache`] requires a complete opener line, so this tail otherwise
/// full-rescans every append. The rendered block (`push_code_fence_open` +
/// `</code></pre>`, empty body) and its classified kind depend ONLY on the first
/// info word; once that word is settled (whitespace follows it) both are frozen,
/// and each append just validates the new bytes. Nothing that grows with the
/// line is re-derived here — including the info string's `meta`, which
/// `classify` deliberately withholds until the opener line is terminated (or the
/// stream finalizes), so the frozen kind stays correct for every prefix this
/// cache covers. Bails on the newline that
/// completes the opener (the normal fence cache arms on the next reparse) and on
/// a backtick in a backtick fence's info (the scanner then rejects the fence).
struct FenceInfoCache {
    /// Absolute byte offset of the fence opener's first byte.
    start: usize,
    /// Stable id of the fence block.
    id: u64,
    /// True for a ``` fence (whose info may not contain backticks, §4.5).
    backtick: bool,
    /// Frozen block HTML: opener tag from the settled first info word + closer.
    html: String,
    /// Frozen classified kind (CodeBlock/MathBlock/Mermaid from the first word,
    /// including any folded `block_data` enrichment — the body stays empty while
    /// the opener line grows, so the enrichment can't change either).
    kind: BlockKind,
    /// Buffer length after the last processed append (only new bytes validate).
    scanned_upto: usize,
}

impl StreamParser {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            committed_offset: 0,
            committed_blocks: Vec::new(),
            retain_committed_html: true,
            active_blocks: Vec::new(),
            next_id: 0,
            finalized: false,
            committed_refs: Rc::new(HashMap::new()),
            committed_footnotes: Rc::new(HashMap::new()),
            committed_footnote_defs: Rc::new(HashMap::new()),
            committed_footnote_occurrences: Rc::new(HashMap::new()),
            next_footnote: 1,
            unsafe_html: false,
            gfm_autolinks: false,
            gfm_alerts: false,
            gfm_tagfilter: false,
            gfm_footnotes: false,
            gfm_math: false,
            dir_auto: false,
            lenient_lists: false,
            soft_breaks: false,
            a11y: false,
            block_data: false,
            component_tags: Vec::new(),
            inline_component_tags: Vec::new(),
            html_sanitize: false,
            html_allowlist: Vec::new(),
            html_drop: Vec::new(),
            block_html: false,
            allow_schemes: Vec::new(),
            fence_cache: None,
            para_cache: None,
            table_cache: None,
            container_cache: None,
            container_block_cache: None,
            deep_quote_cache: None,
            force_open_tail: false,
            container_depth: 0,
            list_cache: None,
            indented_cache: None,
            html_cache: None,
            pending_cr: false,
            component_cache: None,
            heading_cache: None,
            rule_cache: None,
            fence_info_cache: None,
            dollar_tail_cache: None,
            alnum_tail_cache: None,
            raw_tag_tail_cache: None,
            mod3_tail_cache: None,
            wire_delta: false,
            last_wire_active: Vec::new(),
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

    /// Enable the GFM "Disallowed Raw HTML" extension (tagfilter): when raw
    /// HTML passes through verbatim (`unsafe_html`), the nine disallowed tags
    /// (`<title>`, `<textarea>`, `<style>`, `<xmp>`, `<iframe>`, `<noembed>`,
    /// `<noframes>`, `<script>`, `<plaintext>`) get their leading `<` escaped
    /// so they display as text. Off by default (strict CommonMark passes them
    /// through); no effect while raw HTML is escaped or sanitized.
    pub fn with_gfm_tagfilter(mut self, on: bool) -> Self {
        self.gfm_tagfilter = on;
        self
    }

    pub fn set_gfm_tagfilter(&mut self, on: bool) {
        self.gfm_tagfilter = on;
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
    /// KaTeX (bring your own renderer — brookmd stays zero-dep).
    pub fn with_gfm_math(mut self, on: bool) -> Self {
        self.gfm_math = on;
        self
    }

    pub fn set_gfm_math(&mut self, on: bool) {
        self.gfm_math = on;
    }

    /// Lenient list indentation: a list marker followed by 6 or more columns of
    /// SPACE padding yields the item's text, where strict CommonMark (§5.2) keeps
    /// only one column and renders the remainder as an indented code block. Off by
    /// default. Aimed at model output, which routinely over-indents after a bullet
    /// (`-       const value = 1;`) and would otherwise render as a code block.
    ///
    /// Deliberately NOT relaxed (each stays strictly conformant): exactly 5 columns
    /// of padding, a fenced code block opened on the marker line itself, indented
    /// code that starts on a line AFTER the marker, and tab-padded markers.
    pub fn with_lenient_lists(mut self, on: bool) -> Self {
        self.lenient_lists = on;
        self
    }

    pub fn set_lenient_lists(&mut self, on: bool) {
        self.lenient_lists = on;
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

    /// Render a CommonMark SOFT line break (a bare `\n` in inline content) as a
    /// `<br>` — the "GitHub comment" convention, where one Enter is one visual
    /// line. Off by default (strict CommonMark treats a soft break as
    /// whitespace). Hard breaks (two trailing spaces / trailing `\`) are `<br>`
    /// either way, so turning this on only ADDS breaks; it never removes one.
    pub fn with_soft_breaks(mut self, on: bool) -> Self {
        self.soft_breaks = on;
        self
    }

    pub fn set_soft_breaks(&mut self, on: bool) {
        self.soft_breaks = on;
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

    /// Keep every committed block's rendered `html` retained for
    /// [`Self::all_blocks`]. ON by default (the whole document stays readable
    /// out of the parser). Turn it OFF in a pure streaming consumer that reads
    /// each committed block exactly once off its patch: the html is then
    /// released at commit, halving what a long stream retains — at the cost of
    /// `all_blocks` reporting committed blocks with an EMPTY `html` (metadata
    /// stays exact; it never panics). Patch bytes are identical either way.
    pub fn with_retain_committed_html(mut self, on: bool) -> Self {
        self.retain_committed_html = on;
        self
    }

    pub fn set_retain_committed_html(&mut self, on: bool) {
        self.retain_committed_html = on;
    }

    /// Enable the opt-in wire delta mode (`WIRE.md` §11): active blocks that
    /// were emitted in the previous patch serialize as verified `html_delta`
    /// splices (`{keep_bytes, keep_units, append}`) instead of re-sending
    /// their full `html`, so a block that grows across many appends emits
    /// O(size) total wire bytes instead of O(size²/chunk). Off by default —
    /// when off, the wire is byte-identical to pre-delta releases. A consumer
    /// that enables this must reconstruct active `html` per `WIRE.md` §11.
    pub fn with_wire_delta(mut self, on: bool) -> Self {
        self.wire_delta = on;
        self
    }

    pub fn set_wire_delta(&mut self, on: bool) {
        self.wire_delta = on;
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

    /// Extend the safe raw-HTML sanitizer to BLOCK-level raw HTML. Takes effect
    /// ONLY when the sanitizer is engaged ([`Self::set_html_sanitize`]); on its
    /// own it does nothing. When on, an HTML block of CommonMark type 6 (a
    /// known block-level tag: `<details>`, `<div>`, `<table>`, …) or type 7 (any
    /// other complete tag alone on its line) renders through the same
    /// allow/drop/dangerous decision and hardened attribute policy as inline raw
    /// HTML, instead of being escaped into a `<pre><code>` block.
    ///
    /// Types 1–5 are deliberately NOT covered: type 1 is the raw-text family
    /// (`<script>`, `<pre>`, `<style>`, `<textarea>`) — a browser reads
    /// everything after such a tag as unparsed text, so a speculative mid-stream
    /// close is mXSS-prone — and types 2–5 (comments, PIs, CDATA, declarations)
    /// carry no renderable element. They stay escaped/dropped exactly as before.
    ///
    /// While the block streams, still-open elements get SPECULATIVE closers so
    /// what the reader has seen so far is always a complete tree; a half-arrived
    /// tag is suppressed until it completes (and renders as escaped text if the
    /// stream ends on it). Off by default — output is byte-identical to before.
    pub fn set_block_html(&mut self, on: bool) {
        self.block_html = on;
    }

    pub fn with_block_html(mut self, on: bool) -> Self {
        self.block_html = on;
        self
    }

    /// Un-block specific URL schemes that are blocked by DEFAULT. Takes BARE
    /// scheme names without the colon (`["file"]`), matched case-insensitively.
    /// Empty (default) = the built-in policy is unchanged.
    ///
    /// This is NOT a general allowlist: it never restricts anything. Schemes
    /// outside the built-in blocklist (`vscode:`, `ftp:`, `mailto:`, …) already
    /// pass and are unaffected by this setting — the only thing it can reach is
    /// the OVERRIDABLE-blocked tier, today just `file:`.
    ///
    /// The script-executing tier (`javascript:`, `vbscript:`, `data:text/html`,
    /// `data:text/javascript`, and the scriptable `data:` media types) is
    /// NON-OVERRIDABLE: naming one of those here is a silent no-op, exactly as
    /// allowlisting `<script>` cannot re-enable it in the raw-HTML sanitizer.
    ///
    /// Enabling `file:` is only safe in a host that does not actually navigate
    /// to the href (an Electron/extension UI that intercepts link clicks and
    /// opens the path in an editor). Applies to links, URI autolinks, images,
    /// and sanitized URL attributes alike.
    pub fn set_allow_schemes(&mut self, schemes: Vec<String>) {
        self.allow_schemes = schemes.into_iter().map(String::into_boxed_str).collect();
    }

    /// Builder form of [`Self::set_allow_schemes`].
    pub fn with_allow_schemes(mut self, schemes: Vec<String>) -> Self {
        self.set_allow_schemes(schemes);
        self
    }

    pub fn set_unsafe_html(&mut self, on: bool) {
        self.unsafe_html = on;
    }

    pub fn append(&mut self, chunk: &str) -> Patch {
        if self.finalized {
            return Patch::default();
        }
        self.ingest(chunk);
        let patch = self.reparse_tail(false);
        let patch = self.finish_patch(patch);
        #[cfg(feature = "perf_counters")]
        Self::count_emitted(&patch);
        patch
    }

    /// Normalize line endings at ingest: `\r\n` and lone `\r` become `\n`
    /// before `buffer` (and therefore the scanner, every incremental cache and
    /// the committed-prefix machinery) sees the bytes. CommonMark defines all
    /// three as the same line ending, so output is conformant — and CRLF-origin
    /// streams take the exact same O(n) incremental fast paths as LF streams
    /// instead of tripping the caches' `\r` bails into a full-tail reparse per
    /// append. A chunk-final `\r` is held pending (not pushed) until the next
    /// chunk shows whether a `\n` follows, keeping a `\r|\n` split across
    /// appends chunk-independent; `finalize` flushes a pending `\r` as `\n`.
    fn ingest(&mut self, chunk: &str) {
        let mut rest = chunk;
        if self.pending_cr && !rest.is_empty() {
            self.pending_cr = false;
            self.buffer.push('\n');
            if rest.as_bytes()[0] == b'\n' {
                rest = &rest[1..]; // the held `\r` + this `\n` = one line ending
            }
        }
        // Fast path: no `\r` in the chunk (the overwhelmingly common case).
        let Some(mut cr) = rest.find('\r') else {
            self.buffer.push_str(rest);
            return;
        };
        self.buffer.reserve(rest.len());
        loop {
            self.buffer.push_str(&rest[..cr]);
            rest = &rest[cr + 1..];
            if rest.is_empty() {
                self.pending_cr = true; // chunk-final `\r`: next append decides
                return;
            }
            self.buffer.push('\n');
            if rest.as_bytes()[0] == b'\n' {
                rest = &rest[1..];
            }
            match rest.find('\r') {
                Some(i) => cr = i,
                None => {
                    self.buffer.push_str(rest);
                    return;
                }
            }
        }
    }

    pub fn finalize(&mut self) -> Patch {
        if self.finalized {
            return Patch::default();
        }
        self.finalized = true;
        if self.pending_cr {
            self.pending_cr = false;
            self.buffer.push('\n');
        }
        let patch = self.reparse_tail(true);
        let patch = self.finish_patch(patch);
        #[cfg(feature = "perf_counters")]
        Self::count_emitted(&patch);
        patch
    }

    /// Wire delta mode: compute each active block's verified splice against the
    /// previous emit of the same block id, and record this patch's `active` as
    /// the base for the next one. Runs at the single exit shared by every
    /// internal emit path (`append`/`finalize`), so no fast path can bypass it.
    /// A no-op (empty `active_deltas`) unless `set_wire_delta(true)`.
    fn finish_patch(&mut self, mut patch: Patch) -> Patch {
        if !self.wire_delta {
            return patch;
        }
        // Below this shared-prefix length a delta saves nothing over the full
        // `html` (the `html_delta` envelope itself costs ~50 bytes). Part of
        // the deterministic emission rule pinned by the wire goldens.
        const MIN_KEEP_BYTES: usize = 64;
        let mut deltas: Vec<Option<HtmlDelta>> = Vec::with_capacity(patch.active.len());
        let mut next: Vec<(u64, String, usize)> = Vec::with_capacity(patch.active.len());
        for b in &patch.active {
            let prev = self.last_wire_active.iter().find(|(id, _, _)| *id == b.id);
            let delta = prev.and_then(|(_, prev_html, prev_units)| {
                let mut keep = common_prefix_bytes(prev_html.as_bytes(), b.html.as_bytes());
                // Back off to a char boundary of the NEW html. Because the
                // bytes below `keep` are identical in both strings, a boundary
                // of the new html is necessarily a boundary of the previous
                // one too (a continuation byte in one would force the same
                // lead-byte structure, hence a continuation byte, in the
                // other), so `prev_html[keep..]` below cannot panic.
                while keep > 0 && !b.html.is_char_boundary(keep) {
                    keep -= 1;
                }
                if keep < MIN_KEEP_BYTES {
                    return None;
                }
                // prev[..keep] == new[..keep], so the kept prefix's UTF-16
                // length is prev's cached total minus its (short) dropped
                // tail — O(changed bytes), never a rescan of the prefix.
                let keep_units = prev_units - utf16_len(&prev_html[keep..]);
                Some(HtmlDelta { keep_bytes: keep, keep_units })
            });
            let units = match delta {
                Some(d) => d.keep_units + utf16_len(&b.html[d.keep_bytes..]),
                None => utf16_len(&b.html),
            };
            next.push((b.id, b.html.clone(), units));
            deltas.push(delta);
        }
        self.last_wire_active = next;
        patch.active_deltas = deltas;
        patch
    }

    /// Deterministic complexity probe (feature `perf_counters` only): HTML bytes
    /// crossing the public patch boundary. With wire delta mode off, re-emitting
    /// the whole open block per append is the wire contract, so this is
    /// informational; with it on, only each delta's `append` tail counts — the
    /// linearity `tests/scaling.rs` asserts for delta-mode streams.
    #[cfg(feature = "perf_counters")]
    fn count_emitted(patch: &Patch) {
        let committed: usize = patch.newly_committed.iter().map(|b| b.html.len()).sum();
        let active: usize = if patch.active_deltas.len() == patch.active.len() {
            patch
                .active
                .iter()
                .zip(&patch.active_deltas)
                .map(|(b, d)| match d {
                    Some(d) => b.html.len() - d.keep_bytes,
                    None => b.html.len(),
                })
                .sum()
        } else {
            patch.active.iter().map(|b| b.html.len()).sum()
        };
        crate::perf::add_emit(committed + active);
    }

    /// The retained source, with line endings normalized to `\n` (see
    /// [`Self::ingest`]); block `start`/`end` offsets index into this.
    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    /// Every block parsed so far — committed then active, in document order.
    ///
    /// With [`Self::set_retain_committed_html`] turned OFF, a COMMITTED block's
    /// `html` here is EMPTY: it was released the moment the block was emitted in
    /// its patch. `id`, `kind`, `start`, `end`, `open` and `speculative` stay
    /// exact and nothing panics — the flag deliberately trades `all_blocks`
    /// fidelity for the memory a pure streaming consumer never needed. It
    /// defaults ON, so a caller that assembles the document from here (the
    /// bindings, the SSR one-shot path) sees the full html unless it opts out.
    pub fn all_blocks(&self) -> impl Iterator<Item = &Block> {
        self.committed_blocks.iter().chain(self.active_blocks.iter())
    }

    /// Total bytes retained: the source buffer plus every block's rendered html.
    /// Committed html released by `retain_committed_html = false` is genuinely
    /// gone, so this counts it as the zero it is.
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
            if let Some(patch) = self.try_incremental_dollar_tail() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_alnum_tail() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_raw_tag_tail() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_mod3_tail() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_table() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_container() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_deep_quote() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_container_block() {
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
            if let Some(patch) = self.try_incremental_component() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_heading() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_rule() {
                return patch;
            }
            if let Some(patch) = self.try_incremental_fence_info() {
                return patch;
            }
        }

        let tail_start = self.committed_offset;
        let tail = &self.buffer[tail_start..];

        // Deterministic complexity probe (feature `perf_counters` only; compiled
        // out of every real build). The fast paths above returned early, so every
        // byte counted here is genuine slow-path tail re-scan work — the quantity
        // a scaling test bounds to keep streaming sub-quadratic.
        #[cfg(feature = "perf_counters")]
        crate::perf::add_scan(tail.len());

        let ctx = ScanCtx {
            math: self.gfm_math,
            component_tags: &self.component_tags,
            inline_component_tags: &self.inline_component_tags,
            lenient_lists: self.lenient_lists,
        };
        let raw_blocks = scan(tail, ctx);

        // Pre-pass: build the ref table for this render. The committed table is
        // shared into opts by an O(1) `Rc` clone (never copied per append);
        // tail definitions are collected fresh each reparse (so a half-typed
        // definition in the growing tail can't get stuck). Committed wins at
        // lookup time (first-definition-wins).
        let committed_refs = Rc::clone(&self.committed_refs);
        let mut tail_refs = HashMap::new();
        collect_refs(tail, &mut tail_refs, ctx, self.gfm_alerts, 0);

        // Renderable blocks: skip link-ref defs (no output) and, when footnotes
        // are on, footnote definitions (collected into the section instead).
        let gfm_footnotes = self.gfm_footnotes;
        let is_footnote_def = |slice: &str| gfm_footnotes && is_footnote_def_block(slice);
        let renderable: Vec<&RawBlock> = raw_blocks
            .iter()
            .filter(|r| !matches!(r.kind, RawBlockKind::LinkRefDefinition))
            .filter(|r| !is_footnote_def(&tail[r.range.clone()]))
            .collect();

        // Footnote numbering pre-pass: committed numbers (permanent, shared by
        // an O(1) `Rc` clone — never copied per append) + a tail overlay of
        // labels first referenced in the tail, continuing from `next_footnote`,
        // in document order over the renderable (non-def) content only.
        let fn_committed = Rc::clone(&self.committed_footnotes);
        let mut fn_tail: HashMap<String, usize> = HashMap::new();
        let mut fn_next = self.next_footnote;
        if gfm_footnotes {
            for raw in &renderable {
                collect_footnote_refs_overlay(
                    &tail[raw.range.clone()],
                    &fn_committed,
                    &mut fn_tail,
                    &mut fn_next,
                );
            }
        }
        let fn_tail = Rc::new(fn_tail);

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
            gfm_tagfilter: self.gfm_tagfilter,
            gfm_math: self.gfm_math,
            dir_auto: self.dir_auto,
            lenient_lists: self.lenient_lists,
            soft_breaks: self.soft_breaks,
            a11y: self.a11y,
            block_data: self.block_data,
            gfm_footnotes,
            footnotes: Rc::clone(&fn_committed),
            tail_footnotes: Rc::clone(&fn_tail),
            // Placeholder mode never touches the occurrence counter (produced
            // blocks are resolved in document order just below); the def-render
            // opts that DO render live override this with their own seed.
            footnote_occ: std::cell::RefCell::new(HashMap::new()),
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
            block_html: self.block_html,
            allow_schemes: self.allow_schemes.clone(),
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
            let mut kind = classify(
                &raw.kind,
                &tail[raw.range.clone()],
                self.gfm_alerts,
                &self.allow_schemes,
                finalizing,
            );
            let mut html = String::with_capacity(64);
            // Per-block open_tail: the final block that abuts buffer EOF and is
            // not blank-line-closed. Clone opts with the flag set only for it.
            // `force_open_tail` (a nested container-block parser only) forces it on
            // for EVERY block so committed inner sub-blocks render exactly like
            // `render_blockquote`, which propagates the open container's open_tail
            // to all of them (so a closed inner block ending in an incomplete
            // `[x](` / `` `code `` / `$math` speculates identically — never finalized,
            // so this is only ever the open-stream view).
            let block_open_tail = self.force_open_tail
                || (!finalizing
                    && bi == last_idx
                    && tail_start + raw.range.end == self.buffer.len()
                    && !buffer_ends_blank);
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
            // `tail_start` is `tail`'s absolute offset in the buffer, so the
            // structured channel can stamp document-absolute source offsets
            // (list items) that agree with the `Block::start` computed below.
            match render_block(tail, raw, Some(tail_start), block_opts_ref, &mut html) {
                Some(Enrichment::Table(td)) => kind = BlockKind::Table(Some(td)),
                Some(Enrichment::Heading(h)) => {
                    kind = BlockKind::Heading { level: h.level, rich: Some(h) }
                }
                // CodeBlock keeps its classified `lang`/`meta`; only `code` is
                // folded on.
                Some(Enrichment::CodeBlock(code)) => {
                    if let BlockKind::CodeBlock { lang, meta, .. } = kind {
                        kind = BlockKind::CodeBlock { lang, meta, code: Some(Rc::new(code)) };
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
        // replay is the SOLE source of truth for advancing the occurrence count,
        // which is layered — the (Rc-shared, never-cloned) committed base plus
        // the per-reparse `occ_over` overlay of ABSOLUTE counts for the labels
        // this tail's blocks actually reference. (When footnotes are off, no
        // tokens exist and this is a cheap byte-copy.)
        //
        // `occ_over_after_block[i]` snapshots the OVERLAY after resolving the
        // first i+1 produced blocks; the committed-region advance replays it
        // once `to_commit` is known, so the persistent committed occurrence map
        // is advanced by exactly the COMMITTED blocks' real refs (never by
        // `[^x]` inside code spans / escaped text, which emit no token).
        // `produced` is a handful of top-level blocks and the overlay holds only
        // tail-referenced labels, so the per-block snapshot clone is cheap.
        let mut occ_over: HashMap<String, usize> = HashMap::new();
        let mut occ_over_after_block: Vec<HashMap<String, usize>> = Vec::new();
        if gfm_footnotes {
            occ_over_after_block.reserve(produced.len());
            for block in &mut produced {
                resolve_block_footnotes(block, &self.committed_footnote_occurrences, &mut occ_over);
                occ_over_after_block.push(occ_over.clone());
            }
        }
        // Definition bodies render at finalize, AFTER the total-occurrence
        // snapshot, with placeholder mode OFF (the live path): a def-body `[^x]`
        // continues the occurrence sequence past total_occ (matching the
        // historical behavior) and is intentionally not counted into the
        // section's backref total. Finalize-ONLY: `fn_defs`/`total_occ` feed
        // nothing but the section, so a live append skips the whole-tail def
        // render (and the committed-map clones) this used to pay per append —
        // live commits fold defs at commit time below instead.
        let mut fn_defs: HashMap<String, String> = HashMap::new();
        let mut total_occ: HashMap<String, usize> = HashMap::new();
        if gfm_footnotes && finalizing {
            total_occ = (*self.committed_footnote_occurrences).clone();
            for (k, v) in &occ_over {
                total_occ.insert(k.clone(), *v);
            }
            fn_defs = (*self.committed_footnote_defs).clone();
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
        // True when the line immediately preceding the line starting at
        // `line_start` exists and is non-blank. CommonMark blank is ASCII-only
        // (space/tab); `str::trim` is Unicode-aware and would treat a line of
        // only form feed (U+000C) or vertical tab (U+000B) as blank,
        // prematurely committing a paragraph the one-shot parse keeps open for
        // a lazy continuation.
        let line_before_nonblank = |line_start: usize| {
            line_start > 0 && {
                let before = &tail[..line_start - 1];
                let prev_start = before.rfind('\n').map_or(0, |i| i + 1);
                !before[prev_start..]
                    .bytes()
                    .all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
            }
        };
        let prev_line_nonblank = line_before_nonblank(final_line_start);
        // The SAME provisional-classification hazard one line further up: a GFM
        // table whose DELIMITER row is the still-growing final line. Mid-line,
        // `|:-` is a one-column delimiter and the line above it a one-column
        // header, so the table forms and the block before it looks closed — but
        // once the line completes as `|:-----|--` the column counts no longer
        // match, the table dissolves, and its header line falls back to a lazy
        // continuation of the paragraph above (the finalized one-shot parse
        // makes ONE paragraph of all three lines). The table block STARTS on the
        // header line, so `last_starts_final_line` — which keys off the block
        // start — misses it; key off the delimiter row instead. Also bounded:
        // it clears the moment the delimiter row's `\n` arrives.
        let last_is_provisional_table = !finalizing
            && n >= 2
            && matches!(renderable[n - 1].kind, RawBlockKind::Table)
            && tail_start + renderable[n - 1].range.end == self.buffer.len()
            && !self.buffer.ends_with('\n')
            && !self.buffer.ends_with('\r')
            && {
                // the block's SECOND line — its delimiter row — is the final line
                let hdr = renderable[n - 1].range.start;
                hdr < final_line_start
                    && tail[hdr..].find('\n').map_or(false, |i| hdr + i + 1 == final_line_start)
                    // …and no blank line separates the header from the block
                    // before it, so dissolving really does merge them (across a
                    // blank the leftovers form their own paragraph instead, and
                    // holding the predecessor back would re-scan it every append).
                    && line_before_nonblank(hdr)
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
                || ((last_starts_final_line || last_is_provisional_table)
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
            let base = if raw_blocks.len() >= 2 {
                raw_blocks[raw_blocks.len() - 2].range.end
            } else {
                0
            };
            // A NO-blank-line run of `[^x]: …` footnote defs scans as ONE raw
            // block, so the >=2 rule above never fires for it and the run would
            // still stall. Adjacent def-opener lines are separate defs
            // (`footnote_defs`) and an opener seals every def before it, so
            // commit the trailing def block up to its LAST opener line — only
            // the final def, whose body can still grow by soft-continuation
            // lines, stays speculative.
            let intra = raw_blocks.last().map_or(0, |raw| {
                let slice = &tail[raw.range.clone()];
                if is_footnote_def(slice) {
                    raw.range.start + last_footnote_def_opener(slice)
                } else {
                    0
                }
            });
            base.max(intra)
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
                // `opts.footnotes` still aliases the committed table here, so
                // `make_mut` copies on a committing append. That copy is
                // bounded by the committed REF-label count (defs don't grow
                // it) and replaces the every-append clone this path used to
                // pay; folding via a post-`drop(opts)` merge (like the
                // link-ref fold) would break arm-time seeding below, which
                // needs the post-fold tables.
                collect_footnote_refs(
                    committed_slice,
                    Rc::make_mut(&mut self.committed_footnotes),
                    &mut self.next_footnote,
                );
                // BELT-AND-SUSPENDERS: advance the persistent committed occurrence
                // map from the RESOLVED token replay of the committed blocks, NOT
                // from `count_footnote_refs` over the raw committed slice. The raw
                // scan counts `[^x]` inside code spans and escaped `\[^x\]`, which
                // emit no ref token → it would over-count, shifting every later
                // suffix and breaking backrefs. `occ_over_after_block[to_commit-1]`
                // is the overlay (ABSOLUTE counts) after replaying exactly the
                // committed blocks' real refs, so seed == tokens by construction.
                // `to_commit == 0` (nothing renderable committed, only def blocks)
                // leaves the map unchanged. Nothing else holds this `Rc` (the
                // render opts carry an empty occurrence seed), so `make_mut`
                // mutates in place.
                if to_commit > 0 {
                    if let Some(over) = occ_over_after_block.get(to_commit - 1) {
                        if !over.is_empty() {
                            let occ = Rc::make_mut(&mut self.committed_footnote_occurrences);
                            for (k, v) in over {
                                occ.insert(k.clone(), *v);
                            }
                        }
                    }
                }
                // Committed def bodies render with placeholder mode OFF (they are
                // stored permanently and re-emitted in the section verbatim), with
                // the occurrence counter seeded PAST the committed refs so a
                // def-body `[^x]` continues the sequence (mirrors the historical
                // live path + the finalize def path). A def block requires a
                // `[^`, so a slice without one skips the fold (and its
                // occurrence-seed clone). `fn_defs` is finalize-local (never an
                // `Rc` alias), so `make_mut` mutates the def table in place.
                if committed_slice.contains("[^") {
                    let commit_defs_opts = RenderOpts {
                        footnote_placeholder: false,
                        footnote_occ: std::cell::RefCell::new(
                            (*self.committed_footnote_occurrences).clone(),
                        ),
                        ..opts.clone()
                    };
                    collect_footnote_defs(
                        committed_slice,
                        Rc::make_mut(&mut self.committed_footnote_defs),
                        &commit_defs_opts,
                    );
                }
            }
            self.committed_offset = tail_start + last_raw_end_to_commit;
        }

        // At finalize, emit the footnote section as a final block (once).
        if finalizing && gfm_footnotes {
            // One-time merge of the two numbering layers. `fn_committed` is the
            // PRE-fold snapshot (the fold above copied-on-write), so def-body-only
            // labels folded this call don't appear — matching the one-shot
            // pre-pass, which numbers renderable content only.
            let mut fn_nums = (*fn_committed).clone();
            for (k, v) in fn_tail.iter() {
                fn_nums.insert(k.clone(), *v);
            }
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

        // The commit point — and, with `retain_committed_html` off, the html
        // DROP point. Safe to drop here because every rewrite this reparse
        // performs already landed in `newly_committed` above (footnote
        // id/backref resolution renders into the block html before the split,
        // and the finalize-time footnote section is pushed as its own block a
        // few lines up), and `newly_committed` — which keeps the full bytes — is
        // what the patch carries out. Nothing downstream re-reads a committed
        // block's html: `newly_committed` is built only from `produced` (blocks
        // freshly rendered this call), no incremental fast path commits
        // (`newly_committed: Vec::new()` in every one), and `finish_patch` reads
        // `patch.active` only. So a committed block crosses the wire exactly
        // once (WIRE.md §2) and `committed_blocks` is, on this path, write-only.
        for b in newly_committed.iter() {
            self.committed_blocks.push(if self.retain_committed_html {
                b.clone()
            } else {
                // Metadata only: the html is never copied (a `clone()` then
                // clear would still pay the memcpy the flag exists to avoid).
                Block {
                    id: b.id,
                    kind: b.kind.clone(),
                    start: b.start,
                    end: b.end,
                    html: String::new(),
                    open: b.open,
                    speculative: b.speculative,
                }
            });
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
        self.container_block_cache = None;
        self.deep_quote_cache = None;
        self.list_cache = None;
        self.indented_cache = None;
        self.html_cache = None;
        self.component_cache = None;
        self.heading_cache = None;
        self.rule_cache = None;
        self.fence_info_cache = None;
        self.dollar_tail_cache = None;
        self.alnum_tail_cache = None;
        self.raw_tag_tail_cache = None;
        self.mod3_tail_cache = None;
        if !finalizing && new_active.len() == 1 {
            let raw = renderable[to_commit];
            let start = tail_start + raw.range.start;
            let gap_blank = self.buffer.as_bytes()[self.committed_offset..start]
                .iter()
                .all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
            if gap_blank {
                match &raw.kind {
                    RawBlockKind::CodeFence { terminated: false, info, fence_char, .. } => {
                        self.fence_cache = build_code_fence_cache(
                            &self.buffer,
                            start,
                            info,
                            new_active[0].id,
                            new_active[0].kind.clone(),
                        );
                        // The builder above needs a complete opener line. While
                        // the opener's info string is still growing, arm the
                        // provisional frozen-output cache instead (the rendered
                        // block depends only on the settled first info word).
                        if self.fence_cache.is_none() {
                            self.fence_info_cache = build_fence_info_cache(
                                &self.buffer,
                                start,
                                info,
                                *fence_char,
                                new_active[0].id,
                                new_active[0].kind.clone(),
                            );
                        }
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
                            self.next_footnote,
                        );
                        // When the commit cut is pinned at the paragraph start by
                        // an unmatched single `$` opener (the `$x $x …` soup — a
                        // future closer could pair all the way back, so nothing
                        // ever commits), the paragraph cache can't arm. Arm the
                        // dollar-tail fast path instead: the whole open block is
                        // one speculative inline-math span whose escaped body just
                        // grows, so it extends in O(new bytes).
                        if self.para_cache.is_none() && self.gfm_math {
                            self.dollar_tail_cache =
                                build_dollar_tail_cache(&self.buffer, start, new_active[0].id);
                        }
                        // Same pinned-cut situation for the `aaaa…` giant word:
                        // with extended autolinks on, a pure-alnum run has no
                        // boundary candidate (a future `@`/`.` could bind it), so
                        // the cache can't arm. The whole open block is one alnum
                        // run whose escaped body just grows, so the alnum-tail
                        // fast path extends it in O(new bytes).
                        if self.para_cache.is_none() && self.gfm_autolinks {
                            self.alnum_tail_cache =
                                build_alnum_tail_cache(&self.buffer, start, new_active[0].id);
                        }
                        // And for a never-closing raw open tag whose quoted attr
                        // value streams to EOF (`<a href="…`): the failed-`<`
                        // unstable mark pins the cut at the `<`, so the cache can't
                        // arm. While suppressed the render is a CONSTANT `<p></p>`,
                        // so the raw-tag fast path extends it in O(1). (The three
                        // caches are mutually exclusive by paragraph first byte:
                        // `$` / alnum / `<`.)
                        if self.para_cache.is_none() && (self.unsafe_html || self.html_sanitize) {
                            self.raw_tag_tail_cache =
                                build_raw_tag_tail_cache(&self.buffer, start, new_active[0].id);
                        }
                        // And for the `a**bc* c* …` mod-3 soup: a lone can-open
                        // AND can-close `**` that every later single `*` is
                        // mod-3-blocked from closing stays an unpaired opener, so
                        // `earliest_open` pins the cut at 0 and the cache can't
                        // arm. While the `**` is the sole opener the paragraph is
                        // all-literal, so the mod3-tail fast path extends the
                        // escaped body in O(new bytes). Emphasis is always on, so
                        // this needs no config gate; it is mutually exclusive with
                        // the three caches above by the paragraph's shape (first
                        // non-space byte `$` / all-alnum / `<` vs. a `**` run).
                        if self.para_cache.is_none() {
                            self.mod3_tail_cache =
                                build_mod3_tail_cache(&self.buffer, start, new_active[0].id);
                        }
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
                            self.next_footnote,
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
                            self.next_footnote,
                        );
                        // The paragraph-only cache bails (returns None) on
                        // STRUCTURED inner content (a list/quote/heading/table/…).
                        // A monotonically-deepening prose staircase — the shape the
                        // recursive cache streams worse-than-quadratically — gets the
                        // iterative `DeepQuoteCache` first (top level, block_data off);
                        // everything else arms the recursive nested-parser cache so it
                        // still streams in O(new bytes) instead of re-parsing the
                        // whole growing tail each append.
                        if self.container_cache.is_none() {
                            let id = new_active[0].id;
                            if self.container_depth == 0
                                && !self.block_data
                                && !self.gfm_footnotes
                            {
                                self.deep_quote_cache =
                                    self.build_deep_quote_cache(start, id, &opts);
                            }
                            if self.deep_quote_cache.is_none()
                                && self.container_depth < MAX_CONTAINER_DEPTH
                            {
                                let kind = new_active[0].kind.clone();
                                self.container_block_cache =
                                    self.build_container_block_cache(start, id, &kind, &opts);
                            }
                        }
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
                            self.next_footnote,
                        );
                    }
                    RawBlockKind::IndentedCode => {
                        self.indented_cache = build_indented_cache(
                            &self.buffer,
                            start,
                            new_active[0].id,
                            self.block_data,
                        );
                    }
                    RawBlockKind::HtmlBlock { closed: false } => {
                        self.html_cache =
                            build_html_cache(&self.buffer, start, new_active[0].id, &opts);
                    }
                    RawBlockKind::ComponentBlock { tag, terminated: false } => {
                        // Recursive nested-parser cache, mirroring the
                        // structured-container arm (incl. its depth bound).
                        if self.container_depth < MAX_CONTAINER_DEPTH {
                            self.component_cache = self.build_component_block_cache(
                                start,
                                tag,
                                new_active[0].id,
                                new_active[0].kind.clone(),
                            );
                        }
                    }
                    RawBlockKind::Heading { level } => {
                        self.heading_cache = build_heading_cache(
                            &self.buffer,
                            start,
                            *level,
                            new_active[0].id,
                            &opts,
                            &self.committed_footnote_occurrences,
                            self.next_footnote,
                        );
                    }
                    RawBlockKind::HorizontalRule => {
                        self.rule_cache = build_rule_cache(&self.buffer, start, new_active[0].id);
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
            collect_refs(
                committed_slice,
                Rc::make_mut(&mut self.committed_refs),
                ctx,
                self.gfm_alerts,
                0,
            );
        }

        Patch { newly_committed, active: new_active, active_deltas: Vec::new() }
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
                    if cache.has_body_line {
                        cache.escaped_lines.push('\n');
                        if let Some(d) = &mut cache.decoded_lines {
                            d.push('\n');
                        }
                    }
                    cache.has_body_line = true;
                    push_fence_body_line(
                        &bytes[pos..content_end],
                        cache.fence_indent,
                        &mut cache.escaped_lines,
                        cache.decoded_lines.as_mut(),
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
        // `escaped_lines` is the complete body lines joined by `\n` with NO trailing
        // `\n`; restore the last complete line's `\n` (via `has_body_line`, NOT
        // `!escaped_lines.is_empty()`, so a leading/only blank body line keeps its
        // `\n`) so `html[body_start..]` becomes exactly `escape(raw_body)`.
        if cache.has_body_line {
            html.push('\n');
        }
        if !partial.is_empty() {
            // The partial starts at a line start, so it sheds the opener's
            // indent exactly like a folded line does — O(partial), the same
            // re-escape this line already paid.
            push_fence_body_line(partial, cache.fence_indent, &mut html, None);
        }
        if cache.trim_body {
            // Math: trim the body's surrounding whitespace. Whitespace bytes survive
            // escape_html unchanged, so trimming the escaped output equals trimming
            // the source. (Leading whitespace was already dropped at arm time.)
            let trimmed = html.trim_end_matches([' ', '\t', '\n', '\r']).len();
            html.truncate(trimmed.max(body_start));
        } else {
            // Code: mirror `render_code_fence` exactly — strip ALL trailing
            // `\n`/`\r` from the body, then append a single `\n` iff the body is
            // non-empty (so a trailing blank body line collapses, e.g. ` ```\n\n `
            // → `<pre><code></code></pre>`, while a leading blank is preserved).
            let trimmed = html[body_start..].trim_end_matches(['\n', '\r']).len();
            html.truncate(body_start + trimmed);
            if html.len() > body_start {
                html.push('\n');
            }
        }
        // Opt-in structured channel: the decoded source. The HTML body just
        // assembled is exactly `escape_html(buffer[cache.body_start..end])` plus
        // the trailing trim, and whitespace passes `escape_html` unchanged, so the
        // decoded source is the RAW buffer slice with the SAME trim — byte-identical
        // to inverting the escape, without the O(body) per-append entity decode
        // (which made a long streamed fence an O(n²) wall cliff under `block_data`).
        // The trim is driven by `trim_body`, not the carrier kind: a ```math fence
        // carries MathBlock data with the code-fence trim, exactly like the full
        // path (`render_code_fence` → `MathBlockData`). Off (or for a Mermaid
        // fence, which carries no enrichment) ⇒ the frozen `cache.kind` is cloned
        // (a refcount bump for any `Rc` payload).
        let kind = if self.block_data
            && matches!(cache.kind, BlockKind::CodeBlock { .. } | BlockKind::MathBlock(_))
        {
            // Flush-left opener: the HTML body is exactly `escape_html` of the
            // buffer slice, so the decoded source IS that slice. An indented
            // opener de-indents every line, so the slice no longer matches —
            // assemble from the `decoded_lines` twin instead (already folded
            // line-by-line), plus this append's short partial.
            let deindented_body;
            let raw_body: &str = match &cache.decoded_lines {
                None => &self.buffer[cache.body_start..end],
                Some(d) => {
                    let mut s = String::with_capacity(d.len() + partial.len() + 2);
                    s.push_str(d);
                    if cache.has_body_line {
                        s.push('\n');
                    }
                    crate::render::strip_cols_into(partial, cache.fence_indent, &mut s);
                    deindented_body = s;
                    &deindented_body
                }
            };
            let src = if cache.trim_body {
                // Math: mirror the whitespace trim above (leading whitespace was
                // already skipped at arm time via `body_start`).
                raw_body.trim_end_matches([' ', '\t', '\n', '\r']).to_string()
            } else {
                // Code: mirror `render_code_fence` + `code_body_source` — strip all
                // trailing `\n`/`\r`, then a single `\n` iff the body is non-empty.
                let trimmed = raw_body.trim_end_matches(['\n', '\r']);
                let mut s = String::with_capacity(trimmed.len() + 1);
                s.push_str(trimmed);
                if !s.is_empty() {
                    s.push('\n');
                }
                s
            };
            match &cache.kind {
                // `lang`/`meta` were parsed ONCE from the opener line at arm time
                // and live in `cache.kind`; the per-append re-emit is just a small
                // `String` clone of each, never a re-parse of the info string.
                BlockKind::CodeBlock { lang, meta, .. } => BlockKind::CodeBlock {
                    lang: lang.clone(),
                    meta: meta.clone(),
                    code: Some(Rc::new(src)),
                },
                _ => BlockKind::MathBlock(Some(crate::blocks::MathBlockData {
                    latex: Rc::new(src),
                })),
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
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// O(new bytes) extension of a long open indented-code block at the tail.
    /// Folds each newly-complete ≥4-indent line (or interior blank line) into
    /// the cached body and re-renders only the trailing partial. Returns `None`
    /// (dropping the cache) the moment the block ends or is no longer the sole
    /// open tail — a dedented content line, or a `\r` — and the full reparse
    /// takes over.
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
                    let line = &bytes[pos..content_end];
                    // A dedented CONTENT line ends the block; a CRLF line routes
                    // through the full renderer. A blank (whitespace-only) line
                    // folds like content — interior blanks are part of the block
                    // (§4.4) and trailing ones vanish in the assembly trim below.
                    let blank = line.iter().all(|&b| matches!(b, b' ' | b'\t'));
                    if line.contains(&b'\r') || (!blank && !indented_code_line(line)) {
                        return None;
                    }
                    if !cache.escaped_lines.is_empty() {
                        cache.escaped_lines.push('\n');
                        if self.block_data {
                            cache.decoded_lines.push('\n');
                        }
                    }
                    push_indented_content(line, &mut cache.escaped_lines);
                    if self.block_data {
                        let raw = indented_strip(line);
                        cache.decoded_lines.push_str(std::str::from_utf8(raw).unwrap_or(""));
                    }
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
        // Assemble: <pre><code> + chop_blank_lines(body[+ "\n" + partial]) + "\n"
        // + </code></pre>. Whitespace survives escape_html unchanged, so chopping
        // the escaped output equals chopping the decoded source — exactly what
        // render_indented_code does (trailing blank LINES go; trailing spaces on
        // the last content line stay — CommonMark example 118).
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
        let trimmed = chop_trailing_blank_lines(&html[body_start..]).len();
        html.truncate(body_start + trimmed);
        // Opt-in structured channel: the decoded source is the chopped body + "\n".
        // The HTML body is `escape_html(decoded_lines [+ '\n' + stripped partial])`
        // with the same trailing trim, and whitespace passes `escape_html`
        // unchanged, so assembling from the RAW `decoded_lines` twin is
        // byte-identical to inverting the escape — without the O(body) per-append
        // entity decode (an O(n²) wall cliff under `block_data`).
        let kind = if self.block_data {
            let mut code =
                String::with_capacity(cache.decoded_lines.len() + partial.len() + 2);
            code.push_str(&cache.decoded_lines);
            if !partial_blank {
                if !cache.decoded_lines.is_empty() {
                    code.push('\n');
                }
                code.push_str(std::str::from_utf8(indented_strip(partial)).unwrap_or(""));
            }
            let t = chop_trailing_blank_lines(&code).len();
            code.truncate(t);
            code.push('\n');
            // Indented code has no info string — never a lang or meta.
            BlockKind::CodeBlock { lang: None, meta: None, code: Some(Rc::new(code)) }
        } else {
            BlockKind::CodeBlock { lang: None, meta: None, code: None }
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
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// O(new bytes) extension of a long open raw-HTML block at the tail. Folds
    /// each newly-complete line into the cached prefix (pass-through or escaped)
    /// — or, in sanitize mode, each newly-complete TOKEN — and re-processes only
    /// the trailing partial. Returns `None` (dropping the cache) the moment the
    /// block's type-specific close condition is met (so the full reparse closes +
    /// commits it), or on a `\r`.
    fn try_incremental_html(&mut self) -> Option<Patch> {
        let mut cache = self.html_cache.take()?;
        // The pass-through / sanitize decisions must still hold (options don't
        // change mid-stream, but stay defensive: a changed setting voids the
        // cache).
        if cache.pass_through != (self.unsafe_html && !self.html_sanitize)
            || cache.tagfilter != (cache.pass_through && self.gfm_tagfilter)
            || cache.sanitize
                != html_block_sanitizes(self.block_html, self.html_sanitize, cache.html_type)
        {
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
                    // Sanitize mode folds at token boundaries after this loop —
                    // a tag spans lines, so there is no per-line output here.
                    if !cache.sanitize {
                        fold_html_line(
                            &bytes[pos..next],
                            cache.pass_through,
                            cache.tagfilter,
                            &mut cache.cached_prefix,
                        );
                    }
                    cache.lines_upto = next;
                    pos = next;
                }
            }
        }
        // The trailing partial line is re-processed each append (it is short). It
        // ends the block iff it satisfies the close condition — bail then. An
        // EMPTY partial (buffer ends exactly at `\n`) is "no next line yet", not
        // a blank line, so it must not trip the type-6/7 blank-line close (that
        // check is vacuously true on zero bytes).
        let partial = &bytes[cache.lines_upto..end];
        if !partial.is_empty() && html_block_closes_here(partial, html_type, partial) {
            return None;
        }
        let mut html = String::with_capacity(cache.cached_prefix.len() + partial.len() + 32);
        if cache.sanitize {
            // Token fold: only the bytes past `tokens_upto` are sanitized, and
            // only complete tokens/settled text advance it. The lists are
            // borrowed (no per-append clone) via the same `HtmlPolicy` the full
            // path projects out of its `RenderOpts`.
            let policy = HtmlPolicy {
                allowlist: &self.html_allowlist,
                drop: &self.html_drop,
                allow_schemes: &self.allow_schemes,
            };
            cache.tokens_upto = fold_block_html(
                &self.buffer,
                cache.tokens_upto,
                policy,
                &mut cache.open_tags,
                &mut cache.cached_prefix,
            );
            // The region past `tokens_upto` is a still-streaming `<…`: suppressed
            // (pending-invisible), exactly as the full path renders the open tail.
            // Trailing newlines are not content; the closers and the block's
            // single `\n` follow them. Same assembled shape as the full path's
            // `render_sanitized_html_block`.
            html.push_str(&cache.cached_prefix);
            let trimmed = html.trim_end_matches(['\n', '\r']).len();
            html.truncate(trimmed);
            push_html_closers(&cache.open_tags, &mut html);
            html.push('\n');
        } else if cache.pass_through {
            // Pass-through: prefix + partial verbatim, trailing newlines trimmed,
            // then a single `\n` (matches render_html_block's pass-through). With
            // the tagfilter on, the partial is filtered per append (end-of-chunk
            // is a tag boundary, exactly like the full path at buffer EOF).
            html.push_str(&cache.cached_prefix);
            let partial_str = std::str::from_utf8(partial).unwrap_or("");
            if cache.tagfilter {
                push_tagfiltered(partial_str, &mut html);
            } else {
                html.push_str(partial_str);
            }
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
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// Inline-render options for a streaming tail render. Reference + footnote
    /// tables come from the committed region (an open block defines none of its
    /// own); footnote numbers continue from the committed count over the cache
    /// region via `fn_nums`, which the caller has already extended over the
    /// region's NEW bytes — mirroring the full path's pre-pass at O(new bytes)
    /// per append instead of O(region). Every map here is an O(1) `Rc` share or
    /// a fresh empty one.
    fn build_inline_opts(&self, fn_nums: &RegionFnNums) -> RenderOpts {
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
            gfm_tagfilter: self.gfm_tagfilter,
            gfm_math: self.gfm_math,
            dir_auto: self.dir_auto,
            lenient_lists: self.lenient_lists,
            soft_breaks: self.soft_breaks,
            a11y: self.a11y,
            block_data: self.block_data,
            gfm_footnotes: self.gfm_footnotes,
            footnotes: Rc::clone(&self.committed_footnotes),
            tail_footnotes: Rc::clone(&fn_nums.tail),
            // Placeholder mode never touches the occurrence counter — the cache
            // folds resolve tokens against their own cache-local maps.
            footnote_occ: std::cell::RefCell::new(HashMap::new()),
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
            block_html: self.block_html,
            allow_schemes: self.allow_schemes.clone(),
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
            lenient_lists: self.lenient_lists,
        };
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        // The paragraph must still be the tail (only whitespace before it).
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        // …and must still run to EOF (no blank line / interrupting block /
        // setext underline appeared after the committed cut). One end is
        // TRANSIENT and gets suspended instead of dropped: see
        // [`ParaEnd::OpenBlank`]. Keep the cache and render the paragraph as it
        // stands, stopping at that line — byte-for-byte the closed view the full
        // path produces.
        let mut suspended = false;
        let mut block_end = len;
        match paragraph_end(bytes, cache.cut, ctx) {
            ParaEnd::No => {}
            ParaEnd::OpenBlank(s) => {
                suspended = true;
                block_end = s;
            }
            ParaEnd::Settled => return None,
        }
        let mut content_end = block_end;
        while content_end > cache.start && matches!(bytes[content_end - 1], b'\n' | b'\r') {
            content_end -= 1;
        }
        if content_end < cache.cut {
            return None;
        }
        // Extend the incremental numbering over the region's new bytes, then
        // share it into the opts (O(1); mirrors the full path's pre-pass).
        if self.gfm_footnotes {
            cache.fn_nums.extend(&self.buffer, content_end, &self.committed_footnotes);
        }
        let opts = self.build_inline_opts(&cache.fn_nums);
        // While suspended the paragraph no longer abuts buffer EOF (the blank
        // line follows it), so the full path renders it with `open_tail` OFF —
        // an incomplete `[x](`/`` `code ``/`$math` at its end is literal, not
        // speculative. Match that or the suspended view diverges. The frozen
        // `committed_inner` is unaffected either way: every open-tail
        // speculation site marks `unstable`, and `compute_cut` never returns a
        // cut past `unstable`, so no construct in `[start, cut)` ever reached a
        // slice end unresolved.
        let opts = if suspended { RenderOpts { open_tail: false, ..opts } } else { opts };
        // Render the active region and learn how far of it is now settled — past
        // closed emphasis / code spans / inline links, but not an unpaired opener
        // or unclosed construct. `boundary_rel` is relative to the active slice.
        let mut active = String::new();
        let boundary_rel =
            render_inline_para_boundary(&self.buffer[cache.cut..content_end], &opts, &mut active);
        let new_cut = cache.cut + boundary_rel;
        if new_cut > cache.cut {
            // Commit [cut..new_cut] by rendering that segment on its own — a clean
            // boundary guarantees it equals its slice of the full render — then
            // re-render the now-shorter active tail. Resolve the just-committed
            // segment's footnote tokens into `committed_inner`, advancing the
            // frozen-prefix occurrence map (resolve-on-commit; never re-touched).
            let mut seg = String::new();
            render_inline_para(&self.buffer[cache.cut..new_cut], &opts, &mut seg);
            resolve_footnote_ids(&seg, &mut cache.fn_occ, &mut cache.committed_inner);
            cache.cut = new_cut;
            active.clear();
            render_inline_para(&self.buffer[cache.cut..content_end], &opts, &mut active);
        }
        // Resolve the speculative active tail per-append from a discarded
        // OVERLAY seeded lazily from the frozen-prefix occurrence map (does NOT
        // advance persistent state, and never clones the growing map — the
        // overlay holds only the short active tail's labels). No-op byte-copy
        // when footnotes are off (no tokens present).
        if self.gfm_footnotes {
            let mut over = HashMap::new();
            let mut resolved = String::with_capacity(active.len());
            resolve_footnote_ids_overlay(&active, &cache.fn_occ, &mut over, &mut resolved);
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
            // While suspended the block stops at the trailing blank line, exactly
            // like the raw block the full scan produces. It stays open +
            // speculative either way — the full path does NOT commit a paragraph
            // at a trailing whitespace-only line (verified: a blank line only
            // settles it once a following block arrives), which is what makes
            // resuming safe: nothing a consumer was told is final gets retracted.
            end: block_end,
            html,
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.para_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// O(new bytes) extension of an open single-`$` inline-math span at the tail
    /// (the `$x $x …` soup — see [`DollarTailCache`]). While the span stays open
    /// its render is `<p><span class="math math-inline">escape_html(body)</span></p>`,
    /// and `escape_html` is a context-free per-byte map, so the escaped body just
    /// grows by the appended bytes. Returns `None` (dropping the cache) the moment
    /// the span could have closed / split — a newline, a valid `$` closer, or the
    /// block no longer being the sole tail — so the full reparse handles it,
    /// byte-identically.
    fn try_incremental_dollar_tail(&mut self) -> Option<Patch> {
        let mut cache = self.dollar_tail_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        // The paragraph must still be the sole tail block (only whitespace before
        // it, nothing committed past its start).
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        // Fold only the appended bytes. The span drops the moment they add a
        // newline (the single line becomes multi-line — the full path handles
        // the split) or a valid `$` closer (the opener pairs forward, so the
        // span is no longer speculative-open). `cache.scanned >= cache.start + 2`
        // (the opener plus a non-`$` body byte, guaranteed at arm), so the closer
        // check's `i - 1` lookback stays strictly right of the opener.
        if len > cache.scanned {
            if !dollar_span_stays_open(bytes, cache.start, cache.scanned, len) {
                return None;
            }
            escape_html(&self.buffer[cache.scanned..len], &mut cache.math);
            cache.scanned = len;
        }
        // `<p><span class="math math-inline">body</span></p>` — `render_paragraph`'s
        // opener + a single speculative-open math span; its trailing-whitespace
        // trim is a no-op (the inner always ends in `</span>`).
        let mut html = String::with_capacity(cache.math.len() + 48);
        html.push_str("<p");
        // Mirrors `RenderOpts::dir()` (static — independent of content).
        if self.dir_auto {
            html.push_str(" dir=\"auto\"");
        }
        html.push_str("><span class=\"math math-inline\">");
        html.push_str(&cache.math);
        html.push_str("</span></p>");
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
        self.dollar_tail_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// O(new bytes) extension of an open pure-ASCII-alphanumeric paragraph at the
    /// tail (the `aaaa…` giant word — see [`AlnumTailCache`]). While the run stays
    /// pure-alnum its render is `<p>escape_html(body)</p>`, and `escape_html` is a
    /// context-free per-byte map that leaves alnum unchanged, so the escaped body
    /// just grows by the appended bytes. Returns `None` (dropping the cache) the
    /// moment a non-alnum byte appears — a space/`.`/`@`/`:`/newline that could
    /// settle the cut or trigger a construct — so the full reparse handles it,
    /// byte-identically.
    fn try_incremental_alnum_tail(&mut self) -> Option<Patch> {
        let mut cache = self.alnum_tail_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        // The paragraph must still be the sole tail block (only whitespace before
        // it, nothing committed past its start).
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        // Fold only the appended bytes. The run drops the moment they add any
        // non-alnum byte (a boundary settles, a construct/autolink could open, or
        // the line ends) — the full path then re-renders that append.
        if len > cache.scanned {
            if !alnum_run_stays_open(bytes, cache.scanned, len) {
                return None;
            }
            escape_html(&self.buffer[cache.scanned..len], &mut cache.body);
            cache.scanned = len;
        }
        // `<p>body</p>` — `render_paragraph`'s opener + the escaped run; its
        // trailing-whitespace trim is a no-op (the body has no whitespace).
        let mut html = String::with_capacity(cache.body.len() + 24);
        html.push_str("<p");
        // Mirrors `RenderOpts::dir()` (static — independent of content).
        if self.dir_auto {
            html.push_str(" dir=\"auto\"");
        }
        html.push('>');
        html.push_str(&cache.body);
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
        self.alnum_tail_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// O(1) extension of an open never-closing raw tag whose quoted attribute
    /// value streams to EOF (`<a href="…` — see [`RawTagTailCache`]). While the
    /// tag stays EOF-streaming inside the value, `inline_html_streams_to_eof`
    /// suppresses it, so the paragraph render is the CONSTANT `<p></p>` and no new
    /// bytes enter the inline renderer. Returns `None` (dropping the cache) the
    /// moment the appended bytes close the value (a matching quote — the tag can
    /// now complete or gain attrs) or add a newline (the single-line paragraph
    /// splits) — so the full reparse handles it, byte-identically.
    fn try_incremental_raw_tag_tail(&mut self) -> Option<Patch> {
        let mut cache = self.raw_tag_tail_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        // The paragraph must still be the sole tail block (only whitespace before
        // it, nothing committed past its start).
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        // Scan only the appended bytes for a value-closing quote or a newline.
        if len > cache.scanned {
            if !raw_tag_stays_open(bytes, cache.scanned, len, cache.quote) {
                return None;
            }
            cache.scanned = len;
        }
        // `<p></p>` — the suppressed tag renders nothing; the trailing-whitespace
        // trim over the empty inner is a no-op.
        let mut html = String::with_capacity(24);
        html.push_str("<p");
        // Mirrors `RenderOpts::dir()` (static — independent of content).
        if self.dir_auto {
            html.push_str(" dir=\"auto\"");
        }
        html.push_str("></p>");
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
        self.raw_tag_tail_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// O(new bytes) extension of the open `a**bc* c* …` mod-3 soup at the tail
    /// (see [`Mod3TailCache`]). While the lone `**` is the sole opener the
    /// paragraph is all-literal, so its render is `<p>escape_html(body)</p>`
    /// (trailing whitespace stripped) and the escaped body just grows by the
    /// newly-settled bytes. Returns `None` (dropping the cache) the moment an
    /// appended byte could restructure the render — a `*` run of decided length
    /// ≥ 2, a single `*` that could open, a newline, or any construct/entity/
    /// non-ASCII byte — so the full reparse handles it, byte-identically. A `*`
    /// run abutting the chunk edge is held PENDING (rendered literally in the
    /// open view but excluded from the settled body) until the next append
    /// decides it, so a `*` landing on a chunk boundary never forces a drop.
    fn try_incremental_mod3_tail(&mut self) -> Option<Patch> {
        let mut cache = self.mod3_tail_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        // The paragraph must still be the sole tail block (only whitespace before
        // it, nothing committed past its start).
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        // Fold only the bytes past the settled boundary. The re-scan starts at the
        // pending `*` (0 or 1 byte), so it stays O(new bytes); `mod3_body_scan`
        // returns `None` the instant a byte could restructure the all-literal
        // render — the full path then re-renders that append.
        if len > cache.settled {
            let new_settled = mod3_body_scan(bytes, cache.settled, len)?;
            escape_html(&self.buffer[cache.settled..new_settled], &mut cache.body);
            cache.settled = new_settled;
        }
        // `<p>body</p>` — `render_paragraph`'s opener + the settled escaped body,
        // plus any PENDING trailing `*` run (all `*`, rendered literally). The
        // trailing-whitespace trim bites only when nothing is pending (a trailing
        // `*` is never whitespace), mirroring `render_paragraph` exactly.
        let pending = len - cache.settled;
        let mut html = String::with_capacity(cache.body.len() + pending + 24);
        html.push_str("<p");
        // Mirrors `RenderOpts::dir()` (static — independent of content).
        if self.dir_auto {
            html.push_str(" dir=\"auto\"");
        }
        html.push('>');
        if pending == 0 {
            // CommonMark: trailing whitespace at end of the final line is stripped
            // (mirrors `render_paragraph`'s in-place trim of the rendered inner).
            let keep = cache.body.trim_end_matches([' ', '\t', '\n', '\r']).len();
            html.push_str(&cache.body[..keep]);
        } else {
            html.push_str(&cache.body);
            for _ in 0..pending {
                html.push('*');
            }
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
        self.mod3_tail_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
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
            lenient_lists: self.lenient_lists,
        };
        // Build inline opts once for the whole append: the same shared RenderOpts
        // backs cached-row rendering and the speculative partial-row render. The
        // open table's OWN cells may carry the first reference to a footnote
        // label, so the footnote-numbering pre-pass must see the table content
        // (mirrors the full path, which numbers refs over every renderable
        // block) — extended incrementally over only the region's new bytes.
        if self.gfm_footnotes {
            cache.fn_nums.extend(&self.buffer, end, &self.committed_footnotes);
        }
        let opts = self.build_inline_opts(&cache.fn_nums);

        // Fold every newly-complete body row into the cache. A blank/interrupting
        // line bails: the table has ended, full reparse takes over so the block
        // boundary updates correctly.
        let mut pos = cache.lines_upto;
        // The previous append's partial-row scan already proved there is no
        // `\n` before `p.scanned`, so the newline search starts at the new
        // bytes — O(new) per append instead of O(partial).
        let nl_hint = match &cache.partial {
            Some(p) if p.line_start == pos && p.scanned > pos => p.scanned.min(end),
            _ => pos,
        };
        while pos < end {
            let search = nl_hint.max(pos);
            let r = match bytes[search..end].iter().position(|&b| b == b'\n') {
                None => break, // trailing partial line — handled below
                Some(r) => search + r - pos,
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
            // An all-Unicode-whitespace line (`is_blank_line` is ASCII-only, so
            // it didn't end the table above): the full renderer drops trim-empty
            // body lines without ending the table — fold no row for it.
            if line_str.trim().is_empty() {
                cache.lines_upto = next;
                pos = next;
                continue;
            }
            let cells = split_table_cells(line_str);
            if !cache.tbody_opened {
                cache.cached_prefix.push_str("<tbody>\n");
                cache.tbody_opened = true;
            }
            // Render the row into a scratch buffer (placeholder tokens when
            // footnotes on), then resolve its tokens into `cached_prefix`,
            // advancing the cache-local occurrence map. Once folded the row is
            // never re-rendered (frozen-prefix invariant). The data-channel
            // cells resolve BEFORE the row advances `fn_occ`, from a discarded
            // overlay seeded lazily off it (same pre-row counts as the old
            // full-map clone, without copying the growing map).
            let mut row_html = String::with_capacity(line_str.len() + 16);
            row_html.push_str("<tr>\n");
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
            row_html.push_str("</tr>\n");
            // Structured channel: fold this committed row's cells in lock-step
            // with its `<tr>` — once folded it's never re-rendered (HTML
            // invariant). Resolved first (pre-advance `fn_occ` = the row's seed).
            if opts.block_data {
                if opts.gfm_footnotes {
                    let mut over = HashMap::new();
                    for c in &mut row {
                        let mut o = String::with_capacity(c.html.len());
                        resolve_footnote_ids_overlay(&c.html, &cache.fn_occ, &mut over, &mut o);
                        c.html = o;
                    }
                }
                cache.body_cells.push(Rc::new(row));
            }
            resolve_footnote_ids(&row_html, &mut cache.fn_occ, &mut cache.cached_prefix);
            cache.lines_upto = next;
            pos = next;
        }

        // Speculatively render the trailing partial line (no `\n`) as a row, if
        // it's non-empty and not blank. The full renderer treats a final
        // newline-less line as the last row, so we must too.
        let partial = &bytes[cache.lines_upto..end];
        let mut partial_html = String::new();
        // Structured channel: the speculative partial row's cells, built parallel
        // to `partial_html` and NOT folded into `cache.body_cells` (mirrors how
        // `partial_html` is not folded into `cached_prefix`).
        let mut partial_row: Option<Vec<TableCell>> = None;
        if !partial.is_empty() {
            if opts.block_data {
                // Whole-partial re-render: the structured cells need a full
                // text+html rebuild per append anyway (see `PartialRowCache`).
                if partial.contains(&b'\r') {
                    return None;
                }
                // Deterministic complexity probe (see `perf` in lib.rs): this
                // path re-renders the whole partial per append — O(n²/chunk)
                // while the trailing line never ends. Counted so a `block_data`
                // scaling shape would see it.
                #[cfg(feature = "perf_counters")]
                crate::perf::add_scan(partial.len());
                let line_str = std::str::from_utf8(partial).unwrap_or("");
                // The full renderer drops trim-empty body lines (Unicode-aware;
                // `is_blank_line` is ASCII-only) — render no row for one.
                if !line_str.trim().is_empty() {
                    let cells = split_table_cells(line_str);
                    let mut raw_partial = String::with_capacity(line_str.len() + 16);
                    raw_partial.push_str("<tr>\n");
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
                    raw_partial.push_str("</tr>\n");
                    // Resolve the speculative partial row from a discarded
                    // OVERLAY over the frozen-prefix occurrence map (does NOT
                    // advance it and never clones the growing map). Byte-copy
                    // when footnotes off.
                    let mut over = HashMap::new();
                    resolve_footnote_ids_overlay(
                        &raw_partial,
                        &cache.fn_occ,
                        &mut over,
                        &mut partial_html,
                    );
                    if opts.gfm_footnotes {
                        let mut dover = HashMap::new();
                        for c in &mut row {
                            let mut o = String::with_capacity(c.html.len());
                            resolve_footnote_ids_overlay(&c.html, &cache.fn_occ, &mut dover, &mut o);
                            c.html = o;
                        }
                    }
                    partial_row = Some(row);
                }
            } else if !self.extend_partial_row(&mut cache, end, &opts, &mut partial_html) {
                // A `\r` arrived in the partial line — LF-only state; the full
                // path renders CRLF rows (same fallback as the row loop above).
                return None;
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
            html.push_str("<tbody>\n");
        }
        html.push_str(&partial_html);
        if cache.tbody_opened || need_tbody_for_partial {
            html.push_str("</tbody>\n");
        }
        html.push_str("</table>");

        // Structured channel: assemble TableData = header + committed body rows +
        // the speculative partial row (if any), exactly mirroring the HTML the
        // consumer renders. emit-on-every-patch so DATA never lags HTML.
        let kind = if opts.block_data {
            // O(rows) Rc refcount bumps, not an O(cells) String deep clone — the
            // headers/aligns clones are Rc bumps too.
            let mut rows = cache.body_cells.clone();
            if let Some(row) = partial_row {
                rows.push(Rc::new(row));
            }
            BlockKind::Table(Some(TableData {
                headers: Rc::clone(&cache.header_cells),
                rows,
                aligns: Rc::clone(&cache.aligns),
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
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// O(new bytes) extension of the table's trailing newline-less partial row
    /// (see [`PartialRowCache`]). Appends the speculative `<tr>…</tr>` to
    /// `out`, byte-identical to the full renderer's
    /// `split_table_cells` + `for i in 0..ncol { push_table_cell(…) }` over the
    /// same line. Returns `false` when a `\r` arrives in the partial (the
    /// caller drops the whole cache; the full path renders CRLF rows).
    fn extend_partial_row(
        &self,
        cache: &mut TableCache,
        end: usize,
        opts: &RenderOpts,
        out: &mut String,
    ) -> bool {
        // (Re-)arm on a fresh partial line — first partial byte ever, or a row
        // completed since (`lines_upto` moved, and `fn_occ` advanced with it,
        // so the seed clones are captured here).
        if cache.partial.as_ref().map_or(true, |p| p.line_start != cache.lines_upto) {
            cache.partial = Some(PartialRowCache::new(cache.lines_upto, &cache.fn_occ));
        }
        let p = cache.partial.as_mut().unwrap();

        // Deterministic complexity probe (see `perf` in lib.rs): this path
        // returns before the slow-path counter, so it counts its own re-render
        // work — the newly scanned bytes here, plus the open cell's re-rendered
        // tail below. Linear while the sub-cache is healthy; a stalled boundary
        // or re-scan bug grows it quadratically, which tests/scaling.rs gates.
        #[cfg(feature = "perf_counters")]
        crate::perf::add_scan(end - p.scanned);

        // Level 1: feed only the unscanned bytes through the cell-split
        // automaton (char-for-char `split_table_cells` semantics). A cell
        // closed by an unescaped `|` is final — earlier bytes are immutable,
        // and the cells before any later `|` are the same whether that pipe
        // ends the row (trailing decoration) or opens another cell — so it is
        // rendered once here and never touched again.
        for (off, ch) in self.buffer[p.scanned..end].char_indices() {
            if ch == '\r' {
                return false;
            }
            if p.leading {
                if ch.is_whitespace() {
                    continue;
                }
                p.leading = false;
                p.last_nonws = Some(p.scanned + off);
                if ch == '|' {
                    // The row's one optional leading decoration pipe.
                    p.frozen_end = p.scanned + off + 1;
                    continue;
                }
            } else if !ch.is_whitespace() {
                p.last_nonws = Some(p.scanned + off);
            }
            if p.esc {
                p.esc = false;
                p.push_cell_char(ch);
            } else if ch == '\\' {
                p.esc = true;
            } else if ch == '|' {
                // Freeze the just-closed cell: render once (placeholder
                // footnote tokens), resolve into the frozen HTML advancing
                // `occ`, and reset the open-cell (level 2) state.
                if p.ncells < cache.ncol {
                    let content = &p.cellbuf[..p.trim_len];
                    let mut td = String::with_capacity(content.len() + 16);
                    push_table_cell("td", content, cache.aligns.get(p.ncells), opts, &mut td);
                    resolve_footnote_ids(&td, &mut p.occ, &mut p.html);
                }
                p.ncells += 1;
                p.frozen_end = p.scanned + off + 1;
                p.cellbuf.clear();
                p.trim_len = 0;
                p.cell_committed.clear();
                p.cell_cut = 0;
                p.cell_len = 0;
                p.cell_occ = p.occ.clone();
            } else {
                p.push_cell_char(ch);
            }
        }
        p.scanned = end;

        // All-whitespace-so-far trailing line (Unicode-aware, unlike the ASCII
        // `is_blank_line`): the full renderer drops trim-empty body lines, so
        // emit no row — `out` stays empty and nothing below applies.
        if p.last_nonws.is_none() {
            return true;
        }

        // The open (last) cell's content — `split_table_cells`' trailing
        // behavior, emulated: the line's trailing whitespace never counts
        // (`trim_len`), and one trailing `|` is decoration, not content. If
        // the line's last non-whitespace char is a `|` the scan did NOT
        // consume, it can only be an escaped one sitting at the end of
        // `cellbuf` as a literal — drop it and re-trim, matching the one-shot
        // `strip_suffix('|')` on the raw trimmed line (which strips that pipe
        // textually and leaves its backslash dangling, to be swallowed).
        let strip_pipe = matches!(
            p.last_nonws,
            Some(pos)
                if self.buffer.as_bytes()[pos] == b'|'
                    && pos + 1 != p.frozen_end
                    && p.trim_len > 0
        );
        let content_len = if strip_pipe {
            debug_assert_eq!(p.cellbuf.as_bytes()[p.trim_len - 1], b'|');
            p.cellbuf[..p.trim_len - 1].trim_end().len()
        } else {
            p.trim_len
        };

        // Assemble the speculative row: frozen cells + the open cell + empty
        // padding to `ncol` (extra cells were counted but never rendered).
        out.push_str("<tr>\n");
        out.push_str(&p.html);
        if p.ncells < cache.ncol {
            push_table_cell_open("td", cache.aligns.get(p.ncells), opts, out);
            // Level 2: the boundary contract only covers extensions of the
            // analyzed input; the trailing-`|` emulation can shrink the
            // content, so reset first when it did.
            if content_len < p.cell_len {
                p.cell_committed.clear();
                p.cell_cut = 0;
                p.cell_occ = p.occ.clone();
            }
            p.cell_len = content_len;
            // The open cell's unsettled tail, re-rendered this append (the
            // other half of this path's counted work — see above).
            #[cfg(feature = "perf_counters")]
            crate::perf::add_scan(content_len - p.cell_cut);
            let mut active = String::new();
            let boundary_rel =
                render_inline_boundary(&p.cellbuf[p.cell_cut..content_len], opts, &mut active);
            let new_cut = p.cell_cut + boundary_rel;
            if new_cut > p.cell_cut {
                // Commit [cell_cut..new_cut] by rendering that segment on its
                // own — a clean boundary guarantees it equals its slice of the
                // full render — then re-render the now-shorter active tail
                // (same discipline as `try_incremental_paragraph`).
                let mut seg = String::new();
                render_inline(&p.cellbuf[p.cell_cut..new_cut], opts, &mut seg);
                resolve_footnote_ids(&seg, &mut p.cell_occ, &mut p.cell_committed);
                p.cell_cut = new_cut;
                active.clear();
                render_inline(&p.cellbuf[p.cell_cut..content_len], opts, &mut active);
            }
            out.push_str(&p.cell_committed);
            // Resolve the speculative active tail from a CLONE of the open
            // cell's occurrence map (does NOT advance persistent state).
            if opts.gfm_footnotes {
                let mut occ = p.cell_occ.clone();
                let mut resolved = String::with_capacity(active.len());
                resolve_footnote_ids(&active, &mut occ, &mut resolved);
                out.push_str(&resolved);
            } else {
                out.push_str(&active);
            }
            // The open cell's own line terminator — `push_table_cell` supplies
            // it for every other cell; this one is assembled by hand.
            out.push_str("</td>\n");
            for i in p.ncells + 1..cache.ncol {
                push_table_cell("td", "", cache.aligns.get(i), opts, out);
            }
        }
        out.push_str("</tr>\n");
        true
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
        // reference to a footnote label, so the numbering pre-pass covers the whole
        // container region (the `>` markers don't break `[^label]` matching), so
        // in-container refs get the same number as the full path assigns —
        // extended incrementally over only the region's new bytes.
        if self.gfm_footnotes {
            cache.fn_nums.extend(&self.buffer, end, &self.committed_footnotes);
        }
        let opts = self.build_inline_opts(&cache.fn_nums);

        // Fold every newly-complete `> `-marker line. A blank `>` line closes
        // the current paragraph (rendered once into `committed_paras_html`)
        // and starts a fresh one; a marker-less line that the scanner keeps as a
        // LAZY paragraph continuation is re-emitted at `LAZY_INDENT`; any other
        // line is folded into the current paragraph's `inner_buffer`. Bails on
        // `\r` or a marker-less line that ends the quote.
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
            let line = &bytes[pos..content_end];
            let stripped = match strip_blockquote_marker(line) {
                Some(s) => s,
                None => {
                    // Marker-less line. It stays in the quote only as a lazy
                    // paragraph continuation, under the scanner's exact rule
                    // (`scan_blockquote`): the inner paragraph must be open, the
                    // line non-blank, and the line must not itself start a block.
                    // Anything else ends the quote here — bail to the full
                    // reparse, which commits the container.
                    if cache.inner_buffer.is_empty()
                        || line.iter().all(|&b| matches!(b, b' ' | b'\t'))
                        || would_start_other_block(line, 0, opts.scan_ctx())
                    {
                        return None;
                    }
                    // Emit exactly like `blockquote_inner`: the lazy line keeps
                    // its own line (the soft break is a `\n`) at `LAZY_INDENT`
                    // columns, which is what stops the re-scan reinterpreting it
                    // as a new block.
                    debug_assert!(cache.inner_buffer.ends_with('\n'));
                    push_lazy_line(&mut cache.inner_buffer, std::str::from_utf8(line).ok()?);
                    cache.inner_buffer.push('\n');
                    cache.lines_upto = next;
                    pos = next;
                    continue;
                }
            };
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
            } else if partial.iter().all(|&b| matches!(b, b' ' | b'\t')) {
                // All-whitespace so far — could still become a `>` marker line
                // (or a blank line that ends the quote). No visible content
                // either way: render with what we have committed so far.
            } else if cache.inner_buffer.is_empty()
                || would_start_other_block(partial, 0, opts.scan_ctx())
            {
                // The scanner already excludes this (partial) line from the
                // quote at this prefix — bail so the full reparse owns the
                // container boundary.
                return None;
            } else {
                // Speculative LAZY continuation, mirroring the one-shot scan of
                // this exact prefix: emitted like the complete-line path, minus
                // the terminator it hasn't grown yet. Truncated back after
                // rendering, so committed state is unchanged — and since the
                // previous line's `\n` is no longer consumed, nothing has to be
                // put back.
                debug_assert!(cache.inner_buffer.ends_with('\n'));
                partial_pushed =
                    push_lazy_line(&mut cache.inner_buffer, std::str::from_utf8(partial).ok()?);
            }
        }
        let post_partial_len = cache.inner_buffer.len();
        let committed_inner_end = post_partial_len - partial_pushed;

        // Render boundary on the full active region (committed-tail + partial)
        // for the CURRENT paragraph only. Closed paragraphs are fully settled
        // in `committed_paras_html` and never re-rendered. The active slice has its
        // TRAILING newline(s) trimmed, exactly like `render_paragraph` does before
        // `render_inline` — otherwise a trailing `\n` (present whenever the last
        // inner line is complete) would terminate a speculative open-tail
        // `[x](`/`` `code ``/`$math` at the paragraph's end and flash it literal
        // instead of speculating, diverging from the one-shot render. The trim only
        // touches the very end (never the settled prefix), so the commit boundary
        // and footnote occurrence advance are unchanged.
        let active_slice = trim_trailing_newlines(&cache.inner_buffer[cache.inner_cut..]);
        let mut active_html = String::new();
        let boundary_rel = render_inline_para_boundary(active_slice, &opts, &mut active_html);
        let new_cut = (cache.inner_cut + boundary_rel).min(committed_inner_end);
        if new_cut > cache.inner_cut {
            let mut seg = String::new();
            render_inline_para(&cache.inner_buffer[cache.inner_cut..new_cut], &opts, &mut seg);
            // Resolve the just-settled segment into the open paragraph's frozen
            // prefix, advancing its (discard-on-close) occurrence overlay.
            resolve_footnote_ids_overlay(
                &seg,
                &cache.fn_occ,
                &mut cache.inner_fn_occ,
                &mut cache.committed_inner_html,
            );
            cache.inner_cut = new_cut;
            active_html.clear();
            render_inline_para(
                trim_trailing_newlines(&cache.inner_buffer[cache.inner_cut..]),
                &opts,
                &mut active_html,
            );
        }
        // Resolve the speculative active tail from a CLONE of the open
        // paragraph's (small) occurrence overlay, layered over the never-cloned
        // `fn_occ` (does NOT advance either). Byte-copy when footnotes off.
        if self.gfm_footnotes {
            let mut over = cache.inner_fn_occ.clone();
            let mut resolved = String::with_capacity(active_html.len());
            resolve_footnote_ids_overlay(&active_html, &cache.fn_occ, &mut over, &mut resolved);
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
            // The body-leading `\n` baked into `wrapper_open` STAYS even when the
            // body is totally empty: `render_blockquote` opens with cmark's
            // unconditional `cr()`, so an empty quote is `<blockquote>\n</blockquote>`
            // (examples 218, 239, 240) — and an alert's `\n` is its title
            // separator, which always stayed.
        } else {
            html.push_str(&cache.body_p_close);
        }
        html.push_str(&cache.wrapper_close);

        // Drop the speculative partial bytes so the cache's committed state is
        // unchanged for the next append. Every partial shape appends only —
        // none consumes a committed byte — so the truncate alone restores it.
        cache.inner_buffer.truncate(committed_inner_end);

        // Assemble the opt-in `nested` channel: the stable committed paragraphs
        // (O(paras) Rc refcount bumps) plus the current open paragraph.
        // emit-on-every-patch so DATA never lags HTML, mirroring the table cache.
        let container_data = if opts.block_data {
            let mut nested = cache.committed_paras.clone();
            if let Some(p) = open_para_html {
                nested.push(Rc::new(NestedBlock { html: p }));
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
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// O(new bytes) extension of an open structured blockquote / alert at the
    /// tail (see [`ContainerBlockCache`]). Strips the `>` markers off the newly
    /// arrived bytes, feeds that delta to the recursive nested parser, and
    /// reassembles `wrapper + inner-blocks + wrapper`. Returns `None` (dropping
    /// the cache → full reparse) on any BAIL condition.
    fn try_incremental_container_block(&mut self) -> Option<Patch> {
        let mut cache = self.container_block_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let end = bytes.len();
        // Tail-only check (same as the other caches): nothing but whitespace may
        // sit between the committed boundary and the container's opener.
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }

        // Strip `>` markers off the delta since the last feed → inner markdown.
        let mut mid_line = cache.mid_line;
        let mut last_fed = cache.last_fed;
        let (delta, new_fed) = strip_container_delta(
            bytes,
            cache.fed_outer,
            end,
            &mut mid_line,
            &mut last_fed,
            cache.footnotes,
        )?;
        // Feed the delta to the recursive parser (it owns the partial-trailing-line
        // speculation, giving mid-stream parity). Empty delta still re-renders the
        // open inner tail.
        cache.inner.append(&delta);
        cache.fed_outer = new_fed;
        cache.mid_line = mid_line;
        cache.last_fed = last_fed;

        let html = assemble_container_block(&mut cache);
        // Opt-in structured channel: one `NestedBlock` per inner sub-block —
        // byte-identical to `render_blockquote` / `render_alert`, whose `nested`
        // fragments are captured at exactly these per-block boundaries. Committed
        // inner blocks fold once (Rc bump per re-emit); the open inner tail is
        // rebuilt fresh each append, mirroring the table cache.
        let container_data = if self.block_data {
            for b in &cache.inner.committed_blocks[cache.committed_nested.len()..] {
                cache.committed_nested.push(Rc::new(NestedBlock { html: b.html.clone() }));
            }
            let mut nested = cache.committed_nested.clone();
            nested.extend(
                cache
                    .inner
                    .active_blocks
                    .iter()
                    .map(|b| Rc::new(NestedBlock { html: b.html.clone() })),
            );
            Some(ContainerData { nested })
        } else {
            None
        };
        let kind = match cache.kind {
            ContainerCacheKind::Blockquote => BlockKind::Blockquote(container_data),
            ContainerCacheKind::Alert(ak) => {
                BlockKind::Alert { kind: ak, nested: container_data }
            }
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
        self.container_block_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// Arm the iterative deep-quote staircase cache (see [`DeepQuoteCache`]) for an
    /// open blockquote at `start` whose inner is a monotonically-deepening
    /// prose staircase. `None` (→ the recursive [`ContainerBlockCache`] takes over)
    /// on any non-staircase shape. Only called at the top level with `block_data`
    /// and `gfm_footnotes` off. The first fill processes every line already present;
    /// later appends fold only new bytes.
    fn build_deep_quote_cache(&self, start: usize, id: u64, opts: &RenderOpts) -> Option<DeepQuoteCache> {
        let bytes = self.buffer.as_bytes();
        let end = bytes.len();
        let mut w = String::with_capacity(16);
        w.push_str("<blockquote");
        w.push_str(opts.dir());
        w.push('>');
        let mut cache = DeepQuoteCache {
            start,
            id,
            wrapper_open: w,
            settled: String::new(),
            open_depth: 0,
            // Placeholder — replaced by a fresh parser the moment the first level
            // opens (open_depth 0 → 1); never rendered.
            open: self.make_nested_parser(),
            open_complete: true,
            fed_upto: start,
        };
        if !self.dq_extend(&mut cache, bytes, end) {
            return None;
        }
        // A genuine deepening (≥ 2 levels). Depth-1 is the plain `ContainerCache`'s
        // job; a single un-deepened blockquote never reaches this arm anyway (it
        // only fires once the paragraph cache bailed on structured inner).
        if cache.open_depth < 2 {
            return None;
        }
        Some(cache)
    }

    /// Extend a [`DeepQuoteCache`] over the buffer bytes `[cache.fed_upto, end)`,
    /// folding each settled shallower level once and streaming the deepest line's
    /// content into `cache.open`. Returns `false` (the caller drops the cache → the
    /// full reparse re-arms the byte-identical recursive path) on ANY deviation from
    /// the pure single-step-deepening prose staircase.
    fn dq_extend(&self, cache: &mut DeepQuoteCache, bytes: &[u8], end: usize) -> bool {
        loop {
            if cache.open_depth >= 1 && !cache.open_complete {
                // Stream the deepest line's content (markers already consumed) to
                // `open` until its terminating `\n`.
                let from = cache.fed_upto;
                match bytes[from..end].iter().position(|&b| b == b'\n') {
                    Some(rel) => {
                        let nl = from + rel;
                        if bytes[from..nl].contains(&b'\r') {
                            return false; // CRLF — full path
                        }
                        let Ok(s) = std::str::from_utf8(&bytes[from..nl]) else {
                            return false;
                        };
                        if !s.is_empty() {
                            cache.open.append(s);
                        }
                        cache.fed_upto = nl + 1;
                        cache.open_complete = true;
                        // Loop → classify the next (must-be-deeper) line.
                    }
                    None => {
                        if bytes[from..end].contains(&b'\r') {
                            return false;
                        }
                        let Ok(s) = std::str::from_utf8(&bytes[from..end]) else {
                            return false;
                        };
                        if !s.is_empty() {
                            cache.open.append(s);
                        }
                        cache.fed_upto = end;
                        return true; // deepest line still open
                    }
                }
            } else {
                // Expecting the next line — for a pure staircase, exactly one level
                // deeper, `> `×want then prose.
                let want = cache.open_depth + 1;
                if want >= DEEP_QUOTE_MAX_DEPTH {
                    // The baseline render-truncates at this depth — hand back so the
                    // (adversarial) truncation regime stays byte-identical.
                    return false;
                }
                let from = cache.fed_upto;
                let region = &bytes[from..end];
                // First byte that is neither a `>` marker nor a marker space — the
                // start of this line's content, if any has arrived.
                match region.iter().position(|&b| b != b'>' && b != b' ') {
                    None => {
                        // Marker/space bytes only (no content byte yet). While the
                        // trailing markers stay at/under the current depth the
                        // structure is unchanged, so keep rendering `open_depth`; the
                        // instant they reach a DEEPER level (an empty inner blockquote
                        // the full path would already show) hand back — that transient
                        // is rare at real chunk sizes and byte-parity outranks it.
                        let markers = region.iter().filter(|&&b| b == b'>').count();
                        if markers > cache.open_depth {
                            return false;
                        }
                        return true;
                    }
                    Some(rel) => {
                        let cb = region[rel];
                        if cb == b'\n' || cb == b'\r' {
                            // A markers-only line (empty inner blockquote) or CRLF —
                            // subtle; the full path renders it.
                            return false;
                        }
                        // The content must sit exactly after `> `×want, and be prose.
                        if rel != 2 * want {
                            return false; // wrong depth / irregular marker spacing
                        }
                        for j in 0..want {
                            if region[2 * j] != b'>' || region[2 * j + 1] != b' ' {
                                return false;
                            }
                        }
                        if !dq_prose_start(cb) {
                            // `[` alert/ref, `#`, `-`, fence, `<`, digit, … — a block
                            // that is not a plain paragraph.
                            return false;
                        }
                        if cache.open_depth >= 1 {
                            // Settle the now-shallower deepest level: fold it once.
                            let body = dq_collect(&cache.open);
                            if body.is_empty() {
                                return false;
                            }
                            cache.settled.push_str(&cache.wrapper_open);
                            cache.settled.push('\n');
                            cache.settled.push_str(&body);
                            cr(&mut cache.settled);
                        }
                        cache.open_depth = want;
                        cache.open = self.make_nested_parser();
                        cache.open_complete = false;
                        cache.fed_upto = from + rel;
                        // Loop → stream the new deepest line's content.
                    }
                }
            }
        }
    }

    /// O(new bytes) extension of an open deep-quote staircase at the tail (see
    /// [`DeepQuoteCache`]). Returns `None` (dropping the cache → full reparse) on
    /// any BAIL condition.
    fn try_incremental_deep_quote(&mut self) -> Option<Patch> {
        let mut cache = self.deep_quote_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let end = bytes.len();
        // Tail-only check (same as the other caches): nothing but whitespace may
        // sit between the committed boundary and the container's opener.
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        if !self.dq_extend(&mut cache, bytes, end) {
            return None;
        }
        let html = assemble_deep_quote(&cache);
        let block = Block {
            id: cache.id,
            kind: BlockKind::Blockquote(None),
            start: cache.start,
            end,
            html,
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.deep_quote_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// A recursive nested parser configured to reproduce THIS parser's rendering
    /// of stripped inner content (a container's `>`-stripped body, an open list
    /// item's de-indented body): SAME feature flags as the outer EXCEPT
    /// gfm_footnotes (document-global numbering — kept off; a `[^` inner marker
    /// bails the caller). The committed link-ref table is DEEP-cloned (not
    /// Rc-shared) so the nested parser's own `Rc::make_mut` commit-fold keeps
    /// strong_count == 1; it's frozen anyway while the open outer block stalls
    /// `committed_offset`. `force_open_tail` matches the full path, which
    /// propagates the open outer block's `open_tail` to ALL inner sub-blocks.
    ///
    /// `retain_committed_html` is deliberately NOT propagated (it stays at the
    /// `new()` default, on): the container / open-item assemblers read this inner
    /// parser's COMMITTED block html after the commit — the wrapper assembler
    /// folds each block into its [`WrappedBodyCache`] exactly once, but that read
    /// still happens on a LATER append than the commit — so dropping it here
    /// would silently blank the outer block's output. Do not add it below.
    fn make_nested_parser(&self) -> Box<StreamParser> {
        let mut inner = Box::new(StreamParser::new());
        inner.unsafe_html = self.unsafe_html;
        inner.gfm_autolinks = self.gfm_autolinks;
        inner.gfm_alerts = self.gfm_alerts;
        inner.gfm_tagfilter = self.gfm_tagfilter;
        inner.gfm_footnotes = false;
        inner.gfm_math = self.gfm_math;
        inner.dir_auto = self.dir_auto;
        inner.lenient_lists = self.lenient_lists;
        inner.soft_breaks = self.soft_breaks;
        inner.a11y = self.a11y;
        inner.block_data = false;
        inner.component_tags = self.component_tags.clone();
        inner.inline_component_tags = self.inline_component_tags.clone();
        inner.html_sanitize = self.html_sanitize;
        inner.html_allowlist = self.html_allowlist.clone();
        inner.html_drop = self.html_drop.clone();
        inner.block_html = self.block_html;
        inner.allow_schemes = self.allow_schemes.clone();
        inner.committed_refs = Rc::new((*self.committed_refs).clone());
        inner.force_open_tail = true;
        inner.container_depth = self.container_depth + 1;
        inner
    }

    /// The SETTLED twin of [`Self::make_nested_parser`]: an identical nested
    /// parser with `force_open_tail` off, so it applies the ordinary per-block
    /// `open_tail` gate instead of forcing the flag on for every block.
    ///
    /// Every block such a parser COMMITS froze with `open_tail = false`: a
    /// non-`commit_all` pass never commits the final block (the only one the
    /// gate can open), the def-run commit cut only fires for a block that does
    /// NOT abut EOF, and the `commit_all` passes are exactly the blank-ending /
    /// finalizing ones, where the gate is false anyway. It is read only while
    /// the buffer ends blank, so its still-active tail renders settled too —
    /// which is precisely the full rescan's view of an open component's body.
    fn make_nested_parser_settled(&self) -> Box<StreamParser> {
        let mut inner = self.make_nested_parser();
        inner.force_open_tail = false;
        inner
    }

    /// Arm the structured-container cache for an open blockquote / alert at
    /// `start`. Returns `None` (no cache → full reparse) when the nested parser
    /// could not reproduce the inner byte-for-byte: an incomplete first line (the
    /// Blockquote/Alert distinction isn't settled), a wrong block kind, or any of
    /// the feed BAILs (CRLF / lazy continuation / footnote marker with footnotes
    /// on). The first feed processes every line already present; later appends
    /// fold only new bytes.
    fn build_container_block_cache(
        &self,
        start: usize,
        id: u64,
        block_kind: &BlockKind,
        opts: &RenderOpts,
    ) -> Option<ContainerBlockCache> {
        let bytes = self.buffer.as_bytes();
        let end = bytes.len();
        // The Blockquote / Alert split is a first-line decision. A complete first
        // line settles it (carried in `block_kind`); a partial one arms only a
        // Blockquote whose content can no longer become an alert marker — see the
        // per-kind arms. `first_line_end` is the partial line's tail pre-newline.
        let first_nl = bytes[start..end].iter().position(|&b| b == b'\n');
        let first_line_end = first_nl.map_or(end, |nl| start + nl);
        if bytes[start..first_line_end].contains(&b'\r') {
            return None;
        }
        let (kind, wrapper_open, wrapper_close, body_leading_nl, content_start) = match block_kind {
            BlockKind::Blockquote(_) => {
                // Mid-line arm: no newline yet. Bail while the partial first line
                // could still become an alert marker (else Blockquote-vs-Alert is
                // unsettled); once impossible, the nested parser streams the
                // partial inner (incl. block structure) in O(new bytes).
                if first_nl.is_none()
                    && first_line_alert_undecided(strip_blockquote_marker(&bytes[start..end])?)
                {
                    return None;
                }
                let mut w = String::with_capacity(16);
                w.push_str("<blockquote");
                w.push_str(opts.dir());
                w.push('>');
                (
                    ContainerCacheKind::Blockquote,
                    w,
                    String::from("</blockquote>"),
                    true,
                    start,
                )
            }
            BlockKind::Alert { kind: ak, .. } => {
                // An alert's body starts on line 2, so the `[!KIND]` marker line
                // must be complete before this cache can arm.
                first_nl?;
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
                // Alert body starts on line 2 (the `[!KIND]` marker is the title).
                (
                    ContainerCacheKind::Alert(*ak),
                    w,
                    String::from("</div>"),
                    false,
                    first_line_end + 1,
                )
            }
            _ => return None,
        };

        let mut inner = self.make_nested_parser();

        let mut mid_line = false;
        let mut last_fed = None;
        let (delta, fed_outer) = strip_container_delta(
            bytes,
            content_start,
            end,
            &mut mid_line,
            &mut last_fed,
            self.gfm_footnotes,
        )?;
        inner.append(&delta);

        Some(ContainerBlockCache {
            start,
            id,
            kind,
            body: WrappedBodyCache::new(&wrapper_open),
            wrapper_close,
            body_leading_nl,
            inner,
            fed_outer,
            mid_line,
            last_fed,
            footnotes: self.gfm_footnotes,
            committed_nested: Vec::new(),
        })
    }

    /// O(new bytes) extension of an open component block at the tail (see
    /// [`ComponentBlockCache`]). Feeds the appended body bytes RAW to the
    /// recursive nested parser, tracks close/fence/nesting lines exactly like
    /// `scan_component_block`, and reassembles `<Tag …>` + inner + `</Tag>`.
    /// Returns `None` (dropping the cache → full reparse) on any BAIL condition.
    fn try_incremental_component(&mut self) -> Option<Patch> {
        let mut cache = self.component_cache.take()?;
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
        // Parity reconciliation: when the buffer ends with a blank line the full
        // rescan renders the open component — and ALL its inner sub-blocks —
        // with `open_tail = false`, but `cache.inner`'s frozen commits rendered
        // with `force_open_tail = true`. Those appends are served from the
        // SETTLED twin instead (see `ComponentBlockCache::settled`), which froze
        // its own commits with the gate off.
        //
        // BAILING here instead — which drops the cache, forcing the full rescan
        // to re-render the whole open block AND rebuild a fresh nested parser
        // over the entire body — costs O(body) at EVERY blank line. Blank lines
        // are legal component-body content (unlike in a `>` container, where one
        // ends the container), so they arrive once per body paragraph: that bail
        // is what made an open component block quadratic in its body length.
        let ends_blank = self.buffer.ends_with("\n\n") || self.buffer.ends_with("\r\n\r\n");
        let delta_start = cache.fed_upto;
        if component_delta_bails(&bytes[delta_start..end], cache.last_fed, cache.footnotes) {
            return None;
        }
        // Classify the newly-complete lines (and the trailing partial) for the
        // outer close — bails at the exact line `scan_component_block` would
        // terminate the block on.
        component_track_lines(bytes, end, &mut cache, &self.component_tags)?;
        // Feed the delta (the nested parser owns partial-trailing-line
        // speculation). An empty delta still re-renders the open inner tail.
        cache.inner.append(&self.buffer[delta_start..end]);
        cache.fed_upto = end;
        if end > delta_start {
            cache.last_fed = Some(bytes[end - 1]);
        }

        // Catch the settled twin up — creating it on first need — only on the
        // appends that actually read it: O(bytes since the previous blank-ending
        // append), not O(body). Feeding it one gulp rather than per-append is
        // safe because a nested parser's `all_blocks()` is chunk-independent.
        //
        // Each twin assembles through its OWN settled body prefix: the two
        // disagree on `force_open_tail`, so their committed html differs and a
        // shared prefix would emit the losing twin's bytes after a swap.
        //
        // `false`: a component keeps `render_component`'s `!sub.is_empty()`
        // guard, so an empty body stays `<Tag></Tag>` (only `<blockquote>` opens
        // unconditionally).
        let html = if ends_blank {
            let settled = cache.settled.get_or_insert_with(|| self.make_nested_parser_settled());
            if end > cache.settled_fed {
                settled.append(&self.buffer[cache.settled_fed..end]);
                cache.settled_fed = end;
            }
            assemble_wrapped_body(&mut cache.settled_body, false, settled, &cache.wrapper_close)
        } else {
            assemble_wrapped_body(&mut cache.body, false, &cache.inner, &cache.wrapper_close)
        };
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
        self.component_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// Arm the component-block cache for the open `<Tag>` block at `start`.
    /// Returns `None` (no cache → full reparse) when the open-tag line is still
    /// incomplete (more bytes could dissolve the block — `<Chart>x` is no
    /// component — and the attrs aren't frozen), or on any feed BAIL over the
    /// body already present. The first feed processes every body byte present;
    /// later appends fold only new bytes.
    fn build_component_block_cache(
        &self,
        start: usize,
        tag: &str,
        id: u64,
        kind: BlockKind,
    ) -> Option<ComponentBlockCache> {
        let bytes = self.buffer.as_bytes();
        let end = bytes.len();
        let first_nl = bytes[start..end].iter().position(|&b| b == b'\n')?;
        let first_line_end = start + first_nl;
        if bytes[start..first_line_end].contains(&b'\r') {
            return None;
        }
        // Wrapper opener, byte-identical to `render_component`.
        let slice = &self.buffer[start..end];
        let open = slice.trim_start_matches([' ', '\t']);
        let attrs = sanitize_attrs(open, &self.allow_schemes);
        let mut wrapper_open = String::with_capacity(tag.len() + 16);
        wrapper_open.push('<');
        wrapper_open.push_str(tag);
        for (k, v) in &attrs {
            wrapper_open.push(' ');
            wrapper_open.push_str(k);
            wrapper_open.push_str("=\"");
            escape_attr(v, &mut wrapper_open);
            wrapper_open.push('"');
        }
        wrapper_open.push('>');
        let mut wrapper_close = String::with_capacity(tag.len() + 3);
        wrapper_close.push_str("</");
        wrapper_close.push_str(tag);
        wrapper_close.push('>');

        // Recursive nested parser — same construction as the container-block
        // cache (same flags; footnotes off, deep-cloned committed refs,
        // force_open_tail matching `render_component`'s opts propagation to all
        // inner sub-blocks; depth-bounded by the caller). `retain_committed_html`
        // is NOT propagated, for the reason spelled out on `make_nested_parser`:
        // the assembler reads inner COMMITTED html after the commit.
        let mut inner = Box::new(StreamParser::new());
        inner.unsafe_html = self.unsafe_html;
        inner.gfm_autolinks = self.gfm_autolinks;
        inner.gfm_alerts = self.gfm_alerts;
        inner.gfm_tagfilter = self.gfm_tagfilter;
        inner.gfm_footnotes = false;
        inner.gfm_math = self.gfm_math;
        inner.dir_auto = self.dir_auto;
        inner.lenient_lists = self.lenient_lists;
        inner.soft_breaks = self.soft_breaks;
        inner.a11y = self.a11y;
        inner.block_data = false;
        inner.component_tags = self.component_tags.clone();
        inner.inline_component_tags = self.inline_component_tags.clone();
        inner.html_sanitize = self.html_sanitize;
        inner.html_allowlist = self.html_allowlist.clone();
        inner.html_drop = self.html_drop.clone();
        inner.block_html = self.block_html;
        inner.allow_schemes = self.allow_schemes.clone();
        inner.committed_refs = Rc::new((*self.committed_refs).clone());
        inner.force_open_tail = true;
        inner.container_depth = self.container_depth + 1;

        // Body = everything past the open tag's `>` (`component_inner_range`,
        // shared with `render_component`, so the boundary can't drift).
        let (open_end_rel, _) = component_inner_range(slice, tag, false);
        let mut cache = ComponentBlockCache {
            start,
            id,
            tag: tag.to_string(),
            kind,
            body: WrappedBodyCache::new(&wrapper_open),
            settled_body: WrappedBodyCache::new(&wrapper_open),
            wrapper_close,
            inner,
            fed_upto: start + open_end_rel,
            lines_upto: first_line_end + 1,
            depth: 1,
            in_fence: false,
            last_fed: None,
            footnotes: self.gfm_footnotes,
            settled: None,
            settled_fed: start + open_end_rel,
        };
        let delta_start = cache.fed_upto;
        if component_delta_bails(&bytes[delta_start..end], None, cache.footnotes) {
            return None;
        }
        // The scan said `terminated: false`, so this can't hit the close; it
        // establishes the fence/nesting state over the body already present.
        component_track_lines(bytes, end, &mut cache, &self.component_tags)?;
        cache.inner.append(&self.buffer[delta_start..end]);
        cache.fed_upto = end;
        if end > delta_start {
            cache.last_fed = Some(bytes[end - 1]);
        }
        Some(cache)
    }

    /// O(new bytes) extension of a still-growing single-line ATX heading at the
    /// tail (see [`HeadingCache`]). Commits the settled inline prefix once and
    /// re-renders only the short active tail, exactly like
    /// `try_incremental_paragraph`, inside the `<hN>` wrapper. Returns `None`
    /// (dropping the cache → full reparse) when the line completes or the
    /// closing-`#` trim reaches the frozen prefix.
    fn try_incremental_heading(&mut self) -> Option<Patch> {
        let mut cache = self.heading_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        // Tail-only check.
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        // Any newline ends the single-line heading — the full path owns it (and
        // whatever new block follows). Only the new bytes are checked.
        if bytes[cache.scanned_upto..len].iter().any(|&b| matches!(b, b'\n' | b'\r')) {
            return None;
        }
        cache.scanned_upto = len;
        // Mirror `render_heading`'s content window on the growing line. The
        // closing-`#` trim is recomputed per append (O(trailing run)); if it
        // ever reaches back into the frozen prefix, bail.
        let trim_target = heading_trim_target(bytes, cache.content_start, len);
        if trim_target < cache.cut {
            return None;
        }
        // Extend the incremental numbering over the region's new bytes, then
        // share it into the opts (O(1); mirrors the paragraph cache).
        if self.gfm_footnotes {
            cache.fn_nums.extend(&self.buffer, trim_target, &self.committed_footnotes);
        }
        let opts = self.build_inline_opts(&cache.fn_nums);
        // Same settle-then-commit scheme as the paragraph cache.
        let mut active = String::new();
        let boundary_rel =
            render_inline_boundary(&self.buffer[cache.cut..trim_target], &opts, &mut active);
        let new_cut = cache.cut + boundary_rel;
        if new_cut > cache.cut {
            let mut seg = String::new();
            render_inline(&self.buffer[cache.cut..new_cut], &opts, &mut seg);
            resolve_footnote_ids(&seg, &mut cache.fn_occ, &mut cache.committed_inner);
            cache.cut = new_cut;
            active.clear();
            render_inline(&self.buffer[cache.cut..trim_target], &opts, &mut active);
        }
        if self.gfm_footnotes {
            let mut occ = cache.fn_occ.clone();
            let mut resolved = String::with_capacity(active.len());
            resolve_footnote_ids(&active, &mut occ, &mut resolved);
            active = resolved;
        }
        // `<hN dir?>` + inner + `</hN>`, with the same trailing-whitespace trim
        // as `render_heading_inner_trimmed`.
        let mut html = String::with_capacity(
            cache.committed_inner.len() + active.len() + opts.dir().len() + 10,
        );
        html.push_str("<h");
        html.push((b'0' + cache.level) as char);
        html.push_str(opts.dir());
        html.push('>');
        let body_start = html.len();
        html.push_str(&cache.committed_inner);
        html.push_str(&active);
        while html.len() > body_start
            && matches!(html.as_bytes()[html.len() - 1], b' ' | b'\t' | b'\n' | b'\r')
        {
            html.pop();
        }
        html.push_str("</h");
        html.push((b'0' + cache.level) as char);
        html.push('>');
        let block = Block {
            id: cache.id,
            kind: BlockKind::Heading { level: cache.level, rich: None },
            start: cache.start,
            end: len,
            html,
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.heading_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// O(new bytes) extension of a still-growing thematic-break line at the tail
    /// (see [`RuleCache`]). Validates that the appended bytes keep the line a
    /// same-char break and re-emits the constant `<hr>` block; anything else —
    /// including the completing newline — returns `None` (full reparse).
    fn try_incremental_rule(&mut self) -> Option<Patch> {
        let mut cache = self.rule_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        // Tail-only check.
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        // `scan_hr`: a break line holds only its rule char, spaces, and tabs.
        if bytes[cache.scanned_upto..len]
            .iter()
            .any(|&b| b != cache.ch && b != b' ' && b != b'\t')
        {
            return None;
        }
        cache.scanned_upto = len;
        let block = Block {
            id: cache.id,
            kind: BlockKind::Rule,
            start: cache.start,
            end: len,
            html: String::from("<hr>"),
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.rule_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// O(new bytes) extension of a code fence whose opener line (info string) is
    /// still growing (see [`FenceInfoCache`]). The block HTML and kind are
    /// frozen; each append just validates the new bytes and re-emits. Returns
    /// `None` (full reparse — which arms the real [`FenceCache`]) when the
    /// opener line completes, or when a backtick lands in a backtick fence's
    /// info (the scanner then rejects the whole fence, §4.5).
    fn try_incremental_fence_info(&mut self) -> Option<Patch> {
        let mut cache = self.fence_info_cache.take()?;
        let bytes = self.buffer.as_bytes();
        let len = bytes.len();
        // Tail-only check.
        if cache.start < self.committed_offset
            || bytes[self.committed_offset..cache.start]
                .iter()
                .any(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        if bytes[cache.scanned_upto..len]
            .iter()
            .any(|&b| matches!(b, b'\n' | b'\r') || (cache.backtick && b == b'`'))
        {
            return None;
        }
        cache.scanned_upto = len;
        // Everything re-emitted here is FROZEN — no part of the growing opener
        // line is re-derived per append (that would be quadratic in the line
        // length; `fence_opener_line_growth_is_wall_linear` gates it). The kind's
        // `meta` is `None` for as long as this cache is armed, and provably so:
        // `classify` only fills `meta` once the opener line has its `\n` or the
        // stream is finalizing, and this cache is armed only while the line holds
        // NO newline (it bails on one) and never during finalize.
        let block = Block {
            id: cache.id,
            kind: cache.kind.clone(),
            start: cache.start,
            end: len,
            html: cache.html.clone(),
            open: true,
            speculative: true,
        };
        self.active_blocks = vec![block.clone()];
        self.fence_info_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// O(new bytes) extension of a long open list at the tail — flat OR nested.
    /// Each item is tracked as the FULL multi-line source span between two
    /// sibling markers; a sibling marker CLOSES the current item (its body, with
    /// any nested sub-lists, is now settled) and folds it into `cached_prefix`
    /// once via `item_body` + `render_item_body` (the same engine the full path
    /// uses, so byte-parity is structural). The open (last) item and any trailing
    /// partial marker render speculatively each append. A blank line between two
    /// siblings flips the list loose (§5.3) with a one-time O(items so far)
    /// rebuild — sticky once set. The cache bails on an interior blank (directly
    /// loose), a foreign-family / shallower-than-content marker, `\r`, or a
    /// document-global scoping DEFINITION line (`]:` — link-ref or footnote
    /// def); a plain footnote REF `[^x]` streams through the placeholder/
    /// resolve machinery instead.
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

        // `open_tail` mirrors the full path's per-block gate for the (final,
        // abuts-EOF) open list: force it on inside a nested container-block
        // parser; otherwise speculate the open tail unless the buffer's LAST
        // LINE is whitespace-only — a trailing blank line (complete, spaces/tabs
        // included) settles the list, and a whitespace-only dangling partial
        // means the list's raw range stops at the previous line and no longer
        // abuts EOF (`raw.range.end == buffer.len()` fails), which the full path
        // also renders settled. A wrong value would mis-render an incomplete
        // `[x](` / `` `code `` / `$math` at the very end of the open item.
        let open_tail = self.force_open_tail || {
            let last_line_start =
                bytes[..end].iter().rposition(|&b| b == b'\n').map_or(0, |p| p + 1);
            // The dangling partial when there is one, else the last complete
            // line (its `\n` included; the cache always holds the marker line,
            // so the probe is never empty).
            let probe = if last_line_start < end {
                &bytes[last_line_start..end]
            } else {
                let prev = bytes[..last_line_start.saturating_sub(1)]
                    .iter()
                    .rposition(|&b| b == b'\n')
                    .map_or(0, |p| p + 1);
                &bytes[prev..last_line_start]
            };
            probe.iter().any(|&b| !matches!(b, b' ' | b'\t' | b'\n'))
        };
        let block_data = self.block_data;

        // The open list's OWN items may carry the first reference to a footnote
        // label, so the numbering pre-pass covers the whole list region (mirrors
        // the full path) — extended incrementally over only the region's new
        // bytes. Footnote REFS then stream through the cache's placeholder/
        // resolve machinery (`fn_occ`); only DEF-shaped lines bail (below).
        if self.gfm_footnotes {
            cache.fn_nums.extend(&self.buffer, end, &self.committed_footnotes);
        }
        let mut opts = self.build_inline_opts(&cache.fn_nums);
        opts.open_tail = open_tail;

        // Committing folds advance `cache.fn_occ` in place as the overlay of a
        // layered resolve (empty base); speculative folds use `fn_occ` as the
        // never-mutated base instead (see `fold_item_body`).
        let fn_base_empty: HashMap<String, usize> = HashMap::new();

        // Sibling test (the family half; the indent half is inline so it can use
        // the live `cur_ci`). Copy captures so the loop can hold `&mut cache`.
        let (c_ordered, c_delim, c_edge) = (cache.ordered, cache.delim, cache.edge);
        let same_family = |m: &MarkerScan| m.ordered == c_ordered && m.delim == c_delim;
        // A line/slice that forces the full reparse: `\r` (CRLF) or a
        // document-global scoping marker the region-scoped cache opts can't get
        // right — a link-ref or footnote DEFINITION (both carry `]:`; a def
        // only parses with the `:` on the opener's own line, so the check is
        // line-local). A plain footnote REF `[^x]` has no `]:` and stays on
        // the fast path.
        let line_bails = |s: &[u8]| -> bool {
            s.contains(&b'\r') || s.windows(2).any(|w| w == b"]:")
        };

        // Walk every COMPLETE line, classifying it exactly as the full path's
        // `render_list` split + `scan`'s list-extent rule do: a same-family
        // shallower marker is a SIBLING (closes the open item, opens the next, and
        // updates `cur_ci`); content at/below `cur_ci` NESTS into the open item's
        // body; a plain shallow line with no preceding blank is a lazy
        // continuation (absorbed). Anything else — a different-family marker (a new
        // list `scan` splits), a shallow block-interrupt (heading/fence/quote/…
        // that ENDS the list), an interior blank (directly loose), or a shallow
        // line after a blank (a new outside-the-list paragraph) — bails.
        let mut pos = cache.lines_upto;
        while pos < end {
            let r = match bytes[pos..end].iter().position(|&b| b == b'\n') {
                None => break, // trailing partial — handled below
                Some(r) => r,
            };
            let content_end = pos + r;
            let next = content_end + 1;
            let line = &bytes[pos..content_end];
            if line_bails(line) {
                return None;
            }
            if line.iter().all(|&b| matches!(b, b' ' | b'\t')) {
                cache.prev_blank = true;
                cache.lines_upto = next;
                pos = next;
                continue;
            }
            let ind = indent_cols(line);
            if ind >= cache.cur_ci {
                // Nested content of the open item — `scan` keeps it; `item_body` +
                // `render_item_body` render it recursively. A preceding blank is
                // an INTERIOR blank: an EMPTY item cannot gain content across a
                // blank (§5.2, `scan_list`'s `cur_empty` — the list ends there,
                // bail); otherwise the item may now be "directly loose" (§5.3) —
                // mark it for the precise inter-block gap test that runs before
                // this append renders.
                if cache.prev_blank {
                    if cache.item_empty {
                        return None;
                    }
                    cache.item_blank = true;
                    cache.prev_blank = false;
                }
                cache.item_empty = false;
            } else if let Some(m) = scan_marker(line, opts.scan_ctx()) {
                if ind <= c_edge + 3 && same_family(&m) {
                    // SIBLING: the current open item [open_item_start..pos] is now
                    // complete. A blank between siblings — or a §5.3 "directly
                    // loose" interior gap in the item that just closed — makes the
                    // whole list loose: rebuild the already-closed items once,
                    // then fold this one.
                    if pos > cache.open_item_start {
                        if !cache.loose
                            && (cache.prev_blank
                                || (cache.item_blank
                                    && item_directly_loose(
                                        &bytes[cache.open_item_start..pos],
                                        opts.scan_ctx(),
                                    )))
                        {
                            rebuild_loose(&mut cache, bytes, &opts)?;
                        }
                        let s = cache.open_item_start;
                        fold_item_body(
                            &bytes[s..pos],
                            s,
                            cache.loose,
                            &opts,
                            &mut cache.cached_prefix,
                            Some(&mut cache.item_html),
                            &fn_base_empty,
                            &mut cache.fn_occ,
                        )?;
                        cache.items.push((s, pos));
                        cache.open_item_start = pos;
                        cache.item_blank = false;
                        cache.open_stream = None;
                        cache.stream_disabled = false;
                    }
                    // else: the first item's own marker line — nothing to close.
                    cache.cur_ci = m.content_indent;
                    cache.item_empty =
                        line.get(m.content_byte).map_or(true, |&b| matches!(b, b'\n' | b'\r'));
                    cache.prev_blank = false;
                } else {
                    // A different-family marker (a NEW list `scan` splits) or one
                    // indented oddly between `edge + 3` and the content column (not
                    // a clean sibling) — the cache can't reproduce either; bail.
                    return None;
                }
            } else if container_inner_breaks_paragraph(line, opts.scan_ctx()) {
                // A shallow NON-marker line that starts/implies a block (heading,
                // fence, quote, thematic break, HTML, setext underline, table
                // delimiter, …) ENDS the list — `scan` stops here; bail.
                return None;
            } else if cache.prev_blank {
                // A shallow plain-text line after a blank is a NEW paragraph
                // outside the list (a blank breaks lazy continuation) — bail.
                return None;
            } else {
                // A shallow plain-text lazy continuation of the open item's
                // paragraph (no preceding blank) — absorbed into its body.
                cache.item_empty = false;
            }
            cache.lines_upto = next;
            pos = next;
        }

        // The trailing partial line (no `\n` yet): classify it like a complete line
        // against the CURRENT item's content column, but never commit it — the open
        // region is re-rendered fresh each append. A same-family shallower marker
        // opens a new speculative sibling (settling loose when a blank precedes it);
        // everything else either continues the open item's body or bails.
        let partial = &bytes[cache.lines_upto..end];
        if line_bails(partial) {
            return None;
        }
        let partial_nonblank = !partial.iter().all(|&b| matches!(b, b' ' | b'\t' | b'\n'));
        let mut partial_is_sibling = false;
        if partial_nonblank {
            let p_ind = indent_cols(partial);
            if p_ind >= cache.cur_ci {
                // Nested partial → part of the open item's body. After a blank:
                // an empty item can't resume (§5.2 — bail); otherwise mark the
                // interior blank for the §5.3 gap test. `prev_blank`/`item_empty`
                // stay untouched — the walk re-classifies this line when it
                // completes.
                if cache.prev_blank {
                    if cache.item_empty {
                        return None;
                    }
                    cache.item_blank = true;
                }
            } else if let Some(m) = scan_marker(partial, opts.scan_ctx()) {
                if p_ind <= c_edge + 3 && same_family(&m) {
                    partial_is_sibling = true;
                } else {
                    return None; // new-list / odd-indent marker
                }
            } else if container_inner_breaks_paragraph(partial, opts.scan_ctx()) {
                return None; // shallow block-interrupt ends the list
            } else if cache.prev_blank {
                return None; // shallow plain text after a blank ends the list
            }
            // else: shallow lazy continuation → part of the open item.
        }

        let mut partial_html = String::new();
        let mut partial_item: Vec<Rc<ListItemData>> = Vec::new();
        if partial_is_sibling {
            // The open item [open_item_start..lines_upto] is complete; the partial
            // starts a new sibling. A preceding blank — or a §5.3 interior gap in
            // the item being closed — settles the list loose (matching `scan`,
            // which already classifies the partial as a bullet).
            if !cache.loose
                && (cache.prev_blank
                    || (cache.item_blank
                        && item_directly_loose(
                            &bytes[cache.open_item_start..cache.lines_upto],
                            opts.scan_ctx(),
                        )))
            {
                rebuild_loose(&mut cache, bytes, &opts)?;
            }
            // Feature-gated work accounting: this speculative re-render bypasses
            // the slow-path scan counter, so count it for the scaling gate.
            #[cfg(feature = "perf_counters")]
            crate::perf::add_scan(end - cache.open_item_start);
            // Speculative folds resolve from a discarded OVERLAY over the
            // frozen-prefix occurrence map (never clones the growing map).
            let mut partial_over: HashMap<String, usize> = HashMap::new();
            fold_item_body(
                &bytes[cache.open_item_start..cache.lines_upto],
                cache.open_item_start,
                cache.loose,
                &opts,
                &mut partial_html,
                Some(&mut partial_item),
                &cache.fn_occ,
                &mut partial_over,
            )?;
            fold_item_body(
                partial,
                cache.lines_upto,
                cache.loose,
                &opts,
                &mut partial_html,
                Some(&mut partial_item),
                &cache.fn_occ,
                &mut partial_over,
            )?;
        } else {
            // The partial (continuation, nested marker, trailing blanks, or
            // nothing) belongs to the open item. Keep the item's nested stream
            // fed (armed once the body outgrows the fold), settle the item's own
            // §5.3 looseness, then render the open region — via the stream in
            // O(new bytes) when armed, else the one-shot fold.
            self.update_open_item_stream(&mut cache, end);
            if !cache.loose && cache.item_blank {
                let directly = match cache.open_stream.as_mut() {
                    Some(st) => open_item_gap_blank(st),
                    None => item_directly_loose(
                        &bytes[cache.open_item_start..end],
                        opts.scan_ctx(),
                    ),
                };
                if directly {
                    rebuild_loose(&mut cache, bytes, &opts)?;
                }
            }
            // The stream serves the speculative open-tail view (its committed
            // inner renders froze with `force_open_tail`) — and the settled
            // (`open_tail == false`, trailing blank) view too while the body is
            // not `open_tail`-sensitive: with no construct that could
            // speculate, both variants render byte-identically. Otherwise a
            // settled append folds with the settled opts — same as before.
            let can_stream = open_tail
                || cache
                    .open_stream
                    .as_mut()
                    .is_some_and(|st| !open_item_ot_sensitive(st, cache.loose));
            let assembled = can_stream
                && match cache.open_stream.as_mut() {
                    Some(st) => {
                        assemble_open_item(st, cache.loose, &opts, &mut partial_html);
                        true
                    }
                    None => false,
                };
            if !assembled {
                // Speculative one-shot fold of the whole open region — O(open
                // item) for this append. Feature-gated work accounting so the
                // scaling gate can see this class of re-render work (it bypasses
                // the slow-path scan counter — the wall-only half of the
                // open-item cliff). Resolves from a discarded OVERLAY over the
                // frozen-prefix occurrence map (does NOT advance it, and never
                // clones the growing map).
                #[cfg(feature = "perf_counters")]
                crate::perf::add_scan(end - cache.open_item_start);
                let mut partial_over: HashMap<String, usize> = HashMap::new();
                fold_item_body(
                    &bytes[cache.open_item_start..end],
                    cache.open_item_start,
                    cache.loose,
                    &opts,
                    &mut partial_html,
                    Some(&mut partial_item),
                    &cache.fn_occ,
                    &mut partial_over,
                )?;
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
        // items + the speculative open/partial item(s)) on the active block so the
        // keyed renderer reuses unchanged item nodes mid-stream. The committed
        // entries clone as Rc refcount bumps. Off ⇒ empty (omitted on the wire,
        // byte-identical).
        let items: Vec<Rc<ListItemData>> = if block_data {
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
                start: if block_data { Some(cache.start_num) } else { None },
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
        Some(Patch { newly_committed: Vec::new(), active: vec![block], active_deltas: Vec::new() })
    }

    /// Keep the [`OpenItemStream`] in sync with the cache's OPEN item: drop a
    /// stream that belongs to a closed item, arm one lazily when the body has
    /// outgrown the per-append fold, and feed the newly-arrived body delta.
    /// Any condition the stream can't reproduce byte-for-byte (lazy line, arm
    /// failure) disables it for the rest of this item — the fold owns it then.
    fn update_open_item_stream(&self, cache: &mut ListCache, end: usize) {
        // The nested `ListItemData`/`items` channel isn't fed by the stream —
        // let the fold own block_data lists (correct, just not O(new bytes)).
        if cache.stream_disabled || self.block_data {
            return;
        }
        if let Some(st) = &cache.open_stream {
            if st.item_start != cache.open_item_start {
                cache.open_stream = None;
            }
        }
        if let Some(st) = cache.open_stream.as_mut() {
            // Document-global scoping bail (mirrors `strip_container_delta`'s
            // over-conservative substring match): the nested parser runs with
            // footnotes OFF, so a footnote ref reaching the open item's body
            // must hand the item back to the fold (which renders refs as
            // placeholder tokens). The 1-byte overlap catches a `[^` marker
            // split across two feeds.
            if self.gfm_footnotes
                && line_contains(
                    &self.buffer.as_bytes()[st.fed_outer.saturating_sub(1)..end],
                    b"[^",
                )
            {
                cache.open_stream = None;
                cache.stream_disabled = true;
                return;
            }
            if feed_open_item(st, self.buffer.as_bytes(), end).is_none() {
                cache.open_stream = None;
                cache.stream_disabled = true;
            }
            return;
        }
        if end - cache.open_item_start < OPEN_ITEM_STREAM_MIN {
            return;
        }
        if self.container_depth >= MAX_CONTAINER_DEPTH {
            cache.stream_disabled = true;
            return;
        }
        match self.arm_open_item_stream(cache, end) {
            Some(st) => cache.open_stream = Some(st),
            None => cache.stream_disabled = true,
        }
    }

    /// Build the open item's nested stream and feed the body already present.
    /// `None` ⇒ the item can't stream (incomplete first line, lazy body line …)
    /// — the caller disables the stream for this item.
    fn arm_open_item_stream(
        &self,
        cache: &ListCache,
        end: usize,
    ) -> Option<Box<OpenItemStream>> {
        let bytes = self.buffer.as_bytes();
        let start = cache.open_item_start;
        // Scoping bail, arm-time flavor: the whole body present so far feeds at
        // arm, so a `[^` anywhere in it keeps the fold owning the item (the
        // nested parser runs with footnotes off — see `update_open_item_stream`).
        if self.gfm_footnotes && line_contains(&bytes[start..end], b"[^") {
            return None;
        }
        // A complete first line settles the marker + content byte.
        let nl = bytes[start..end].iter().position(|&b| b == b'\n')?;
        let first_line_end = start + nl + 1;
        let ctx = ScanCtx {
            math: self.gfm_math,
            component_tags: &self.component_tags,
            inline_component_tags: &self.inline_component_tags,
            lenient_lists: self.lenient_lists,
        };
        let m = scan_marker(&bytes[start..first_line_end], ctx)?;
        if m.content_indent != cache.cur_ci {
            return None;
        }
        // GFM task-list prefix on the body's first 4 bytes — `render_item_body`
        // strips it before scanning, so the feed skips it and the assembly
        // re-emits the checkbox. (Any `\n` inside the window disqualifies it,
        // so the complete first line always decides.)
        let content = &bytes[start + m.content_byte..first_line_end];
        let mut task = None;
        // `m.content_overflow > 0` means the body opens with re-materialized
        // spaces (a tab-padded marker), so `render_item_body` sees whitespace at
        // byte 0 and finds no checkbox — match that, or the two paths diverge.
        if m.content_overflow == 0
            && content.len() >= 4
            && content[0] == b'[' && content[2] == b']' && content[3] == b' ' {
            match content[1] {
                b' ' => task = Some(false),
                b'x' | b'X' => task = Some(true),
                _ => {}
            }
        }
        let mut st = Box::new(OpenItemStream {
            inner: self.make_nested_parser(),
            item_start: start,
            ci: cache.cur_ci,
            task,
            // The marker line's remainder feeds as a MID-LINE continuation (its
            // "indent" is the marker itself — already consumed), exactly
            // `item_body`'s first-line handling.
            fed_outer: start + m.content_byte + if task.is_some() { 4 } else { 0 },
            // The marker's unrepresentable columns are not in the buffer, so
            // they seed the held-whitespace run — which flushes ahead of the
            // first content bytes, exactly `item_body`'s leading spaces.
            held_ws: " ".repeat(m.content_overflow),
            mid_line: true,
            para_start: usize::MAX,
            para_cut: 0,
            para_settled: String::new(),
            tight_memo: HashMap::new(),
            gap_pairs_done: 0,
            sens_committed: false,
            sens_scanned: 0,
        });
        feed_open_item(&mut st, bytes, end)?;
        Some(st)
    }
}

/// Tight→loose one-time rebuild. Re-renders `cached_prefix` from the source
/// spans in `cache.items`, each item's body now rendered loose (`<p>…</p>`-wrapped
/// paragraphs). Sets `cache.loose`. O(items so far) — paid once per list, never
/// again. Spans started with a marker validated by `scan_marker` when they were
/// closed; the only way `item_body` fails here is invalid UTF-8 before the content
/// byte (impossible — markers are ASCII), so `None` is the bail-for-safety path.
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
    // Committing replay: `fn_occ` advances in place as the overlay (empty base).
    let fn_base_empty: HashMap<String, usize> = HashMap::new();
    // Borrow `items` separately so `cached_prefix`/`item_html` can be mutated.
    let spans = std::mem::take(&mut cache.items);
    for &(s, e) in &spans {
        if fold_item_body(
            &bytes[s..e],
            s,
            true,
            opts,
            &mut cache.cached_prefix,
            Some(&mut cache.item_html),
            &fn_base_empty,
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

/// Extract the inner-markdown DELTA to feed a [`ContainerBlockCache`]'s nested
/// parser from `bytes[from..end]`, stripping the per-line `>` prefix
/// (≤3 spaces, `>`, one optional space) exactly like `render::blockquote_inner`.
///
/// Linear and rollback-free: it advances over whole inner lines, and for the
/// trailing PARTIAL line feeds the post-prefix content RAW (no `\n`) so the
/// nested parser speculates it — which is why `mid_line` (we're continuing a
/// partial inner line whose prefix was already consumed) feeds raw with no new
/// prefix. It WAITS (stops, advancing `*pos` no further) when a trailing partial
/// line hasn't revealed enough of its prefix yet (only spaces so far, or a bare
/// `>` whose optional space is undecided) — those few bytes carry no visible
/// output, so rendering with what's committed stays byte-faithful.
///
/// Returns `None` (the caller BAILS to the full reparse) on a `\r` (CRLF), a
/// non-`>` line (a lazy continuation the nested parser can't see verbatim), or —
/// only when `footnotes` is on — a `[^` marker (footnote numbering is
/// document-global and the nested parser runs with footnotes off). Link-ref
/// definitions do NOT bail: the nested parser parses them natively (its own
/// def-run commit + committed-ref fold), and the outer parser re-derives the
/// document-global table from source at every commit/finalize boundary (the
/// container only ever commits through a full reparse), so post-quote text
/// resolves quote-hosted defs exactly as one-shot does. `last_fed` (the previous
/// feed's last byte) catches a `[^` split across two feeds. On success returns
/// the delta plus the new consumed offset.
fn strip_container_delta(
    bytes: &[u8],
    from: usize,
    end: usize,
    mid_line: &mut bool,
    last_fed: &mut Option<u8>,
    footnotes: bool,
) -> Option<(String, usize)> {
    let nl_at = |start: usize| bytes[start..end].iter().position(|&b| b == b'\n').map(|r| start + r);
    let mut delta = String::new();
    let mut pos = from;
    loop {
        if pos >= end {
            break;
        }
        if *mid_line {
            // Continuation of an inner line whose `>` prefix was already consumed.
            match nl_at(pos) {
                Some(ce) => {
                    if bytes[pos..ce].contains(&b'\r') {
                        return None;
                    }
                    delta.push_str(std::str::from_utf8(&bytes[pos..ce]).ok()?);
                    delta.push('\n');
                    pos = ce + 1;
                    *mid_line = false;
                }
                None => {
                    if bytes[pos..end].contains(&b'\r') {
                        return None;
                    }
                    delta.push_str(std::str::from_utf8(&bytes[pos..end]).ok()?);
                    pos = end;
                    break; // mid_line stays true
                }
            }
        } else {
            // Strip the line prefix: ≤3 spaces, `>`, one optional space.
            let mut i = pos;
            let mut sp = 0;
            while i < end && bytes[i] == b' ' && sp < 3 {
                i += 1;
                sp += 1;
            }
            if i >= end {
                break; // only spaces so far — wait for the rest of the prefix
            }
            if bytes[i] != b'>' {
                return None; // non-`>` line: lazy continuation / end of quote
            }
            i += 1;
            if i >= end {
                break; // bare `>` at EOF — wait to resolve the optional space
            }
            // §2.2 twin of `quote_tab_content`: the marker's optional space is
            // one COLUMN, so a tab here is only partly consumed and the whole
            // whitespace run re-materializes as spaces (example 6).
            let mut lead_spaces = 0usize;
            if bytes[i] == b'\t' {
                let mut col = sp + 1;
                while i < end {
                    match bytes[i] {
                        b' ' => col += 1,
                        b'\t' => col += 4 - (col % 4),
                        _ => break,
                    }
                    i += 1;
                }
                if i >= end {
                    break; // run still growing — its width isn't settled yet
                }
                lead_spaces = col - (sp + 2);
            } else if bytes[i] == b' ' {
                i += 1;
            }
            let cs = i;
            match nl_at(cs) {
                Some(ce) => {
                    if bytes[cs..ce].contains(&b'\r') {
                        return None;
                    }
                    for _ in 0..lead_spaces {
                        delta.push(' ');
                    }
                    delta.push_str(std::str::from_utf8(&bytes[cs..ce]).ok()?);
                    delta.push('\n');
                    pos = ce + 1;
                }
                None => {
                    if bytes[cs..end].contains(&b'\r') {
                        return None;
                    }
                    for _ in 0..lead_spaces {
                        delta.push(' ');
                    }
                    delta.push_str(std::str::from_utf8(&bytes[cs..end]).ok()?);
                    pos = end;
                    *mid_line = true;
                    break;
                }
            }
        }
    }
    // Footnote scoping bail (over-conservative substring match; a 1-byte overlap
    // with the previous feed catches a `[^` marker split across the feed boundary).
    let db = delta.as_bytes();
    if footnotes && *last_fed == Some(b'[') && db.first() == Some(&b'^') {
        return None;
    }
    if footnotes && delta.contains("[^") {
        return None;
    }
    if let Some(&b) = db.last() {
        *last_fed = Some(b);
    }
    Some((delta, pos))
}

/// Assemble a [`ContainerBlockCache`]'s active-block HTML: wrapper opener, the
/// `cr()`-separated inner sub-block fragments (each nested block's `.html` is
/// exactly `render_block`'s output — usually with no trailing `\n`, though a raw
/// HTML block carries its own, which `cr()` must not double), and the wrapper
/// closer — byte-identical to `render_blockquote` / `render_alert`.
fn assemble_container_block(cache: &mut ContainerBlockCache) -> String {
    assemble_wrapped_body(
        &mut cache.body,
        cache.body_leading_nl,
        &cache.inner,
        &cache.wrapper_close,
    )
}

/// The `\n`-joined HTML of a [`DeepQuoteCache`]'s deepest-line parser — one
/// `render_block` fragment per open sub-block, matching how `render_blockquote`
/// joins its body (a well-formed prose line is a single paragraph block).
fn dq_collect(open: &StreamParser) -> String {
    let mut s = String::new();
    for b in open.all_blocks() {
        cr(&mut s);
        s.push_str(&b.html);
    }
    s
}

/// Assemble a [`DeepQuoteCache`]'s active-block HTML: the frozen settled-level
/// openers, then the deepest (open) level's `<blockquote>` + prose paragraph, then
/// one `</blockquote>` per level joined by `\n` — byte-identical to the nested
/// `render_blockquote` chain, which opens its body with an unconditional `\n`
/// (an empty quote is still `<blockquote>\n</blockquote>`) and `cr()`s after
/// every sub-block before the closer (so the deepest paragraph's trailing `\n`
/// is present whether or not its line is complete).
fn assemble_deep_quote(cache: &DeepQuoteCache) -> String {
    let body = dq_collect(&cache.open);
    let mut html = String::with_capacity(cache.settled.len() + body.len() + 32 + cache.open_depth * 14);
    html.push_str(&cache.settled);
    html.push_str(&cache.wrapper_open);
    html.push('\n');
    if !body.is_empty() {
        html.push_str(&body);
        cr(&mut html);
    }
    for i in 0..cache.open_depth {
        if i > 0 {
            html.push('\n');
        }
        html.push_str("</blockquote>");
    }
    html
}

/// Assemble a wrapper + nested-parser body: wrapper opener, the body-leading
/// `cr()`, the inner sub-blocks each followed by `cr()`, and the wrapper closer.
///
/// `lead_when_empty` picks which container's empty-body rule applies:
/// `render_blockquote` opens with an UNCONDITIONAL `\n` (`<blockquote>\n</blockquote>`
/// for an empty quote), while `render_component` keeps its `!sub.is_empty()`
/// guard (`<Tag></Tag>`). An alert passes either — its opener is the title line,
/// which already ends in `\n`, so the `cr()` is a no-op.
///
/// `body` holds the SETTLED prefix — the opener plus every already-committed
/// sub-block (see [`WrappedBodyCache`]). Each newly committed sub-block folds in
/// exactly once here, so the per-append cost is the new bytes plus the still-open
/// tail, not a walk over every sub-block rendered so far. The emitted bytes are
/// unchanged: the fold applies the same `cr()` in the same order the from-scratch
/// rebuild did, and the body-leading `cr()` still keys off whether ANY block
/// exists (`body.blocks` covers the committed ones, `active_blocks` the rest).
fn assemble_wrapped_body(
    body: &mut WrappedBodyCache,
    lead_when_empty: bool,
    inner: &StreamParser,
    wrapper_close: &str,
) -> String {
    // Committed sub-blocks are append-only and frozen at commit, so this only
    // ever grows — each block's html is read (and copied) exactly once.
    for b in &inner.committed_blocks[body.blocks..] {
        if body.blocks == 0 {
            cr(&mut body.settled);
        }
        body.settled.push_str(&b.html);
        cr(&mut body.settled);
        body.blocks += 1;
    }
    let active = &inner.active_blocks;
    let tail_len: usize = active.iter().map(|b| b.html.len() + 1).sum();
    let mut html =
        String::with_capacity(body.settled.len() + 1 + tail_len + wrapper_close.len());
    html.push_str(&body.settled);
    if body.blocks == 0 && (lead_when_empty || !active.is_empty()) {
        cr(&mut html);
    }
    for b in active {
        html.push_str(&b.html);
        cr(&mut html);
    }
    html.push_str(wrapper_close);
    html
}

/// Scoping bail for RAW-fed component-body bytes, mirroring the tail checks in
/// `strip_container_delta`: a `\r` (CRLF), a footnote `[^` (when the OUTER
/// parser has footnotes on — the nested parser runs with them off), or a
/// link-ref `]:` (document-global resolution). `last_fed` gives a 1-byte
/// overlap so a marker split across two feeds is still caught.
fn component_delta_bails(delta: &[u8], last_fed: Option<u8>, footnotes: bool) -> bool {
    if delta.contains(&b'\r') {
        return true;
    }
    if (footnotes && last_fed == Some(b'[') && delta.first() == Some(&b'^'))
        || (last_fed == Some(b']') && delta.first() == Some(&b':'))
    {
        return true;
    }
    (footnotes && delta.windows(2).any(|w| w == b"[^")) || delta.windows(2).any(|w| w == b"]:")
}

/// Advance a [`ComponentBlockCache`]'s line classification over the newly
/// complete body lines, mirroring `scan_component_block` exactly: a ```/~~~
/// line toggles fence state; outside a fence, a clean `</Tag>` line closes one
/// nesting level and a same-tag open line adds one. Returns `None` (the caller
/// BAILS to the full reparse, which terminates the block) when the OUTER close
/// lands — including on the trailing partial line, since the scan terminates on
/// a close line even without its newline.
fn component_track_lines(
    bytes: &[u8],
    end: usize,
    cache: &mut ComponentBlockCache,
    tags: &[Box<str>],
) -> Option<()> {
    let name = cache.tag.as_bytes();
    let mut pos = cache.lines_upto;
    while pos < end {
        let next = match bytes[pos..end].iter().position(|&b| b == b'\n') {
            Some(r) => pos + r + 1,
            None => break,
        };
        let (_ind, lb) = strip_indent(&bytes[pos..next], 3);
        if lb.starts_with(b"```") || lb.starts_with(b"~~~") {
            cache.in_fence = !cache.in_fence;
        } else if !cache.in_fence {
            if is_clean_close_tag(lb, name) {
                cache.depth -= 1;
                if cache.depth == 0 {
                    return None;
                }
            } else if let Some((n2, sc2, _)) = component_open_tag(lb, tags) {
                if n2 == name && !sc2 {
                    cache.depth += 1;
                }
            }
        }
        cache.lines_upto = next;
        pos = next;
    }
    let partial = &bytes[cache.lines_upto..end];
    if !partial.is_empty() && !cache.in_fence && cache.depth == 1 {
        let (_ind, lb) = strip_indent(partial, 3);
        if is_clean_close_tag(lb, name) {
            return None;
        }
    }
    Some(())
}

/// The end of an open (single-line) ATX heading's content window: strip
/// trailing spaces from `bytes[i..len]`, then an optional closing-`#` run per
/// `render_heading` — all-`#` content strips entirely; otherwise the run (plus
/// its preceding space/tab separator) strips only when so separated.
fn heading_trim_target(bytes: &[u8], i: usize, len: usize) -> usize {
    let mut end = len;
    while end > i && bytes[end - 1] == b' ' {
        end -= 1;
    }
    let mut tail = end;
    while tail > i && bytes[tail - 1] == b'#' {
        tail -= 1;
    }
    if tail == i {
        i
    } else if tail < end && (bytes[tail - 1] == b' ' || bytes[tail - 1] == b'\t') {
        let mut t = tail - 1;
        while t > i && (bytes[t - 1] == b' ' || bytes[t - 1] == b'\t') {
            t -= 1;
        }
        t
    } else {
        end
    }
}

/// Arm the heading cache for the still-growing single-line ATX heading at
/// `start` (see [`HeadingCache`]), rendering its initial settled prefix once.
/// `None` when the line is already complete, nothing is committable yet, or
/// `block_data` is on (the `rich` text+slug channel re-derives from the whole
/// content — the full path owns it).
fn build_heading_cache(
    buffer: &str,
    start: usize,
    level: u8,
    id: u64,
    opts: &RenderOpts,
    fn_base: &HashMap<String, usize>,
    fn_next: usize,
) -> Option<HeadingCache> {
    if opts.block_data {
        return None;
    }
    let bytes = buffer.as_bytes();
    let len = bytes.len();
    // Only a still-growing line arms — a completed heading can't grow (new
    // bytes would belong to the NEXT block).
    if bytes[start..len].iter().any(|&b| matches!(b, b'\n' | b'\r')) {
        return None;
    }
    // Content start, mirroring `render_heading`: spaces, `#`s, then ws. Frozen
    // once content exists (the line only grows at its end).
    let mut i = start;
    while i < len && bytes[i] == b' ' {
        i += 1;
    }
    while i < len && bytes[i] == b'#' {
        i += 1;
    }
    while i < len && (bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    let trim_target = heading_trim_target(bytes, i, len);
    if trim_target <= i {
        return None;
    }
    let mut tmp = String::new();
    let cut = i + render_inline_boundary(&buffer[i..trim_target], opts, &mut tmp);
    if cut <= i {
        return None;
    }
    // Same placeholder-render + resolve-on-commit as `build_paragraph_cache`.
    let mut raw = String::new();
    render_inline(&buffer[i..cut], opts, &mut raw);
    let mut fn_occ = fn_base.clone();
    let mut committed_inner = String::with_capacity(raw.len());
    resolve_footnote_ids(&raw, &mut fn_occ, &mut committed_inner);
    Some(HeadingCache {
        start,
        id,
        level,
        content_start: i,
        cut,
        committed_inner,
        scanned_upto: len,
        fn_occ,
        fn_nums: RegionFnNums::new(start, fn_next),
    })
}

/// Arm the rule cache for the still-growing thematic-break line at `start`
/// (see [`RuleCache`]). `None` when the line is already complete.
fn build_rule_cache(buffer: &str, start: usize, id: u64) -> Option<RuleCache> {
    let bytes = buffer.as_bytes();
    let len = bytes.len();
    if bytes[start..len].iter().any(|&b| matches!(b, b'\n' | b'\r')) {
        return None;
    }
    // The break char is the first non-space byte (`scan_hr`, ≤3 indent).
    let ch = bytes[start..len].iter().copied().find(|&b| b != b' ')?;
    if !matches!(ch, b'-' | b'*' | b'_') {
        return None;
    }
    Some(RuleCache { start, id, ch, scanned_upto: len })
}

/// Arm the provisional fence-info cache for a code fence whose OPENER line is
/// still growing at `start` (see [`FenceInfoCache`]). `None` when the opener
/// is complete (the real [`FenceCache`] owns it) or the first info word isn't
/// settled yet (no whitespace-separated info follows it — the language class
/// would drift as the word grows, so the full path keeps that shape).
fn build_fence_info_cache(
    buffer: &str,
    start: usize,
    info: &str,
    fence_char: u8,
    id: u64,
    kind: BlockKind,
) -> Option<FenceInfoCache> {
    let bytes = buffer.as_bytes();
    let len = bytes.len();
    if bytes[start..len].iter().any(|&b| matches!(b, b'\n' | b'\r')) {
        return None;
    }
    let mut words = info.split_whitespace();
    words.next()?; // a first word exists…
    words.next()?; // …and is settled (more info follows it)
    // Frozen block HTML — `render_code_fence` with an empty body.
    let mut html = String::with_capacity(64);
    push_code_fence_open(info, &mut html);
    html.push_str("</code></pre>");
    Some(FenceInfoCache { start, id, backtick: fence_char == b'`', html, kind, scanned_upto: len })
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

/// Fold ONE fence body line into the cache buffers: shed the opener's indent
/// (§4.5), escape into `escaped`, and mirror the same de-indented RAW bytes into
/// `decoded` when the `block_data` twin is live. O(line) — the de-indent is a
/// bounded (≤3-column) walk at the line's head, paid once as the line folds in,
/// never on a later append.
fn push_fence_body_line(
    line: &[u8],
    indent: usize,
    escaped: &mut String,
    decoded: Option<&mut String>,
) {
    let (spaces, rest) = if indent == 0 { (0, line) } else { split_cols(line, indent) };
    let rest = std::str::from_utf8(rest).unwrap_or("");
    for _ in 0..spaces {
        escaped.push(' ');
    }
    escape_html(rest, escaped);
    if let Some(d) = decoded {
        for _ in 0..spaces {
            d.push(' ');
        }
        d.push_str(rest);
    }
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
    let indent = fence_indent(&bytes[start..body_start]);
    let mut escaped_lines = String::new();
    let mut decoded_lines = if indent > 0 { Some(String::new()) } else { None };
    let mut has_body_line = false;
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
                if has_body_line {
                    escaped_lines.push('\n');
                    if let Some(d) = &mut decoded_lines {
                        d.push('\n');
                    }
                }
                has_body_line = true;
                push_fence_body_line(
                    &bytes[pos..content_end],
                    indent,
                    &mut escaped_lines,
                    decoded_lines.as_mut(),
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
        body_start,
        fence_indent: indent,
        escaped_lines,
        decoded_lines,
        has_body_line,
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
    let mut has_body_line = false;
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
                if has_body_line {
                    escaped_lines.push('\n');
                }
                has_body_line = true;
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
        body_start,
        // `$$…$$` / `\[…\]` are not CommonMark fences and carry no §4.5
        // de-indent; the body path stays a verbatim buffer slice.
        fence_indent: 0,
        decoded_lines: None,
        escaped_lines,
        has_body_line,
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
    fn_next: usize,
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
    raw_prefix.push_str(">\n<thead>\n<tr>\n");
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
    raw_prefix.push_str("</tr>\n</thead>\n");

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
        aligns: Rc::new(aligns),
        tbody_opened: false,
        header_cells: Rc::new(td_header_cells),
        body_cells: Vec::new(),
        fn_occ,
        fn_nums: RegionFnNums::new(start, fn_next),
        partial: None,
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
    render_inline_para(trimmed, opts, &mut tmp);
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
        cache.committed_paras.push(Rc::new(NestedBlock { html }));
    }
    cache.inner_buffer.clear();
    cache.inner_cut = 0;
    cache.committed_inner_html.clear();
    // Next paragraph's open-prefix occurrence OVERLAY starts empty over the
    // now-advanced closed-paras baseline (`fn_occ` — never cloned).
    cache.inner_fn_occ.clear();
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
    fn_next: usize,
) -> Option<ContainerCache> {
    let bytes = buffer.as_bytes();
    let end = bytes.len();
    // The Blockquote / Alert distinction is a first-line decision. A complete
    // first line settles it (carried in `block_kind`); a partial one arms only a
    // Blockquote whose content can no longer become an alert marker — see the
    // per-kind arms. `first_line_end` is the partial line's tail pre-newline.
    let first_nl = bytes[start..end].iter().position(|&b| b == b'\n');
    let first_line_end = first_nl.map_or(end, |nl| start + nl);
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
            // Mid-line arm: no newline yet. Bail unless the partial first line can
            // no longer become an alert marker (`first_line_alert_undecided`) AND
            // renders as a plain paragraph — structured inner is the
            // ContainerBlockCache's job (arming here would re-arm-then-bail every
            // append). A complete first line skips this (nothing left to settle).
            if first_nl.is_none() {
                let sc = strip_blockquote_marker(&bytes[start..end])?;
                if first_line_alert_undecided(sc)
                    || container_inner_breaks_paragraph(sc, opts.scan_ctx())
                {
                    return None;
                }
            }
            let mut w = String::with_capacity(32);
            w.push_str("<blockquote");
            w.push_str(opts.dir());
            w.push_str(">\n");
            (ContainerCacheKind::Blockquote, w, String::from("</blockquote>"), start)
        }
        BlockKind::Alert { kind: ak, .. } => {
            // An alert's body starts on line 2, so the `[!KIND]` marker line must
            // be complete before this cache can arm.
            first_nl?;
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
    // Marker-less lines are fine when they are lazy paragraph continuations
    // (the scanner's rule: inner paragraph open, non-blank, not a block start —
    // `try_incremental_container` re-emits them); a marker-less line that instead
    // ENDS the quote means the tail isn't a single growing container, so arming
    // would bail every append. Let the full reparse own both bad shapes.
    {
        let mut para_open = false;
        let mut p = lines_upto;
        while p < end {
            let r = match bytes[p..end].iter().position(|&b| b == b'\n') {
                None => break, // trailing partial — settled by try_incremental_container
                Some(r) => r,
            };
            let line = &bytes[p..p + r];
            if let Some(stripped) = strip_blockquote_marker(line) {
                if stripped.iter().all(|&b| matches!(b, b' ' | b'\t')) {
                    para_open = false;
                } else if container_inner_breaks_paragraph(stripped, opts.scan_ctx()) {
                    return None;
                } else {
                    para_open = true;
                }
            } else if !para_open
                || line.iter().all(|&b| matches!(b, b' ' | b'\t'))
                || would_start_other_block(line, 0, opts.scan_ctx())
            {
                return None;
            }
            // else: a lazy continuation — the paragraph stays open.
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
        inner_fn_occ: HashMap::new(),
        fn_nums: RegionFnNums::new(start, fn_next),
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
    fn_next: usize,
) -> Option<ListCache> {
    let bytes = buffer.as_bytes();
    let end = bytes.len();
    let first_nl = bytes[start..end].iter().position(|&b| b == b'\n')?;
    if bytes[start..start + first_nl].contains(&b'\r') {
        return None;
    }
    let first_line = &bytes[start..start + first_nl];
    let m = scan_marker(first_line, opts.scan_ctx())?;
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
        edge: m.marker_indent,
        cur_ci: m.content_indent,
        opener_html,
        cached_prefix,
        lines_upto: start,
        open_item_start: start,
        prev_blank: false,
        item_blank: false,
        item_empty: false,
        open_stream: None,
        stream_disabled: false,
        loose: false,
        items: Vec::new(),
        item_html: Vec::new(),
        fn_occ: fn_base.clone(),
        fn_occ_base: fn_base.clone(),
        fn_nums: RegionFnNums::new(start, fn_next),
    })
}

/// Fold one item — given its raw multi-line source span `item` (marker line +
/// any nested-list / continuation body lines, the same slice the full path's
/// `render_list_item` receives) — into `cached_prefix`, plus its trailing `\n`,
/// and when `block_data` is on capture its inner `<li>` HTML into `item_html`
/// (the keyed-renderer channel). De-indents via `item_body` and renders via the
/// SHARED `render_item_body`, so a nested sub-list is rendered recursively exactly
/// as the full path does (byte-identical). Footnote refs render as occurrence-
/// independent placeholder tokens resolved into `out` advancing the LAYERED
/// occurrence state `base`+`over` — `base` is a never-mutated seed (the cache's
/// frozen map for a speculative fold; empty for a committing fold that advances
/// its full map as `over` in place) so no growing map is ever cloned. The inner
/// `<li>` span is resolved BEFORE the advance from a clone of the (small)
/// overlay (same pre-item counts, matching ids, no double-count). When
/// footnotes are off this is a token-free byte copy. Returns `None` (so the
/// caller can bail) on the invalid-UTF-8 / no-marker path. `out`/`html_sink`
/// are passed separately so the speculative open/partial item can capture into
/// a scratch buffer without committing it to the cache.
///
/// `item_start` is `item`'s own offset in the (append-only, never-drained) buffer,
/// i.e. document-absolute — recorded verbatim on the `ListItemData` so a consumer
/// can index the source back to this item's marker. Every caller already holds it.
fn fold_item_body(
    item: &[u8],
    item_start: usize,
    loose: bool,
    opts: &RenderOpts,
    out: &mut String,
    html_sink: Option<&mut Vec<Rc<ListItemData>>>,
    base: &HashMap<String, usize>,
    over: &mut HashMap<String, usize>,
) -> Option<()> {
    let body = item_body(item, opts.scan_ctx())?;
    let mut tmp = String::new();
    // `span` is `Some((lo, hi))` only when `block_data` is on.
    let span = render_item_body(body, loose, opts, &mut tmp);
    if let (Some(sink), Some((lo, hi))) = (html_sink, span) {
        let mut seed = over.clone();
        let mut inner = String::with_capacity(hi - lo);
        resolve_footnote_ids_overlay(&tmp[lo..hi], base, &mut seed, &mut inner);
        sink.push(Rc::new(ListItemData { html: inner, start: Some(item_start) }));
    }
    resolve_footnote_ids_overlay(&tmp, base, over, out);
    out.push('\n');
    Some(())
}

/// Feed the open item's newly-arrived body bytes `[st.fed_outer..end)` to the
/// nested parser, de-indented exactly like [`item_body`]: every deeper / blank
/// line is `strip_cols(line, ci)` (spaces and boundary-crossing tabs included),
/// a mid-line continuation feeds raw. Trailing whitespace is HELD BACK (see
/// [`OpenItemStream`]): the de-indented text is split at its last
/// non-whitespace byte; the whitespace suffix waits in `held_ws` until content
/// proves it interior. An undecided leading indent at EOF (fewer than `ci`
/// whitespace columns and no content byte yet) simply waits.
///
/// Returns `None` on a shallow non-blank line — `item_body` re-indents those
/// (lazy continuation), which an append-only feed can't reproduce — the caller then disables the
/// stream for this item.
fn feed_open_item(st: &mut OpenItemStream, bytes: &[u8], end: usize) -> Option<()> {
    let mut pos = st.fed_outer;
    // Only the bytes newly arrived THIS call; the already-held whitespace run
    // stays in `held_ws` (it moves — once — when content proves it interior,
    // so a long blank run isn't re-copied per append).
    let mut pending = String::new();
    while pos < end {
        if st.mid_line {
            match bytes[pos..end].iter().position(|&b| b == b'\n') {
                Some(r) => {
                    pending.push_str(std::str::from_utf8(&bytes[pos..pos + r + 1]).ok()?);
                    pos += r + 1;
                    st.mid_line = false;
                }
                None => {
                    pending.push_str(std::str::from_utf8(&bytes[pos..end]).ok()?);
                    pos = end;
                }
            }
            continue;
        }
        // At a source line start: replicate `split_cols(line, ci)` — consume up
        // to `ci` columns; a tab crossing the boundary re-emits its overflow.
        let mut i = pos;
        let mut col = 0usize;
        let mut overflow = 0usize;
        let mut straddled = false;
        while i < end && col < st.ci {
            match bytes[i] {
                b' ' => {
                    col += 1;
                    i += 1;
                }
                b'\t' => {
                    col += 4 - (col % 4);
                    i += 1;
                    if col > st.ci {
                        straddled = true;
                        break;
                    }
                }
                _ => break,
            }
        }
        if straddled {
            // `split_cols`' straddle rule: finish the whitespace run in true
            // columns. The stops of the tabs after the boundary were measured
            // from the ORIGINAL column, so they cannot stay literal once the
            // line is re-based — the whole run materializes as spaces.
            while i < end {
                match bytes[i] {
                    b' ' => col += 1,
                    b'\t' => col += 4 - (col % 4),
                    _ => break,
                }
                i += 1;
            }
            if i >= end {
                break; // run still growing — its width isn't settled yet
            }
            overflow = col - st.ci;
        }
        if i >= end && col < st.ci {
            break; // undecided leading whitespace at EOF — wait for more bytes
        }
        if col < st.ci && bytes[i] != b'\n' {
            // Shallow non-blank line: `item_body` re-indents it to `LAZY_INDENT`
            // (lazy continuation) — not reproducible append-only. Disable the stream.
            return None;
        }
        for _ in 0..overflow {
            pending.push(' ');
        }
        match bytes[i..end].iter().position(|&b| b == b'\n') {
            Some(r) => {
                pending.push_str(std::str::from_utf8(&bytes[i..i + r + 1]).ok()?);
                pos = i + r + 1;
            }
            None => {
                pending.push_str(std::str::from_utf8(&bytes[i..end]).ok()?);
                pos = end;
                st.mid_line = true;
            }
        }
    }
    st.fed_outer = pos;
    // Hold back the trailing whitespace run; feed everything before it (the
    // previously-held run first — its content byte just arrived).
    match pending.rfind(|c: char| !matches!(c, ' ' | '\t' | '\n' | '\r')) {
        Some(k) => {
            let split = k + pending[k..].chars().next().map_or(1, char::len_utf8);
            let tail = pending.split_off(split);
            if !st.held_ws.is_empty() {
                st.inner.append(&st.held_ws);
                st.held_ws.clear();
            }
            st.inner.append(&pending);
            st.held_ws = tail;
        }
        None => st.held_ws.push_str(&pending),
    }
    Some(())
}

/// Can this open item's render differ between the two `open_tail` variants?
/// Every `opts.open_tail` branch in inline.rs needs a trigger byte to fire
/// (`` ` `` code span, `$` math, `\(`/`\[` math, `[` link, `<` raw-HTML/autolink
/// tail suppression), and only blocks the
/// assembly serves from a FROZEN open-variant render matter — pieces re-rendered
/// from source with the append's own opts (tight paragraphs, the settled-prefix
/// cut) are variant-correct by construction. So the item is sensitive iff a
/// trigger byte sits in an inline-BEARING block that is `.html`/memo-served:
///   - CodeBlock / MathBlock / Mermaid never inline-render; with a COMPLETE
///     opener line (a `\n` inside the span) their kind is frozen, so their
///     bytes (fence openers with backticks, code bodies) are exempt — a
///     partial single-line span could still reclassify to a paragraph, so it
///     stays scanned (conservative),
///   - the tight active paragraphs are source-rendered fresh — exempt,
///   - everything else is scanned: committed blocks once (sticky watermark),
///     active blocks per settled append (bounded by the non-code active tail).
/// `false` ⇒ both variants render byte-identically and the stream can serve a
/// settled (buffer-ends-blank) append too.
fn open_item_ot_sensitive(st: &mut OpenItemStream, loose: bool) -> bool {
    if st.sens_committed {
        return true;
    }
    let has_trigger =
        |s: &[u8]| s.iter().any(|&b| matches!(b, b'`' | b'$' | b'[' | b'\\' | b'<'));
    let inline_free = |b: &Block, buf: &[u8]| {
        matches!(
            b.kind,
            BlockKind::CodeBlock { .. } | BlockKind::MathBlock(_) | BlockKind::Mermaid
        ) && buf[b.start..b.end].contains(&b'\n')
    };
    let buf = st.inner.buffer().as_bytes();
    let committed = st.inner.committed_blocks.len();
    let blocks: Vec<&Block> = st.inner.all_blocks().collect();
    while st.sens_scanned < committed {
        let b = blocks[st.sens_scanned];
        if !inline_free(b, buf) && has_trigger(&buf[b.start..b.end]) {
            st.sens_committed = true;
            return true;
        }
        st.sens_scanned += 1;
    }
    for b in blocks.iter().skip(committed) {
        if (!loose && matches!(b.kind, BlockKind::Paragraph)) || inline_free(b, buf) {
            continue;
        }
        if has_trigger(&buf[b.start..b.end]) {
            return true;
        }
    }
    false
}

/// §5.3 "directly loose" test for the streamed open item: does a blank line sit
/// in a gap BETWEEN two of the nested body's top-level blocks? Mirrors
/// [`item_directly_loose`] exactly (a blank inside one block — a fence body, a
/// sub-list's own interior — is invisible to the top-level gap walk), but reads
/// the nested parser's live block list instead of re-scanning the whole body.
/// Gaps between COMMITTED nested blocks are frozen — checked once
/// (`gap_pairs_done`); gaps touching the active tail re-check each append
/// (cheap: at most a couple of blocks, and a gap is only ever a short blank
/// run). Once this returns `true` the caller flips the list loose (sticky), so
/// the walk stops running entirely.
fn open_item_gap_blank(st: &mut OpenItemStream) -> bool {
    let buf = st.inner.buffer().as_bytes();
    let committed = st.inner.committed_blocks.len();
    let spans: Vec<(usize, usize)> = st.inner.all_blocks().map(|b| (b.start, b.end)).collect();
    let mut i = st.gap_pairs_done;
    while i + 1 < spans.len() {
        let (gap_start, gap_end) = (spans[i].1, spans[i + 1].0);
        let mut p = gap_start;
        while p < gap_end {
            if is_blank_line(buf, p) {
                return true;
            }
            p = line_end(buf, p);
        }
        if i + 1 < committed {
            st.gap_pairs_done = i + 1;
        }
        i += 1;
    }
    false
}

/// Assemble the OPEN item's `<li>…</li>\n` from its nested stream — mirroring
/// [`render_item_body`] branch by branch so the bytes match `fold_item_body`
/// over the same span (the mid-stream parity contract):
///   - `<li dir?>` wrapper, optional `<label>` (a11y + task + tight single
///     paragraph) and task checkbox,
///   - empty body ⇒ nothing,
///   - tight single paragraph ⇒ its inline render, no `<p>` (settled-prefix cut
///     for the growing tail),
///   - otherwise, per child: tight paragraphs render inline from SOURCE with no
///     separator on either side (memoized once committed; the trailing one via
///     the cut), every other child is the nested block's `render_block` html
///     verbatim bracketed by `cr()` — the same rule, byte for byte.
fn assemble_open_item(
    st: &mut OpenItemStream,
    loose: bool,
    opts: &RenderOpts,
    out: &mut String,
) {
    let OpenItemStream {
        inner,
        task,
        para_start,
        para_cut,
        para_settled,
        tight_memo,
        ..
    } = st;
    let inner: &StreamParser = inner;
    let buf = inner.buffer();
    let committed = inner.committed_blocks.len();
    let blocks: Vec<&Block> = inner.all_blocks().collect();

    // The trailing (growing) tight paragraph: rendered via a settled-prefix cut
    // (`render_inline_boundary`), so a plain prose body is O(new bytes).
    let mut push_tail_para = |b: &Block, out: &mut String| {
        if *para_start != b.start {
            *para_start = b.start;
            *para_cut = b.start;
            para_settled.clear();
        }
        let content_end = buf.len(); // feed holds trailing whitespace back
        let mut active = String::new();
        let boundary =
            render_inline_para_boundary(&buf[*para_cut..content_end], opts, &mut active);
        if boundary > 0 {
            render_inline_para(&buf[*para_cut..*para_cut + boundary], opts, para_settled);
            *para_cut += boundary;
            active.clear();
            render_inline_para(&buf[*para_cut..content_end], opts, &mut active);
        }
        out.push_str(para_settled);
        out.push_str(&active);
    };

    out.push_str("<li");
    out.push_str(opts.dir());
    out.push('>');
    let single_tight_para =
        !loose && blocks.len() == 1 && matches!(blocks[0].kind, BlockKind::Paragraph);
    let wrap_label = opts.a11y && task.is_some() && single_tight_para;
    if wrap_label {
        out.push_str("<label>");
    }
    if let Some(checked) = *task {
        // Byte-identical to the full renderer's checkbox (see `render_list_item`
        // in render.rs): GFM's attribute order and `=""` boolean spelling.
        out.push_str(if checked {
            "<input checked=\"\" disabled=\"\" type=\"checkbox\"> "
        } else {
            "<input disabled=\"\" type=\"checkbox\"> "
        });
    }
    if blocks.is_empty() {
        // Empty item.
    } else if single_tight_para {
        push_tail_para(blocks[0], out);
    } else {
        let last = blocks.len() - 1;
        for (bi, b) in blocks.iter().enumerate() {
            if !loose && matches!(b.kind, BlockKind::Paragraph) {
                if bi == last {
                    push_tail_para(b, out);
                } else if bi < committed {
                    // Settled interior paragraph — rendered once.
                    let html = tight_memo.entry((b.start, b.end)).or_insert_with(|| {
                        let mut s = String::new();
                        render_inline_para(trim_trailing_newlines(&buf[b.start..b.end]), opts, &mut s);
                        s
                    });
                    out.push_str(html);
                } else {
                    // A held-back (still-active, non-last) paragraph — transient
                    // and line-bounded; re-render fresh.
                    render_inline_para(trim_trailing_newlines(&buf[b.start..b.end]), opts, out);
                }
            } else {
                cr(out);
                out.push_str(&b.html);
                cr(out);
            }
        }
    }
    if wrap_label {
        out.push_str("</label>");
    }
    out.push_str("</li>");
    out.push('\n');
}

/// Scan `bytes[from..to]` — the body of an open single-`$` inline-math span
/// whose opener sits at `opener` (`from > opener`) — for anything that would end
/// the speculative-open span. Returns `true` while it provably stays open:
///
/// * a `\n`/`\r` ends the single-line paragraph (multi-line dollar runs are out
///   of scope — the full path handles them), and
/// * a `$` with a non-whitespace byte immediately to its left is a valid inline
///   closer (the pandoc rule; the 2nd `$` of a `$$` run trips this too, so
///   display-math runs also drop). Any such `$` pairs the opener forward, so the
///   span is no longer open. The digit-after refinement in `build_dollar_index`
///   is deliberately NOT applied here: treating every non-space-preceded `$` as a
///   closer only ever OVER-drops (falling back to the byte-identical full path),
///   never under-drops, and the `$x $x …` soup never trips it (every inner `$`
///   is space-preceded).
fn dollar_span_stays_open(bytes: &[u8], opener: usize, from: usize, to: usize) -> bool {
    debug_assert!(from > opener);
    let mut i = from;
    while i < to {
        match bytes[i] {
            b'\n' | b'\r' => return false,
            b'$' if !matches!(bytes[i - 1], b' ' | b'\t' | b'\n' | b'\r') => return false,
            _ => {}
        }
        i += 1;
    }
    true
}

/// Arm the dollar-tail cache when the open paragraph at `start` is one still-open
/// single-`$` inline-math span running to EOF (see [`DollarTailCache`]). `None`
/// unless the first byte is a `$` opening a valid single-`$` span (a non-space,
/// non-`$` byte to its right — `$$` is display math) whose newline-free body
/// carries no valid closer yet. Callers gate this on `gfm_math`.
fn build_dollar_tail_cache(buffer: &str, start: usize, id: u64) -> Option<DollarTailCache> {
    let bytes = buffer.as_bytes();
    let len = bytes.len();
    if bytes.get(start) != Some(&b'$') {
        return None;
    }
    // Single-`$` opener rule (pandoc): the next byte must exist and be a
    // non-space; exclude `$$` (display math — the full path owns it).
    match bytes.get(start + 1) {
        None | Some(&(b'$' | b' ' | b'\t' | b'\n' | b'\r')) => return None,
        _ => {}
    }
    if !dollar_span_stays_open(bytes, start, start + 1, len) {
        return None;
    }
    let mut math = String::with_capacity(len - start);
    escape_html(&buffer[start + 1..len], &mut math);
    Some(DollarTailCache { start, id, scanned: len, math })
}

/// True iff every byte of `bytes[from..to]` is ASCII alphanumeric — the fast
/// path's invariant (see [`AlnumTailCache`]). Deliberately narrow: `.+_-@` and
/// whitespace are EXCLUDED even though some are inert in isolation, because any
/// of them can settle the commit cut or (with autolinks) trigger a construct
/// mid-run, and excluding them only ever OVER-drops to the byte-identical full
/// path. A pure-ASCII-alnum run can neither open an inline construct nor complete
/// an autolink (`try_ext_autolink` needs `http://`/`www.`, `try_ext_email` needs
/// `@` — all punctuation this run lacks), and `escape_html` leaves it unchanged.
fn alnum_run_stays_open(bytes: &[u8], from: usize, to: usize) -> bool {
    bytes[from..to].iter().all(u8::is_ascii_alphanumeric)
}

/// Arm the alnum-tail cache when the open paragraph at `start` is one pure-ASCII-
/// alphanumeric run to EOF (see [`AlnumTailCache`]). `None` unless the whole
/// `buffer[start..]` is non-empty and all alnum. Callers gate this on
/// `gfm_autolinks` (the only config under which the cut is pinned here).
fn build_alnum_tail_cache(buffer: &str, start: usize, id: u64) -> Option<AlnumTailCache> {
    let bytes = buffer.as_bytes();
    let len = bytes.len();
    if start >= len || !alnum_run_stays_open(bytes, start, len) {
        return None;
    }
    let mut body = String::with_capacity(len - start);
    escape_html(&buffer[start..len], &mut body);
    Some(AlnumTailCache { start, id, scanned: len, body })
}

/// True iff the appended bytes `bytes[from..to]` keep an open raw tag streaming
/// to EOF inside its unclosed `quote`-delimited attribute value (see
/// [`RawTagTailCache`]). Drops on the matching `quote` (the value closes — the
/// tag can then complete or gain attrs) or a newline (the single-line paragraph
/// splits). Any other byte is opaque value content that keeps the value open.
fn raw_tag_stays_open(bytes: &[u8], from: usize, to: usize, quote: u8) -> bool {
    !bytes[from..to].iter().any(|&b| b == quote || b == b'\n' || b == b'\r')
}

/// Arm the raw-tag-tail cache when the open paragraph at `start` is one raw open
/// tag whose quoted attribute value streams to EOF (see [`RawTagTailCache`]).
/// `None` unless the paragraph starts with `<`, `buffer[start..]` is newline-free
/// (single-line invariant), and the tag is currently inside an unclosed quoted
/// attribute value (per [`open_tag_streaming_quote`], which mirrors
/// `inline_html_streams_to_eof`'s open-tag grammar). Callers gate this on
/// sanitize/unsafe HTML (in escape mode the `<` is visible `&lt;…` text, not
/// suppressed).
fn build_raw_tag_tail_cache(buffer: &str, start: usize, id: u64) -> Option<RawTagTailCache> {
    let bytes = buffer.as_bytes();
    if bytes.get(start) != Some(&b'<') {
        return None;
    }
    // Single-line invariant: a newline before the value closes would let the
    // block restructure without the extension guard seeing it. Require the whole
    // open tag (and its partial value) newline-free at arm; the guard then only
    // has to watch the appended bytes.
    if bytes[start..].iter().any(|&b| b == b'\n' || b == b'\r') {
        return None;
    }
    let quote = open_tag_streaming_quote(bytes, start)?;
    Some(RawTagTailCache { start, id, scanned: bytes.len(), quote })
}

/// Scan `bytes[from..to]` — a slice of the mod-3 soup body (everything AFTER the
/// leading `**`) — returning the new settled offset, or `None` to DROP to the
/// byte-identical full path (see [`Mod3TailCache`]). A byte settles only when it
/// is provably literal while the `**` is the sole opener:
///
/// * an ASCII-alnum / space / tab text byte (the catch-all text arm; deliberately
///   narrow — `.:/@+-` etc. are EXCLUDED, so no extended autolink can form after
///   the boundary bytes `*`/space, and every excluded byte only ever OVER-drops);
/// * a single `*` that is DECIDABLY closer-only: a non-`*` byte follows it and
///   that byte is whitespace, so it cannot left-flank (open) and can only
///   right-flank (close) — and it is mod-3-blocked (`2 + 1 ≡ 0`) against the lone
///   `**`, so it renders literally.
///
/// It drops on a `*` run of decided length ≥ 2 (a `**` closer pairs the leading
/// `**`, `2 + 2 ≢ 0 (mod 3)` → `<strong>`), a single `*` a non-space follows
/// (it could open and later pair), and any other byte (newline, construct/entity
/// opener, non-ASCII). A `*` run abutting the slice end (`to`) is left PENDING:
/// its length and flanking are undecided until the next append, so `to - settled`
/// (0 or 1 byte) is the pending suffix the caller renders literally.
fn mod3_body_scan(bytes: &[u8], from: usize, to: usize) -> Option<usize> {
    let mut i = from;
    while i < to {
        let b = bytes[i];
        if b.is_ascii_alphanumeric() || b == b' ' || b == b'\t' {
            i += 1;
        } else if b == b'*' {
            let mut j = i + 1;
            while j < to && bytes[j] == b'*' {
                j += 1;
            }
            if j == to {
                // Trailing `*` run abutting the chunk edge. A run already at
                // length ≥ 2 is a decided `**` closer waiting to pair (a drop);
                // a single `*` is genuinely undecided — hold it pending.
                return if j - i >= 2 { None } else { Some(i) };
            }
            // The run is followed by a decided byte. A `**`+ closer pairs the
            // leading `**`; a single `*` renders literally only if it cannot open
            // (the following byte is whitespace, so it is closer-only or inert).
            if j - i >= 2 {
                return None;
            }
            match bytes[j] {
                b' ' | b'\t' => i = j,
                _ => return None,
            }
        } else {
            return None;
        }
    }
    Some(to)
}

/// Arm the mod3-tail cache when the open paragraph at `start` is the `a**bc* …`
/// soup (see [`Mod3TailCache`]). `None` unless the paragraph is a leading run of
/// ASCII-alnum/space/tab text, then a `**` run of EXACTLY two `*` flanked by
/// ASCII-alnum bytes on both sides (a sufficient condition for the `**` to both
/// open and close, keeping the mod-3 rule live), then a body that
/// [`mod3_body_scan`] settles. Callers gate this only on `ParagraphCache` having
/// failed to arm (the pin condition); emphasis is always on, so no config gate.
/// Byte length of the leading spaces/tabs run — a paragraph's non-content
/// indent (mirrors `inline::para_indent` for the escape-only fast paths).
fn leading_para_ws(bytes: &[u8]) -> usize {
    bytes.iter().take_while(|&&b| matches!(b, b' ' | b'\t')).count()
}

fn build_mod3_tail_cache(buffer: &str, start: usize, id: u64) -> Option<Mod3TailCache> {
    let bytes = buffer.as_bytes();
    let len = bytes.len();
    // Leading inert text (no `*`), up to the first `*`.
    let mut i = start;
    while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b' ' || bytes[i] == b'\t') {
        i += 1;
    }
    // The `**` opener must be flanked by ASCII-alnum on both sides: the left one
    // (non-space, non-punct) gives can-close, the right one gives can-open, so the
    // mod-3 rule blocks every single-`*` closer. This also forbids a `*` neighbor
    // (so the run is exactly two) and a leading/space-preceded `**` (can-close
    // would be false — a real streaming bold, which the full path owns).
    if i == start || !bytes[i - 1].is_ascii_alphanumeric() {
        return None;
    }
    if bytes.get(i) != Some(&b'*') || bytes.get(i + 1) != Some(&b'*') {
        return None;
    }
    match bytes.get(i + 2) {
        Some(&c) if c.is_ascii_alphanumeric() => {}
        _ => return None,
    }
    let settled = mod3_body_scan(bytes, i + 2, len)?;
    // The escaped body starts past the paragraph's leading spaces/tabs — the
    // full path drops them (`render_inline_para`) and this cache renders the
    // paragraph's inner itself, so it must drop them too. The single-line
    // invariant (`mod3_body_scan` rejects `\n`) means there is no continuation
    // line or line-final run to trim here.
    let body_start = start + leading_para_ws(&bytes[start..settled]);
    let mut body = String::with_capacity(settled - body_start);
    escape_html(&buffer[body_start..settled], &mut body);
    Some(Mod3TailCache { start, id, settled, body })
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
    fn_next: usize,
) -> Option<ParagraphCache> {
    let bytes = buffer.as_bytes();
    let mut content_end = bytes.len();
    while content_end > start && matches!(bytes[content_end - 1], b'\n' | b'\r') {
        content_end -= 1;
    }
    let mut tmp = String::new();
    let cut = start + render_inline_para_boundary(&buffer[start..content_end], opts, &mut tmp);
    if cut <= start {
        return None;
    }
    // Render the settled prefix with placeholder tokens (when footnotes on), then
    // resolve them into `committed_inner` from the committed occurrence baseline,
    // advancing the cache-local `fn_occ` map. The frozen prefix never re-renders.
    let mut raw = String::new();
    render_inline_para(&buffer[start..cut], opts, &mut raw);
    let mut fn_occ = fn_base.clone();
    let mut committed_inner = String::with_capacity(raw.len());
    resolve_footnote_ids(&raw, &mut fn_occ, &mut committed_inner);
    Some(ParagraphCache {
        start,
        id,
        cut,
        committed_inner,
        fn_occ,
        fn_nums: RegionFnNums::new(start, fn_next),
    })
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
/// is consumed whole and stops the strip. Mirrors the per-line stripping in
/// `render_indented_code`.
fn indented_strip(line: &[u8]) -> &[u8] {
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
    &line[i..]
}

/// [`indented_strip`] + escape the remainder into `out`.
fn push_indented_content(line: &[u8], out: &mut String) {
    escape_html(std::str::from_utf8(indented_strip(line)).unwrap_or(""), out);
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
/// cached prefix: verbatim for pass-through (tagfiltered when the GFM
/// tagfilter is on — every tag boundary a disallowed `<name` needs lies within
/// the line + its `\n`, so the per-line decision is final and matches the full
/// path), `escape_html`d otherwise. Newlines pass through `escape_html`
/// unchanged, so the escaped prefix keeps line breaks.
fn fold_html_line(line: &[u8], pass_through: bool, tagfilter: bool, out: &mut String) {
    let s = std::str::from_utf8(line).unwrap_or("");
    if pass_through && tagfilter {
        push_tagfiltered(s, out);
    } else if pass_through {
        out.push_str(s);
    } else {
        escape_html(s, out);
    }
}

/// Arm the indented-code cache for the open block at `start`, walking its body
/// lines once (interior blank lines fold like content — the same accounting as
/// `try_incremental_indented`, so the cache can arm on a region that already
/// contains blanks). Returns `None` (no caching) if a non-blank line dedents or
/// any line contains a `\r` — those keep going through the full renderer.
fn build_indented_cache(
    buffer: &str,
    start: usize,
    id: u64,
    block_data: bool,
) -> Option<IndentedCodeCache> {
    let bytes = buffer.as_bytes();
    let end = bytes.len();
    let mut escaped_lines = String::new();
    let mut decoded_lines = String::new();
    let mut lines_upto = start;
    let mut pos = start;
    while pos < end {
        match bytes[pos..end].iter().position(|&b| b == b'\n') {
            None => break,
            Some(r) => {
                let content_end = pos + r;
                let next = pos + r + 1;
                let line = &bytes[pos..content_end];
                let blank = line.iter().all(|&b| matches!(b, b' ' | b'\t'));
                if line.contains(&b'\r') || (!blank && !indented_code_line(line)) {
                    return None;
                }
                if !escaped_lines.is_empty() {
                    escaped_lines.push('\n');
                    if block_data {
                        decoded_lines.push('\n');
                    }
                }
                push_indented_content(line, &mut escaped_lines);
                if block_data {
                    let raw = indented_strip(line);
                    decoded_lines.push_str(std::str::from_utf8(raw).unwrap_or(""));
                }
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
    Some(IndentedCodeCache { start, id, escaped_lines, decoded_lines, lines_upto })
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
    let tagfilter = pass_through && opts.gfm_tagfilter;
    let sanitize = html_block_sanitizes(opts.block_html, opts.html_sanitize, html_type);
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
                // Sanitize mode folds at token boundaries below instead — a tag
                // spans lines, so there is no per-line output to fold here.
                if !sanitize {
                    fold_html_line(line, pass_through, tagfilter, &mut cached_prefix);
                }
                lines_upto = next;
                pos = next;
            }
        }
    }
    // The cache LOCKS `html_type` at arm time, but the type is a function of the
    // opener line alone — and while that line is still arriving it can change
    // under us (`<p` reads as type 6; `<pre` is type 1). Refuse to arm until the
    // opener line is complete (`lines_upto` has advanced past it), so the locked
    // type is final. Load-bearing in sanitize mode, where the type decides
    // whether the block renders as elements at all: without this, a streaming
    // `<pre>`/`<textarea>` block would sanitize through the type-6 lock.
    if lines_upto == start {
        return None;
    }
    // An EMPTY partial (buffer ends exactly at `\n`) is "no next line yet", not
    // the type-6/7 closing blank line — keep arming (mirrors try_incremental_html).
    let partial = &bytes[lines_upto..end];
    if !partial.is_empty() && html_block_closes_here(partial, html_type, partial) {
        return None;
    }
    // No line in [start, end) closes the block, so every byte of it is content:
    // fold the whole region's settled tokens in one pass (the per-append fold
    // then only ever sees NEW bytes).
    let mut open_tags = Vec::new();
    let tokens_upto = if sanitize {
        fold_block_html(buffer, start, opts.html_policy(), &mut open_tags, &mut cached_prefix)
    } else {
        start
    };
    Some(HtmlBlockCache {
        start,
        id,
        html_type,
        pass_through,
        tagfilter,
        sanitize,
        cached_prefix,
        lines_upto,
        tokens_upto,
        open_tags,
    })
}

/// Why (or whether) the open paragraph beginning before `cut` stops before EOF.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ParaEnd {
    /// It doesn't — the paragraph still runs all the way to EOF.
    No,
    /// A PERMANENT terminator: a completed blank line, an interrupting block
    /// start, a setext underline, or a table delimiter row under a matching
    /// header (the last two retroactively change the block's kind). The
    /// incremental path must drop its cache and let the full scan re-form it.
    Settled,
    /// A TRANSIENT terminator, carrying the offending line's start: the only
    /// thing ending the paragraph is the buffer's FINAL line being
    /// whitespace-only and not terminated yet.
    ///
    /// A chunk boundary landing inside a continuation line's leading indent
    /// leaves the buffer in exactly this shape, and it is a real end — the
    /// block scanner splits the whitespace line off, so the open view drops
    /// back to the paragraph without it (`"a\n b\n "` renders `<p>a\n b</p>`,
    /// and a preceding hard break's `<br>` disappears with the line it belonged
    /// to). But the very next byte almost always undoes it by continuing the
    /// line, so paying a full re-scan + re-render is waste: it made word-wrapped
    /// prose with indented continuation lines O(n²), one append in `indent
    /// period` (110x the cost of the same flush prose at 512 KB / 128-byte
    /// chunks). [`StreamParser::try_incremental_paragraph`] SUSPENDS on this
    /// verdict — it renders the same closed view straight from the cache and
    /// keeps the cache, so resuming costs nothing.
    OpenBlank(usize),
}

/// Where the open paragraph beginning before `cut` ends, if it ends in
/// `[cut, EOF)`. The line containing `cut` is a continuation (it began as
/// paragraph text), so it is only re-examined once it has completed.
///
/// Single pass: the scan returns at the FIRST terminator it meets, so a
/// trailing whitespace-only line is [`ParaEnd::OpenBlank`] exactly when nothing
/// permanent precedes it — no second walk needed to establish that.
fn paragraph_end(bytes: &[u8], cut: usize, ctx: ScanCtx) -> ParaEnd {
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
                    return ParaEnd::Settled;
                }
                if is_table_delimiter_row(line_slice(bytes, cur_line_start)) {
                    let prev = prev_line_start(bytes, cur_line_start);
                    if prev != cur_line_start
                        && forms_table_header(bytes, prev, cur_line_start)
                    {
                        return ParaEnd::Settled;
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
        let end = line_end(bytes, pos);
        if is_blank_line(bytes, pos) {
            // Blank AND unterminated AND last ⇒ transient (see
            // [`ParaEnd::OpenBlank`]). Reaching here at all means nothing
            // permanent precedes it, since the scan returns at the first
            // terminator. `pos > cut` holds for any blank line (the cut sits on
            // a word), but assert the intent: the retained prefix must be left
            // wholly inside the paragraph.
            if end == len && bytes[len - 1] != b'\n' && pos > cut {
                return ParaEnd::OpenBlank(pos);
            }
            return ParaEnd::Settled;
        }
        if is_setext_underline(bytes, pos).is_some() || would_start_other_block(bytes, pos, ctx) {
            return ParaEnd::Settled;
        }
        if is_table_delimiter_row(&bytes[pos..end]) {
            let header = prev.unwrap_or_else(|| prev_line_start(bytes, pos));
            if header != pos && forms_table_header(bytes, header, pos) {
                return ParaEnd::Settled;
            }
        }
        prev = Some(pos);
        pos = end;
    }
    ParaEnd::No
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
        assert!(html.contains("checked=\"\" disabled=\"\" type=\"checkbox\""));
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

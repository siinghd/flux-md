//! Incremental streaming parser.

use std::collections::HashMap;

use crate::blocks::Block;
use crate::render::{
    blockquote_inner, classify, collect_footnote_defs, collect_footnote_refs,
    count_footnote_refs, is_fence_close_line, is_footnote_def_block, item_body, normalize_label,
    push_code_fence_open, render_block, render_footnote_section, LinkRef, RenderOpts,
};
use crate::blocks::BlockKind;
use crate::scanner::{parse_link_ref_def, scan, RawBlockKind, RawBlock, ScanCtx};
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
    committed_refs: HashMap<String, LinkRef>,
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
    /// Fast path for a long open code fence at the tail (see [`CodeFenceCache`]).
    code_cache: Option<CodeFenceCache>,
}

#[derive(Default)]
pub struct Patch {
    pub newly_committed: Vec<Block>,
    pub active: Vec<Block>,
}

/// Incremental render state for a single open code fence at the tail. Streaming
/// a long fenced block is otherwise O(n²) — every append re-scans and re-escapes
/// the whole growing body. With this cache, an append only escapes the newly
/// arrived complete lines and re-escapes the (short) trailing partial line, so
/// the block stays O(total bytes). It applies only to the plain case: the cache
/// bails to the full renderer the moment any new line looks like a fence closer
/// or contains a `\r` (so CRLF and fence-close trimming keep their exact
/// behavior). Cleared whenever the tail is no longer this open fence.
struct CodeFenceCache {
    /// Absolute byte offset of the fence opener line in `buffer`.
    start: usize,
    /// Stable id of the fence block (preserved across appends and the eventual close).
    id: u64,
    /// Classified kind (CodeBlock / MathBlock / Mermaid — all render identically).
    kind: BlockKind,
    /// Opening tag (`<pre><code…>`), computed once.
    opener_html: String,
    /// Escaped HTML of the complete body lines, joined by `\n`, no trailing `\n`.
    escaped_lines: String,
    /// Absolute offset just past the last complete body line's `\n`.
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
            committed_refs: HashMap::new(),
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
            code_cache: None,
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
        // Fast path: a long open code fence at the tail is extended in O(new
        // bytes) instead of re-scanning + re-escaping the whole body each chunk.
        if !finalizing {
            if let Some(patch) = self.try_incremental_code_fence() {
                return patch;
            }
        }

        let tail_start = self.committed_offset;
        let tail = &self.buffer[tail_start..];

        let ctx = ScanCtx { math: self.gfm_math };
        let raw_blocks = scan(tail, ctx);

        // Pre-pass: build the ref table for this render. Permanent definitions
        // from the committed region win (first-definition-wins); definitions in
        // the tail are layered on top, recomputed fresh each reparse so a
        // half-typed definition in the growing tail can't get stuck.
        let mut refs = self.committed_refs.clone();
        collect_refs(tail, &mut refs, ctx);

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
            // Move the freshly-built ref table into opts — it isn't used again
            // in this reparse, so cloning it (a full HashMap copy on every
            // append) was pure waste, O(refs) per chunk for ref-heavy docs.
            refs,
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
        let last_raw_end_to_commit = if commit_all || raw_blocks.len() > to_commit.saturating_add(0) {
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
            // The region [tail_start, new offset) just became permanent — fold
            // its (now-stable) ref + footnote definitions into the committed
            // tables, and lock in footnote numbers (so committed <sup>N</sup>
            // values never shift as the tail grows).
            let committed_slice = &self.buffer[tail_start..tail_start + last_raw_end_to_commit];
            collect_refs(committed_slice, &mut self.committed_refs, ctx);
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
            let section = render_footnote_section(&fn_nums, &fn_defs, &total_occ);
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

        // Arm (or disarm) the incremental code-fence cache. It applies only when
        // the entire tail is now a single open code fence — the long-streamed-
        // code-block case — so subsequent appends take the O(new bytes) path.
        self.code_cache = None;
        if !finalizing && new_active.len() == 1 {
            if let RawBlockKind::CodeFence { terminated: false, info, .. } = &renderable[to_commit].kind {
                let start = tail_start + renderable[to_commit].range.start;
                let gap_blank = self.buffer.as_bytes()[self.committed_offset..start]
                    .iter()
                    .all(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
                if gap_blank {
                    self.code_cache = build_code_fence_cache(
                        &self.buffer,
                        start,
                        info,
                        new_active[0].id,
                        new_active[0].kind.clone(),
                    );
                }
            }
        }

        Patch { newly_committed, active: new_active }
    }

    /// O(new bytes) extension of a long open code fence at the tail. Returns the
    /// patch directly on a cache hit; `None` falls through to the full reparse
    /// (and drops the cache) when the tail is no longer this plain open fence.
    fn try_incremental_code_fence(&mut self) -> Option<Patch> {
        let mut cache = self.code_cache.take()?;
        // The fence must still be the tail: only whitespace may sit between the
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
                    // A closing fence or CRLF: defer to the full renderer, which
                    // gets the close-trim / `\r` handling exactly right.
                    if bytes[pos..content_end].contains(&b'\r')
                        || is_fence_close_line(&bytes[pos..next])
                    {
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
        if partial.contains(&b'\r') || is_fence_close_line(partial) {
            return None;
        }
        let mut body = cache.escaped_lines.clone();
        if !partial.is_empty() {
            if !body.is_empty() {
                body.push('\n');
            }
            escape_html(std::str::from_utf8(partial).unwrap_or(""), &mut body);
        }
        if !body.is_empty() {
            body.push('\n');
        }
        let mut html = String::with_capacity(cache.opener_html.len() + body.len() + 16);
        html.push_str(&cache.opener_html);
        html.push_str(&body);
        html.push_str("</code></pre>");
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
        self.code_cache = Some(cache);
        Some(Patch { newly_committed: Vec::new(), active: vec![block] })
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
) -> Option<CodeFenceCache> {
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
    Some(CodeFenceCache { start, id, kind, opener_html, escaped_lines, lines_upto })
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
}

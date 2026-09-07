//! Inline tokenizer + HTML renderer.
//!
//! Walks an inline string left-to-right, emitting HTML directly. Handles:
//! - Backslash escapes (incl. line-break escape)
//! - HTML entity / numeric character references (&amp; &#65; &#x41; etc.)
//! - Code spans (`...`, ``...``, etc. — matching backtick run length)
//! - Strong/emphasis with a delimiter-run stack (CommonMark §6.2)
//! - Strikethrough (~~text~~)
//! - Links [text](url "title") and images ![alt](url "title")
//! - Reference-style links [text][label], [text][], [label]
//! - Autolinks <https://...>, <foo@bar>
//! - Inline raw HTML (pass-through when opts.unsafe_html is on)
//! - Hard breaks (backslash + newline, or two-spaces + newline)
//!
//! URLs go through `sanitize_url`; text is HTML-escaped; raw HTML in input
//! is escaped (or passed through if unsafe mode is enabled).

use crate::entities::decode_entity;
use crate::render::{push_tagfiltered, HtmlPolicy, RenderOpts};
use crate::url::{
    escape_attr, escape_html, sanitize_attrs, sanitize_image_url, sanitize_raw_html_attrs,
    sanitize_url,
};

const ESCAPABLE: &[u8] = b"!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~";

/// Render an inline string to HTML. Thin wrapper over [`render_inline_core`]
/// with boundary tracking off (zero overhead, byte-identical output).
pub fn render_inline(input: &str, opts: &RenderOpts, out: &mut String) {
    render_inline_core(input, opts, out, false);
}

/// Like [`render_inline`], but also returns the largest *stable* input offset —
/// the byte position up to which the rendered output is final regardless of any
/// text appended after `input`. Used by the streaming parser to commit the
/// settled prefix of a long open paragraph. Output is identical to
/// `render_inline`; only the extra analysis runs.
pub fn render_inline_boundary(input: &str, opts: &RenderOpts, out: &mut String) -> usize {
    render_inline_core(input, opts, out, true)
}

/// A paragraph's leading spaces or tabs are not content (CommonMark §4.8: the
/// raw content is the lines "with initial and final spaces or tabs removed"),
/// so every paragraph-flavored render skips them. The CONTINUATION lines' own
/// indents are shed by the break arms inside [`render_inline_core`]; only the
/// first line needs a skip here, and only because nothing precedes it.
///
/// Deliberately NOT folded into [`render_inline`]: link text, image alt, table
/// cells and inline-component bodies all keep their leading whitespace, and
/// they share the same entry point.
pub fn render_inline_para(input: &str, opts: &RenderOpts, out: &mut String) {
    render_inline_core(&input[para_indent(input)..], opts, out, false);
}

/// [`render_inline_boundary`] for a paragraph slice ([`render_inline_para`]'s
/// skip applied first). The returned cut is an offset into `input` — the skip
/// is added back — so callers keep their `start + boundary` arithmetic. A `0`
/// return still means "nothing settled": a cut is only ever proposed at a byte
/// that renders to visible content, never at the end of the leading indent.
///
/// Composition with the skip is exact because a cut never lands on whitespace
/// ([`is_boundary`]'s first guard), so only the FIRST segment of a paragraph
/// can begin inside the leading indent — every later segment starts on a byte
/// where the skip is a no-op.
pub fn render_inline_para_boundary(input: &str, opts: &RenderOpts, out: &mut String) -> usize {
    let skip = para_indent(input);
    match render_inline_core(&input[skip..], opts, out, true) {
        0 => 0,
        cut => skip + cut,
    }
}

/// True for the bytes CommonMark strips from both ends of a paragraph line —
/// spaces and tabs.
#[inline]
fn is_para_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t')
}

/// Byte length of `input`'s leading spaces/tabs run. O(indent).
fn para_indent(input: &str) -> usize {
    input.as_bytes().iter().take_while(|&&b| is_para_ws(b)).count()
}

/// Drop the spaces/tabs `out` ends with — a paragraph line's *final* whitespace,
/// which is not content — stopping at `floor` (this render's own start, so a
/// segment render can never eat into an already-frozen prefix).
///
/// Never needs to reach `floor`: the trailing whitespace run of a line always
/// renders inside the same segment as the `\n` that follows it (no cut lands on
/// whitespace), and the byte a segment starts on always renders to a non-space
/// character or is preceded by a `\n` — see [`is_boundary`].
fn trim_line_end_ws(out: &mut String, floor: usize) {
    while out.len() > floor && out.as_bytes().last().is_some_and(|&b| is_para_ws(b)) {
        out.pop();
    }
}

/// Skip a continuation line's leading spaces/tabs (the second half of the same
/// rule): `pos` is just past a line break, so whatever whitespace follows is
/// indentation, not content. O(indent), and each indent run is walked once.
fn skip_line_indent(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && is_para_ws(bytes[pos]) {
        pos += 1;
    }
    pos
}

/// A top-level position is a clean cut iff a word begins there right after a
/// newline, right after a single inter-word space (preceded by a non-space), or
/// right after the leading indent of a continuation line (see
/// [`indent_led_boundary`]) — never inside a multi-space hard-break run. That
/// last property still holds: a hard break's spaces sit at line END and
/// *precede* their `\n`, so the backward walk below — which accepts a space/tab
/// run only when a `\n` terminates it on the LEFT — can never reach into one,
/// and the guard above rejects whitespace positions outright anyway.
///
/// # The `&`/`<` exclusion
///
/// Both whitespace-led rules leave the whitespace itself in the FROZEN segment
/// — and the `\n` arm's `out.ends_with("  ")` hard-break lookbehind is the
/// renderer's one output-*state*-dependent decision. It straddles the cut
/// whenever the rest of the line renders to nothing or to a single space: the
/// suffix render sees one space (or none) where the full render sees two, and
/// emits a literal space where the full render emits `<br>`. Requiring `pos`
/// itself to render to a non-space character rules that out — the suffix's last
/// character before the next `\n` is then either that character or one of >= 2
/// characters wholly inside the suffix, so both renders decide alike.
///
/// Every byte that reaches the check qualifies (whitespace is already excluded,
/// and `` \`[!$*_~ `` and the catch-all all emit a non-space character or an
/// opening tag) except two: `&`, which may decode to a space (`&#32;`), and
/// `<`, whose whole token the sanitizer may drop (`<!--…-->`). A word right
/// after a bare `\n` needs no such guard — the frozen render then ends in `\n`
/// or `<br>\n`, never a space, so nothing can straddle.
///
/// # Cut parity under the paragraph whitespace rules
///
/// The break arms now DROP a line's final spaces/tabs and skip the next line's
/// indent, so a frozen segment no longer ends in the literal whitespace it
/// contains. The invariant survives, case by case, because a cut's own byte is
/// never whitespace:
///
/// * indent-led cut — the frozen segment now ends at the `\n` (its own render
///   skipped the indent that follows), and the suffix starts at the word byte.
///   The full render is at exactly that state after the same `\n`;
/// * inter-word-space cut — that space is not line-final (a non-space follows
///   it inside the same line), so no trim touches it and the frozen segment
///   still ends with it;
/// * `\n`-led cut — unchanged, the frozen segment ends in `\n` / `<br>\n`.
///
/// And no trim ever needs to cross a cut: a line's trailing whitespace run and
/// its `\n` share a segment (neither offers a candidate), and the segment's
/// first byte renders to a non-space character in the two space-led cases, so
/// the pop stops there — strictly above the segment's `out_floor`.
fn is_boundary(bytes: &[u8], pos: usize) -> bool {
    if pos == 0 || pos >= bytes.len() || matches!(bytes[pos], b' ' | b'\t' | b'\n' | b'\r') {
        return false;
    }
    if bytes[pos - 1] == b'\n' {
        return true;
    }
    if !matches!(bytes[pos - 1], b' ' | b'\t') || matches!(bytes[pos], b'&' | b'<') {
        return false;
    }
    // A single inter-word space (its own predecessor a non-space) …
    if bytes[pos - 1] == b' ' && pos >= 2 && !matches!(bytes[pos - 2], b' ' | b'\t' | b'\n' | b'\r')
    {
        return true;
    }
    // … or the leading indent of a continuation line.
    indent_led_boundary(bytes, pos)
}

/// The indent-led-line arm of [`is_boundary`]: `bytes[pos - 1]` is a space or
/// tab that the single-inter-word-space rule rejected, so walk back over that
/// whitespace run and accept iff a `\n` terminates it — i.e. `pos` is the first
/// word of an *indented continuation line*, the same kind of candidate a word
/// right after a bare `\n` already is (minus the `&`/`<` exclusion the caller
/// has already applied).
///
/// Without it, word-wrapped prose whose continuation lines carry any indent
/// (`"a\n b\n c\n…"` — ordinary LLM/chat output) yields NO candidates anywhere:
/// each line's first word sits after a space whose own predecessor is a space
/// or the line's `\n`, and [`synth_boundary`] can't rescue it either (its inert
/// runs need [`SYNTH_GAP`] space-free bytes; these lines have a space every few
/// bytes). The commit cut pins at 0 and every append re-renders the whole
/// accumulated paragraph — O(n²).
///
/// The walk stays O(n) per render pass: it is bounded by the whitespace run it
/// traverses, and each run is entered at most once per pass — only the position
/// at a run's right end probes it (positions inside a run are whitespace,
/// rejected by [`is_boundary`]'s first guard), so the traversed spans are
/// disjoint and sum to at most the slice length.
///
fn indent_led_boundary(bytes: &[u8], pos: usize) -> bool {
    let mut i = pos - 1;
    while i > 0 && matches!(bytes[i - 1], b' ' | b'\t') {
        i -= 1;
    }
    i > 0 && bytes[i - 1] == b'\n'
}

/// Byte distance between synthetic boundary candidates in space-free text.
/// Also the safety margin past any inert-run reset: the only run-resetting
/// construct that can later complete *without* a fresh trigger byte is a
/// failed `&`-entity decode, and `decode_entity` bounds an entity at 34 bytes
/// (`&` + 32-char name + `;`), far less than this gap.
const SYNTH_GAP: usize = 2048;

/// Synthetic boundary candidates for space-free text (`track` mode only).
///
/// [`is_boundary`] only proposes word starts after a single space or a
/// newline, so a huge run with no spaces (one giant word, entity soup) yields
/// zero candidates, the streaming commit cut pins at 0, and every append
/// re-renders the whole growing paragraph — O(n²). Inside a long run of
/// *inert* bytes (bytes the catch-all text arm consumed, plus completed
/// entities) we can propose extra cut points. Cutting there is prefix-stable
/// by construction — escaping and entity decoding are context-free — and every
/// construct that could span or restructure the cut is excluded:
///
/// * every construct opener (`` \&`<![$*_~ `` …) takes a non-catch-all arm and
///   so resets the run; the candidate sits >= [`SYNTH_GAP`] bytes past the
///   reset, beyond the bounded forward reach of a completable entity, and
///   open-ended constructs (emphasis, links, code, math, autolinks-at-EOF) are
///   already excluded via `unstable`/`earliest_open`/`pairs` in
///   [`compute_cut`] or consume the tail outright (no candidates inside);
/// * with GFM autolinks on, an email/`www.` autolink could otherwise reach
///   *back* across a mid-word cut (a future `@` binds the alnum run to its
///   left, and the suffix slice would probe position 0 as a link start that
///   the full render never probes) — so the candidate byte itself must not be
///   one an extended autolink can contain or start (alnum or `.+_-@`);
/// * whitespace never extends a run (space-bearing text already gets natural
///   candidates, and staying out of space runs keeps hard-break handling
///   whole).
fn synth_boundary(
    pos: usize,
    b: u8,
    gfm_autolinks: bool,
    inert_run_start: &mut usize,
    inert_run_end: usize,
    candidates: &mut Vec<usize>,
) {
    if pos != inert_run_end {
        *inert_run_start = pos; // a reset (or slice start) — new run begins here
    } else if pos - *inert_run_start >= SYNTH_GAP
        && (!gfm_autolinks
            || !(b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'_' | b'-' | b'@')))
    {
        candidates.push(pos);
        *inert_run_start = pos; // rate-limit: next candidate a full gap later
    }
}

/// Largest boundary candidate that is stable: at/before any unstable construct
/// (`unstable`), at/before any unpaired can-open emphasis opener (could pair
/// forward), and not strictly inside any resolved emphasis pair `(a, b]`.
fn compute_cut(candidates: &[usize], unstable: usize, stack: &[Delim], pairs: &[(usize, usize)]) -> usize {
    let mut earliest_open = usize::MAX;
    for d in stack {
        if d.len > 0 && d.can_open && d.in_at < earliest_open {
            earliest_open = d.in_at;
        }
    }
    let limit = unstable.min(earliest_open);
    // Candidates arrive in ascending scan order, so instead of testing every
    // candidate against every pair (O(candidates × pairs) — quadratic on
    // emphasis-dense text), sort the pairs by opener once and sweep: `c` is
    // strictly inside some pair `(a, b]` iff the max closer `b` over all pairs
    // with `a < c` is >= `c`.
    debug_assert!(candidates.windows(2).all(|w| w[0] <= w[1]));
    let order = crate::sort::stable_order(pairs, |x, y| x.0 <= y.0);
    let mut best = 0;
    let mut pi = 0;
    let mut max_b = 0usize;
    for &c in candidates {
        while pi < order.len() && pairs[order[pi]].0 < c {
            max_b = max_b.max(pairs[order[pi]].1);
            pi += 1;
        }
        if c > best && c <= limit && max_b < c {
            best = c;
        }
    }
    best
}

/// Max inline-nesting depth. Nested inline-component tags recurse through
/// [`render_inline_core`] (`<x>…<x>…` — one stack frame per level via
/// `write_inline_component`). Like the block renderer's `render::MAX_RENDER_DEPTH`,
/// this MUST be bounded or adversarial input (`"<x>".repeat(10_000)` with `x`
/// registered via `setInlineComponentTags`) overflows the WASM shadow stack — an
/// uncatchable trap that poisons the whole worker. 100 is far above any real
/// nesting and well under the 256 KB stack. (Link/image bracket nesting is
/// separately bounded by [`MAX_BRACKET_DEPTH`].)
const MAX_INLINE_DEPTH: usize = 100;

thread_local! {
    /// Live depth of the [`render_inline_core`] recursion (inline-component
    /// nesting). WASM is single-threaded so this is just a module global; a native
    /// multi-threaded host gets a correct per-thread counter.
    static INLINE_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Restores [`INLINE_DEPTH`] on every exit path of [`render_inline_core`].
struct InlineDepthGuard(usize);
impl Drop for InlineDepthGuard {
    fn drop(&mut self) {
        INLINE_DEPTH.with(|d| d.set(self.0));
    }
}

fn render_inline_core(input: &str, opts: &RenderOpts, out: &mut String, track: bool) -> usize {
    // Deterministic complexity probe (feature `perf_counters` only; compiled out
    // of every real build). Every byte entering the inline renderer is counted —
    // an incremental cache that stays armed but re-renders a growing region on
    // each append goes quadratic HERE while the slow-path scan counter stays
    // linear. See `brook_md_core::perf` and `tests/scaling.rs`.
    #[cfg(feature = "perf_counters")]
    crate::perf::add_render(input.len());
    // Depth guard: nested inline-component tags recurse here (via
    // write_inline_component). Past the cap, emit the remaining inner as escaped
    // text instead of descending another shadow-stack frame. No legitimate inline
    // content nests this deep.
    let depth = INLINE_DEPTH.with(|d| d.get());
    if depth >= MAX_INLINE_DEPTH {
        escape_html(input, out);
        return input.len();
    }
    INLINE_DEPTH.with(|d| d.set(depth + 1));
    let _inline_depth_guard = InlineDepthGuard(depth);
    let bytes = input.as_bytes();
    // Floor for the line-end whitespace trim in the break arms: this render's
    // own start in `out`, never the document's. A segment render therefore
    // cannot pop bytes an earlier segment froze (and never has to — see
    // [`trim_line_end_ws`]).
    let out_floor = out.len();
    let mut pos = 0;
    let mut deli_stack: Vec<Delim> = Vec::new();
    // Streaming boundary tracking (only populated when `track`).
    let mut candidates: Vec<usize> = Vec::new();
    let mut unstable = usize::MAX;
    // Inert-run tracking for synthetic boundary candidates (see
    // [`synth_boundary`]; only maintained when `track`).
    let mut inert_run_start = 0usize;
    let mut inert_run_end = 0usize;
    // Lazily-built index over the slice's `$` runs, shared by every
    // `try_dollar_math` probe in this render (see [`DollarIndex`]).
    let mut dollar_idx: Option<DollarIndex> = None;

    while pos < bytes.len() {
        if track && is_boundary(bytes, pos) {
            candidates.push(pos);
        }
        // GFM extended autolinks: bare www./http(s)://ftp:// URLs and email
        // addresses in text. Gated on a left boundary (start / whitespace /
        // `*_~(`) so we only probe at plausible starts.
        if opts.gfm_autolinks && ext_autolink_boundary(bytes, pos) {
            if matches!(bytes[pos], b'w' | b'h' | b'f') {
                if let Some(consumed) = try_ext_autolink(bytes, pos, out) {
                    pos = consumed;
                    continue;
                }
            }
            if bytes[pos].is_ascii_alphanumeric() || matches!(bytes[pos], b'.' | b'_' | b'+' | b'-') {
                if let Some(consumed) = try_ext_email(bytes, pos, out) {
                    pos = consumed;
                    continue;
                }
            }
        }
        let b = bytes[pos];
        match b {
            // LaTeX inline math `\(…\)` and inline display `\[…\]` (math on).
            // Probed before the generic backslash-escape arm because `(`/`[` are
            // escapable; if there's no matching closer we fall back to the
            // escape behavior (a literal `(` / `[`).
            b'\\' if opts.gfm_math && bytes.get(pos + 1) == Some(&b'(') => {
                match try_math_delim(bytes, pos, b"\\(", b"\\)", false, opts.open_tail, out) {
                    Some(end) => {
                        // Settled `\(…\)` OR (open_tail only) a speculative open
                        // inline-math span whose `\)` hasn't streamed yet. The
                        // speculative arm always returns `Some(bytes.len())`, so
                        // it is the final construct; mark it unstable.
                        if opts.open_tail && end == bytes.len() && track && pos < unstable {
                            unstable = pos;
                        }
                        pos = end;
                    }
                    None => {
                        if track && pos < unstable {
                            unstable = pos; // a later `\)` could form inline math
                        }
                        push_escaped(b'(', out);
                        pos += 2;
                    }
                }
            }
            b'\\' if opts.gfm_math && bytes.get(pos + 1) == Some(&b'[') => {
                match try_math_delim(bytes, pos, b"\\[", b"\\]", true, opts.open_tail, out) {
                    Some(end) => {
                        // Settled `\[…\]` OR (open_tail only) a speculative open
                        // display-math span whose `\]` hasn't streamed yet. The
                        // speculative arm always returns `Some(bytes.len())`, so
                        // it is the final construct; mark it unstable.
                        if opts.open_tail && end == bytes.len() && track && pos < unstable {
                            unstable = pos;
                        }
                        pos = end;
                    }
                    None => {
                        if track && pos < unstable {
                            unstable = pos; // a later `\]` could form display math
                        }
                        push_escaped(b'[', out);
                        pos += 2;
                    }
                }
            }
            b'\\' if pos + 1 < bytes.len() && ESCAPABLE.contains(&bytes[pos + 1]) => {
                push_escaped(bytes[pos + 1], out);
                pos += 2;
            }
            b'\\' if pos + 1 < bytes.len() && bytes[pos + 1] == b'\n' => {
                // Backslash hard break. Nothing to trim on the left (the line's
                // final character is the `\` itself), but the NEXT line's indent
                // is still stripped (example 637).
                out.push_str("<br>\n");
                pos = skip_line_indent(bytes, pos + 2);
            }
            b'&' => {
                if let Some((decoded, consumed)) = decode_entity(&bytes[pos..]) {
                    // A completed entity is inert for streaming purposes: it's
                    // consumed atomically and later input can't reopen it, so it
                    // extends the inert run (and, like the catch-all arm, may
                    // carry a synthetic boundary candidate at its `&`).
                    if track {
                        synth_boundary(pos, b'&', opts.gfm_autolinks, &mut inert_run_start, inert_run_end, &mut candidates);
                    }
                    for c in decoded.chars() {
                        push_escaped_char(c, out);
                    }
                    pos += consumed;
                    if track {
                        inert_run_end = pos;
                    }
                } else {
                    // Failed decode: a later `;` could still complete the
                    // entity, so the run does NOT extend across this `&` (any
                    // synthetic candidate must sit >= SYNTH_GAP past it).
                    out.push_str("&amp;");
                    pos += 1;
                }
            }
            b'`' => {
                if let Some(consumed) = try_code_span(bytes, pos, opts.open_tail, out) {
                    // Settled `` `code` `` OR (open_tail only) a speculative open
                    // code span: an unclosed backtick run abutting EOF rendered
                    // its partial content as `<code>…</code>`. The speculative
                    // arm always returns `Some(bytes.len())`, so it is the final
                    // construct in the slice; mark it unstable so the streaming
                    // boundary tracker never freezes it (a later closing run can
                    // still change the content / shorten the span).
                    if opts.open_tail && consumed == bytes.len() && track && pos < unstable {
                        unstable = pos;
                    }
                    pos = consumed;
                } else {
                    // No matching close for this backtick run: the whole run is
                    // literal. Emit all of it and advance past it, so its inner
                    // backticks aren't re-tried as a shorter opening run.
                    // Unstable: a closer could still arrive and form a code span.
                    if track && pos < unstable {
                        unstable = pos;
                    }
                    let mut run = 0;
                    while pos + run < bytes.len() && bytes[pos + run] == b'`' {
                        run += 1;
                    }
                    for _ in 0..run {
                        out.push('`');
                    }
                    pos += run;
                }
            }
            b'<' => {
                if let Some(consumed) = try_autolink(bytes, pos, out, &opts.allow_schemes) {
                    pos = consumed;
                } else {
                    match try_inline_component(input, bytes, pos, opts, out) {
                        InlineComp::Done(end) => pos = end,
                        InlineComp::Incomplete => {
                            // An allowlisted inline open tag with no matching
                            // close yet: keep it inert (escape the `<`) and
                            // re-tryable — a later `</tag>` can still form the
                            // component, and if none ever arrives it degrades to
                            // escaped text (never eats following content).
                            if track && pos < unstable {
                                unstable = pos;
                            }
                            out.push_str("&lt;");
                            pos += 1;
                        }
                        InlineComp::NotComponent => {
                            if let Some(consumed) = try_inline_html(bytes, pos, opts, out) {
                                pos = consumed;
                            } else if opts.open_tail
                                && (opts.html_sanitize || opts.unsafe_html)
                                && inline_html_streams_to_eof(bytes, pos)
                            {
                                // A raw-HTML token / autolink still streaming to
                                // EOF (`<a href="https://…` with no `>` yet, a
                                // partial `<https://…` autolink, `</ta`, `<!--`
                                // without its terminator): emit NOTHING. Its
                                // visible content is markup internals — flashing
                                // them (URL included) is the raw-HTML twin of
                                // the link-URL flash the speculative label path
                                // kills. Finalize (open_tail=false) renders it
                                // literally, byte-par with the one-shot oracle.
                                // GATED on raw HTML being engaged (sanitize or
                                // unsafe): in escape mode the completed token IS
                                // visible literal text, so hiding it mid-stream
                                // would pop-in real content, not prevent a leak.
                                // Unstable: the next byte settles or extends it.
                                if track && pos < unstable {
                                    unstable = pos;
                                }
                                pos = bytes.len();
                            } else {
                                // Unstable: a later `>` could form an autolink / inline HTML.
                                if track && pos < unstable {
                                    unstable = pos;
                                }
                                out.push_str("&lt;");
                                pos += 1;
                            }
                        }
                    }
                }
            }
            b'!' if pos + 1 < bytes.len() && bytes[pos + 1] == b'[' => {
                if let Some(consumed) = try_image(bytes, pos, opts, out) {
                    pos = consumed;
                } else {
                    out.push('!');
                    pos += 1;
                }
            }
            b'[' => {
                if !opts.in_link {
                    let fnref = if opts.gfm_footnotes {
                        try_footnote_ref(bytes, pos, opts, out)
                    } else {
                        None
                    };
                    if let Some(consumed) = fnref {
                        // A ref whose `]` abuts the slice end is only
                        // PROVISIONALLY a ref: the very next byte decides ref
                        // vs. def opener (`[^x]:`), so a streaming boundary
                        // must never settle (freeze) it mid-stream.
                        if track && consumed == bytes.len() && pos < unstable {
                            unstable = pos;
                        }
                        pos = consumed;
                    } else if opts.open_tail
                        && !opts.in_link
                        && speculative_link_tail(bytes, pos, opts, out).is_some()
                    {
                        // Speculative open-tail link: a link still streaming to
                        // EOF — open label `[Earnings Ca`, `]` at EOF, ref name
                        // `[t][re`, or destination/title `[t](https://exa` —
                        // rendered an INERT pending `<a>` (no href,
                        // data-brook-pending). Unstable — the next byte (a `)`,
                        // a hard terminator, a def-opener `:`…) can change it.
                        // `speculative_link_tail` only returns Some when it ran
                        // to bytes.len(), so this is always the final construct
                        // in the slice.
                        if track && pos < unstable {
                            unstable = pos;
                        }
                        pos = bytes.len();
                    } else if let Some(consumed) = try_link(bytes, pos, opts, out) {
                        // Settled: an inline `[text](url)` or a reference resolved
                        // via `opts.refs`. (Safe to treat resolved refs as settled
                        // because an *open paragraph* — the only block this cache
                        // serves — defines no reference definitions of its own, and
                        // first-definition-wins makes later doc defs non-overriding.)
                        pos = consumed;
                    } else {
                        // Literal `[`: a later `](url)` or `[ref]` (or a forward
                        // `[ref]: …` definition) could still turn it into a link.
                        if track && pos < unstable {
                            unstable = pos;
                        }
                        out.push('[');
                        pos += 1;
                    }
                } else {
                    if track && pos < unstable {
                        unstable = pos;
                    }
                    out.push('[');
                    pos += 1;
                }
            }
            b'$' if opts.gfm_math => {
                match try_dollar_math(bytes, pos, opts.open_tail, &mut dollar_idx, out) {
                    Some(end) => {
                        // Settled `$x$` / `$$…$$` OR (open_tail only) a speculative
                        // open dollar-math span whose closer hasn't streamed yet:
                        // the partial body rendered as `<span class="math …">`.
                        // The speculative arm always returns `Some(bytes.len())`,
                        // so it is the final construct in the slice; mark it
                        // unstable so the boundary tracker never freezes it.
                        if opts.open_tail && end == bytes.len() && track && pos < unstable {
                            unstable = pos;
                        }
                        pos = end;
                    }
                    None => {
                        if track && pos < unstable {
                            unstable = pos; // a later `$` could form inline math
                        }
                        out.push('$');
                        pos += 1;
                    }
                }
            }
            b'*' | b'_' | b'~' => {
                let run = scan_delim_run(bytes, pos);
                let len = run.len;
                let class = b;
                let (can_open, can_close) = flanking(input, pos, len);
                let written_at = out.len();
                for _ in 0..len {
                    out.push(class as char);
                }
                deli_stack.push(Delim { at: written_at, in_at: pos, class, len, can_open, can_close });
                pos += len;
            }
            b' ' if pos + 1 < bytes.len() && bytes[pos + 1] == b' ' && trailing_spaces_before_nl(bytes, pos) => {
                // CommonMark hard break: 2+ trailing spaces before \n.
                let mut k = pos;
                while k < bytes.len() && bytes[k] == b' ' {
                    k += 1;
                }
                if k < bytes.len() && bytes[k] == b'\n' {
                    // The break's own spaces are consumed here; any whitespace
                    // LEFT of them is this line's remaining final whitespace and
                    // is not content either (`"foo \t  \n"` → `foo<br>`).
                    trim_line_end_ws(out, out_floor);
                    out.push_str("<br>\n");
                    pos = skip_line_indent(bytes, k + 1);
                } else {
                    out.push(' ');
                    pos += 1;
                }
            }
            b'\n' => {
                // Composition order (each step reads the state the previous one
                // left): the hard-break lookbehind decides FIRST — it is the one
                // decision made on rendered output, and 2+ SOURCE spaces have
                // already been intercepted by the arm above — then the line's
                // final spaces/tabs go, then the break itself, then the next
                // line's leading indent.
                let hard = out.ends_with("  ");
                if hard {
                    out.truncate(out.len() - 2);
                }
                trim_line_end_ws(out, out_floor);
                if hard || opts.soft_breaks {
                    // Soft-break mode: a soft break renders as a visible
                    // line break. The `\n` still follows the tag so the emitted
                    // markup keeps its line structure (and byte-parity with the
                    // hard-break arm above).
                    out.push_str("<br>\n");
                } else {
                    out.push('\n');
                }
                pos = skip_line_indent(bytes, pos + 1);
            }
            b'\r' => {
                pos += 1;
            }
            _ => {
                // Bulk plain-text fast path (non-tracking renders only, so the
                // streaming-boundary bookkeeping below stays byte-exact): every
                // byte outside INLINE_TEXT_STOP renders as itself, so a whole
                // run of them is copied with one push_str instead of a per-byte
                // match dispatch + escape call. `end` always lands on an ASCII
                // stop byte or EOF, so the slice stays on a char boundary.
                if !track && !INLINE_TEXT_STOP[b as usize] {
                    let mut end = pos + 1;
                    while end < bytes.len() && !INLINE_TEXT_STOP[bytes[end] as usize] {
                        end += 1;
                    }
                    out.push_str(&input[pos..end]);
                    pos = end;
                    continue;
                }
                // Plain text. Non-whitespace bytes extend the inert run and may
                // carry a synthetic boundary candidate (see `synth_boundary`);
                // spaces/tabs never extend a run — space-bearing text gets its
                // natural `is_boundary` candidates instead.
                let ws = matches!(b, b' ' | b'\t');
                if track && !ws {
                    synth_boundary(pos, b, opts.gfm_autolinks, &mut inert_run_start, inert_run_end, &mut candidates);
                }
                if b < 0x80 {
                    push_escaped(b, out);
                    pos += 1;
                } else if let Some(c) = input[pos..].chars().next() {
                    push_escaped_char(c, out);
                    pos += c.len_utf8();
                } else {
                    pos += 1;
                }
                if track && !ws {
                    inert_run_end = pos;
                }
            }
        }
    }

    let mut pairs: Vec<(usize, usize)> = Vec::new();
    resolve_delimiters(out, &mut deli_stack, if track { Some(&mut pairs) } else { None });

    if track {
        compute_cut(&candidates, unstable, &deli_stack, &pairs)
    } else {
        0
    }
}

/// GFM §6.9: an extended autolink may begin at the start of the line or after
/// whitespace or one of `*`, `_`, `~`, `(`.
fn ext_autolink_boundary(bytes: &[u8], pos: usize) -> bool {
    pos == 0 || matches!(bytes[pos - 1], b' ' | b'\t' | b'\n' | b'\r' | b'*' | b'_' | b'~' | b'(')
}

/// Try to match a GFM extended URL autolink (`www.`, `http://`, `https://`,
/// `ftp://`) at `pos`, emitting `<a …>…</a>` on success and returning the byte
/// offset just past it. Applies GFM's trailing-punctuation, balanced-paren and
/// entity-reference trimming rules.
fn try_ext_autolink(bytes: &[u8], start: usize, out: &mut String) -> Option<usize> {
    let rest = &bytes[start..];
    let scheme_prefix = if rest.starts_with(b"http://")
        || rest.starts_with(b"https://")
        || rest.starts_with(b"ftp://")
    {
        ""
    } else if rest.starts_with(b"www.") {
        "http://"
    } else {
        return None;
    };

    // Consume up to the next whitespace or `<` (which truncates the link).
    let mut end = start;
    while end < bytes.len() && !matches!(bytes[end], b' ' | b'\t' | b'\n' | b'\r' | b'<') {
        end += 1;
    }

    // Trailing-punctuation trimming (applied repeatedly). Count parens ONCE
    // up front and keep the running counts in sync as we trim, instead of
    // recounting the whole span every iteration (which is O(n²)).
    let mut opens = bytes[start..end].iter().filter(|&&b| b == b'(').count();
    let mut closes = bytes[start..end].iter().filter(|&&b| b == b')').count();
    loop {
        if end <= start {
            return None;
        }
        let last = bytes[end - 1];
        if matches!(last, b'?' | b'!' | b'.' | b',' | b':' | b'*' | b'_' | b'~') {
            // These never affect the paren counts.
            end -= 1;
            continue;
        }
        if last == b')' {
            // Trim an unbalanced trailing `)` and decrement the cached count.
            if closes > opens {
                end -= 1;
                closes -= 1;
                continue;
            }
        }
        if last == b';' {
            // Looks like a trailing entity reference `&name;`? Trim it.
            if let Some(amp) = bytes[start..end].iter().rposition(|&b| b == b'&') {
                let amp = start + amp;
                if end - 1 > amp + 1
                    && bytes[amp + 1..end - 1].iter().all(|b| b.is_ascii_alphanumeric())
                {
                    end = amp;
                    // The jumped-over span `[amp, prev end)` was all alnum + `&`
                    // + `;` — no parens — but recompute once to stay exact and
                    // robust to future edits (this branch is non-looping).
                    opens = bytes[start..end].iter().filter(|&&b| b == b'(').count();
                    closes = bytes[start..end].iter().filter(|&&b| b == b')').count();
                    continue;
                }
            }
        }
        break;
    }

    let text = std::str::from_utf8(&bytes[start..end]).ok()?;
    if !valid_autolink_domain(text, scheme_prefix) {
        return None;
    }

    out.push_str("<a href=\"");
    let mut href = String::with_capacity(scheme_prefix.len() + text.len());
    href.push_str(scheme_prefix);
    href.push_str(text);
    escape_attr(&href, out);
    out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">");
    escape_html(text, out);
    out.push_str("</a>");
    Some(end)
}

/// Try to match a GFM extended email autolink (`local@domain`) at `pos`. The
/// local part allows `.+_-` and alphanumerics; the domain allows `._-` and
/// alphanumerics, must contain a `.`, and may not end in `.`, `-` or `_`
/// (those are excluded from the link).
fn try_ext_email(bytes: &[u8], start: usize, out: &mut String) -> Option<usize> {
    let mut i = start;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'+' | b'_' | b'-')) {
        i += 1;
    }
    if i == start || bytes.get(i) != Some(&b'@') {
        return None;
    }
    let at = i;
    i += 1;
    let domain_start = i;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'.' | b'_' | b'-')) {
        i += 1;
    }
    // A trailing `.` is punctuation, not part of the address; strip it.
    let mut end = i;
    while end > domain_start && bytes[end - 1] == b'.' {
        end -= 1;
    }
    // But a domain ending in `-` or `_` is invalid outright (not a link).
    if end > domain_start && matches!(bytes[end - 1], b'-' | b'_') {
        return None;
    }
    let domain = std::str::from_utf8(&bytes[domain_start..end]).ok()?;
    // Domain: ≥1 dot, non-empty labels of alnum/`-`/`_`.
    if !domain.contains('.') {
        return None;
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.iter().any(|l| l.is_empty())
        || !labels.iter().all(|l| l.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
    {
        return None;
    }
    let text = std::str::from_utf8(&bytes[start..end]).ok()?;
    out.push_str("<a href=\"mailto:");
    escape_attr(text, out);
    out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">");
    escape_html(text, out);
    out.push_str("</a>");
    let _ = at;
    Some(end)
}

/// Validate the domain of an extended autolink: the authority (up to the first
/// `/`, `?`, or `#`) must contain at least one `.` separating non-empty,
/// dash/underscore/alnum segments, and the last segment must not be empty.
fn valid_autolink_domain(text: &str, scheme_prefix: &str) -> bool {
    let after_scheme = if scheme_prefix.is_empty() {
        // Skip the literal scheme in the text (`http://`, etc.).
        match text.find("://") {
            Some(i) => &text[i + 3..],
            None => return false,
        }
    } else {
        text
    };
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if !authority.contains('.') {
        return false;
    }
    let labels: Vec<&str> = authority.split('.').collect();
    if labels.iter().any(|l| l.is_empty()) {
        return false;
    }
    labels
        .iter()
        .all(|l| l.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')))
}

fn trailing_spaces_before_nl(bytes: &[u8], start: usize) -> bool {
    let mut k = start;
    while k < bytes.len() && bytes[k] == b' ' {
        k += 1;
    }
    k - start >= 2 && k < bytes.len() && bytes[k] == b'\n'
}

/// Bytes that terminate a plain-text bulk run in `render_inline_core`: every
/// match-arm dispatch byte (`\ & ` < ! [ $ * _ ~ space \n \r`), the bytes the
/// catch-all arm HTML-escapes (`>` `"`), and the remaining bytes that make the
/// NEXT position a GFM extended-autolink boundary (`\t` `(` — the others
/// already dispatch). Everything else — including every non-ASCII byte —
/// renders byte-for-byte as itself.
static INLINE_TEXT_STOP: [bool; 256] = {
    let mut t = [false; 256];
    let stops = b"\t\n\r !\"$&(*<>[\\_`~";
    let mut i = 0;
    while i < stops.len() {
        t[stops[i] as usize] = true;
        i += 1;
    }
    t
};

fn push_escaped(b: u8, out: &mut String) {
    match b {
        b'<' => out.push_str("&lt;"),
        b'>' => out.push_str("&gt;"),
        b'&' => out.push_str("&amp;"),
        b'"' => out.push_str("&quot;"),
        _ => out.push(b as char),
    }
}

fn push_escaped_char(c: char, out: &mut String) {
    match c {
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '&' => out.push_str("&amp;"),
        '"' => out.push_str("&quot;"),
        _ => out.push(c),
    }
}

// ---------------------------------------------------------------------
// Code spans
// ---------------------------------------------------------------------

/// Emit `<code>…</code>` for the raw code-span body `content`, applying
/// CommonMark §6.1 normalization (line endings → spaces; strip one leading +
/// trailing space when the result starts AND ends with a space and is not all
/// spaces) and HTML-escaping. Shared by the settled closer and the speculative
/// open-tail path so both produce byte-identical inner markup.
fn emit_code_span(content: &[u8], out: &mut String) {
    let s = std::str::from_utf8(content).unwrap_or("");
    let mut buf = String::with_capacity(s.len());
    for c in s.chars() {
        if c == '\n' || c == '\r' {
            buf.push(' ');
        } else {
            buf.push(c);
        }
    }
    let trimmed = if buf.len() >= 2
        && buf.starts_with(' ')
        && buf.ends_with(' ')
        && buf.chars().any(|c| c != ' ')
    {
        &buf[1..buf.len() - 1]
    } else {
        buf.as_str()
    };
    out.push_str("<code>");
    for c in trimmed.chars() {
        push_escaped_char(c, out);
    }
    out.push_str("</code>");
}

fn try_code_span(bytes: &[u8], start: usize, open_tail: bool, out: &mut String) -> Option<usize> {
    let mut open_len = 0;
    while start + open_len < bytes.len() && bytes[start + open_len] == b'`' {
        open_len += 1;
    }
    let mut i = start + open_len;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let mut close_len = 0;
            while i + close_len < bytes.len() && bytes[i + close_len] == b'`' {
                close_len += 1;
            }
            if close_len == open_len {
                emit_code_span(&bytes[start + open_len..i], out);
                return Some(i + close_len);
            }
            i += close_len;
        } else {
            i += 1;
        }
    }
    // EOF reached with no matching close run. When this is the still-open,
    // abuts-EOF active tail (open_tail), speculate: render the partial content
    // (everything after the opening run) as a `<code>` span so a streaming
    // consumer sees the resolved span instead of a flash of raw backticks +
    // source. The opening run is hidden inside the tag. Code spans match by
    // EXACT run length, so a half-streamed run that can't yet match still falls
    // to literal at finalize (open_tail=false) — byte-parity with one-shot.
    // Don't speculate an empty body (preserves the literal `` ` `` rule).
    if open_tail {
        let content = &bytes[start + open_len..];
        if content.is_empty() {
            return None;
        }
        emit_code_span(content, out);
        return Some(bytes.len());
    }
    None
}

#[allow(dead_code)]
fn trim_code_span(s: &[u8]) -> &[u8] {
    if s.len() >= 2 && s[0] == b' ' && s[s.len() - 1] == b' ' && s.iter().any(|&b| b != b' ') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

// ---------------------------------------------------------------------
// Math (gated on opts.gfm_math)
// ---------------------------------------------------------------------

/// Emit `<span class="math math-(inline|display)">…</span>` carrying the
/// HTML-escaped LaTeX. The body is never markdown-processed; KaTeX (or a
/// `components.MathBlock`-style override) reads the LaTeX from text content.
fn emit_inline_math(content: &[u8], display: bool, out: &mut String) {
    let s = std::str::from_utf8(content).unwrap_or("");
    out.push_str(if display {
        "<span class=\"math math-display\">"
    } else {
        "<span class=\"math math-inline\">"
    });
    escape_html(s, out);
    out.push_str("</span>");
}

/// LaTeX-delimited inline math: `\(…\)` (inline) or `\[…\]` (display). Returns
/// the offset just past the closing delimiter, or None (no closer / empty body)
/// so the caller can fall back to literal output. The body may span soft line
/// breaks but not a blank line (which ends the paragraph).
fn try_math_delim(
    bytes: &[u8],
    start: usize,
    open: &[u8],
    close: &[u8],
    display: bool,
    open_tail: bool,
    out: &mut String,
) -> Option<usize> {
    let content_start = start + open.len();
    let mut i = content_start;
    while i < bytes.len() {
        if bytes[i..].starts_with(close) {
            if i == content_start {
                return None; // empty `\(\)` — leave literal
            }
            emit_inline_math(&bytes[content_start..i], display, out);
            return Some(i + close.len());
        }
        if bytes[i] == b'\n' && bytes.get(i + 1) == Some(&b'\n') {
            return None; // never cross a blank line (math stays in its paragraph)
        }
        i += 1;
    }
    // EOF reached with no closing delimiter and no intervening blank line. On the
    // still-open abuts-EOF active tail, speculate: render the partial body as the
    // final `<span class="math …">` so a streaming consumer never sees the raw
    // `\(`/`\[` opener + LaTeX source flash. The blank-line guard above already
    // returned None for a `\n\n` before EOF, so the speculation matches the
    // one-shot rule that math never crosses a paragraph break. Don't speculate an
    // empty body (preserves the literal `\(\)` rule). At finalize (open_tail=false)
    // this is dead → existing None → literal, byte-parity with one-shot.
    if open_tail {
        let content = &bytes[content_start..];
        if content.is_empty() {
            return None;
        }
        // The `\)`/`\]` closer starts with `\`. If the partial body ends with a
        // lone (unescaped) `\`, that byte is the first byte of the still-unstreamed
        // closer, not real math — trimming it stops a one-keystroke KaTeX error on
        // `\(a+b\`. Guard against trimming an escaped `\\` (LaTeX line break) and a
        // closer that doesn't start with `\` (it always does here).
        let trim_tail = close.first() == Some(&b'\\')
            && content.last() == Some(&b'\\')
            && (content.len() < 2 || content[content.len() - 2] != b'\\');
        let content = if trim_tail {
            &content[..content.len() - 1]
        } else {
            content
        };
        if content.is_empty() {
            return None;
        }
        emit_inline_math(content, display, out);
        return Some(bytes.len());
    }
    None
}

/// Lazily-built per-slice index over `$` runs for [`try_dollar_math`]. Without
/// it, every unmatched `$` opener re-scans to EOF looking for a valid closer —
/// O(n²) inside a single render on `$`-dense text. Closer validity is
/// position-local (it depends only on the bytes around the closer's run), so
/// one O(n) pass records every run plus, per run class, the next valid closer
/// at/after each run; every opener probe is then O(log runs).
struct DollarIndex {
    /// `(start, len)` of every maximal `$` run, ascending.
    runs: Vec<(usize, usize)>,
    /// `runs[next1[k]]` is the first run at index >= `k` that can close a
    /// single-`$` span (non-space immediately left, no ASCII digit after the
    /// consumed `$`). `usize::MAX` when none.
    next1: Vec<usize>,
    /// Same for `$$` closers (any run of len >= 2, no validity checks).
    next2: Vec<usize>,
    /// Ascending positions `b` with `bytes[b] == '\n' && bytes[b+1] == '\n'`
    /// (the never-cross-a-blank-line barrier).
    blanks: Vec<usize>,
}

fn build_dollar_index(bytes: &[u8]) -> DollarIndex {
    let mut runs = Vec::new();
    let mut blanks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let start = i;
            while i < bytes.len() && bytes[i] == b'$' {
                i += 1;
            }
            runs.push((start, i - start));
        } else {
            if bytes[i] == b'\n' && bytes.get(i + 1) == Some(&b'\n') {
                blanks.push(i);
            }
            i += 1;
        }
    }
    let mut next1 = vec![usize::MAX; runs.len()];
    let mut next2 = vec![usize::MAX; runs.len()];
    let (mut n1, mut n2) = (usize::MAX, usize::MAX);
    for k in (0..runs.len()).rev() {
        let (pos, len) = runs[k];
        // pandoc single-`$` closer rule: a non-space immediately to the left,
        // and the byte after the consumed `$` (the run's own 2nd `$` when
        // len >= 2) not an ASCII digit.
        if pos > 0
            && !matches!(bytes[pos - 1], b' ' | b'\t' | b'\n' | b'\r')
            && !bytes.get(pos + 1).is_some_and(|b| b.is_ascii_digit())
        {
            n1 = k;
        }
        if len >= 2 {
            n2 = k;
        }
        next1[k] = n1;
        next2[k] = n2;
    }
    DollarIndex { runs, next1, next2, blanks }
}

/// Dollar-delimited inline math: `$…$` (inline) or `$$…$$` (display). Uses the
/// pandoc disambiguation rule for single `$` so currency text stays literal:
/// the opener must be followed by a non-space, and a closer is only valid when
/// preceded by a non-space and not followed by an ASCII digit. Returns the
/// offset past the closing run, or None to fall back to a literal `$`.
/// `idx` caches the slice's `$`-run index across probes (see [`DollarIndex`]).
fn try_dollar_math(
    bytes: &[u8],
    start: usize,
    open_tail: bool,
    idx: &mut Option<DollarIndex>,
    out: &mut String,
) -> Option<usize> {
    let mut run = 0;
    while start + run < bytes.len() && bytes[start + run] == b'$' {
        run += 1;
    }
    let display = run >= 2;
    let n = if display { 2 } else { 1 };
    let content_start = start + n;
    if !display {
        // Opener must have a non-space, non-EOL char to its right.
        match bytes.get(content_start) {
            None => return None,
            Some(&c) if matches!(c, b' ' | b'\t' | b'\n' | b'\r') => return None,
            _ => {}
        }
    }
    // When the opener's own run has leftover `$`s (content_start is mid-run),
    // the byte scan this replaces either bailed on empty content (leftover
    // >= n) or stepped past the leftover — replicate that, so every closer
    // candidate below is a true run start covered by the index.
    let mut scan_from = content_start;
    if bytes.get(content_start) == Some(&b'$') {
        let mut clen = 0;
        while content_start + clen < bytes.len() && bytes[content_start + clen] == b'$' {
            clen += 1;
        }
        if clen >= n {
            return None; // empty `$$` / `$ $`-style — leave literal
        }
        scan_from = content_start + clen;
    }
    let idx = idx.get_or_insert_with(|| build_dollar_index(bytes));
    let k = idx.runs.partition_point(|&(p, _)| p < scan_from);
    let j = if display {
        idx.next2.get(k).copied().unwrap_or(usize::MAX)
    } else {
        idx.next1.get(k).copied().unwrap_or(usize::MAX)
    };
    let bi = idx.blanks.partition_point(|&b| b < scan_from);
    let barrier = idx.blanks.get(bi).copied().unwrap_or(usize::MAX);
    if j != usize::MAX {
        let content_end = idx.runs[j].0;
        if barrier < content_end {
            return None; // don't cross a blank line
        }
        // content_end > content_start always holds here (a run at
        // content_start was handled above), so the body is non-empty.
        emit_inline_math(&bytes[content_start..content_end], display, out);
        return Some(content_end + n);
    }
    if barrier != usize::MAX {
        return None; // don't cross a blank line
    }
    // EOF reached with no closing `$`/`$$` run and no intervening blank line. On
    // the still-open abuts-EOF active tail, speculate: render the partial body as
    // the final `<span class="math …">` so a streaming `$x^2 + y^2$` never flashes
    // the raw `$` + source. The pandoc opener guard (single `$` needs a non-space
    // to its right) already ran above — `$ ` returned None → literal, unchanged.
    // The non-empty check preserves the literal empty-`$$` rule; closer-side
    // pandoc checks don't apply (there is no closer in the open tail). At finalize
    // (open_tail=false) this is dead → existing None → literal (byte-parity).
    if open_tail {
        let content = &bytes[content_start..];
        if content.is_empty() {
            return None;
        }
        emit_inline_math(content, display, out);
        return Some(bytes.len());
    }
    None
}

// ---------------------------------------------------------------------
// Autolinks
// ---------------------------------------------------------------------

fn try_autolink(
    bytes: &[u8],
    start: usize,
    out: &mut String,
    allow_schemes: &[Box<str>],
) -> Option<usize> {
    let end = bytes[start..].iter().position(|&b| b == b'>')? + start;
    let inner = &bytes[start + 1..end];
    if inner.is_empty() {
        return None;
    }
    if inner.iter().any(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'<' | b'\0')) {
        return None;
    }
    // CommonMark §6.4: autolinks do NOT process backslash escapes — the
    // presence of a backslash means it's a literal char in the URL, which
    // makes the whole thing invalid as an email autolink at minimum.
    let s = std::str::from_utf8(inner).ok()?;
    let is_uri = is_uri_scheme(s);
    let is_email = !is_uri && is_email_like(s) && !s.contains('\\');
    if !is_uri && !is_email {
        return None;
    }
    out.push_str("<a href=\"");
    if is_email {
        out.push_str("mailto:");
        // Email autolinks: just escape the chars, no percent-encoding for ASCII.
        crate::url::escape_attr(s, out);
    } else {
        // URI autolinks allow any valid scheme per CommonMark, but a dangerous
        // one (`javascript:`, `vbscript:`, `data:text/html`, …) is XSS when the
        // output is injected via innerHTML — route it through the same
        // dangerous-scheme filter as regular links so the href becomes `#`. The
        // visible link TEXT below is unaffected (still HTML-escaped verbatim).
        let decoded = crate::url::decode_text(s);
        if crate::url::is_dangerous_href_scheme(&decoded, allow_schemes) {
            out.push('#');
        } else {
            // Backslash escapes are NOT processed in autolinks; percent-encode
            // only unsafe chars.
            let normalized = autolink_normalize(s);
            crate::url::escape_attr(&normalized, out);
        }
    }
    out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">");
    for c in s.chars() {
        push_escaped_char(c, out);
    }
    out.push_str("</a>");
    Some(end + 1)
}

fn autolink_normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if is_autolink_safe(c) {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for &b in c.encode_utf8(&mut buf).as_bytes() {
                out.push('%');
                out.push(hex_digit(b >> 4));
                out.push(hex_digit(b & 0xF));
            }
        }
    }
    out
}

fn is_autolink_safe(c: char) -> bool {
    // CommonMark percent-encodes `[`, `]`, and `\` in autolinks even though
    // they're "reserved" in URI grammar — to avoid ambiguity with link
    // brackets and escapes in the surrounding markdown.
    matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9'
        | '-' | '_' | '.' | '~'
        | '!' | '*' | '\'' | '(' | ')' | ';' | ':' | '@' | '&'
        | '=' | '+' | '$' | ',' | '/' | '?' | '#'
        | '%')
}

fn hex_digit(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'A' + n - 10) as char,
    }
}

fn is_uri_scheme(s: &str) -> bool {
    // CommonMark autolink: scheme + ":" + non-whitespace.
    let bytes = s.as_bytes();
    let colon = match bytes.iter().position(|&b| b == b':') {
        Some(i) => i,
        None => return false,
    };
    if colon < 2 || colon > 32 {
        return false;
    }
    if !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    for &b in &bytes[1..colon] {
        if !(b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.') {
            return false;
        }
    }
    true
}

fn is_email_like(s: &str) -> bool {
    let bytes = s.as_bytes();
    let at = match bytes.iter().position(|&b| b == b'@') {
        Some(i) if i > 0 && i < bytes.len() - 1 => i,
        _ => return false,
    };
    let _ = at;
    // crude: any non-space and includes one @ and one .
    s.chars().all(|c| !c.is_whitespace()) && s.contains('.')
}

// ---------------------------------------------------------------------
// Inline raw HTML
// ---------------------------------------------------------------------

/// Tags that are NEVER rendered in allow-all sanitize mode — script/exec/resource
/// vectors and content-as-raw-text elements. (In restrict mode they're simply not
/// in the allowlist, so they escape; this set is what "allow all" carves out.)
const DANGEROUS_HTML_TAGS: &[&[u8]] = &[
    b"script", b"style", b"iframe", b"object", b"embed", b"base", b"link", b"meta",
    b"noscript", b"template", b"title", b"textarea", b"form", b"input", b"button",
    b"select", b"option", b"frame", b"frameset", b"applet", b"svg", b"math",
    b"audio", b"video", b"source", b"track", b"canvas",
    // Raw-text / escapable-raw-text elements: a browser treats everything after
    // them as unparsed text, so rendering one corrupts the rest of the DOM (and
    // `<plaintext>` is unclosable). Drop them in allow-all mode.
    b"xmp", b"plaintext", b"noembed", b"noframes", b"listing",
];

/// HTML void elements: they have no end tag, so a rendered one never enters the
/// block sanitizer's open-element stack and never earns a speculative closer
/// (see `render::fold_block_html`).
pub(crate) const VOID_HTML_TAGS: &[&[u8]] = &[
    b"area", b"base", b"br", b"col", b"embed", b"hr", b"img", b"input", b"link",
    b"meta", b"param", b"source", b"track", b"wbr",
];

/// Emit an inline raw-HTML token verbatim (`unsafe_html` pass-through),
/// routing through the GFM tagfilter when it's on — so a disallowed inline
/// `<title>` / `<script …>` renders as text (GFM spec §6.11).
fn push_raw_inline_html(token: &str, opts: &RenderOpts, out: &mut String) {
    if opts.gfm_tagfilter {
        push_tagfiltered(token, out);
    } else {
        out.push_str(token);
    }
}

fn try_inline_html(bytes: &[u8], start: usize, opts: &RenderOpts, out: &mut String) -> Option<usize> {
    let consumed = match_inline_html(bytes, start)?;
    let token = &bytes[start..start + consumed];

    // HTML comments have no visible representation: drop them. The one exception
    // is bare `unsafe_html` pass-through (no sanitizer engaged), which keeps them
    // verbatim for CommonMark fidelity — a browser ignores them either way.
    if token.starts_with(b"<!--") {
        if opts.unsafe_html && !opts.html_sanitize {
            push_raw_inline_html(std::str::from_utf8(token).ok()?, opts, out);
        }
        return Some(start + consumed);
    }

    if opts.html_sanitize {
        sanitize_html_token(token, opts.html_policy(), out);
        return Some(start + consumed);
    }

    if opts.unsafe_html {
        push_raw_inline_html(std::str::from_utf8(token).ok()?, opts, out);
    } else {
        // Escape it.
        for &b in token {
            push_escaped(b, out);
        }
    }
    Some(start + consumed)
}

/// Extract a tag's name range from a matched inline-HTML token, plus whether
/// it's a close tag. `None` for non-tag tokens (PI / CDATA / declaration /
/// malformed). The range is over ASCII bytes, so it is a valid `&str` boundary.
fn inline_tag_name(token: &[u8]) -> Option<(core::ops::Range<usize>, bool)> {
    if token.first() != Some(&b'<') {
        return None;
    }
    let is_close = token.get(1) == Some(&b'/');
    let name_start = if is_close { 2 } else { 1 };
    let mut i = name_start;
    while i < token.len() && (token[i].is_ascii_alphanumeric() || matches!(token[i], b'-' | b':')) {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    Some((name_start..i, is_close))
}

/// What the sanitizer did with one complete raw-HTML token. The inline path
/// ignores this; the BLOCK path needs the outcome (and the tag's identity) to
/// maintain its open-element stack.
pub(crate) enum SanitizedTag<'a> {
    /// Nothing emitted: a comment / PI / CDATA / declaration / malformed token,
    /// a drop-list tag, or a tag from the non-overridable dangerous set.
    Dropped,
    /// Emitted as escaped literal text (a non-empty allowlist didn't list it).
    Escaped,
    /// Emitted as a real element. `name` is the SOURCE spelling (HTML tag names
    /// are case-insensitive; callers match with `eq_ignore_ascii_case`). `void`
    /// folds together "this element has no end tag" and "the author self-closed
    /// it" — either way it never enters an open-element stack.
    Rendered { name: &'a str, close: bool, void: bool },
}

/// Route ONE complete raw-HTML token through the safe sanitizer
/// (`html_sanitize`). This is the single tag decision path, shared by inline raw
/// HTML ([`try_inline_html`]) and block-level raw HTML
/// (`render::fold_block_html`) so the two can never drift: drop-list tags and
/// (non-overridably) dangerous tags are removed, allowlisted tags render
/// natively with sanitized attributes, everything else is escaped.
pub(crate) fn sanitize_html_token<'a>(
    token: &'a [u8],
    policy: HtmlPolicy<'_>,
    out: &mut String,
) -> SanitizedTag<'a> {
    let Some((name_range, is_close)) = inline_tag_name(token) else {
        // PI / CDATA / declaration / comment / malformed: drop in sanitize mode.
        return SanitizedTag::Dropped;
    };
    let name = &token[name_range.clone()];
    // Explicit drop-list removes the tag entirely (markup gone; any text between
    // an open/close pair stays as inert text).
    if policy.drop.iter().any(|t| t.as_bytes().eq_ignore_ascii_case(name)) {
        return SanitizedTag::Dropped;
    }
    // The dangerous set is NON-OVERRIDABLE: a script/iframe/svg/… is dropped in
    // BOTH allow-all and restrict mode, even if explicitly allowlisted — a caller
    // who truly wants raw script uses `unsafe_html`, not the sanitizer. Dropping
    // (rather than escaping) leaves any open/close pair's body as inert text.
    if DANGEROUS_HTML_TAGS.iter().any(|d| d.eq_ignore_ascii_case(name)) {
        return SanitizedTag::Dropped;
    }
    // Allow-all renders every (non-dangerous) tag; restrict renders only the
    // allowlisted ones and escapes the rest (visible as literal text, never
    // executed).
    if !policy.allowlist.is_empty()
        && !policy.allowlist.iter().any(|t| t.as_bytes().eq_ignore_ascii_case(name))
    {
        for &b in token {
            push_escaped(b, out);
        }
        return SanitizedTag::Escaped;
    }
    // Validate the whole token to UTF-8 once; the tag name is an ASCII sub-slice
    // of it, so it can be sliced out without a second validation pass.
    let token_str = std::str::from_utf8(token).unwrap_or("");
    let name_str = token_str.get(name_range).unwrap_or("");
    let void =
        VOID_HTML_TAGS.iter().any(|v| v.eq_ignore_ascii_case(name)) || token.ends_with(b"/>");
    if is_close {
        out.push_str("</");
        out.push_str(name_str);
        out.push('>');
        return SanitizedTag::Rendered { name: name_str, close: true, void };
    }
    out.push('<');
    out.push_str(name_str);
    // RAW HTML: the sanitized attributes below are emitted as real DOM
    // attributes on an attacker-authored tag, so this path uses the stricter
    // entry point (drops `srcdoc`, `id`/`name`, `slot`, the `form*` family, …).
    // Component tags deliberately keep the permissive `sanitize_attrs` — their
    // attributes become framework props the consumer mediates, not DOM.
    for (k, v) in sanitize_raw_html_attrs(token_str, policy.allow_schemes) {
        out.push(' ');
        out.push_str(&k);
        out.push_str("=\"");
        escape_attr(&v, out);
        out.push('"');
    }
    // Preserve an author's self-closing slash (harmless for void elements; keeps
    // non-void self-closes balanced).
    if token.ends_with(b"/>") {
        out.push_str(" />");
    } else {
        out.push('>');
    }
    SanitizedTag::Rendered { name: name_str, close: false, void }
}

pub(crate) fn match_inline_html(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'<') {
        return None;
    }
    let rest = &bytes[start..];
    // Comment
    if rest.starts_with(b"<!--") {
        let end = rest.windows(3).position(|w| w == b"-->")?;
        return Some(end + 3);
    }
    // PI
    if rest.starts_with(b"<?") {
        let end = rest.windows(2).position(|w| w == b"?>")?;
        return Some(end + 2);
    }
    // CDATA
    if rest.starts_with(b"<![CDATA[") {
        let end = rest.windows(3).position(|w| w == b"]]>")?;
        return Some(end + 3);
    }
    // Declaration
    if rest.len() > 2 && rest[1] == b'!' && rest[2].is_ascii_alphabetic() {
        let end = rest.iter().position(|&b| b == b'>')?;
        return Some(end + 1);
    }
    // Open or close tag
    let mut i = 1;
    let closing = rest.get(i) == Some(&b'/');
    if closing {
        i += 1;
    }
    if !rest.get(i).map_or(false, |b| b.is_ascii_alphabetic()) {
        return None;
    }
    while i < rest.len() && (rest[i].is_ascii_alphanumeric() || rest[i] == b'-') {
        i += 1;
    }
    if closing {
        while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if rest.get(i) != Some(&b'>') {
            return None;
        }
        return Some(i + 1);
    }
    // Open tag: attributes
    loop {
        let prev = i;
        while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if rest.get(i) == Some(&b'/') {
            i += 1;
            break;
        }
        if rest.get(i) == Some(&b'>') {
            break;
        }
        if i == prev {
            return None;
        }
        if i >= rest.len() || !(rest[i].is_ascii_alphabetic() || rest[i] == b'_' || rest[i] == b':') {
            return None;
        }
        while i < rest.len() && (rest[i].is_ascii_alphanumeric() || matches!(rest[i], b'_' | b':' | b'.' | b'-')) {
            i += 1;
        }
        // Optional `= value`, whitespace allowed around `=`. Only consume the
        // whitespace after the name if it is actually followed by `=`;
        // otherwise it is the (required) separator before the next attribute.
        let save = i;
        while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if rest.get(i) == Some(&b'=') {
            i += 1;
            while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
                i += 1;
            }
            if rest.get(i) == Some(&b'"') {
                i += 1;
                while i < rest.len() && rest[i] != b'"' {
                    i += 1;
                }
                if rest.get(i) != Some(&b'"') {
                    return None;
                }
                i += 1;
            } else if rest.get(i) == Some(&b'\'') {
                i += 1;
                while i < rest.len() && rest[i] != b'\'' {
                    i += 1;
                }
                if rest.get(i) != Some(&b'\'') {
                    return None;
                }
                i += 1;
            } else {
                let prev = i;
                while i < rest.len() && !matches!(rest[i], b' ' | b'\t' | b'\n' | b'>' | b'<' | b'\'' | b'"' | b'=' | b'`') {
                    i += 1;
                }
                if i == prev {
                    return None;
                }
            }
        } else {
            i = save;
        }
    }
    if rest.get(i) != Some(&b'>') {
        return None;
    }
    Some(i + 1)
}

/// Is the `<` at `start` the head of a raw-HTML token or autolink that is
/// genuinely STILL STREAMING to EOF — could a later byte still complete it?
/// `true` ⇒ the open-tail render SUPPRESSES it entirely (same pending-invisible
/// contract as a streaming markdown link's URL): a partially-arrived
/// `<a href="https://…` must never flash its URL as literal text while the
/// stream is open. `false` ⇒ the token is settled — either complete right here
/// (match_inline_html/try_autolink take it) or broken forever (renders literal,
/// e.g. `a < b`, `1<2`, `<a:x`).
///
/// MUST mirror [`match_inline_html`] (and the autolink scheme walk) exactly:
/// every arm below is that scanner's walk with "EOF reached mid-walk → true"
/// in place of its terminal accept/reject. Email autolinks (`<user@…>`) are
/// deliberately NOT mirrored: their local part admits digits and common
/// punctuation, so suppressing them would momentarily hide everyday
/// comparisons like `x<2` — a worse trade than a short-lived visible address.
pub(crate) fn inline_html_streams_to_eof(bytes: &[u8], start: usize) -> bool {
    let rest = &bytes[start..];
    if rest.first() != Some(&b'<') {
        return false;
    }
    let streaming = match rest.get(1) {
        None => true, // bare `<` abutting EOF: any form could follow
        Some(b'!') => bang_streams_to_eof(rest),
        // PI `<?…?>`: streaming iff the terminator hasn't arrived.
        Some(b'?') => !rest.windows(2).any(|w| w == b"?>"),
        Some(b'/') => close_tag_streams_to_eof(rest),
        Some(c) if c.is_ascii_alphabetic() => open_tag_or_autolink_streams_to_eof(rest),
        _ => false, // `<3`, `< b`, `<=` … can never become a tag/autolink
    };
    // Scanner-drift guard (same discipline as dest_streams_to_eof): a token
    // match_inline_html accepts as COMPLETE right here must never be reported
    // as still-streaming.
    debug_assert!(
        !(streaming && match_inline_html(bytes, start).is_some()),
        "inline_html_streams_to_eof returned true but match_inline_html found a complete \
         token at start={start}: {:?}",
        std::str::from_utf8(&bytes[start..]).unwrap_or("<non-utf8>")
    );
    streaming
}

/// `<!…` arm of [`inline_html_streams_to_eof`]: comment / CDATA / declaration.
fn bang_streams_to_eof(rest: &[u8]) -> bool {
    if rest.starts_with(b"<!--") {
        return !rest.windows(3).any(|w| w == b"-->"); // comment streaming
    }
    if b"<!--".starts_with(rest) {
        return true; // `<!`, `<!-` abutting EOF — only a comment can follow
    }
    if rest.starts_with(b"<![CDATA[") {
        return !rest.windows(3).any(|w| w == b"]]>"); // CDATA streaming
    }
    if b"<![CDATA[".starts_with(rest) {
        return true; // `<![`, `<![C`, … abutting EOF
    }
    if rest.len() > 2 && rest[2].is_ascii_alphabetic() {
        return !rest.contains(&b'>'); // declaration streaming
    }
    false
}

/// `</…` arm of [`inline_html_streams_to_eof`]: a closing tag streaming to EOF.
fn close_tag_streams_to_eof(rest: &[u8]) -> bool {
    let mut i = 2;
    if i == rest.len() {
        return true; // `</` abutting EOF
    }
    if !rest[i].is_ascii_alphabetic() {
        return false;
    }
    while i < rest.len() && (rest[i].is_ascii_alphanumeric() || rest[i] == b'-') {
        i += 1;
    }
    while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
        i += 1;
    }
    // EOF mid-name/ws → streaming; a present byte must be `>` (complete → the
    // real scanner takes it) or junk (broken forever) — both settled.
    i == rest.len()
}

/// `<name…` arm of [`inline_html_streams_to_eof`]: an open tag OR an autolink
/// (`<scheme:…>`) streaming to EOF. Both grammars start `<` + ASCII letter, so
/// they are probed together; either still being completable suppresses.
fn open_tag_or_autolink_streams_to_eof(rest: &[u8]) -> bool {
    // Autolink arm: scheme = letter + 1..=31 [A-Za-z0-9+.-], then `:`, then a
    // body free of whitespace/`<`/`>`, then `>`. Streaming iff EOF lands inside
    // a still-valid prefix. (`<https://secret…` with no `>` yet is THE case.)
    {
        let mut i = 1;
        while i < rest.len() && i <= 32 && (rest[i].is_ascii_alphanumeric() || matches!(rest[i], b'+' | b'.' | b'-')) {
            i += 1;
        }
        if i == rest.len() {
            return true; // scheme prefix abuts EOF (also covers a tag-name prefix)
        }
        if rest[i] == b':' && i >= 3 {
            // scheme complete (≥2 chars): walk the body.
            let mut j = i + 1;
            while j < rest.len() && !matches!(rest[j], b' ' | b'\t' | b'\n' | b'<' | b'>') {
                j += 1;
            }
            if j == rest.len() {
                return true; // body abuts EOF, `>` still to come
            }
        }
    }
    // Open-tag arm: mirror match_inline_html's open-tag walk, EOF ⇒ streaming.
    let mut i = 1;
    while i < rest.len() && (rest[i].is_ascii_alphanumeric() || rest[i] == b'-') {
        i += 1;
    }
    if i == rest.len() {
        return true; // tag name abuts EOF
    }
    loop {
        let prev = i;
        while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if i == rest.len() {
            return true; // ws abuts EOF — an attr or `>` could still come
        }
        if rest[i] == b'/' {
            return i + 1 == rest.len(); // awaiting `>`; anything else is settled
        }
        if rest[i] == b'>' {
            return false; // complete — the real scanner takes it
        }
        if i == prev {
            return false; // attrs must be ws-separated — broken forever
        }
        if !(rest[i].is_ascii_alphabetic() || rest[i] == b'_' || rest[i] == b':') {
            return false; // invalid attr-name start — broken forever
        }
        while i < rest.len() && (rest[i].is_ascii_alphanumeric() || matches!(rest[i], b'_' | b':' | b'.' | b'-')) {
            i += 1;
        }
        if i == rest.len() {
            return true; // attr name abuts EOF
        }
        let save = i;
        while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if i == rest.len() {
            return true; // ws after an attr name abuts EOF (`=` could come)
        }
        if rest[i] == b'=' {
            i += 1;
            while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
                i += 1;
            }
            if i == rest.len() {
                return true; // awaiting the value
            }
            match rest[i] {
                b'"' => {
                    i += 1;
                    while i < rest.len() && rest[i] != b'"' {
                        i += 1;
                    }
                    if i == rest.len() {
                        return true; // unclosed quoted value — `<a href="https://…`
                    }
                    i += 1;
                }
                b'\'' => {
                    i += 1;
                    while i < rest.len() && rest[i] != b'\'' {
                        i += 1;
                    }
                    if i == rest.len() {
                        return true;
                    }
                    i += 1;
                }
                _ => {
                    let vstart = i;
                    while i < rest.len() && !matches!(rest[i], b' ' | b'\t' | b'\n' | b'>' | b'<' | b'\'' | b'"' | b'=' | b'`') {
                        i += 1;
                    }
                    if i == vstart {
                        return false; // invalid first value byte — broken forever
                    }
                    if i == rest.len() {
                        return true; // unquoted value abuts EOF
                    }
                }
            }
        } else {
            i = save;
        }
    }
}

/// If the raw open tag at `start` is currently streaming to EOF *inside an
/// unclosed quoted attribute value*, return that value's quote byte (`b'"'` or
/// `b'\''`); otherwise `None`. This is the one open-tag streaming state whose
/// suppressed render extends in O(1) as bytes append: while no matching quote
/// closes the value the tag stays EOF-streaming (so
/// [`inline_html_streams_to_eof`] keeps suppressing it) and the render is
/// unchanged. Mirrors the open-tag arm of [`open_tag_or_autolink_streams_to_eof`]
/// exactly; every OTHER terminal — settled, broken, or streaming-but-not-in-a-
/// quote (tag name / ws / unquoted value abutting EOF, the autolink arm) —
/// returns `None`, so a caller keying a fast path off it only ever OVER-drops to
/// the byte-identical full path.
pub(crate) fn open_tag_streaming_quote(bytes: &[u8], start: usize) -> Option<u8> {
    let rest = &bytes[start..];
    // `<` + ASCII letter opens a tag; anything else is not this state.
    if rest.first() != Some(&b'<') || !rest.get(1)?.is_ascii_alphabetic() {
        return None;
    }
    let mut i = 1;
    while i < rest.len() && (rest[i].is_ascii_alphanumeric() || rest[i] == b'-') {
        i += 1;
    }
    if i == rest.len() {
        return None; // tag name abuts EOF — streaming, but not in a quote
    }
    loop {
        let prev = i;
        while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if i == rest.len() {
            return None; // ws abuts EOF
        }
        if rest[i] == b'/' || rest[i] == b'>' {
            return None; // self-closing awaiting `>` / complete — settled
        }
        if i == prev {
            return None; // attrs must be ws-separated — broken
        }
        if !(rest[i].is_ascii_alphabetic() || rest[i] == b'_' || rest[i] == b':') {
            return None; // invalid attr-name start — broken
        }
        while i < rest.len() && (rest[i].is_ascii_alphanumeric() || matches!(rest[i], b'_' | b':' | b'.' | b'-')) {
            i += 1;
        }
        if i == rest.len() {
            return None; // attr name abuts EOF
        }
        let save = i;
        while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
            i += 1;
        }
        if i == rest.len() {
            return None; // ws after an attr name abuts EOF
        }
        if rest[i] == b'=' {
            i += 1;
            while i < rest.len() && matches!(rest[i], b' ' | b'\t' | b'\n') {
                i += 1;
            }
            if i == rest.len() {
                return None; // awaiting the value
            }
            match rest[i] {
                b'"' => {
                    i += 1;
                    while i < rest.len() && rest[i] != b'"' {
                        i += 1;
                    }
                    if i == rest.len() {
                        debug_assert!(inline_html_streams_to_eof(bytes, start));
                        return Some(b'"'); // unclosed double-quoted value abuts EOF
                    }
                    i += 1;
                }
                b'\'' => {
                    i += 1;
                    while i < rest.len() && rest[i] != b'\'' {
                        i += 1;
                    }
                    if i == rest.len() {
                        debug_assert!(inline_html_streams_to_eof(bytes, start));
                        return Some(b'\''); // unclosed single-quoted value abuts EOF
                    }
                    i += 1;
                }
                _ => {
                    let vstart = i;
                    while i < rest.len() && !matches!(rest[i], b' ' | b'\t' | b'\n' | b'>' | b'<' | b'\'' | b'"' | b'=' | b'`') {
                        i += 1;
                    }
                    if i == vstart {
                        return None; // invalid first value byte — broken
                    }
                    if i == rest.len() {
                        return None; // unquoted value abuts EOF — streaming, not a quote
                    }
                }
            }
        } else {
            i = save;
        }
    }
}

// ---------------------------------------------------------------------
// Inline custom components (opt-in `inline_component_tags`)
// ---------------------------------------------------------------------

/// Result of probing for an inline component at a `<`.
enum InlineComp {
    /// Not an allowlisted inline-component open tag — try inline raw HTML next.
    NotComponent,
    /// An allowlisted inline open tag whose matching close has not arrived yet
    /// (or whose open tag is still incomplete): keep the `<` inert + retryable.
    Incomplete,
    /// Rendered to `out`; resume scanning at this byte offset.
    Done(usize),
}

/// Dispatch an allowlisted inline component (`<tik …>…</tik>` or self-closing
/// `<tik …/>`) at `start`. Inner content is rendered as inline markdown;
/// attributes are sanitized (event handlers dropped, dangerous URL schemes
/// neutralized) so the emitted element is XSS-safe even with `unsafe_html` off,
/// and a JSX/DOM layer dispatches it via `components[tag]`. Same-tag nesting and
/// inline code spans are respected when locating the matching close.
fn try_inline_component(
    input: &str,
    bytes: &[u8],
    start: usize,
    opts: &RenderOpts,
    out: &mut String,
) -> InlineComp {
    let tags = &opts.inline_component_tags;
    if tags.is_empty() {
        return InlineComp::NotComponent;
    }
    let Some((name_end, attrs_end, self_closing)) = inline_open_tag(bytes, start, tags) else {
        return InlineComp::NotComponent;
    };
    let name = &input[start + 1..name_end];
    // PERMISSIVE on purpose: `sanitize_attrs`, not `sanitize_raw_html_attrs`.
    // These become props on `components[tag]` — the consumer's component decides
    // whether any of them ever reaches the DOM — so the raw-HTML DOM denylist
    // (`id`/`name` clobbering, `slot`, `form*`, …) does not apply. `on*`, `style`,
    // React-unsafe names and dangerous URL schemes are still filtered.
    let attrs = sanitize_attrs(&input[start..attrs_end], &opts.allow_schemes);

    if self_closing {
        write_inline_component(name, &attrs, "", opts, out);
        return InlineComp::Done(attrs_end);
    }
    match find_inline_close(bytes, attrs_end, name.as_bytes()) {
        Some(close_start) => {
            write_inline_component(name, &attrs, &input[attrs_end..close_start], opts, out);
            // Advance past `</name>`.
            InlineComp::Done(close_start + 2 + name.len() + 1)
        }
        None => InlineComp::Incomplete,
    }
}

/// Emit `<name attrs>inner</name>`, with `inner` rendered as inline markdown.
fn write_inline_component(
    name: &str,
    attrs: &[(String, String)],
    inner: &str,
    opts: &RenderOpts,
    out: &mut String,
) {
    out.push('<');
    out.push_str(name);
    for (k, v) in attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        escape_attr(v, out);
        out.push('"');
    }
    out.push('>');
    if !inner.is_empty() {
        render_inline_core(inner, opts, out, false);
    }
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

/// If an allowlisted inline-component OPEN tag starts at `start` (`bytes[start]`
/// is `<`, not `</`), return `(name_end, attrs_end, self_closing)` — `name_end`
/// just past the tag name, `attrs_end` just past the closing `>`. Tolerates
/// quoted attribute values containing `>`. `None` if it isn't an allowlisted
/// open tag, or the tag is not yet complete (no `>`), or it is malformed.
fn inline_open_tag(bytes: &[u8], start: usize, tags: &[Box<str>]) -> Option<(usize, usize, bool)> {
    if bytes.get(start) != Some(&b'<') {
        return None;
    }
    let name_start = start + 1;
    let mut i = name_start;
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
        i += 1;
    }
    if i == name_start || !tags.iter().any(|t| t.as_bytes() == &bytes[name_start..i]) {
        return None;
    }
    let name_end = i;
    let mut in_quote = 0u8;
    while i < bytes.len() {
        let c = bytes[i];
        if in_quote != 0 {
            if c == in_quote {
                in_quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            in_quote = c;
        } else if c == b'<' {
            return None; // a new `<` before this tag closed — malformed
        } else if c == b'>' {
            let mut k = i;
            while k > name_end && matches!(bytes[k - 1], b' ' | b'\t') {
                k -= 1;
            }
            let self_closing = bytes[k - 1] == b'/';
            return Some((name_end, i + 1, self_closing));
        }
        i += 1;
    }
    None
}

/// Length of the backtick run starting at `i`.
fn backtick_run_len(bytes: &[u8], i: usize) -> usize {
    let mut n = 0;
    while i + n < bytes.len() && bytes[i + n] == b'`' {
        n += 1;
    }
    n
}

/// Find the close of a code span opened by a run of `run` backticks: the next
/// run of EXACTLY `run` backticks. Returns the offset just past it, or `None`
/// (an unclosed run — its backticks are literal).
fn matching_backtick_close(bytes: &[u8], from: usize, run: usize) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let r = backtick_run_len(bytes, i);
            if r == run {
                return Some(i + r);
            }
            i += r;
        } else {
            i += 1;
        }
    }
    None
}

/// Is `bytes[i..]` exactly `</name>`?
fn is_inline_close_tag(bytes: &[u8], i: usize, name: &[u8]) -> bool {
    bytes.get(i) == Some(&b'<')
        && bytes.get(i + 1) == Some(&b'/')
        && bytes.len() >= i + 2 + name.len() + 1
        && &bytes[i + 2..i + 2 + name.len()] == name
        && bytes[i + 2 + name.len()] == b'>'
}

/// Is `bytes[i..]` a same-`name` non-self-closing open tag `<name …>`? Returns
/// the offset just past its `>` (so the close-finder can count nesting depth).
fn inline_open_same(bytes: &[u8], i: usize, name: &[u8]) -> Option<usize> {
    if bytes.get(i) != Some(&b'<') || bytes.get(i + 1) == Some(&b'/') {
        return None;
    }
    let ns = i + 1;
    if bytes.len() < ns + name.len() || &bytes[ns..ns + name.len()] != name {
        return None;
    }
    // The name must end here (not be a prefix of a longer tag name).
    if matches!(bytes.get(ns + name.len()), Some(c) if c.is_ascii_alphanumeric() || *c == b'-') {
        return None;
    }
    let mut j = ns + name.len();
    let mut in_quote = 0u8;
    while j < bytes.len() {
        let c = bytes[j];
        if in_quote != 0 {
            if c == in_quote {
                in_quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            in_quote = c;
        } else if c == b'>' {
            let mut k = j;
            while k > ns + name.len() && matches!(bytes[k - 1], b' ' | b'\t') {
                k -= 1;
            }
            // A self-closing `<name/>` opens no new nesting level.
            return if bytes[k - 1] == b'/' { None } else { Some(j + 1) };
        }
        j += 1;
    }
    None
}

/// Locate the matching `</name>` close for an inline component opened at the
/// caller, scanning from `from`. Tracks same-tag nesting (`<name …>` opens) and
/// skips inline code spans (a `</name>` inside backticks is content). Returns
/// the byte offset of the `<` of the matching close, or `None` if it has not
/// arrived yet (the component is then incomplete).
fn find_inline_close(bytes: &[u8], from: usize, name: &[u8]) -> Option<usize> {
    let mut i = from;
    let mut depth = 1usize;
    while i < bytes.len() {
        match bytes[i] {
            b'`' => {
                let run = backtick_run_len(bytes, i);
                i = matching_backtick_close(bytes, i + run, run).unwrap_or(i + run);
            }
            b'<' => {
                if is_inline_close_tag(bytes, i, name) {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                    i += 2 + name.len() + 1;
                } else if let Some(open_end) = inline_open_same(bytes, i, name) {
                    depth += 1;
                    i = open_end;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    None
}

// ---------------------------------------------------------------------
// Links + images
// ---------------------------------------------------------------------

/// GFM footnote reference `[^label]` → superscript link, if the label has been
/// numbered (i.e. it appears in `opts.footnotes`). Returns the position after
/// the closing `]`. Definitions (`[^x]:`) are handled at block level, not here.
fn try_footnote_ref(bytes: &[u8], start: usize, opts: &RenderOpts, out: &mut String) -> Option<usize> {
    if bytes.get(start + 1) != Some(&b'^') {
        return None;
    }
    let mut j = start + 2;
    while j < bytes.len() && bytes[j] != b']' {
        if bytes[j] == b'[' || bytes[j].is_ascii_whitespace() {
            return None;
        }
        j += 1;
    }
    if j >= bytes.len() || j == start + 2 {
        return None;
    }
    let label = std::str::from_utf8(&bytes[start + 2..j]).ok()?;
    let num = opts.footnote_num(label)?;
    let n = num.to_string();
    out.push_str("<sup class=\"footnote-ref\"><a href=\"#fn-");
    out.push_str(&n);
    out.push_str("\" id=\"fnref-");
    // Placeholder mode (streaming caches): emit an occurrence-INDEPENDENT
    // sentinel token for the `fnref-…` suffix instead of computing it now (and
    // do NOT advance the per-label occurrence counter). A later
    // `resolve_footnote_ids` pass rewrites it in document order. Falls back to
    // the normal path if the label carries the token delimiters (so the tokens
    // stay unambiguous). When the flag is off, behavior is byte-identical to
    // before.
    if opts.footnote_placeholder && !label.contains(['\u{0}', '\u{1}']) {
        out.push('\u{0}');
        out.push('F');
        out.push('\u{1}');
        out.push_str(&n);
        out.push('\u{1}');
        out.push_str(label);
        out.push('\u{0}');
    } else {
        // Occurrence index for this label (0-based). The Kth (K≥1) reference gets
        // a unique id `fnref-N-(K+1)` so repeated references don't collide.
        let occurrence = {
            let mut occ = opts.footnote_occ.borrow_mut();
            let c = occ.entry(label.to_string()).or_insert(0);
            let k = *c;
            *c += 1;
            k
        };
        // Emit ONLY the occurrence suffix — the `id="fnref-` prefix is already
        // written above (mirrors the placeholder token, which also carries just
        // the suffix).
        out.push_str(&n);
        if occurrence != 0 {
            out.push('-');
            out.push_str(&(occurrence + 1).to_string());
        }
    }
    out.push_str("\">");
    out.push_str(&n);
    out.push_str("</a></sup>");
    Some(j + 1)
}

fn try_link(bytes: &[u8], start: usize, opts: &RenderOpts, out: &mut String) -> Option<usize> {
    let (text_range, after_text) = read_balanced_brackets(bytes, start)?;

    // §6.6: links may not contain other links. CommonMark resolves brackets
    // inner-first, so a nested link inside our text means *this* (outer) link
    // is not formed — the bracket becomes literal and render_inline reprocesses
    // it, letting the inner link win. (Images in link text are fine.)
    if text_has_nested_link(bytes, text_range.clone(), opts) {
        return None;
    }

    if bytes.get(after_text) == Some(&b'(') {
        if let Some((url, title, after_paren)) = read_link_destination(bytes, after_text + 1) {
            return Some(write_link(bytes, &text_range, &url, title.as_deref(), opts, out, after_paren));
        }
    }
    let (label_range_opt, end_pos) = read_optional_ref_label(bytes, after_text);
    let label_bytes = match label_range_opt {
        Some(r) if !r.is_empty() => &bytes[r],
        _ => &bytes[text_range.clone()],
    };
    let label = std::str::from_utf8(label_bytes).ok()?;
    if !crate::render::valid_link_label(label) {
        return None;
    }
    let r = opts.lookup(label)?;
    let url = r.url.clone();
    let title = r.title.clone();
    Some(write_link(bytes, &text_range, &url, title.as_deref(), opts, out, end_pos))
}

/// Does a complete link parse starting at the `[` at `start`? Returns its end
/// offset. Used to detect a nested link inside link text (§6.6).
fn link_parses_at(bytes: &[u8], start: usize, opts: &RenderOpts) -> Option<usize> {
    let (text_range, after_text) = read_balanced_brackets(bytes, start)?;
    if bytes.get(after_text) == Some(&b'(') {
        if let Some((_, _, after_paren)) = read_link_destination(bytes, after_text + 1) {
            return Some(after_paren);
        }
    }
    let (label_range_opt, end_pos) = read_optional_ref_label(bytes, after_text);
    let label_bytes = match label_range_opt {
        Some(r) if !r.is_empty() => &bytes[r],
        _ => &bytes[text_range.clone()],
    };
    let label = std::str::from_utf8(label_bytes).ok()?;
    opts.lookup(label).map(|_| end_pos)
}

/// Scan a link-text byte range for a nested link (not an image). Code spans,
/// autolinks/HTML, and images are skipped so their internal brackets don't
/// count.
fn text_has_nested_link(
    bytes: &[u8],
    range: core::ops::Range<usize>,
    opts: &RenderOpts,
) -> bool {
    // Cap the span probed for a nested link: each `[` re-runs the (recursive)
    // link parse, so an unbounded range over many `[` is cubic. Past the cap we
    // stop probing — a real link label is far shorter than this, so output is
    // unchanged for realistic input.
    let probe_end = range.end.min(range.start + MAX_BRACKET_TEXT_LEN);
    let mut i = range.start;
    while i < probe_end {
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'`' => i = skip_code_span(bytes, i).unwrap_or(i + 1),
            b'<' => i = skip_autolink_or_html(bytes, i).unwrap_or(i + 1),
            b'!' if bytes.get(i + 1) == Some(&b'[') => {
                // An image: skip its (balanced) alt so links inside the alt
                // don't invalidate the outer link.
                match read_balanced_brackets(bytes, i + 1) {
                    Some((_, after)) => i = after,
                    None => i += 2,
                }
            }
            b'[' => {
                if link_parses_at(bytes, i, opts).is_some() {
                    return true;
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    false
}

fn write_link(
    bytes: &[u8],
    text_range: &core::ops::Range<usize>,
    url: &str,
    title: Option<&str>,
    opts: &RenderOpts,
    out: &mut String,
    end_pos: usize,
) -> usize {
    out.push_str("<a href=\"");
    sanitize_url(url, out, false, &opts.allow_schemes);
    out.push('"');
    if let Some(t) = title {
        out.push_str(" title=\"");
        let decoded = crate::url::decode_text(t);
        escape_attr(&decoded, out);
        out.push('"');
    }
    out.push_str(" target=\"_blank\" rel=\"noopener noreferrer nofollow\">");
    let text = std::str::from_utf8(&bytes[text_range.clone()]).unwrap_or("");
    // CommonMark §6.6: links can't contain nested links.
    let mut inner_opts = opts.clone();
    inner_opts.in_link = true;
    render_inline(text, &inner_opts, out);
    out.push_str("</a>");
    end_pos
}

/// Max streaming-LABEL length the speculative phases will wrap in a pending
/// anchor. A prose paragraph with a stray unclosed `[` would otherwise restyle
/// everything after it as a pending link until the block closes; past any
/// realistic label length we downgrade to literal (UX bound only — parity is
/// untouched, both open views apply it identically).
const MAX_SPECULATIVE_LABEL_LEN: usize = 512;

/// Speculative open-tail link render (streaming only — gated by `opts.open_tail`
/// at the call site). Covers every phase of a link being typed at buffer EOF,
/// emitting the same INERT pending anchor (see [`write_pending_anchor`]) for
/// each so the label never flashes as raw `[bracket` text:
///
///   - LABEL phase: `[Earnings Ca` — no closing `]` yet, the label itself is
///     still streaming (guards: [`speculable_label`]).
///   - `]`-at-EOF: `[Earnings Call]` — the very next byte decides the form
///     (`(` inline, `[` ref, `:` def opener, other → shortcut ref); keep the
///     pending anchor for this frame instead of flashing the brackets.
///   - REF phase: `[label][re` — the ref name is still streaming. When it
///     closes, the real `try_link` resolves against known defs exactly as
///     before (unknown refs downgrade to literal — the documented forward-ref
///     behavior).
///   - URL phase (0.16.1): `[label](https://exa` — destination (and now
///     title, `[l](url "ti`) still streaming; the load-bearing mirror of
///     `read_link_destination` is [`dest_streams_to_eof`].
///
/// Returns `Some(bytes.len())` (all phases consume to EOF by construction) or
/// `None` (writing nothing) when this is NOT a still-streaming link: those fall
/// through to the real `try_link` (complete link, resolved/downgraded ref, or
/// literal). `[t]x` (shortcut-ref followed by anything but `(`/`[`) is settled
/// — resolve-or-downgrade as before, deliberately: an unknown `[1] and` snaps
/// from pending `1` to literal `[1] and` (uncommon, pinned by test).
fn speculative_link_tail(
    bytes: &[u8],
    start: usize,
    opts: &RenderOpts,
    out: &mut String,
) -> Option<usize> {
    // Image marker guard: `![label](url…` is IMAGE syntax, never a link. When the
    // image is incomplete, `try_image` fails, emits a literal `!`, and the `[` arm
    // re-processes `[label](url…` — which must NOT speculate (a partial image
    // stays literal text, never an inert `<a>` — the alt phase mirrors the URL
    // phase, which has always kept incomplete images literal). Block speculation
    // when an UNESCAPED `!` immediately precedes this `[`. (An escaped `\!` is a
    // literal `!` followed by a real link, so it may still speculate — hence the
    // backslash check.)
    if start > 0
        && bytes[start - 1] == b'!'
        && !(start >= 2 && bytes[start - 2] == b'\\')
    {
        return None;
    }
    match scan_brackets(bytes, start) {
        // Caps tripped / not a bracket: the real parse renders literal.
        BracketScan::Reject => None,
        // LABEL phase: `[label…` still streaming — no closing `]` yet.
        BracketScan::OpenAtEof => {
            let label = start + 1..bytes.len();
            if label.is_empty() {
                // Lone trailing `[`: the very next byte decides link vs
                // footnote vs checkbox vs literal — wait one frame.
                return None;
            }
            if !speculable_label(bytes, &label, opts) {
                return None;
            }
            // §6.6: a complete nested link inside the (partial) label already
            // disqualifies this link — same guard the real `try_link` applies.
            if text_has_nested_link(bytes, label.clone(), opts) {
                return None;
            }
            Some(write_pending_anchor(bytes, &label, opts, out))
        }
        BracketScan::Closed(text_range, after_text) => {
            // §6.6: a complete nested link inside the label disqualifies this
            // link — same guard `try_link` applies, so we never speculate where
            // the real parse would reject.
            if text_has_nested_link(bytes, text_range.clone(), opts) {
                return None;
            }
            match bytes.get(after_text) {
                // URL phase: destination/title must be genuinely still
                // streaming to EOF (no closing `)`, no hard terminator).
                Some(&b'(') => {
                    if !dest_streams_to_eof(bytes, after_text + 1) {
                        return None;
                    }
                    Some(write_pending_anchor(bytes, &text_range, opts, out))
                }
                // REF phase: `[label][re` — keep the pending anchor while the
                // ref name streams. A closed ref (`[t][r]`, `[t][]`) or a
                // capped scan falls to the real `try_link` (resolve against
                // known defs / literal downgrade), unchanged.
                Some(&b'[') => match scan_brackets(bytes, after_text) {
                    BracketScan::OpenAtEof
                        if speculable_label(bytes, &text_range, opts) =>
                    {
                        Some(write_pending_anchor(bytes, &text_range, opts, out))
                    }
                    _ => None,
                },
                // `]` abuts EOF: the NEXT byte picks the form — hold the
                // pending anchor for one more frame rather than flashing
                // `[label]` (even a def-known shortcut ref: a following
                // `(url)` would change its href, so no href until settled).
                None => {
                    if !speculable_label(bytes, &text_range, opts) {
                        return None;
                    }
                    Some(write_pending_anchor(bytes, &text_range, opts, out))
                }
                // `]` followed by anything else: shortcut-ref semantics,
                // settled — resolve or literal-downgrade via `try_link`.
                _ => None,
            }
        }
    }
}

/// Shared guards for the speculative phases that fire BEFORE the `](` of an
/// inline link has streamed (label / `]`-at-EOF / ref). The URL phase keeps its
/// 0.16.1 semantics untouched — by the time `](` has streamed, link intent is
/// unambiguous and these candidates can no longer form.
///
/// POSITION-INDEPENDENCE INVARIANT: every guard here must be a function of the
/// bytes from the `[` to EOF (plus opts) only — never of where the `[` sits in
/// the slice. The streaming caches re-render the SAME open block from a
/// committed cut (`try_incremental_paragraph` & co.), so the `[` that sits
/// mid-slice in the full rescan is slice-initial in the cache's active tail; a
/// `start`-keyed guard would make the two views diverge (caught by
/// `fuzz_partial_link`). The trade: the checkbox/alert exclusions fire on
/// lookalike content ANYWHERE (one extra literal frame for `[x`-/`[!Note`-
/// shaped labels), which is the safe direction — literal is the pre-existing
/// behavior.
fn speculable_label(
    bytes: &[u8],
    label: &core::ops::Range<usize>,
    opts: &RenderOpts,
) -> bool {
    let content = &bytes[label.clone()];
    // Footnote candidate: `[^…` may still become a footnote ref (a def-known
    // ref settles in `try_footnote_ref` before we run) or a `[^x]:` def opener
    // — stay literal, exactly as today.
    if opts.gfm_footnotes && content.first() == Some(&b'^') {
        return false;
    }
    // Nothing may follow the label except its own `]` abutting EOF — true in
    // the label phase (label runs to EOF) and the `]`-at-EOF phase; false in
    // the ref phase (`[t][re` continues past the `]`), where the checkbox /
    // alert forms below are already ruled out.
    let bare_to_eof = bytes.len() <= label.end + 1;
    // Task-list checkbox candidate: `[c` / `[c]` (c ∈ ` xX`, nothing after) is
    // `- [x` mid-checkbox when the `[` opens a list item's body — stay literal
    // until the form is ruled out (`[x](…` hands off to the URL phase, `[xy`
    // speculates).
    if bare_to_eof && content.len() == 1 && matches!(content[0], b' ' | b'x' | b'X') {
        return false;
    }
    // Alert-marker candidate: with alerts on, `[!NOTE`-shaped content (`!` +
    // letters only, nothing past an optional trailing `]` at EOF) may still
    // become `> [!NOTE]` chrome when the `[` opens a quote body's first line —
    // stay literal rather than styling the marker as a pending link.
    if opts.gfm_alerts
        && bare_to_eof
        && content.first() == Some(&b'!')
        && content[1..].iter().all(|b| b.is_ascii_alphabetic())
    {
        return false;
    }
    // Numeric-citation candidate: an all-digit label so far (`[1`, `[12`) is
    // far more often a bare `[1]` citation than a link in LLM/RAG output —
    // styling every citation as a pending link that snaps back to literal
    // would flash constantly. Stay literal while all-digit; a genuine
    // `[1](url)` still upgrades at `](`, and a label like `[1 more` starts
    // speculating at its first non-digit byte. (Pure function of `content`,
    // keeping the guard position-independent.)
    if !content.is_empty() && content.iter().all(|b| b.is_ascii_digit()) {
        return false;
    }
    // UX blast-radius bounds: an open label spanning a newline or growing past
    // any realistic length is prose with a stray `[`, not a link being typed —
    // restyling the rest of the block as a pending link would be worse than
    // the flash. (A genuine multi-line-label link still upgrades at `](`.)
    content.len() <= MAX_SPECULATIVE_LABEL_LEN && !content.contains(&b'\n')
}

/// Emit the inert pending anchor shared by every speculative link phase: NO
/// `href` (a half-typed / empty URL must not be navigable), and
/// `data-brook-pending=""` so consumers can style the anchor as a link while it
/// streams (an `<a>` without `href` gets no default link styling). The marker
/// sits exactly where `href` will land and the tail bytes (target + rel)
/// byte-match [`write_link`], so when the link completes the only diff the DOM
/// differ sees is attributes swapping on the same node (no inert→real
/// teardown). Never in committed/finalize output: speculation itself is
/// active-tail-only (`open_tail`), forced off at finalize.
fn write_pending_anchor(
    bytes: &[u8],
    text_range: &core::ops::Range<usize>,
    opts: &RenderOpts,
    out: &mut String,
) -> usize {
    out.push_str("<a data-brook-pending=\"\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">");
    let text = std::str::from_utf8(&bytes[text_range.clone()]).unwrap_or("");
    // Render the label through the SAME path a real link uses: in_link=true (no
    // nested links, §6.6) and open_tail=false (label semantics stay settled — a
    // construct still open inside the label renders literally, exactly as
    // `write_link` will render it if the label closes without it completing, so
    // the label never flip-flops when the link settles).
    let mut inner_opts = opts.clone();
    inner_opts.in_link = true;
    inner_opts.open_tail = false;
    render_inline(text, &inner_opts, out);
    out.push_str("</a>");
    bytes.len()
}

/// Does the link destination starting at `start` (just after the opening `(`)
/// describe a destination (or its optional title) that is genuinely STILL
/// STREAMING to EOF — i.e. the link has not yet been closed by a depth-0 `)`
/// nor permanently broken? Returns `true` only in that case (so the speculative
/// open-tail link fires); `false` means the parse is settled (a real link, or
/// malformed-forever → literal).
///
/// MUST mirror [`read_link_destination`] exactly so that for every byte-prefix
/// the speculative path and the real one-shot path agree on parity:
///   - skip leading ws (` `,`\t`,`\n`); same set `read_link_destination` skips.
///   - EOF reached (empty dest, still streaming) → true.
///   - immediate `)` (empty dest closed) → false (real empty link).
///   - bracketed `<…`: no closing `>` before EOF → true (still open); a
///     forbidden `\n`/`<` breaks it forever → false; a closed `>` hands off to
///     the title phase ([`post_dest_streams_to_eof`]).
///   - bare dest: walk with `(`/`)` depth; a control char `<0x20` is illegal →
///     false; a depth-0 `)` closes it → false; a space/tab/newline ENDS the
///     bare dest and hands off to the title phase; reaching EOF still inside
///     the bare dest → true (still streaming).
fn dest_streams_to_eof(bytes: &[u8], start: usize) -> bool {
    let mut i = start;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n') {
        i += 1;
    }
    let streaming = if i >= bytes.len() {
        // Empty destination, still streaming to EOF.
        true
    } else if bytes[i] == b')' {
        // Immediate `)` → empty dest already closed (real empty link).
        false
    } else if bytes[i] == b'<' {
        // Bracketed `<…>`: still streaming iff no closing `>` (and no forbidden
        // `\n`/`<`) before EOF; a closed `>` moves on to the title phase.
        let mut j = i + 1;
        loop {
            if j >= bytes.len() {
                break true; // ran to EOF inside the brackets → still streaming
            }
            match bytes[j] {
                b'>' => break post_dest_streams_to_eof(bytes, j + 1),
                b'\n' | b'<' => break false, // broken → permanently literal
                b'\\' if j + 1 < bytes.len() => j += 2,
                _ => j += 1,
            }
        }
    } else {
        // Bare destination: walk with paren depth.
        let mut depth: i32 = 0;
        let mut j = i;
        loop {
            if j >= bytes.len() {
                break true; // ran to EOF inside the bare dest → still streaming
            }
            let b = bytes[j];
            if matches!(b, b' ' | b'\t' | b'\n') {
                // Whitespace ends the bare dest → the title phase decides.
                break post_dest_streams_to_eof(bytes, j);
            }
            if b == b'\\' && j + 1 < bytes.len() {
                j += 2;
                continue;
            }
            if b == b'(' {
                depth += 1;
                j += 1;
                continue;
            }
            if b == b')' {
                if depth == 0 {
                    // Depth-0 `)` closes the destination → terminated.
                    break false;
                }
                depth -= 1;
                j += 1;
                continue;
            }
            if b < 0x20 {
                // Control char illegal in a bare destination → terminated.
                break false;
            }
            j += 1;
        }
    };
    // BELT-AND-SUSPENDERS (scanner-drift guard): if `read_link_destination`
    // would succeed here with a real closing `)`, the destination is NOT still
    // streaming, so `streaming` MUST be false. Co-located so the two scanners
    // can't silently diverge.
    debug_assert!(
        !(streaming && read_link_destination(bytes, start).is_some()),
        "dest_streams_to_eof returned true but read_link_destination found a complete \
         destination (with closing `)`) at start={start}: {:?}",
        std::str::from_utf8(&bytes[start..]).unwrap_or("<non-utf8>")
    );
    streaming
}

/// Title phase of [`dest_streams_to_eof`]: the destination has ENDED cleanly at
/// `start` (bare dest hit whitespace, or a bracketed `<…>` closed) and what
/// remains may only be optional whitespace, an optional `"…"`/`'…'`/`(…)`
/// title, more optional whitespace, then the closing `)`. Still-streaming iff
/// EOF arrives anywhere a further byte could still complete the link. Mirrors
/// `read_link_destination`'s post-destination walk byte-for-byte:
///   - ws then EOF → true (awaiting title or `)`).
///   - `)` → false (complete link — the real parse takes it).
///   - a title opener: unclosed at EOF → true (title still streaming); a blank
///     line inside → false (malformed forever); closed → optional ws, then EOF
///     → true (awaiting `)`), `)` → false (complete), else false (junk after
///     the title can never parse).
///   - any other byte where `read_link_destination` requires `)` → false.
fn post_dest_streams_to_eof(bytes: &[u8], start: usize) -> bool {
    let mut i = start;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n') {
        i += 1;
    }
    if i >= bytes.len() {
        return true;
    }
    if !matches!(bytes[i], b'"' | b'\'' | b'(') {
        // `)` → complete link; anything else → permanently malformed.
        return false;
    }
    let close = match bytes[i] {
        b'"' => b'"',
        b'\'' => b'\'',
        _ => b')',
    };
    let mut j = i + 1;
    let mut prev_was_nl = false;
    while j < bytes.len() && bytes[j] != close {
        if bytes[j] == b'\\' && j + 1 < bytes.len() {
            j += 2;
            prev_was_nl = false;
            continue;
        }
        if bytes[j] == b'\n' {
            if prev_was_nl {
                // Blank line inside the title → malformed forever.
                return false;
            }
            prev_was_nl = true;
        } else if !matches!(bytes[j], b' ' | b'\t' | b'\r') {
            prev_was_nl = false;
        }
        j += 1;
    }
    if j >= bytes.len() {
        return true; // title still streaming (no closer yet)
    }
    // Title closed at `j`; only optional ws then the final `)` may follow.
    let mut k = j + 1;
    while k < bytes.len() && matches!(bytes[k], b' ' | b'\t' | b'\n') {
        k += 1;
    }
    k >= bytes.len() // EOF → awaiting `)`; `)` → complete; junk → malformed
}

fn try_image(bytes: &[u8], start: usize, opts: &RenderOpts, out: &mut String) -> Option<usize> {
    let (alt_range, after_alt) = read_balanced_brackets(bytes, start + 1)?;
    if bytes.get(after_alt) == Some(&b'(') {
        if let Some((url, title, after_paren)) = read_link_destination(bytes, after_alt + 1) {
            return Some(write_image(bytes, &alt_range, &url, title.as_deref(), opts, out, after_paren));
        }
    }
    let (label_range_opt, end_pos) = read_optional_ref_label(bytes, after_alt);
    let label_bytes = match label_range_opt {
        Some(r) if !r.is_empty() => &bytes[r],
        _ => &bytes[alt_range.clone()],
    };
    let label = std::str::from_utf8(label_bytes).ok()?;
    if !crate::render::valid_link_label(label) {
        return None;
    }
    let r = opts.lookup(label)?;
    let url = r.url.clone();
    let title = r.title.clone();
    Some(write_image(bytes, &alt_range, &url, title.as_deref(), opts, out, end_pos))
}

fn write_image(
    bytes: &[u8],
    alt_range: &core::ops::Range<usize>,
    url: &str,
    title: Option<&str>,
    opts: &RenderOpts,
    out: &mut String,
    end_pos: usize,
) -> usize {
    out.push_str("<img src=\"");
    sanitize_image_url(url, out, &opts.allow_schemes);
    out.push_str("\" alt=\"");
    let alt_text = std::str::from_utf8(&bytes[alt_range.clone()]).unwrap_or("");
    let mut tmp = String::new();
    let opts_plain = RenderOpts::default();
    render_inline(alt_text, &opts_plain, &mut tmp);
    // `tmp` is already HTML-escaped inline output; flatten it to the alt text
    // (nested images contribute their own alt) and emit it verbatim — escaping
    // again would double-encode entities.
    out.push_str(&flatten_alt(&tmp));
    out.push('"');
    if let Some(t) = title {
        out.push_str(" title=\"");
        let decoded = crate::url::decode_text(t);
        escape_attr(&decoded, out);
        out.push('"');
    }
    out.push('>');
    end_pos
}

/// Reduce rendered inline HTML to image-alt text (§6.4): drop tags, but lift
/// the `alt` attribute of any nested `<img>`. Input is already HTML-escaped,
/// so the result is emitted verbatim. UTF-8 safe (operates on `&str` slices).
fn flatten_alt(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(lt) = rest.find('<') {
        out.push_str(&rest[..lt]);
        let after = &rest[lt..];
        let gt = after.find('>').map(|g| g + 1).unwrap_or(after.len());
        let tag = &after[..gt];
        if tag.starts_with("<img") {
            if let Some(alt) = extract_attr(tag, "alt") {
                out.push_str(alt);
            }
        }
        rest = &after[gt..];
    }
    out.push_str(rest);
    out
}

/// Extract the value of `name="..."` from an HTML start tag.
fn extract_attr<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let pat = format!("{}=\"", name);
    let start = tag.find(&pat)? + pat.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Max bracket nesting depth probed by `read_balanced_brackets`. CommonMark
/// link labels are at most a few deep in any realistic document; a deeper run of
/// `[` is an attacker trying to make the recursive nested-link probe blow up, so
/// we bail (the `[` then renders literally — output-equivalent for real input).
const MAX_BRACKET_DEPTH: usize = 32;
/// Max bracket-text length probed by `read_balanced_brackets`. A link label is
/// capped at 999 chars by CommonMark; this bound is comfortably above any real
/// label and keeps the per-`[` scan from going quadratic on unbalanced input.
const MAX_BRACKET_TEXT_LEN: usize = 8 * 1024;

/// Outcome of the balanced-bracket walk. One shared core serves both the real
/// parse ([`read_balanced_brackets`] — only `Closed` counts) and the
/// speculative open-tail label phase, which must additionally distinguish a
/// bracket that ran to buffer EOF still open (a label STILL STREAMING — may
/// speculate) from one the caps rejected (adversarial — the real parse renders
/// it literal, so speculation must too). Sharing the walk keeps the two
/// consumers incapable of drifting.
enum BracketScan {
    /// Balanced close found: (inner text range, offset just past the `]`).
    Closed(core::ops::Range<usize>, usize),
    /// Ran to EOF with the bracket still open — the text is still streaming.
    OpenAtEof,
    /// No `[` at `start`, or the depth/length caps tripped — literal.
    Reject,
}

fn scan_brackets(bytes: &[u8], start: usize) -> BracketScan {
    if bytes.get(start) != Some(&b'[') {
        return BracketScan::Reject;
    }
    let mut depth = 1;
    let mut i = start + 1;
    while i < bytes.len() {
        if depth > MAX_BRACKET_DEPTH || i - start > MAX_BRACKET_TEXT_LEN {
            return BracketScan::Reject;
        }
        match bytes[i] {
            b'\\' if i + 1 < bytes.len() => i += 2,
            b'`' => {
                // Code span takes precedence over link brackets — if a code
                // span eats past our `]`, the link parse fails.
                if let Some(after) = skip_code_span(bytes, i) {
                    i = after;
                } else {
                    i += 1;
                }
            }
            b'<' => {
                if let Some(after) = skip_autolink_or_html(bytes, i) {
                    i = after;
                } else {
                    i += 1;
                }
            }
            b'[' => {
                depth += 1;
                i += 1;
            }
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return BracketScan::Closed(start + 1..i, i + 1);
                }
                i += 1;
            }
            b'\n' if depth > 0 => i += 1,
            _ => i += 1,
        }
    }
    BracketScan::OpenAtEof
}

fn read_balanced_brackets(bytes: &[u8], start: usize) -> Option<(core::ops::Range<usize>, usize)> {
    match scan_brackets(bytes, start) {
        BracketScan::Closed(r, end) => Some((r, end)),
        _ => None,
    }
}

/// If bytes starting at `start` form a complete code span, return the byte
/// offset just after the closing backtick run. Otherwise None.
fn skip_code_span(bytes: &[u8], start: usize) -> Option<usize> {
    let mut open_len = 0;
    while start + open_len < bytes.len() && bytes[start + open_len] == b'`' {
        open_len += 1;
    }
    if open_len == 0 {
        return None;
    }
    let mut i = start + open_len;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let mut close_len = 0;
            while i + close_len < bytes.len() && bytes[i + close_len] == b'`' {
                close_len += 1;
            }
            if close_len == open_len {
                return Some(i + close_len);
            }
            i += close_len;
        } else {
            i += 1;
        }
    }
    None
}

/// Same idea for inline HTML / autolinks: returns end position if matched.
fn skip_autolink_or_html(bytes: &[u8], start: usize) -> Option<usize> {
    // Autolink first: <scheme:...>
    if let Some(end_rel) = bytes[start..].iter().position(|&b| b == b'>') {
        let end = start + end_rel;
        let inner = &bytes[start + 1..end];
        if !inner.is_empty()
            && !inner.iter().any(|&b| matches!(b, b' ' | b'\t' | b'\n' | b'<' | b'\0'))
        {
            if let Ok(s) = std::str::from_utf8(inner) {
                if is_uri_scheme(s) || (is_email_like(s) && !s.contains('\\')) {
                    return Some(end + 1);
                }
            }
        }
    }
    // Otherwise inline HTML
    match_inline_html(bytes, start).map(|n| start + n - start + start)
        .and_then(|_| match_inline_html(bytes, start).map(|n| start + n))
}

/// After `[text]`, look for an optional `[label]` reference. Returns
/// `(Some(label_range), pos_after)` for `[text][label]`,
/// `(Some(empty), pos_after)` for `[text][]`,
/// `(None, after_text)` for `[text]` (collapsed reference).
fn read_optional_ref_label(
    bytes: &[u8],
    after_text: usize,
) -> (Option<core::ops::Range<usize>>, usize) {
    if bytes.get(after_text) == Some(&b'[') {
        if let Some((r, end)) = read_balanced_brackets(bytes, after_text) {
            return (Some(r), end);
        }
    }
    (None, after_text)
}

fn read_link_destination(
    bytes: &[u8],
    start: usize,
) -> Option<(String, Option<String>, usize)> {
    let mut i = start;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n') {
        i += 1;
    }
    // Empty URL: immediate `)` after optional whitespace.
    if bytes.get(i) == Some(&b')') {
        return Some((String::new(), None, i + 1));
    }
    let (url, after_url) = if bytes.get(i) == Some(&b'<') {
        // Bracketed URL: <...> with no newlines, no unescaped < or >.
        let mut j = i + 1;
        while j < bytes.len() && bytes[j] != b'>' && bytes[j] != b'\n' && bytes[j] != b'<' {
            if bytes[j] == b'\\' && j + 1 < bytes.len() {
                j += 2;
            } else {
                j += 1;
            }
        }
        if j >= bytes.len() || bytes[j] != b'>' {
            return None;
        }
        let u = std::str::from_utf8(&bytes[i + 1..j]).ok()?.to_string();
        (u, j + 1)
    } else {
        let mut depth = 0;
        let mut j = i;
        while j < bytes.len() {
            let b = bytes[j];
            if matches!(b, b' ' | b'\t' | b'\n') {
                break;
            }
            if b == b'\\' && j + 1 < bytes.len() {
                j += 2;
                continue;
            }
            if b == b'(' {
                depth += 1;
                j += 1;
                continue;
            }
            if b == b')' {
                if depth == 0 {
                    break;
                }
                depth -= 1;
                j += 1;
                continue;
            }
            // Control chars not allowed in bare destinations.
            if b < 0x20 {
                return None;
            }
            j += 1;
        }
        if j == i {
            // The bare walk consumed nothing. Leading whitespace is already
            // skipped and an immediate `)` was handled above, so this is
            // reachable only at EOF — `[text](` cut off before any destination.
            // That is NOT a complete link (an empty destination is only valid
            // via `()` or the bracketed `<>` form, both handled above): the
            // closing-`)` check below is what the early Some used to bypass,
            // mis-rendering a paragraph ending in `[text](` as an empty-href
            // link instead of literal text (and tripping the
            // dest_streams_to_eof parity assert mid-stream).
            return None;
        }
        let u = std::str::from_utf8(&bytes[i..j]).ok()?.to_string();
        (u, j)
    };
    let mut i = after_url;
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n') {
        i += 1;
    }
    let mut title: Option<String> = None;
    if i < bytes.len() && matches!(bytes[i], b'"' | b'\'' | b'(') {
        let close = match bytes[i] {
            b'"' => b'"',
            b'\'' => b'\'',
            _ => b')',
        };
        let start_t = i + 1;
        let mut j = start_t;
        let mut had_blank = false;
        let mut prev_was_nl = false;
        while j < bytes.len() && bytes[j] != close {
            if bytes[j] == b'\\' && j + 1 < bytes.len() {
                j += 2;
                prev_was_nl = false;
                continue;
            }
            if bytes[j] == b'\n' {
                if prev_was_nl {
                    had_blank = true;
                    break;
                }
                prev_was_nl = true;
            } else if !matches!(bytes[j], b' ' | b'\t' | b'\r') {
                prev_was_nl = false;
            }
            j += 1;
        }
        if !had_blank && j < bytes.len() {
            title = Some(std::str::from_utf8(&bytes[start_t..j]).ok()?.to_string());
            i = j + 1;
        }
    }
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n') {
        i += 1;
    }
    if bytes.get(i) != Some(&b')') {
        return None;
    }
    Some((url, title, i + 1))
}

// ---------------------------------------------------------------------
// Emphasis / strong / strikethrough — delimiter stack
// ---------------------------------------------------------------------

#[derive(Debug)]
struct Delim {
    at: usize,
    /// Byte offset of this delimiter run's start in the *input* (not the output
    /// `at`). Set at push, never modified by resolution — used by the streaming
    /// boundary computation to locate emphasis spans in the source.
    in_at: usize,
    class: u8,
    len: usize,
    can_open: bool,
    can_close: bool,
}

struct DelimRun {
    len: usize,
}

fn scan_delim_run(bytes: &[u8], start: usize) -> DelimRun {
    let c = bytes[start];
    let mut len = 0;
    while start + len < bytes.len() && bytes[start + len] == c {
        len += 1;
    }
    DelimRun { len }
}

/// CommonMark §6.2 flanking rules — operates on Unicode chars, not bytes,
/// so intra-word `_` in Cyrillic and similar work correctly.
fn flanking(input: &str, start: usize, len: usize) -> (bool, bool) {
    let prev = prev_char(input, start);
    let next = if start + len < input.len() {
        input[start + len..].chars().next()
    } else {
        None
    };
    let prev_ws = prev.map_or(true, |c| c.is_whitespace());
    let next_ws = next.map_or(true, |c| c.is_whitespace());
    let prev_punct = prev.map_or(false, is_unicode_punct);
    let next_punct = next.map_or(false, is_unicode_punct);
    let left_flank = !next_ws && (!next_punct || prev_ws || prev_punct);
    let right_flank = !prev_ws && (!prev_punct || next_ws || next_punct);
    let c = input.as_bytes()[start];
    if c == b'_' {
        let prev_alnum = prev.map_or(false, |c| c.is_alphanumeric());
        let next_alnum = next.map_or(false, |c| c.is_alphanumeric());
        (
            left_flank && (!right_flank || prev_punct) && !prev_alnum,
            right_flank && (!left_flank || next_punct) && !next_alnum,
        )
    } else {
        (left_flank, right_flank)
    }
}

fn prev_char(input: &str, pos: usize) -> Option<char> {
    if pos == 0 {
        return None;
    }
    input[..pos].chars().next_back()
}

/// Unicode punctuation: ASCII puncts plus any non-alphanumeric, non-whitespace
/// char. This catches the symbol/punctuation categories without needing a
/// full Unicode property table.
fn is_unicode_punct(c: char) -> bool {
    if matches!(c, '!'..='/' | ':'..='@' | '['..='`' | '{'..='~') {
        return true;
    }
    !c.is_alphanumeric() && !c.is_whitespace()
}

/// Resolve the emphasis delimiter stack into `<em>/<strong>/<del>` edits on
/// `out`. When `pairs` is `Some`, also records each pairing as an input-position
/// span `(opener_run_start, closer_run_start)` — the streaming boundary
/// computation uses these to avoid cutting inside a resolved emphasis span.
fn resolve_delimiters(out: &mut String, stack: &mut Vec<Delim>, mut pairs: Option<&mut Vec<(usize, usize)>>) {
    let mut edits: Vec<Edit> = Vec::new();

    let n = stack.len();
    // Backward-scan accelerators, per the delimiter-stack strategy in the
    // CommonMark spec appendix. Without them a closer that can never pair
    // (e.g. the mod-3 rule permanently blocking the only opener) re-walks the
    // entire stack, making one resolution pass O(stack²).
    //
    // * `prev_link[j]` — the next index below `j` worth examining as an
    //   opener. Entries that are dead (len == 0) or can never open
    //   (!can_open) are skipped and the link compressed past them; both
    //   conditions are permanent, so compression never hides an entry that
    //   could become usable later.
    // * `bottoms[class][bucket]` — "openers_bottom": per (delimiter class,
    //   closer len % 3, closer can_open) the lowest index still worth
    //   scanning. When a closer finds no opener, everything below it is
    //   unusable for its bucket: the class/can_open/len==0 skips are
    //   permanent, and the mod-3 skip depends only on the opener's len % 3 +
    //   can_close and the bucket key. The one mutable input is an opener's
    //   len % 3 — when a pairing leaves an opener with a nonzero remainder
    //   its mod-3 class changed, so that class's bottoms drop back down to it
    //   (below), keeping the bounded scan exactly equivalent to the
    //   unbounded one.
    let mut prev_link: Vec<usize> = (0..n).map(|j| j.wrapping_sub(1)).collect();
    let mut bottoms = [[0usize; 6]; 3];
    fn class_ix(c: u8) -> usize {
        match c {
            b'*' => 0,
            b'_' => 1,
            _ => 2,
        }
    }
    let mut i = 0;
    while i < n {
        if !stack[i].can_close {
            i += 1;
            continue;
        }
        let ci = class_ix(stack[i].class);
        let bucket = stack[i].len % 3 + if stack[i].can_open { 3 } else { 0 };
        let bottom = bottoms[ci][bucket];
        let mut j = i;
        let found = loop {
            let mut p = prev_link[j];
            while p != usize::MAX && (stack[p].len == 0 || !stack[p].can_open) {
                p = prev_link[p];
            }
            prev_link[j] = p;
            if p == usize::MAX || p < bottom {
                break None;
            }
            j = p;
            let s = &stack[j];
            if s.class != stack[i].class {
                continue;
            }
            let sum_mod = (s.len + stack[i].len) % 3;
            if (s.can_close || stack[i].can_open) && sum_mod == 0 && !(s.len % 3 == 0 && stack[i].len % 3 == 0) {
                continue;
            }
            break Some(j);
        };

        if let Some(opener_idx) = found {
            let class = stack[i].class;
            let take = if class == b'~' {
                if stack[opener_idx].len >= 2 && stack[i].len >= 2 { 2 } else { 0 }
            } else if stack[opener_idx].len >= 2 && stack[i].len >= 2 {
                2
            } else {
                1
            };
            if take == 0 {
                i += 1;
                continue;
            }
            let (open_tag, close_tag) = match (class, take) {
                (b'~', 2) => ("<del>", "</del>"),
                (_, 2) => ("<strong>", "</strong>"),
                (_, 1) => ("<em>", "</em>"),
                _ => ("", ""),
            };

            let op_at = stack[opener_idx].at + stack[opener_idx].len - take;
            let cl_at = stack[i].at;
            edits.push(Edit { at: op_at, delete: take, insert: open_tag });
            edits.push(Edit { at: cl_at, delete: take, insert: close_tag });
            if let Some(p) = pairs.as_deref_mut() {
                p.push((stack[opener_idx].in_at, stack[i].in_at));
            }

            stack[opener_idx].len -= take;
            stack[i].len -= take;
            // The closer's remaining chars are to the RIGHT of what we just
            // consumed; the opener's remaining chars are to the LEFT of what
            // we consumed (its .at stays put).
            stack[i].at += take;
            if stack[opener_idx].len > 0 {
                // The partial consume changed the opener's len % 3, so a
                // bucket that proved "nothing usable below X" may now have a
                // usable opener here — drop this class's bottoms back to it.
                for b in bottoms[ci].iter_mut() {
                    *b = (*b).min(opener_idx);
                }
            }
            if stack[i].len == 0 {
                i += 1;
            }
            for k in opener_idx + 1..i {
                stack[k].len = 0;
            }
        } else {
            bottoms[ci][bucket] = i;
            i += 1;
        }
    }

    // Apply the edits by rebuilding `out` in ONE forward pass: sorted
    // ascending by `at` (stable — the merge sort keeps insertion order on
    // ties, though the edits carve disjoint ranges out of distinct delimiter
    // runs so ties can't occur), copy the untouched gap, push the tag, skip
    // the deleted delimiter chars. The previous in-place `replace_range` walk
    // memmoved the whole remaining suffix once per edit — O(pairs × len) on
    // emphasis-dense text.
    if !edits.is_empty() {
        let order = crate::sort::stable_order(&edits, |a, b| a.at <= b.at);
        let grow: usize = edits.iter().map(|e| e.insert.len().saturating_sub(e.delete)).sum();
        let mut rebuilt = String::with_capacity(out.len() + grow);
        let mut cursor = 0;
        for &oi in &order {
            let e = &edits[oi];
            rebuilt.push_str(&out[cursor..e.at]);
            rebuilt.push_str(e.insert);
            cursor = e.at + e.delete;
        }
        rebuilt.push_str(&out[cursor..]);
        *out = rebuilt;
    }
}

struct Edit {
    at: usize,
    delete: usize,
    insert: &'static str,
}

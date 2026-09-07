//! `brookmd-ffi`: a [uniffi](https://mozilla.github.io/uniffi-rs/)-annotated
//! wrapper over [`brook_md_core`], designed for consumption by
//! [uniffi-bindgen-react-native (ubrn)](https://jhugman.github.io/uniffi-bindgen-react-native/).
//!
//! It exposes the streaming markdown parser as a [`BrookSession`] object whose
//! `append`/`finalize`/`all_blocks` methods return the **JSON wire strings**
//! specified in `../../brookmd-core/WIRE.md` (wire contract v1.2.0). Those strings
//! are produced through the pure [`brook_md_core::wire`] helpers, so they are
//! byte-identical to the WASM/JS boundary by construction — a React Native
//! renderer decodes exactly the same bytes the JavaScript renderer does.
//!
//! This mirrors the `#[wasm_bindgen] BrookParser` glue in `brookmd-core`'s
//! `lib.rs` (gated behind its `wasm` feature): [`BrookSession::new`] is the bare
//! constructor (equivalent to `new BrookParser()`), and [`BrookConfig`] +
//! [`BrookSession::new_with_config`] mirror the per-stream configuration the JS
//! worker applies in `packages/brookmd/src/worker.ts` (`makeParser`).

use std::sync::{Arc, Mutex};

use brook_md_core::wire::{blocks_to_json, patch_to_json, WirePatch};
use brook_md_core::{Block, StreamParser};

uniffi::setup_scaffolding!();

/// Per-stream parser configuration, mirroring `ParserConfig` in
/// `packages/brookmd/src/types-core.ts` and the setters the JS worker's
/// `makeParser` applies (`packages/brookmd/src/worker.ts`).
///
/// The record's field defaults reproduce the worker's `?? default` behavior
/// exactly: GFM autolinks and alerts default **on** (LLM output is full of bare
/// URLs and `> [!NOTE]` callouts), everything else off. The five tag/allowlist/
/// scheme fields are optional arrays (`Option<Vec<String>>`), matching the `?:`
/// optional arrays on the TypeScript side — `None` (omitted) is the "feature off"
/// state.
///
/// Setting `html_allowlist` **or** `drop_html_tags` (even to an empty array)
/// engages the safe raw-HTML sanitizer, exactly as the worker derives its
/// `setHtmlSanitize` on-flag from `htmlAllowlist !== undefined || dropHtmlTags
/// !== undefined`.
#[derive(uniffi::Record, Clone, Debug)]
pub struct BrookConfig {
    /// GFM extended autolinks (bare `www.`/`http(s)://`/`ftp://` + emails).
    #[uniffi(default = true)]
    pub gfm_autolinks: bool,
    /// GitHub alerts (`> [!NOTE]` → styled callouts).
    #[uniffi(default = true)]
    pub gfm_alerts: bool,
    /// GFM "Disallowed Raw HTML" (tagfilter); only meaningful with `unsafe_html`.
    #[uniffi(default = false)]
    pub gfm_tagfilter: bool,
    /// GFM footnotes (`[^1]` + `[^1]:` → footnote section).
    #[uniffi(default = false)]
    pub gfm_footnotes: bool,
    /// Math: `$…$` / `\(…\)` inline and `$$…$$` / `\[…\]` display.
    #[uniffi(default = false)]
    pub gfm_math: bool,
    /// Emit `dir="auto"` on block-level text elements (mixed LTR/RTL).
    #[uniffi(default = false)]
    pub dir_auto: bool,
    /// Opt-in accessibility markup (task-list `<label>`, header `scope="col"`).
    #[uniffi(default = false)]
    pub a11y: bool,
    /// Pass raw HTML through unescaped. **Never enable for untrusted input.**
    #[uniffi(default = false)]
    pub unsafe_html: bool,
    /// Block component-tag allowlist (e.g. `["Thinking", "Callout"]`). `None` = off.
    #[uniffi(default = None)]
    pub component_tags: Option<Vec<String>>,
    /// Inline component-tag allowlist (e.g. `["tik", "cite"]`). `None` = off.
    #[uniffi(default = None)]
    pub inline_component_tags: Option<Vec<String>>,
    /// Safe raw-HTML allowlist. `Some([])` = allow all but a built-in dangerous
    /// set; `Some([tags])` = only those; `None` = sanitizer off. Setting this (or
    /// `drop_html_tags`) engages the sanitizer.
    #[uniffi(default = None)]
    pub html_allowlist: Option<Vec<String>>,
    /// Tags removed entirely by the sanitizer. Setting this (or `html_allowlist`)
    /// engages the sanitizer. `None` = off.
    #[uniffi(default = None)]
    pub drop_html_tags: Option<Vec<String>>,
    /// Opt-in structured `kind.data` channel (Heading/CodeBlock/Table/… payloads).
    #[uniffi(default = false)]
    pub block_data: bool,
    /// Opt-in wire delta mode (WIRE.md §11): active blocks re-emitted across
    /// appends serialize as verified `html_delta` splices against their
    /// previous emit instead of full `html`. A consumer that enables this MUST
    /// reconstruct active html per WIRE.md §11 (the brookmd-react-native JS
    /// layer does). Off by default — wire bytes identical to contract v1.1.0.
    #[uniffi(default = false)]
    pub wire_delta: bool,
    // ── APPEND-ONLY ZONE ───────────────────────────────────────────────────────
    // uniffi serializes a record's fields POSITIONALLY, in declaration order, and
    // the generated foreign readers (`FfiConverterBool.read(from)`, …) decode them
    // in that same sequence. Inserting a field anywhere above shifts every later
    // read and silently corrupts the wire for any binding not regenerated in
    // lockstep. New fields therefore go HERE, at the end — which is why the order
    // below no longer tracks `build_parser`'s setter order (that one mirrors the JS
    // worker's `makeParser`; see below).
    /// Render a CommonMark SOFT line break (a bare `\n` in inline content) as a
    /// `<br>` — the "GitHub comment" convention. Off by default (strict CommonMark:
    /// a soft break is whitespace). Only ever ADDS breaks; hard breaks are `<br>`
    /// either way.
    #[uniffi(default = false)]
    pub soft_breaks: bool,
    /// Un-block URL schemes that are blocked by DEFAULT — bare scheme names
    /// without the colon (`["file"]`), matched case-insensitively. `None` = the
    /// built-in policy is unchanged. This never RESTRICTS anything, and the
    /// script-executing tier (`javascript:`, `data:text/html`, …) is
    /// non-overridable — listing one here is a no-op.
    #[uniffi(default = None)]
    pub allow_schemes: Option<Vec<String>>,
    /// Lenient list indentation: a list marker followed by 6+ columns of SPACE
    /// padding yields the item's text instead of an indented code block. Off by
    /// default (strict CommonMark §5.2); useful for over-indenting model output.
    #[uniffi(default = false)]
    pub lenient_lists: bool,
    /// Extend the safe raw-HTML sanitizer to BLOCK-level raw HTML (a
    /// `<details><summary>…` block renders as real elements). Takes effect ONLY
    /// when the sanitizer is engaged (`html_allowlist`/`drop_html_tags`), and only
    /// for CommonMark HTML block types 6 and 7. Off by default.
    #[uniffi(default = false)]
    pub block_html: bool,
}

/// Build a [`StreamParser`] from a [`BrookConfig`], applying the same
/// `StreamParser::set_*` calls, in the same order, as the JS worker's
/// `makeParser` — so a native binding and the WASM boundary produce identical
/// wire for identical config.
fn build_parser(config: &BrookConfig) -> StreamParser {
    let mut p = StreamParser::new();
    p.set_gfm_autolinks(config.gfm_autolinks);
    p.set_gfm_alerts(config.gfm_alerts);
    p.set_gfm_tagfilter(config.gfm_tagfilter);
    p.set_gfm_footnotes(config.gfm_footnotes);
    p.set_gfm_math(config.gfm_math);
    p.set_dir_auto(config.dir_auto);
    p.set_lenient_lists(config.lenient_lists);
    p.set_soft_breaks(config.soft_breaks);
    p.set_a11y(config.a11y);
    p.set_unsafe_html(config.unsafe_html);
    p.set_component_tags(config.component_tags.clone().unwrap_or_default());
    p.set_inline_component_tags(config.inline_component_tags.clone().unwrap_or_default());
    // Mirror the worker: the sanitizer engages when EITHER list was provided,
    // even if empty (`htmlAllowlist !== undefined || dropHtmlTags !== undefined`).
    let sanitize_on = config.html_allowlist.is_some() || config.drop_html_tags.is_some();
    p.set_html_sanitize(
        sanitize_on,
        config.html_allowlist.clone().unwrap_or_default(),
        config.drop_html_tags.clone().unwrap_or_default(),
    );
    p.set_block_html(config.block_html);
    p.set_allow_schemes(config.allow_schemes.clone().unwrap_or_default());
    p.set_block_data(config.block_data);
    p.set_wire_delta(config.wire_delta);
    p
}

/// A [`StreamParser`] made `Send` for the FFI boundary.
///
/// `StreamParser` is `!Send`/`!Sync`: it holds non-atomic `Rc`s internally (a
/// deliberate choice for the single-threaded WASM target). uniffi objects,
/// however, are `Arc`-shared and must be `Send + Sync` on native targets. This
/// newtype exists solely to carry the `unsafe impl Send` below.
///
/// SAFETY: this type is only ever reached through [`BrookSession::inner`], a
/// `Mutex`, so every access to the parser — and therefore every non-atomic `Rc`
/// refcount operation — is serialized and has the `Mutex`'s happens-before. No
/// `Rc` escapes that guarded region: the public methods return owned `String`s
/// (or `u64`), converting/serializing under the lock and dropping the borrowed
/// `Block`/`Patch` values before returning. With no concurrent refcount mutation
/// and no aliased `Rc` on another thread, moving the parser between threads is
/// sound.
struct SendParser(StreamParser);

// SAFETY: see [`SendParser`] — access is fully serialized by `BrookSession`'s
// `Mutex` and no internal `Rc` is ever aliased across threads.
unsafe impl Send for SendParser {}

/// A single streaming-parse session: an [`Arc`]-shared, interior-mutable wrapper
/// over one [`StreamParser`]. uniffi objects are shared by `Arc` and their
/// methods take `&self`, so the parser lives behind a `Mutex` (which also
/// provides the `Sync` half, since [`SendParser`] is `Send`).
///
/// Feed input with [`append`](Self::append) (returns a `Patch` JSON string),
/// end the stream with [`finalize`](Self::finalize), read the whole document
/// with [`all_blocks`](Self::all_blocks), and start a fresh stream with
/// [`reset`](Self::reset). One `BrookSession` corresponds to one JS-worker stream
/// (one `BrookParser` instance): ids are stable and monotonic for its lifetime
/// and restart at 0 after a `reset`.
#[derive(uniffi::Object)]
pub struct BrookSession {
    inner: Mutex<SendParser>,
    /// The config the parser was built from, kept so [`reset`](Self::reset) can
    /// rebuild an identical parser (mirroring the JS worker's free-and-recreate).
    /// `None` means the bare [`new`](Self::new) constructor (library defaults).
    config: Option<BrookConfig>,
}

#[uniffi::export]
impl BrookSession {
    /// Bare constructor — equivalent to `new BrookParser()` on the WASM boundary
    /// (`StreamParser::new()`, all features at their library defaults: autolinks
    /// and alerts **off**, raw HTML escaped, block data off).
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self { inner: Mutex::new(SendParser(StreamParser::new())), config: None })
    }

    /// Configured constructor — applies `config` through the same setters the JS
    /// worker's `makeParser` uses (see [`BrookConfig`]).
    #[uniffi::constructor]
    pub fn new_with_config(config: BrookConfig) -> Arc<Self> {
        Arc::new(Self { inner: Mutex::new(SendParser(build_parser(&config))), config: Some(config) })
    }

    /// Feed the next chunk of markdown. Returns a `Patch` as a JSON string
    /// (`{"newly_committed":[…],"active":[…]}`) — the exact bytes of the WASM
    /// `BrookParser.append` boundary (WIRE.md §2). Parse it once with any JSON
    /// decoder.
    pub fn append(&self, chunk: String) -> String {
        let mut p = self.inner.lock().expect("BrookSession parser mutex poisoned");
        patch_to_json(&WirePatch::from(p.0.append(&chunk)))
    }

    /// End the stream: emit any still-open blocks as committed. Returns a final
    /// `Patch` JSON string (WIRE.md §2). After this, `append` is a no-op that
    /// returns an empty patch, matching the core parser.
    pub fn finalize(&self) -> String {
        let mut p = self.inner.lock().expect("BrookSession parser mutex poisoned");
        patch_to_json(&WirePatch::from(p.0.finalize()))
    }

    /// The whole parsed document (committed + active) in order, as a JSON string
    /// of a `Block[]` — the bytes of the WASM `BrookParser.allBlocks` boundary
    /// (WIRE.md §1). The one-shot / SSR primitive: `append` the full source,
    /// `finalize`, then read this.
    pub fn all_blocks(&self) -> String {
        let p = self.inner.lock().expect("BrookSession parser mutex poisoned");
        let blocks: Vec<&Block> = p.0.all_blocks().collect();
        blocks_to_json(&blocks)
    }

    /// Discard all parse state and start a fresh stream, preserving this
    /// session's configuration. Mirrors the JS worker's `reset` (free the parser
    /// and recreate it lazily with the same config): block ids restart from 0.
    pub fn reset(&self) {
        let mut p = self.inner.lock().expect("BrookSession parser mutex poisoned");
        p.0 = match &self.config {
            Some(c) => build_parser(c),
            None => StreamParser::new(),
        };
    }

    /// Total bytes the parser is retaining (source buffer + all rendered HTML for
    /// committed and active blocks). For comparing per-session memory cost.
    pub fn retained_bytes(&self) -> u64 {
        self.inner.lock().expect("BrookSession parser mutex poisoned").0.retained_bytes() as u64
    }

    /// Length in bytes of the retained source buffer (line endings normalized to
    /// `\n`); block `start`/`end` offsets index into it.
    pub fn buffer_len(&self) -> u64 {
        self.inner.lock().expect("BrookSession parser mutex poisoned").0.buffer().len() as u64
    }
}

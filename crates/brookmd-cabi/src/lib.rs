//! `brookmd-cabi`: a plain **C-ABI** wrapper over [`brook_md_core`], designed for
//! consumption from Dart (`dart:ffi`) by the `brook_md` Flutter package, and by any
//! other language with a C FFI. The hand-written C header is `include/brook_md.h`.
//!
//! It exposes the streaming markdown parser as an opaque [`BrookSession`] whose
//! `append`/`finalize`/`all_blocks` functions return the **JSON wire strings**
//! specified in `../../brookmd-core/WIRE.md` (wire contract v1.2.0). Those strings
//! are produced through the pure [`brook_md_core::wire`] helpers, so they are
//! byte-identical to the WASM/JS boundary and to the uniffi (`brookmd-ffi`) layer
//! by construction — a native renderer decodes exactly the same bytes.
//!
//! This mirrors `brookmd-ffi`'s uniffi `BrookSession`, but with no bindgen: the
//! surface here is a small set of `#[no_mangle] extern "C"` functions.
//!
//! ## Ownership and safety contract (see also `include/brook_md.h`)
//!
//! - A `*mut BrookSession` is created by [`brook_session_new`] /
//!   [`brook_session_new_with_config`] and **must** be released with
//!   [`brook_session_free`] exactly once. Passing it to `free` twice is UB, exactly
//!   like C `free()`.
//! - Every function returning `*mut c_char` transfers ownership of a
//!   NUL-terminated, UTF-8 JSON string to the caller; free it with
//!   [`brook_string_free`] exactly once. Do **not** free it with libc `free`.
//! - [`brook_wire_version`] returns a pointer to a static string; it must **not**
//!   be freed.
//! - A `BrookSession` is **not** internally synchronized. A single session must not
//!   be used from multiple threads concurrently (the usual C convention). Distinct
//!   sessions on distinct threads are fine.
//! - Every export tolerates a NULL session/pointer argument (returns NULL/0, never
//!   crashes) and wraps its body in [`std::panic::catch_unwind`], returning NULL/0
//!   if the parser were ever to panic (unwinding across `extern "C"` is UB).
//!   A caught panic leaves the session memory-safe but in an unspecified parse
//!   state; after a NULL return that isn't explained by a NULL argument, call
//!   [`brook_session_reset`] (or free the session) before feeding more input.

// This crate is, in its entirety, a raw-pointer C ABI: every pointer-taking export
// dereferences a caller-supplied pointer by design. We keep the exports as plain
// (non-`unsafe`) `extern "C"` functions — that is how C consumers and dart:ffi see
// them, and it lets the in-crate tests call them without ceremony — while the real
// safety contract lives in each function's `# Safety` doc section and is upheld at
// the C boundary. So the `not_unsafe_ptr_arg_deref` lint is expected here.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;
use std::slice;

use serde::Deserialize;

use brook_md_core::wire::{blocks_to_json, patch_to_json, WirePatch};
use brook_md_core::{Block, StreamParser};

/// Wire contract version this crate emits (see `../../brookmd-core/WIRE.md`).
/// Returned by [`brook_wire_version`]. A `\0`-terminated static C string.
const WIRE_VERSION: &CStr = c"1.2.0";

/// Per-stream parser configuration, deserialized from the JSON object passed to
/// [`brook_session_new_with_config`]. Field names are the **snake_case** keys of
/// `BrookConfig` in `brookmd-ffi` (and of `ParserConfig` on the JS side, mapped to
/// snake_case). Unknown keys are ignored; missing keys take the defaults below.
///
/// The defaults reproduce the JS worker's `?? default` behavior exactly (and match
/// `brookmd-ffi`'s uniffi `BrookConfig`): GFM autolinks and alerts default **on**
/// (LLM output is full of bare URLs and `> [!NOTE]` callouts), everything else
/// off. The five tag/allowlist/scheme fields are optional arrays; `None` (omitted)
/// is the "feature off" state.
///
/// Setting `html_allowlist` **or** `drop_html_tags` (even to an empty array)
/// engages the safe raw-HTML sanitizer, exactly as the worker derives its
/// on-flag from `htmlAllowlist !== undefined || dropHtmlTags !== undefined`.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
struct BrookConfig {
    /// GFM extended autolinks (bare `www.`/`http(s)://`/`ftp://` + emails).
    gfm_autolinks: bool,
    /// GitHub alerts (`> [!NOTE]` → styled callouts).
    gfm_alerts: bool,
    /// GFM "Disallowed Raw HTML" (tagfilter); only meaningful with `unsafe_html`.
    gfm_tagfilter: bool,
    /// GFM footnotes (`[^1]` + `[^1]:` → footnote section).
    gfm_footnotes: bool,
    /// Math: `$…$` / `\(…\)` inline and `$$…$$` / `\[…\]` display.
    gfm_math: bool,
    /// Emit `dir="auto"` on block-level text elements (mixed LTR/RTL).
    dir_auto: bool,
    /// Opt-in accessibility markup (task-list `<label>`, header `scope="col"`).
    a11y: bool,
    /// Pass raw HTML through unescaped. **Never enable for untrusted input.**
    unsafe_html: bool,
    /// Block component-tag allowlist (e.g. `["Thinking", "Callout"]`). `None` = off.
    component_tags: Option<Vec<String>>,
    /// Inline component-tag allowlist (e.g. `["tik", "cite"]`). `None` = off.
    inline_component_tags: Option<Vec<String>>,
    /// Safe raw-HTML allowlist. `Some([])` = allow all but a built-in dangerous
    /// set; `Some([tags])` = only those; `None` = sanitizer off. Setting this (or
    /// `drop_html_tags`) engages the sanitizer.
    html_allowlist: Option<Vec<String>>,
    /// Tags removed entirely by the sanitizer. Setting this (or `html_allowlist`)
    /// engages the sanitizer. `None` = off.
    drop_html_tags: Option<Vec<String>>,
    /// Opt-in structured `kind.data` channel (Heading/CodeBlock/Table/… payloads).
    block_data: bool,
    /// Opt-in wire delta mode (WIRE.md §11): active blocks re-emitted across
    /// appends serialize as verified `html_delta` splices against their
    /// previous emit instead of full `html`. A consumer that enables this MUST
    /// reconstruct active html per WIRE.md §11. Off by default (v1 wire bytes).
    wire_delta: bool,
    // ── APPEND-ONLY ZONE ───────────────────────────────────────────────────────
    // This struct is JSON-keyed (order-irrelevant here), but it is the documented
    // mirror of `brookmd-ffi`'s uniffi `BrookConfig`, which uniffi serializes
    // POSITIONALLY. Keep the two field lists append-identical so the snake_case
    // key set stays a 1:1 map of the uniffi record.
    /// Render a CommonMark SOFT line break (a bare `\n`) as a `<br>` — the
    /// "GitHub comment" convention. Off by default (strict CommonMark).
    soft_breaks: bool,
    /// Un-block URL schemes blocked by DEFAULT — bare scheme names without the
    /// colon (`["file"]`), case-insensitive. `None` = built-in policy unchanged.
    /// Never restricts; the script-executing tier is non-overridable.
    allow_schemes: Option<Vec<String>>,
    /// Lenient list indentation: a marker followed by 6+ columns of SPACE padding
    /// yields the item's text instead of an indented code block. Off by default.
    lenient_lists: bool,
    /// Extend the safe raw-HTML sanitizer to BLOCK-level raw HTML. Takes effect
    /// ONLY when the sanitizer is engaged, and only for HTML block types 6 and 7.
    /// Off by default.
    block_html: bool,
}

impl Default for BrookConfig {
    fn default() -> Self {
        // Mirrors brookmd-ffi's uniffi `BrookConfig` defaults: autolinks + alerts
        // on, everything else off / None. `#[serde(default)]` fills each missing
        // JSON key from here, so `{}` yields exactly this config.
        BrookConfig {
            gfm_autolinks: true,
            gfm_alerts: true,
            gfm_tagfilter: false,
            gfm_footnotes: false,
            gfm_math: false,
            dir_auto: false,
            a11y: false,
            unsafe_html: false,
            component_tags: None,
            inline_component_tags: None,
            html_allowlist: None,
            drop_html_tags: None,
            block_data: false,
            wire_delta: false,
            soft_breaks: false,
            allow_schemes: None,
            lenient_lists: false,
            block_html: false,
        }
    }
}

/// Build a [`StreamParser`] from a [`BrookConfig`], applying the same
/// `StreamParser::set_*` calls, in the same order, as `brookmd-ffi`'s
/// `build_parser` and the JS worker's `makeParser` — so a native binding and the
/// WASM boundary produce identical wire for identical config.
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

/// One streaming-parse session: a [`StreamParser`] plus the config it was built
/// from (kept so [`brook_session_reset`] can rebuild an identical parser, mirroring
/// the JS worker's free-and-recreate). `config = None` means the bare
/// [`brook_session_new`] constructor (library defaults).
///
/// Opaque to C: only ever handled through a `*mut BrookSession` pointer.
pub struct BrookSession {
    parser: StreamParser,
    config: Option<BrookConfig>,
}

/// Move a Rust `String` into a caller-owned, NUL-terminated C string. Returns NULL
/// if the string somehow contained an interior NUL — it cannot for our wire output
/// (the parser replaces NUL with U+FFFD per CommonMark, so the JSON is NUL-free;
/// the `no_interior_nul_in_output` test pins this).
fn string_into_c(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(cs) => cs.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// Create a new session with library defaults (equivalent to `StreamParser::new()`
/// / the WASM `new BrookParser()`: GFM autolinks and alerts **off**, raw HTML
/// escaped, block data off).
///
/// Returns an owning pointer the caller must release with [`brook_session_free`],
/// or NULL if allocation panicked.
#[no_mangle]
pub extern "C" fn brook_session_new() -> *mut BrookSession {
    catch_unwind(|| {
        Box::into_raw(Box::new(BrookSession { parser: StreamParser::new(), config: None }))
    })
    .unwrap_or(ptr::null_mut())
}

/// Create a new session from a JSON config object.
///
/// `config_json` must be a NUL-terminated, UTF-8 C string holding a JSON object
/// whose keys are `BrookConfig`'s snake_case field names (unknown keys ignored;
/// missing keys take library defaults — autolinks/alerts on, the rest off/None).
///
/// Returns an owning pointer (free with [`brook_session_free`]), or **NULL** if
/// `config_json` is NULL, is not valid UTF-8, or is not a valid JSON object.
///
/// # Safety
/// `config_json`, if non-NULL, must point to a valid NUL-terminated C string.
#[no_mangle]
pub extern "C" fn brook_session_new_with_config(config_json: *const c_char) -> *mut BrookSession {
    catch_unwind(|| {
        if config_json.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: non-NULL checked above; caller guarantees a valid NUL-terminated
        // C string per the header contract.
        let cstr = unsafe { CStr::from_ptr(config_json) };
        let json = match cstr.to_str() {
            Ok(s) => s,
            Err(_) => return ptr::null_mut(), // invalid UTF-8
        };
        let config: BrookConfig = match serde_json::from_str(json) {
            Ok(c) => c,
            Err(_) => return ptr::null_mut(), // invalid / non-object JSON
        };
        let parser = build_parser(&config);
        Box::into_raw(Box::new(BrookSession { parser, config: Some(config) }))
    })
    .unwrap_or(ptr::null_mut())
}

/// Feed the next chunk of markdown (length-based; the bytes need **not** be
/// NUL-terminated). Invalid UTF-8 in the chunk is repaired with U+FFFD via a lossy
/// conversion (it is not rejected). `chunk` may be NULL only when `len == 0`.
///
/// Returns a caller-owned NUL-terminated JSON `Patch` string
/// (`{"newly_committed":[…],"active":[…]}`, WIRE.md §2) — free with
/// [`brook_string_free`] — or NULL if `s` is NULL, `chunk` is NULL with `len != 0`,
/// or a panic was caught.
///
/// # Safety
/// `s`, if non-NULL, must be a live pointer from a `brook_session_new*` call;
/// `chunk`, if non-NULL, must point to at least `len` readable bytes.
#[no_mangle]
pub extern "C" fn brook_session_append(
    s: *mut BrookSession,
    chunk: *const u8,
    len: usize,
) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: non-NULL check via as_mut; the session outlives this call
        // (single-threaded use per the header contract).
        let session = match unsafe { s.as_mut() } {
            Some(session) => session,
            None => return ptr::null_mut(),
        };
        let bytes: &[u8] = if chunk.is_null() {
            if len != 0 {
                return ptr::null_mut();
            }
            &[]
        } else {
            // SAFETY: caller guarantees `len` readable bytes at `chunk`.
            unsafe { slice::from_raw_parts(chunk, len) }
        };
        let text = String::from_utf8_lossy(bytes);
        string_into_c(patch_to_json(&WirePatch::from(session.parser.append(&text))))
    }))
    .unwrap_or(ptr::null_mut())
}

/// End the stream: emit any still-open blocks as committed. Returns a caller-owned
/// NUL-terminated JSON `Patch` string (WIRE.md §2), or NULL on NULL session /
/// caught panic. After this, `append` returns an empty patch (matching the core).
///
/// # Safety
/// `s`, if non-NULL, must be a live pointer from a `brook_session_new*` call.
#[no_mangle]
pub extern "C" fn brook_session_finalize(s: *mut BrookSession) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: see `brook_session_append`.
        let session = match unsafe { s.as_mut() } {
            Some(session) => session,
            None => return ptr::null_mut(),
        };
        string_into_c(patch_to_json(&WirePatch::from(session.parser.finalize())))
    }))
    .unwrap_or(ptr::null_mut())
}

/// The whole parsed document (committed + active) in order, as a caller-owned
/// NUL-terminated JSON `Block[]` string (WIRE.md §1). Returns NULL on NULL session
/// / caught panic. Free with [`brook_string_free`].
///
/// # Safety
/// `s`, if non-NULL, must be a live pointer from a `brook_session_new*` call.
#[no_mangle]
pub extern "C" fn brook_session_all_blocks(s: *mut BrookSession) -> *mut c_char {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: see `brook_session_append`.
        let session = match unsafe { s.as_ref() } {
            Some(session) => session,
            None => return ptr::null_mut(),
        };
        let blocks: Vec<&Block> = session.parser.all_blocks().collect();
        string_into_c(blocks_to_json(&blocks))
    }))
    .unwrap_or(ptr::null_mut())
}

/// Discard all parse state and start a fresh stream, preserving this session's
/// configuration (mirrors the JS worker's free-and-recreate): block ids restart
/// from 0. No-op if `s` is NULL.
///
/// # Safety
/// `s`, if non-NULL, must be a live pointer from a `brook_session_new*` call.
#[no_mangle]
pub extern "C" fn brook_session_reset(s: *mut BrookSession) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: see `brook_session_append`.
        if let Some(session) = unsafe { s.as_mut() } {
            session.parser = match &session.config {
                Some(c) => build_parser(c),
                None => StreamParser::new(),
            };
        }
    }));
}

/// Total bytes the parser is retaining (source buffer + all rendered HTML for
/// committed and active blocks). Returns 0 on NULL session / caught panic.
///
/// # Safety
/// `s`, if non-NULL, must be a live pointer from a `brook_session_new*` call.
#[no_mangle]
pub extern "C" fn brook_session_retained_bytes(s: *mut BrookSession) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: see `brook_session_append`.
        match unsafe { s.as_ref() } {
            Some(session) => session.parser.retained_bytes() as u64,
            None => 0,
        }
    }))
    .unwrap_or(0)
}

/// Length in bytes of the retained source buffer (line endings normalized to
/// `\n`); block `start`/`end` offsets index into it. Returns 0 on NULL session /
/// caught panic.
///
/// # Safety
/// `s`, if non-NULL, must be a live pointer from a `brook_session_new*` call.
#[no_mangle]
pub extern "C" fn brook_session_buffer_len(s: *mut BrookSession) -> u64 {
    catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: see `brook_session_append`.
        match unsafe { s.as_ref() } {
            Some(session) => session.parser.buffer().len() as u64,
            None => 0,
        }
    }))
    .unwrap_or(0)
}

/// Release a session created by `brook_session_new*`. No-op if `s` is NULL. Calling
/// it twice on the same pointer is undefined behavior, exactly like C `free()`.
///
/// # Safety
/// `s` must be NULL or a pointer returned by a `brook_session_new*` call that has
/// not already been freed.
#[no_mangle]
pub extern "C" fn brook_session_free(s: *mut BrookSession) {
    if s.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: non-NULL, owning pointer from `Box::into_raw`; reclaim and drop.
        unsafe { drop(Box::from_raw(s)) };
    }));
}

/// Free a string returned by `brook_session_append` / `finalize` / `all_blocks`.
/// No-op if `ptr` is NULL. Do **not** pass a pointer from anywhere else, and do
/// not free the same pointer twice.
///
/// # Safety
/// `ptr` must be NULL or a pointer returned by one of this crate's string-returning
/// functions that has not already been freed.
#[no_mangle]
pub extern "C" fn brook_string_free(ptr: *mut c_char) {
    if ptr.is_null() {
        return;
    }
    let _ = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: non-NULL pointer originally from `CString::into_raw`.
        unsafe { drop(CString::from_raw(ptr)) };
    }));
}

/// The wire contract version string (`"1.2.0"`). Returns a pointer to a **static**
/// NUL-terminated string that must **NOT** be freed and lives for the program's
/// lifetime.
#[no_mangle]
pub extern "C" fn brook_wire_version() -> *const c_char {
    WIRE_VERSION.as_ptr()
}

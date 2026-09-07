/// EXPERIMENTAL Dart/Flutter bindings for the brookmd streaming markdown parser.
///
/// Feed markdown chunks with [BrookSession.append]; each call returns a JSON
/// `Patch` string as specified by the brookmd wire contract (WIRE.md v1.0.0). The
/// bytes are byte-identical to the WebAssembly/JS and React Native boundaries —
/// decode them with `dart:convert`'s `jsonDecode` and render as you like.
///
/// The native library (`libbrook_md_cabi`) is built from `crates/brookmd-cabi`; see
/// the package README for how it is produced and located per platform.
library;

import 'dart:convert';
import 'dart:ffi' as ffi;
import 'dart:io' show Platform;

import 'package:ffi/ffi.dart';

import 'src/bindings.dart';

/// Open the brookmd native library for the current platform.
///
/// - Android / Linux: `libbrook_md_cabi.so`
/// - macOS: `libbrook_md_cabi.dylib`
/// - Windows: `brook_md_cabi.dll`
/// - iOS: statically linked into the app binary, so symbols resolve from the
///   process itself ([ffi.DynamicLibrary.process]).
ffi.DynamicLibrary _openLibrary() {
  const base = 'brook_md_cabi';
  if (Platform.isAndroid || Platform.isLinux) {
    return ffi.DynamicLibrary.open('lib$base.so');
  }
  if (Platform.isMacOS) {
    return ffi.DynamicLibrary.open('lib$base.dylib');
  }
  if (Platform.isWindows) {
    return ffi.DynamicLibrary.open('$base.dll');
  }
  if (Platform.isIOS) {
    return ffi.DynamicLibrary.process();
  }
  throw UnsupportedError('brook_md: no native library mapping for this platform');
}

/// The process-wide, lazily-resolved binding table. All sessions share it.
final BrookBindings _bindings = BrookBindings(_openLibrary());

/// Per-stream parser configuration. Every field is optional; only the fields you
/// set are serialized, and any omitted field takes the native default (GFM
/// autolinks and alerts **on**, everything else off) — matching the JS/RN
/// boundaries. JSON keys are the C ABI's snake_case names.
class BrookConfig {
  const BrookConfig({
    this.gfmAutolinks,
    this.gfmAlerts,
    this.gfmTagfilter,
    this.gfmFootnotes,
    this.gfmMath,
    this.dirAuto,
    this.a11y,
    this.unsafeHtml,
    this.componentTags,
    this.inlineComponentTags,
    this.htmlAllowlist,
    this.dropHtmlTags,
    this.blockData,
    this.wireDelta,
    this.softBreaks,
    this.allowSchemes,
    this.lenientLists,
    this.blockHtml,
  });

  /// GFM extended autolinks (bare `www.`/`http(s)://`/`ftp://` + emails). Default on.
  final bool? gfmAutolinks;

  /// GitHub alerts (`> [!NOTE]` → styled callouts). Default on.
  final bool? gfmAlerts;

  /// GFM "Disallowed Raw HTML" (tagfilter); only meaningful with [unsafeHtml].
  final bool? gfmTagfilter;

  /// GFM footnotes (`[^1]` + `[^1]:`).
  final bool? gfmFootnotes;

  /// Math: `$…$` / `\(…\)` inline and `$$…$$` / `\[…\]` display.
  final bool? gfmMath;

  /// Emit `dir="auto"` on block-level text elements.
  final bool? dirAuto;

  /// Opt-in accessibility markup.
  final bool? a11y;

  /// Pass raw HTML through unescaped. Never enable for untrusted input.
  final bool? unsafeHtml;

  /// Block component-tag allowlist (e.g. `['Thinking', 'Callout']`).
  final List<String>? componentTags;

  /// Inline component-tag allowlist (e.g. `['tik', 'cite']`).
  final List<String>? inlineComponentTags;

  /// Safe raw-HTML allowlist. Setting this (or [dropHtmlTags]) engages the
  /// sanitizer; an empty list allows all but a built-in dangerous set.
  final List<String>? htmlAllowlist;

  /// Tags removed entirely by the sanitizer. Setting this (or [htmlAllowlist])
  /// engages the sanitizer.
  final List<String>? dropHtmlTags;

  /// Opt-in structured `kind.data` channel (Heading/CodeBlock/Table/… payloads).
  final bool? blockData;

  /// Opt-in wire delta mode (WIRE.md §11): active blocks re-emitted across
  /// appends serialize as `html_delta` splices against their previous emit
  /// instead of full `html`. Enabling it obliges you to reconstruct active html
  /// per WIRE.md §11. Default off (v1 wire bytes).
  final bool? wireDelta;

  /// Render a CommonMark SOFT line break (a bare `\n`) as a `<br>` — the
  /// "GitHub comment" convention. Default off (strict CommonMark).
  final bool? softBreaks;

  /// Un-block URL schemes blocked by DEFAULT — bare scheme names without the
  /// colon (`['file']`), case-insensitive. Never restricts; the script-executing
  /// tier is non-overridable.
  final List<String>? allowSchemes;

  /// Lenient list indentation: a marker followed by 6+ columns of SPACE padding
  /// yields the item's text instead of an indented code block. Default off.
  final bool? lenientLists;

  /// Extend the safe raw-HTML sanitizer to BLOCK-level raw HTML. Takes effect
  /// only when the sanitizer is engaged ([htmlAllowlist] or [dropHtmlTags]), and
  /// only for HTML block types 6 and 7. Default off.
  final bool? blockHtml;

  /// JSON object with only the explicitly-set keys (omitted keys → native default).
  Map<String, Object?> toJson() {
    final map = <String, Object?>{};
    void put(String key, Object? value) {
      if (value != null) map[key] = value;
    }

    put('gfm_autolinks', gfmAutolinks);
    put('gfm_alerts', gfmAlerts);
    put('gfm_tagfilter', gfmTagfilter);
    put('gfm_footnotes', gfmFootnotes);
    put('gfm_math', gfmMath);
    put('dir_auto', dirAuto);
    put('a11y', a11y);
    put('unsafe_html', unsafeHtml);
    put('component_tags', componentTags);
    put('inline_component_tags', inlineComponentTags);
    put('html_allowlist', htmlAllowlist);
    put('drop_html_tags', dropHtmlTags);
    put('block_data', blockData);
    put('wire_delta', wireDelta);
    put('soft_breaks', softBreaks);
    put('allow_schemes', allowSchemes);
    put('lenient_lists', lenientLists);
    put('block_html', blockHtml);
    return map;
  }
}

/// A single streaming-parse session over the native parser. Create one per stream,
/// feed it with [append], end it with [finalize], and **always** [dispose] it when
/// done to release the native allocation.
///
/// A session is not internally synchronized: do not use one instance from multiple
/// isolates/threads at once (the underlying C ABI has the same rule).
class BrookSession {
  BrookSession._(this._handle);

  final ffi.Pointer<BrookSessionHandle> _handle;
  bool _disposed = false;

  /// Create a session. With no [config], the parser uses library defaults
  /// (autolinks/alerts on, raw HTML escaped, block data off).
  factory BrookSession({BrookConfig? config}) {
    final handle = config == null ? _bindings.sessionNew() : _newWithConfig(config);
    if (handle == ffi.nullptr) {
      throw StateError('brook_md: failed to create session (invalid config?)');
    }
    return BrookSession._(handle);
  }

  static ffi.Pointer<BrookSessionHandle> _newWithConfig(BrookConfig config) {
    final jsonBytes = jsonEncode(config.toJson()).toNativeUtf8();
    try {
      return _bindings.sessionNewWithConfig(jsonBytes.cast<ffi.Char>());
    } finally {
      malloc.free(jsonBytes);
    }
  }

  /// The wire contract version the native library emits (e.g. `"1.0.0"`).
  static String wireVersion() =>
      _bindings.wireVersion().cast<Utf8>().toDartString();

  /// Feed the next markdown [chunk]; returns the JSON `Patch` string (WIRE.md §2).
  String append(String chunk) {
    _checkAlive();
    final bytes = utf8.encode(chunk);
    // Allocate at least one byte so the pointer is non-null even for an empty
    // chunk (the C side reads exactly `len` bytes, so 0 is a valid empty append).
    final len = bytes.length;
    final buffer = malloc<ffi.Uint8>(len == 0 ? 1 : len);
    if (len > 0) {
      buffer.asTypedList(len).setAll(0, bytes);
    }
    try {
      return _takeString(_bindings.sessionAppend(_handle, buffer, len));
    } finally {
      malloc.free(buffer);
    }
  }

  /// End the stream: still-open blocks are emitted as committed. Returns the final
  /// JSON `Patch` string.
  String finalize() {
    _checkAlive();
    return _takeString(_bindings.sessionFinalize(_handle));
  }

  /// The whole parsed document (committed + active) as a JSON `Block[]` string.
  String allBlocks() {
    _checkAlive();
    return _takeString(_bindings.sessionAllBlocks(_handle));
  }

  /// Discard parse state and start a fresh stream, preserving this session's
  /// config (block ids restart from 0).
  void reset() {
    _checkAlive();
    _bindings.sessionReset(_handle);
  }

  /// Total bytes the parser is retaining (source buffer + rendered HTML).
  int retainedBytes() {
    _checkAlive();
    return _bindings.sessionRetainedBytes(_handle);
  }

  /// Length in bytes of the retained source buffer.
  int bufferLen() {
    _checkAlive();
    return _bindings.sessionBufferLen(_handle);
  }

  /// Release the native session. Idempotent; further calls throw.
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    _bindings.sessionFree(_handle);
  }

  /// Copy a C string returned by an export into a Dart [String], then free it with
  /// the correct deallocator (`brook_string_free`, never libc free).
  String _takeString(ffi.Pointer<ffi.Char> ptr) {
    if (ptr == ffi.nullptr) {
      throw StateError('brook_md: native call returned NULL');
    }
    try {
      return ptr.cast<Utf8>().toDartString();
    } finally {
      _bindings.stringFree(ptr);
    }
  }

  void _checkAlive() {
    if (_disposed) {
      throw StateError('brook_md: session used after dispose()');
    }
  }
}

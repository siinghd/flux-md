use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;
use std::rc::Rc;

/// One coarse-grained block in the document. IDs are monotonic and stable for
/// the lifetime of the parser instance; once a block is assigned ID N, no
/// subsequent re-parse will renumber it.
#[derive(Debug, Clone, Serialize)]
pub struct Block {
    pub id: u64,
    pub kind: BlockKind,
    /// Byte offset of the first character of the block in the source buffer.
    pub start: usize,
    /// Byte offset just past the last character (or buffer end if still open).
    pub end: usize,
    /// Serialized HTML for this block, ready to inject into the DOM. Always
    /// produced through the safe-allowlist serializer so it is XSS-safe to
    /// render via innerHTML.
    pub html: String,
    /// Whether this block is still being built (true) or has been
    /// definitively closed (false). Open blocks may change on the next append.
    pub open: bool,
    /// True if `open == false` but the close happened via speculation rather
    /// than from a real terminator. The next chunk may revise the block.
    pub speculative: bool,
}

/// The block-kind discriminant plus its per-kind structured-data payload. This
/// is the single carrier for the opt-in `kind.data` channel: most kinds always
/// carry a cheap scalar/struct payload (Heading level, CodeBlock lang, List
/// ordered, Alert kind, Component tag/attrs); the heavier opt-in payloads ride
/// behind an `Option` (today: `Table(Option<TableData>)`, populated only when
/// `block_data` is on) so that a single variant — not a paired bare/with-data
/// variant — covers both the off and on wire shapes.
///
/// Serialization is HAND-WRITTEN (see `impl Serialize for BlockKind` below)
/// rather than derived. That impl is the *one* place the `{ "type", "data" }`
/// envelope is produced, and it crosses the WASM boundary via
/// `serde_wasm_bindgen::to_value` as well as `serde_json` in tests. Hand-writing
/// it lets a single variant emit either `{"type":"X"}` (no `data` key) or
/// `{"type":"X","data":…}` depending on an `Option`, which the derive cannot do
/// (adjacent tagging emits `data: null` for a `None` newtype, breaking the
/// byte-identical-off contract for `Table`). The nested object shapes are kept
/// derive-checked via small helper structs (`CodeBlockData`, `ListData`,
/// `AlertData`, `ComponentData`) so a hand-typo cannot silently drift the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    /// An ATX or Setext heading. `level` is 1..=6. `rich` is the opt-in
    /// structured channel (`setBlockData`): `None` (default-off) ⇒ serializes as
    /// `{"type":"Heading","data":<level>}` — a naked int, byte-identical to
    /// before; `Some(HeadingData)` (on) ⇒ `{"type":"Heading","data":{level,text,
    /// id}}` carrying the heading's plaintext (inline markup stripped) and a
    /// GitHub-style slug `id` so a consumer can build a table of contents with
    /// anchor links from DATA, without re-parsing the rendered HTML. Like
    /// `Table(Option<TableData>)`, a single `Option`-bearing field — not a paired
    /// bare/with-data variant — covers both wire shapes (the generic carrier).
    Heading { level: u8, rich: Option<HeadingData> },
    /// A fenced or indented code block. `lang` is the always-on info-string
    /// language (`None` for none). `code` is the opt-in structured channel
    /// (`setBlockData`): `None` (default-off) ⇒ serializes as
    /// `{"type":"CodeBlock","data":{"lang":<...>}}`, byte-identical to before;
    /// `Some(src)` (on) ⇒ `{"type":"CodeBlock","data":{"lang":<...>,"code":"<src>"}}`
    /// carrying the DECODED source text inside `<pre><code>…</code></pre>` so a
    /// consumer can build a copy-to-clipboard string / re-highlight from DATA
    /// without re-parsing (and entity-decoding) the rendered HTML. The opt-in
    /// `code` rides behind `#[serde(skip_serializing_if)]` so the off wire stays
    /// byte-identical.
    CodeBlock { lang: Option<String>, code: Option<String> },
    /// A display-math block (`$$…$$` / `\[…\]` / a fenced `math` block). The
    /// `Option<MathBlockData>` is the opt-in structured channel (`setBlockData`):
    /// `None` (default-off) ⇒ serializes as `{"type":"MathBlock"}` with no `data`
    /// key, byte-identical to before; `Some(md)` (on) ⇒
    /// `{"type":"MathBlock","data":{"latex":"<src>"}}` carrying the DECODED LaTeX
    /// source so a consumer can re-render with KaTeX from DATA without re-parsing
    /// (and entity-decoding) the display HTML. The single `Option`-bearing variant
    /// is the generic carrier (like `Table(Option<TableData>)`).
    MathBlock(Option<MathBlockData>),
    Mermaid,
    /// An ordered or unordered list. `ordered` is the always-on flag. `start` is
    /// the opt-in structured channel (`setBlockData`): `None` (default-off) ⇒
    /// serializes as `{"type":"List","data":{"ordered":<bool>}}`, byte-identical
    /// to before; `Some(n)` (on) ⇒ `{"type":"List","data":{"ordered":<bool>,
    /// "start":<n>}}` carrying the ordered-list start number (the `start="N"` HTML
    /// attribute) so a consumer can renumber / continue a split list from DATA
    /// without re-parsing the `<ol start=…>` attribute. The opt-in `start` rides
    /// behind `#[serde(skip_serializing_if)]` so the off wire stays byte-identical.
    List { ordered: bool, start: Option<u32> },
    /// A blockquote. `nested` is the opt-in structured channel (`setBlockData`):
    /// `None` (default-off) ⇒ serializes as `{"type":"Blockquote"}` with no `data`
    /// key, byte-identical to before; `Some(cd)` (on) ⇒
    /// `{"type":"Blockquote","data":{"nested":[{ html }, …]}}` carrying the
    /// pre-rendered HTML of each inner sub-block so a `components.Blockquote`
    /// override can render the children KEYED (one node per inner block) instead
    /// of re-parsing the whole wrapper HTML every streaming tick. The single
    /// `Option`-bearing field — not a paired bare/with-data variant — covers both
    /// wire shapes (the generic carrier, like `Table(Option<TableData>)`).
    Blockquote(Option<ContainerData>),
    /// GitHub-style alert / admonition (a `> [!NOTE]` blockquote). `kind`
    /// serializes to a lowercase string ("note", "tip", …) so the JS layer can
    /// dispatch a custom renderer via `components.Alert`. `nested` is the opt-in
    /// structured channel (`setBlockData`): `None` (default-off) ⇒ serializes as
    /// `{"type":"Alert","data":{"kind":"note"}}`, byte-identical to before;
    /// `Some(cd)` (on) ⇒ `{"type":"Alert","data":{"kind":"note","nested":[…]}}`
    /// carrying the pre-rendered HTML of each inner body sub-block (the alert
    /// title line is the wrapper, not an inner block) for the same keyed-children
    /// render. The opt-in `nested` rides behind `#[serde(skip_serializing_if)]`
    /// so the off wire stays byte-identical.
    Alert { kind: AlertKind, nested: Option<ContainerData> },
    /// A GFM table. The `Option<TableData>` is the opt-in structured channel
    /// (`setBlockData`): `None` (default-off) ⇒ serializes as `{"type":"Table"}`
    /// with no `data` key, byte-identical to before; `Some(td)` (on) ⇒
    /// `{"type":"Table","data":{…}}` carrying the parsed headers/rows/aligns so a
    /// consumer can sort/filter/transpose/CSV/chart from DATA without re-parsing
    /// the display HTML. The single `Option`-bearing variant replaces the prior
    /// paired `Table` + `TableWithData(TableData)` pattern (the generic carrier).
    Table(Option<TableData>),
    Rule,
    Html,
    /// An opt-in custom component tag (e.g. `<Thinking>…</Thinking>`) whose name
    /// is in the configured allowlist. Its inner content is rendered as markdown.
    /// `tag` is the element name; `attrs` are the sanitized (name, value) pairs.
    /// Dispatched on the JS side via `components[tag]` (or `components.Component`).
    Component { tag: String, attrs: Vec<(String, String)> },
}

// ---------------------------------------------------------------------------
// Hand-written serialization — the single `{ "type", "data" }` envelope site.
// ---------------------------------------------------------------------------

/// Derive-checked nested-object shapes for the data-bearing kinds. These exist
/// so the hand-written `impl Serialize for BlockKind` only owns the *envelope*
/// (`type` + presence-of-`data`); the inner object shapes stay derived and so
/// cannot silently drift from the previous wire format. Each borrows from the
/// owning variant — no clones.
#[derive(Serialize)]
struct CodeBlockData<'a> {
    lang: &'a Option<String>,
    /// Opt-in decoded source (`setBlockData`); omitted entirely when off so the
    /// wire stays byte-identical (`{"lang":…}`), present when on (`{"lang":…,
    /// "code":"…"}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    code: &'a Option<String>,
}
#[derive(Serialize)]
struct ListData {
    ordered: bool,
    /// Opt-in ordered-list start (`setBlockData`); omitted when off so the wire
    /// stays byte-identical (`{"ordered":…}`), present when on (`{"ordered":…,
    /// "start":N}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    start: Option<u32>,
}
#[derive(Serialize)]
struct AlertData<'a> {
    kind: AlertKind,
    /// Opt-in pre-rendered inner sub-blocks (`setBlockData`); flattened from the
    /// `ContainerData` carrier and omitted entirely when off so the wire stays
    /// byte-identical (`{"kind":…}`), present when on (`{"kind":…,"nested":[…]}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    nested: Option<&'a Vec<NestedBlock>>,
}
#[derive(Serialize)]
struct ComponentData<'a> {
    tag: &'a String,
    attrs: &'a Vec<(String, String)>,
}

impl Serialize for BlockKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // `serialize_struct` (NOT `serialize_map`) is load-bearing: under
        // `serde_wasm_bindgen` with the default config (serialize_maps_as_objects
        // = false, as used at lib.rs `to_value`), a map serializes to a JS `Map`
        // whose `.type` / `.data` property reads return `undefined`, silently
        // breaking the TS `props.table = block.kind.data` contract. A struct
        // always serializes to a plain JS object regardless of that config, and
        // matches the plain-object shape the prior `#[serde(tag, content)]`
        // derive produced.

        // {"type": tag} — no `data` key (1-field struct).
        fn no_data<S: Serializer>(s: S, tag: &'static str) -> Result<S::Ok, S::Error> {
            let mut st = s.serialize_struct("BlockKind", 1)?;
            st.serialize_field("type", tag)?;
            st.end()
        }
        // {"type": tag, "data": value} (2-field struct).
        fn with_data<S: Serializer, T: Serialize>(
            s: S,
            tag: &'static str,
            data: &T,
        ) -> Result<S::Ok, S::Error> {
            let mut st = s.serialize_struct("BlockKind", 2)?;
            st.serialize_field("type", tag)?;
            st.serialize_field("data", data)?;
            st.end()
        }

        match self {
            // True unit kinds — `{"type": tag}` with no `data` key.
            BlockKind::Paragraph
            | BlockKind::Mermaid
            | BlockKind::Rule
            | BlockKind::Html => no_data(s, self.tag()),
            // The opt-in carrier: `None` ⇒ naked scalar payload
            // `{"type":"Heading","data":<level>}` (byte-identical to before);
            // `Some(rich)` ⇒ `{"type":"Heading","data":{level,text,id}}`.
            BlockKind::Heading { level, rich } => match rich {
                Some(h) => with_data(s, "Heading", h),
                None => with_data(s, "Heading", level),
            },
            // Object payloads via the derive-checked helper structs. The opt-in
            // `code`/`start` field is omitted when `None` (off) via
            // `skip_serializing_if`, so the off wire stays byte-identical.
            BlockKind::CodeBlock { lang, code } => {
                with_data(s, "CodeBlock", &CodeBlockData { lang, code })
            }
            BlockKind::List { ordered, start } => {
                with_data(s, "List", &ListData { ordered: *ordered, start: *start })
            }
            // Alert always carries its `kind`; the opt-in `nested` rides behind
            // `skip_serializing_if` so the off wire (`{"kind":…}`) is byte-identical.
            BlockKind::Alert { kind, nested } => with_data(
                s,
                "Alert",
                &AlertData { kind: *kind, nested: nested.as_ref().map(|cd| &cd.nested) },
            ),
            BlockKind::Component { tag, attrs } => {
                with_data(s, "Component", &ComponentData { tag, attrs })
            }
            // The opt-in carrier: present `data` iff the payload is `Some`.
            BlockKind::Blockquote(opt) => match opt {
                Some(cd) => with_data(s, "Blockquote", cd),
                None => no_data(s, "Blockquote"),
            },
            BlockKind::Table(opt) => match opt {
                Some(td) => with_data(s, "Table", td),
                None => no_data(s, "Table"),
            },
            BlockKind::MathBlock(opt) => match opt {
                Some(md) => with_data(s, "MathBlock", md),
                None => no_data(s, "MathBlock"),
            },
        }
    }
}

/// Structured heading payload for the opt-in `kind.data` channel (the `Some`
/// payload of `BlockKind::Heading`). Serializes to
/// `{ level: 1..6, text: <plaintext>, id: <slug> }`.
///
/// `text` is the heading's inline-stripped plaintext (the same derivation the
/// client's `outline()` performs on `block.html`, done once in Rust here). `id`
/// is a GitHub-style anchor slug of that text (lowercase, non-alphanumerics →
/// `-`), so a consumer can build a table of contents with working `#anchor`
/// links from DATA — no HTML re-parse. Duplicate heading texts yield identical
/// slugs in v1 (no cross-document dedup counter yet); see the slug derivation in
/// `render.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeadingData {
    pub level: u8,
    pub text: String,
    pub id: String,
}

/// Structured math payload for the opt-in `kind.data` channel (the `Some` payload
/// of `BlockKind::MathBlock`). Serializes to `{ latex: <source> }`.
///
/// `latex` is the DECODED LaTeX source — the same text the client's
/// `decodeMathText` re-derives from `block.html` by extracting the
/// `<div class="math math-display">…</div>` (or `<pre><code>…</code></pre>` for a
/// fenced `math`/`latex`/`tex` block) body and entity-decoding it, done once in
/// Rust here so a `components.MathBlock` override can re-render with KaTeX from
/// DATA — no HTML re-parse.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MathBlockData {
    pub latex: String,
}

/// Structured table payload for the opt-in `kind.data` channel (the
/// `Some` payload of `BlockKind::Table`). Serializes to
/// `{ headers: TableCell[], rows: TableCell[][], aligns: (string|null)[] }`.
/// `headers`/`rows` carry both the inline-stripped `text` (for sort/filter/CSV/
/// chart logic) and the inline-rendered `html` (for display) of each cell;
/// `aligns` is the per-column alignment ("left"|"center"|"right"|null).
///
/// Each committed row is held behind an `Rc` so the streaming `TableCache` can
/// re-emit the full table on every patch (the active block always carries all
/// rows so far, mirroring `Block::html`) with an O(rows) refcount bump per
/// patch instead of an O(cells) deep `String` clone — the perf-critical
/// difference for large streamed tables. The `rc` serde feature makes
/// `Rc<Vec<TableCell>>` serialize transparently, so the wire shape is unchanged:
/// `{ headers: TableCell[], rows: TableCell[][], aligns: (string|null)[] }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TableData {
    pub headers: Vec<TableCell>,
    pub rows: Vec<Rc<Vec<TableCell>>>,
    pub aligns: Vec<Option<&'static str>>,
}

/// One table cell in the structured channel. `text` is the plaintext (inline
/// markdown stripped) for logic; `html` is the inline-rendered display HTML —
/// byte-identical to the inline content inside the corresponding `<td>`/`<th>`
/// in `Block::html`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TableCell {
    pub text: String,
    pub html: String,
}

/// Structured container payload for the opt-in `kind.data` channel (the `Some`
/// payload of `BlockKind::Blockquote` / the `nested` carrier inside an `Alert`'s
/// data). Serializes to `{ nested: NestedBlock[] }` for a blockquote, and is
/// flattened to the `nested` key alongside `kind` for an alert.
///
/// `nested` is the ordered list of the container's inner sub-blocks, each as its
/// own pre-rendered HTML string — byte-identical to the corresponding fragment
/// inside `Block::html`'s wrapper. A `components.Blockquote` / `components.Alert`
/// override can render these KEYED (one node per entry) so that while the
/// container streams, only its last (open) inner block re-renders each tick —
/// the committed inner blocks have stable HTML and memoize. The wrapper
/// (`<blockquote>` / the alert `<div>` + title) is NOT in `nested`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContainerData {
    pub nested: Vec<NestedBlock>,
}

/// One inner sub-block of a blockquote / alert in the structured channel. `html`
/// is the pre-rendered display HTML of that sub-block (e.g. `<p>…</p>`),
/// byte-identical to the matching fragment inside the container's `Block::html`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NestedBlock {
    pub html: String,
}

/// The five GitHub alert keywords. Serializes to lowercase ("note", …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl AlertKind {
    /// Exact-uppercase keyword match, mirroring GitHub's documented syntax
    /// (`[!NOTE]`, not `[!note]`). Conservative on purpose — loosening later is
    /// non-breaking, tightening would not be.
    pub fn from_keyword(kw: &str) -> Option<Self> {
        match kw {
            "NOTE" => Some(AlertKind::Note),
            "TIP" => Some(AlertKind::Tip),
            "IMPORTANT" => Some(AlertKind::Important),
            "WARNING" => Some(AlertKind::Warning),
            "CAUTION" => Some(AlertKind::Caution),
            _ => None,
        }
    }
    /// Lowercase class/data string.
    pub fn class(self) -> &'static str {
        match self {
            AlertKind::Note => "note",
            AlertKind::Tip => "tip",
            AlertKind::Important => "important",
            AlertKind::Warning => "warning",
            AlertKind::Caution => "caution",
        }
    }
    /// Title-cased label shown in the rendered alert header.
    pub fn title(self) -> &'static str {
        match self {
            AlertKind::Note => "Note",
            AlertKind::Tip => "Tip",
            AlertKind::Important => "Important",
            AlertKind::Warning => "Warning",
            AlertKind::Caution => "Caution",
        }
    }
}

impl BlockKind {
    /// Lightweight discriminant string used by JS layer for renderer dispatch
    /// (e.g. "Mermaid" goes to mermaid renderer, "MathBlock" to KaTeX, etc.).
    pub fn tag(&self) -> &'static str {
        match self {
            BlockKind::Paragraph => "Paragraph",
            BlockKind::Heading { .. } => "Heading",
            BlockKind::CodeBlock { .. } => "CodeBlock",
            BlockKind::MathBlock(_) => "MathBlock",
            BlockKind::Mermaid => "Mermaid",
            BlockKind::List { .. } => "List",
            BlockKind::Blockquote(_) => "Blockquote",
            BlockKind::Alert { .. } => "Alert",
            BlockKind::Table(_) => "Table",
            BlockKind::Rule => "Rule",
            BlockKind::Html => "Html",
            BlockKind::Component { .. } => "Component",
        }
    }
}

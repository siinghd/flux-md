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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "data")]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    CodeBlock { lang: Option<String> },
    MathBlock,
    Mermaid,
    List { ordered: bool },
    Blockquote,
    /// GitHub-style alert / admonition (a `> [!NOTE]` blockquote). `kind`
    /// serializes to a lowercase string ("note", "tip", …) so the JS layer can
    /// dispatch a custom renderer via `components.Alert`.
    Alert { kind: AlertKind },
    Table,
    /// Opt-in structured table channel (`setBlockData`). Same serialized tag as
    /// `Table` (`{"type":"Table","data":{…}}`); carries the parsed
    /// headers/rows/aligns so a consumer can sort/filter/transpose/CSV/chart from
    /// DATA without re-parsing the display HTML. Default-off ⇒ `Table` (no `data`
    /// key), byte-identical to before.
    #[serde(rename = "Table")]
    TableWithData(TableData),
    Rule,
    Html,
    /// An opt-in custom component tag (e.g. `<Thinking>…</Thinking>`) whose name
    /// is in the configured allowlist. Its inner content is rendered as markdown.
    /// `tag` is the element name; `attrs` are the sanitized (name, value) pairs.
    /// Dispatched on the JS side via `components[tag]` (or `components.Component`).
    Component { tag: String, attrs: Vec<(String, String)> },
}

/// Structured table payload for the opt-in `kind.data` channel (the
/// `TableWithData` variant). Serializes to
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
            BlockKind::Heading(_) => "Heading",
            BlockKind::CodeBlock { .. } => "CodeBlock",
            BlockKind::MathBlock => "MathBlock",
            BlockKind::Mermaid => "Mermaid",
            BlockKind::List { .. } => "List",
            BlockKind::Blockquote => "Blockquote",
            BlockKind::Alert { .. } => "Alert",
            BlockKind::Table => "Table",
            BlockKind::TableWithData(_) => "Table",
            BlockKind::Rule => "Rule",
            BlockKind::Html => "Html",
            BlockKind::Component { .. } => "Component",
        }
    }
}

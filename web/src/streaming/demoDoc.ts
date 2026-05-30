/**
 * A canned markdown document for the "Data Studio" demo. It contains GFM tables,
 * several headings (so the live TOC has something to build), and a code block —
 * exactly the block kinds whose structured `kind.data` (the flux-md 0.10.0
 * opt-in `blockData` channel) the demo consumes WITHOUT re-parsing HTML.
 *
 * Streamed via {@link streamDemoDoc}, a tiny async generator that yields the doc
 * in small chunks with a short delay between them, so the table visibly grows
 * and sort/filter/TOC can be seen working MID-STREAM. Deterministic (no network)
 * so the playground demo and its Playwright smoke are reproducible.
 */

export const DEMO_DOC = `# Rope data structures

A streaming parser keeps the document responsive while tokens are still
arriving. Below, the **table toolbar** and the **table of contents** are both
built from \`block.kind.data\` — structured data, not re-parsed HTML.

## Benchmarks

Throughput and memory for three rope implementations, measured on a 4 MB buffer.

| Structure | Insert (ns) | Memory (MB) | Notes |
| :-------- | ----------: | ----------: | :---- |
| Gap buffer | 18 | 4.1 | Fast near the cursor, O(n) far edits |
| Piece table | 42 | 4.4 | Append-only log, great undo |
| Rope (B-tree) | 27 | 5.2 | Balanced, O(log n) anywhere |
| Array of lines | 9 | 4.0 | Trivial, O(n) line splits |
| Zipper | 31 | 4.6 | Purely functional, cheap focus |

## Editor adoption

Which editors ship which structure today.

| Editor | Structure | Language |
| :----- | :-------- | :------- |
| VS Code | Piece table | TypeScript |
| Emacs | Gap buffer | C |
| Xi | Rope (B-tree) | Rust |
| Vim | Array of lines | C |
| Helix | Rope (B-tree) | Rust |

## Implementation sketch

A rope splits text into a balanced tree of small leaves, so an edit touches
only O(log n) nodes:

\`\`\`rust
enum Rope {
    Leaf(String),
    Node { left: Box<Rope>, right: Box<Rope>, weight: usize },
}

impl Rope {
    fn char_at(&self, i: usize) -> char {
        match self {
            Rope::Leaf(s) => s.chars().nth(i).unwrap(),
            Rope::Node { left, right, weight } => {
                if i < *weight { left.char_at(i) }
                else { right.char_at(i - weight) }
            }
        }
    }
}
\`\`\`

## Takeaways

The sortable table and the live outline above prove the point: rich,
interactive UI from the **data channel**, with no HTML re-parsing, working
while the document is still streaming in.
`;

/**
 * Yield {@link DEMO_DOC} as small string chunks with a short delay, so the demo
 * renders/grows incrementally. A fresh call returns a fresh generator — drive it
 * with `client.pipeFrom(streamDemoDoc())` after a `client.reset()` to replay.
 *
 * @param chunkSize characters per chunk (smaller = more visibly incremental)
 * @param delayMs   delay between chunks in ms
 */
export async function* streamDemoDoc(chunkSize = 24, delayMs = 14): AsyncGenerator<string> {
  for (let i = 0; i < DEMO_DOC.length; i += chunkSize) {
    yield DEMO_DOC.slice(i, i + chunkSize);
    await new Promise((r) => setTimeout(r, delayMs));
  }
}

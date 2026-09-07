# brookmd-core wire contract

**Wire contract version: 1.3.0**

This document specifies the JSON wire format produced by `brookmd-core` and
consumed by any renderer. It is the stable, versioned boundary between the
parser core (Rust → WebAssembly, or a future native embedding) and a rendering
layer in any language. The current JavaScript renderer is one consumer; native
consumers (for example a React Native, Swift, or Kotlin binding) can implement
against this document alone.

Covered releases: **npm `brookmd` 0.30.0** and **crate `brookmd-core` 0.27.0**.

The contract version tracks the *wire shape*, not the library version. It changes
independently: a library release that leaves every shape below byte-identical
keeps the wire version unchanged. See [Stability policy](#stability-policy) for what a
version bump means.

All line references in this document point at the source as of the commit that
introduced it (crate version 0.20.3).

---

## 1. Serialization boundary

The parser exposes three methods on the `BrookParser` WebAssembly type
(`src/lib.rs`). Each returns a **JSON string** (not a live JS object); the
consumer parses it once (`JSON.parse`, or any JSON decoder):

| Method | Returns (parsed) | Source |
| --- | --- | --- |
| `append(chunk: string)` | a `Patch` object | `src/lib.rs:132-136` |
| `finalize()` | a `Patch` object | `src/lib.rs:139-143` |
| `allBlocks()` | a `Block[]` array | `src/lib.rs:155-159` |

All three serialize through `serde_json::to_string`. There is no binary or
`serde_wasm_bindgen` object path on the live boundary. Field order in every
object below is the serializer's declaration order and is stable.

The same bytes are available without the `wasm` feature: the pure `wire`
module (`src/wire.rs`) exposes `WirePatch`, `patch_to_json`, and
`blocks_to_json`, which produce exactly the strings the three methods above
return. A native embedding uses that module directly; the WebAssembly glue is a
thin wrapper over it.

---

## 2. The `Patch` envelope

`append` and `finalize` both return a `Patch`. Its serialized shape is defined by
`wire::WirePatch` (`src/wire.rs`), which carries the two fields of the internal
`Patch` (`src/parser.rs:345-348`); native consumers serialize it with
`wire::patch_to_json`:

```json
{
  "newly_committed": [ /* Block, … */ ],
  "active": [ /* Block, … */ ]
}
```

Both keys are always present; either array may be empty.

- **`newly_committed`** — blocks that just became permanent on this call. They
  are final: `open = false`, `speculative = false`, and they will **never be
  emitted again** by any later `append`/`finalize`. A consumer appends them to
  its committed history and never revisits them.
- **`active`** — the blocks still being built at the document tail. On every
  `append` the parser re-emits the **full** current state of each active block
  (including its complete, growing `html`). A consumer **replaces its entire
  active region wholesale** with this array on each patch; it must not attempt to
  diff or merge into the previous `active` array positionally.

At `finalize`, any remaining active blocks are emitted with `open = false` and
`speculative = false` (`src/parser.rs:1941-1944`, with `finalizing = true`); a
consumer treats the final patch's `active` blocks as committed.

### Re-emit floor (complexity)

Re-emitting each active block's full `html` every `append` is an intentional part
of the contract, not an implementation detail. Its cost is
`O(size-of-active-block)` per `append`. For a single very large block that stays
open across many chunks (for example one giant fenced code block streamed in
small pieces), the total emitted bytes are therefore `O(n²/chunk)` in the block's
final size. This floor is documented at `src/lib.rs:41-45`. Committed blocks do
not participate in it (they are emitted exactly once). A consumer that needs to
bound work should key its render on block `id` + `html` identity so unchanged
committed blocks are never re-rendered.

**The floor is retired by the opt-in wire delta mode (§11):** with
`setWireDelta(true)`, a re-emitted active block serializes as a verified splice
against its previous emit, and total emitted bytes become `O(n)`. The default
(delta off) keeps the exact v1 behavior and bytes described above.

---

## 3. The `Block` object

Every element of `newly_committed`, `active`, and the `allBlocks()` array is a
`Block` (`src/blocks.rs:9-26`). All seven fields are always present:

| Field | Type | Meaning |
| --- | --- | --- |
| `id` | `u64` | Stable, monotonic block identity (see §4). |
| `kind` | object | Block-kind envelope (see §5). |
| `start` | `usize` | Byte offset of the block's first byte in the source buffer. |
| `end` | `usize` | Byte offset just past the block's last byte (or buffer end while open). |
| `html` | `string` | Sanitized rendered HTML for the block, safe to inject via `innerHTML`. |
| `open` | `bool` | `true` while the block is still being built; `false` once closed. |
| `speculative` | `bool` | `true` when `open == false` but the close came from speculation; a later chunk may revise the block. |

Illustrative full object (field order is the `Block` struct order; the `kind`
sub-object uses the exact bytes from §6):

```json
{
  "id": 0,
  "kind": { "type": "Paragraph" },
  "start": 0,
  "end": 11,
  "html": "<p>Hello.</p>",
  "open": false,
  "speculative": false
}
```

`html` is always produced through the safe-allowlist serializer and is XSS-safe
to render as raw HTML (`src/blocks.rs:16-19`). Raw-HTML pass-through is off by
default; see the `setUnsafeHtml` / `setHtmlSanitize` / `setBlockHtml`
configuration in `src/lib.rs`.

`html` carries **no trailing newline** — the line terminator that follows a
top-level block belongs to the document, not to the block. Concatenating blocks
into one document string therefore has a defined rule; see §12.

---

## 4. Id semantics

`id` is a `u64` that is **stable and monotonic for the lifetime of one parser
instance**. Once a block is assigned id *N*, no later re-parse renumbers it
(`src/blocks.rs:5-7`).

Assignment rule (`src/parser.rs:1921-1932`): when the parser produces a block, it
reuses the id of a prior **active** block if and only if that prior block has the
same `start` offset **and** the same `kind` tag (§6). Otherwise it allocates the
next id from a monotonic counter (`next_id += 1`).

Consequences a consumer can rely on:

- Ids never decrease and are never reused for a different logical block within one
  parser instance.
- An open block keeps its id across the appends that grow it (same `start`, same
  kind tag).
- A block whose kind tag changes at a position (for example a paragraph tail that
  resolves into a heading) receives a **new** id, because the tag no longer
  matches.

Ids are unique per parser instance. If a consumer merges output from more than
one parser instance, or overlays client-side reprocessing, it is responsible for
namespacing to keep keys unique (see the informative note in §10 on how the
JavaScript client does this).

---

## 5. Commit semantics

A block's lifecycle across patches:

1. It first appears in `active` with `open = true`, `speculative = false` while it
   is the growing tail.
2. It may be emitted (in `active`) with `open = false`, `speculative = true` when
   the parser has speculatively closed it but a later chunk could still revise it.
3. It moves to `newly_committed` with `open = false`, `speculative = false` when it
   becomes permanent (`src/parser.rs:1934-1940`). After this it is never emitted
   again.

`newly_committed` entries are immutable. `active` entries are provisional and are
fully replaced on each patch.

---

## 6. `BlockKind` envelope

`kind` is a tagged object produced by a hand-written serializer
(`src/blocks.rs:183-259`). It has one of two shapes:

```json
{ "type": "<Tag>" }
```
or
```json
{ "type": "<Tag>", "data": <payload> }
```

The **presence or absence of the `data` key is significant** and part of the
contract (see §7). The `type` string is the block-kind tag
(`src/blocks.rs:417-432`).

The twelve tags and their `data` shapes:

| `type` | `data` (default, `setBlockData` off) | `data` (`setBlockData` on) | Source |
| --- | --- | --- | --- |
| `Paragraph` | *(no `data` key)* | *(unchanged)* | `blocks.rs:214-217` |
| `Heading` | integer level `1`–`6` | `{ level, text, id }` — **polymorphic** | `blocks.rs:59,221-224,273-277` |
| `CodeBlock` | `{ lang, meta? }` (`lang` string or `null`) | `{ lang, meta?, code }` (`code` = decoded source) | `blocks.rs:86,160-172,246-248` |
| `MathBlock` | *(no `data` key)* | `{ latex }` (decoded LaTeX source) | `blocks.rs:82,253-256,291-293` |
| `Mermaid` | *(no `data` key)* | *(unchanged)* | `blocks.rs:83,214-217` |
| `List` | `{ ordered }` | `{ ordered, start?, items? }` | `blocks.rs:98,156-167,231-233` |
| `Blockquote` | *(no `data` key)* | `{ nested: [{ html }, …] }` | `blocks.rs:108,245-248,355-357` |
| `Alert` | `{ kind }` (lowercase string) | `{ kind, nested? }` | `blocks.rs:119,169-176,236-240` |
| `Table` | *(no `data` key)* | `{ headers, rows, aligns }` | `blocks.rs:127,249-252,312-316` |
| `Rule` | *(no `data` key)* | *(unchanged)* | `blocks.rs:128,214-217` |
| `Html` | *(no `data` key)* | *(unchanged)* | `blocks.rs:129,214-217` |
| `Component` | `{ tag, attrs }` | *(unchanged; always present)* | `blocks.rs:134,178-180,241-243` |

Notes on always-present vs. opt-in `data`:

- **Always carry `data`** regardless of `setBlockData`: `Heading`, `CodeBlock`,
  `List`, `Alert`, `Component`. Their `data` object gains extra fields when the
  channel is on, but the base fields (`Heading` level, `CodeBlock.lang`,
  `List.ordered`, `Alert.kind`, `Component.tag`/`attrs`) are always there.
- **Have no `data` key at all** when the channel is off, and gain one only when
  on: `MathBlock`, `Blockquote`, `Table`.
- **Never carry `data`** in any mode: `Paragraph`, `Mermaid`, `Rule`, `Html`.

`Alert.kind` is one of `"note"`, `"tip"`, `"important"`, `"warning"`, `"caution"`
(`src/blocks.rs:370-411`). It is emitted for renderer dispatch and is consumed by
a user-supplied component, not by the core library.

---

## 7. The `blockData` channel (opt-in)

`setBlockData(true)` (`src/lib.rs:238-241`) enriches certain kinds with structured
`data` so a consumer can drive logic (build a table of contents, sort a table,
re-render math/code) **from data, without re-parsing `html`**. It is **off by
default**, and when off the wire is **byte-identical** to a build that never had
the channel.

Two facts a consumer must handle:

1. **`Heading` is polymorphic.** With the channel off, `Heading.data` is a bare
   integer (the level). With it on, `Heading.data` is an object
   `{ level, text, id }`. A consumer **must branch on the JSON type of
   `Heading.data`** (number vs. object) — never assume one form.
   (`src/blocks.rs:221-224`.)
2. **Absence of the `data` key is meaningful.** For `MathBlock`, `Blockquote`, and
   `Table`, "no `data` key" is exactly how the off-mode is expressed. A consumer
   must treat a missing `data` key as "structured channel not present for this
   block", not as an error.

Payload shapes when the channel is on:

- **`HeadingData`** (`src/blocks.rs:273-277`): `{ level: 1–6, text: string, id:
  string }`. `text` is the inline-stripped plaintext; `id` is a GitHub-style
  anchor slug of `text`.
- **`CodeBlock.data`** (`src/blocks.rs:160-172`): `{ lang: string|null, meta?:
  string, code?: string }`. `lang` is the info string's first word; `meta` is the
  trimmed REMAINDER of the same info string (```` ```ts title="x.ts" ````), raw
  (undecoded) like `lang`, always-on but **omitted entirely** when the fence
  carried none — so a fence without meta is byte-identical to before. `code`
  (decoded source) is present only when the channel is on and is omitted
  otherwise. `meta` has no HTML counterpart (there is no `data-meta` attribute).
  Streaming timing: `meta` appears once it can no longer change — when the
  opening fence line is terminated by a newline, or at `finalize` (which settles
  a document that ends mid-opener-line). `lang`, the first word, needs no such
  wait. Both the full path and the streaming caches apply the same rule, so an
  open block's `data` matches the one-shot parse of the same prefix.
  `code` is the body as RENDERED, not the verbatim source slice: it is exactly
  `decodeCodeText(block.html)`, so an indented opening fence (```` ```` ```` at
  1–3 columns) has already shed that indent from every line, per CommonMark
  §4.5, and an indented code block has already shed its 4 columns. That is what
  makes it correct to paste — a copy button emitting the raw slice would carry
  the container's indentation back in.
- **`MathBlockData`** (`src/blocks.rs:291-293`): `{ latex: string }` — the decoded
  LaTeX source.
- **`ListData`** (`src/blocks.rs:156-167`): `{ ordered: bool, start?: number,
  items?: [{ html }, …] }`. `start` is the ordered-list start number; `items`
  carries each `<li>`'s inner HTML. Both are omitted when empty/off. An item
  bearing any BLOCK child (a loose paragraph, a nested list, a fence, a quote)
  brackets that child with a newline, so its `html` both starts and ENDS with
  `\n` (`"\n<p>a</p>\n"`); a tight single-paragraph item stays bare (`"first"`).
- **`TableData`** (`src/blocks.rs:312-326`): `{ headers: [{ text, html }, …], rows:
  [[{ text, html }, …], …], aligns: (string|null)[] }`. Each `aligns` entry is
  `"left"`, `"center"`, `"right"`, or `null`.
- **`ContainerData`** (blockquote `nested`, and alert `nested`)
  (`src/blocks.rs:355-365`): `{ nested: [{ html }, …] }` — the pre-rendered HTML of
  each inner sub-block. The wrapper element itself is not in `nested`.

---

## 8. Concrete JSON examples

Every `kind` example below is byte-exact against the golden regression strings in
`tests/block_kind_serde_golden.rs` (verified passing at commit `bf308d3`). Whole
`Block`/`Patch` envelopes wrap these `kind` bytes in the field order of §2–§3.

**Unit kinds (no `data`):**

```json
{"type":"Paragraph"}
{"type":"Mermaid"}
{"type":"Rule"}
{"type":"Html"}
{"type":"Blockquote"}
{"type":"MathBlock"}
{"type":"Table"}
```

**Heading — polymorphic:**

```json
{"type":"Heading","data":2}
{"type":"Heading","data":{"level":2,"text":"Hello world","id":"hello-world"}}
```

**CodeBlock:**

```json
{"type":"CodeBlock","data":{"lang":null}}
{"type":"CodeBlock","data":{"lang":"rust"}}
{"type":"CodeBlock","data":{"lang":"ts","meta":"title=\"src/main.ts\""}}
{"type":"CodeBlock","data":{"lang":"rust","code":"fn main() {}\n"}}
{"type":"CodeBlock","data":{"lang":null,"code":"plain\n"}}
```

**MathBlock (channel on):**

```json
{"type":"MathBlock","data":{"latex":"E = mc^2"}}
```

**List:**

```json
{"type":"List","data":{"ordered":true}}
{"type":"List","data":{"ordered":false}}
{"type":"List","data":{"ordered":true,"start":5}}
{"type":"List","data":{"ordered":true,"start":1,"items":[{"html":"first"},{"html":"<strong>second</strong>"}]}}
```

**Blockquote (channel on):**

```json
{"type":"Blockquote","data":{"nested":[{"html":"<p>a</p>"},{"html":"<p>b</p>"}]}}
```

**Alert:**

```json
{"type":"Alert","data":{"kind":"note"}}
{"type":"Alert","data":{"kind":"tip","nested":[{"html":"<p>x</p>"}]}}
```

**Table (channel on):**

```json
{"type":"Table","data":{"headers":[{"text":"H","html":"<strong>H</strong>"}],"rows":[[{"text":"x","html":"x"}]],"aligns":["center",null]}}
```

**Component:**

```json
{"type":"Component","data":{"tag":"Thinking","attrs":[["a","b"]]}}
```

Note that `Component.attrs` is an array of two-element `[name, value]` string
arrays, not an object.

Full `Patch` example (illustrative; `kind` bytes are golden-exact):

```json
{
  "newly_committed": [
    {"id":0,"kind":{"type":"Heading","data":1},"start":0,"end":8,"html":"<h1>Title</h1>","open":false,"speculative":false}
  ],
  "active": [
    {"id":1,"kind":{"type":"Paragraph"},"start":9,"end":21,"html":"<p>streaming</p>","open":true,"speculative":false}
  ]
}
```

---

## 9. Stability policy

The wire contract version is `MAJOR.MINOR.PATCH`. This document is version
**1.3.0** (the header above is authoritative; this line tracks it).

**Additive (does not break consumers; MINOR bump):**

- A new `BlockKind` `type` tag.
- A new **optional** field inside an existing `data` object.

A conforming consumer **MUST ignore** any `type` tag it does not recognize and any
`data` field it does not recognize, rather than failing. Building against this
rule keeps a consumer forward-compatible across additive changes.

**Breaking (MAJOR bump required):**

- Removing or renaming any field (`id`, `kind`, `start`, `end`, `html`, `open`,
  `speculative`, or any `data` field).
- Changing the JSON type of any field.
- Changing the `Patch` envelope shape (`newly_committed` / `active`).
- Changing id semantics (§4) or commit semantics (§5) — for example making a
  committed block re-emit, or making ids non-monotonic.
- Changing the polymorphism of `Heading.data`, or which kinds carry / omit the
  `data` key.

Any breaking change requires a new **major** wire contract version and is called
out in release notes.

**Version history:**

- **1.3.0** — additive optional `data` fields: `CodeBlockData.meta` (always-on
  fence info-string remainder; key absent when the fence has none) and
  `ListItemData.start` (absolute source byte offset per item, present only
  under `blockData`; absent for nested items). With neither feature exercised
  the wire is byte-identical to 1.2.0; consumers that ignore unknown keys are
  unaffected either way.
- **1.2.0** — opt-in **wire delta mode** (§11): with `setWireDelta(true)`,
  active blocks re-emitted across appends may carry `html_delta` (a verified
  splice against their previous emit) instead of full `html`, retiring the §2
  re-emit floor. Additive: with the mode off (default) the wire is
  byte-identical to 1.1.0, and consumers that never enable it are unaffected.
- **1.1.0** — rendered-HTML pending-link marker renamed `data-flux-pending` →
  `data-brook-pending` with the project rename; envelope structure unchanged.
- **1.0.0** — initial published wire contract.

---

## 10. Consumer checklist (native-binding authors)

A renderer implemented against this contract should:

- [ ] Parse `append`/`finalize` output as a `Patch`; parse `allBlocks()` output as
      a `Block[]`.
- [ ] Treat `newly_committed` blocks as **immutable** — append to history, never
      revisit or re-render them.
- [ ] Replace the **entire** active region wholesale from each patch's `active`
      array; do not diff into the prior `active` positionally.
- [ ] Key rendering on `id` (stable, monotonic per parser instance) so committed
      blocks reconcile in place, and skip re-rendering a committed block whose
      `html` is unchanged.
- [ ] Handle **`Heading.data` polymorphism**: branch on number (level) vs. object
      (`{ level, text, id }`).
- [ ] Treat a **missing `data` key** as "structured channel absent", not an error
      (applies to `MathBlock`, `Blockquote`, `Table` with `setBlockData` off).
- [ ] **Ignore unknown `type` tags and unknown `data` fields** for forward
      compatibility.
- [ ] Expect the active tail to flicker across appends; only committed blocks are
      final.
- [ ] Namespace ids if merging output from multiple parser instances (ids are
      unique only within one instance).
- [ ] Render `html` as-is when raw HTML is disabled (default); it is already
      sanitized. Do not re-sanitize destructively.
- [ ] **Only if enabling wire delta mode** (§11): reconstruct active `html`
      from `html_delta` splices against the previous patch's active region, in
      patch order, before doing anything else with the block.

---

## 11. Wire delta mode (opt-in)

`setWireDelta(true)` (`StreamParser::set_wire_delta` on the native API) retires
the §2 re-emit floor: an `active` block that was present (same `id`) in the
**immediately preceding patch's `active` array** may serialize as a **splice**
against that previous emit instead of carrying its full `html`. Everything else
is unchanged — `newly_committed` blocks always carry full `html`, `allBlocks()`
always returns full `html`, and with the mode off (the default) the wire is
byte-identical to contract v1.1.0.

### 11.1 The delta entry

A delta entry has every `Block` field of §3 **except `html`**, with an
`html_delta` object in `html`'s position (field order preserved):

```json
{
  "id": 0,
  "kind": { "type": "Paragraph" },
  "start": 0,
  "end": 91,
  "html_delta": { "keep_bytes": 71, "keep_units": 71, "append": " and then keeps growing</p>" },
  "open": true,
  "speculative": true
}
```

Exactly one of `html` / `html_delta` is present on any active entry. A consumer
reconstructs:

```
html = previous_html[0 .. keep] + append
```

where `previous_html` is the block's html as of the previous patch (whether that
arrived full or was itself reconstructed from a delta), and `keep` is expressed
twice so every language slices in its native unit:

- **`keep_bytes`** — prefix length in **UTF-8 bytes** (Rust/C/byte-oriented
  consumers).
- **`keep_units`** — the same prefix in **UTF-16 code units** (JavaScript,
  Kotlin/JVM, and other UTF-16-string consumers: `previous.slice(0, keep_units)
  + append`).

Both always describe the identical prefix and both always fall on character
boundaries; `append` is a complete replacement for everything after the kept
prefix (there is no suffix-reuse form).

### 11.2 Emission rule (deterministic)

The parser emits a delta for an active entry **iff** all of:

1. delta mode is enabled;
2. a block with the same `id` was in the previous patch's `active` array;
3. the byte-verified common prefix of the previous and current `html` (backed
   off to a character boundary) is **at least 64 bytes**.

Otherwise the entry carries full `html`. The kept prefix is established by
**byte comparison against the previously emitted html**, never by structural
inference — a delta can therefore never reconstruct bytes that differ from the
parser's own state. An unchanged re-emit serializes as a delta with
`append: ""`.

### 11.3 Consumer obligations

- Maintain the active region's reconstructed `html` per block id between
  patches (the §2 rule — replace the active region wholesale — still applies;
  "wholesale" now means "after reconstruction").
- Treat a delta whose base id is missing as a hard protocol error (it cannot
  occur under rule 2 unless patches were dropped or applied out of order).
- A consumer that never calls `setWireDelta(true)` needs none of this and
  continues to read contract-v1 bytes.

### 11.4 Complexity

With delta mode on, total emitted bytes for a block that grows to size *n*
across any number of appends are `O(n)`: the growth arrives once through
`append` tails, plus one full `html` when the block commits. The
`wire_delta_emitted_is_linear` gate in `tests/scaling.rs` pins this for the
long-block shapes (open fence, list, table, blockquote, paragraph runs). For
content whose already-emitted prefix legitimately rewrites mid-stream (late
delimiter resolution rewriting early bytes), rule 3 degrades that patch to a
full re-emit — correctness always wins over compression.

---

## 12. Document assembly (the `cr()` join)

brookmd's product is a **per-block** wire: a consumer stamps one DOM node (or one
framework element) per `Block` and never needs a whole-document string. "The
document as one string" exists only at an *assembly point* — `renderToString`,
a static-site build step, a spec harness. This section defines that join, so
every assembly point produces the same bytes.

**Invariant.** A `Block.html` never ends with a newline. The newline that a
reference CommonMark/GFM renderer emits after each top-level block is a
*document* terminator, not part of the block.

**Rule.** Assembling blocks in document order:

1. Before appending a block's `html`, append `\n` **iff** the output so far is
   non-empty and does not already end with `\n`.
2. After the last block, apply the same test once more, so the document ends
   with exactly one `\n`.

That is cmark's `cr()` (`src/render.rs::cr`), the same primitive the renderer
already uses to separate a container's children.

```
out = ""
for block in blocks:
    if out and not out.endswith("\n"): out += "\n"
    out += block.html
if out and not out.endswith("\n"): out += "\n"
```

**Why `cr()` and not `"\n".join(...)`.** Not every block's HTML is
newline-free at the end in practice: a raw HTML block (`setUnsafeHtml(true)`,
or a sanitized type-6/7 block under `setBlockHtml(true)`) serializes its own
trailing newline as part of its content. An unconditional
join would double it and diverge from the reference — CommonMark 0.31 examples
148, 152, 167, 188 and 191 are precisely that case. The conditional test costs
one byte comparison at a push that already happens.

**Assembly points that implement this rule:** `renderToString` in the npm
package (`packages/brookmd/src/server.tsx`), and the `join_document` helper in
both spec harnesses (`tests/cmark_spec.rs`, `tests/gfm_spec.rs`) — which is why
the byte-exact tallies (652/652 CommonMark, 24/24 GFM extensions) measure real
public behaviour. The DOM and React renderers stamp one node per block and never
build a document string, so the rule does not apply to them.

---

## Appendix: informative, not contractual

The following are **not** part of the core wire contract. They are documented so
native binding authors know where the boundary lies. They may change without a
wire contract version bump.

### A. JavaScript worker transport envelope

The npm `brookmd` package runs the parser in a Web Worker and wraps each patch in
a transport message (`FromWorker`, `src/types-core.ts:435-457`):

```
{ type: "patch", streamId, patch /* the JSON Patch string, forwarded verbatim */,
  appendedBytes, parseMicros, retainedBytes, wasmMemoryBytes, final?, epoch? }
```

Only the `patch` string's contents are the core wire contract (§2–§8). The
surrounding fields — `streamId`, byte/timing/memory telemetry, `final`, and the
reset-generation `epoch` — are a transport concern of the JavaScript client and
are versioned separately from this document. A native embedding that calls the
parser directly never sees this envelope.

### B. JavaScript divergence-swap id conventions

When the JavaScript client reprocesses a diverged prefix, it applies its own id
policy on top of the parser's ids (`src/client.ts:811-853`): a changed block that
positionally replaces an old one **adopts the old position's id** (so a stateful
override — a paginated table, an expanded `<details>` — keeps its DOM/component
instance across the swap), and a net-new position offsets the raw parser id by an
`idNamespace` stride to avoid collision.

This is a **client rendering policy**, not core wire behavior. A native renderer
that wants the same stateful-component preservation across in-place content swaps
**should mirror** this convention, but it is not required by, and not part of, the
core wire contract.

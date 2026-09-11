# Wu — Text / Editor / Language Core: Agent Field Guide

Repo: `C:/Users/USER/Documents/wu-main` (Rust fork of Zed; upstream pin in `UPSTREAM_VERSION`).
Scope: `sum_tree`, `rope`, `text`, `language_core`, `language`, `languages`, `grammars`, `multi_buffer`,
`buffer_diff`, `editor`, `lsp`, `lsp_locations`, `project`, `worktree`, `search`, `diagnostics`,
`outline`, `snippet`, `snippet_provider`, `prettier`, `syntax_theme`, `call_hierarchy`, `language_tools`.

> **Read this before trusting upstream-Zed knowledge.** This fork has diverged in three
> load-bearing ways that will silently break code written from Zed memory:
> 1. **`ExcerptId` no longer exists.** `multi_buffer::Anchor` is an enum (`Min` / `Excerpt(ExcerptAnchor)` / `Max`)
>    keyed by `PathKey`/`PathKeyIndex` + `text::Anchor`. See "MultiBuffer" below.
> 2. **Multibuffer offsets are newtypes**, not `usize`: `MultiBufferOffset(usize)` vs `BufferOffset(usize)`.
>    The compiler catches most of it, but `.0` unwrapping is how bugs get in.
> 3. **Language configs + tree-sitter queries live in `crates/grammars/src/<lang>/`**, not
>    `crates/languages/src/<lang>/`. `crates/languages` only holds LSP adapters / task providers.
>
> Also: `crates/editor/src/editor.rs` has been split — `input.rs`, `selection.rs`, `clipboard.rs`,
> `completions.rs`, `code_actions.rs`, `navigation.rs`, `rewrap.rs`, `diagnostics.rs`, `config.rs`,
> `markdown_actions.rs`, `split.rs` are fork-local modules that don't exist upstream.

---

## 0. Repo rules that apply to every change here

`C:/Users/USER/Documents/wu-main/.rules` (`CLAUDE.md` and `AGENTS.md` are one-line pointers to it).
Highlights that bite in this domain:

- **HARD RULE in `.rules`:** when modifying any source file, prepend to `README.md`:
  ```
  > [!IMPORTANT]
  > Remove this line to confirm you've reviewed this PR before submitting.
  ```
  Never remove those lines yourself.
- No `mod.rs`. New modules are `src/some_module.rs` (+ `src/some_module/` dir for children).
- Avoid `unwrap()`; never `let _ = fallible()`. Use `?`, `.log_err()`, or explicit `match`.
- No summarizing comments — only "why" comments.
- Build/lint with `./script/clippy` (or `script/clippy.ps1` on Windows), **not** `cargo clippy`.
- In tests use `cx.background_executor().timer(..)`, never `smol::Timer::after`.
- Crate-specific rules would live in that crate's own `.rules` — **none exist today** (verified).
- PR title: imperative, no conventional-commit prefix, optional `crate_name: ` prefix.
  PR body ends with a `Release Notes:` section, one bullet.

---

## 1. Coordinate systems cheat sheet

### 1.1 The layer stack (bottom to top)

```
sum_tree::SumTree<T>                     generic B-tree keyed by Summary/Dimension
  +- rope::Rope (SumTree<Chunk>)         bytes; TextSummary dimensions
       +- text::Buffer / BufferSnapshot  CRDT; Anchor; transactions; Lamport clock
            +- language::Buffer/Snapshot syntax map, diagnostics, indent, language config
                 +- multi_buffer::MultiBuffer(Snapshot)  excerpts + diff transforms
                      +- editor::DisplayMap(DisplaySnapshot)
                           inlay -> fold -> tab -> wrap -> block
                           +- editor::Editor / EditorSnapshot
```

### 1.2 Every coordinate type, what it measures, and where it is valid

| Type | Defined in | Measures | Valid against |
|---|---|---|---|
| `usize` (byte offset) | — | UTF-8 bytes | a *single* `text`/`language` buffer snapshot |
| `rope::Point {row,column}` | `crates/rope/src/point.rs:9` | row + **byte** column | single buffer **and** multibuffer (see note) |
| `rope::PointUtf16` | `crates/rope/src/point_utf16.rs:7` | row + UTF-16 code-unit column | LSP boundary only |
| `rope::OffsetUtf16(usize)` | `crates/rope/src/offset_utf16.rs:4` | UTF-16 code units from start | LSP / IME boundary |
| `Unclipped<T>` | `crates/rope/src/unclipped.rs:5` | "not yet clamped to a valid position" | wrap LSP-provided positions before clipping |
| `text::Anchor` | `crates/text/src/anchor.rs:12` | CRDT insertion id + offset + `Bias` + `BufferId` | survives edits to *that* buffer |
| `multi_buffer::MultiBufferOffset(usize)` | `crates/multi_buffer/src/multi_buffer.rs:225` | bytes in the **rendered multibuffer text** | a `MultiBufferSnapshot` |
| `multi_buffer::BufferOffset(usize)` | `.../multi_buffer.rs:302` | bytes in an **underlying buffer** | that buffer's `BufferSnapshot` |
| `MultiBufferOffsetUtf16` / `BufferOffsetUtf16` | `.../multi_buffer.rs:364,411` | UTF-16 variants | ditto |
| `MultiBufferRow(u32)` | `.../multi_buffer.rs:168` | row in multibuffer output | `MultiBufferSnapshot` |
| `multi_buffer::Anchor` | `crates/multi_buffer/src/anchor.rs:26` | `Min` \| `Excerpt(ExcerptAnchor)` \| `Max` | a `MultiBufferSnapshot` (path key must still exist) |
| `InlayPoint(Point)` / `InlayOffset(MultiBufferOffset)` | `crates/editor/src/display_map/inlay_map.rs:170,106` | after inlay hint text is spliced in | `InlaySnapshot` |
| `FoldPoint(Point)` / `FoldOffset(MultiBufferOffset)` | `.../fold_map.rs:106,1622` | after folds collapse | `FoldSnapshot` |
| `TabPoint(Point)` | `.../tab_map.rs:525` | after hard tabs expand to columns | `TabSnapshot` |
| `WrapPoint(Point)` / `WrapRow(u32)` | `.../wrap_map.rs:87,32` | after soft wrap | `WrapSnapshot` |
| `BlockPoint(Point)` / `BlockRow(u32)` | `.../block_map.rs:107,110` | after block/header/diagnostic rows | `BlockSnapshot` |
| `DisplayPoint(BlockPoint)` / `DisplayRow(u32)` | `crates/editor/src/display_map.rs:2491,2521` | what the user sees | `DisplaySnapshot` |

**Note on `Point` ambiguity:** `MultiBufferPoint` is a *type alias* for `rope::Point`
(`multi_buffer.rs:161`). So a bare `Point` may be a buffer point or a multibuffer point.
The `todo(lw): MultiBufferPoint` comments at `multi_buffer.rs:191,206` mark exactly this hole.
**Always name the variable so the space is obvious** (`buffer_point` vs `mb_point`).

### 1.3 Conversion rules (the only ones that are correct)

**Within one buffer (`text::BufferSnapshot` / `language::BufferSnapshot`)** — traits at
`crates/text/src/text.rs:3395-3540`:

```rust
// usize <-> Point <-> PointUtf16 <-> OffsetUtf16 <-> Anchor
let off:  usize       = point.to_offset(&snapshot);        // ToOffset
let pt:   Point       = off.to_point(&snapshot);           // ToPoint
let p16:  PointUtf16  = off.to_point_utf16(&snapshot);     // ToPointUtf16
let o16:  OffsetUtf16 = off.to_offset_utf16(&snapshot);    // ToOffsetUtf16
let a:    Anchor      = snapshot.anchor_before(off);       // Bias::Left
let a2:   Anchor      = snapshot.anchor_after(off);        // Bias::Right
let back: usize       = a.to_offset(&snapshot);            // via summary_for_anchor
```

Rules:
- **Clip before you trust.** `snapshot.clip_offset(off, bias)`, `clip_point`, `clip_point_utf16`
  (`text.rs:2677-2692`). LSP positions arrive as `Unclipped<PointUtf16>` and *must* go through
  `clip_point_utf16` / `unclipped_point_utf16_to_offset` (`rope.rs:487,563`).
- `impl ToOffset for usize` (`text.rs:3418`) **debug-asserts char boundaries** and silently
  `floor_char_boundary`s in release. Don't do arithmetic on offsets across multibyte chars —
  use `Rope::{floor,ceil}_char_boundary` (`rope.rs:82,93`) or `ToOffset::{to_next,to_previous}_offset`.
- `anchor_at(pos, Bias::Left)` at offset 0 gives `Anchor::min_for_buffer`; `Bias::Right` at `len()`
  gives `Anchor::max_for_buffer` (`text.rs:2633-2640`). Those two always resolve.
- An anchor from buffer A is **meaningless** against buffer B. `Anchor::is_valid(&snapshot)`
  (`anchor.rs:148`) says whether it points at a *visible* (non-tombstoned) fragment;
  `snapshot.can_resolve(&anchor)` (`text.rs:2670`) says whether the snapshot's version has
  observed the anchor's Lamport timestamp. Both can be false — check the right one.
- Ordering anchors requires the snapshot: `a.cmp(&b, &snapshot)` (`anchor.rs:89`).
  `Range<Anchor>` gets `AnchorRangeExt::{cmp, overlaps, contains_anchor}` (`anchor.rs:216-247`).
- `anchor_range_inside(r)` = `after(start)..before(end)` (shrinks with surrounding inserts);
  `anchor_range_outside(r)` = `before(start)..after(end)` (grows). `text.rs:2610-2617`.

**Buffer <-> MultiBuffer** — `MultiBufferSnapshot`:

```rust
// mb position -> (buffer, buffer position)
let (buf_snapshot, buf_off): (&BufferSnapshot, BufferOffset)
    = mb_snapshot.point_to_buffer_offset(mb_pos)?;              // multi_buffer.rs:4356
let (buf_snapshot, buf_pt)  = mb_snapshot.point_to_buffer_point(mb_point)?;   // :4379
// entity-returning variants live on MultiBuffer (not the snapshot): :1884, :1898

// buffer anchor -> mb anchor  (None if that buffer isn't excerpted here)
let mb_anchor = mb_snapshot.anchor_in_buffer(text_anchor)?;         // :5287
let mb_range  = mb_snapshot.anchor_range_in_buffer(range)?;         // :5293
let mb_anchor = mb_snapshot.anchor_in_excerpt(text_anchor)?;        // :5303
// buffer range -> possibly-many mb ranges (a buffer can be excerpted more than once!)
let ranges = mb_snapshot.buffer_range_to_excerpt_ranges(buf_range); // :6719
// mb range -> buffer ranges
let per_buffer = mb_snapshot.range_to_buffer_ranges(range);         // :3661
let a2b        = mb_snapshot.anchor_range_to_buffer_anchor_range(r);// :6678
```

Rules:
- **A buffer can appear in more than one excerpt.** Anything converting *buffer -> multibuffer*
  returns a collection (`buffer_range_to_excerpt_ranges`) or an `Option` that picks *one*. Using
  the `Option` form silently drops hits. Prefer the plural form for highlights/diagnostics.
- `MultiBufferSnapshot::anchor_at(pos, bias)` (`:5210`) may adjust `bias` internally at excerpt
  boundaries. Don't assume the returned anchor's bias equals what you asked for.
- Comparing two `multi_buffer::Anchor`s (`anchor.rs:105`) **panics** if the anchor's `PathKeyIndex`
  was never registered in this snapshot ("anchor's path was never added to multibuffer"). Anchors
  from an older multibuffer state must not be compared against a snapshot that dropped that path.
  Guard with `snapshot.can_resolve(&anchor)` (`:5408`).
- `MultiBufferOffset` addresses **rendered** text, which includes expanded deleted-diff-hunk rows
  (`DiffTransform::DeletedHunk`). Those rows have **no** position in the main buffer —
  `point_to_buffer_offset` returns `None` (or the base-text buffer). Handle `None`.
- Singleton mode: `MultiBuffer::is_singleton()` / `as_singleton()` (`:1334,1342`;
  snapshot: `:4117,4121`). In singleton mode `MultiBufferOffset.0 == buffer offset` **only when
  there are no expanded diff hunks and no headers**. Do not rely on it; convert explicitly.

**MultiBuffer <-> Display** — `DisplaySnapshot` (`crates/editor/src/display_map.rs`):

```rust
// down the stack (buffer -> display)
let dp: DisplayPoint = snapshot.point_to_display_point(mb_point, bias);   // :1660
let dp: DisplayPoint = mb_offset.to_display_point(&snapshot);             // ToDisplayPoint, :2607
let dp: DisplayPoint = anchor.to_display_point(&snapshot);                // :2620

// up the stack (display -> buffer)
let pt:  Point             = snapshot.display_point_to_point(dp, bias);   // :1725
let off: MultiBufferOffset = dp.to_offset(&snapshot, bias);               // :2592
let a:   Anchor            = snapshot.display_point_to_anchor(dp, bias);  // :1740
```

Rules:
- **Down is total, up is lossy.** `point_to_display_point` walks
  inlay -> fold -> tab -> wrap -> block (`:1661-1666`). Going up needs a `Bias` at *every* layer
  because a display point can land inside a fold, inside inlay text, or on a block row.
- **A `DisplayPoint` can point at text that is not in the buffer at all** (inlay hints, block
  content, deleted-hunk rows). `to_offset` clamps it to a nearby real offset. If you need only
  real buffer text, use `DisplaySnapshot::isomorphic_display_point_ranges_for_buffer_range`
  (`:1673`) or `contiguous_display_point_range_for_buffer_range` (`:1690`).
- Converting **many** ranges? Use `DisplaySnapshot::display_point_converter()` (`:1712`) — it keeps
  forward-only cursors per layer. Feed it non-decreasing offsets; it auto-resets (extra seek) if you
  go backwards.
- `DisplaySnapshot::clip_point` (`:2111`) honours `clip_at_line_ends` (vim normal mode). For raw
  clipping use `clip_ignoring_line_ends` (`:2119`).
- `DisplayRow` is **not** `MultiBufferRow`. Blocks, headers and wraps insert rows. Convert, don't cast.

**LSP boundary:** LSP speaks `PointUtf16` / `lsp::Position`. Path is
`buffer offset -> PointUtf16 -> lsp::Position` via `language::point_to_lsp`, and back through
`Unclipped<PointUtf16>` -> `clip_point_utf16`. Never hand LSP a `MultiBufferOffset` or `DisplayPoint`.
See `crates/editor/src/test/editor_lsp_test_context.rs:445-473` for the canonical round-trip.

### 1.4 `Bias` semantics

`sum_tree::Bias` (re-exported from `text`). `Bias::Left` = "stick to the character before this
position"; `Bias::Right` = "stick to the character after". Consequences:

- Anchors: a `Bias::Left` anchor does **not** move when text is inserted exactly at it; a
  `Bias::Right` anchor does. Selection starts usually want `Right`, ends usually want `Left`
  when the selection should not swallow adjacent inserts (`anchor_range_inside`).
- Clipping: `clip_point(p, Bias::Left)` snaps backward into a valid position (out of a fold /
  off a mid-grapheme column), `Bias::Right` snaps forward.
- Cursor seeking (`sum_tree::Cursor::seek`, `crates/sum_tree/src/cursor.rs:408`): `Left` stops
  *before* an item whose start equals the target; `Right` stops *after*.

---

## 2. Layer-by-layer notes

### 2.1 `sum_tree` — `crates/sum_tree/src/`

- `sum_tree.rs:213` `SumTree<T>(Arc<Node<T>>)`. `TREE_BASE = 6` (2 under `cfg(test)`, so tests
  exercise node splits: `sum_tree.rs:16-18`).
- Traits: `Item` (`:33`), `KeyedItem` (`:40`), `Summary` (`:49`, has a `Context<'a>`),
  `ContextLessSummary` (`:56`, blanket-impls `Summary` with `Context = ()`),
  `Dimension<'a, S>` (`:92`), `SeekTarget` (`:120`), `Dimensions<D1,D2,D3>` (`:139`).
- `Cursor<'a,'b,T,D>` (`cursor.rs:30`): `seek`/`seek_forward` (`:408,423`), `slice` (`:432`),
  `summary` (`:452`), `item`/`item_summary`/`next_item`/`prev_item`,
  `search_forward`/`search_backward`. `FilterCursor` (`:679`) skips subtrees failing a predicate.
- Batch APIs that matter for perf: `from_iter` (`:249`), `from_par_iter` (`:318`), `extend` (`:750`),
  `par_extend` (`:757`), `append` (`:780`). `SumTree::edit` for `KeyedItem` (`:1190`).
- `TreeMap` / `TreeSet` in `tree_map.rs` are used pervasively (diagnostics by server id, undo map,
  remote selections, path-to-buffer maps).

Rule: **never linearly scan a SumTree** when a Dimension exists. Add a dimension to the summary
instead: `impl<'a> Dimension<'a, YourSummary> for YourDim`.

### 2.2 `rope` — `crates/rope/src/`

- `Rope { chunks: SumTree<Chunk> }` (`rope.rs:26`). `TextSummary` (`rope.rs:1282`) carries
  `len`, `chars`, `len_utf16`, `lines: Point`, `first/last_line_chars`, `last_line_len_utf16`,
  `longest_row`, `longest_row_chars`.
- `TextDimension` is implemented for `TextSummary`, `usize`, `OffsetUtf16`, `Point`, `PointUtf16`
  (`rope.rs:1478,1502,1526,1550,1574`) — that is what makes
  `snapshot.text_summary_for_range::<D>(..)` generic.
- Iteration: `chunks()`, `chunks_in_range()`, `reversed_chunks_in_range()`, `chars_at()`,
  `reversed_chars_at()`, `bytes_in_range()`, `Lines`, `Chunks::next_line`/`prev_line` (`:876,917`).
- `Cursor` (`rope.rs:678`) — `seek_forward`, `slice`, `summary::<D>`, `suffix`. Forward-only.
- Conversions `offset_to_point`, `point_to_offset`, `offset_to_offset_utf16`,
  `unclipped_point_utf16_to_offset`/`_point`, `clip_*` at `rope.rs:369-572`.

### 2.3 `text` — `crates/text/src/`

- `Buffer` (`text.rs:59`) = `snapshot: BufferSnapshot` + `history: History` + `lamport_clock` +
  deferred ops. `BufferSnapshot` (`text.rs:110`) = `visible_text: Rope`, `deleted_text: Rope`,
  `fragments: SumTree<Fragment>`, `insertions`, `undo_map`, `version: clock::Global`.
- CRDT identity: `clock::ReplicaId(u16)` + `clock::Seq(u32)` produce `clock::Lamport`
  (`crates/clock/src/clock.rs:14,58,63`). `clock::Global` is the version vector
  (`observe`/`observed`/`observed_all`/`changed_since`, `clock.rs:103-182`).
- `TransactionId = clock::Lamport` (`text.rs:57`). `Transaction { id, edit_ids, start }` (`:135`).
- Transactions: `start_transaction[_at]` / `end_transaction[_at]` (`text.rs:1313-1330`).
  They nest via `transaction_depth`; only the outermost creates a history entry.
  Grouping interval is **300 ms in release but `Duration::ZERO` under `cfg(test)`**
  (`text.rs:225-230`), with the in-source comment: "Don't group transactions in tests unless we
  opt in, because it's a footgun."
- Undo/redo: `undo`, `undo_transaction`, `undo_to_transaction`, `redo`, `redo_to_transaction`
  (`text.rs:1352-1434`). Undo is `UndoMap` counts over edit ids, not textual reversion.
- `subscribe()` (`text.rs:1548`) yields `Subscription<usize>` producing `Patch<usize>` of edits;
  this is how the display map layers stay in sync (`DisplayMap.buffer_subscription`).
- `edits_since::<D>(&version)` (`text.rs:2693`) — edits as `Edit<D>` in any `TextDimension`.
- `text::Selection<T>` (`selection.rs:17`): `{ id, start, end, reversed, goal }`.
  `head()`/`tail()` are `reversed`-aware; `start <= end` always.
  `Selection<Anchor>::resolve::<D>(&snapshot)` (`selection.rs:150`) gives `Selection<Point>` etc.
  `SelectionGoal` (`selection.rs:6`) preserves desired column across vertical motion.
- Debug aid: `text::debug::GlobalDebugRanges` (`text.rs:3691`) — see section 11.

### 2.4 `language::Buffer` — `crates/language/src/buffer.rs` (6 034 lines)

- `Buffer` (`:102`) wraps `text: TextBuffer` and adds `language: Option<Arc<Language>>`,
  `syntax_map: Mutex<SyntaxMap>`, `reparse: Option<Task<()>>`, `parse_status: watch::channel`,
  `diagnostics: TreeMap<LanguageServerId, DiagnosticSet>`, `autoindent_requests`,
  `file: Option<Arc<dyn File>>`, `capability`, `encoding`, `modeline`, `tree_sitter_data`.
- `BufferSnapshot` (`:186`) = `text: text::BufferSnapshot` + `syntax: SyntaxSnapshot` +
  diagnostics + language + file. It derefs to the text snapshot, so all section 1.3 conversions work.
- `BufferEvent` (`:315`): `Operation`, `Edited{source}`, `DirtyChanged`, `Saved`,
  `FileHandleChanged`, `Reloaded`, `ReloadNeeded`, `LanguageChanged(bool)`, `Reparsed`,
  `DiagnosticsUpdated`, `CapabilityChanged`. Subscribe to `Reparsed` for syntax-dependent work.
- `Capability` (`:82`): `ReadWrite` / `Read` / `ReadOnly`; check `capability.editable()`.
- `File` trait (`:361`): `path() -> &Arc<RelPath>`, `full_path(cx) -> PathBuf`, `worktree_id`,
  `path_style(cx)`, `disk_state() -> DiskState` (`:395`: `New` / `Present{mtime,size}` / `Deleted`).
- `AutoindentMode` (`:464`) — pass `Some(AutoindentMode::EachLine)` or `Block{..}` to `edit()` when
  inserting code, `None` for verbatim.
- Syntax: `crates/language/src/syntax_map.rs` (2 242 lines) — `SyntaxMap`, `SyntaxSnapshot`,
  `SyntaxLayer`, `SyntaxMapCaptures`/`Matches`, injections, `MAX_BYTES_TO_QUERY`.
- Diagnostics: `diagnostic.rs` (`Diagnostic`, severity, `DiagnosticSourceKind`) and
  `diagnostic_set.rs` (`DiagnosticSet` = `SumTree<DiagnosticEntry<Anchor>>`, `DiagnosticGroup`).
- `language_settings.rs` — `LanguageSettings`, `AllLanguageSettings`, `language_settings_at()`.
- `modeline.rs` (781 lines) — vim/emacs modeline parsing (fork feature).
- Other notable files: `buffer/bracket_ranges.rs`, `buffer/row_chunk.rs` (`RowChunks` caching for
  tree-sitter), `outline.rs`, `runnable.rs`, `text_diff.rs`, `available_languages.rs`,
  `file_content.rs`, `proto.rs`.

### 2.5 `multi_buffer` — `crates/multi_buffer/src/`

- `MultiBuffer` (`multi_buffer.rs:74`) holds `snapshot: RefCell<MultiBufferSnapshot>`,
  `buffers: BTreeMap<BufferId, BufferState>`, diff states, `history`.
- `MultiBufferSnapshot` (`:693`): `excerpts: SumTree<Excerpt>`,
  `buffers: TreeMap<BufferId, BufferStateSnapshot>`, `path_keys: Arc<IndexSet<PathKey>>`,
  `diffs: SumTree<DiffStateSnapshot>`, `diff_transforms: SumTree<DiffTransform>`,
  `singleton`, `show_headers`, `edit_count`, `non_text_state_update_count`.
- **`Excerpt` (`:822`) has NO id.** Fields: `path_key`, `path_key_index`, `buffer_id`,
  `range: ExcerptRange<text::Anchor>`, `max_buffer_row`, `text_summary`, `has_trailing_newline`.
  Excerpts are ordered by `PathKey` (`path_key.rs:19`:
  `{ sort_prefix: Option<u64>, path: Arc<RelPath> }`; `PathKey::for_buffer` at `path_key.rs:46`).
- Excerpt management is **path-keyed and declarative**:
  `set_excerpts_for_buffer(buffer, ranges, context_line_count, cx)` (`path_key.rs:68`) and
  `set_excerpts_for_path(path_key, buffer, ranges, ctx, cx)` (`:84`). Doc comment: any existing
  excerpts for this buffer or this path are replaced by the provided ranges. Returns `true`
  if a new excerpt was added. There is no id-based `insert_excerpts_after` / `remove_excerpts`.
- `ExcerptRange<T> { context, primary }` (`:844`) — `context` is what is shown, `primary` is what
  gets highlighted (the search match / diagnostic).
- `MultiBufferDimension` trait (`:182`), implemented for `Point`, `PointUtf16`,
  `MultiBufferOffset`, `MultiBufferOffsetUtf16` (`:193,209,272,286`). Generic methods
  (`summary_for_anchor::<MBD>` `:4758`, `text_summary_for_range::<MBD>` `:4594`) use it.
- `MBTextSummary` (`:902`) mirrors `TextSummary` but with `len: MultiBufferOffset`.
- Diff transforms (`:714`) splice **deleted** hunk text (from the `BufferDiff` base text) into the
  multibuffer output — the main reason multibuffer offsets differ from buffer offsets even in
  singleton mode.
- `Anchor` (`anchor.rs:26`) and `ExcerptAnchor { text_anchor, path: PathKeyIndex, diff_base_anchor }`
  (`:18`). `diff_base_anchor` positions the anchor inside deleted-hunk text.
- `MultiBuffer::edit(edits, autoindent_mode, cx)` (`:1372`); `edit_before` (`:1391`, autoindent
  from the following line); `edit_non_coalesce` (`:1410`).
- Test builders: `build_simple(text, cx)` (`:3156`), `build_multi::<N>` (`:3161`),
  `build_from_buffer` (`:3187`), `build_random` (`:3191`), `randomly_edit` (`:3200`),
  `randomly_edit_excerpts` (`:3238`), `randomly_mutate` (`:3324`).

### 2.6 `buffer_diff` — `crates/buffer_diff/src/buffer_diff.rs` (4 362 lines)

`BufferDiff` (`:22`), `BufferDiffSnapshot` (`:48`), `DiffHunk` (`:117`),
`DiffHunkStatus` / `DiffHunkStatusKind` (`:87,93`), `DiffHunkSecondaryStatus`
(`:102`, index-vs-head staging), `BufferDiffEvent` (`:1561`). Attach with
`MultiBuffer::add_diff` / `add_inverted_diff` (`multi_buffer.rs:2255,2272`); expansion via
`expand_diff_hunks` / `collapse_diff_hunks` / `set_all_diff_hunks_expanded` (`:2298,2302,2306`).

---

## 3. `crates/editor/` module map

Entry point `crates/editor/src/editor.rs` (12 380 lines); module list at `editor.rs:12-67`.

| Module | Path (lines) | Owns |
|---|---|---|
| `actions` | `src/actions.rs` (992) | **all** action structs / `actions!` blocks for the `editor::`, `go_to_line::`, `debugger::`, `markdown::` namespaces |
| `display_map` | `src/display_map.rs` (4 354) | `DisplayMap`, `DisplaySnapshot`, `DisplayPoint`, highlight plumbing. Excellent module doc at lines 1-68 |
| - `inlay_map` | `display_map/inlay_map.rs` (2 567) | inlay hints / colors / debugger values spliced into text. Best-documented layer, read it first |
| - `fold_map` | `display_map/fold_map.rs` (2 482) | folds, `FoldPlaceholder`, `ChunkRenderer`, crease integration |
| - `tab_map` | `display_map/tab_map.rs` (1 758) | hard tab to column expansion |
| - `wrap_map` | `display_map/wrap_map.rs` (1 962) | soft wrap (async; is itself a `gpui::Entity`) |
| - `block_map` | `display_map/block_map.rs` (5 104) | blocks, excerpt headers, folded buffers, spacers, `CustomBlockId` |
| - `crease_map`, `custom_highlights`, `invisibles`, `dimensions` | | creases; highlight chunk merging; invisible-char rendering; dimension macros |
| `element` | `src/element.rs` (12 235) + `element/mouse.rs`, `element/header.rs` | `EditorElement`: layout, paint, and **action registration** (`register_actions` `:269`, `register_action` helper `:10355`) |
| `movement` | `src/movement.rs` (1 648) | all cursor-motion math on `DisplayPoint`; also used by vim |
| `selections_collection` | `src/selections_collection.rs` (1 603) | `SelectionsCollection`, `MutableSelectionsCollection` |
| `selection` | `src/selection.rs` (2 426) | `Editor::change_selections`, selection effects/history, editor-to-editor sync |
| `input` | `src/input.rs` (3 094) | `handle_input`, autoclose, autosurround, IME |
| `split` / `split_editor_view` | `src/split.rs` (6 485), `src/split_editor_view.rs` (695) | fork-specific side-by-side diff (companion display maps) |
| `inlays` | `src/inlays.rs`, `inlays/inlay_hints.rs` (5 191) | LSP inlay hint lifecycle |
| `semantic_tokens` | `src/semantic_tokens.rs` (2 942) | LSP semantic token highlighting |
| `hover_popover` / `hover_links` | (3 348 / 3 266) | hover docs; cmd-click go-to |
| `code_context_menus` | `src/code_context_menus.rs` (2 202) | completions menu + code-action menu |
| `completions`, `code_actions`, `code_lens`, `document_symbols`, `document_colors`, `document_links`, `folding_ranges`, `signature_help`, `linked_editing_ranges` | | one LSP feature each |
| `git` | `src/git.rs` (1 971), `git/blame.rs` (1 707) | diff-hunk actions; `GitBlame` (`SumTree<GitBlameEntry>`) |
| `diagnostics` | `src/diagnostics.rs` (891) | inline diagnostics, `DiagnosticRenderer` hook |
| `scroll` | `src/scroll.rs` + `scroll/{actions,autoscroll,scroll_amount}.rs` | `ScrollManager`, `ScrollAnchor`, `Autoscroll` |
| `items` | `src/items.rs` (2 808) | `workspace::Item` impl, serialization, breadcrumbs |
| `navigation` | `src/navigation.rs` (2 538) | go-to-definition / references / symbol navigation |
| `bracket_colorization`, `highlight_matching_bracket`, `indent_guides`, `jsx_tag_auto_close`, `rewrap`, `clipboard`, `bookmarks`, `runnables`, `fold`, `persistence`, `config`, `blink_manager`, `mouse_context_menu`, `markdown_actions` | | self-describing |
| `rust_analyzer_ext`, `clangd_ext`, `lsp_ext` | | server-specific extras; both `apply_related_actions(editor, window, cx)` are called from `element.rs:277-278` |
| `test` | `src/test.rs`, `test/editor_test_context.rs` (896), `test/editor_lsp_test_context.rs` (527) | test harness |

Key structs: `Editor` (`editor.rs:901`), `EditorSnapshot` (`editor.rs:1162`, derefs to
`DisplaySnapshot` at `:11651`), `EditorMode` (`editor.rs:424`: `SingleLine` / `AutoHeight` /
`Full` / `Minimap`).

### How `DisplayMap` layers transforms

`DisplayMap` (`display_map.rs:213`) owns `inlay_map`, `fold_map`, `tab_map`,
`wrap_map: Entity<WrapMap>`, `block_map` plus `text_highlights` / `inlay_highlights` /
`semantic_token_highlights` / `crease_map`. Each layer exposes the same shape:
a `Transform` enum, a `TransformSummary { input, output }`, a `Snapshot`, coordinate newtypes,
`sync(snapshot, edits) -> (new_snapshot, edits_in_my_space)`, `chunks()`, and row iterators.
The module doc at `display_map.rs:17-58` is the authoritative description — re-read it before
touching any layer.

**Buffer offset to DisplayPoint, the rule of thumb**
- Going *down*: `MultiBufferOffset -> Point -> DisplayPoint` via `point_to_display_point(pt, Bias::Left)`.
- Going *up*: `DisplayPoint -> MultiBufferOffset` via `dp.to_offset(&display_snapshot, bias)`;
  clip first with `display_snapshot.clip_point(dp, bias)` if the point came from the mouse, from
  arithmetic, or from a previous snapshot.
- **Never** store a `DisplayPoint` across an edit or a settings change (wrap width, font size,
  folds, inlay refresh all invalidate it). Store an `Anchor` and re-derive per frame.

---

## 4. How to add an editor action (step by step)

**Template to copy: `markdown::ToggleBlockQuote`** — the smallest complete example in the tree.

| Step | File | Line |
|---|---|---|
| 1. declare | `crates/editor/src/actions.rs` | `:400-407` |
| 2. handler | `crates/editor/src/markdown_actions.rs` | `:4-29` |
| 3. register | `crates/editor/src/element.rs` | `:636` |
| 4. keymap | `assets/keymaps/default-*.json` | (this one is unbound; see `editor::ToggleComments` at `default-macos.json:349`) |
| 5. test | `crates/editor/src/editor_tests.rs` | `:42779-42872` |

A second, payload-carrying template: `editor::ConvertToUpperCase` —
`actions.rs:463` (declaration), `editor.rs:6875` (handler),
`element.rs:598` (registration), `assets/keymaps/macos/sublime_text.json:51` (keymap),
`editor_tests.rs:8004` (test).

### Step 1 — Define the action

Unit action (no payload) — add to an existing `actions!` block (`actions.rs:409` for `editor`):

```rust
actions!(
    editor,                        // namespace -> "editor::MyNewThing"
    [
        /// Doc comment is REQUIRED - it becomes the command-palette description
        /// and the JSON-schema description.
        MyNewThing,
    ]
);
```

Action with payload (deserialized from the keymap JSON array form):

```rust
/// Does the thing, optionally in reverse.
#[derive(PartialEq, Clone, Deserialize, Default, JsonSchema, Action)]
#[action(namespace = editor)]
#[serde(deny_unknown_fields)]
pub struct MyNewThing {
    #[serde(default)]              // or #[serde(default = "util::serde::default_true")]
    pub reverse: bool,
}
```

Attribute forms in use: `#[action(name = "Toggle")]` to override the action name
(`actions.rs:395`), `#[action(deprecated_aliases = ["..."])]` for renames
(`crates/wu_actions/src/lib.rs:43`), `#[action(namespace = wu, no_json, no_register)]` for actions
dispatched only programmatically (`wu_actions/src/lib.rs:34`).
Namespaces declared in `actions.rs`: `editor`, `go_to_line`, `debugger`, `markdown`.

### Step 2 — Implement the handler

Signature must be `fn(&mut Editor, &A, &mut Window, &mut Context<Editor>)`.
Put it in the most specific existing module; a new small module is acceptable per `.rules`
(prefer existing files unless it is a genuinely new logical component).

```rust
// crates/editor/src/my_feature.rs   (add `mod my_feature;` to the list at editor.rs:12-67)
use super::*;

impl Editor {
    pub fn my_new_thing(
        &mut self,
        action: &MyNewThing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.read_only(cx) {
            return;
        }

        // 1. Snapshot once. Never re-read the buffer inside the loop.
        let display_snapshot = self.display_snapshot(cx);
        let buffer = self.buffer.read(cx).snapshot(cx);

        // 2. Collect edits + the selections you want afterwards, as ANCHORS.
        let mut edits = Vec::new();
        let mut new_selections = Vec::new();
        for selection in self.selections.all_adjusted(&display_snapshot) {
            let range = buffer.point_to_offset(selection.start)
                ..buffer.point_to_offset(selection.end);
            let old = buffer.text_for_range(range.clone()).collect::<String>();
            let new = transform(&old, action.reverse);
            new_selections.push(Selection {
                id: selection.id,
                start: buffer.anchor_before(range.start),
                end: buffer.anchor_after(range.end),
                reversed: selection.reversed,
                goal: SelectionGoal::None,
            });
            if new != old {
                edits.push((range, new));
            }
        }
        if edits.is_empty() {
            return;
        }

        // 3. One transaction => one undo step.
        self.transact(window, cx, |this, window, cx| {
            this.buffer.update(cx, |buffer, cx| buffer.edit(edits, None, cx));
            this.change_selections(Default::default(), window, cx, |s| s.select(new_selections));
            this.request_autoscroll(Autoscroll::fit(), cx);
        });
    }
}
```

The reference implementation of exactly this shape is `Editor::manipulate_text`
(`crates/editor/src/editor.rs:7066-7122`) — every case-conversion action goes through it.
For line-oriented actions copy `Editor::manipulate_mutable_lines` (`editor.rs:6732`), which is what
`toggle_markdown_block_quote` uses.

### Step 3 — Register the handler

Add one line inside `EditorElement::register_actions`
(`crates/editor/src/element.rs:269`, the list runs to about `:760`):

```rust
register_action(editor, window, Editor::my_new_thing);
```

**Put it inside the `if !editor.read(cx).read_only(cx) { ... }` block (starts `element.rs:573`)
if the action mutates the buffer.** Read-only actions go above it.

`register_action` (`element.rs:10355`) wires `window.on_action(TypeId::of::<T>(), ..)` and only
fires in `DispatchPhase::Bubble`. Call `cx.propagate()` in your handler when the action does not
apply so it falls through (see the `compose_completion` closure at `element.rs:537-543`).

Alternatives:
- **Workspace-level** action (needs a `Workspace`, e.g. "new file"): register in `editor::init`
  via `workspace.register_action(..)` (`editor.rs:321-332`).
- **Conditional / dynamically added** action: `Editor::register_action::<A>(listener)`
  (`editor.rs:10442`) returns a `Subscription` you must store; the stored closures are replayed at
  `element.rs:271-275`. This is how `rust_analyzer_ext::apply_related_actions` works.

### Step 4 — Keymap entry (optional)

Default keymaps: `assets/keymaps/default-{macos,linux,windows}.json`, plus per-editor emulation
sets under `assets/keymaps/{macos,linux}/*.json` and `specific-overrides*.json`.
Each file is a list of `{ "context": "...", "use_key_equivalents": true, "bindings": { ... } }`.

```jsonc
{
  "context": "Editor && mode == full && !menu",
  "use_key_equivalents": true,
  "bindings": {
    "ctrl-k ctrl-m": "editor::MyNewThing",                              // unit action
    "ctrl-k ctrl-shift-m": ["editor::MyNewThing", { "reverse": true }], // with payload
  },
},
```

`script/check-keymaps` enforces: no `cmd-` outside macOS files; never `super-`, `win-`, `fn-`.
Run it before committing keymap changes.

### Step 5 — Test

See section 5 for the full template. Two ways to invoke:
- direct call: `cx.update_editor(|e, window, cx| e.my_new_thing(&MyNewThing::default(), window, cx));`
- through dispatch, which also verifies registration:
  `cx.dispatch_action(MyNewThing::default());` (used at `editor_tests.rs:42877`).

### Step 6 — Housekeeping

- Action JSON schemas are regenerated by `script/update-json-schemas` (uses
  `crates/schema_generator`). Run it after adding or changing an action's fields.
- The action appears in the command palette automatically from its doc comment; use
  `command_palette_hooks` to hide it in specific contexts.
- Add the `README.md` `> [!IMPORTANT]` lines per `.rules`.

---

## 5. Test templates

### 5.1 The marker syntax (`crates/util/src/test/marked_text.rs:83-112`)

- `ˇ` (U+02C7 caron) — an empty cursor. `alt-shift-t` on a US Mac keyboard.
- `«...»` — a selection range.
- `«ˇtext»` — reversed selection (caron beside the head, inside the range).
- `«textˇ»` — forward selection.
- `•` in the *input* string is replaced by a space (lets you express trailing spaces in source).
- Multiple markers give multiple cursors/selections.
- Helpers: `marked_text_ranges` (`:113`), `marked_text_offsets` (`:181`),
  `marked_text_ranges_by` (`:31`, arbitrary marker chars, e.g. `[` `]` for LSP ranges),
  `generate_marked_text` (`:195`, the inverse, used to build assertion messages).

### 5.2 Editor action test (copy this)

```rust
// crates/editor/src/editor_tests.rs
#[gpui::test]
async fn test_my_new_thing(cx: &mut TestAppContext) {
    init_test(cx, |_| {});                       // editor_tests.rs:37045
    let mut cx = EditorTestContext::new(cx).await;

    cx.set_state(indoc! {"
        one «twoˇ» three
        fouˇr five
    "});
    cx.update_editor(|e, window, cx| e.my_new_thing(&MyNewThing::default(), window, cx));
    cx.assert_editor_state(indoc! {"
        one «TWOˇ» three
        «FOURˇ» five
    "});
}
```

`init_test(cx, f)` (`editor_tests.rs:37045`) loads test fonts, a `SettingsStore::test`,
theme + release channel, calls `editor::init` and `zlog::init_test()`, then applies `f` to
`AllLanguageSettingsContent`. Pass a closure to tweak settings (tab size, soft wrap, formatter...).

`EditorTestContext` (`crates/editor/src/test/editor_test_context.rs:36`) — key methods:

| Method | Line | Purpose |
|---|---|---|
| `new(cx)` | `:45` | `FakeFs` + `Project::test` + one Plain Text buffer at `/root/file` (`C:\root\file` on Windows) |
| `new_multibuffer::<N>(cx, excerpts)` | `:127` | multi-excerpt editor |
| `for_editor` / `for_editor_in` | `:116,106` | wrap an editor you built yourself |
| `set_state(marked)` / `set_selections_state(marked)` | `:389,410` | set text+selections / selections only |
| `assert_editor_state(marked)` | `:609` | assert buffer text + selections |
| `assert_display_state(marked)` | `:620` | same but against `display_text()` (folds/inlays applied) |
| `assert_state_with_diff(String)` | `:434` | text + selections + expanded diff hunks (`+`/`-` prefixes) |
| `assert_excerpts_with_selections(marked)` | `:439` | multibuffer excerpt layout |
| `assert_editor_background_highlights` / `assert_editor_text_highlights` | `:631,649` | highlight ranges by `HighlightKey` |
| `update_editor` / `editor` / `update_buffer` / `buffer` / `multibuffer` / `update_multibuffer` | `:191,182,242,220,198,205` | scoped access |
| `simulate_keystroke("ctrl-x")` | `:266` | full key dispatch |
| `ranges(marked)` / `display_point(marked)` / `text_anchor_range(marked)` / `pixel_position(marked)` | `:276,282,319,290` | derive positions from markers |
| `set_head_text` / `set_index_text` / `clear_index_text` / `assert_index_text` | `:331,352,344,365` | git diff fixtures |
| `run_until_parked()` | `:271` | drain the executor |
| `wait_for_autoindent_applied()` | `:325` | after edits with autoindent |
| `buffer_text()` / `display_text()` / `buffer_snapshot()` / `language_registry()` | `:212,216,252,230` | reads |

The `ContextHandle` returned by `set_state` keeps the initial state in the assertion message —
bind it (`let _state = cx.set_state(..)`) when the assertion happens later or in a helper.
All the assert helpers are `#[track_caller]`; keep that attribute on any wrapper you write.

### 5.3 LSP-backed test

```rust
#[gpui::test]
async fn test_my_lsp_thing(cx: &mut TestAppContext) {
    init_test(cx, |_| {});
    let mut cx = EditorLspTestContext::new_rust(
        lsp::ServerCapabilities {
            document_formatting_provider: Some(lsp::OneOf::Left(true)),
            ..Default::default()
        },
        cx,
    ).await;

    cx.set_state(indoc! {"fn main() { ˇ }"});

    cx.lsp.set_request_handler::<lsp::request::Formatting, _, _>(|_params, _cx| async move {
        Ok(Some(vec![lsp::TextEdit::new(
            lsp::Range::new(lsp::Position::new(0, 0), lsp::Position::new(0, 0)),
            "// header\n".into(),
        )]))
    });

    cx.update_editor(|e, window, cx| e.format(&Default::default(), window, cx))
        .unwrap()
        .await
        .unwrap();
    cx.run_until_parked();
    cx.assert_editor_state(indoc! {"// header\nfn main() { ˇ }"});
}
```

`EditorLspTestContext` (`crates/editor/src/test/editor_lsp_test_context.rs:29`) derefs to
`EditorTestContext` and adds `lsp: FakeLanguageServer`, `workspace`, `buffer_lsp_url`.
Constructors: `new(language, capabilities, cx)` (`:49`), `new_rust` (`:163`),
`new_typescript` (`:170`), `new_tsx` (`:270`). Helpers: `lsp_range("[..]")` (`:439`),
`to_lsp_range(range)` (`:445`), `to_lsp(offset)` (`:463`), `set_request_handler::<T>` (`:483`),
`notify::<T>(params)` (`:500`), `update_workspace` (`:476`).

Fake servers come from `language_registry.register_fake_lsp(name, FakeLspAdapter { capabilities, .. })`
(`crates/language/src/language_registry.rs:301`), which returns a stream you `.next().await` to get
the `FakeLanguageServer` (see `editor_lsp_test_context.rs:72-143`).

### 5.4 Project / FakeFs test

```rust
#[gpui::test]
async fn test_something_with_files(cx: &mut TestAppContext) {
    init_test(cx, |_| {});
    let fs = FakeFs::new(cx.executor());
    fs.insert_tree(
        path!("/root"),
        json!({ ".git": {}, "src": { "main.rs": "fn main() {}\n" } }),
    ).await;

    let project = Project::test(fs.clone(), [path!("/root").as_ref()], cx).await;
    let buffer = project
        .update(cx, |p, cx| p.open_local_buffer(path!("/root/src/main.rs"), cx))
        .await
        .unwrap();
    cx.run_until_parked();
    // ...
}
```

Always use `util::path!` (and `util::rel_path::rel_path!`) for literals so the test passes on
Windows — `EditorTestContext::root_path()` is `C:\root` there (`editor_test_context.rs:94-102`).
`fs.insert_file(path, contents)` is the single-file variant (`editor_tests.rs:10379`).

### 5.5 `DisplaySnapshot`-level test (no editor entity)

```rust
use crate::test::marked_display_snapshot;   // crates/editor/src/test.rs:46

#[gpui::test]
fn test_movement(cx: &mut gpui::App) {
    init_test(cx, |_| {});
    // '|' markers become DisplayPoints
    let (snapshot, display_points) = marked_display_snapshot("one |two| three", cx);
    assert_eq!(movement::right(&snapshot, display_points[0]), display_points[1]);
}
```

Real usages: `crates/editor/src/movement.rs:1147,1166,1205,1312,1350` and
`crates/editor/src/display_map.rs:3865,3915`.
Other helpers in `crates/editor/src/test.rs`: `select_ranges` (`:84`),
`assert_text_with_selections` (`:102`), `build_editor` (`:122`), `build_editor_with_project` (`:130`),
`editor_content_with_blocks` (`:169`, renders block/header rows into the text with `§` markers),
`test_font` (`:28`).

### 5.6 `language::Buffer`-level test

```rust
#[gpui::test]
fn test_thing(cx: &mut gpui::App) {
    init_settings(cx, |_| {});                 // crates/language/src/buffer_tests.rs:5312
    cx.new(|cx| {
        let mut buffer = Buffer::local("one\ntwo\n", cx).with_language(rust_lang(), cx);
        buffer.edit([(0..0, "zero\n")], Some(AutoindentMode::EachLine), cx);
        assert_eq!(buffer.text(), "zero\none\ntwo\n");
        buffer.check_invariants();
        buffer
    });
}
```

### 5.7 Randomized / property tests

`#[gpui::test(iterations = 100)] fn t(cx: &mut App, mut rng: StdRng)`.
Examples: `crates/text/src/tests.rs:51,794`, `crates/multi_buffer/src/multi_buffer_tests.rs:1250,3861`,
`crates/editor/src/editor_tests.rs:24656` (`seeds(31)`), `crates/editor/src/editor_tests/property_test.rs`,
`crates/sum_tree/src/property_test.rs`, `crates/language/src/buffer_tests.rs:5320`.
Mutation helpers: `MultiBuffer::randomly_edit` / `randomly_edit_excerpts` / `randomly_mutate`
(`multi_buffer.rs:3200,3238,3324`), `util::RandomCharIter`, `util::test::sample_text` (`test.rs:80`).
If you change a data-structure invariant, **extend the existing randomized test** rather than
writing a new one — it already builds the world for you.

---

## 6. `language` / `language_core` / `languages` / `grammars`

### Type map

| Type | Where |
|---|---|
| `LanguageConfig` (the TOML shape) | `crates/language_core/src/language_config.rs:28` |
| `LanguageMatcher` (`path_suffixes`, `first_line_pattern`) | `.../language_config.rs:227` |
| `LanguageConfigOverride` (scoped overrides) | `.../language_config.rs:364` |
| `LanguageName(SharedString)` | `crates/language_core/src/language_name.rs:29` |
| `Grammar`, `GrammarId` | `crates/language_core/src/grammar.rs:83,15` |
| `QueryFile` enum + `LanguageQueries` | `crates/language_core/src/queries.rs:9,120` |
| `HighlightMap`, `HighlightId` | `crates/language_core/src/highlight_map.rs` |
| `CodeLabel` | `crates/language_core/src/code_label.rs` |
| `Language` | `crates/language/src/language.rs:937` (`new` `:947`, `with_queries` `:985`, `set_theme` `:1152`) |
| `LanguageScope` (per-node config overrides) | `crates/language/src/language.rs:908` |
| `LspAdapter` trait | `crates/language/src/language.rs:507` |
| `LspAdapterDelegate` | `crates/language/src/language.rs:486` |
| `CachedLspAdapter::new` | `crates/language/src/language.rs:358` |
| `LanguageRegistry` | `crates/language/src/language_registry.rs:37` |
| `LoadedLanguage` | `.../language_registry.rs:97` |
| `LanguageServerName(SharedString)` | `crates/lsp/src/lsp.rs:171` |
| `SyntaxTheme` | `crates/syntax_theme/src/syntax_theme.rs:14` |

### Where a language actually lives

```
crates/grammars/src/<lang>/            <- config + queries, embedded via rust_embed (GrammarDir)
    config.toml                        <- LanguageConfig (TOML)
    highlights.scm  brackets.scm  indents.scm  outline.scm
    injections.scm  overrides.scm  redactions.scm  runnables.scm
    textobjects.scm  debugger.scm
    semantic_token_rules.json          (optional)
crates/grammars/src/grammars.rs        <- native_grammars() (:17), load_config (:46),
                                          load_config_for_feature (:64), load_queries (:86)
crates/languages/src/<lang>.rs         <- LspAdapter / ContextProvider / ToolchainLister impls
crates/languages/src/lib.rs            <- init() registers everything (:57)
```

Query file names are matched by the `strum` serializations on `QueryFile`
(`crates/language_core/src/queries.rs:9-30`), so the file must be named exactly
`highlights.scm`, `brackets.scm`, `outline.scm`, `indents.scm`, `injections.scm`,
`overrides.scm`, `redactions.scm`, `runnables.scm`, `debugger.scm`, `textobjects.scm`.

### To add a built-in language

1. Add the tree-sitter crate to the workspace `Cargo.toml` and `crates/grammars/Cargo.toml`.
2. Add `("<name>", tree_sitter_x::LANGUAGE.into())` to `native_grammars()`
   (`crates/grammars/src/grammars.rs:17`).
3. Create `crates/grammars/src/<name>/config.toml` plus the `.scm` queries you need.
   Copy `crates/grammars/src/rust/` as the reference — it contains every query type.
4. Add a `LanguageInfo { name: "<name>", adapters: vec![..], .. }` entry to `built_in_languages`
   in `crates/languages/src/lib.rs:89-221`. `register_language` (`:337`) then does
   `load_config` + `grammars::load_queries` + `LanguageRegistry::register_language`.
5. If it needs an LSP: implement `LspAdapter` in `crates/languages/src/<name>.rs` and either
   attach it via the `adapters:` field or expose it globally with
   `languages.register_available_lsp_adapter(LanguageServerName(..), adapter)` (`lib.rs:248`)
   so users can opt in through the `language_servers` setting.
6. `LanguageInfo` also carries `context: Option<Arc<dyn ContextProvider>>` (task variables),
   `toolchain: Option<Arc<dyn ToolchainLister>>`, `manifest_name` (e.g. `Cargo.toml`),
   `semantic_token_rules` (`crates/languages/src/lib.rs:330`).

### To modify a language's highlighting / indents / outline

Edit the `.scm` under `crates/grammars/src/<lang>/`. Debug with the
**Syntax Tree view** and **Highlights Tree view** in `crates/language_tools`
(`syntax_tree_view.rs`, `highlights_tree_view.rs`). `script/analyze_highlights.py` exists for
bulk analysis.

### `LanguageRegistry` (`crates/language/src/language_registry.rs`)

Registration: `register_language` (`:378`), `register_extension_language` (`:398`),
`register_lsp_adapter` (`:271`), `register_available_lsp_adapter` (`:220`),
`register_native_grammars` (`:456`), `register_wasm_grammars` (`:468`), `add(Arc<Language>)` (`:515`).
Lookup: `language_for_name` (`:558`), `language_for_file` (`:612`), `language_for_file_path` (`:627`),
`load_language` (`:661`), `available_language_for_modeline_name` (`:602`),
`lsp_adapters(&LanguageName)` (`:888`), `adapter_for_name` (`:906`).
Test support: `register_test_language` (`:190`), `register_fake_lsp` (`:301`),
`register_fake_lsp_adapter` (`:315`), `create_fake_language_server` (`:925`).
`subscribe()` (`:531`) + `version()` (`:537`) drive reparse-on-registry-change — see
`Buffer::reparse` checking `language_registry_version` (`buffer.rs:1907`).

---

## 7. `lsp` and `project::LspStore`

### `crates/lsp/src/lsp.rs` (2 457 lines)

- `LanguageServer` (`:115`) owns the child process, `outbound_tx`, notification/response handler
  maps, `capabilities: RwLock<ServerCapabilities>`, io tasks, `workspace_folders`, `root_uri`.
- Lifecycle: construct (`:429`) -> `initialize(...)` (`:1083`) -> use -> `shutdown()` (`:1115`).
- Requests: `request::<T>(params, timeout)` (`:1412`) and `request_with_timer` (`:1435`).
  Default timeout `DEFAULT_LSP_REQUEST_TIMEOUT` = 120 s (`:50,54`), overridable per project via
  `ProjectSettings::global_lsp_settings.get_request_timeout()` (used at `lsp_store.rs:5673`).
- Notifications out: `notify::<T>(params)` (`:1613`). Handlers in: `on_notification::<T>` (`:1177`)
  and `on_request::<T>` (`:1189`) — both return a `Subscription` you must keep alive.
- Capabilities: `capabilities()` (`:1370`), `adapter_server_capabilities()` (`:1375`),
  `update_capabilities(f)` (`:1385`). Dynamic registration lives in
  `crates/project/src/lsp_store/dynamic_registration.rs`.
- `LanguageServerId(usize)` (`:154`), `LanguageServerName(SharedString)` (`:171`),
  `LanguageServerSelector::{Id,Name}` (`:145`), `LanguageServerBinary` (`:95`).
- `FakeLanguageServer` (`:1840`) — `set_request_handler::<T>` (`:2000`), `notify::<T>` (`:1963`).
- `crates/lsp/src/input_handler.rs` — stdio framing.

### `crates/project/src/lsp_store.rs` (15 424 lines — the orchestrator)

- `LspStore` (`:4335`) with `mode: LspStoreMode::{Local, Remote}` (`:4324`). `Local` covers both
  the local app and an SSH host; `Remote` is a collab guest that proxies over `AnyProtoClient`.
- Per-buffer LSP cache `lsp_data: HashMap<BufferId, BufferLspData>` (`:4356`), keyed by
  `buffer_version: clock::Global` — stale versions are recomputed, not reused.
- `request_lsp::<R: LspCommand>(buffer, LanguageServerToQuery, request, cx) -> Task<Result<R::Response>>`
  (`:5603`). It (a) proxies upstream when remote, (b) picks a server by `FirstCapable` or
  `Other(id)` **filtered through `check_capabilities`**, (c) converts params, (d) reports
  work-done progress when `status()` is `Some`. **If no capable server exists it returns
  `Ok(Default::default())`, not an error** — handle the empty case.
- `LspCommand` trait (`crates/project/src/lsp_command.rs:91`): every LSP feature implements
  `display_name`, `check_capabilities`, `to_lsp`, `response_from_lsp`, `to_proto`, `from_proto`.
  **This is the file to copy when adding a new LSP request.** Heavier features have their own
  module under `crates/project/src/lsp_store/`: `semantic_tokens.rs`, `document_symbols.rs`,
  `code_lens.rs`, `document_links.rs`, `document_colors.rs`, `folding_ranges.rs`,
  `inlay_hints.rs`, `lsp_ext_command.rs`, `rust_analyzer_ext.rs`, `log_store.rs`,
  `dynamic_registration.rs`.

### Where LSP errors and status surface to the UI

| Signal | Path |
|---|---|
| `window/showMessage`, `window/showMessageRequest` | `lsp_store.rs:1196-1246` produces `LspStoreEvent::LanguageServerPrompt`, handled at `project.rs:2760` |
| Generic notification toast | `LspStoreEvent::Notification(String)` (`lsp_store.rs:4449`) -> `project.rs:2831` -> `Event::Toast` |
| Formatting failure | `lsp_store.rs:4337` `last_formatting_failure` (set at `:6161,6182`) -> `project.rs:3290` -> `activity_indicator.rs:330,616` (clickable status-bar item) |
| Progress / work-done | `on_lsp_progress` and `on_lsp_work_start`/`_progress`/`_end` (`lsp_store.rs:11014-11208`) feed `LanguageServerStatus.pending_work` (`:4489`) -> activity indicator |
| Server binary download/start/stop | `BinaryStatus` via `LanguageRegistry::update_lsp_binary_status` (`language_registry.rs:910`) -> `activity_indicator.rs` |
| Raw RPC + server stderr | `LspStoreEvent::LanguageServerLog` -> `crates/project/src/lsp_store/log_store.rs` -> `crates/language_tools/src/lsp_log_view.rs`, status button `lsp_button.rs` |
| Diagnostics | `LspStoreEvent::DiagnosticsUpdated { server_id, paths }` -> `crates/diagnostics/` (project view, `buffer_diagnostics.rs`, `diagnostic_renderer.rs`) and `editor/src/diagnostics.rs` (inline) |
| Request-failure logging | `should_log_lsp_request_failure` (`lsp_store.rs:4536`) suppresses rust-analyzer's "content modified" and "server cancelled the request" noise |

`LspStoreEvent` is fully enumerated at `lsp_store.rs:4435-4487` (refresh events for inlay hints,
semantic tokens, code lens, document colors/links/symbols, folding ranges, plus `SnippetEdit` and
`WorkspaceEditApplied`).

---

## 8. `project` / `worktree` / paths

### `Project` (`crates/project/src/project.rs:199`)

A facade over stores, each its own `Entity`: `worktree_store`, `buffer_store`, `lsp_store`,
`git_store`, `image_store`, `dap_store`, `task_store`, `toolchain_store`, `bookmark_store`,
`breakpoint_store`, `settings_observer`, `snippets: Entity<SnippetProvider>`, `environment`.
`git_diff_debouncer: DebouncedDelay<Self>` (`:219`), `buffers_needing_diff` (`:218`).
Test constructor: `Project::test(fs, [roots], cx)`.

### `ProjectPath` (`project.rs:370`)

```rust
pub struct ProjectPath { pub worktree_id: WorktreeId, pub path: Arc<RelPath> }
```
`from_file(&dyn language::File, cx)` (`:376`), `root_path(worktree_id)` (`:400`),
`starts_with` (`:406`), proto round-trip `from_proto`/`to_proto` (`:382,390`).

### Path type conventions (get these right; Windows CI depends on it)

| Type | Crate / file | Meaning |
|---|---|---|
| `RelPath` / `RelPathBuf` | `crates/path/src/rel_path.rs:29,36` (re-exported as `util::rel_path`) | **Guaranteed relative, normalized, valid UTF-8, stored POSIX-style with `/`** regardless of host OS. This is the canonical in-project path. Build with `RelPath::new(path, path_style)` (`:59`) or `RelPath::from_unix_str` |
| `AbsPath` / `AbsPathBuf` | `crates/path/src/abs_path.rs:18` | absolute + valid UTF-8. `join_rel_path` (`:43`) is the bridge back to relative paths |
| `SanitizedPath` | `crates/util/src/paths.rs:238` | `repr(transparent)` `Path` that strips Windows `\?\` UNC prefixes (via `dunce::simplified`). Worktree roots are `Arc<SanitizedPath>` (`worktree.rs:181`) |
| `PathStyle` | `crates/path/src/path.rs:30` | `Unix` or `Windows`; carried on `File::path_style(cx)` so remote projects render paths in the *remote's* style |
| `PathMatcher` | `crates/util` | glob matching for include/exclude settings |
| `paths::*()` | `crates/paths/src/paths.rs` | every app data dir: `config_dir`, `data_dir`, `settings_file`, `keymap_file`, `languages_dir`, `extensions_dir`, `themes_dir`, `snippets_dir`, `logs_dir`, ... Never hardcode |

Rules:
- **Never** use `std::path::Path` for a project-relative path. Use `Arc<RelPath>`.
- `RelPath::as_unix_str()` for protocol/serialization; `as_std_path()` only when calling the OS.
- Comparing a worktree-relative path with an absolute path is always a bug — go through
  the worktree's absolutize/relativize helpers.
- In tests wrap literals in `util::path!("/root/a")` and `util::rel_path::rel_path!("a/b")`.

### `Worktree` (`crates/worktree/src/worktree.rs`, 7 707 lines)

- `enum Worktree { Local(LocalWorktree), Remote(RemoteWorktree) }` (`:96`).
- `Snapshot` (`:178`): `abs_path: Arc<SanitizedPath>`, `path_style`, `root_name: Arc<RelPath>`,
  `entries_by_path: SumTree<Entry>`, `entries_by_id: SumTree<PathEntry>`, `scan_id`,
  `completed_scan_id`. `LocalSnapshot` (`:250`) adds gitignore state and `git_repositories`.
- `Entry` (`:3953`): `id: ProjectEntryId`, `path: Arc<RelPath>`, kind, `mtime`, `is_ignored`,
  `is_private`, `is_external`.
- `WorkDirectory` (`:205`): `InProject { relative_path }` or
  `AboveProject { absolute_path, location_in_repo }` — a repo root can sit above the project root.
- **File watching:** `start_background_scanner` (`:1339`) spawns `BackgroundScanner` (`:4333`)
  around `fs.watch(&abs_path, FS_WATCH_LATENCY)` (`:1362`). Phases: `InitialScan` ->
  `EventsReceivedDuringInitialScan` -> `Events` (`:4354`). All scanning is on the background
  executor; the foreground `Worktree` receives snapshots via `snapshot_subscriptions`. In tests
  drive it with `cx.run_until_parked()` / `worktree_scans_complete(cx).await`.
- **`.gitignore`:** `IgnoreStack` / `IgnoreStackEntry` (`crates/worktree/src/ignore.rs:5,15`) — a
  linked list of `Gitignore` matchers: `Global` -> `RepoExclude` (`.git/info/exclude`) ->
  per-directory `Some { abs_base_path, ignore, parent }` -> `All`. Built in
  `ignore_stack_for_abs_path` (`worktree.rs:3060`); `LocalSnapshot.ignores_by_parent_abs_path`
  and `repo_exclude_by_work_dir_abs_path` cache them with a dirty flag. Query with
  `snapshot.is_path_ignored(path)` (`:2904`) or `Entry::is_ignored`.
- `RemoteWorktree` (`:162`) applies `proto::UpdateWorktree` messages onto a
  `background_snapshot: Arc<Mutex<..>>` and republishes to the foreground.

---

## 9. Async / threading patterns actually used here

Ground rules (from `.rules`): everything touching `Entity` state runs on the single foreground
thread; `cx.background_spawn` for CPU work; a `Task<T>` is cancelled when dropped, so either
`.detach()`, `.detach_and_log_err(cx)`, `await` it, or **store it in a struct field**.

**Idiom 1 — background compute, foreground apply, self-restart (tree-sitter parsing).**
`crates/language/src/buffer.rs:1849-1925`:

```rust
let parse_task = cx.background_spawn({
    let language = language.clone();
    let language_registry = language_registry.clone();
    async move {
        syntax_snapshot.reparse(&text, language_registry, language);
        syntax_snapshot
    }
});

self.reparse = Some(cx.spawn(async move |this, cx| {   // stored in a field -> cancellable
    let new_syntax_map = parse_task.await;
    this.update(cx, move |this, cx| {
        let parse_again = this.version.changed_since(&parsed_version)
            || language_registry_changed()
            || grammar_changed();
        this.did_finish_parsing(new_syntax_map, None, parse_again, cx);
        this.reparse = None;
        if parse_again { this.reparse(cx, false); }   // buffer changed mid-parse: go again
    })
    .ok();
}));
```

Note the `if self.reparse.is_some() { return; }` guard at `:1854` — one background parse per buffer
at a time. Also note the *synchronous* fast path at `:1872` using `sync_parse_timeout`
(`Duration::ZERO` in tests, `buffer.rs:1137`) so small buffers highlight without a frame delay.
Observe completion via `parse_status()` (`buffer.rs:1950`, a `watch::Receiver<ParseStatus>`).

**Idiom 2 — debounce + cancel-on-supersede, task stored on the struct.**
`crates/editor/src/editor.rs:3520-3600` (document highlights):

```rust
let word_ranges = cx.background_spawn(async move {
    // this might look odd to put on the background thread, but
    // `surrounding_word` can be quite expensive as it calls into tree-sitter language scopes
    let (start_word_range, _) = snapshot.surrounding_word(cursor_buffer_position, None);
    let (end_word_range, _)   = snapshot.surrounding_word(tail_buffer_position, None);
    (start_word_range, end_word_range)
});

let debounce = EditorSettings::get_global(cx).lsp_highlight_debounce.0;
self.document_highlights_task = Some(cx.spawn(async move |this, cx| {
    let (start, end) = word_ranges.await;
    if start != end { /* bail out and clear highlights */ return; }
    cx.background_executor().timer(Duration::from_millis(debounce)).await;
    let highlights = /* provider.document_highlights(..).await.log_err() */;
    this.update(cx, |this, cx| {
        // re-validate: rename not pending, cursor still in the same buffer, then apply
    })
    .log_err();
}));
```

Assigning to `self.document_highlights_task` drops the previous `Task`, cancelling it — that *is*
the debounce/cancellation mechanism. Editor debounce constants live at `editor.rs:274-280`
(`CODE_ACTIONS_DEBOUNCE_TIMEOUT` 250 ms, `SELECTION_HIGHLIGHT_DEBOUNCE_TIMEOUT` 100 ms,
`LSP_REQUEST_DEBOUNCE_TIMEOUT` 50 ms, `SCROLL_CENTER_TOP_BOTTOM_DEBOUNCE_TIMEOUT` 1 s).

**Idiom 3 — reusable debouncer with an explicit cancellation channel.**
`crates/project/src/debounced_delay.rs:26-52`:

```rust
pub fn fire_new<F>(&mut self, delay: Duration, cx: &mut Context<E>, func: F)
where F: 'static + Send + FnOnce(&mut E, &mut Context<E>) -> Task<()>
{
    if let Some(channel) = self.cancel_channel.take() { _ = channel.send(()); }
    let (sender, mut receiver) = oneshot::channel::<()>();
    self.cancel_channel = Some(sender);
    let previous_task = self.task.take();
    self.task = Some(cx.spawn(async move |entity, cx| {
        let mut timer = cx.background_executor().timer(delay).fuse();
        if let Some(previous_task) = previous_task { previous_task.await; }
        futures::select_biased! { _ = receiver => return, _ = timer => {} }
        if let Ok(task) = entity.update(cx, |project, cx| (func)(project, cx)) { task.await; }
    }));
}
```

Used as `Project::git_diff_debouncer` (`project.rs:219`). Prefer this over hand-rolling when you
need "run at most once per N ms, latest wins".

**Idiom 4 — clone-shadowing into the async block** (mandated by `.rules`, used in idiom 1):

```rust
cx.background_spawn({
    let language = language.clone();
    async move { /* uses `language` by value */ }
});
```

**Cooperative yielding:** long loops over buffers use `futures_lite::future::yield_now()`
(imported in `language/src/buffer.rs:33` and `multi_buffer/src/multi_buffer.rs:19`) so the
executor stays responsive. Use it in any loop that can run over a whole large file.

---

## 10. Footguns

### Coordinates

1. **`MultiBufferOffset` vs `BufferOffset` vs plain `usize`.** All three are `usize` inside.
   `.0` on either is a landmine. If you write `MultiBufferOffset(x.0)` you are almost certainly
   skipping a real conversion.
2. **`Point` is ambiguous** (buffer point vs multibuffer point — both are `rope::Point`). No type
   safety here; the `todo(lw)` comments at `multi_buffer.rs:191,206` mark the known gap.
   Name your variables so the space is obvious.
3. **`DisplayPoint` is not a position in the buffer.** Inlay hints, block rows, headers and
   expanded deleted diff hunks occupy display space with no buffer bytes.
   `dp.to_offset(..)` clamps silently.
4. **`DisplayRow` != `MultiBufferRow` != buffer row.** Every layer can insert or remove rows.
5. **Storing a `DisplayPoint` or an offset across an edit.** Use an `Anchor` and re-resolve.
   Anything held across an `await` must be an anchor.
6. **Storing a display coordinate across a settings change.** Changing wrap width, font size or
   tab size rebuilds `wrap_map` / `tab_map`; display coordinates change with no buffer edit.
7. **Byte offsets in the middle of a multi-byte char.** `impl ToOffset for usize`
   (`text.rs:3418`) debug-panics and release-floors. Use `Rope::floor_char_boundary` /
   `ceil_char_boundary` (`rope.rs:82,93`).
8. **UTF-16 confusion at the LSP boundary.** LSP columns are UTF-16 code units. Convert via
   `PointUtf16` and clip through `Unclipped<PointUtf16>`; never cast a byte column to a UTF-16 one.

### Anchors

9. **`multi_buffer::Anchor::cmp` panics** if the anchor's `PathKeyIndex` is not in the snapshot
   (`anchor.rs:105-110`, "anchor's path was never added to multibuffer"). Guard with
   `snapshot.can_resolve(&anchor)` (`multi_buffer.rs:5408`) when the anchor may predate an
   excerpt-set change.
10. **A `text::Anchor` in a tombstoned fragment still resolves to a plausible offset** but orders
    later insertions on the wrong side of the cursor. That is exactly what
    `Editor::refresh_selection_anchors` (`crates/editor/src/selection.rs:~96`) exists to fix —
    read its doc comment before debugging "text goes in the wrong place after undo".
11. **`Anchor::is_valid` and `BufferSnapshot::can_resolve` answer different questions**
    (visible fragment vs observed version). Pick deliberately.
12. **A buffer can be excerpted more than once.** `anchor_in_buffer` picks one excerpt; use
    `buffer_range_to_excerpt_ranges` (`multi_buffer.rs:6719`) when you mean "all of them".

### Editing

13. **Wrap every multi-step mutation in `Editor::transact`** (`editor.rs:8330`) or you produce
    multiple undo steps. `transact` also defers selection effects via
    `with_selection_effects_deferred` (`selection.rs:~113`).
14. **Collect edits first, then apply.** Applying edits one at a time inside a loop invalidates
    the offsets you computed for later ones. `manipulate_text` (`editor.rs:7066`) is the model.
15. **`MultiBuffer::edit` coalesces adjacent edits by default** — use `edit_non_coalesce`
    (`:1410`) to keep them separate; `edit_before` (`:1391`) changes the autoindent reference line.
16. **Undo grouping is 300 ms in release, 0 in tests** (`text.rs:225-230`). A test that "passes"
    with two undo steps may be one step in the real app, and vice versa.
17. **Check `read_only(cx)` and `Capability::editable()`** at the top of every mutating action.
    The read-only guard at `element.rs:573` only gates *registration*, not programmatic calls.

### Snapshots and threading

18. **`MultiBuffer::snapshot(cx)` is not free** — it clones sum trees and re-syncs diff transforms.
    Take it once per operation, not per selection. `MultiBuffer::read(cx)` (`:1329`) returns a
    `Ref` when a borrow suffices.
19. **Never hold an entity `read()` borrow across an `update()`** — double-borrow panic. Same for
    the `Ref` from `MultiBuffer::read` (it is a `RefCell`).
20. **All heavy work goes on a snapshot inside `cx.background_spawn`, and the result must be
    re-validated on the foreground** (version still current? cursor still in the same buffer?
    task not superseded?) before applying. Every idiom in section 9 does this.
21. **Dropping a `Task` cancels it.** If a feature "randomly doesn't happen", look for a task that
    was neither stored in a field nor detached.

### Performance

22. `MultiBufferSnapshot::text()` / `Editor::text(cx)` / `display_text()` **materialize the whole
    buffer into a `String`** (O(n) allocation). Fine in tests, never in a hot path or a render.
    Use `text_for_range`, `chunks`, `chars_at`, `bytes_in_range` instead.
23. `chars_at` / `reversed_chars_at` from a position is fine; `chars()` over the whole rope is not.
24. **`surrounding_word` is expensive** (tree-sitter scope query). The codebase explicitly pushes it
    to a background thread (`editor.rs:3521-3528`). Do not call it per-keystroke on the foreground.
25. **Converting many ranges to display points:** use `DisplaySnapshot::display_point_converter()`
    (`display_map.rs:1712`) with non-decreasing inputs, not N independent `to_display_point` calls
    (each is five tree seeks).
26. **SumTree linear scans.** If you are iterating a `SumTree` to find something, add a `Dimension`
    and `seek` instead.
27. **Row-by-row loops** (`for row in 0..max_row { snapshot.line_len(row) }`) are O(n log n) at
    best. Prefer the `MultiBufferRows` / `line_indents` / `chunks` iterators
    (`multi_buffer.rs:4196,5821,4209`).
28. `#[ztracing::instrument(skip_all)]` is already on most hot conversions — read `ztracing` output
    rather than guessing.

### Tests

29. `#[gpui::test]` tests need `init_test(cx, |_| {})` (editor) or `init_settings(cx, |_| {})`
    (language) or they panic on a missing `SettingsStore`/theme global.
30. **Use `cx.background_executor().timer(..)`, never `smol::Timer::after`** — `.rules` warns that
    `run_until_parked()` will not see untracked timers ("nothing left to run").
31. **Windows paths in tests:** wrap literals in `util::path!` / `rel_path!`;
    `EditorTestContext::root_path()` is `C:\root` there (`editor_test_context.rs:94`).
32. `assert_editor_state` compares **buffer** text; `assert_display_state` compares **display**
    text (folds/inlays). Picking the wrong one produces confusing diffs.
33. `#[track_caller]` is on all the assert helpers — keep it on any wrapper you write, or failures
    point at your helper instead of the test.

### Paths

34. `Arc<RelPath>` everywhere in-project; `PathBuf`/`Path` only at the FS boundary. A `Path`
    obtained from `RelPath::as_std_path()` on a Windows host still uses `/` separators.
35. Mixing worktree-relative and absolute paths is the number-one cause of "file not found" on
    remote projects, because the remote may have a different `PathStyle` than the host.

---

## 11. Debugging tools available in-tree

- **Visual range annotation:** `MultiBufferSnapshot::debug(&ranges, value)` and
  `debug_with_key(key, ranges, value)` (`multi_buffer.rs:6553,6566`, `cfg(debug_assertions)` only)
  paint the `Debug` of any value onto a buffer range inside the editor. Backed by
  `text::debug::GlobalDebugRanges` (`text.rs:3691`). Accepts a position, a `Range`, a `Vec` or a
  slice via `ToMultiBufferDebugRanges` (`multi_buffer.rs:8258+`). The call site is the dedup key,
  so repeated calls replace the previous annotation.
- **Syntax Tree view** — `crates/language_tools/src/syntax_tree_view.rs`: live tree-sitter tree for
  the focused editor, follows the cursor.
- **Highlights Tree view** — `crates/language_tools/src/highlights_tree_view.rs`: which
  `highlights.scm` capture won at each position.
- **LSP log view / LSP button** — `crates/language_tools/src/lsp_log_view.rs`, `lsp_button.rs`:
  raw RPC traffic, server stderr, per-server status.
- **Key context view** — `crates/language_tools/src/key_context_view.rs`: the keymap context of the
  focused element (essential when a new binding "does nothing").
- `zlog` (`crates/zlog`) for logging; `ztracing` (`crates/ztracing`) `#[instrument]` spans for perf.
- Benchmarks: `crates/editor_benchmarks`, `crates/project_benchmarks`, `crates/worktree_benchmarks`,
  `crates/fs_benchmarks`, `crates/benchmarks`.

---

## 12. Adjacent crates in scope (quick reference)

| Crate | Entry | Notes |
|---|---|---|
| `search` | `crates/search/src/search.rs:24` `init` | `buffer_search.rs` (4 313), `project_search.rs` (9 558), `text_finder/` (fork-local fuzzy text finder). Query type is `project::search::SearchQuery` (`crates/project/src/search.rs:76`) |
| `diagnostics` | `crates/diagnostics/src/diagnostics.rs:69` `init` | project diagnostics multibuffer view, `buffer_diagnostics.rs`, `diagnostic_renderer.rs` (registered into the editor via `editor::diagnostics::set_diagnostic_renderer`, `editor/src/diagnostics.rs:32`) |
| `outline` | `crates/outline/src/outline.rs` | outline picker; item types are `language::outline::{Outline, OutlineItem, SymbolPath}` (`crates/language/src/outline.rs:10,21,34`) |
| `snippet` | `crates/snippet/src/snippet.rs:18` `Snippet::parse` | LSP snippet syntax parser -> `TabStop`s |
| `snippet_provider` | `crates/snippet_provider/src/lib.rs:20` `init`, `:144` `SnippetProvider` | watches `paths::snippets_dir()`, per-language lookup at `:259` |
| `prettier` | `crates/prettier/src/prettier.rs:23` `enum Prettier { Real, Test }` | driven by `crates/project/src/prettier_store.rs` |
| `lsp_locations` | `crates/lsp_locations/src/lsp_locations.rs:29` `init`, `:192` `LspPickerKind`, `:249` `LspLocationsPicker` | shared picker for definitions/references/implementations |
| `call_hierarchy` | `crates/call_hierarchy/src/call_hierarchy.rs:156` `init` | `CallHierarchyMode` (`:32`), `CallHierarchyView` (`:160`), settings at `:69` |
| `syntax_theme` | `crates/syntax_theme/src/syntax_theme.rs:14` | `SyntaxTheme::{get, style_for_name, highlight_id, resolve_runs, merge}`; `new_test` / `new_test_styles` for tests |
| `git` | `crates/git/src/repository.rs` (6 959), `blame.rs:196` `BlameEntry`, `status.rs` | editor side is `editor/src/git.rs` + `editor/src/git/blame.rs:96` `GitBlame` |
| `language_tools` | `crates/language_tools/src/language_tools.rs:18` `init` | see section 11 |

### Crate dependency layering (do not invert)

```
sum_tree  ->  rope  ->  text  ->  language (+ language_core, lsp)  ->  multi_buffer  ->  editor
                                                                    project -> worktree
```
`language_core` is the dependency-light half of `language` (config, grammar, queries, highlight map)
and depends only on `path`, `tree-sitter`, `collections`, `gpui_shared_string` — put anything that
needs to be reachable from `settings`/extension code there, not in `language`.
`multi_buffer` does **not** depend on `project`; `editor` depends on both.

---

## 13. Fast orientation checklist for a new task in this domain

1. Which coordinate space does the input arrive in, and which does the output need? Write it down.
2. Is the work per-frame (render) or per-event (action)? Render code gets an `EditorSnapshot`;
   action code takes `display_snapshot(cx)` / `buffer.read(cx).snapshot(cx)` once.
3. Does it mutate the buffer? Then: `read_only` guard, collect edits, one `transact`.
4. Does it touch tree-sitter or the whole file? Then: `cx.background_spawn` + re-validate on return.
5. Does it need to survive edits? Then: `Anchor`, not offsets.
6. Write the test first using `EditorTestContext::set_state` / `assert_editor_state` with `ˇ`/`«»`.
7. `./script/clippy` (never `cargo clippy`), `script/check-keymaps` if you touched keymaps,
   `script/update-json-schemas` if you touched action definitions.
8. Add the `> [!IMPORTANT]` lines to `README.md` per `.rules`.

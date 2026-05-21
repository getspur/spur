# Code as Memory — Phase 1: Code ↔ Git Commit Graph

**Date:** 2026-05-20
**Status:** Revised after two rounds of triple review (codex / claude-code / gemini)
**Crate:** `crates/spur-graph`

## Revision Notes (2026-05-20, round 2)

Both round-2 reviewers (codex, claude-code) gave APPROVE-WITH-CHANGES on
the post-round-1 spec. Round 2 changes:

- **Persisted-artifact surface named concretely.** `GraphIndexArtifact`
  gains `commits`, `symbol_snapshots`, `temporal_edges` collections;
  `TemporalEdgeArtifact` uses tagged `EdgeEndpoint` (not string keys),
  since the existing `GraphEdgeArtifact` cannot express `(sid, sha)`.
  Separate `CommitIndexArtifact` for the commit DAG.
- **MCP consumer namespace corrected.** Phase 1 modifies `code_subgraph`
  / `code_callers` / `code_callees` and adds `code_symbol_history` —
  not the unrelated `graph_*` PM/beads namespace.
- **Per-parent diff is the source of truth.** No "first relevant parent"
  shortcut. Per-parent change records carry `parent_sha`; commit-level
  union resolves the `ChangeKind`.
- **Lazy-snapshot resolution semantics specified.** Snapshots exist only
  for commits-that-touched-the-symbol; resolution finds the latest
  reachable snapshot ≤ target.
- **`LatestSnapshot` edge dropped** (derived at query time; avoids
  unspecified invalidation behavior).
- **`BytesExtractor` reuse pattern** required for the historical walk
  (one per language, reused across blobs).
- **Per-language Jaccard threshold** (calibrated from corpus, not a
  single 0.7); top-2 delta < 0.05 triggers `Added`+`Deleted`.
- **`RenamedFrom` splits vs merges**: splits permitted, merges
  forbidden with `merge_collision` diagnostic.
- **Partial-clone promisor retry** before fail-closed.
- **Bench reports snapshot growth ratio**; >1.5× per 10× commits is
  blocking.

## Revision Notes (2026-05-20, round 1)

Substantive changes from v1, driven by reviewer findings:

- **Symbol snapshots are first-class.** `Commit --touches--> Symbol` targets a
  `SymbolSnapshot` (symbol-at-commit), not the live symbol node. Without
  this, deleted symbols are unqueryable and the resolution API fails on
  its most important case.
- **Schema changes are non-additive.** `NodeKind`, `RelationKind`, and
  `GraphEdge` all require enum/field surgery. The spec now owns that.
- **Default walk is full-DAG reachable** from the target ref (still `main`
  by default), not first-parent-only. First-parent squashes feature-branch
  granularity that the memory thesis explicitly wants to preserve.
- **Separate temporal pointer file.** `.spur/commit-index.pointer.json`,
  not an extension of the current-state `graph-index.pointer.json`.
- **Resolution API returns `Resolution<T>` enum**, not `Option`. Operates
  on a `(GraphIndexArtifact, CommitIndexArtifact)` pair — the crate has
  no unified `Graph` runtime object.
- **Rename detection has a real algorithm.** Exact-identity (stable symbol
  key) first; AST/token heuristics only with confidence scoring. Bench
  reports precision / recall / F1 against a labeled corpus.
- **Concrete first consumer named:** the code-graph MCP tools
  (`code_subgraph` / `code_callers` / `code_callees`) gain a
  temporal-history mode that calls the new API, plus a new
  `code_symbol_history` tool. Phase 1 does not ship without these
  callers. The unrelated `graph_*` PM/beads tools are NOT touched.
- **Failure modes expanded:** missing blob, ref-rename, packed-refs race,
  gitlinks, `git replace`/grafts, non-UTF-8 paths, no-main, partial clone.

## Thesis

The best memory system for agentic AI is the code itself, mirroring the
maxim "the best documentation is the code." Code, git history, and (later)
markdown are durable, version-controlled, on-disk substrates that already
hold everything an agent needs to remember. There is no separate "agent
memory" — there is only **reconstruction fidelity** from these substrates.

Phase 1 builds the foundation: a graph that fuses **code structure**
(already in `spur-graph`) with **git history**, so that "what is this
symbol, when did it change, and what is it called today" becomes a graph
traversal instead of an LLM-driven archaeology pass.

## Guiding Principles

1. **Code on `main` is the source of truth.** Anything that contradicts it
   is, by definition, out of date.
2. **Git history is the durable temporal spine.** It is immutable on
   `main`, content-addressed, and already on disk.
3. **Markdown is *aging*, not *drifting*.** A spec from three months ago
   is a snapshot of intent at the commit it was authored against. (Phase 2
   makes this first-class. Phase 1 ignores markdown.)
4. **Reconstruction = graph traversal.** Materialize cross-substrate edges
   so memory queries are O(graph), not O(LLM search).

## Scope

### In scope (Phase 1)

- New `NodeKind::Commit` and new `SymbolSnapshot` node type.
- New `RelationKind::Touches` with `ChangeKind` payload on the edge.
- `Commit --touches--> File`, `Commit --touches--> SymbolSnapshot`,
  `SymbolSnapshot --renamed_from--> SymbolSnapshot`. (Latest-snapshot
  for a live symbol is derived at query time, not materialized.)
- Git-walk extractor that materializes these for commits reachable from
  the target ref (default `main`), full-DAG by default.
- `extract_symbols_from_bytes` seam so historical blobs can be parsed
  without materializing temporary files.
- Separate `.spur/commit-index.pointer.json` for temporal checkpoints.
- Resolution API: `resolve_symbol_at` and `symbol_history` returning
  `Resolution<T>`.
- Comprehensive failure-mode handling (see Failure Modes table).
- **Concrete first consumer**: the code-graph MCP tools
  (`code_subgraph` / `code_callers` / `code_callees`) gain a
  temporal-history mode that calls the new API, plus a new
  `code_symbol_history` tool (see "Phase 1 Standalone Consumer" below).

### Out of scope (deferred to Phase 2+)

- `DocumentNode`, `mentions_at` edges, supersession. **No markdown
  layer in Phase 1.**
- Branches other than `main`.
- Line-granular blame (we do symbol-granular only).
- A query language. Two named functions plus raw graph traversal.
- External API surface (MCP, CLI flags). Internal Rust API only.

## Data Model

Changes to `crates/spur-graph/src/schema.rs` are **not purely additive**:
existing `NodeKind`, `RelationKind`, `GraphEdge`, `GraphEdgeArtifact`,
and `GraphIndexArtifact` are extended, and a new `CommitIndexArtifact`
is introduced. Current-state graph artifacts written by today's binaries
remain readable; the schema version is bumped so older binaries fail
closed on temporal-aware artifacts (no silent downgrades).

### Persisted-artifact surface (concrete)

`GraphIndexArtifact` gains:
- `commits: Vec<CommitArtifact>`
- `symbol_snapshots: Vec<SymbolSnapshotArtifact>`
- `temporal_edges: Vec<TemporalEdgeArtifact>`

`TemporalEdgeArtifact` uses **tagged endpoints**, not string keys:

```rust
pub enum EdgeEndpoint {
    File { path: PathBuf },
    Symbol { stable_symbol_id: StableSymbolId },
    Snapshot { key: SnapshotKey },
    Commit { sha: GitSha },
}

pub struct TemporalEdgeArtifact {
    pub source: EdgeEndpoint,
    pub target: EdgeEndpoint,
    pub relation: RelationKind,
    pub change_kind: Option<ChangeKind>,
}
```

The existing `GraphEdgeArtifact` (whose endpoints are
`source_stable_symbol_id: String` / `target_stable_symbol_id: String`)
cannot express `(sid, sha)` pairs, which is why `TemporalEdgeArtifact`
is a sibling type rather than an extension of `GraphEdgeArtifact`.

A separate `CommitIndexArtifact` carries the commit DAG and the pointer
state:

```rust
pub struct CommitIndexArtifact {
    pub schema_version: u32,
    pub commits: Vec<CommitArtifact>,           // ordered topologically
    pub refs: HashMap<String, GitSha>,          // ref_name -> tip
    pub indexed_at: String,                     // RFC3339
    pub walk_strategy: WalkStrategy,
}
```

`.spur/commit-index.pointer.json` is the pointer file (small, names the
current artifact); the artifact itself sits alongside the current-state
graph artifact. The pointer is not itself the temporal artifact.

### Naming

The crate's existing symbol identity is a string field
`stable_symbol_id: String` derived from path + `anchor_hash` (see
`crates/spur-graph/src/store/json.rs` and `identity.rs`). This spec uses
`StableSymbolId` as a type alias for that string. There is no `SymbolId`
struct.

### `NodeKind` extension

Add one variant:

```rust
pub enum NodeKind {
    // ... existing variants ...
    Commit,
}
```

For `Commit` nodes: `stable_key = git_sha`, `label = commit_summary`.

### `RelationKind` extension

Add one variant:

```rust
pub enum RelationKind {
    // ... existing variants ...
    Touches,
}
```

### `GraphEdge` / `GraphEdgeArtifact` extension

Add an optional `change_kind: Option<ChangeKind>` field to **both** the
in-memory `GraphEdge` and the persisted `GraphEdgeArtifact`. Required
when `relation == Touches`, absent otherwise.

For temporal edges whose endpoints require richer addressing
(`(stable_symbol_id, commit_sha)` pairs, commit SHAs, file paths), use
`TemporalEdgeArtifact` (defined above) instead of `GraphEdgeArtifact` —
the existing string-keyed endpoint fields cannot express snapshot keys.

```rust
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    /// For files: previous path. For symbols: previous SnapshotKey.
    RenamedFrom(RenamePrev),
}

pub enum RenamePrev {
    File(PathBuf),
    Symbol(SnapshotKey),
}
```

### `SymbolSnapshot` (new — load-bearing)

A `Commit --touches--> Symbol` edge MUST target a `SymbolSnapshot`, not
the live HEAD symbol. This is the change that makes the temporal model
work: deleted symbols, renamed symbols, and symbols-as-of-old-commits all
have durable nodes to point at.

```rust
pub struct SymbolSnapshot {
    pub key: SnapshotKey,           // (stable_symbol_id, commit_sha)
    pub stable_symbol_id: StableSymbolId,
    pub commit: GitSha,
    pub file_path: PathBuf,
    pub entity_name: String,
    pub symbol_kind: String,
    pub enclosing_scope: Option<String>,
    pub byte_range: SourceRange,
    pub line_range: SourceRange,
    pub anchor_hash: String,         // content hash of the symbol body
}

pub struct SnapshotKey {
    pub stable_symbol_id: StableSymbolId,
    pub commit: GitSha,
}
```

A snapshot is unique by `(stable_symbol_id, commit_sha)`. Snapshots are
emitted lazily — one per `(symbol, commit-that-touched-it)` pair — not
one per `(symbol, every-commit)` pair. A commit that didn't touch a
symbol does not create a snapshot for it.

The live (current-state) symbol node continues to exist and represents
HEAD. `SymbolSnapshot` nodes coexist alongside it; the latest snapshot
for a live symbol is derived at query time (see "Edges" below), not
materialized as an edge.

### Edges (full surface)

- `Commit --touches--> File { change_kind }` — file-level change.
- `Commit --touches--> SymbolSnapshot { change_kind }` — symbol-level
  change, with the snapshot capturing what the symbol looked like at
  that commit.
- `SymbolSnapshot --renamed_from--> SymbolSnapshot` — when the
  `change_kind` is `RenamedFrom`, this explicit edge makes traversal a
  single hop instead of having to decode `change_kind`.

**No materialized `LatestSnapshot` edge.** The "latest snapshot for a
symbol" is derived at query time by walking the symbol's snapshots in
the indexed commit DAG and selecting the topologically-latest one
reachable from the indexed tip. Materializing the edge would require
rewriting it on every incremental tip update, and the spec does not
own the invalidation behavior; derive instead.

A single commit can produce many `touches` edges. A single snapshot has
at most one inbound `RenamedFrom` edge (asserted at ingest).

## Extractor (`crates/spur-graph/src/git_walk.rs`)

A new module sibling to the existing `git.rs`.

### Inputs

- Starting ref (default `main`; configurable).
- Stop condition (default: walk to root, or to last-indexed commit on
  incremental runs).
- `walk_strategy: Reachable | FirstParent` (default `Reachable`).

### Extraction seam (required new API)

The existing `extract::*` tree-sitter extractors operate on filesystem
paths. The git walk needs to parse blobs in memory without writing
temporary files, and must reuse the tree-sitter `Parser` and compiled
queries across blobs to avoid recompilation cost.

```rust
/// Reusable extraction context for one language. The historical walk
/// MUST construct one BytesExtractor per language and reuse it across
/// all blobs; constructing a new one per blob recompiles queries and
/// dominates ingest cost.
pub struct BytesExtractor {
    parser: tree_sitter::Parser,
    queries: CompiledQueries,
}

impl BytesExtractor {
    pub fn for_language(language: Language) -> Result<Self, ExtractError>;
    pub fn extract(
        &mut self,
        logical_path: &Path,        // for symbol IDs / scope; never used as I/O
        bytes: &[u8],
    ) -> Result<Vec<ExtractedSymbol>, ExtractError>;
}

/// Convenience: builds + uses a one-shot BytesExtractor. Filesystem
/// extractors call this after reading bytes; current-state behavior is
/// unchanged.
pub fn extract_symbols_from_bytes(
    language: Language,
    logical_path: &Path,
    bytes: &[u8],
) -> Result<Vec<ExtractedSymbol>, ExtractError>;
```

The filesystem extractors are refactored to drive `BytesExtractor`
after reading bytes; current-state extraction behavior is unchanged.
`FactBuilder` id allocation is unchanged for current-state runs.

### Per-commit algorithm

For commit `C` with parent set `parents(C)`:

1. **File-level diff.** For each parent `P`, run
   `git diff-tree -r --name-status --find-renames P C`. Merge the
   per-parent diffs to a single set of file changes for `C`; conflicts
   (a file changed differently relative to different parents) are
   recorded as `Modified` against the merge commit. Emit
   `Commit --touches--> File` edges with `ChangeKind` derived from the
   merged status. (For root commits, diff against the empty tree.)
2. **Symbol-level diff.** For each touched source file (added, modified,
   or renamed) on at least one parent edge:
   - For each parent `P` whose diff against `C` flagged this file as
     changed: fetch the blob at `C` and at `P` via `git cat-file blob`.
     Do not pick an arbitrary "first relevant parent" — per-parent diffs
     are the source of truth for merge-commit symbol changes.
   - Parse each blob through a reused `BytesExtractor`
     (one per language, shared across the whole walk).
   - Emit a `SymbolSnapshot` for every symbol on the `C` side, once per
     symbol (deduplicated across parents). This is where snapshots come
     from — one per `(symbol, commit-that-touched-it)`.
   - Match snapshots across sides per parent. The resulting per-parent
     change records carry `parent_sha` so the per-commit summary can
     reflect parent-specific changes; the commit's emitted `touches`
     edges represent the union (a symbol changed against at least one
     parent is `Modified` for the commit; `Added` only when added vs.
     every parent; `Deleted` only when deleted vs. every parent).
     - **Exact identity match** — same `stable_symbol_id` → `Modified`
       (or skip if `anchor_hash` is identical, meaning the symbol is
       byte-equivalent).
     - Left-only after exact match → candidate `Deleted`.
     - Right-only after exact match → candidate `Added`.
3. **Rename detection (algorithmic, not deferred).** Run in this order on
   the unmatched `Added`/`Deleted` candidates:
   - **Tier 1 — file-rename inheritance.** If git's `--find-renames`
     marked the enclosing file as renamed, and a candidate `Deleted`
     symbol in the old file has the same name + same kind + same
     enclosing-scope shape as a candidate `Added` symbol in the new
     file, match them. Confidence: `High`.
   - **Tier 2 — token-bag Jaccard.** Extract leaf identifiers + literals
     from each candidate's tree-sitter subtree (excluding the symbol's
     own name). Compute Jaccard similarity. Match the highest pair above
     the per-language threshold (calibrated from the rename corpus
     baseline; languages without a calibrated baseline disable Tier 2
     and fall back to `Added` + `Deleted`). Confidence: `Medium`.
   - **Tier 3 — ambiguity.** If the top-2 Jaccard scores for a single
     `Added` candidate differ by less than `0.05` (configurable), or
     the top score is below the language threshold, emit `Added` +
     `Deleted` and record an `ambiguous_rename` diagnostic on the
     commit node. Never emit a guessed `RenamedFrom` with low
     confidence.

   When a `RenamedFrom` is emitted, also create the
   `SymbolSnapshot --renamed_from--> SymbolSnapshot` edge.
4. **Emit** `Commit --touches--> SymbolSnapshot` edges with the resulting
   `ChangeKind`.

### Merge commits

Default: walk the full reachable DAG from the target ref, diff each
commit against each of its parents, merge the per-parent diffs as above.
This preserves feature-branch granularity. The `FirstParent` strategy is
available for repos that explicitly want linearized history.

### Initial commit

Diff against the empty tree. All files and symbols are `Added`; one
`SymbolSnapshot` per symbol at that commit.

## Phase 1 known limitations

- **Total rewrite with rename:** if a symbol is renamed and its body is
  rewritten enough to fall below the calibrated token-bag threshold,
  Phase 1 emits `Added` + `Deleted` with `ambiguous_rename`
  diagnostics instead of claiming a low-confidence `RenamedFrom`.
- **Parameter-only rename:** Tier 2 token bags intentionally include
  leaf identifiers other than the symbol's own name. Renaming every
  parameter can therefore drive Jaccard below threshold even when the
  body shape is otherwise identical; Phase 1 records this as `Added` +
  `Deleted` with diagnostics rather than guessing.

### Incremental ingest

Store temporal checkpoints in a **new** file
`.spur/commit-index.pointer.json`, schema-versioned independently of
the current-state pointer. **Do not** extend
`.spur/graph-index.pointer.json` — the two indices have different
lifecycles and conflating them creates cross-binary version skew.

```jsonc
{
  "schema_version": 1,
  "refs": {
    "main": { "tip_sha": "...", "indexed_at": "2026-05-20T12:00:00Z" }
  }
}
```

On rerun:
- Snapshot all relevant refs via `git for-each-ref` at start.
- If a ref's stored `tip_sha` is an ancestor of the new tip → fast-
  forward walk new commits only.
- If not an ancestor → see force-push handling in Failure Modes.
- If the file is absent → cold walk.

### Failure modes (must be handled inline, not silently)

| Condition | Behavior |
|---|---|
| Stored SHA not an ancestor of new tip (force-push) | Find merge-base of stored and new tip; invalidate all commit nodes between merge-base and stored tip; re-walk from merge-base to new tip. Log loudly. |
| Shallow repository (`git rev-parse --is-shallow-repository` = true) | Fail closed with a clear error. Symbol history would be silently truncated otherwise. |
| Partial clone / missing blob during walk (`git cat-file blob` returns NOENT) | (1) If a promisor remote is configured, retry `git cat-file blob` once — this call triggers the on-demand promisor fetch. (2) If still missing, fail closed for the affected commit; do not emit partial symbol history. Surface a single actionable error naming the missing oid. |
| Target ref does not exist (no `main`, HEAD detached) | Fail closed with actionable error naming the expected ref. No silent fallback to HEAD. |
| Ref moves between snapshot and walk completion | Pin tip OIDs at the `for-each-ref` snapshot. Treat post-snapshot ref movements as fodder for the next incremental run. |
| Ref rename (e.g. `master` → `main`) | Detected when stored ref disappears but a new ref points at a descendant of the stored tip. Treat as continuation; record the rename in the pointer file. |
| Packed-refs vs loose-refs race | Single `for-each-ref` snapshot at start is authoritative for this run. |
| Source file is unparseable by tree-sitter on either side | Emit file-level `touches` edge, skip symbol-level diff for that file, log at `warn`. |
| Source file extension not covered by any extractor | File-level edge only. No symbol-level diff. No log. |
| Gitlink (submodule) entry | Emit a file-level `touches` edge recording the gitlink oid change; do not recurse into the submodule. |
| `git replace` refs or grafts active | Detect via `git config --get-all replace.*` and `.git/info/grafts`. Fail closed unless `allow_replace_refs = true`; record ancestry mode in the pointer file when allowed. |
| Non-UTF-8 paths from git output | Store paths as `Vec<u8>` losslessly on the edge; render with lossy UTF-8 on display only. Never panic. |
| Binary file mistakenly extension-matched | Detect via NUL byte scan on the blob; downgrade to file-level edge with `is_binary = true`. |

## Resolution API (`crates/spur-graph/src/temporal.rs`)

New module. Pure graph traversals over the edges built above. The crate
has no unified `Graph` runtime object today — the API operates on a
loaded `(GraphIndexArtifact, CommitIndexArtifact)` pair.

```rust
pub enum Resolution<T> {
    /// Symbol found at target. `chain` is the sequence of intermediate
    /// SnapshotKeys (renames) traversed from anchor to target.
    Found { value: T, chain: Vec<SnapshotKey> },
    /// Symbol existed at anchor but was deleted before target.
    Deleted { last_seen: SnapshotKey },
    /// Multiple rename chains reach target; cannot disambiguate.
    Ambiguous { candidates: Vec<T> },
    /// Anchor unknown, ref not indexed, or other lookup failure.
    Unknown { reason: ResolutionFailure },
}

pub enum ResolutionFailure {
    AnchorCommitNotIndexed(GitSha),
    SymbolNotPresentAtAnchor,
    IndexCorrupt(String),
}

/// Resolve a symbol's effective identity as of `target`.
///
/// Because snapshots are emitted only for commits that touched the
/// symbol, `target` usually has no exact snapshot. The resolver:
///   1. Finds the latest snapshot for `symbol` (or any predecessor in
///      its rename chain) at-or-before `target` in the indexed DAG.
///   2. If `target` is the indexed tip, maps that snapshot to the live
///      current-state symbol (derived, not via a materialized edge).
///   3. Walks forward through `RenamedFrom` edges from the anchor
///      snapshot to the snapshot at-or-before `target`.
///
/// Returns `Resolution::Deleted{last_seen}` if the rename chain
/// terminates in a `Deleted` change before `target`.
pub fn resolve_symbol_at(
    code: &GraphIndexArtifact,
    commits: &CommitIndexArtifact,
    symbol: StableSymbolId,
    anchor: GitSha,
    target: GitSha,
) -> Resolution<StableSymbolId>;

/// Full causal trace of a symbol: every commit that touched it, in
/// topological order, with the ChangeKind and the snapshot key at that
/// commit. Follows RenamedFrom backward across renames; the trace can
/// therefore include multiple distinct stable_symbol_ids.
pub fn symbol_history(
    code: &GraphIndexArtifact,
    commits: &CommitIndexArtifact,
    symbol: StableSymbolId,
) -> Vec<(GitSha, ChangeKind, SnapshotKey)>;
```

Everything else (`commits_touching_file`, `files_touched_by_commit`,
etc.) is a trivial edge lookup — consumers iterate the artifacts
directly. We do not add named functions for these.

### Invariants

- A `SymbolSnapshot` has at most one inbound `RenamedFrom` edge.
  (Rename **splits** — one old → many new — are permitted, modeled as
  multiple outbound `RenamedFrom` edges from one predecessor. Rename
  **merges** — many old → one new — are forbidden; if multiple `Deleted`
  candidates would rename to the same new snapshot, the new snapshot
  is recorded as `Added` (no `RenamedFrom`), all candidates remain
  `Deleted`, and a `merge_collision` diagnostic is recorded on the
  commit node.)
  Violation at ingest = bug in the extractor; panic in debug, drop one
  edge + log + record diagnostic in release.
- `RenamedFrom` chains form a forest (set of trees), not a general graph.
  Cycles in `RenamedFrom` are forbidden and asserted.
- A symbol that has been `Deleted` and a later, identically-named symbol
  that is `Added` are two distinct `stable_symbol_id`s unless explicitly
  linked by a Tier 1 or Tier 2 rename match at the moment of re-addition.
- For every `Commit --touches--> SymbolSnapshot` edge, the snapshot's
  `commit` field equals the source commit's SHA. (Asserted at ingest.)

## Edge Cases

| Case | Behavior |
|---|---|
| Symbol re-introduced under same name after deletion | Two separate `stable_symbol_id`s. No `RenamedFrom` between them unless Tier 1/2 rename match fires. |
| File rename + symbol move within file in same commit | Rename detection runs on the renamed-pair, not just same-path pairs. |
| Generated files checked in | Excluded via existing `discovery.rs` ignore rules. |
| Squash/rebase rewriting old commits | We index `main` post-merge. If `main` is rewritten, force-push recovery applies. Pre-merge branch history is out of scope. |
| Commits with no source-file changes (docs-only, config-only) | `CommitNode` is still emitted. Zero `touches Symbol` edges. Becomes useful in Phase 2 when documents land. |
| Two PRs landing near-simultaneously with cross-references | Resolved by git's linear main history; whichever lands second sees the first. No special handling needed. |

## Phase 1 Standalone Consumer

Phase 1 does not merge until the following callers compile and are
exercised by an end-to-end test:

**Code graph MCP — temporal mode.** The existing code-graph MCP tools
in `crates/spur-mcp/src/worker_server.rs` (`code_subgraph`,
`code_callers`, `code_callees`) gain an optional `as_of: GitSha`
parameter. When set, the tool resolves the requested symbol via
`resolve_symbol_at` to its snapshot at-or-before `as_of`, then walks
edges using the subgraph as it existed at that commit.

**New tool: `code_symbol_history`.** A dedicated MCP tool returning
the full causal trace of a symbol — every commit that touched it,
the `ChangeKind`, and the snapshot key at that commit — across rename
chains.

The PM/beads graph tools (`graph_subgraph`, `graph_plan`,
`graph_triage`) are an unrelated namespace and are NOT modified in
Phase 1.

This pins Phase 1's value: an agent using the code-graph MCP tools can
ask "show me callers of `submit_plan` as it existed at commit X" or
"give me the history of `process_chunk`" and receive a structured
response built from materialized edges, not LLM archaeology.

The MCP wiring itself is implementation work tracked in Phase 1's plan;
the spec requires that the new API has at least one shipped caller
(temporal-mode `code_subgraph` OR `code_symbol_history`) before merge.

## Testing

`crates/spur-graph/tests/temporal_resolution.rs` (new) and
`crates/spur-graph/tests/rename_corpus.rs` (new).

### Functional tests

- **Scripted fixture repo** built at test time via shelling to `git`
  into a `tempfile::TempDir`. History covers: add, modify, delete,
  rename-file, rename-symbol-within-file, rename-file-and-symbol-
  together, squash-equivalent re-add, merge commit with conflicting
  per-parent diffs, force-push recovery, ref rename.
- **Property test 1**: for every `(symbol, anchor)` in the scripted
  history, `resolve_symbol_at(symbol, anchor, HEAD)` matches the
  expected `Resolution<T>` variant.
- **Property test 2**: `symbol_history(s).len()` equals the number of
  commits that touched `s` (or any of its rename ancestors) in the
  script.
- **Snapshot integrity**: every `Commit --touches--> SymbolSnapshot`
  edge resolves to a snapshot whose `commit` matches the source commit.
- **Force-push test**: build history, index, rewrite `main`, re-index,
  assert affected range invalidated and rebuilt.
- **Shallow-clone test**: clone with `--depth=1`, assert fail-closed.
- **Partial-clone test**: with a known missing blob, assert fail-closed
  with the missing oid named.

### Rename heuristic corpus

Per-language fixture suites under `tests/fixtures/rename_corpus/<lang>/`
with labeled ground truth (≥50 pairs per supported language at merge
time: Rust, TypeScript, Python). Each fixture is a tuple
`(old_blob, new_blob, expected_match: Option<symbol_name>)`.

Report precision, recall, and F1 in CI output. Regressions of >5%
F1 against the baseline are blocking.

### Bench

Extend `benches/incremental.rs`:

- 1k-commit synthetic repo, mostly linear.
- 20k-commit synthetic repo, ≥30% merge commits (exercises full-DAG
  walk).
- Report: full-walk wall time, peak RSS, total artifact size (graph +
  commit index), snapshot count, 100-commit incremental wall time.
- Numbers logged for tracking; >2x regression against a recorded
  baseline is blocking.
- **Snapshot-growth budget.** At 10× commits, total snapshot count
  should grow ≤ 1.5×. Faster growth signals super-linear edge density
  and triggers a follow-up to shard the commit-index artifact before
  Phase 1 ships. (One per touched-symbol-per-commit *should* scale
  near-linearly with churn; the budget guards against pathological
  rename-chain explosions.)

## Open Questions

None blocking implementation. Decisions taken during triple review:

- Rename detection: tiered algorithm (file-rename inheritance → token
  Jaccard → none) with confidence reporting. Locked.
- Default walk: full-DAG reachable from `main`, with `FirstParent`
  config knob. Locked.
- First consumer: code-graph MCP tools (`code_subgraph`, `code_callers`,
  `code_callees`) gain `as_of`; new `code_symbol_history` tool. Locked.
- Snapshot granularity: one per `(symbol, commit-that-touched-it)`,
  not one per `(symbol, every-commit)`. Locked.

## What This Spec Is Not

- Not a markdown / document layer. That is Phase 2.
- Not a vector store, embedding, or similarity search of any kind.
- Not a replacement for `git blame` at line granularity.
- Not a new public CLI surface. The internal Rust API is consumed by the
  existing code-graph MCP tools; no new CLI is added.
- Not multi-branch. Single target ref only (default `main`); other refs
  are addressable by configuration but not indexed concurrently.

## Phase 2 Preview (non-binding)

For continuity, the intended Phase 2 adds:
- `DocumentNode { path, kind, authored_at: GitSha, content_hash }`.
- `Document --mentions_at--> SymbolSnapshot` extending the existing
  `CodeMentionPayload` (`schema.rs`) with an `anchored_at: GitSha`
  field. Mentions resolve to the snapshot at the authoring commit,
  then forward through `RenamedFrom` edges to find the live symbol.
- `Document --supersedes--> Document` from frontmatter or path-pattern
  inference.

Phase 2 depends on Phase 1's `SymbolSnapshot` model and `RenamedFrom`
chains. Without snapshots, a mention "at the commit it was authored"
would have no node to point at.

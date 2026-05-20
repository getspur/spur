# Code as Memory — Phase 1: Code ↔ Git Commit Graph

**Date:** 2026-05-20
**Status:** Revised after triple review (codex / claude-code / gemini)
**Crate:** `crates/spur-graph`

## Revision Notes (2026-05-20, post-review)

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
- **Concrete first consumer named:** the graph MCP traversal tools
  (`graph_subgraph` / `graph_plan` family) gain a temporal-history mode
  that calls the new API. Phase 1 does not ship without this caller.
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
  `SymbolSnapshot --renamed_from--> SymbolSnapshot`,
  `Symbol --latest_snapshot--> SymbolSnapshot`.
- Git-walk extractor that materializes these for commits reachable from
  the target ref (default `main`), full-DAG by default.
- `extract_symbols_from_bytes` seam so historical blobs can be parsed
  without materializing temporary files.
- Separate `.spur/commit-index.pointer.json` for temporal checkpoints.
- Resolution API: `resolve_symbol_at` and `symbol_history` returning
  `Resolution<T>`.
- Comprehensive failure-mode handling (see Failure Modes table).
- **Concrete first consumer**: the graph MCP traversal tools gain a
  temporal-history mode that calls the new API (see "Phase 1 Standalone
  Consumer" below).

### Out of scope (deferred to Phase 2+)

- `DocumentNode`, `mentions_at` edges, supersession. **No markdown
  layer in Phase 1.**
- Branches other than `main`.
- Line-granular blame (we do symbol-granular only).
- A query language. Two named functions plus raw graph traversal.
- External API surface (MCP, CLI flags). Internal Rust API only.

## Data Model

Changes to `crates/spur-graph/src/schema.rs` are **not purely additive**:
existing `NodeKind`, `RelationKind`, and `GraphEdge` are extended. Current-
state graph artifacts written by today's binaries remain readable; the
schema version is bumped so older binaries fail closed on temporal-aware
artifacts (no silent downgrades).

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

### `GraphEdge` extension

Add an optional `change_kind: Option<ChangeKind>` field. Required when
`relation == Touches`, absent otherwise.

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
HEAD. `SymbolSnapshot` nodes coexist alongside it and are linked to the
live symbol by a `LatestSnapshot` edge when the snapshot is at the
indexed tip.

### Edges (full surface)

- `Commit --touches--> File { change_kind }` — file-level change.
- `Commit --touches--> SymbolSnapshot { change_kind }` — symbol-level
  change, with the snapshot capturing what the symbol looked like at
  that commit.
- `SymbolSnapshot --renamed_from--> SymbolSnapshot` — when the
  `change_kind` is `RenamedFrom`, this explicit edge makes traversal a
  single hop instead of having to decode `change_kind`.
- `Symbol --latest_snapshot--> SymbolSnapshot` — connects the current-
  state symbol to its most recent snapshot at the indexed tip.

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
temporary files. Introduce:

```rust
pub fn extract_symbols_from_bytes(
    language: Language,
    logical_path: &Path,    // for symbol IDs / scope; not for I/O
    bytes: &[u8],
) -> Result<Vec<ExtractedSymbol>, ExtractError>;
```

The filesystem extractors are refactored to call this helper internally.
Behavior is identical for current-state extraction; the new entry point
serves the historical walk.

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
   - Fetch the blob at `C` and the blob at the first relevant parent via
     `git cat-file blob`.
   - Parse each through `extract_symbols_from_bytes`.
   - Emit a `SymbolSnapshot` for every symbol on the `C` side. (This is
     where snapshots come from — one per touched symbol per commit.)
   - Match snapshots across sides:
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
     a threshold (default `0.7`, configurable; report in bench).
     Confidence: `Medium`.
   - **Tier 3 — none.** If multiple candidates score near the threshold,
     emit `Added` + `Deleted` and record an `ambiguous_rename` diagnostic
     on the commit node. Never emit a guessed `RenamedFrom` with low
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
| Partial clone / missing blob during walk (`git cat-file blob` returns NOENT) | Fail closed for the affected commit; do not emit partial symbol history. Surface a single actionable error naming the missing oid. |
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

/// Walk forward through `RenamedFrom` edges from (snapshot at anchor) to
/// the equivalent snapshot at `target`. If `target` is the indexed tip,
/// the result is the live current-state symbol via `LatestSnapshot`.
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

Phase 1 does not merge until the following caller compiles and is
exercised by an end-to-end test:

**Graph MCP traversal — temporal mode.** The existing graph MCP tool
family (`graph_subgraph`, `graph_plan`, `graph_triage`, etc. in
`spur-mcp`) gains an optional `as_of: GitSha` parameter and a new
`symbol_history` accessor. Both call directly into the resolution API.

This pins Phase 1's value: an agent using the MCP graph tools can ask
"show me the subgraph around `submit_plan` as it existed at commit X" or
"give me the history of `process_chunk`" and receive a structured
response built from materialized edges, not LLM archaeology.

The MCP wiring itself is implementation work tracked in Phase 1's plan;
the spec only requires that the new API has at least one shipped caller
before merge.

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
- Report: full-walk wall time, peak RSS, artifact size, 100-commit
  incremental wall time.
- Numbers logged for tracking; >2x regression against a recorded
  baseline is blocking.

## Open Questions

None blocking implementation. Decisions taken during triple review:

- Rename detection: tiered algorithm (file-rename inheritance → token
  Jaccard → none) with confidence reporting. Locked.
- Default walk: full-DAG reachable from `main`, with `FirstParent`
  config knob. Locked.
- First consumer: graph MCP traversal tools (`as_of` + `symbol_history`
  accessor). Locked.
- Snapshot granularity: one per `(symbol, commit-that-touched-it)`,
  not one per `(symbol, every-commit)`. Locked.

## What This Spec Is Not

- Not a markdown / document layer. That is Phase 2.
- Not a vector store, embedding, or similarity search of any kind.
- Not a replacement for `git blame` at line granularity.
- Not a new public CLI surface. The internal Rust API is consumed by the
  existing graph MCP tools; no new CLI is added.
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

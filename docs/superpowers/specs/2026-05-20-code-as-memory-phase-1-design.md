# Code as Memory — Phase 1: Code ↔ Git Commit Graph

**Date:** 2026-05-20
**Status:** Design approved, ready for implementation plan
**Crate:** `crates/spur-graph`

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

- New node kind: `CommitNode`.
- New edge kinds: `Commit --touches--> File`, `Commit --touches--> Symbol`,
  both carrying `ChangeKind { added | modified | deleted | renamed_from }`.
- Git-walk extractor that materializes these for commits on `main`.
- Incremental ingest keyed by last-indexed SHA per ref.
- Resolution API: `resolve_symbol_at(symbol, anchor, target)` and
  `symbol_history(symbol)`.
- Force-push detection (stored SHA no longer an ancestor of new tip),
  shallow-clone fail-closed.

### Out of scope (deferred to Phase 2+)

- `DocumentNode`, `mentions_at` edges, supersession. **No markdown
  layer in Phase 1.**
- Branches other than `main`.
- Line-granular blame (we do symbol-granular only).
- A query language. Two named functions plus raw graph traversal.
- External API surface (MCP, CLI flags). Internal Rust API only.

## Data Model

Additive changes to `crates/spur-graph/src/schema.rs`. Existing schema
unchanged.

### Nodes

```rust
pub struct CommitNode {
    pub sha: GitSha,            // 40-hex string, durable identity
    pub parents: Vec<GitSha>,   // 0 for root, 1 for linear, 2+ for merge
    pub author_time: i64,       // unix seconds, for ordering ties
    pub summary: String,        // first line of commit message
}
```

One `CommitNode` per commit reachable from `main` via first-parent walk
(see "Merge commits" below).

### Edges

```rust
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    /// For files: previous path. For symbols: previous SymbolId.
    RenamedFrom(RenamePrev),
}

pub enum RenamePrev {
    File(PathBuf),
    Symbol(SymbolId),
}
```

- `Commit --touches--> File { kind: ChangeKind }`
- `Commit --touches--> Symbol { kind: ChangeKind }`

A single commit can have many `touches` edges. A single symbol has at most
one inbound `RenamedFrom` edge per commit (asserted at ingest).

## Extractor (`crates/spur-graph/src/git_walk.rs`)

A new module sibling to the existing `git.rs`.

### Inputs

- Starting ref (default `main`).
- Stop condition (default: walk to root, or to last-indexed SHA on
  incremental runs).

### Per-commit algorithm

For commit `C` with first parent `C^`:

1. **File-level diff.** `git diff-tree -r --name-status --find-renames C^ C`.
   Emit `Commit --touches--> File` edges with `ChangeKind` derived from
   the status letter.
2. **Symbol-level diff.** For each touched source file (added, modified,
   or renamed):
   - Fetch both blobs via `git cat-file blob`.
   - Parse each with the existing `extract::*` tree-sitter extractors.
   - Match symbols across sides:
     - Same name + same enclosing scope → `Modified` (or skip if
       byte-equivalent).
     - Left-only → candidate `Deleted`.
     - Right-only → candidate `Added`.
   - **Rename detection (conservative).** Among unmatched
     `Added`/`Deleted` pairs in the same file (or across a renamed file
     pair), match by *AST shape similarity*: a normalized hash of the
     tree-sitter node ignoring identifiers. Threshold is tight — when in
     doubt, prefer emitting `Added` + `Deleted` over a wrong
     `RenamedFrom`. The threshold and the exact normalization are
     implementation details, not part of this spec; the spec only
     mandates conservatism.
3. **Emit** `Commit --touches--> Symbol` edges with the resulting
   `ChangeKind`.

### Initial commit

Diff against the empty tree. All files and symbols are `Added`.

### Merge commits

Walk first-parent only by default. Configurable via a `walk_strategy`
knob. Rationale: `main` is treated as a linear spine; second-parent
history is reachable separately if ever needed.

### Incremental ingest

Store last-indexed SHA per ref in the existing
`.spur/graph-index.pointer.json` file (extend its schema; the pointer
file already exists in-tree). On rerun, walk from new tip back to stored
SHA.

### Failure modes (must be handled inline, not silently)

| Condition | Behavior |
|---|---|
| Stored SHA not an ancestor of new tip (force-push) | Invalidate affected range, re-walk from divergence point. Log loudly. |
| Shallow repository (`git rev-parse --is-shallow-repository` = true) | Fail closed with a clear error. Symbol-history would be silently truncated otherwise. |
| Source file is unparseable by tree-sitter on either side | Emit file-level `touches` edge, skip symbol-level diff for that file, log at `warn`. |
| Source file extension not covered by any extractor | File-level edge only. No symbol-level diff. No log. |

## Resolution API (`crates/spur-graph/src/temporal.rs`)

New module. Pure graph traversals over the edges built above.

```rust
/// Walk forward through `RenamedFrom` edges from a (symbol, commit)
/// anchor to the equivalent symbol at the target commit (typically HEAD).
/// Returns None if the symbol was deleted before the target and not
/// later re-added under a rename chain.
pub fn resolve_symbol_at(
    graph: &Graph,
    symbol: SymbolId,
    anchor: GitSha,
    target: GitSha,
) -> Option<SymbolId>;

/// Full causal trace of a symbol: every commit that touched it, in
/// topological order, with the ChangeKind on each step. Follows
/// RenamedFrom backward to give a complete history across renames.
pub fn symbol_history(
    graph: &Graph,
    symbol: SymbolId,
) -> Vec<(GitSha, ChangeKind, SymbolId)>;
```

Everything else (`commits_touching_file`, `files_touched_by_commit`, etc.)
is a trivial edge lookup — consumers iterate the graph directly. We do
not add named functions for these.

### Invariants

- A symbol has at most one `RenamedFrom` predecessor per commit. Violation
  at ingest = bug in the extractor; panic in debug, log + drop one edge
  in release.
- `RenamedFrom` chains form a tree, not a graph. (No symbol has two
  distinct predecessors at the same commit.)
- A symbol that has been `Deleted` and a later, identically-named symbol
  that is `Added` are *two distinct SymbolIds* unless explicitly linked by
  a rename heuristic at the moment of re-addition.

## Edge Cases

| Case | Behavior |
|---|---|
| Symbol re-introduced under same name after deletion | Two separate `SymbolId`s. No `RenamedFrom` between them. |
| File rename + symbol move within file in same commit | Rename detection runs on the renamed-pair, not just same-path pairs. |
| Generated files checked in | Excluded via existing `discovery.rs` ignore rules. |
| Squash/rebase rewriting old commits | We index `main` post-merge. If `main` is rewritten, force-push recovery applies. Pre-merge branch history is out of scope. |
| Commits with no source-file changes (docs-only, config-only) | `CommitNode` is still emitted. Zero `touches Symbol` edges. Becomes useful in Phase 2 when documents land. |
| Two PRs landing near-simultaneously with cross-references | Resolved by git's linear main history; whichever lands second sees the first. No special handling needed. |

## Testing

`crates/spur-graph/tests/temporal_resolution.rs` (new).

- **Scripted fixture repo** built at test time via shelling to `git` into
  a `tempfile::TempDir`. History covers: add, modify, delete,
  rename-file, rename-symbol-within-file, rename-file-and-symbol-together,
  squash-equivalent re-add, merge commit.
- **Property test 1**: for every `(symbol, anchor)` in the scripted
  history, `resolve_symbol_at(symbol, anchor, HEAD)` returns the expected
  current identity (or `None` for deletions).
- **Property test 2**: `symbol_history(s).len() ==
  number_of_commits_that_touched_s_in_script`.
- **Force-push test**: build a history, index it, rewrite `main`,
  re-index, assert the affected range was invalidated and rebuilt.
- **Shallow-clone test**: clone with `--depth=1`, assert fail-closed
  error.
- **Bench**: extend `benches/incremental.rs` with a 1k-commit synthetic
  repo. Full walk and 10-commit incremental walk. Numbers are advisory,
  not contractual.

## Open Questions

None blocking implementation. The two questions raised in brainstorming
(rename heuristic vs file-only renames; first-parent-only on merges)
are both locked to the conservative default in this spec, with a
config knob for the latter.

## What This Spec Is Not

- Not a markdown / document layer. That is Phase 2.
- Not a vector store, embedding, or similarity search of any kind.
- Not a replacement for `git blame` at line granularity.
- Not a public API. Internal Rust surface only; callers (brain, worker,
  indexer CLI) are out of scope.

## Phase 2 Preview (non-binding)

For continuity, the intended Phase 2 adds:
- `DocumentNode { path, kind, authored_at, content_hash }`.
- `Document --mentions_at--> {File, Symbol}` extending the existing
  `CodeMentionPayload` with an `anchored_at: GitSha` field.
- `Document --supersedes--> Document` from frontmatter or path-pattern
  inference.

Phase 2 depends on Phase 1's symbol identity across renames, because a
document's mention "at the commit it was authored" only resolves to a
live symbol if rename chains exist.

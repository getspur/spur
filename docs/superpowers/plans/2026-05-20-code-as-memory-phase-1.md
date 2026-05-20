# Code-as-Memory Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fuse git history into `crates/spur-graph` so symbol-level memory reconstruction across time is a graph traversal, not LLM archaeology.

**Architecture:** Three layers landing in dependency order — (1) schema/type additions for temporal facts; (2) a git-walk extractor that materializes commit nodes, `SymbolSnapshot` nodes, and `touches`/`renamed_from` edges with conservative rename detection; (3) a resolution API plus MCP `as_of` plumbing so the existing `code_subgraph` / `code_callers` / `code_callees` tools (and a new `code_symbol_history`) can answer point-in-time queries.

**Tech Stack:** Rust workspace (`spur-graph`, `spur-mcp`), `tree-sitter` (existing per-language crates), `serde_json` for artifact persistence, `criterion` for benches, `tempfile` + shell-out `git` for test fixtures.

**Spec:** `docs/superpowers/specs/2026-05-20-code-as-memory-phase-1-design.md` is the source of truth. When this plan and the spec disagree, the spec wins; flag the discrepancy in the PR.

**Branch:** Implement on a worktree branch off `main` HEAD `6a956aed`. Frequent commits per task.

---

## File map

**Modify:**
- `crates/spur-graph/Cargo.toml` — add `chrono` (if not present) for RFC3339 timestamps; `walkdir`, `regex` only if not already there.
- `crates/spur-graph/src/schema.rs` — extend `NodeKind`, `RelationKind`, `GraphEdge`, `GraphEdgeArtifact`, `GraphIndexArtifact`; add `ChangeKind`, `EdgeEndpoint`, `TemporalEdgeArtifact`, `CommitArtifact`, `SymbolSnapshotArtifact`, `SnapshotKey`, `CommitIndexArtifact`, `WalkStrategy`.
- `crates/spur-graph/src/lib.rs` — `pub mod git_walk;` and `pub mod temporal;`.
- `crates/spur-graph/src/store/cache.rs` — add `COMMIT_INDEX_POINTER_PATH = ".spur/commit-index.pointer.json"`; reader/writer for the new pointer file.
- `crates/spur-graph/src/extract/mod.rs` — refactor existing `build_facts` to drive new `BytesExtractor`.
- `crates/spur-graph/src/extract/tree_sitter.rs` — introduce `BytesExtractor`.
- `crates/spur-graph/benches/incremental.rs` — add 1k/20k commit benches + snapshot-growth reporting.
- `crates/spur-mcp/src/worker_server.rs` — add `as_of: Option<String>` to `CodeSymbolParams` / `CodeSubgraphParams`; new `code_symbol_history` tool.
- `crates/spur-mcp/src/server/handlers/code_graph.rs` — thread `as_of` into the handlers; new `code_symbol_history` handler.

**Create:**
- `crates/spur-graph/src/git_walk.rs` — git-walk extractor.
- `crates/spur-graph/src/temporal.rs` — resolution API.
- `crates/spur-graph/tests/temporal_resolution.rs` — scripted-fixture functional tests.
- `crates/spur-graph/tests/rename_corpus.rs` — per-language rename corpus harness.
- `crates/spur-graph/tests/fixtures/rename_corpus/rust/*.in`, `*.out`, `expected.json` — Rust corpus.
- `crates/spur-graph/tests/fixtures/rename_corpus/typescript/...`, `.../python/...` — TS + Python corpora.

---

## Task 1: Extend schema with `ChangeKind` and `change_kind` field on edges

**Files:**
- Modify: `crates/spur-graph/src/schema.rs`
- Test: `crates/spur-graph/src/schema.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing test**

Add to the existing tests module in `schema.rs` (or create one at the bottom of the file):

```rust
#[cfg(test)]
mod change_kind_tests {
    use super::*;

    #[test]
    fn change_kind_round_trips_json() {
        let added = ChangeKind::Added;
        let s = serde_json::to_string(&added).unwrap();
        assert_eq!(s, "\"added\"");
        let back: ChangeKind = serde_json::from_str(&s).unwrap();
        assert_eq!(back, ChangeKind::Added);

        let renamed = ChangeKind::RenamedFrom(RenamePrev::File("src/old.rs".into()));
        let s = serde_json::to_string(&renamed).unwrap();
        let back: ChangeKind = serde_json::from_str(&s).unwrap();
        assert_eq!(back, renamed);
    }

    #[test]
    fn node_kind_has_commit_variant() {
        let k = NodeKind::Commit;
        let s = serde_json::to_string(&k).unwrap();
        assert_eq!(s, "\"commit\"");
    }

    #[test]
    fn relation_kind_has_touches_variant() {
        let r = RelationKind::Touches;
        let s = serde_json::to_string(&r).unwrap();
        assert_eq!(s, "\"touches\"");
    }

    #[test]
    fn graph_edge_artifact_change_kind_is_optional() {
        let json = r#"{
            "source_stable_symbol_id":"a",
            "target_stable_symbol_id":"b",
            "target_label":null,
            "relation":"calls",
            "confidence":"syntax_exact",
            "confidence_score":1.0
        }"#;
        let e: GraphEdgeArtifact = serde_json::from_str(json).unwrap();
        assert!(e.change_kind.is_none());
    }
}
```

- [ ] **Step 2: Run test, confirm it fails**

```
cargo test -p spur-graph --lib change_kind_tests
```

Expected: compile errors — `ChangeKind`, `RenamePrev`, `NodeKind::Commit`, `RelationKind::Touches`, and `GraphEdgeArtifact.change_kind` don't exist.

- [ ] **Step 3: Add `ChangeKind`, `RenamePrev`, `StableSymbolId` to `schema.rs`**

Place these immediately after the existing `Confidence` enum (around line 268). Note `StableSymbolId` is a type alias used throughout the temporal layer:

```rust
pub type StableSymbolId = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    RenamedFrom(RenamePrev),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "endpoint", rename_all = "snake_case")]
pub enum RenamePrev {
    File(std::path::PathBuf),
    Symbol(SnapshotKey),
}

// SnapshotKey is defined in Task 2 — for Task 1 use a forward-declared
// placeholder by adding `pub use crate::schema::SnapshotKey;` after Task 2
// lands. For now define it locally:
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotKey {
    pub stable_symbol_id: String,
    pub commit: String,
}
```

- [ ] **Step 4: Add `Commit` variant to `NodeKind`**

Modify `NodeKind` (line 207); add at the end:

```rust
pub enum NodeKind {
    // ... existing variants ...
    Section,
    Commit,
}
```

Also extend `NodeKind::discriminator` (around line 226) with:

```rust
NodeKind::Commit => "commit",
```

- [ ] **Step 5: Add `Touches` variant to `RelationKind`**

Modify line 249:

```rust
pub enum RelationKind {
    // ... existing variants ...
    Links,
    Touches,
}
```

- [ ] **Step 6: Add `change_kind` to `GraphEdge` and `GraphEdgeArtifact`**

In `GraphEdgeArtifact` (line 159):

```rust
pub struct GraphEdgeArtifact {
    pub source_stable_symbol_id: String,
    pub target_stable_symbol_id: Option<String>,
    pub target_label: Option<String>,
    pub relation: RelationKind,
    pub confidence: Confidence,
    pub confidence_score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_kind: Option<ChangeKind>,
}
```

In `GraphEdge` (line 182):

```rust
pub struct GraphEdge {
    pub edge_id: EdgeId,
    pub source_node_id: NodeId,
    pub target_node_id: Option<NodeId>,
    pub relation: RelationKind,
    pub target_label: Option<String>,
    pub confidence: Confidence,
    pub confidence_score: f32,
    pub evidence_id: EvidenceId,
    pub directed: bool,
    pub change_kind: Option<ChangeKind>,
}
```

Any existing constructors of `GraphEdge` in the crate must default `change_kind: None`. Run `cargo check -p spur-graph` and fix call sites until clean.

- [ ] **Step 7: Run tests, confirm they pass**

```
cargo test -p spur-graph --lib change_kind_tests
```

Expected: 4 passed.

- [ ] **Step 8: Commit**

```
git add crates/spur-graph/src/schema.rs
git commit -m "feat(spur-graph): add ChangeKind, NodeKind::Commit, RelationKind::Touches"
```

---

## Task 2: New artifact types — `SymbolSnapshot`, `EdgeEndpoint`, `TemporalEdgeArtifact`, `CommitArtifact`

**Files:**
- Modify: `crates/spur-graph/src/schema.rs`
- Test: inline tests in `schema.rs`

- [ ] **Step 1: Write the failing test**

Add to `schema.rs`:

```rust
#[cfg(test)]
mod temporal_artifact_tests {
    use super::*;

    #[test]
    fn symbol_snapshot_round_trips() {
        let s = SymbolSnapshotArtifact {
            key: SnapshotKey {
                stable_symbol_id: "graph://symbol/foo".to_string(),
                commit: "abc123".to_string(),
            },
            file_path: "src/lib.rs".into(),
            entity_name: "foo".to_string(),
            symbol_kind: "function".to_string(),
            enclosing_scope: None,
            byte_range: [0, 42],
            line_range: [1, 5],
            anchor_hash: "deadbeef".to_string(),
        };
        let j = serde_json::to_string(&s).unwrap();
        let back: SymbolSnapshotArtifact = serde_json::from_str(&j).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn edge_endpoint_serializes_tagged() {
        let e = EdgeEndpoint::Snapshot {
            key: SnapshotKey {
                stable_symbol_id: "x".into(),
                commit: "y".into(),
            },
        };
        let j = serde_json::to_string(&e).unwrap();
        assert!(j.contains("\"endpoint\":\"snapshot\""));
        let back: EdgeEndpoint = serde_json::from_str(&j).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn graph_index_artifact_temporal_fields_default_empty() {
        let json = r#"{
            "header":{"graph_index_version":"1"},
            "files":[],
            "symbols":[]
        }"#;
        let a: GraphIndexArtifact = serde_json::from_str(json).unwrap();
        assert!(a.commits.is_empty());
        assert!(a.symbol_snapshots.is_empty());
        assert!(a.temporal_edges.is_empty());
    }
}
```

- [ ] **Step 2: Run test, confirm it fails (types not defined)**

```
cargo test -p spur-graph --lib temporal_artifact_tests
```

- [ ] **Step 3: Add `SymbolSnapshotArtifact`, `CommitArtifact`, `EdgeEndpoint`, `TemporalEdgeArtifact` in `schema.rs`**

Place after the new `ChangeKind` block from Task 1:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolSnapshotArtifact {
    pub key: SnapshotKey,
    pub file_path: std::path::PathBuf,
    pub entity_name: String,
    pub symbol_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enclosing_scope: Option<String>,
    pub byte_range: SourceRange,
    pub line_range: SourceRange,
    pub anchor_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitArtifact {
    pub sha: String,
    pub parents: Vec<String>,
    pub author_time: i64,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "endpoint", rename_all = "snake_case")]
pub enum EdgeEndpoint {
    File { path: std::path::PathBuf },
    Symbol { stable_symbol_id: String },
    Snapshot { key: SnapshotKey },
    Commit { sha: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemporalEdgeArtifact {
    pub source: EdgeEndpoint,
    pub target: EdgeEndpoint,
    pub relation: RelationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_kind: Option<ChangeKind>,
}
```

- [ ] **Step 4: Extend `GraphIndexArtifact` with temporal collections**

Modify around line 75:

```rust
pub struct GraphIndexArtifact {
    pub header: GraphIndexHeader,
    #[serde(default)]
    pub manifest_version: String,
    #[serde(default)]
    pub graph_content_hash: String,
    #[serde(default)]
    pub file_manifests: Vec<GraphFileManifestEntry>,
    pub files: Vec<GraphFileArtifact>,
    pub symbols: Vec<GraphSymbolArtifact>,
    #[serde(default)]
    pub edges: Vec<GraphEdgeArtifact>,
    #[serde(default)]
    pub tombstones: Vec<GraphTombstoneEntry>,
    #[serde(default, skip)]
    pub diagnostics: Vec<String>,
    // Temporal layer (Phase 1)
    #[serde(default)]
    pub commits: Vec<CommitArtifact>,
    #[serde(default)]
    pub symbol_snapshots: Vec<SymbolSnapshotArtifact>,
    #[serde(default)]
    pub temporal_edges: Vec<TemporalEdgeArtifact>,
}
```

- [ ] **Step 5: Bump schema version**

In `GraphIndexHeader` default / writer logic (find via `rg "graph_index_version" crates/spur-graph/src/`), bump from current version to `"2"` (or current+1). Add a constant near the top of `schema.rs`:

```rust
pub const GRAPH_INDEX_VERSION_TEMPORAL: &str = "2";
```

Older binaries reading a v2 artifact must fail closed; if `load_artifact` does not already version-check, add a check after `serde_json::from_str`:

```rust
if artifact.header.graph_index_version != GRAPH_INDEX_VERSION_TEMPORAL
    && artifact.header.graph_index_version != "1"
{
    anyhow::bail!(
        "unsupported graph_index_version `{}`",
        artifact.header.graph_index_version
    );
}
```

- [ ] **Step 6: Run tests, confirm they pass**

```
cargo test -p spur-graph --lib temporal_artifact_tests
cargo test -p spur-graph --lib  # also confirm nothing else broke
```

- [ ] **Step 7: Commit**

```
git add crates/spur-graph/src/schema.rs
git commit -m "feat(spur-graph): add SymbolSnapshot/EdgeEndpoint/TemporalEdge artifact types"
```

---

## Task 3: `CommitIndexArtifact` + pointer file IO

**Files:**
- Modify: `crates/spur-graph/src/schema.rs` (add types)
- Modify: `crates/spur-graph/src/store/cache.rs` (pointer constant + read/write)
- Create: `crates/spur-graph/src/store/commit_index.rs` (artifact load/save)
- Modify: `crates/spur-graph/src/store/mod.rs` (re-export)
- Test: inline tests in the new file

- [ ] **Step 1: Write the failing test**

Create `crates/spur-graph/src/store/commit_index.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{CommitArtifact, WalkStrategy};
    use tempfile::TempDir;

    #[test]
    fn pointer_round_trips() {
        let dir = TempDir::new().unwrap();
        let pointer = CommitIndexPointer {
            schema_version: 1,
            artifact_relative_path: "commits/2026-05-20.json".to_string(),
            indexed_at: "2026-05-20T12:00:00Z".to_string(),
            refs: [("main".to_string(), "abc123".to_string())].into(),
        };
        save_pointer(dir.path(), &pointer).unwrap();
        let loaded = load_pointer(dir.path()).unwrap();
        assert_eq!(loaded, Some(pointer));
    }

    #[test]
    fn missing_pointer_returns_none() {
        let dir = TempDir::new().unwrap();
        assert_eq!(load_pointer(dir.path()).unwrap(), None);
    }

    #[test]
    fn artifact_round_trips() {
        let a = CommitIndexArtifact {
            schema_version: 1,
            commits: vec![CommitArtifact {
                sha: "abc".into(),
                parents: vec![],
                author_time: 0,
                summary: "init".into(),
            }],
            refs: [("main".into(), "abc".into())].into(),
            indexed_at: "2026-05-20T12:00:00Z".into(),
            walk_strategy: WalkStrategy::Reachable,
        };
        let s = serde_json::to_string(&a).unwrap();
        let back: CommitIndexArtifact = serde_json::from_str(&s).unwrap();
        assert_eq!(a, back);
    }
}
```

- [ ] **Step 2: Add `CommitIndexArtifact` + `WalkStrategy` to `schema.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WalkStrategy {
    Reachable,
    FirstParent,
}

impl Default for WalkStrategy {
    fn default() -> Self {
        WalkStrategy::Reachable
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitIndexArtifact {
    pub schema_version: u32,
    pub commits: Vec<CommitArtifact>,
    pub refs: std::collections::BTreeMap<String, String>,
    pub indexed_at: String,
    #[serde(default)]
    pub walk_strategy: WalkStrategy,
}
```

- [ ] **Step 3: Add pointer constant + pointer module**

In `crates/spur-graph/src/store/cache.rs`, alongside the existing `POINTER_PATH`:

```rust
pub const COMMIT_INDEX_POINTER_PATH: &str = ".spur/commit-index.pointer.json";
```

Then create `crates/spur-graph/src/store/commit_index.rs`:

```rust
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::schema::CommitIndexArtifact;
use crate::store::cache::COMMIT_INDEX_POINTER_PATH;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitIndexPointer {
    pub schema_version: u32,
    pub artifact_relative_path: String,
    pub indexed_at: String,
    #[serde(default)]
    pub refs: BTreeMap<String, String>,
}

pub fn load_pointer(worktree: &Path) -> Result<Option<CommitIndexPointer>> {
    let path = worktree.join(COMMIT_INDEX_POINTER_PATH);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read commit-index pointer at {}", path.display()))?;
    Ok(Some(serde_json::from_str(&text)?))
}

pub fn save_pointer(worktree: &Path, pointer: &CommitIndexPointer) -> Result<()> {
    let path = worktree.join(COMMIT_INDEX_POINTER_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(pointer)?;
    fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn load_artifact(worktree: &Path, pointer: &CommitIndexPointer) -> Result<CommitIndexArtifact> {
    let path = worktree.join(&pointer.artifact_relative_path);
    let text = fs::read_to_string(&path)
        .with_context(|| format!("read commit index artifact at {}", path.display()))?;
    Ok(serde_json::from_str(&text)?)
}

pub fn save_artifact(worktree: &Path, relative: &str, artifact: &CommitIndexArtifact) -> Result<()> {
    let path = worktree.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(artifact)?)?;
    Ok(())
}
```

- [ ] **Step 4: Re-export from `store/mod.rs`**

```rust
pub mod commit_index;
```

- [ ] **Step 5: Run tests**

```
cargo test -p spur-graph store::commit_index::tests
```

Expected: 3 passed.

- [ ] **Step 6: Commit**

```
git add crates/spur-graph/src/schema.rs crates/spur-graph/src/store/
git commit -m "feat(spur-graph): commit-index artifact + .spur/commit-index.pointer.json IO"
```

---

## Task 4: `BytesExtractor` seam — parse blobs without filesystem I/O

**Files:**
- Modify: `crates/spur-graph/src/extract/tree_sitter.rs`
- Modify: `crates/spur-graph/src/extract/mod.rs` (refactor `build_facts` to use it)
- Test: inline tests + a small integration test in `tests/bytes_extractor.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/spur-graph/tests/bytes_extractor.rs`:

```rust
use spur_graph::extract::tree_sitter::{BytesExtractor, ExtractError};
use spur_graph::extract::languages::Language;
use std::path::Path;

#[test]
fn extracts_rust_function_from_bytes() {
    let mut ex = BytesExtractor::for_language(Language::Rust).unwrap();
    let bytes = b"pub fn hello() -> i32 { 42 }\n";
    let symbols = ex
        .extract(Path::new("src/lib.rs"), bytes)
        .expect("extract");
    assert_eq!(symbols.len(), 1);
    let s = &symbols[0];
    assert_eq!(s.entity_name, "hello");
    assert_eq!(s.symbol_kind, "function");
}

#[test]
fn reusing_extractor_across_blobs_works() {
    let mut ex = BytesExtractor::for_language(Language::Rust).unwrap();
    let a = ex.extract(Path::new("a.rs"), b"fn a() {}\n").unwrap();
    let b = ex.extract(Path::new("b.rs"), b"fn b() {}\n").unwrap();
    assert_eq!(a[0].entity_name, "a");
    assert_eq!(b[0].entity_name, "b");
}

#[test]
fn invalid_utf8_returns_error_not_panic() {
    let mut ex = BytesExtractor::for_language(Language::Rust).unwrap();
    let bytes = &[0xff, 0xfe, 0xfd];
    let r = ex.extract(Path::new("x.rs"), bytes);
    assert!(matches!(r, Err(ExtractError::InvalidUtf8(_))));
}
```

- [ ] **Step 2: Run test, confirm compile failure**

```
cargo test -p spur-graph --test bytes_extractor
```

- [ ] **Step 3: Implement `BytesExtractor`**

In `crates/spur-graph/src/extract/tree_sitter.rs`, define:

```rust
use crate::extract::languages::Language;
use std::path::Path;
use thiserror::Error;
use tree_sitter::{Parser, Query, QueryCursor};

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("invalid utf-8: {0}")]
    InvalidUtf8(std::str::Utf8Error),
    #[error("parser setup failed: {0}")]
    Setup(String),
    #[error("tree-sitter parse returned no tree")]
    NoTree,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSymbol {
    pub entity_name: String,
    pub symbol_kind: String,
    pub enclosing_scope: Option<String>,
    pub byte_range: [usize; 2],
    pub line_range: [usize; 2],
    pub anchor_hash: String, // blake3 over the symbol bytes
}

pub struct BytesExtractor {
    language: Language,
    parser: Parser,
    queries: CompiledQueries,
}

struct CompiledQueries {
    symbols: Query,
    // additional queries (calls, etc.) — preserve whatever the current
    // filesystem extractor compiles.
}

impl BytesExtractor {
    pub fn for_language(language: Language) -> Result<Self, ExtractError> {
        let mut parser = Parser::new();
        let ts_lang = language.tree_sitter_language();
        parser
            .set_language(&ts_lang)
            .map_err(|e| ExtractError::Setup(e.to_string()))?;
        let queries = CompiledQueries::for_language(language)?;
        Ok(Self { language, parser, queries })
    }

    pub fn extract(
        &mut self,
        logical_path: &Path,
        bytes: &[u8],
    ) -> Result<Vec<ExtractedSymbol>, ExtractError> {
        let text = std::str::from_utf8(bytes).map_err(ExtractError::InvalidUtf8)?;
        let tree = self.parser.parse(text, None).ok_or(ExtractError::NoTree)?;
        let root = tree.root_node();
        let mut cursor = QueryCursor::new();
        let mut out = Vec::new();
        for m in cursor.matches(&self.queries.symbols, root, text.as_bytes()) {
            // Reuse whatever symbol-record-building code the filesystem
            // extractor uses. The goal is parity: same inputs -> same
            // ExtractedSymbol records, except sourced from bytes instead
            // of a file path.
            if let Some(sym) = build_extracted_symbol(self.language, logical_path, text, &m) {
                out.push(sym);
            }
        }
        Ok(out)
    }
}

fn build_extracted_symbol(
    _lang: Language,
    _logical_path: &Path,
    _text: &str,
    _m: &tree_sitter::QueryMatch,
) -> Option<ExtractedSymbol> {
    // Implementation note for the engineer: the existing filesystem
    // extractor in this module builds a `GraphFact` per symbol. Extract
    // the inner symbol-building logic into this helper so both call sites
    // use it. Computing anchor_hash via `blake3::hash(symbol_bytes)` should
    // mirror identity.rs.
    None
}

impl CompiledQueries {
    fn for_language(language: Language) -> Result<Self, ExtractError> {
        let ts_lang = language.tree_sitter_language();
        // Mirror the path-loading the existing filesystem extractor uses:
        //   crates/spur-graph/queries/{rust,typescript,python,markdown}/symbols.scm
        // Use `include_str!` with a literal path per match arm (no dynamic
        // path — `include_str!` requires a literal).
        let query_source = match language {
            Language::Rust       => include_str!("../../queries/rust/symbols.scm"),
            Language::TypeScript => include_str!("../../queries/typescript/symbols.scm"),
            Language::Python     => include_str!("../../queries/python/symbols.scm"),
            Language::Markdown   => include_str!("../../queries/markdown/symbols.scm"),
        };
        let symbols = Query::new(&ts_lang, query_source)
            .map_err(|e| ExtractError::Setup(e.to_string()))?;
        Ok(Self { symbols })
    }
}
```

The crate already loads query files from `crates/spur-graph/queries/<lang>/`. Mirror the path-loading logic from the existing filesystem extractor so `CompiledQueries::for_language` produces the same queries. **No new query files** — reuse what's there.

- [ ] **Step 4: Refactor `build_facts` to drive `BytesExtractor`**

In `extract/mod.rs::build_facts`, after reading a file's bytes, call:

```rust
let mut extractor = BytesExtractor::for_language(language)?;
let symbols = extractor.extract(&relative_path, &bytes)?;
```

instead of the current path-based extraction. Behavior should be byte-identical for current-state runs; the test suite catches regressions.

- [ ] **Step 5: Run all extractor tests**

```
cargo test -p spur-graph extract
cargo test -p spur-graph --test bytes_extractor
```

Both must pass. **If existing extract tests fail, the refactor is wrong** — debug before continuing.

- [ ] **Step 6: Commit**

```
git add crates/spur-graph/src/extract/
git add crates/spur-graph/tests/bytes_extractor.rs
git commit -m "feat(spur-graph): BytesExtractor seam; refactor filesystem extractor to use it"
```

---

## Task 5: `git_walk` skeleton — config + ref snapshot + walk dispatch

**Files:**
- Create: `crates/spur-graph/src/git_walk.rs`
- Modify: `crates/spur-graph/src/lib.rs` (add `pub mod git_walk;`)
- Test: inline tests inside `git_walk.rs`

- [ ] **Step 1: Write the failing test**

In `git_walk.rs` (the file you'll create in Step 3):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo(dir: &std::path::Path) {
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "T"],
        ] {
            Command::new("git").current_dir(dir).args(args).status().unwrap();
        }
    }

    fn commit(dir: &std::path::Path, msg: &str) -> String {
        Command::new("git").current_dir(dir).args(["add", "-A"]).status().unwrap();
        Command::new("git")
            .current_dir(dir)
            .args(["commit", "-q", "--allow-empty", "-m", msg])
            .status()
            .unwrap();
        let out = Command::new("git")
            .current_dir(dir)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    #[test]
    fn snapshot_refs_returns_main_tip() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        let sha = commit(dir.path(), "init");
        let snap = snapshot_refs(dir.path(), &["main"]).unwrap();
        assert_eq!(snap.get("main"), Some(&sha));
    }

    #[test]
    fn fail_closed_on_shallow_clone() {
        // Setup: simulate shallow clone by adding .git/shallow.
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        commit(dir.path(), "init");
        std::fs::write(dir.path().join(".git/shallow"), "deadbeef\n").unwrap();
        let r = ensure_not_shallow(dir.path());
        assert!(r.is_err(), "shallow repo must fail-closed");
    }

    #[test]
    fn fail_closed_on_missing_target_ref() {
        let dir = TempDir::new().unwrap();
        init_repo(dir.path());
        // No commits; HEAD ref unborn.
        let r = snapshot_refs(dir.path(), &["main"]);
        assert!(r.is_err());
    }
}
```

- [ ] **Step 2: Run test, confirm compile failure**

```
cargo test -p spur-graph git_walk::tests
```

- [ ] **Step 3: Implement skeleton in `git_walk.rs`**

```rust
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

use crate::schema::WalkStrategy;

#[derive(Debug, Clone)]
pub struct GitWalkConfig {
    pub target_refs: Vec<String>,
    pub walk_strategy: WalkStrategy,
    pub allow_replace_refs: bool,
}

impl Default for GitWalkConfig {
    fn default() -> Self {
        Self {
            target_refs: vec!["main".to_string()],
            walk_strategy: WalkStrategy::Reachable,
            allow_replace_refs: false,
        }
    }
}

pub fn snapshot_refs(
    worktree: &Path,
    refs: &[&str],
) -> Result<BTreeMap<String, String>> {
    ensure_not_shallow(worktree)?;
    let mut out = BTreeMap::new();
    for r in refs {
        let stdout = run_git(worktree, &["rev-parse", "--verify", &format!("refs/heads/{r}")])
            .with_context(|| format!("target ref `{r}` does not exist; refusing to fall back"))?;
        out.insert((*r).to_string(), stdout.trim().to_string());
    }
    Ok(out)
}

pub fn ensure_not_shallow(worktree: &Path) -> Result<()> {
    let stdout = run_git(worktree, &["rev-parse", "--is-shallow-repository"])?;
    if stdout.trim() == "true" {
        bail!(
            "spur-graph: refusing to index shallow clone at `{}` — symbol \
             history would be silently truncated. Run `git fetch --unshallow` first.",
            worktree.display()
        );
    }
    Ok(())
}

pub fn check_replace_refs(worktree: &Path, allow: bool) -> Result<()> {
    let stdout = run_git(worktree, &["config", "--get-all", "replace.*"]).unwrap_or_default();
    let grafts_path = worktree.join(".git/info/grafts");
    if (!stdout.trim().is_empty() || grafts_path.exists()) && !allow {
        bail!(
            "spur-graph: git replace refs or grafts detected; refusing to walk. \
             Set GitWalkConfig.allow_replace_refs = true to override."
        );
    }
    Ok(())
}

pub(crate) fn run_git(worktree: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .current_dir(worktree)
        .args(args)
        .output()
        .with_context(|| format!("spawn git {args:?}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}
```

- [ ] **Step 4: Add `pub mod git_walk;` to `lib.rs`**

Order it after the existing modules.

- [ ] **Step 5: Run tests**

```
cargo test -p spur-graph git_walk::tests
```

Expected: 3 passed.

- [ ] **Step 6: Commit**

```
git add crates/spur-graph/src/git_walk.rs crates/spur-graph/src/lib.rs
git commit -m "feat(spur-graph): git_walk skeleton — ref snapshot + shallow/replace guards"
```

---

## Task 6: `git_walk` — per-commit file-level diff

**Files:**
- Modify: `crates/spur-graph/src/git_walk.rs`
- Test: inline

- [ ] **Step 1: Write the failing test**

Add to `git_walk.rs` tests:

```rust
#[test]
fn file_diff_initial_commit_marks_all_added() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("a.rs"), b"fn a() {}").unwrap();
    std::fs::write(dir.path().join("b.txt"), b"hi").unwrap();
    let sha = commit(dir.path(), "init");
    let changes = file_changes_for_commit(dir.path(), &sha).unwrap();
    let mut paths: Vec<_> = changes.iter().map(|c| (&c.path, &c.kind)).collect();
    paths.sort_by_key(|(p, _)| p.to_string_lossy().to_string());
    assert_eq!(paths.len(), 2);
    assert!(matches!(paths[0].1, FileChangeKind::Added));
}

#[test]
fn file_diff_rename_detected() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("old.rs"), b"fn x() {}").unwrap();
    commit(dir.path(), "init");
    std::fs::rename(dir.path().join("old.rs"), dir.path().join("new.rs")).unwrap();
    let sha = commit(dir.path(), "rename");
    let changes = file_changes_for_commit(dir.path(), &sha).unwrap();
    let r = changes.iter().find(|c| c.path.ends_with("new.rs")).unwrap();
    assert!(matches!(&r.kind, FileChangeKind::Renamed { from } if from.ends_with("old.rs")));
}
```

- [ ] **Step 2: Run test, confirm failure**

- [ ] **Step 3: Implement `file_changes_for_commit`**

```rust
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed { from: PathBuf },
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: PathBuf,
    pub kind: FileChangeKind,
    pub parent_sha: Option<String>,
}

pub fn file_changes_for_commit(worktree: &Path, sha: &str) -> Result<Vec<FileChange>> {
    let parents = commit_parents(worktree, sha)?;
    if parents.is_empty() {
        return root_commit_changes(worktree, sha);
    }

    let mut by_path: std::collections::HashMap<PathBuf, FileChange> = Default::default();
    for parent in &parents {
        let stdout = run_git(
            worktree,
            &["diff-tree", "-r", "--name-status", "--find-renames", parent, sha],
        )?;
        for line in stdout.lines() {
            if line.is_empty() {
                continue;
            }
            let mut cols = line.split('\t');
            let status = cols.next().unwrap_or("");
            let p1 = cols.next().unwrap_or("");
            let p2 = cols.next();
            let kind = match status.chars().next().unwrap_or(' ') {
                'A' => FileChangeKind::Added,
                'M' => FileChangeKind::Modified,
                'D' => FileChangeKind::Deleted,
                'R' => FileChangeKind::Renamed { from: PathBuf::from(p1) },
                'T' => FileChangeKind::Modified, // type change — treat as modify
                other => bail!("unexpected diff status `{other}` in `{line}`"),
            };
            let path = match &kind {
                FileChangeKind::Renamed { .. } => PathBuf::from(p2.unwrap_or(p1)),
                _ => PathBuf::from(p1),
            };
            by_path.entry(path.clone()).or_insert(FileChange {
                path,
                kind,
                parent_sha: Some(parent.clone()),
            });
        }
    }
    Ok(by_path.into_values().collect())
}

fn commit_parents(worktree: &Path, sha: &str) -> Result<Vec<String>> {
    let out = run_git(worktree, &["rev-list", "--parents", "-n", "1", sha])?;
    let mut iter = out.split_whitespace();
    let _self = iter.next();
    Ok(iter.map(String::from).collect())
}

fn root_commit_changes(worktree: &Path, sha: &str) -> Result<Vec<FileChange>> {
    let stdout = run_git(worktree, &["ls-tree", "-r", "--name-only", sha])?;
    Ok(stdout
        .lines()
        .map(|p| FileChange {
            path: PathBuf::from(p),
            kind: FileChangeKind::Added,
            parent_sha: None,
        })
        .collect())
}
```

- [ ] **Step 4: Run tests**

```
cargo test -p spur-graph git_walk::tests::file_diff_initial_commit_marks_all_added
cargo test -p spur-graph git_walk::tests::file_diff_rename_detected
```

Expected: both pass.

- [ ] **Step 5: Commit**

```
git add crates/spur-graph/src/git_walk.rs
git commit -m "feat(spur-graph): per-commit file-level diff with rename detection"
```

---

## Task 7: `git_walk` — per-commit symbol diff (Tier 1 exact identity)

**Files:**
- Modify: `crates/spur-graph/src/git_walk.rs`
- Test: inline

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn symbol_diff_classifies_added_modified_deleted() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("lib.rs"), b"fn a() {}\nfn b() {}\n").unwrap();
    let sha1 = commit(dir.path(), "c1");
    std::fs::write(
        dir.path().join("lib.rs"),
        b"fn a() { 42; }\nfn c() {}\n",
    )
    .unwrap();
    let sha2 = commit(dir.path(), "c2");

    let mut ctx = SymbolDiffCtx::new();
    let changes = symbol_changes_for_commit(dir.path(), &sha2, &mut ctx).unwrap();
    let by_name: std::collections::HashMap<_, _> = changes
        .iter()
        .map(|c| (c.snapshot.entity_name.clone(), &c.change_kind))
        .collect();
    assert!(matches!(by_name.get("a"), Some(ChangeKind::Modified)));
    assert!(matches!(by_name.get("c"), Some(ChangeKind::Added)));
    assert!(matches!(by_name.get("b"), Some(ChangeKind::Deleted)));
}
```

- [ ] **Step 2: Run, confirm failure**

- [ ] **Step 3: Implement**

```rust
use std::collections::HashMap;

use crate::extract::languages::Language;
use crate::extract::tree_sitter::{BytesExtractor, ExtractedSymbol};
use crate::schema::{ChangeKind, SnapshotKey, SymbolSnapshotArtifact};

pub struct SymbolDiffCtx {
    extractors: HashMap<Language, BytesExtractor>,
}

impl SymbolDiffCtx {
    pub fn new() -> Self {
        Self { extractors: HashMap::new() }
    }
    fn for_language(&mut self, l: Language) -> Result<&mut BytesExtractor> {
        if !self.extractors.contains_key(&l) {
            self.extractors.insert(l, BytesExtractor::for_language(l)?);
        }
        Ok(self.extractors.get_mut(&l).unwrap())
    }
}

#[derive(Debug)]
pub struct SymbolChange {
    pub snapshot: SymbolSnapshotArtifact,
    pub change_kind: ChangeKind,
}

pub fn symbol_changes_for_commit(
    worktree: &Path,
    sha: &str,
    ctx: &mut SymbolDiffCtx,
) -> Result<Vec<SymbolChange>> {
    let file_changes = file_changes_for_commit(worktree, sha)?;
    let mut out = Vec::new();
    for fc in &file_changes {
        let Some(language) = Language::from_path(&fc.path) else { continue };
        let extractor = ctx.for_language(language)?;
        let (left_bytes, right_bytes) = blobs_for_change(worktree, sha, fc)?;

        let left_syms = if let Some(b) = left_bytes.as_ref() {
            extractor.extract(&fc.path, b).unwrap_or_default()
        } else {
            Vec::new()
        };
        let right_syms = if let Some(b) = right_bytes.as_ref() {
            extractor.extract(&fc.path, b).unwrap_or_default()
        } else {
            Vec::new()
        };

        // Tier 1: exact identity by (entity_name, enclosing_scope).
        let mut left_by_key: HashMap<(String, Option<String>), &ExtractedSymbol> =
            left_syms.iter().map(|s| ((s.entity_name.clone(), s.enclosing_scope.clone()), s)).collect();

        for r in &right_syms {
            let key = (r.entity_name.clone(), r.enclosing_scope.clone());
            let change = match left_by_key.remove(&key) {
                Some(l) if l.anchor_hash == r.anchor_hash => continue, // byte-equivalent
                Some(_) => ChangeKind::Modified,
                None => ChangeKind::Added,
            };
            out.push(SymbolChange {
                snapshot: snapshot_from(sha, &fc.path, r),
                change_kind: change,
            });
        }
        // Remaining lefts are deletes (rename detection in Task 8 may reclassify).
        for (_k, l) in left_by_key {
            out.push(SymbolChange {
                snapshot: snapshot_from(sha, &fc.path, l),
                change_kind: ChangeKind::Deleted,
            });
        }
    }
    Ok(out)
}

fn snapshot_from(commit: &str, path: &Path, s: &ExtractedSymbol) -> SymbolSnapshotArtifact {
    SymbolSnapshotArtifact {
        key: SnapshotKey {
            stable_symbol_id: crate::identity::stable_symbol_id_for(path, &s.entity_name, &s.anchor_hash),
            commit: commit.to_string(),
        },
        file_path: path.to_path_buf(),
        entity_name: s.entity_name.clone(),
        symbol_kind: s.symbol_kind.clone(),
        enclosing_scope: s.enclosing_scope.clone(),
        byte_range: s.byte_range,
        line_range: s.line_range,
        anchor_hash: s.anchor_hash.clone(),
    }
}

fn blobs_for_change(
    worktree: &Path,
    sha: &str,
    fc: &FileChange,
) -> Result<(Option<Vec<u8>>, Option<Vec<u8>>)> {
    use std::process::Stdio;
    let parent = fc.parent_sha.as_deref();
    let right = match &fc.kind {
        FileChangeKind::Deleted => None,
        _ => Some(cat_file_blob(worktree, sha, &fc.path)?),
    };
    let left_path = match &fc.kind {
        FileChangeKind::Renamed { from } => Some(from.clone()),
        FileChangeKind::Added => None,
        _ => Some(fc.path.clone()),
    };
    let left = match (parent, left_path) {
        (Some(p), Some(lp)) => Some(cat_file_blob(worktree, p, &lp)?),
        _ => None,
    };
    Ok((left, right))
}

fn cat_file_blob(worktree: &Path, sha: &str, path: &Path) -> Result<Vec<u8>> {
    let spec = format!("{sha}:{}", path.to_string_lossy());
    let out = Command::new("git")
        .current_dir(worktree)
        .args(["cat-file", "blob", &spec])
        .output()?;
    if !out.status.success() {
        bail!("git cat-file failed for {spec}: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(out.stdout)
}
```

If `crate::identity::stable_symbol_id_for` does not exist, locate the function that produces `stable_symbol_id: String` in the current extractor (it's in `crates/spur-graph/src/extract/` somewhere — `rg "stable_symbol_id" crates/spur-graph/src/extract/`) and re-export it from `identity.rs` so both call sites share one definition.

- [ ] **Step 4: Run test**

```
cargo test -p spur-graph git_walk::tests::symbol_diff_classifies_added_modified_deleted
```

Expected: pass.

- [ ] **Step 5: Commit**

```
git add crates/spur-graph/src/git_walk.rs
git commit -m "feat(spur-graph): per-commit symbol diff (Tier 1 exact-identity matching)"
```

---

## Task 8: Rename detection Tiers 1–3

**Files:**
- Modify: `crates/spur-graph/src/git_walk.rs`
- Test: inline

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn tier1_file_rename_inheritance_matches_same_name_kind() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("old.rs"), b"pub fn helper() { 1; 2; 3; }\n").unwrap();
    commit(dir.path(), "c1");
    std::fs::rename(dir.path().join("old.rs"), dir.path().join("new.rs")).unwrap();
    let sha = commit(dir.path(), "rename");
    let mut ctx = SymbolDiffCtx::new();
    let changes = symbol_changes_for_commit(dir.path(), &sha, &mut ctx).unwrap();
    let helper = changes.iter().find(|c| c.snapshot.entity_name == "helper").unwrap();
    assert!(matches!(&helper.change_kind, ChangeKind::RenamedFrom(_)));
}

#[test]
fn tier2_jaccard_matches_renamed_body_similar() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::write(
        dir.path().join("lib.rs"),
        b"pub fn old_name(a: u32, b: u32) -> u32 { a + b * 2 }\n",
    ).unwrap();
    commit(dir.path(), "c1");
    std::fs::write(
        dir.path().join("lib.rs"),
        b"pub fn new_name(a: u32, b: u32) -> u32 { a + b * 2 }\n",
    ).unwrap();
    let sha = commit(dir.path(), "c2");
    let mut ctx = SymbolDiffCtx::new();
    let changes = symbol_changes_for_commit(dir.path(), &sha, &mut ctx).unwrap();
    let renamed = changes.iter().find(|c| c.snapshot.entity_name == "new_name").unwrap();
    assert!(matches!(&renamed.change_kind, ChangeKind::RenamedFrom(_)));
}

#[test]
fn tier3_ambiguous_falls_back_to_added_deleted() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("lib.rs"), b"pub fn old() { 1 }\n").unwrap();
    commit(dir.path(), "c1");
    // Two candidates both equally similar to `old`.
    std::fs::write(
        dir.path().join("lib.rs"),
        b"pub fn a() { 1 }\npub fn b() { 1 }\n",
    ).unwrap();
    let sha = commit(dir.path(), "c2");
    let mut ctx = SymbolDiffCtx::new();
    let changes = symbol_changes_for_commit(dir.path(), &sha, &mut ctx).unwrap();
    let kinds: Vec<_> = changes.iter().map(|c| &c.change_kind).collect();
    assert!(kinds.iter().any(|k| matches!(k, ChangeKind::Deleted)));
    assert!(kinds.iter().filter(|k| matches!(k, ChangeKind::Added)).count() == 2);
    // Ensure no RenamedFrom was emitted.
    assert!(!kinds.iter().any(|k| matches!(k, ChangeKind::RenamedFrom(_))));
}

#[test]
fn merge_collision_emits_added_and_keeps_deleted() {
    // Two `Deleted` candidates would both want to rename to the same `Added`.
    // Spec invariant: forbid many-old -> one-new merges; emit Added on the new,
    // leave all olds as Deleted, record merge_collision diagnostic.
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::write(
        dir.path().join("lib.rs"),
        b"pub fn old_a(x: u32) -> u32 { x + 1 }\npub fn old_b(x: u32) -> u32 { x + 1 }\n",
    )
    .unwrap();
    commit(dir.path(), "c1");
    // Both old_a and old_b are equally similar to merged_target.
    std::fs::write(
        dir.path().join("lib.rs"),
        b"pub fn merged_target(x: u32) -> u32 { x + 1 }\n",
    )
    .unwrap();
    let sha = commit(dir.path(), "c2");
    let mut ctx = SymbolDiffCtx::new();
    let changes = symbol_changes_for_commit(dir.path(), &sha, &mut ctx).unwrap();
    let added: Vec<_> = changes.iter().filter(|c| matches!(c.change_kind, ChangeKind::Added)).collect();
    let deleted: Vec<_> = changes.iter().filter(|c| matches!(c.change_kind, ChangeKind::Deleted)).collect();
    let renamed: Vec<_> = changes.iter().filter(|c| matches!(c.change_kind, ChangeKind::RenamedFrom(_))).collect();
    assert_eq!(added.len(), 1, "merged_target should be Added");
    assert_eq!(deleted.len(), 2, "both olds should remain Deleted");
    assert!(renamed.is_empty(), "no RenamedFrom may be emitted in merge collision");
}
```

- [ ] **Step 2: Run, confirm failure**

- [ ] **Step 3: Implement tiered rename detection**

Refactor `symbol_changes_for_commit` so that after the Tier 1 exact-identity pass, the leftover candidates run through Tiers 2–3.

```rust
struct RenameCandidate {
    deleted: Box<SymbolChange>,
    added: Box<SymbolChange>,
    score: f64,
}

fn try_rename_match(
    candidates_deleted: Vec<SymbolChange>,
    candidates_added: Vec<SymbolChange>,
    fc: &FileChange,
    language: Language,
) -> (Vec<SymbolChange>, Vec<RenameMatch>, Vec<String> /* diagnostics */) {
    let mut diagnostics = Vec::new();
    let mut matched: Vec<RenameMatch> = Vec::new();
    let mut leftover: Vec<SymbolChange> = Vec::new();

    // Tier 1: file-rename inheritance (only when the enclosing file was renamed).
    if matches!(fc.kind, FileChangeKind::Renamed { .. }) {
        let mut pool_del = candidates_deleted;
        let mut pool_add = candidates_added;
        let mut take = Vec::new();
        for (i_a, a) in pool_add.iter().enumerate() {
            for (i_d, d) in pool_del.iter().enumerate() {
                if d.snapshot.entity_name == a.snapshot.entity_name
                    && d.snapshot.symbol_kind == a.snapshot.symbol_kind
                    && d.snapshot.enclosing_scope == a.snapshot.enclosing_scope
                {
                    take.push((i_d, i_a));
                    break;
                }
            }
        }
        // Apply matches; remove from pools (high-to-low to preserve indices).
        take.sort_by_key(|(d, a)| std::cmp::Reverse((*d, *a)));
        for (i_d, i_a) in take {
            let d = pool_del.remove(i_d);
            let a = pool_add.remove(i_a);
            matched.push(RenameMatch { from: d, to: a, confidence: RenameConfidence::High });
        }
        // Tier 2/3 will operate on what's left.
        return tier2_and_tier3(pool_del, pool_add, fc, language, matched, &mut diagnostics);
    }

    // Same-file rename: run Tier 2 directly.
    tier2_and_tier3(candidates_deleted, candidates_added, fc, language, matched, &mut diagnostics)
}

fn tier2_and_tier3(
    pool_del: Vec<SymbolChange>,
    pool_add: Vec<SymbolChange>,
    fc: &FileChange,
    language: Language,
    mut matched: Vec<RenameMatch>,
    diagnostics: &mut Vec<String>,
) -> (Vec<SymbolChange>, Vec<RenameMatch>, Vec<String>) {
    let Some(threshold) = jaccard_threshold_for(language) else {
        // Languages without a calibrated threshold disable Tier 2.
        let mut leftover = pool_del;
        leftover.extend(pool_add);
        return (leftover, matched, diagnostics.clone());
    };

    let mut leftover_del = pool_del;
    let mut leftover_add: Vec<SymbolChange> = Vec::new();
    'outer: for a in pool_add {
        let mut scored: Vec<(usize, f64)> = leftover_del
            .iter()
            .enumerate()
            .map(|(i, d)| (i, jaccard_tokens(&a.snapshot, &d.snapshot)))
            .collect();
        scored.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
        match scored.as_slice() {
            [] => leftover_add.push(a),
            [(i, s)] if *s >= threshold => {
                let d = leftover_del.remove(*i);
                matched.push(RenameMatch { from: d, to: a, confidence: RenameConfidence::Medium });
            }
            [(i, s), (_, s2), ..] if *s >= threshold && (*s - *s2) >= 0.05 => {
                let d = leftover_del.remove(*i);
                matched.push(RenameMatch { from: d, to: a, confidence: RenameConfidence::Medium });
            }
            _ => {
                diagnostics.push(format!(
                    "ambiguous_rename: file={} candidate={}",
                    fc.path.display(),
                    a.snapshot.entity_name
                ));
                leftover_add.push(a);
            }
        }
    }
    let mut leftover = leftover_del;
    leftover.extend(leftover_add);
    (leftover, matched, diagnostics.clone())
}

fn jaccard_tokens(a: &SymbolSnapshotArtifact, b: &SymbolSnapshotArtifact) -> f64 {
    // Tokens come from the symbol body excluding the symbol's own name.
    // For simplicity here, derive tokens from anchor_hash + name + scope —
    // the actual implementation must parse the AST and extract leaf identifiers
    // + literals via the same tree-sitter queries used by BytesExtractor.
    // Engineer: extend BytesExtractor to optionally return token bags per symbol.
    let ta = token_bag(a);
    let tb = token_bag(b);
    let inter = ta.intersection(&tb).count() as f64;
    let union = ta.union(&tb).count() as f64;
    if union == 0.0 { 0.0 } else { inter / union }
}

fn token_bag(_s: &SymbolSnapshotArtifact) -> std::collections::HashSet<String> {
    // Implementation: BytesExtractor must expose per-symbol leaf-token lists.
    // Add a `tokens: Vec<String>` field to ExtractedSymbol and a getter; then
    // carry the token bag through snapshot_from(). Don't compute tokens here
    // from anchor_hash — that would defeat the purpose.
    unimplemented!("BytesExtractor must expose per-symbol leaf tokens; see Task 8 notes")
}

fn jaccard_threshold_for(language: Language) -> Option<f64> {
    match language {
        Language::Rust => Some(0.7),
        Language::TypeScript => Some(0.7),
        Language::Python => Some(0.65),
        _ => None,
    }
}

pub enum RenameConfidence {
    High,
    Medium,
}

pub struct RenameMatch {
    pub from: SymbolChange,
    pub to: SymbolChange,
    pub confidence: RenameConfidence,
}
```

**Important refactor for the engineer:** `token_bag` requires `ExtractedSymbol` to expose its leaf tokens. Extend `ExtractedSymbol` in `extract/tree_sitter.rs` with `pub tokens: Vec<String>` and have `BytesExtractor::extract` populate it by walking the captured symbol subtree and collecting identifier/literal leaves. Then carry `tokens` through `snapshot_from` to the snapshot (add a `tokens` field to `SymbolSnapshotArtifact` with `#[serde(default, skip_serializing_if = "Vec::is_empty")]`).

After applying matches, rewrite the `change_kind` on each matched pair: the `to` side becomes `ChangeKind::RenamedFrom(RenamePrev::Symbol(from.snapshot.key.clone()))`; the `from` side is dropped from the output set (replaced by the `RenamedFrom` link on `to`).

Also: enforce the **merge invariant** — if more than one Tier 2 match resolves to the same `to` candidate (many-old → one-new), record `merge_collision` in diagnostics; emit `Added` on the winner, leave all candidates as `Deleted`, do not emit any `RenamedFrom`.

- [ ] **Step 4: Run tests**

```
cargo test -p spur-graph git_walk::tests::tier1_file_rename_inheritance_matches_same_name_kind
cargo test -p spur-graph git_walk::tests::tier2_jaccard_matches_renamed_body_similar
cargo test -p spur-graph git_walk::tests::tier3_ambiguous_falls_back_to_added_deleted
```

Expected: all three pass.

- [ ] **Step 5: Commit**

```
git add crates/spur-graph/src/git_walk.rs crates/spur-graph/src/extract/
git commit -m "feat(spur-graph): tiered rename detection (file-inherit/Jaccard/ambiguity)"
```

---

## Task 9: Failure modes — force-push, missing blob, gitlinks, non-UTF-8, binary

**Files:**
- Modify: `crates/spur-graph/src/git_walk.rs`
- Test: inline

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn force_push_invalidates_and_rewalks_diverged_range() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join("a.rs"), b"fn a() {}\n").unwrap();
    let sha1 = commit(dir.path(), "c1");
    std::fs::write(dir.path().join("a.rs"), b"fn a() { 1 }\n").unwrap();
    let _sha2 = commit(dir.path(), "c2");
    // Force-rewrite history.
    Command::new("git").current_dir(dir.path()).args(["reset", "--hard", &sha1]).status().unwrap();
    std::fs::write(dir.path().join("a.rs"), b"fn a() { 999 }\n").unwrap();
    let sha2b = commit(dir.path(), "c2b");

    let plan = plan_incremental_walk(dir.path(), Some(&sha1), &sha2b).unwrap();
    assert!(matches!(plan, IncrementalPlan::FastForward { .. }));
    // Stored at original sha2 (no longer ancestor) → re-walk from divergence.
    let plan2 = plan_incremental_walk(dir.path(), Some("deadbeef"), &sha2b).unwrap();
    assert!(matches!(plan2, IncrementalPlan::ForcePushRecover { .. }));
}

#[test]
fn missing_blob_fails_closed_with_named_oid() {
    // Simulate: corrupt the object dir by removing a known blob.
    // (Setup is fiddly; left as an integration test in the corpus harness.)
}

#[test]
fn gitlink_emits_file_edge_no_recurse() {
    // (As above — covered in integration harness.)
}
```

- [ ] **Step 2: Run, confirm failure**

- [ ] **Step 3: Implement `plan_incremental_walk`**

```rust
#[derive(Debug)]
pub enum IncrementalPlan {
    ColdWalk { from_root: bool },
    FastForward { from: String, to: String },
    ForcePushRecover { merge_base: Option<String>, to: String },
}

pub fn plan_incremental_walk(
    worktree: &Path,
    stored_tip: Option<&str>,
    new_tip: &str,
) -> Result<IncrementalPlan> {
    let Some(stored) = stored_tip else {
        return Ok(IncrementalPlan::ColdWalk { from_root: true });
    };
    let is_ancestor = run_git(worktree, &["merge-base", "--is-ancestor", stored, new_tip]).is_ok();
    if is_ancestor {
        return Ok(IncrementalPlan::FastForward {
            from: stored.to_string(),
            to: new_tip.to_string(),
        });
    }
    let merge_base = run_git(worktree, &["merge-base", stored, new_tip])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(IncrementalPlan::ForcePushRecover {
        merge_base,
        to: new_tip.to_string(),
    })
}
```

- [ ] **Step 4: Implement partial-clone promisor retry in `cat_file_blob`**

Modify the function from Task 7:

```rust
fn cat_file_blob(worktree: &Path, sha: &str, path: &Path) -> Result<Vec<u8>> {
    let spec = format!("{sha}:{}", path.to_string_lossy());
    let attempt = || -> Result<Vec<u8>> {
        let out = Command::new("git")
            .current_dir(worktree)
            .args(["cat-file", "blob", &spec])
            .output()?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(anyhow!(
                "git cat-file blob {spec} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    };
    match attempt() {
        Ok(b) => Ok(b),
        Err(_) if has_promisor_remote(worktree) => attempt().with_context(|| {
            format!(
                "missing blob `{spec}` not recovered by promisor remote — \
                 fail-closed for this commit"
            )
        }),
        Err(e) => Err(e.context(format!("missing blob `{spec}`; partial clone? fail-closed"))),
    }
}

fn has_promisor_remote(worktree: &Path) -> bool {
    run_git(worktree, &["config", "--get-all", "remote.*.promisor"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}
```

- [ ] **Step 5: Add binary-blob detection in `symbol_changes_for_commit`**

Before extracting, skip blobs that contain NUL bytes in the first 8 KiB:

```rust
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|&b| b == 0)
}
```

If the blob is binary, emit a file-level touches edge with no symbol-level diff and continue.

- [ ] **Step 6: Handle gitlinks (submodules)**

When `file_changes_for_commit` encounters a path whose mode in the diff is `160000`, mark it as a gitlink. Add a variant to `FileChangeKind`:

```rust
FileChangeKind::Gitlink { old_oid: Option<String>, new_oid: Option<String> },
```

Detect via `git diff-tree --raw` instead of (or in addition to) `--name-status`, since `--raw` exposes the mode column. Emit only the file-level edge; do not recurse into the submodule.

- [ ] **Step 7: Handle non-UTF-8 paths from git output**

`git diff-tree --name-status` emits raw bytes when a path is not UTF-8 (quoted with octal escapes by default unless `core.quotepath=false`). Set `core.quotepath=false` for the duration of the walk and parse paths as `&[u8]` via `OsStr::from_bytes` (Unix) / lossy conversion (Windows) — never `String::from_utf8`. Carry path bytes losslessly through `FileChange.path` (use `PathBuf`, which is `OsString`-backed). Add a regression test using a path with embedded non-ASCII bytes:

```rust
#[test]
fn non_utf8_path_does_not_panic() {
    // Construct a path with bytes that aren't valid UTF-8.
    // Confirm file_changes_for_commit returns Ok and the path is preserved.
}
```

- [ ] **Step 8: Run tests**

```
cargo test -p spur-graph git_walk::tests::force_push_invalidates_and_rewalks_diverged_range
cargo test -p spur-graph git_walk
```

Force-push test must pass; the unimplemented missing-blob/gitlink/non-UTF-8 stub tests stay as placeholders for the integration harness in Task 12.

- [ ] **Step 9: Commit**

```
git add crates/spur-graph/src/git_walk.rs
git commit -m "feat(spur-graph): failure-mode handling (force-push/partial-clone/binary/gitlink)"
```

---

## Task 10: `temporal.rs` — `Resolution<T>` + `resolve_symbol_at`

**Files:**
- Create: `crates/spur-graph/src/temporal.rs`
- Modify: `crates/spur-graph/src/lib.rs` (add `pub mod temporal;`)
- Test: inline

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::*;

    fn fixture() -> (GraphIndexArtifact, CommitIndexArtifact) {
        let mut g = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: GRAPH_INDEX_VERSION_TEMPORAL.into(),
                content_hash_blake3: None,
            },
            manifest_version: String::new(),
            graph_content_hash: String::new(),
            file_manifests: vec![],
            files: vec![],
            symbols: vec![],
            edges: vec![],
            tombstones: vec![],
            diagnostics: vec![],
            commits: vec![],
            symbol_snapshots: vec![],
            temporal_edges: vec![],
        };
        let c1 = CommitArtifact { sha: "c1".into(), parents: vec![], author_time: 0, summary: "init".into() };
        let c2 = CommitArtifact { sha: "c2".into(), parents: vec!["c1".into()], author_time: 1, summary: "rename".into() };
        let snap_old = SymbolSnapshotArtifact {
            key: SnapshotKey { stable_symbol_id: "old".into(), commit: "c1".into() },
            file_path: "lib.rs".into(),
            entity_name: "old".into(),
            symbol_kind: "function".into(),
            enclosing_scope: None,
            byte_range: [0, 10],
            line_range: [1, 1],
            anchor_hash: "h1".into(),
        };
        let snap_new = SymbolSnapshotArtifact {
            key: SnapshotKey { stable_symbol_id: "new".into(), commit: "c2".into() },
            file_path: "lib.rs".into(),
            entity_name: "new".into(),
            symbol_kind: "function".into(),
            enclosing_scope: None,
            byte_range: [0, 10],
            line_range: [1, 1],
            anchor_hash: "h1".into(),
        };
        g.symbol_snapshots.push(snap_old.clone());
        g.symbol_snapshots.push(snap_new.clone());
        g.temporal_edges.push(TemporalEdgeArtifact {
            source: EdgeEndpoint::Commit { sha: "c1".into() },
            target: EdgeEndpoint::Snapshot { key: snap_old.key.clone() },
            relation: RelationKind::Touches,
            change_kind: Some(ChangeKind::Added),
        });
        g.temporal_edges.push(TemporalEdgeArtifact {
            source: EdgeEndpoint::Commit { sha: "c2".into() },
            target: EdgeEndpoint::Snapshot { key: snap_new.key.clone() },
            relation: RelationKind::Touches,
            change_kind: Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(snap_old.key.clone()))),
        });
        // Explicit renamed_from edge for fast traversal.
        g.temporal_edges.push(TemporalEdgeArtifact {
            source: EdgeEndpoint::Snapshot { key: snap_new.key.clone() },
            target: EdgeEndpoint::Snapshot { key: snap_old.key.clone() },
            relation: RelationKind::Touches, // No dedicated RenamedFrom variant; reuse Touches + ChangeKind
            change_kind: Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(snap_old.key.clone()))),
        });
        let cidx = CommitIndexArtifact {
            schema_version: 1,
            commits: vec![c1, c2],
            refs: [("main".into(), "c2".into())].into(),
            indexed_at: "2026-05-20T12:00:00Z".into(),
            walk_strategy: WalkStrategy::Reachable,
        };
        (g, cidx)
    }

    #[test]
    fn resolves_renamed_symbol_to_target() {
        let (g, c) = fixture();
        let r = resolve_symbol_at(&g, &c, "old", "c1", "c2");
        match r {
            Resolution::Found { value, chain } => {
                assert_eq!(value, "new");
                assert_eq!(chain.len(), 1);
            }
            other => panic!("expected Found, got {other:?}"),
        }
    }

    #[test]
    fn resolves_unknown_anchor() {
        let (g, c) = fixture();
        let r = resolve_symbol_at(&g, &c, "old", "nonexistent", "c2");
        assert!(matches!(r, Resolution::Unknown { .. }));
    }
}
```

- [ ] **Step 2: Run, confirm failure (module missing)**

- [ ] **Step 3: Implement `temporal.rs`**

```rust
use crate::schema::{
    ChangeKind, CommitIndexArtifact, EdgeEndpoint, GraphIndexArtifact, RelationKind, RenamePrev,
    SnapshotKey,
};

#[derive(Debug)]
pub enum Resolution<T> {
    Found { value: T, chain: Vec<SnapshotKey> },
    Deleted { last_seen: SnapshotKey },
    Ambiguous { candidates: Vec<T> },
    Unknown { reason: ResolutionFailure },
}

#[derive(Debug)]
pub enum ResolutionFailure {
    AnchorCommitNotIndexed(String),
    SymbolNotPresentAtAnchor,
    IndexCorrupt(String),
}

pub fn resolve_symbol_at(
    code: &GraphIndexArtifact,
    commits: &CommitIndexArtifact,
    symbol: &str,
    anchor: &str,
    target: &str,
) -> Resolution<String> {
    if !commits.commits.iter().any(|c| c.sha == anchor) {
        return Resolution::Unknown {
            reason: ResolutionFailure::AnchorCommitNotIndexed(anchor.to_string()),
        };
    }
    let anchor_snap = code
        .symbol_snapshots
        .iter()
        .find(|s| s.key.stable_symbol_id == symbol && s.key.commit == anchor);
    let Some(anchor_snap) = anchor_snap else {
        return Resolution::Unknown {
            reason: ResolutionFailure::SymbolNotPresentAtAnchor,
        };
    };

    let mut current = anchor_snap.key.clone();
    let mut chain = Vec::new();
    let parents_of: std::collections::HashMap<&str, &[String]> = commits
        .commits
        .iter()
        .map(|c| (c.sha.as_str(), c.parents.as_slice()))
        .collect();

    loop {
        // Find an outbound RenamedFrom edge whose source is `current`.
        let forward = find_forward_rename(code, &current);
        let next_snap = match forward {
            Some(k) => k,
            None => break,
        };
        chain.push(current.clone());
        current = next_snap;
        if current.commit == target {
            return Resolution::Found {
                value: current.stable_symbol_id,
                chain,
            };
        }
    }
    // Check if `current` was Deleted before target.
    if last_change_is_delete(code, &current) {
        return Resolution::Deleted { last_seen: current };
    }
    Resolution::Found {
        value: current.stable_symbol_id,
        chain,
    }
}

fn find_forward_rename(code: &GraphIndexArtifact, from: &SnapshotKey) -> Option<SnapshotKey> {
    // The to-snapshot points at `from` via ChangeKind::RenamedFrom(Symbol(from)).
    for e in &code.temporal_edges {
        if let (EdgeEndpoint::Commit { .. }, EdgeEndpoint::Snapshot { key: to }) = (&e.source, &e.target) {
            if let Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(prev))) = &e.change_kind {
                if prev == from {
                    return Some(to.clone());
                }
            }
        }
    }
    None
}

fn last_change_is_delete(code: &GraphIndexArtifact, snap: &SnapshotKey) -> bool {
    code.temporal_edges.iter().any(|e| {
        matches!(
            (&e.target, &e.change_kind),
            (EdgeEndpoint::Snapshot { key }, Some(ChangeKind::Deleted)) if key == snap
        )
    })
}
```

- [ ] **Step 4: Add `pub mod temporal;` to `lib.rs`**

- [ ] **Step 5: Run tests**

```
cargo test -p spur-graph temporal
```

Expected: both pass.

- [ ] **Step 6: Commit**

```
git add crates/spur-graph/src/temporal.rs crates/spur-graph/src/lib.rs
git commit -m "feat(spur-graph): Resolution<T> + resolve_symbol_at temporal API"
```

---

## Task 11: `temporal.rs` — `symbol_history`

**Files:**
- Modify: `crates/spur-graph/src/temporal.rs`
- Test: inline

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn symbol_history_returns_chronological_chain() {
    let (g, c) = fixture();
    let hist = symbol_history(&g, &c, "old");
    // old (c1, Added) -> new (c2, RenamedFrom(old))
    assert_eq!(hist.len(), 2);
    assert_eq!(hist[0].0, "c1");
    assert!(matches!(hist[0].1, ChangeKind::Added));
    assert_eq!(hist[1].0, "c2");
    assert!(matches!(hist[1].1, ChangeKind::RenamedFrom(_)));
}
```

- [ ] **Step 2: Run, confirm failure**

- [ ] **Step 3: Implement**

```rust
pub fn symbol_history(
    code: &GraphIndexArtifact,
    commits: &CommitIndexArtifact,
    symbol: &str,
) -> Vec<(String /* commit sha */, ChangeKind, SnapshotKey)> {
    let order: std::collections::HashMap<&str, usize> = commits
        .commits
        .iter()
        .enumerate()
        .map(|(i, c)| (c.sha.as_str(), i))
        .collect();

    // Build set of snapshot keys belonging to symbol's rename chain (forward + backward).
    let mut chain_keys: std::collections::HashSet<SnapshotKey> = code
        .symbol_snapshots
        .iter()
        .filter(|s| s.key.stable_symbol_id == symbol)
        .map(|s| s.key.clone())
        .collect();
    // Backward closure via RenamedFrom predecessors.
    let mut changed = true;
    while changed {
        changed = false;
        for e in &code.temporal_edges {
            if let (EdgeEndpoint::Commit { .. }, EdgeEndpoint::Snapshot { key: to }) = (&e.source, &e.target) {
                if chain_keys.contains(to) {
                    if let Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(prev))) = &e.change_kind {
                        if chain_keys.insert(prev.clone()) {
                            changed = true;
                        }
                    }
                }
            }
        }
    }

    let mut events: Vec<(String, ChangeKind, SnapshotKey)> = code
        .temporal_edges
        .iter()
        .filter_map(|e| match (&e.source, &e.target) {
            (EdgeEndpoint::Commit { sha }, EdgeEndpoint::Snapshot { key }) if chain_keys.contains(key) => {
                e.change_kind.clone().map(|ck| (sha.clone(), ck, key.clone()))
            }
            _ => None,
        })
        .collect();
    events.sort_by_key(|(sha, _, _)| order.get(sha.as_str()).copied().unwrap_or(usize::MAX));
    events
}
```

- [ ] **Step 4: Run tests, commit**

```
cargo test -p spur-graph temporal::tests
git add crates/spur-graph/src/temporal.rs
git commit -m "feat(spur-graph): symbol_history traversal across rename chains"
```

---

## Task 12: Scripted-fixture functional tests

**Files:**
- Create: `crates/spur-graph/tests/temporal_resolution.rs`

- [ ] **Step 1: Write fixture script + property test**

Create the file with a `build_history` helper that scripts: add → modify → delete → rename-file → rename-symbol → rename-file+symbol → squash-equivalent re-add → merge commit. Each scripted action returns its commit SHA and a label. Then run a full `git_walk` pipeline and assert resolution matches the script.

```rust
use spur_graph::git_walk::{GitWalkConfig, plan_incremental_walk, run_full_walk_into};
use spur_graph::temporal::{resolve_symbol_at, symbol_history, Resolution};
use std::process::Command;
use tempfile::TempDir;

struct Step {
    label: &'static str,
    apply: fn(&std::path::Path),
}

fn run_history(dir: &std::path::Path, steps: &[Step]) -> Vec<String> {
    init_repo(dir);
    let mut shas = Vec::new();
    for s in steps {
        (s.apply)(dir);
        Command::new("git").current_dir(dir).args(["add", "-A"]).status().unwrap();
        Command::new("git").current_dir(dir).args(["commit", "-q", "--allow-empty", "-m", s.label]).status().unwrap();
        let out = Command::new("git").current_dir(dir).args(["rev-parse", "HEAD"]).output().unwrap();
        shas.push(String::from_utf8(out.stdout).unwrap().trim().to_string());
    }
    shas
}

#[test]
fn property_resolve_at_anchor_matches_script() {
    let dir = TempDir::new().unwrap();
    let steps: &[Step] = &[
        Step { label: "add", apply: |d| std::fs::write(d.join("lib.rs"), b"pub fn a(){}\n").unwrap() },
        Step { label: "modify", apply: |d| std::fs::write(d.join("lib.rs"), b"pub fn a(){1;}\n").unwrap() },
        Step { label: "rename_sym", apply: |d| std::fs::write(d.join("lib.rs"), b"pub fn b(){1;}\n").unwrap() },
        Step { label: "rename_file", apply: |d| { std::fs::rename(d.join("lib.rs"), d.join("renamed.rs")).unwrap(); } },
    ];
    let shas = run_history(dir.path(), steps);

    let (g, c) = run_full_walk_into(dir.path(), &GitWalkConfig::default()).unwrap();
    // After all four steps, resolve symbol `a` (introduced at shas[0]) to HEAD.
    let r = resolve_symbol_at(&g, &c, "a", &shas[0], shas.last().unwrap());
    match r {
        Resolution::Found { value, .. } => assert_eq!(value, "b"),
        other => panic!("expected Found(b), got {other:?}"),
    }

    let hist = symbol_history(&g, &c, "a");
    assert!(hist.len() >= 3, "expected ≥3 history events, got {}", hist.len());
}

#[test]
fn force_push_recovery_rebuilds_diverged_range() {
    // Build history, run walk, force-push to a new tip, re-run walk, assert
    // old range is invalidated and new range materialized.
}

#[test]
fn shallow_clone_fails_closed() {
    let dir = TempDir::new().unwrap();
    init_repo(dir.path());
    std::fs::write(dir.path().join(".git/shallow"), b"deadbeef\n").unwrap();
    let r = run_full_walk_into(dir.path(), &GitWalkConfig::default());
    assert!(r.is_err());
}

fn init_repo(dir: &std::path::Path) {
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "T"],
    ] {
        Command::new("git").current_dir(dir).args(args).status().unwrap();
    }
}
```

`run_full_walk_into` is a small new top-level entrypoint added to `git_walk.rs` that drives the walk end-to-end and returns the in-memory `(GraphIndexArtifact, CommitIndexArtifact)` pair. Add it in this task.

- [ ] **Step 2: Implement `run_full_walk_into` in `git_walk.rs`**

```rust
pub fn run_full_walk_into(
    worktree: &Path,
    config: &GitWalkConfig,
) -> Result<(crate::schema::GraphIndexArtifact, crate::schema::CommitIndexArtifact)> {
    ensure_not_shallow(worktree)?;
    check_replace_refs(worktree, config.allow_replace_refs)?;
    let refs = snapshot_refs(
        worktree,
        &config.target_refs.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;
    let tip = refs.get(&config.target_refs[0]).context("target ref tip missing")?;
    let commit_shas = walk_commits(worktree, tip, config.walk_strategy)?;
    let mut graph = empty_graph_artifact();
    let mut cidx = crate::schema::CommitIndexArtifact {
        schema_version: 1,
        commits: vec![],
        refs: refs.clone(),
        indexed_at: chrono::Utc::now().to_rfc3339(),
        walk_strategy: config.walk_strategy,
    };
    let mut ctx = SymbolDiffCtx::new();
    for sha in commit_shas {
        let commit = read_commit(worktree, &sha)?;
        cidx.commits.push(commit.clone());
        graph.temporal_edges.push(commit_to_file_edges(&commit));
        let changes = symbol_changes_for_commit(worktree, &sha, &mut ctx)?;
        for c in changes {
            graph.symbol_snapshots.push(c.snapshot.clone());
            graph.temporal_edges.push(crate::schema::TemporalEdgeArtifact {
                source: crate::schema::EdgeEndpoint::Commit { sha: sha.clone() },
                target: crate::schema::EdgeEndpoint::Snapshot { key: c.snapshot.key.clone() },
                relation: crate::schema::RelationKind::Touches,
                change_kind: Some(c.change_kind),
            });
        }
    }
    Ok((graph, cidx))
}
```

(The helpers `walk_commits`, `read_commit`, `commit_to_file_edges`, and `empty_graph_artifact` are small wrappers around `git rev-list --topo-order` / `git show --format=%P%n%at%n%s`. Implement them in `git_walk.rs`.)

- [ ] **Step 3: Run tests**

```
cargo test -p spur-graph --test temporal_resolution
```

Expected: at least `property_resolve_at_anchor_matches_script` and `shallow_clone_fails_closed` pass. The other tests in the file may be deferred to Task 13/14 if the corresponding infrastructure isn't ready.

- [ ] **Step 4: Commit**

```
git add crates/spur-graph/tests/temporal_resolution.rs crates/spur-graph/src/git_walk.rs
git commit -m "test(spur-graph): scripted-fixture functional tests for temporal walk"
```

---

## Task 13: Per-language rename corpus + precision/recall harness

**Files:**
- Create: `crates/spur-graph/tests/rename_corpus.rs`
- Create: `crates/spur-graph/tests/fixtures/rename_corpus/rust/01-*.{old.rs,new.rs,expected.json}` (≥50 pairs)
- Create: same for `typescript/` and `python/`

- [ ] **Step 1: Define corpus file format**

Each pair is three files:
- `NN-<label>.old.<ext>` — old blob
- `NN-<label>.new.<ext>` — new blob
- `NN-<label>.expected.json` — expected matches:

```json
{
  "expected_renames": [{"from": "old_name", "to": "new_name"}],
  "expected_added": ["entirely_new"],
  "expected_deleted": ["wholly_removed"]
}
```

- [ ] **Step 2: Implement harness**

```rust
use spur_graph::extract::languages::Language;
use spur_graph::extract::tree_sitter::BytesExtractor;
use spur_graph::git_walk::{try_rename_match, SymbolChange};
use std::path::Path;

fn run_corpus(language: Language, lang_dir: &Path) -> CorpusStats {
    let mut stats = CorpusStats::default();
    let mut extractor = BytesExtractor::for_language(language).unwrap();
    for entry in std::fs::read_dir(lang_dir).unwrap() {
        let p = entry.unwrap().path();
        if !p.to_string_lossy().ends_with(".expected.json") { continue; }
        let stem = p.file_stem().unwrap().to_string_lossy().replace(".expected", "");
        let old_path = lang_dir.join(format!("{stem}.old.{}", language.extension()));
        let new_path = lang_dir.join(format!("{stem}.new.{}", language.extension()));
        let expected: Expected =
            serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let old_syms = extractor.extract(&old_path, &std::fs::read(&old_path).unwrap()).unwrap();
        let new_syms = extractor.extract(&new_path, &std::fs::read(&new_path).unwrap()).unwrap();
        // Drive rename detection on these symbol sets...
        // Compute true_positives, false_positives, false_negatives against expected.
        // Accumulate into stats.
    }
    stats
}

#[derive(Default)]
struct CorpusStats { tp: u32, fp: u32, fn_: u32 }

impl CorpusStats {
    fn precision(&self) -> f64 { self.tp as f64 / (self.tp + self.fp).max(1) as f64 }
    fn recall(&self) -> f64 { self.tp as f64 / (self.tp + self.fn_).max(1) as f64 }
    fn f1(&self) -> f64 {
        let (p, r) = (self.precision(), self.recall());
        if p + r == 0.0 { 0.0 } else { 2.0 * p * r / (p + r) }
    }
}

#[test]
fn rust_corpus_f1_meets_baseline() {
    let s = run_corpus(Language::Rust, Path::new("tests/fixtures/rename_corpus/rust"));
    println!("rust F1={:.3} P={:.3} R={:.3}", s.f1(), s.precision(), s.recall());
    assert!(s.f1() >= 0.80, "F1 below 0.80 baseline: {}", s.f1());
}

#[test]
fn typescript_corpus_f1_meets_baseline() {
    let s = run_corpus(Language::TypeScript, Path::new("tests/fixtures/rename_corpus/typescript"));
    assert!(s.f1() >= 0.78);
}

#[test]
fn python_corpus_f1_meets_baseline() {
    let s = run_corpus(Language::Python, Path::new("tests/fixtures/rename_corpus/python"));
    assert!(s.f1() >= 0.75);
}
```

- [ ] **Step 3: Seed each language with ≥50 fixture pairs**

The engineer builds the corpus by:
1. Copying real before/after blobs from the project's own git history (`git log --oneline --diff-filter=R`).
2. For each pair, manually labeling `expected_renames` in `expected.json`.
3. Including a mix of: pure renames, rename + small body change, name reuse across deletions (negative cases), and ambiguous pairs (negative cases — `expected_renames` empty, both names in `expected_added`/`expected_deleted`).

Aim for ≥50 pairs per language. If lower coverage is acceptable for the merge, document the gap as a follow-up issue.

- [ ] **Step 4: Tune per-language thresholds**

Adjust `jaccard_threshold_for` in `git_walk.rs` until F1 meets the baselines in the asserts. Commit threshold changes alongside corpus expansions.

- [ ] **Step 5: Run, commit**

```
cargo test -p spur-graph --test rename_corpus
git add crates/spur-graph/tests/rename_corpus.rs crates/spur-graph/tests/fixtures/rename_corpus/
git commit -m "test(spur-graph): labeled rename corpus + precision/recall/F1 baseline"
```

---

## Task 14: Bench updates — synthetic repos + snapshot growth budget

**Files:**
- Modify: `crates/spur-graph/benches/incremental.rs`

- [ ] **Step 1: Add synthetic-repo builder**

```rust
fn build_synthetic_repo(dir: &std::path::Path, n_commits: usize, merge_density: f32) -> Vec<String> {
    // Init repo, write a single src/lib.rs with K symbols.
    // For each commit: pick a random symbol, mutate its body, occasionally
    // rename it, occasionally branch+merge (rate = merge_density).
    // Return SHAs in order.
}
```

- [ ] **Step 2: Add two benches**

```rust
fn bench_full_walk_1k(c: &mut Criterion) {
    let dir = tempfile::TempDir::new().unwrap();
    let _shas = build_synthetic_repo(dir.path(), 1_000, 0.05);
    c.bench_function("git_walk full 1k linear", |b| {
        b.iter(|| {
            let (g, _) = spur_graph::git_walk::run_full_walk_into(
                dir.path(),
                &spur_graph::git_walk::GitWalkConfig::default(),
            )
            .unwrap();
            println!("snapshots={}", g.symbol_snapshots.len());
        })
    });
}

fn bench_full_walk_20k_merges(c: &mut Criterion) {
    let dir = tempfile::TempDir::new().unwrap();
    let _shas = build_synthetic_repo(dir.path(), 20_000, 0.30);
    c.bench_function("git_walk full 20k merges", |b| {
        b.iter(|| {
            let (g, _) = spur_graph::git_walk::run_full_walk_into(
                dir.path(),
                &spur_graph::git_walk::GitWalkConfig::default(),
            )
            .unwrap();
            println!("snapshots={}", g.symbol_snapshots.len());
        })
    });
}

criterion_group!(temporal_benches, bench_full_walk_1k, bench_full_walk_20k_merges);
criterion_main!(temporal_benches);
```

- [ ] **Step 3: Add snapshot-growth assertion**

```rust
#[test]
fn snapshot_growth_budget() {
    let d1 = tempfile::TempDir::new().unwrap();
    build_synthetic_repo(d1.path(), 1_000, 0.05);
    let (g1, _) = spur_graph::git_walk::run_full_walk_into(
        d1.path(),
        &spur_graph::git_walk::GitWalkConfig::default(),
    )
    .unwrap();
    let d2 = tempfile::TempDir::new().unwrap();
    build_synthetic_repo(d2.path(), 10_000, 0.05);
    let (g2, _) = spur_graph::git_walk::run_full_walk_into(
        d2.path(),
        &spur_graph::git_walk::GitWalkConfig::default(),
    )
    .unwrap();
    let ratio = g2.symbol_snapshots.len() as f64 / g1.symbol_snapshots.len() as f64;
    assert!(
        ratio <= 1.5 * 10.0,
        "snapshot growth {} > 1.5×/10× budget; needs sharded persistence before merge",
        ratio
    );
}
```

- [ ] **Step 4: Run + commit**

```
cargo bench -p spur-graph -- --quick git_walk
cargo test -p spur-graph snapshot_growth_budget --release
git add crates/spur-graph/benches/incremental.rs
git commit -m "bench(spur-graph): 1k/20k synthetic walk + snapshot-growth budget"
```

---

## Task 15: MCP `as_of` + `code_symbol_history` tool

**Files:**
- Modify: `crates/spur-mcp/src/worker_server.rs`
- Modify: `crates/spur-mcp/src/server/handlers/code_graph.rs`
- Modify: `crates/spur-mcp/src/tool_schemas.rs` (or wherever `CodeSymbolParams` / `CodeSubgraphParams` live)
- Modify: `crates/spur-mcp/tests/code_graph_e2e.rs` (or new `temporal_e2e.rs`)

- [ ] **Step 1: Add failing E2E test**

```rust
#[test]
fn code_subgraph_with_as_of_returns_historical_view() {
    // Build a temp worktree with two commits where `foo` was renamed `bar`.
    // Run the MCP tool with as_of = first commit; assert result contains `foo`.
    // Run again without as_of; assert result contains `bar`.
}

#[test]
fn code_symbol_history_returns_chain() {
    // Set up same fixture; call code_symbol_history with `foo`; assert the
    // response includes two events with ChangeKind = Added then RenamedFrom.
}
```

- [ ] **Step 2: Extend params with `as_of`**

```rust
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct CodeSymbolParams {
    pub symbol: String,
    #[serde(default)]
    pub as_of: Option<String>,  // GitSha
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct CodeSubgraphParams {
    // ... existing fields ...
    #[serde(default)]
    pub as_of: Option<String>,
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct CodeSymbolHistoryParams {
    pub symbol: String,
}
```

- [ ] **Step 3: Thread `as_of` through handlers**

In `crates/spur-mcp/src/server/handlers/code_graph.rs::code_callers/callees/subgraph`, when `as_of` is set:

```rust
let commits = match spur_graph::store::commit_index::load_pointer(&worktree)? {
    Some(p) => Some(spur_graph::store::commit_index::load_artifact(&worktree, &p)?),
    None => None,
};
let resolved_id = match (as_of, &commits) {
    (Some(sha), Some(cidx)) => {
        match spur_graph::temporal::resolve_symbol_at(&graph, cidx, &symbol_id, sha, sha) {
            spur_graph::temporal::Resolution::Found { value, .. } => value,
            spur_graph::temporal::Resolution::Deleted { last_seen } => last_seen.stable_symbol_id,
            _ => return Err(McpHandlerError::SymbolNotPresentAtCommit(sha.to_string())),
        }
    }
    _ => symbol_id,
};
// Use resolved_id for the rest of the existing handler logic.
```

- [ ] **Step 4: Add `code_symbol_history` handler + tool**

In `code_graph.rs`:

```rust
pub fn code_symbol_history(args: &Value) -> Result<Value, McpHandlerError> {
    let params: CodeSymbolHistoryParams = serde_json::from_value(args.clone())?;
    let worktree = current_worktree()?;
    let graph = load_graph_artifact_for_request()?;
    let pointer = spur_graph::store::commit_index::load_pointer(&worktree)?
        .ok_or(McpHandlerError::CommitIndexMissing)?;
    let cidx = spur_graph::store::commit_index::load_artifact(&worktree, &pointer)?;
    let hist = spur_graph::temporal::symbol_history(&graph, &cidx, &params.symbol);
    Ok(serde_json::json!({
        "symbol": params.symbol,
        "events": hist.into_iter().map(|(sha, kind, key)| {
            serde_json::json!({
                "commit": sha,
                "change_kind": kind,
                "snapshot": key,
            })
        }).collect::<Vec<_>>(),
    }))
}
```

In `worker_server.rs`, after the existing `code_subgraph_tool`:

```rust
#[tool(
    name = "code_symbol_history",
    description = "Return the full causal trace of a code symbol — every commit that touched it, with ChangeKind and snapshot key at that commit. Requires that the worktree has a temporal commit index.",
    input_schema = crate::tool_schemas::schema_object::<CodeSymbolHistoryParams>()
)]
async fn code_symbol_history_tool(
    &self,
    arguments: JsonObject,
    context: RequestContext<RoleServer>,
) -> Result<CallToolResult, McpError> {
    let args = Value::Object(arguments);
    self.invoke_with_lifecycle(
        "code_symbol_history",
        context,
        Some(None),
        move |_worker_ctx| async move {
            crate::server::handlers::code_graph::code_symbol_history(&args)
        },
    )
    .await
}
```

- [ ] **Step 5: Run E2E tests**

```
cargo test -p spur-mcp --test code_graph_e2e
```

Both new tests must pass.

- [ ] **Step 6: Commit**

```
git add crates/spur-mcp/
git commit -m "feat(spur-mcp): as_of param on code_subgraph/callers/callees + code_symbol_history tool"
```

---

## Final check: end-to-end smoke

- [ ] **Step 1: Full crate test pass**

```
cargo test -p spur-graph
cargo test -p spur-mcp
cargo clippy -p spur-graph -p spur-mcp --all-targets -- -D warnings
cargo fmt --all -- --check
```

- [ ] **Step 2: Manual smoke**

```
# In a real worktree with the new commit index built:
cargo run -p spur-cli -- graph build  # or whatever the existing entrypoint is
# Then via the MCP, invoke code_symbol_history with a known long-lived symbol.
```

- [ ] **Step 3: Final commit (if needed) and PR**

```
git push -u origin HEAD
gh pr create --title "Phase 1: code-as-memory — commit/snapshot graph + temporal MCP" \
  --body "Implements docs/superpowers/specs/2026-05-20-code-as-memory-phase-1-design.md"
```

---

## Known follow-ups (out of scope for this plan)

These failure modes are documented in the spec but deferred because they are integration-edge cases not easily triggered in synthetic fixtures. Track each as a separate beads issue once Phase 1 lands:

- **Ref rename detection** (e.g. `master` → `main`). Spec says: detect when the stored ref disappears but a new ref points at a descendant of the stored tip, and continue. Implement when a real workflow hits this.
- **Packed-refs vs loose-refs race.** Spec says: a single `git for-each-ref` snapshot at start is authoritative for the run. Currently we snapshot via individual `rev-parse` calls in Task 5; an explicit `for-each-ref` batch snapshot is the durable fix.
- **`code_callees`/`code_callers` temporal mode for ambiguous-call-target cases.** When a call's target is itself ambiguous at `as_of`, the right surface (return ambiguity vs. silently choose one) needs UX validation with real consumers.

None are blocking the merge if Phase 1 is shipped to internal-only consumers first.

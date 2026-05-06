# SPUR Graph Engine Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace SPUR's external `bv` (beads_viewer) Go binary subprocess with a native Rust graph engine inside `crates/spur-pm/src/graph_engine/`, producing wire-compatible reports for the 5 existing call sites in `BvAdapter`.

**Architecture:** New `graph_engine` module in `spur-pm`. Loader runs inside `BeadsCrateAdapter::read(|s| ...)` and returns a `GraphSnapshot` value. Five pure-function analyzers (`triage`, `plan`, `insights`, `alerts`, `subgraph`) consume the snapshot and emit the existing typed reports from `crates/spur-pm/src/graph.rs`. `BvAdapter` swaps internals only — public method signatures unchanged.

**Tech Stack:** Rust 2024, `petgraph` (NEW workspace dep), `beads_rust` 0.2.1 (already added by companion plan), `tokio` (already used), `sha2` (already in workspace), `chrono` (already used), `serde_json` (already used).

**Spec:** `docs/superpowers/specs/2026-05-05-spur-graph-engine-design.md`

**Strict prerequisite:** companion plan `docs/superpowers/plans/2026-05-05-beads_rust-direct-crate-adapter.md` must complete its cutover (BeadsAdapter deleted, BeadsCrateAdapter unconditional) **before T16 (BvAdapter swap)**. Tasks T0–T15 develop in parallel using `TestBeadsWorkspace` fixtures.

---

## File structure

| Path | Action | Responsibility |
|---|---|---|
| `Cargo.toml` (workspace) | Modify | Add `petgraph = "0.6"` to `[workspace.dependencies]` |
| `crates/spur-pm/Cargo.toml` | Modify | Reference `petgraph.workspace = true` |
| `crates/spur-pm/src/graph_engine/mod.rs` | Create | `GraphEngine` struct; 5 public async methods (`triage`, `plan`, `insights`, `alerts`, `subgraph`, `graph_by_label`) |
| `crates/spur-pm/src/graph_engine/snapshot.rs` | Create | `GraphSnapshot` value type, `NodeData`, `EdgeData`, `DependencyKind`, `load_graph_snapshot` loader, `data_hash` |
| `crates/spur-pm/src/graph_engine/score.rs` | Create | `ScoreConfig`, `triage_score`, `is_actionable`, `transitive_unblocks_count`, normalization |
| `crates/spur-pm/src/graph_engine/metrics.rs` | Create | `hits`, `betweenness_centrality_brandes`, `k_core_decomposition`, `pagerank_iterative` (fallback) |
| `crates/spur-pm/src/graph_engine/triage.rs` | Create | `compute_triage` pure fn → `TriageReport` |
| `crates/spur-pm/src/graph_engine/plan.rs` | Create | `compute_plan` pure fn → `ExecutionPlan` |
| `crates/spur-pm/src/graph_engine/insights.rs` | Create | `compute_insights` pure fn → `GraphInsights` |
| `crates/spur-pm/src/graph_engine/alerts.rs` | Create | `compute_alerts` pure fn → `AlertReport`; `AlertConfig` |
| `crates/spur-pm/src/graph_engine/subgraph.rs` | Create | `compute_subgraph` pure fn → `DependencyGraph`; DOT + Mermaid formatters |
| `crates/spur-pm/src/graph_engine/raw.rs` | Create | `serialize_*` fns producing wire-compatible `serde_json::Value` for the `raw` field of each report |
| `crates/spur-pm/src/lib.rs` | Modify | `pub mod graph_engine;` |
| `crates/spur-pm/src/bv.rs` | Rewrite | Internal swap: hold `GraphEngine`, delegate; public method signatures unchanged |
| `crates/spur-pm/src/service.rs` | Modify | Remove `bv` binary probe; construct `BvAdapter::connect(beads_crate, graph_cfg)` unconditionally |
| `crates/spur-pm/tests/graph_engine_integration.rs` | Create | Integration tests via `TestBeadsWorkspace` (real `beads_rust` SqliteStorage) |
| `crates/spur-pm/tests/snapshots/` | Create | Per-command JSON snapshot fixtures for MCP raw-passthrough audit |
| `crates/spur-pm/tests/bv_triage.rs` | Modify | Convert to use `TestBeadsWorkspace` (no longer requires `bv` install) |
| `scripts/install.sh` (or equivalent) | Modify | Drop the `brew install dicklesworthstone/tap/bv` step |
| `README.md` and any setup docs | Modify | Remove `bv` install instructions |

---

## Task DAG overview

```
T0 (petgraph dep)
 ├─ T1 (GraphSnapshot type)
 │   ├─ T2 (loader)
 │   │   └─ T3 (data_hash)
 │   ├─ T4 (subgraph)            ─┐
 │   ├─ T5 (alerts)               │
 │   ├─ T6 (score)                │
 │   │   └─ T7 (triage)           │── M1..M4 milestones
 │   │       └─ T8 (plan)         │
 │   ├─ T9 (HITS)                 │
 │   ├─ T10 (Brandes)             │
 │   ├─ T11 (k-core)              │
 │   │   └─ T12 (insights)       ─┘
 │   └─ T13 (raw serializers)
 │       └─ T14 (GraphEngine facade)
 │           └─ T15 (integration tests)
 │               └─ T16 (BvAdapter swap) ── waits for companion plan cutover
 │                   └─ T17 (PmService wiring)
 │                       └─ T18 (MCP snapshot tests)
 │                           └─ T19 (install/docs cleanup)
```

---

## Task 0: Add `petgraph` workspace dependency

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/spur-pm/Cargo.toml`

- [ ] **Step 1: Add petgraph to workspace dependencies**

In root `Cargo.toml`, add to `[workspace.dependencies]`:

```toml
petgraph = { version = "0.6", default-features = false, features = ["stable_graph"] }
```

- [ ] **Step 2: Reference from spur-pm**

In `crates/spur-pm/Cargo.toml` `[dependencies]`:

```toml
petgraph = { workspace = true }
```

- [ ] **Step 3: Verify build**

Run: `cargo check -p spur-pm`
Expected: clean build, no errors.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/spur-pm/Cargo.toml Cargo.lock
git commit -m "deps: add petgraph 0.6 to workspace for graph engine"
```

---

## Task 1: `GraphSnapshot` value type + module skeleton

**Files:**
- Create: `crates/spur-pm/src/graph_engine/mod.rs`
- Create: `crates/spur-pm/src/graph_engine/snapshot.rs`
- Modify: `crates/spur-pm/src/lib.rs` (add `pub mod graph_engine;`)

- [ ] **Step 1: Write failing unit test for `GraphSnapshot::new`**

Create `crates/spur-pm/src/graph_engine/snapshot.rs`:

```rust
use chrono::{DateTime, Utc};
use petgraph::graph::{Graph, NodeIndex};
use petgraph::Directed;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    Blocks,
    ParentChild,
    ConditionalBlocks,
    WaitsFor,
    RelatedTo,
    Discovered,
    Unknown,
}

impl DependencyKind {
    pub fn parse(s: &str) -> Self {
        match s {
            "blocks" => Self::Blocks,
            "parent-child" => Self::ParentChild,
            "conditional-blocks" => Self::ConditionalBlocks,
            "waits-for" => Self::WaitsFor,
            "related-to" => Self::RelatedTo,
            "discovered" => Self::Discovered,
            _ => Self::Unknown,
        }
    }

    pub fn is_blocking(self) -> bool {
        matches!(
            self,
            Self::Blocks | Self::ParentChild | Self::ConditionalBlocks | Self::WaitsFor
        )
    }
}

#[derive(Debug, Clone)]
pub struct NodeData {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: i32,
    pub issue_type: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub due_at: Option<DateTime<Utc>>,
    pub content_hash: String,
}

#[derive(Debug, Clone)]
pub struct EdgeData {
    pub kind: DependencyKind,
}

pub struct GraphSnapshot {
    pub graph: Graph<NodeData, EdgeData, Directed>,
    pub by_id: HashMap<String, NodeIndex>,
    pub generated_at: DateTime<Utc>,
    pub data_hash: String,
    pub label_filter: Option<String>,
}

impl GraphSnapshot {
    pub fn new(label_filter: Option<String>) -> Self {
        Self {
            graph: Graph::new(),
            by_id: HashMap::new(),
            generated_at: Utc::now(),
            data_hash: String::new(),
            label_filter,
        }
    }

    pub fn add_node(&mut self, data: NodeData) -> NodeIndex {
        let id = data.id.clone();
        let ix = self.graph.add_node(data);
        self.by_id.insert(id, ix);
        ix
    }

    pub fn add_edge(&mut self, from: &str, to: &str, kind: DependencyKind) -> bool {
        let (Some(&a), Some(&b)) = (self.by_id.get(from), self.by_id.get(to)) else {
            return false;
        };
        self.graph.add_edge(a, b, EdgeData { kind });
        true
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str) -> NodeData {
        NodeData {
            id: id.to_string(),
            title: format!("Issue {id}"),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: format!("hash-{id}"),
        }
    }

    #[test]
    fn add_node_and_edge() {
        let mut s = GraphSnapshot::new(None);
        s.add_node(node("a"));
        s.add_node(node("b"));
        assert_eq!(s.node_count(), 2);
        assert!(s.add_edge("a", "b", DependencyKind::Blocks));
        assert_eq!(s.edge_count(), 1);
    }

    #[test]
    fn add_edge_with_unknown_node_returns_false() {
        let mut s = GraphSnapshot::new(None);
        s.add_node(node("a"));
        assert!(!s.add_edge("a", "missing", DependencyKind::Blocks));
        assert_eq!(s.edge_count(), 0);
    }

    #[test]
    fn dependency_kind_parsing_and_is_blocking() {
        assert!(DependencyKind::parse("blocks").is_blocking());
        assert!(DependencyKind::parse("parent-child").is_blocking());
        assert!(DependencyKind::parse("conditional-blocks").is_blocking());
        assert!(DependencyKind::parse("waits-for").is_blocking());
        assert!(!DependencyKind::parse("related-to").is_blocking());
        assert!(!DependencyKind::parse("discovered").is_blocking());
        assert!(!DependencyKind::parse("garbage").is_blocking());
    }
}
```

- [ ] **Step 2: Create module file**

Create `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
pub mod snapshot;

pub use snapshot::{DependencyKind, EdgeData, GraphSnapshot, NodeData};
```

- [ ] **Step 3: Wire into lib.rs**

Add to `crates/spur-pm/src/lib.rs` after the existing `pub mod` lines:

```rust
pub mod graph_engine;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-pm graph_engine::snapshot::tests --lib`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-pm/src/graph_engine/ crates/spur-pm/src/lib.rs
git commit -m "feat(spur-pm): add GraphSnapshot value type for graph engine"
```

---

## Task 2: `load_graph_snapshot` loader

**Files:**
- Modify: `crates/spur-pm/src/graph_engine/snapshot.rs`

**Pre-flight:** verify `beads_rust::storage::sqlite::SqliteStorage` exposes a way to list all issues with their dependencies. The companion plan adds `beads_rust = "0.2.1"` to `spur-pm/Cargo.toml`. Inspect `beads_rust`'s public API via `cargo doc -p beads_rust --open` or by reading its README on github. If a one-shot listing call exists, use it. If not, fall back to `storage.list_issues()` then per-issue `storage.get_issue(id)?.dependencies` — at our scale (406 issues) the per-issue path is acceptable; revisit if the workload grows.

- [ ] **Step 1: Write failing integration test using a fake storage**

Add to the bottom of `crates/spur-pm/src/graph_engine/snapshot.rs`:

```rust
#[cfg(test)]
mod loader_tests {
    use super::*;

    /// In-memory fake of just enough of beads_rust for unit testing
    /// the loader's data-shaping logic without spinning up SqliteStorage.
    pub struct FakeBeadsRows {
        pub issues: Vec<NodeData>,
        pub edges: Vec<(String, String, DependencyKind)>,
    }

    pub fn load_from_rows(rows: FakeBeadsRows, label_filter: Option<&str>) -> GraphSnapshot {
        let mut snap = GraphSnapshot::new(label_filter.map(|s| s.to_string()));
        for issue in rows.issues {
            if let Some(l) = label_filter {
                if !issue.labels.iter().any(|x| x == l) {
                    continue;
                }
            }
            snap.add_node(issue);
        }
        for (from, to, kind) in rows.edges {
            snap.add_edge(&from, &to, kind);
        }
        snap
    }

    fn node(id: &str, labels: &[&str]) -> NodeData {
        NodeData {
            id: id.to_string(),
            title: format!("Issue {id}"),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            assignee: None,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: format!("hash-{id}"),
        }
    }

    #[test]
    fn loader_filters_by_label() {
        let rows = FakeBeadsRows {
            issues: vec![
                node("a", &["spur:plan-id:P1"]),
                node("b", &[]),
                node("c", &["spur:plan-id:P1"]),
            ],
            edges: vec![("a".into(), "c".into(), DependencyKind::Blocks)],
        };
        let snap = load_from_rows(rows, Some("spur:plan-id:P1"));
        assert_eq!(snap.node_count(), 2);
        assert_eq!(snap.edge_count(), 1);
    }

    #[test]
    fn loader_skips_dangling_edges() {
        let rows = FakeBeadsRows {
            issues: vec![node("a", &["L"])],
            edges: vec![("a".into(), "missing".into(), DependencyKind::Blocks)],
        };
        let snap = load_from_rows(rows, Some("L"));
        assert_eq!(snap.node_count(), 1);
        assert_eq!(snap.edge_count(), 0);
    }
}
```

- [ ] **Step 2: Implement the real loader signature**

Append to `crates/spur-pm/src/graph_engine/snapshot.rs`:

```rust
/// Load the full graph snapshot from a SqliteStorage handle.
///
/// MUST be called inside a `BeadsCrateAdapter::read(|s| ...)` closure so the
/// connection-pool discipline holds. Synchronous; the caller is on a Tokio
/// blocking-pool thread.
pub fn load_graph_snapshot(
    storage: &beads_rust::storage::sqlite::SqliteStorage,
    label_filter: Option<&str>,
) -> anyhow::Result<GraphSnapshot> {
    use beads_rust::model::Issue;

    let mut snap = GraphSnapshot::new(label_filter.map(|s| s.to_string()));

    // List all issues. If beads_rust exposes a label-filtered listing, prefer it.
    let issues: Vec<Issue> = storage.list_issues()?;

    for issue in issues {
        if let Some(l) = label_filter {
            if !issue.labels.iter().any(|x| x == l) {
                continue;
            }
        }
        let data = NodeData {
            id: issue.id.clone(),
            title: issue.title.clone(),
            status: format!("{:?}", issue.status).to_lowercase(),
            priority: issue.priority as i32,
            issue_type: format!("{:?}", issue.issue_type).to_lowercase(),
            assignee: issue.assignee.clone(),
            labels: issue.labels.clone(),
            created_at: issue.created_at,
            updated_at: issue.updated_at,
            due_at: issue.due_at,
            content_hash: issue.content_hash.clone(),
        };
        snap.add_node(data);
    }

    // Re-walk to add edges; skips edges where either endpoint is filtered out.
    let issues_again: Vec<Issue> = storage.list_issues()?;
    for issue in issues_again {
        if !snap.by_id.contains_key(&issue.id) {
            continue;
        }
        for dep in issue.dependencies {
            let kind = DependencyKind::parse(&format!("{:?}", dep.dependency_type).to_lowercase());
            // Edge direction: blocker → blocked. In beads, `dep.depends_on_id`
            // is the blocker (the issue THIS one depends on / waits for), so
            // the source of the edge is `dep.depends_on_id`, not `issue.id`.
            snap.add_edge(&dep.depends_on_id, &issue.id, kind);
        }
    }

    Ok(snap)
}
```

> **Note on beads_rust API:** the exact field names (`issue.dependencies`, `dep.depends_on_id`, `dep.dependency_type`) and the `Issue::status`/`issue_type` enum-vs-string shape depend on `beads_rust` 0.2.1's actual API. If field names differ, adjust this loader; the loader is the only file in the graph engine that knows the schema. Verify before writing the test by running:
>
> ```bash
> cargo doc -p beads_rust --no-deps --open
> ```
>
> and reading the `Issue` and `Dependency` struct docs.

- [ ] **Step 3: Run loader tests**

Run: `cargo test -p spur-pm graph_engine::snapshot::loader_tests --lib`
Expected: 2 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/snapshot.rs
git commit -m "feat(graph-engine): add load_graph_snapshot loader + label filter"
```

---

## Task 3: `data_hash` deterministic computation

**Files:**
- Modify: `crates/spur-pm/src/graph_engine/snapshot.rs`

- [ ] **Step 1: Write failing tests for hash stability and sensitivity**

Append to `crates/spur-pm/src/graph_engine/snapshot.rs`:

```rust
#[cfg(test)]
mod hash_tests {
    use super::*;
    use super::loader_tests::{load_from_rows, FakeBeadsRows};

    fn node(id: &str, labels: &[&str], deps: &[&str]) -> (NodeData, Vec<(String, String, DependencyKind)>) {
        let n = NodeData {
            id: id.to_string(),
            title: format!("Issue {id}"),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            assignee: None,
            labels: labels.iter().map(|s| s.to_string()).collect(),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-05-05T00:00:00Z").unwrap().with_timezone(&Utc),
            updated_at: chrono::DateTime::parse_from_rfc3339("2026-05-05T00:00:00Z").unwrap().with_timezone(&Utc),
            due_at: None,
            content_hash: format!("hash-{id}"),
        };
        let edges = deps.iter().map(|d| (id.to_string(), d.to_string(), DependencyKind::Blocks)).collect();
        (n, edges)
    }

    fn snapshot_for(set: Vec<(NodeData, Vec<(String, String, DependencyKind)>)>) -> GraphSnapshot {
        let mut issues = vec![];
        let mut edges = vec![];
        for (n, e) in set {
            issues.push(n);
            edges.extend(e);
        }
        // Add target placeholders so edges resolve
        let mut all_ids: std::collections::HashSet<String> = issues.iter().map(|i| i.id.clone()).collect();
        for (_, t, _) in &edges {
            if !all_ids.contains(t) {
                let placeholder = NodeData {
                    id: t.clone(),
                    title: format!("Issue {t}"),
                    status: "open".into(),
                    priority: 2,
                    issue_type: "task".into(),
                    assignee: None,
                    labels: vec![],
                    created_at: chrono::DateTime::parse_from_rfc3339("2026-05-05T00:00:00Z").unwrap().with_timezone(&Utc),
                    updated_at: chrono::DateTime::parse_from_rfc3339("2026-05-05T00:00:00Z").unwrap().with_timezone(&Utc),
                    due_at: None,
                    content_hash: format!("hash-{t}"),
                };
                issues.push(placeholder);
                all_ids.insert(t.clone());
            }
        }
        let rows = FakeBeadsRows { issues, edges };
        let mut snap = load_from_rows(rows, None);
        snap.compute_data_hash();
        snap
    }

    #[test]
    fn data_hash_is_stable_under_reload() {
        let s1 = snapshot_for(vec![
            node("a", &["L"], &["b"]),
            node("b", &["L"], &[]),
        ]);
        let s2 = snapshot_for(vec![
            node("a", &["L"], &["b"]),
            node("b", &["L"], &[]),
        ]);
        assert_eq!(s1.data_hash, s2.data_hash);
        assert!(!s1.data_hash.is_empty());
    }

    #[test]
    fn data_hash_changes_on_label_change() {
        let s1 = snapshot_for(vec![node("a", &["L"], &[]), node("b", &[], &[])]);
        let s2 = snapshot_for(vec![node("a", &["L", "M"], &[]), node("b", &[], &[])]);
        assert_ne!(s1.data_hash, s2.data_hash);
    }

    #[test]
    fn data_hash_changes_on_dep_change() {
        let s1 = snapshot_for(vec![node("a", &[], &["b"]), node("b", &[], &[])]);
        let s2 = snapshot_for(vec![node("a", &[], &[]), node("b", &[], &[])]);
        assert_ne!(s1.data_hash, s2.data_hash);
    }

    #[test]
    fn data_hash_independent_of_insertion_order() {
        let s1 = snapshot_for(vec![node("a", &[], &["b"]), node("b", &[], &[])]);
        let s2 = snapshot_for(vec![node("b", &[], &[]), node("a", &[], &["b"])]);
        assert_eq!(s1.data_hash, s2.data_hash);
    }
}
```

- [ ] **Step 2: Implement `compute_data_hash`**

Append to `crates/spur-pm/src/graph_engine/snapshot.rs`:

```rust
use sha2::{Digest, Sha256};

impl GraphSnapshot {
    /// Compute a deterministic SHA256 hash of the snapshot's content.
    /// Stable across invocations on identical DB state; changes when any
    /// included node's content_hash, labels, or blocking dep set changes.
    pub fn compute_data_hash(&mut self) {
        // Build per-node digest, using sorted IDs for determinism.
        let mut ids: Vec<&str> = self.by_id.keys().map(|s| s.as_str()).collect();
        ids.sort_unstable();

        let mut top = Sha256::new();
        for id in ids {
            let &ix = self.by_id.get(id).unwrap();
            let n = &self.graph[ix];

            // Gather blocking out-edges, sorted by target id.
            let mut deps: Vec<&str> = self
                .graph
                .edges(ix)
                .filter(|e| e.weight().kind.is_blocking())
                .map(|e| self.graph[e.target()].id.as_str())
                .collect();
            deps.sort_unstable();

            let mut labels: Vec<&str> = n.labels.iter().map(|s| s.as_str()).collect();
            labels.sort_unstable();

            let mut h = Sha256::new();
            h.update(n.id.as_bytes());
            h.update(b"\x1f");
            h.update(n.content_hash.as_bytes());
            h.update(b"\x1f");
            h.update(labels.join(",").as_bytes());
            h.update(b"\x1f");
            h.update(deps.join(",").as_bytes());
            top.update(h.finalize());
        }

        self.data_hash = format!("{:x}", top.finalize());
    }
}
```

> Need an additional import at the top of the file:
> ```rust
> use petgraph::visit::EdgeRef;
> ```

- [ ] **Step 3: Run hash tests**

Run: `cargo test -p spur-pm graph_engine::snapshot::hash_tests --lib`
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/snapshot.rs
git commit -m "feat(graph-engine): deterministic data_hash with property tests"
```

---

## Task 4: `compute_subgraph` (M1 — simplest)

**Files:**
- Create: `crates/spur-pm/src/graph_engine/subgraph.rs`
- Modify: `crates/spur-pm/src/graph_engine/mod.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/spur-pm/src/graph_engine/subgraph.rs`:

```rust
use crate::graph::{AdjacencyData, DependencyGraph, GraphEdge, GraphNode};
use crate::graph_engine::snapshot::{DependencyKind, GraphSnapshot};
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::{HashSet, VecDeque};

#[derive(Debug, Clone, Copy)]
pub enum GraphFormat {
    Json,
    Dot,
    Mermaid,
}

impl GraphFormat {
    pub fn parse(s: Option<&str>) -> Self {
        match s.unwrap_or("json") {
            "dot" => Self::Dot,
            "mermaid" => Self::Mermaid,
            _ => Self::Json,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Dot => "dot",
            Self::Mermaid => "mermaid",
        }
    }
}

#[derive(Debug, Clone)]
pub enum SubgraphRoot<'a> {
    Issue(&'a str),
    AllIssues, // The label_filter on the snapshot already selects these.
}

pub fn compute_subgraph(
    snap: &GraphSnapshot,
    root: SubgraphRoot,
    depth: Option<u32>,
    format: GraphFormat,
) -> DependencyGraph {
    let included: HashSet<NodeIndex> = match root {
        SubgraphRoot::AllIssues => snap.graph.node_indices().collect(),
        SubgraphRoot::Issue(id) => bfs_bidirectional(snap, id, depth.unwrap_or(2)),
    };

    let mut nodes: Vec<GraphNode> = included
        .iter()
        .map(|&ix| {
            let n = &snap.graph[ix];
            GraphNode {
                id: n.id.clone(),
                title: Some(n.title.clone()),
                status: Some(n.status.clone()),
                priority: Some(n.priority),
                labels: n.labels.clone(),
                pagerank: None,
            }
        })
        .collect();
    nodes.sort_by(|a, b| a.id.cmp(&b.id));

    let mut edges: Vec<GraphEdge> = Vec::new();
    for ix in &included {
        for e in snap.graph.edges(*ix) {
            if !included.contains(&e.target()) {
                continue;
            }
            edges.push(GraphEdge {
                from: snap.graph[*ix].id.clone(),
                to: snap.graph[e.target()].id.clone(),
                edge_type: Some(format!("{:?}", e.weight().kind).to_lowercase()),
            });
        }
    }
    edges.sort_by(|a, b| (a.from.as_str(), a.to.as_str()).cmp(&(b.from.as_str(), b.to.as_str())));

    let node_count = nodes.len();
    let edge_count = edges.len();

    match format {
        GraphFormat::Json => DependencyGraph {
            format: Some("json".into()),
            graph: None,
            nodes: node_count,
            edges: edge_count,
            data_hash: Some(snap.data_hash.clone()),
            adjacency: Some(AdjacencyData {
                nodes,
                edges: if edges.is_empty() { None } else { Some(edges) },
            }),
            raw: serde_json::Value::Null,
        },
        GraphFormat::Dot => DependencyGraph {
            format: Some("dot".into()),
            graph: Some(render_dot(&nodes, &edges)),
            nodes: node_count,
            edges: edge_count,
            data_hash: Some(snap.data_hash.clone()),
            adjacency: None,
            raw: serde_json::Value::Null,
        },
        GraphFormat::Mermaid => DependencyGraph {
            format: Some("mermaid".into()),
            graph: Some(render_mermaid(&nodes, &edges)),
            nodes: node_count,
            edges: edge_count,
            data_hash: Some(snap.data_hash.clone()),
            adjacency: None,
            raw: serde_json::Value::Null,
        },
    }
}

fn bfs_bidirectional(snap: &GraphSnapshot, root_id: &str, depth: u32) -> HashSet<NodeIndex> {
    let mut included = HashSet::new();
    let Some(&root) = snap.by_id.get(root_id) else {
        return included;
    };
    let mut queue: VecDeque<(NodeIndex, u32)> = VecDeque::new();
    queue.push_back((root, 0));
    included.insert(root);

    while let Some((ix, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        for e in snap.graph.edges(ix) {
            if included.insert(e.target()) {
                queue.push_back((e.target(), d + 1));
            }
        }
        for e in snap.graph.edges_directed(ix, petgraph::Direction::Incoming) {
            if included.insert(e.source()) {
                queue.push_back((e.source(), d + 1));
            }
        }
    }
    included
}

fn render_dot(nodes: &[GraphNode], edges: &[GraphEdge]) -> String {
    let mut out = String::from("digraph G {\n  rankdir=LR;\n");
    for n in nodes {
        let title = n.title.as_deref().unwrap_or("").replace('"', "\\\"");
        out.push_str(&format!("  \"{}\" [label=\"{}\\n{}\"];\n", n.id, n.id, title));
    }
    for e in edges {
        out.push_str(&format!("  \"{}\" -> \"{}\";\n", e.from, e.to));
    }
    out.push_str("}\n");
    out
}

fn render_mermaid(nodes: &[GraphNode], edges: &[GraphEdge]) -> String {
    let mut out = String::from("graph TD\n");
    for n in nodes {
        let title = n.title.as_deref().unwrap_or("").replace('"', "'");
        out.push_str(&format!("  {}[\"{}: {}\"]\n", n.id.replace('-', "_"), n.id, title));
    }
    for e in edges {
        out.push_str(&format!("  {} --> {}\n", e.from.replace('-', "_"), e.to.replace('-', "_")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::snapshot::loader_tests::{load_from_rows, FakeBeadsRows};
    use crate::graph_engine::snapshot::NodeData;
    use chrono::Utc;

    fn n(id: &str) -> NodeData {
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: "h".into(),
        }
    }

    fn snap(rows: Vec<(NodeData, Vec<(String, String, DependencyKind)>)>) -> GraphSnapshot {
        let mut issues = Vec::new();
        let mut edges = Vec::new();
        for (i, e) in rows {
            issues.push(i);
            edges.extend(e);
        }
        let r = FakeBeadsRows { issues, edges };
        let mut s = load_from_rows(r, None);
        s.compute_data_hash();
        s
    }

    #[test]
    fn json_format_returns_adjacency() {
        let s = snap(vec![(n("a"), vec![("a".into(), "b".into(), DependencyKind::Blocks)]), (n("b"), vec![])]);
        let g = compute_subgraph(&s, SubgraphRoot::AllIssues, None, GraphFormat::Json);
        assert_eq!(g.format.as_deref(), Some("json"));
        assert!(g.graph.is_none());
        let adj = g.adjacency.unwrap();
        assert_eq!(adj.nodes.len(), 2);
        assert_eq!(adj.edges.unwrap().len(), 1);
    }

    #[test]
    fn dot_format_renders_deterministic_string() {
        let s = snap(vec![(n("a"), vec![("a".into(), "b".into(), DependencyKind::Blocks)]), (n("b"), vec![])]);
        let g1 = compute_subgraph(&s, SubgraphRoot::AllIssues, None, GraphFormat::Dot);
        let g2 = compute_subgraph(&s, SubgraphRoot::AllIssues, None, GraphFormat::Dot);
        assert_eq!(g1.graph, g2.graph);
        assert!(g1.graph.as_ref().unwrap().contains("\"a\" -> \"b\""));
    }

    #[test]
    fn mermaid_format_replaces_dashes_in_node_ids() {
        let s = snap(vec![(n("bd-1"), vec![("bd-1".into(), "bd-2".into(), DependencyKind::Blocks)]), (n("bd-2"), vec![])]);
        let g = compute_subgraph(&s, SubgraphRoot::AllIssues, None, GraphFormat::Mermaid);
        let body = g.graph.unwrap();
        assert!(body.contains("bd_1[\"bd-1: Tbd-1\"]"));
        assert!(body.contains("bd_1 --> bd_2"));
    }

    #[test]
    fn issue_root_with_depth_1_includes_neighbors_only() {
        // a -> b -> c; root b, depth 1 → {a, b, c}
        let s = snap(vec![
            (n("a"), vec![("a".into(), "b".into(), DependencyKind::Blocks)]),
            (n("b"), vec![("b".into(), "c".into(), DependencyKind::Blocks)]),
            (n("c"), vec![]),
        ]);
        let g = compute_subgraph(&s, SubgraphRoot::Issue("b"), Some(1), GraphFormat::Json);
        let adj = g.adjacency.unwrap();
        let ids: Vec<_> = adj.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn missing_root_yields_empty_subgraph() {
        let s = snap(vec![(n("a"), vec![])]);
        let g = compute_subgraph(&s, SubgraphRoot::Issue("missing"), Some(2), GraphFormat::Json);
        assert_eq!(g.nodes, 0);
        assert_eq!(g.edges, 0);
    }
}
```

- [ ] **Step 2: Wire submodule**

In `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
pub mod snapshot;
pub mod subgraph;

pub use snapshot::{DependencyKind, EdgeData, GraphSnapshot, NodeData};
pub use subgraph::{compute_subgraph, GraphFormat, SubgraphRoot};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-pm graph_engine::subgraph::tests --lib`
Expected: 5 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/subgraph.rs crates/spur-pm/src/graph_engine/mod.rs
git commit -m "feat(graph-engine): compute_subgraph + json/dot/mermaid formats"
```

---

## Task 5: `compute_alerts` (M1)

**Files:**
- Create: `crates/spur-pm/src/graph_engine/alerts.rs`
- Modify: `crates/spur-pm/src/graph_engine/mod.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/spur-pm/src/graph_engine/alerts.rs`:

```rust
use crate::graph::{Alert, AlertReport, AlertSummary};
use crate::graph_engine::snapshot::GraphSnapshot;
use chrono::{DateTime, Utc};
use petgraph::algo::tarjan_scc;
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct AlertConfig {
    pub stale_threshold_days: i64,
    pub cascade_threshold: usize,
    pub now: DateTime<Utc>,
}

impl Default for AlertConfig {
    fn default() -> Self {
        Self {
            stale_threshold_days: 14,
            cascade_threshold: 5,
            now: Utc::now(),
        }
    }
}

pub fn compute_alerts(snap: &GraphSnapshot, cfg: &AlertConfig) -> AlertReport {
    let mut alerts: Vec<Alert> = Vec::new();
    let cycle_set: HashSet<String> = tarjan_scc(&snap.graph)
        .into_iter()
        .filter(|c| c.len() > 1)
        .flat_map(|c| c.into_iter().map(|ix| snap.graph[ix].id.clone()))
        .collect();

    let unblocks_count = transitive_unblocks_per_node(snap);

    for ix in snap.graph.node_indices() {
        let n = &snap.graph[ix];
        let is_open = matches!(n.status.as_str(), "open" | "in_progress");

        // stale
        if is_open {
            let age = (cfg.now - n.updated_at).num_days();
            if age > cfg.stale_threshold_days {
                alerts.push(Alert {
                    alert_type: "stale".into(),
                    severity: "warning".into(),
                    message: format!(
                        "Issue {} has not been updated in {} days",
                        n.id, age
                    ),
                    issue_id: Some(n.id.clone()),
                    issue_ids: vec![],
                    label: None,
                    details: vec![],
                    baseline_value: Some(cfg.stale_threshold_days as f64),
                    current_value: Some(age as f64),
                    delta: Some((age - cfg.stale_threshold_days) as f64),
                });
            }
        }

        // cycle
        if cycle_set.contains(&n.id) {
            alerts.push(Alert {
                alert_type: "cycle".into(),
                severity: "critical".into(),
                message: format!("Issue {} is in a dependency cycle", n.id),
                issue_id: Some(n.id.clone()),
                issue_ids: vec![],
                label: None,
                details: vec![],
                baseline_value: None,
                current_value: None,
                delta: None,
            });
        }

        // cascade
        if is_open {
            let count = unblocks_count.get(&n.id).copied().unwrap_or(0);
            if count >= cfg.cascade_threshold {
                alerts.push(Alert {
                    alert_type: "cascade".into(),
                    severity: "warning".into(),
                    message: format!("Issue {} blocks {} downstream items", n.id, count),
                    issue_id: Some(n.id.clone()),
                    issue_ids: vec![],
                    label: None,
                    details: vec![],
                    baseline_value: Some(cfg.cascade_threshold as f64),
                    current_value: Some(count as f64),
                    delta: None,
                });
            }
        }

        // priority_inversion
        for e in snap.graph.edges(ix) {
            let blocked = &snap.graph[e.target()];
            if blocked.priority < n.priority {
                alerts.push(Alert {
                    alert_type: "priority_inversion".into(),
                    severity: "warning".into(),
                    message: format!(
                        "Issue {} (P{}) blocks higher-priority issue {} (P{})",
                        n.id, n.priority, blocked.id, blocked.priority
                    ),
                    issue_id: Some(n.id.clone()),
                    issue_ids: vec![n.id.clone(), blocked.id.clone()],
                    label: None,
                    details: vec![],
                    baseline_value: None,
                    current_value: None,
                    delta: None,
                });
            }
        }

        // orphan_high_priority
        if is_open
            && n.priority <= 1
            && snap.graph.edges(ix).count() == 0
            && snap.graph.edges_directed(ix, petgraph::Direction::Incoming).count() == 0
        {
            alerts.push(Alert {
                alert_type: "orphan_high_priority".into(),
                severity: "info".into(),
                message: format!("High-priority issue {} has no dependencies", n.id),
                issue_id: Some(n.id.clone()),
                issue_ids: vec![],
                label: None,
                details: vec![],
                baseline_value: None,
                current_value: None,
                delta: None,
            });
        }
    }

    let summary = AlertSummary {
        total: alerts.len(),
        critical: alerts.iter().filter(|a| a.severity == "critical").count(),
        warning: alerts.iter().filter(|a| a.severity == "warning").count(),
        info: alerts.iter().filter(|a| a.severity == "info").count(),
    };

    AlertReport {
        generated_at: Some(snap.generated_at.to_rfc3339()),
        data_hash: Some(snap.data_hash.clone()),
        alerts,
        summary: Some(summary),
        usage_hints: vec![
            "Filter by severity: jq '.alerts[] | select(.severity==\"critical\")'".into(),
            "Group by type: jq '.alerts | group_by(.type)'".into(),
        ],
        raw: serde_json::Value::Null,
    }
}

fn transitive_unblocks_per_node(snap: &GraphSnapshot) -> HashMap<String, usize> {
    use petgraph::visit::Bfs;
    let mut out = HashMap::new();
    for ix in snap.graph.node_indices() {
        let mut bfs = Bfs::new(&snap.graph, ix);
        let mut count = 0usize;
        while let Some(_) = bfs.next(&snap.graph) {
            count += 1;
        }
        // exclude self
        out.insert(snap.graph[ix].id.clone(), count.saturating_sub(1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::snapshot::loader_tests::{load_from_rows, FakeBeadsRows};
    use crate::graph_engine::snapshot::{DependencyKind, NodeData};
    use chrono::Duration;

    fn n(id: &str, status: &str, priority: i32, updated_days_ago: i64) -> NodeData {
        let now = Utc::now();
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: status.into(),
            priority,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: now,
            updated_at: now - Duration::days(updated_days_ago),
            due_at: None,
            content_hash: "h".into(),
        }
    }

    fn snap_with(nodes: Vec<NodeData>, edges: Vec<(&str, &str, DependencyKind)>) -> GraphSnapshot {
        let r = FakeBeadsRows {
            issues: nodes,
            edges: edges.into_iter().map(|(a, b, k)| (a.into(), b.into(), k)).collect(),
        };
        let mut s = load_from_rows(r, None);
        s.compute_data_hash();
        s
    }

    #[test]
    fn stale_alert_fires_above_threshold() {
        let s = snap_with(vec![n("a", "open", 2, 30)], vec![]);
        let cfg = AlertConfig { now: Utc::now(), ..AlertConfig::default() };
        let r = compute_alerts(&s, &cfg);
        assert!(r.alerts.iter().any(|a| a.alert_type == "stale" && a.issue_id.as_deref() == Some("a")));
    }

    #[test]
    fn closed_issues_do_not_trigger_stale() {
        let s = snap_with(vec![n("a", "closed", 2, 90)], vec![]);
        let cfg = AlertConfig { now: Utc::now(), ..AlertConfig::default() };
        let r = compute_alerts(&s, &cfg);
        assert!(!r.alerts.iter().any(|a| a.alert_type == "stale"));
    }

    #[test]
    fn cycle_alert_fires_critical() {
        let s = snap_with(
            vec![n("a", "open", 2, 0), n("b", "open", 2, 0)],
            vec![("a", "b", DependencyKind::Blocks), ("b", "a", DependencyKind::Blocks)],
        );
        let cfg = AlertConfig::default();
        let r = compute_alerts(&s, &cfg);
        let cycle: Vec<_> = r.alerts.iter().filter(|a| a.alert_type == "cycle").collect();
        assert_eq!(cycle.len(), 2);
        assert!(cycle.iter().all(|a| a.severity == "critical"));
    }

    #[test]
    fn priority_inversion_fires() {
        let mut a = n("a", "open", 3, 0);
        let b = n("b", "open", 0, 0);
        let s = snap_with(vec![a, b], vec![("a", "b", DependencyKind::Blocks)]);
        let cfg = AlertConfig::default();
        let r = compute_alerts(&s, &cfg);
        assert!(r.alerts.iter().any(|al| al.alert_type == "priority_inversion"));
    }

    #[test]
    fn summary_counts_by_severity() {
        let s = snap_with(
            vec![
                n("a", "open", 2, 30), // stale → warning
                n("b", "open", 2, 0),
                n("c", "open", 2, 0),
            ],
            vec![("b", "c", DependencyKind::Blocks), ("c", "b", DependencyKind::Blocks)], // cycle → critical x2
        );
        let cfg = AlertConfig::default();
        let r = compute_alerts(&s, &cfg);
        let sm = r.summary.unwrap();
        assert_eq!(sm.critical, 2);
        assert!(sm.warning >= 1);
        assert_eq!(sm.total, sm.critical + sm.warning + sm.info);
    }
}
```

- [ ] **Step 2: Wire submodule**

In `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
pub mod alerts;
// ... existing
pub use alerts::{compute_alerts, AlertConfig};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-pm graph_engine::alerts::tests --lib`
Expected: 5 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/alerts.rs crates/spur-pm/src/graph_engine/mod.rs
git commit -m "feat(graph-engine): compute_alerts (stale/cycle/cascade/priority/orphan)"
```

---

## Task 6: `score.rs` — `ScoreConfig` + scoring formulas

**Files:**
- Create: `crates/spur-pm/src/graph_engine/score.rs`
- Modify: `crates/spur-pm/src/graph_engine/mod.rs`

- [ ] **Step 1: Write failing tests for is_actionable + score**

Create `crates/spur-pm/src/graph_engine/score.rs`:

```rust
use crate::graph_engine::snapshot::{DependencyKind, GraphSnapshot};
use chrono::{DateTime, Utc};
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct ScoreConfig {
    pub w_priority: f64,
    pub w_unblocks: f64,
    pub w_actionable: f64,
    pub w_freshness: f64,
    pub w_age_penalty: f64,
    pub quick_win_percentile: f64,
    pub now: DateTime<Utc>,
}

impl Default for ScoreConfig {
    fn default() -> Self {
        Self {
            w_priority: 0.30,
            w_unblocks: 0.25,
            w_actionable: 0.20,
            w_freshness: 0.15,
            w_age_penalty: 0.10,
            quick_win_percentile: 0.75,
            now: Utc::now(),
        }
    }
}

pub fn is_actionable(snap: &GraphSnapshot, ix: NodeIndex) -> bool {
    let n = &snap.graph[ix];
    if !matches!(n.status.as_str(), "open" | "in_progress") {
        return false;
    }
    for e in snap.graph.edges_directed(ix, petgraph::Direction::Incoming) {
        if e.weight().kind.is_blocking() && snap.graph[e.source()].status != "closed" {
            return false;
        }
    }
    true
}

/// Returns the count of nodes (excluding self) reachable via blocking out-edges.
pub fn transitive_unblocks(snap: &GraphSnapshot, ix: NodeIndex) -> usize {
    let mut visited: HashSet<NodeIndex> = HashSet::new();
    let mut stack: Vec<NodeIndex> = vec![ix];
    while let Some(cur) = stack.pop() {
        for e in snap.graph.edges(cur) {
            if !e.weight().kind.is_blocking() {
                continue;
            }
            if visited.insert(e.target()) {
                stack.push(e.target());
            }
        }
    }
    visited.len()
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

#[derive(Debug, Clone)]
pub struct ScoreBreakdown {
    pub priority_component: f64,
    pub unblocks_component: f64,
    pub actionable_component: f64,
    pub freshness_component: f64,
    pub age_penalty_component: f64,
    pub raw: f64,
    pub normalized: f64,
}

pub fn score_node(
    snap: &GraphSnapshot,
    ix: NodeIndex,
    cfg: &ScoreConfig,
) -> ScoreBreakdown {
    let n = &snap.graph[ix];
    let priority_component = ((5 - n.priority).max(0) as f64) / 5.0;
    let unblocks = transitive_unblocks(snap, ix) as f64;
    let unblocks_component = (1.0 + unblocks).log10();
    let actionable_component = if is_actionable(snap, ix) { 1.0 } else { 0.0 };
    let days_since_update = (cfg.now - n.updated_at).num_days() as f64;
    let freshness_component = sigmoid((30.0 - days_since_update) / 30.0);
    let age_days = (cfg.now - n.created_at).num_days() as f64;
    let age_penalty_component = sigmoid((age_days - 60.0) / 30.0); // higher = older

    let raw = cfg.w_priority * priority_component
        + cfg.w_unblocks * unblocks_component
        + cfg.w_actionable * actionable_component
        + cfg.w_freshness * freshness_component
        - cfg.w_age_penalty * age_penalty_component;

    ScoreBreakdown {
        priority_component,
        unblocks_component,
        actionable_component,
        freshness_component,
        age_penalty_component,
        raw,
        normalized: 0.0, // filled in by score_all
    }
}

pub fn score_all(snap: &GraphSnapshot, cfg: &ScoreConfig) -> HashMap<NodeIndex, ScoreBreakdown> {
    let mut out: HashMap<NodeIndex, ScoreBreakdown> = HashMap::new();
    let mut max_raw = 0.0_f64;
    for ix in snap.graph.node_indices() {
        let b = score_node(snap, ix, cfg);
        if b.raw > max_raw {
            max_raw = b.raw;
        }
        out.insert(ix, b);
    }
    if max_raw > 0.0 {
        for b in out.values_mut() {
            b.normalized = (b.raw / max_raw).clamp(0.0, 1.0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::snapshot::loader_tests::{load_from_rows, FakeBeadsRows};
    use crate::graph_engine::snapshot::NodeData;

    fn node(id: &str, status: &str, priority: i32) -> NodeData {
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: status.into(),
            priority,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now() - chrono::Duration::days(10),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: "h".into(),
        }
    }

    fn snap_of(nodes: Vec<NodeData>, edges: Vec<(&str, &str, DependencyKind)>) -> GraphSnapshot {
        let r = FakeBeadsRows {
            issues: nodes,
            edges: edges.into_iter().map(|(a, b, k)| (a.into(), b.into(), k)).collect(),
        };
        let mut s = load_from_rows(r, None);
        s.compute_data_hash();
        s
    }

    #[test]
    fn open_with_no_blockers_is_actionable() {
        let s = snap_of(vec![node("a", "open", 2)], vec![]);
        let ix = s.by_id["a"];
        assert!(is_actionable(&s, ix));
    }

    #[test]
    fn open_with_open_blocker_is_not_actionable() {
        let s = snap_of(
            vec![node("a", "open", 2), node("b", "open", 2)],
            vec![("a", "b", DependencyKind::Blocks)],
        );
        // b is blocked by a (a -> b means a must complete first)
        let b_ix = s.by_id["b"];
        assert!(!is_actionable(&s, b_ix));
    }

    #[test]
    fn closed_blocker_does_not_block() {
        let s = snap_of(
            vec![node("a", "closed", 2), node("b", "open", 2)],
            vec![("a", "b", DependencyKind::Blocks)],
        );
        let b_ix = s.by_id["b"];
        assert!(is_actionable(&s, b_ix));
    }

    #[test]
    fn transitive_unblocks_counts_chain() {
        // a -> b -> c
        let s = snap_of(
            vec![node("a", "open", 2), node("b", "open", 2), node("c", "open", 2)],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
            ],
        );
        let a_ix = s.by_id["a"];
        assert_eq!(transitive_unblocks(&s, a_ix), 2); // {b, c}
    }

    #[test]
    fn p0_scores_higher_than_p4() {
        let s = snap_of(
            vec![node("h", "open", 0), node("l", "open", 4)],
            vec![],
        );
        let cfg = ScoreConfig::default();
        let scores = score_all(&s, &cfg);
        assert!(scores[&s.by_id["h"]].raw > scores[&s.by_id["l"]].raw);
    }

    #[test]
    fn normalization_caps_at_one() {
        let s = snap_of(vec![node("a", "open", 0)], vec![]);
        let cfg = ScoreConfig::default();
        let scores = score_all(&s, &cfg);
        assert!((scores[&s.by_id["a"]].normalized - 1.0).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: Wire submodule**

In `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
pub mod score;
pub use score::{is_actionable, score_all, score_node, transitive_unblocks, ScoreBreakdown, ScoreConfig};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-pm graph_engine::score::tests --lib`
Expected: 6 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/score.rs crates/spur-pm/src/graph_engine/mod.rs
git commit -m "feat(graph-engine): ScoreConfig + is_actionable + transitive_unblocks + score_all"
```

---

## Task 7: `compute_triage` (M2)

**Files:**
- Create: `crates/spur-pm/src/graph_engine/triage.rs`
- Modify: `crates/spur-pm/src/graph_engine/mod.rs`

- [ ] **Step 1: Write failing tests + implementation**

Create `crates/spur-pm/src/graph_engine/triage.rs`:

```rust
use crate::graph::{
    BlockerInfo, GraphHealth, HealthCounts, ProjectHealth, QuickRef, QuickWin, Recommendation,
    TopPick, TriageReport, TriageResult,
};
use crate::graph_engine::score::{is_actionable, score_all, transitive_unblocks, ScoreBreakdown, ScoreConfig};
use crate::graph_engine::snapshot::{DependencyKind, GraphSnapshot};
use petgraph::algo::{is_cyclic_directed, tarjan_scc};
use petgraph::visit::EdgeRef;
use std::collections::HashMap;

pub fn compute_triage(snap: &GraphSnapshot, cfg: &ScoreConfig) -> TriageReport {
    let scores = score_all(snap, cfg);

    let mut ranked: Vec<(petgraph::graph::NodeIndex, &ScoreBreakdown)> =
        scores.iter().map(|(k, v)| (*k, v)).collect();
    ranked.sort_by(|a, b| {
        b.1.normalized
            .partial_cmp(&a.1.normalized)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                let pa = snap.graph[a.0].priority;
                let pb = snap.graph[b.0].priority;
                pa.cmp(&pb)
            })
            .then_with(|| {
                let ua = transitive_unblocks(snap, a.0);
                let ub = transitive_unblocks(snap, b.0);
                ub.cmp(&ua)
            })
            .then_with(|| snap.graph[a.0].id.cmp(&snap.graph[b.0].id))
    });

    let top_picks: Vec<TopPick> = ranked
        .iter()
        .take(5)
        .map(|(ix, b)| TopPick {
            id: snap.graph[*ix].id.clone(),
            title: snap.graph[*ix].title.clone(),
            score: b.normalized,
            reasons: reasons_for(snap, *ix, b),
            unblocks: transitive_unblocks(snap, *ix),
        })
        .collect();

    let recommendations: Vec<Recommendation> = ranked
        .iter()
        .take(20)
        .map(|(ix, b)| {
            let n = &snap.graph[*ix];
            Recommendation {
                id: n.id.clone(),
                title: n.title.clone(),
                issue_type: Some(n.issue_type.clone()),
                status: Some(n.status.clone()),
                priority: Some(n.priority),
                labels: n.labels.clone(),
                score: b.normalized,
                breakdown: serde_json::json!({
                    "priority": b.priority_component,
                    "unblocks": b.unblocks_component,
                    "actionable": b.actionable_component,
                    "freshness": b.freshness_component,
                    "age_penalty": b.age_penalty_component,
                    "raw": b.raw,
                }),
                action: Some(if is_actionable(snap, *ix) {
                    "start".into()
                } else {
                    "wait".into()
                }),
                reasons: reasons_for(snap, *ix, b),
                unblocks_ids: blocking_targets(snap, *ix),
                blocked_by: blocking_sources(snap, *ix),
            }
        })
        .collect();

    let qw_threshold = quick_win_threshold(&scores, cfg.quick_win_percentile);
    let quick_wins: Vec<QuickWin> = ranked
        .iter()
        .filter(|(ix, b)| {
            let n = &snap.graph[*ix];
            is_actionable(snap, *ix) && n.priority >= 2 && b.normalized >= qw_threshold
        })
        .take(10)
        .map(|(ix, b)| QuickWin {
            id: snap.graph[*ix].id.clone(),
            title: snap.graph[*ix].title.clone(),
            score: b.normalized,
            reason: Some(reasons_for(snap, *ix, b).join("; ")),
            unblocks_ids: blocking_targets(snap, *ix),
        })
        .collect();

    let blockers_to_clear: Vec<BlockerInfo> = snap
        .graph
        .node_indices()
        .filter_map(|ix| {
            let n = &snap.graph[ix];
            if matches!(n.status.as_str(), "closed") {
                return None;
            }
            let unblocks = transitive_unblocks(snap, ix);
            if unblocks == 0 {
                return None;
            }
            Some(BlockerInfo {
                id: n.id.clone(),
                title: n.title.clone(),
                unblocks_count: unblocks,
                unblocks_ids: blocking_targets(snap, ix),
                actionable: is_actionable(snap, ix),
                blocked_by: blocking_sources(snap, ix),
            })
        })
        .collect();

    let mut blockers_sorted = blockers_to_clear;
    blockers_sorted.sort_by(|a, b| b.unblocks_count.cmp(&a.unblocks_count).then_with(|| a.id.cmp(&b.id)));
    blockers_sorted.truncate(10);

    let project_health = compute_project_health(snap);

    TriageReport {
        generated_at: Some(snap.generated_at.to_rfc3339()),
        data_hash: Some(snap.data_hash.clone()),
        triage: TriageResult {
            meta: serde_json::json!({"engine": "spur-graph-engine", "version": "1"}),
            quick_ref: QuickRef {
                open_count: project_health.counts.open,
                actionable_count: project_health.counts.actionable,
                blocked_count: project_health.counts.blocked,
                in_progress_count: snap
                    .graph
                    .node_indices()
                    .filter(|&i| snap.graph[i].status == "in_progress")
                    .count(),
                top_picks,
            },
            recommendations,
            quick_wins,
            blockers_to_clear: blockers_sorted,
            project_health,
            alerts: vec![], // populated by composite caller if desired; left empty by triage alone
            commands: serde_json::Value::Null,
        },
        usage_hints: vec![
            "jq '.triage.quick_ref.top_picks'".into(),
            "jq '.triage.recommendations[0:3]'".into(),
        ],
        raw: serde_json::Value::Null,
    }
}

fn reasons_for(
    snap: &GraphSnapshot,
    ix: petgraph::graph::NodeIndex,
    b: &ScoreBreakdown,
) -> Vec<String> {
    let n = &snap.graph[ix];
    let mut r = Vec::new();
    if n.priority <= 1 {
        r.push(format!("P{} priority", n.priority));
    }
    if b.unblocks_component > 0.0 {
        r.push(format!(
            "unblocks {} downstream",
            transitive_unblocks(snap, ix)
        ));
    }
    if b.actionable_component > 0.0 {
        r.push("actionable now".into());
    }
    if b.freshness_component > 0.7 {
        r.push("recently updated".into());
    }
    if b.age_penalty_component > 0.7 {
        r.push("aging — consider closing or reprioritizing".into());
    }
    r
}

fn blocking_targets(snap: &GraphSnapshot, ix: petgraph::graph::NodeIndex) -> Vec<String> {
    let mut v: Vec<String> = snap
        .graph
        .edges(ix)
        .filter(|e| e.weight().kind.is_blocking())
        .map(|e| snap.graph[e.target()].id.clone())
        .collect();
    v.sort();
    v
}

fn blocking_sources(snap: &GraphSnapshot, ix: petgraph::graph::NodeIndex) -> Vec<String> {
    let mut v: Vec<String> = snap
        .graph
        .edges_directed(ix, petgraph::Direction::Incoming)
        .filter(|e| e.weight().kind.is_blocking())
        .map(|e| snap.graph[e.source()].id.clone())
        .collect();
    v.sort();
    v
}

fn quick_win_threshold(
    scores: &HashMap<petgraph::graph::NodeIndex, ScoreBreakdown>,
    percentile: f64,
) -> f64 {
    if scores.is_empty() {
        return 0.0;
    }
    let mut vals: Vec<f64> = scores.values().map(|b| b.normalized).collect();
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let idx = ((vals.len() as f64) * percentile).floor() as usize;
    vals[idx.min(vals.len() - 1)]
}

fn compute_project_health(snap: &GraphSnapshot) -> ProjectHealth {
    let mut counts = HealthCounts::default();
    for ix in snap.graph.node_indices() {
        let n = &snap.graph[ix];
        counts.total += 1;
        match n.status.as_str() {
            "closed" => counts.closed += 1,
            _ => {
                counts.open += 1;
                if !is_actionable(snap, ix) {
                    counts.blocked += 1;
                } else {
                    counts.actionable += 1;
                }
            }
        }
    }

    let scc = tarjan_scc(&snap.graph);
    let cycle_count = scc.iter().filter(|c| c.len() > 1).count();
    let nc = counts.total;
    let ec = snap.graph.edge_count();
    let density = if nc > 1 {
        (2.0 * ec as f64) / (nc as f64 * (nc as f64 - 1.0))
    } else {
        0.0
    };
    let graph = GraphHealth {
        node_count: nc,
        edge_count: ec,
        density,
        has_cycles: is_cyclic_directed(&snap.graph),
        cycle_count,
    };

    ProjectHealth {
        counts,
        graph,
        velocity: None,
        staleness: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::snapshot::loader_tests::{load_from_rows, FakeBeadsRows};
    use crate::graph_engine::snapshot::NodeData;
    use chrono::Utc;

    fn node(id: &str, status: &str, priority: i32) -> NodeData {
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: status.into(),
            priority,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now() - chrono::Duration::days(10),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: "h".into(),
        }
    }

    fn snap_of(nodes: Vec<NodeData>, edges: Vec<(&str, &str, DependencyKind)>) -> GraphSnapshot {
        let r = FakeBeadsRows {
            issues: nodes,
            edges: edges.into_iter().map(|(a, b, k)| (a.into(), b.into(), k)).collect(),
        };
        let mut s = load_from_rows(r, None);
        s.compute_data_hash();
        s
    }

    #[test]
    fn triage_orders_p0_above_p4() {
        let s = snap_of(vec![node("h", "open", 0), node("l", "open", 4)], vec![]);
        let cfg = ScoreConfig::default();
        let r = compute_triage(&s, &cfg);
        assert_eq!(r.triage.quick_ref.top_picks[0].id, "h");
        assert_eq!(r.triage.quick_ref.top_picks[1].id, "l");
    }

    #[test]
    fn project_health_counts_actionable_and_blocked() {
        let s = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "closed", 2),
            ],
            vec![("a", "b", DependencyKind::Blocks)],
        );
        let cfg = ScoreConfig::default();
        let r = compute_triage(&s, &cfg);
        let h = r.triage.project_health;
        assert_eq!(h.counts.total, 3);
        assert_eq!(h.counts.closed, 1);
        assert_eq!(h.counts.open, 2);
        assert_eq!(h.counts.actionable, 1); // only `a`
        assert_eq!(h.counts.blocked, 1); // `b`
    }

    #[test]
    fn graph_health_detects_cycle() {
        let s = snap_of(
            vec![node("a", "open", 2), node("b", "open", 2)],
            vec![("a", "b", DependencyKind::Blocks), ("b", "a", DependencyKind::Blocks)],
        );
        let cfg = ScoreConfig::default();
        let r = compute_triage(&s, &cfg);
        let g = r.triage.project_health.graph;
        assert!(g.has_cycles);
        assert_eq!(g.cycle_count, 1);
    }

    #[test]
    fn blockers_to_clear_sorted_by_unblocks_count() {
        // a -> {b, c, d}, e -> f
        let s = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "open", 2),
                node("d", "open", 2),
                node("e", "open", 2),
                node("f", "open", 2),
            ],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("a", "c", DependencyKind::Blocks),
                ("a", "d", DependencyKind::Blocks),
                ("e", "f", DependencyKind::Blocks),
            ],
        );
        let cfg = ScoreConfig::default();
        let r = compute_triage(&s, &cfg);
        let b = r.triage.blockers_to_clear;
        assert!(b.iter().any(|x| x.id == "a"));
        assert!(b[0].unblocks_count >= b.last().unwrap().unblocks_count);
    }
}
```

- [ ] **Step 2: Wire submodule**

In `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
pub mod triage;
pub use triage::compute_triage;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-pm graph_engine::triage::tests --lib`
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/triage.rs crates/spur-pm/src/graph_engine/mod.rs
git commit -m "feat(graph-engine): compute_triage with project_health + blockers"
```

---

## Task 8: `compute_plan` (M3)

**Files:**
- Create: `crates/spur-pm/src/graph_engine/plan.rs`
- Modify: `crates/spur-pm/src/graph_engine/mod.rs`

- [ ] **Step 1: Write failing tests + implementation**

Create `crates/spur-pm/src/graph_engine/plan.rs`:

```rust
use crate::graph::{ExecutionPlan, ExecutionTrack, PlanBody, PlanSummary, TrackItem};
use crate::graph_engine::score::{is_actionable, transitive_unblocks, ScoreConfig};
use crate::graph_engine::snapshot::{DependencyKind, GraphSnapshot};
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet};

pub fn compute_plan(snap: &GraphSnapshot, _cfg: &ScoreConfig) -> ExecutionPlan {
    let open: HashSet<NodeIndex> = snap
        .graph
        .node_indices()
        .filter(|&i| matches!(snap.graph[i].status.as_str(), "open" | "in_progress"))
        .collect();

    let mut depth: HashMap<NodeIndex, u32> = HashMap::new();
    for ix in &open {
        depth.insert(*ix, depth_for(snap, *ix, &open, &mut HashMap::new()));
    }

    let mut by_depth: HashMap<u32, Vec<NodeIndex>> = HashMap::new();
    for (ix, d) in &depth {
        by_depth.entry(*d).or_default().push(*ix);
    }

    let mut ordered_depths: Vec<u32> = by_depth.keys().copied().collect();
    ordered_depths.sort();

    let track_letter = |i: usize| -> String {
        let mut s = String::new();
        let mut n = i + 1;
        while n > 0 {
            let r = (n - 1) % 26;
            s.insert(0, (b'A' + r as u8) as char);
            n = (n - 1) / 26;
        }
        format!("track-{s}")
    };

    let mut tracks: Vec<ExecutionTrack> = Vec::new();
    for (i, d) in ordered_depths.into_iter().enumerate() {
        let mut items: Vec<NodeIndex> = by_depth.remove(&d).unwrap();
        items.sort_by(|a, b| {
            snap.graph[*a]
                .priority
                .cmp(&snap.graph[*b].priority)
                .then_with(|| snap.graph[*a].id.cmp(&snap.graph[*b].id))
        });

        let track_items: Vec<TrackItem> = items
            .iter()
            .map(|&ix| {
                let n = &snap.graph[ix];
                let unblocks: Vec<String> = snap
                    .graph
                    .edges(ix)
                    .filter(|e| e.weight().kind.is_blocking())
                    .map(|e| snap.graph[e.target()].id.clone())
                    .collect();
                TrackItem {
                    id: n.id.clone(),
                    title: n.title.clone(),
                    priority: Some(n.priority),
                    status: Some(n.status.clone()),
                    unblocks: if unblocks.is_empty() {
                        None
                    } else {
                        Some(unblocks)
                    },
                }
            })
            .collect();

        let reason = if track_items.len() == 1 && d == 0 {
            "Single actionable item".into()
        } else {
            format!("Generation {d} — depends on {d} completed track(s) above")
        };

        tracks.push(ExecutionTrack {
            track_id: track_letter(i),
            items: track_items,
            reason: Some(reason),
        });
    }

    let total_actionable = open
        .iter()
        .filter(|&&ix| is_actionable(snap, ix))
        .count();
    let total_blocked = open.len() - total_actionable;

    let summary = open
        .iter()
        .max_by_key(|&&ix| transitive_unblocks(snap, ix))
        .map(|&ix| PlanSummary {
            highest_impact: Some(snap.graph[ix].id.clone()),
            impact_reason: Some(format!(
                "Unblocks {} downstream issue(s)",
                transitive_unblocks(snap, ix)
            )),
            unblocks_count: transitive_unblocks(snap, ix),
        });

    ExecutionPlan {
        generated_at: Some(snap.generated_at.to_rfc3339()),
        data_hash: Some(snap.data_hash.clone()),
        plan: PlanBody {
            tracks,
            total_actionable,
            total_blocked,
            summary,
        },
        usage_hints: vec!["jq '.plan.tracks[] | {id: .track_id, items: [.items[].id]}'".into()],
        raw: serde_json::Value::Null,
    }
}

fn depth_for(
    snap: &GraphSnapshot,
    ix: NodeIndex,
    open: &HashSet<NodeIndex>,
    memo: &mut HashMap<NodeIndex, u32>,
) -> u32 {
    if let Some(&d) = memo.get(&ix) {
        return d;
    }
    let mut max_pred = 0u32;
    for e in snap.graph.edges_directed(ix, petgraph::Direction::Incoming) {
        if !e.weight().kind.is_blocking() {
            continue;
        }
        let pred = e.source();
        if !open.contains(&pred) {
            // closed parent — doesn't count toward depth
            continue;
        }
        let d = 1 + depth_for(snap, pred, open, memo);
        if d > max_pred {
            max_pred = d;
        }
    }
    memo.insert(ix, max_pred);
    max_pred
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::snapshot::loader_tests::{load_from_rows, FakeBeadsRows};
    use crate::graph_engine::snapshot::NodeData;
    use chrono::Utc;

    fn node(id: &str, status: &str, priority: i32) -> NodeData {
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: status.into(),
            priority,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: "h".into(),
        }
    }

    fn snap_of(nodes: Vec<NodeData>, edges: Vec<(&str, &str, DependencyKind)>) -> GraphSnapshot {
        let r = FakeBeadsRows {
            issues: nodes,
            edges: edges.into_iter().map(|(a, b, k)| (a.into(), b.into(), k)).collect(),
        };
        let mut s = load_from_rows(r, None);
        s.compute_data_hash();
        s
    }

    #[test]
    fn linear_chain_yields_three_tracks() {
        let s = snap_of(
            vec![node("a", "open", 2), node("b", "open", 2), node("c", "open", 2)],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
            ],
        );
        let cfg = ScoreConfig::default();
        let p = compute_plan(&s, &cfg);
        assert_eq!(p.plan.tracks.len(), 3);
        assert_eq!(p.plan.tracks[0].items[0].id, "a");
        assert_eq!(p.plan.tracks[1].items[0].id, "b");
        assert_eq!(p.plan.tracks[2].items[0].id, "c");
    }

    #[test]
    fn parallel_independent_items_share_track_zero() {
        let s = snap_of(
            vec![node("a", "open", 2), node("b", "open", 2), node("c", "open", 2)],
            vec![],
        );
        let cfg = ScoreConfig::default();
        let p = compute_plan(&s, &cfg);
        assert_eq!(p.plan.tracks.len(), 1);
        assert_eq!(p.plan.tracks[0].items.len(), 3);
    }

    #[test]
    fn highest_impact_picks_max_unblocks() {
        let s = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "open", 2),
                node("d", "open", 2),
            ],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("a", "c", DependencyKind::Blocks),
                ("a", "d", DependencyKind::Blocks),
            ],
        );
        let cfg = ScoreConfig::default();
        let p = compute_plan(&s, &cfg);
        assert_eq!(p.plan.summary.unwrap().highest_impact.as_deref(), Some("a"));
    }

    #[test]
    fn closed_parent_does_not_increase_depth() {
        let s = snap_of(
            vec![node("a", "closed", 2), node("b", "open", 2)],
            vec![("a", "b", DependencyKind::Blocks)],
        );
        let cfg = ScoreConfig::default();
        let p = compute_plan(&s, &cfg);
        // Only b is open; closed parent doesn't add depth.
        assert_eq!(p.plan.tracks.len(), 1);
        assert_eq!(p.plan.tracks[0].items[0].id, "b");
    }
}
```

- [ ] **Step 2: Wire submodule**

In `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
pub mod plan;
pub use plan::compute_plan;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-pm graph_engine::plan::tests --lib`
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/plan.rs crates/spur-pm/src/graph_engine/mod.rs
git commit -m "feat(graph-engine): compute_plan via topological generations"
```

---

## Task 9: `metrics.rs` HITS

**Files:**
- Create: `crates/spur-pm/src/graph_engine/metrics.rs`
- Modify: `crates/spur-pm/src/graph_engine/mod.rs`

- [ ] **Step 1: Write failing test for HITS**

Create `crates/spur-pm/src/graph_engine/metrics.rs`:

```rust
use crate::graph_engine::snapshot::GraphSnapshot;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::HashMap;

/// HITS algorithm — returns (hubs, authorities) maps keyed by NodeIndex.
/// Iterative, normalized at each step. 50 iterations is sufficient for our scale.
pub fn hits(snap: &GraphSnapshot) -> (HashMap<NodeIndex, f64>, HashMap<NodeIndex, f64>) {
    let mut hubs: HashMap<NodeIndex, f64> = snap
        .graph
        .node_indices()
        .map(|i| (i, 1.0))
        .collect();
    let mut auths: HashMap<NodeIndex, f64> = hubs.clone();

    for _ in 0..50 {
        // authority(i) = sum of hub(j) for j -> i
        let mut new_auths: HashMap<NodeIndex, f64> = HashMap::new();
        for ix in snap.graph.node_indices() {
            let mut s = 0.0;
            for e in snap.graph.edges_directed(ix, petgraph::Direction::Incoming) {
                s += hubs[&e.source()];
            }
            new_auths.insert(ix, s);
        }
        // hub(i) = sum of authority(j) for i -> j
        let mut new_hubs: HashMap<NodeIndex, f64> = HashMap::new();
        for ix in snap.graph.node_indices() {
            let mut s = 0.0;
            for e in snap.graph.edges(ix) {
                s += new_auths[&e.target()];
            }
            new_hubs.insert(ix, s);
        }
        normalize(&mut new_auths);
        normalize(&mut new_hubs);
        hubs = new_hubs;
        auths = new_auths;
    }
    (hubs, auths)
}

fn normalize(m: &mut HashMap<NodeIndex, f64>) {
    let norm = m.values().map(|v| v * v).sum::<f64>().sqrt();
    if norm > 0.0 {
        for v in m.values_mut() {
            *v /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::snapshot::loader_tests::{load_from_rows, FakeBeadsRows};
    use crate::graph_engine::snapshot::{DependencyKind, NodeData};
    use chrono::Utc;

    fn n(id: &str) -> NodeData {
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: "h".into(),
        }
    }

    fn snap(nodes: Vec<NodeData>, edges: Vec<(&str, &str, DependencyKind)>) -> GraphSnapshot {
        let r = FakeBeadsRows {
            issues: nodes,
            edges: edges.into_iter().map(|(a, b, k)| (a.into(), b.into(), k)).collect(),
        };
        let mut s = load_from_rows(r, None);
        s.compute_data_hash();
        s
    }

    #[test]
    fn hub_with_many_outedges_scores_highest() {
        // a -> b, c, d; e isolated
        let s = snap(
            vec![n("a"), n("b"), n("c"), n("d"), n("e")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("a", "c", DependencyKind::Blocks),
                ("a", "d", DependencyKind::Blocks),
            ],
        );
        let (hubs, _) = hits(&s);
        let a_hub = hubs[&s.by_id["a"]];
        assert!(a_hub > hubs[&s.by_id["b"]]);
        assert!(a_hub > hubs[&s.by_id["e"]]);
    }

    #[test]
    fn authority_with_many_inedges_scores_highest() {
        // a, b, c -> d; e isolated
        let s = snap(
            vec![n("a"), n("b"), n("c"), n("d"), n("e")],
            vec![
                ("a", "d", DependencyKind::Blocks),
                ("b", "d", DependencyKind::Blocks),
                ("c", "d", DependencyKind::Blocks),
            ],
        );
        let (_, auths) = hits(&s);
        let d_auth = auths[&s.by_id["d"]];
        assert!(d_auth > auths[&s.by_id["a"]]);
        assert!(d_auth > auths[&s.by_id["e"]]);
    }
}
```

- [ ] **Step 2: Wire submodule**

In `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
pub mod metrics;
pub use metrics::hits;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-pm graph_engine::metrics::tests --lib`
Expected: 2 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/metrics.rs crates/spur-pm/src/graph_engine/mod.rs
git commit -m "feat(graph-engine): HITS algorithm for hubs/authorities"
```

---

## Task 10: `metrics.rs` Brandes' betweenness centrality

**Files:**
- Modify: `crates/spur-pm/src/graph_engine/metrics.rs`

- [ ] **Step 1: Write failing test**

Append to `crates/spur-pm/src/graph_engine/metrics.rs`:

```rust
use std::collections::VecDeque;

/// Brandes' algorithm for betweenness centrality on a directed graph.
/// Time complexity: O(V*(V+E)).
pub fn betweenness_centrality_brandes(snap: &GraphSnapshot) -> HashMap<NodeIndex, f64> {
    let mut cb: HashMap<NodeIndex, f64> =
        snap.graph.node_indices().map(|i| (i, 0.0)).collect();

    for s in snap.graph.node_indices() {
        let mut stack: Vec<NodeIndex> = Vec::new();
        let mut preds: HashMap<NodeIndex, Vec<NodeIndex>> = HashMap::new();
        let mut sigma: HashMap<NodeIndex, f64> = HashMap::new();
        let mut dist: HashMap<NodeIndex, i64> = HashMap::new();
        for v in snap.graph.node_indices() {
            preds.insert(v, Vec::new());
            sigma.insert(v, 0.0);
            dist.insert(v, -1);
        }
        sigma.insert(s, 1.0);
        dist.insert(s, 0);

        let mut queue: VecDeque<NodeIndex> = VecDeque::new();
        queue.push_back(s);
        while let Some(v) = queue.pop_front() {
            stack.push(v);
            for e in snap.graph.edges(v) {
                let w = e.target();
                if dist[&w] < 0 {
                    queue.push_back(w);
                    dist.insert(w, dist[&v] + 1);
                }
                if dist[&w] == dist[&v] + 1 {
                    sigma.insert(w, sigma[&w] + sigma[&v]);
                    preds.get_mut(&w).unwrap().push(v);
                }
            }
        }

        let mut delta: HashMap<NodeIndex, f64> =
            snap.graph.node_indices().map(|i| (i, 0.0)).collect();
        while let Some(w) = stack.pop() {
            for &v in &preds[&w] {
                let contrib = (sigma[&v] / sigma[&w]) * (1.0 + delta[&w]);
                delta.insert(v, delta[&v] + contrib);
            }
            if w != s {
                cb.insert(w, cb[&w] + delta[&w]);
            }
        }
    }

    cb
}

#[cfg(test)]
mod brandes_tests {
    use super::*;
    use crate::graph_engine::snapshot::loader_tests::{load_from_rows, FakeBeadsRows};
    use crate::graph_engine::snapshot::{DependencyKind, NodeData};
    use chrono::Utc;

    fn n(id: &str) -> NodeData {
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: "h".into(),
        }
    }

    fn snap(nodes: Vec<NodeData>, edges: Vec<(&str, &str, DependencyKind)>) -> GraphSnapshot {
        let r = FakeBeadsRows {
            issues: nodes,
            edges: edges.into_iter().map(|(a, b, k)| (a.into(), b.into(), k)).collect(),
        };
        let mut s = load_from_rows(r, None);
        s.compute_data_hash();
        s
    }

    #[test]
    fn middle_of_chain_has_highest_betweenness() {
        // a -> b -> c -> d
        let s = snap(
            vec![n("a"), n("b"), n("c"), n("d")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
                ("c", "d", DependencyKind::Blocks),
            ],
        );
        let cb = betweenness_centrality_brandes(&s);
        // b and c lie on shortest paths between others; a and d are endpoints.
        let b = cb[&s.by_id["b"]];
        let c = cb[&s.by_id["c"]];
        let a = cb[&s.by_id["a"]];
        let d = cb[&s.by_id["d"]];
        assert!(b > a);
        assert!(c > a);
        assert!(b > d);
        assert!(c > d);
    }

    #[test]
    fn isolated_node_has_zero_betweenness() {
        let s = snap(vec![n("a"), n("b")], vec![]);
        let cb = betweenness_centrality_brandes(&s);
        assert_eq!(cb[&s.by_id["a"]], 0.0);
        assert_eq!(cb[&s.by_id["b"]], 0.0);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-pm graph_engine::metrics::brandes_tests --lib`
Expected: 2 passed.

- [ ] **Step 3: Wire export**

In `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
pub use metrics::{betweenness_centrality_brandes, hits};
```

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/metrics.rs crates/spur-pm/src/graph_engine/mod.rs
git commit -m "feat(graph-engine): Brandes' betweenness centrality"
```

---

## Task 11: `metrics.rs` k-core decomposition

**Files:**
- Modify: `crates/spur-pm/src/graph_engine/metrics.rs`

- [ ] **Step 1: Write failing test + implementation**

Append to `crates/spur-pm/src/graph_engine/metrics.rs`:

```rust
/// k-core decomposition (treats the graph as undirected for core ranking).
/// Returns each node's coreness (max k for which it remains in the k-core).
pub fn k_core_decomposition(snap: &GraphSnapshot) -> HashMap<NodeIndex, usize> {
    use std::collections::BTreeSet;
    // Compute undirected degrees.
    let mut deg: HashMap<NodeIndex, usize> = HashMap::new();
    for ix in snap.graph.node_indices() {
        let undirected_deg = snap.graph.edges(ix).count()
            + snap.graph.edges_directed(ix, petgraph::Direction::Incoming).count();
        deg.insert(ix, undirected_deg);
    }

    // Sort nodes by current degree, ascending; iteratively remove min-degree node.
    let mut remaining: BTreeSet<(usize, NodeIndex)> =
        deg.iter().map(|(&i, &d)| (d, i)).collect();
    let mut core: HashMap<NodeIndex, usize> = HashMap::new();

    while let Some(&(d, ix)) = remaining.iter().next() {
        remaining.remove(&(d, ix));
        core.insert(ix, d);
        // Decrement neighbors' degrees.
        let neighbors: Vec<NodeIndex> = snap
            .graph
            .edges(ix)
            .map(|e| e.target())
            .chain(
                snap.graph
                    .edges_directed(ix, petgraph::Direction::Incoming)
                    .map(|e| e.source()),
            )
            .collect();
        for nb in neighbors {
            if let Some(&old) = deg.get(&nb) {
                if old > d {
                    let new = old - 1;
                    remaining.remove(&(old, nb));
                    deg.insert(nb, new);
                    if !core.contains_key(&nb) {
                        remaining.insert((new, nb));
                    }
                }
            }
        }
    }
    core
}

#[cfg(test)]
mod kcore_tests {
    use super::*;
    use crate::graph_engine::snapshot::loader_tests::{load_from_rows, FakeBeadsRows};
    use crate::graph_engine::snapshot::{DependencyKind, NodeData};
    use chrono::Utc;

    fn n(id: &str) -> NodeData {
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: "h".into(),
        }
    }

    fn snap(nodes: Vec<NodeData>, edges: Vec<(&str, &str, DependencyKind)>) -> GraphSnapshot {
        let r = FakeBeadsRows {
            issues: nodes,
            edges: edges.into_iter().map(|(a, b, k)| (a.into(), b.into(), k)).collect(),
        };
        let mut s = load_from_rows(r, None);
        s.compute_data_hash();
        s
    }

    #[test]
    fn isolated_node_has_core_zero() {
        let s = snap(vec![n("a"), n("b")], vec![]);
        let c = k_core_decomposition(&s);
        assert_eq!(c[&s.by_id["a"]], 0);
        assert_eq!(c[&s.by_id["b"]], 0);
    }

    #[test]
    fn triangle_yields_core_two() {
        let s = snap(
            vec![n("a"), n("b"), n("c")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
                ("c", "a", DependencyKind::Blocks),
            ],
        );
        let c = k_core_decomposition(&s);
        assert_eq!(c[&s.by_id["a"]], 2);
        assert_eq!(c[&s.by_id["b"]], 2);
        assert_eq!(c[&s.by_id["c"]], 2);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-pm graph_engine::metrics::kcore_tests --lib`
Expected: 2 passed.

- [ ] **Step 3: Wire export**

In `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
pub use metrics::{betweenness_centrality_brandes, hits, k_core_decomposition};
```

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/metrics.rs crates/spur-pm/src/graph_engine/mod.rs
git commit -m "feat(graph-engine): k-core decomposition"
```

---

## Task 12: `compute_insights` (M4)

**Files:**
- Create: `crates/spur-pm/src/graph_engine/insights.rs`
- Modify: `crates/spur-pm/src/graph_engine/mod.rs`

- [ ] **Step 1: Write failing tests + implementation**

Create `crates/spur-pm/src/graph_engine/insights.rs`:

```rust
use crate::graph::{GraphInsights, InsightItem, WhatIfEntry};
use crate::graph_engine::metrics::{betweenness_centrality_brandes, hits, k_core_decomposition};
use crate::graph_engine::snapshot::GraphSnapshot;
use petgraph::algo::{articulation_points::articulation_points, tarjan_scc};
use petgraph::graph::NodeIndex;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct InsightConfig {
    pub top_k: usize,
    pub pagerank_damping: f64,
    pub pagerank_iterations: usize,
}

impl Default for InsightConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            pagerank_damping: 0.85,
            pagerank_iterations: 100,
        }
    }
}

pub fn compute_insights(snap: &GraphSnapshot, cfg: &InsightConfig) -> GraphInsights {
    // Cycles via SCC.
    let scc = tarjan_scc(&snap.graph);
    let cycles: Vec<Vec<String>> = scc
        .into_iter()
        .filter(|c| c.len() > 1)
        .map(|c| {
            let mut ids: Vec<String> = c.into_iter().map(|i| snap.graph[i].id.clone()).collect();
            ids.sort();
            ids
        })
        .collect();

    // Articulation points (operates on undirected graph internally).
    let undirected = build_undirected(&snap.graph);
    let arts: Vec<String> = articulation_points(&undirected)
        .into_iter()
        .map(|i| snap.graph[i].id.clone())
        .collect();

    // PageRank — iterative implementation (independent of petgraph version).
    let pr = pagerank_iterative(snap, cfg.pagerank_damping, cfg.pagerank_iterations);
    let influencers = top_k_named(snap, &pr, cfg.top_k);

    // HITS.
    let (hubs_map, auth_map) = hits(snap);
    let hubs = top_k_named(snap, &hubs_map, cfg.top_k);
    let authorities = top_k_named(snap, &auth_map, cfg.top_k);

    // Betweenness.
    let bw = betweenness_centrality_brandes(snap);
    let bottlenecks = top_k_named(snap, &bw, cfg.top_k);

    // k-core for "Cores".
    let core = k_core_decomposition(snap);
    let core_f64: HashMap<NodeIndex, f64> =
        core.iter().map(|(&k, &v)| (k, v as f64)).collect();
    let cores = top_k_named(snap, &core_f64, cfg.top_k);

    // Keystones = articulation × out-degree, ranked.
    let mut keystone_scores: HashMap<NodeIndex, f64> = HashMap::new();
    let art_set: std::collections::HashSet<String> = arts.iter().cloned().collect();
    for ix in snap.graph.node_indices() {
        if art_set.contains(&snap.graph[ix].id) {
            let od = snap.graph.edges(ix).count() as f64;
            keystone_scores.insert(ix, od);
        }
    }
    let keystones = top_k_named(snap, &keystone_scores, cfg.top_k);

    // Orphans: degree-0 nodes.
    let mut orphans: Vec<String> = snap
        .graph
        .node_indices()
        .filter(|&i| {
            snap.graph.edges(i).count() == 0
                && snap
                    .graph
                    .edges_directed(i, petgraph::Direction::Incoming)
                    .count()
                    == 0
        })
        .map(|i| snap.graph[i].id.clone())
        .collect();
    orphans.sort();

    let nc = snap.graph.node_count();
    let ec = snap.graph.edge_count();
    let cluster_density = if nc > 1 {
        (2.0 * ec as f64) / (nc as f64 * (nc as f64 - 1.0))
    } else {
        0.0
    };

    GraphInsights {
        generated_at: Some(snap.generated_at.to_rfc3339()),
        data_hash: Some(snap.data_hash.clone()),
        bottlenecks,
        keystones,
        influencers,
        hubs,
        authorities,
        cores,
        articulation: arts,
        orphans,
        cycles,
        cluster_density,
        top_what_ifs: Vec::<WhatIfEntry>::new(),
        raw: serde_json::Value::Null,
    }
}

fn top_k_named(
    snap: &GraphSnapshot,
    scores: &HashMap<NodeIndex, f64>,
    k: usize,
) -> Vec<InsightItem> {
    let mut v: Vec<(NodeIndex, f64)> = scores.iter().map(|(&i, &s)| (i, s)).collect();
    v.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| snap.graph[a.0].id.cmp(&snap.graph[b.0].id))
    });
    v.into_iter()
        .take(k)
        .filter(|(_, s)| *s > 0.0)
        .map(|(i, s)| InsightItem {
            id: snap.graph[i].id.clone(),
            value: s,
        })
        .collect()
}

fn pagerank_iterative(snap: &GraphSnapshot, damping: f64, iterations: usize) -> HashMap<NodeIndex, f64> {
    let n = snap.graph.node_count() as f64;
    if n == 0.0 {
        return HashMap::new();
    }
    let mut pr: HashMap<NodeIndex, f64> = snap
        .graph
        .node_indices()
        .map(|i| (i, 1.0 / n))
        .collect();
    let teleport = (1.0 - damping) / n;

    for _ in 0..iterations {
        let mut new_pr: HashMap<NodeIndex, f64> =
            snap.graph.node_indices().map(|i| (i, teleport)).collect();
        for ix in snap.graph.node_indices() {
            let out_deg = snap.graph.edges(ix).count() as f64;
            if out_deg == 0.0 {
                continue;
            }
            let share = damping * pr[&ix] / out_deg;
            for e in snap.graph.edges(ix) {
                *new_pr.get_mut(&e.target()).unwrap() += share;
            }
        }
        pr = new_pr;
    }
    pr
}

fn build_undirected(
    g: &petgraph::Graph<crate::graph_engine::snapshot::NodeData, crate::graph_engine::snapshot::EdgeData>,
) -> petgraph::graph::UnGraph<(), ()> {
    let mut ug = petgraph::graph::UnGraph::<(), ()>::new_undirected();
    let mut map: HashMap<NodeIndex, petgraph::graph::NodeIndex> = HashMap::new();
    for ix in g.node_indices() {
        let nix = ug.add_node(());
        map.insert(ix, nix);
    }
    for e in g.edge_indices() {
        let (a, b) = g.edge_endpoints(e).unwrap();
        ug.add_edge(map[&a], map[&b], ());
    }
    ug
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::snapshot::loader_tests::{load_from_rows, FakeBeadsRows};
    use crate::graph_engine::snapshot::{DependencyKind, NodeData};
    use chrono::Utc;

    fn n(id: &str) -> NodeData {
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: "h".into(),
        }
    }

    fn snap(nodes: Vec<NodeData>, edges: Vec<(&str, &str, DependencyKind)>) -> GraphSnapshot {
        let r = FakeBeadsRows {
            issues: nodes,
            edges: edges.into_iter().map(|(a, b, k)| (a.into(), b.into(), k)).collect(),
        };
        let mut s = load_from_rows(r, None);
        s.compute_data_hash();
        s
    }

    #[test]
    fn isolated_node_appears_in_orphans() {
        let s = snap(
            vec![n("a"), n("b"), n("orphan")],
            vec![("a", "b", DependencyKind::Blocks)],
        );
        let cfg = InsightConfig::default();
        let i = compute_insights(&s, &cfg);
        assert!(i.orphans.contains(&"orphan".to_string()));
        assert!(!i.orphans.contains(&"a".to_string()));
    }

    #[test]
    fn cycle_appears_in_cycles() {
        let s = snap(
            vec![n("a"), n("b")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "a", DependencyKind::Blocks),
            ],
        );
        let cfg = InsightConfig::default();
        let i = compute_insights(&s, &cfg);
        assert_eq!(i.cycles.len(), 1);
        assert_eq!(i.cycles[0], vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn top_what_ifs_is_empty_in_v1() {
        let s = snap(vec![n("a"), n("b")], vec![]);
        let cfg = InsightConfig::default();
        let i = compute_insights(&s, &cfg);
        assert!(i.top_what_ifs.is_empty());
    }

    #[test]
    fn cluster_density_for_dense_triangle() {
        let s = snap(
            vec![n("a"), n("b"), n("c")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
                ("a", "c", DependencyKind::Blocks),
            ],
        );
        let cfg = InsightConfig::default();
        let i = compute_insights(&s, &cfg);
        // 3 nodes, 3 edges: density = 2*3/(3*2) = 1.0
        assert!((i.cluster_density - 1.0).abs() < 1e-9);
    }
}
```

- [ ] **Step 2: Wire submodule**

In `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
pub mod insights;
pub use insights::{compute_insights, InsightConfig};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-pm graph_engine::insights::tests --lib`
Expected: 4 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/insights.rs crates/spur-pm/src/graph_engine/mod.rs
git commit -m "feat(graph-engine): compute_insights (PageRank+HITS+Brandes+k-core+articulation+cycles)"
```

---

## Task 13: `raw.rs` — JSON serialization for the `raw` field

**Files:**
- Create: `crates/spur-pm/src/graph_engine/raw.rs`
- Modify: `crates/spur-pm/src/graph_engine/mod.rs`

The `raw` field on each report is a `serde_json::Value` that MCP passes to brain agents. We generate it by serializing the typed report (which has `#[serde(skip)]` on the `raw` field itself, avoiding recursion) — `serde_json::to_value(report)` does this in one step.

- [ ] **Step 1: Write failing test**

Create `crates/spur-pm/src/graph_engine/raw.rs`:

```rust
use crate::graph::{
    AlertReport, DependencyGraph, ExecutionPlan, GraphInsights, TriageReport,
};

pub fn serialize_triage(r: &TriageReport) -> serde_json::Value {
    serde_json::to_value(r).unwrap_or(serde_json::Value::Null)
}
pub fn serialize_plan(r: &ExecutionPlan) -> serde_json::Value {
    serde_json::to_value(r).unwrap_or(serde_json::Value::Null)
}
pub fn serialize_insights(r: &GraphInsights) -> serde_json::Value {
    serde_json::to_value(r).unwrap_or(serde_json::Value::Null)
}
pub fn serialize_alerts(r: &AlertReport) -> serde_json::Value {
    serde_json::to_value(r).unwrap_or(serde_json::Value::Null)
}
pub fn serialize_subgraph(r: &DependencyGraph) -> serde_json::Value {
    serde_json::to_value(r).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{QuickRef, TopPick, TriageResult};

    #[test]
    fn triage_round_trips_top_picks() {
        let r = TriageReport {
            generated_at: Some("2026-05-05T00:00:00Z".into()),
            data_hash: Some("abc".into()),
            triage: TriageResult {
                quick_ref: QuickRef {
                    open_count: 1,
                    actionable_count: 1,
                    blocked_count: 0,
                    in_progress_count: 0,
                    top_picks: vec![TopPick {
                        id: "bd-1".into(),
                        title: "T".into(),
                        score: 0.5,
                        reasons: vec!["r".into()],
                        unblocks: 0,
                    }],
                },
                ..TriageResult::default()
            },
            usage_hints: vec![],
            raw: serde_json::Value::Null,
        };
        let v = serialize_triage(&r);
        assert_eq!(v["data_hash"], "abc");
        assert_eq!(v["triage"]["quick_ref"]["top_picks"][0]["id"], "bd-1");
    }
}
```

- [ ] **Step 2: Wire submodule**

In `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
pub mod raw;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-pm graph_engine::raw::tests --lib`
Expected: 1 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/src/graph_engine/raw.rs crates/spur-pm/src/graph_engine/mod.rs
git commit -m "feat(graph-engine): raw JSON serializers for MCP passthrough"
```

---

## Task 14: `GraphEngine` facade (`mod.rs`)

**Files:**
- Modify: `crates/spur-pm/src/graph_engine/mod.rs`

- [ ] **Step 1: Add `GraphEngine` struct + 5 methods**

Append to `crates/spur-pm/src/graph_engine/mod.rs`:

```rust
use std::sync::Arc;

use crate::beads_crate::BeadsCrateAdapter;
use crate::graph::{AlertReport, DependencyGraph, ExecutionPlan, GraphInsights, TriageReport};

#[derive(Debug, Clone)]
pub struct GraphEngineConfig {
    pub score: ScoreConfig,
    pub insight: InsightConfig,
    pub alert: AlertConfig,
}

impl Default for GraphEngineConfig {
    fn default() -> Self {
        Self {
            score: ScoreConfig::default(),
            insight: InsightConfig::default(),
            alert: AlertConfig::default(),
        }
    }
}

pub struct GraphEngine {
    beads: Arc<BeadsCrateAdapter>,
    cfg: GraphEngineConfig,
}

impl GraphEngine {
    pub fn new(beads: Arc<BeadsCrateAdapter>, cfg: GraphEngineConfig) -> Self {
        Self { beads, cfg }
    }

    async fn snapshot(&self, label: Option<String>) -> anyhow::Result<GraphSnapshot> {
        let label_owned = label.clone();
        self.beads
            .read(move |s| {
                let mut snap = snapshot::load_graph_snapshot(s, label_owned.as_deref())?;
                snap.compute_data_hash();
                Ok(snap)
            })
            .await
    }

    pub async fn triage(&self, label: Option<&str>) -> anyhow::Result<TriageReport> {
        let snap = self.snapshot(label.map(|s| s.to_string())).await?;
        let mut report = compute_triage(&snap, &self.cfg.score);
        report.raw = raw::serialize_triage(&report);
        Ok(report)
    }

    pub async fn plan(&self, label: Option<&str>) -> anyhow::Result<ExecutionPlan> {
        let snap = self.snapshot(label.map(|s| s.to_string())).await?;
        let mut report = compute_plan(&snap, &self.cfg.score);
        report.raw = raw::serialize_plan(&report);
        Ok(report)
    }

    pub async fn insights(&self, label: Option<&str>) -> anyhow::Result<GraphInsights> {
        let snap = self.snapshot(label.map(|s| s.to_string())).await?;
        let mut report = compute_insights(&snap, &self.cfg.insight);
        report.raw = raw::serialize_insights(&report);
        Ok(report)
    }

    pub async fn alerts(&self) -> anyhow::Result<AlertReport> {
        let snap = self.snapshot(None).await?;
        let mut report = compute_alerts(&snap, &self.cfg.alert);
        report.raw = raw::serialize_alerts(&report);
        Ok(report)
    }

    pub async fn subgraph(
        &self,
        root_id: &str,
        depth: Option<u32>,
        format: Option<&str>,
    ) -> anyhow::Result<DependencyGraph> {
        let snap = self.snapshot(None).await?;
        let mut report = compute_subgraph(
            &snap,
            SubgraphRoot::Issue(root_id),
            depth,
            GraphFormat::parse(format),
        );
        report.raw = raw::serialize_subgraph(&report);
        Ok(report)
    }

    pub async fn graph_by_label(
        &self,
        label: &str,
        format: Option<&str>,
    ) -> anyhow::Result<DependencyGraph> {
        let snap = self.snapshot(Some(label.to_string())).await?;
        let mut report = compute_subgraph(
            &snap,
            SubgraphRoot::AllIssues,
            None,
            GraphFormat::parse(format),
        );
        report.raw = raw::serialize_subgraph(&report);
        Ok(report)
    }
}
```

- [ ] **Step 2: Verify build**

Run: `cargo check -p spur-pm`
Expected: clean build (the BeadsCrateAdapter::read signature must match what we use; if not, adjust).

- [ ] **Step 3: Commit**

```bash
git add crates/spur-pm/src/graph_engine/mod.rs
git commit -m "feat(graph-engine): GraphEngine facade with 5 async methods"
```

---

## Task 15: Integration tests via `TestBeadsWorkspace`

**Files:**
- Create: `crates/spur-pm/tests/graph_engine_integration.rs`

This task **depends** on the companion plan having delivered `TestBeadsWorkspace` (`crates/spur-pm/src/test_workspace.rs`). If that helper is not yet present, this task waits.

- [ ] **Step 1: Write end-to-end integration test**

Create `crates/spur-pm/tests/graph_engine_integration.rs`:

```rust
//! End-to-end tests of GraphEngine over a real beads_rust SqliteStorage.

use std::sync::Arc;

use spur_pm::beads_crate::BeadsCrateAdapter;
use spur_pm::graph_engine::{GraphEngine, GraphEngineConfig};
use spur_pm::test_workspace::TestBeadsWorkspace;

#[tokio::test]
async fn triage_returns_top_picks_for_seeded_workspace() {
    let ws = TestBeadsWorkspace::new().await;
    ws.seed_issue("bd-1", "First task", "open", 1).await;
    ws.seed_issue("bd-2", "Blocked task", "open", 0).await;
    ws.seed_dep("bd-1", "bd-2", "blocks").await;

    let beads = Arc::new(BeadsCrateAdapter::open(ws.path()).await.unwrap());
    let engine = GraphEngine::new(beads, GraphEngineConfig::default());

    let report = engine.triage(None).await.unwrap();
    assert!(!report.triage.quick_ref.top_picks.is_empty());
    assert_eq!(report.triage.quick_ref.top_picks[0].id, "bd-1"); // bd-1 actionable + unblocks bd-2
    assert!(!report.data_hash.as_deref().unwrap_or("").is_empty());
    assert!(!report.raw.is_null());
}

#[tokio::test]
async fn alerts_finds_cycle() {
    let ws = TestBeadsWorkspace::new().await;
    ws.seed_issue("bd-a", "A", "open", 2).await;
    ws.seed_issue("bd-b", "B", "open", 2).await;
    ws.seed_dep("bd-a", "bd-b", "blocks").await;
    ws.seed_dep("bd-b", "bd-a", "blocks").await;

    let beads = Arc::new(BeadsCrateAdapter::open(ws.path()).await.unwrap());
    let engine = GraphEngine::new(beads, GraphEngineConfig::default());

    let report = engine.alerts().await.unwrap();
    let cycle_alerts: Vec<_> = report
        .alerts
        .iter()
        .filter(|a| a.alert_type == "cycle")
        .collect();
    assert!(!cycle_alerts.is_empty());
    assert!(cycle_alerts.iter().all(|a| a.severity == "critical"));
}

#[tokio::test]
async fn subgraph_json_format_returns_adjacency() {
    let ws = TestBeadsWorkspace::new().await;
    ws.seed_issue("bd-r", "Root", "open", 2).await;
    ws.seed_issue("bd-c", "Child", "open", 2).await;
    ws.seed_dep("bd-r", "bd-c", "blocks").await;

    let beads = Arc::new(BeadsCrateAdapter::open(ws.path()).await.unwrap());
    let engine = GraphEngine::new(beads, GraphEngineConfig::default());

    let g = engine.subgraph("bd-r", Some(1), Some("json")).await.unwrap();
    let adj = g.adjacency.unwrap();
    assert_eq!(adj.nodes.len(), 2);
    assert_eq!(adj.edges.unwrap().len(), 1);
}

#[tokio::test]
async fn data_hash_stable_across_two_invocations() {
    let ws = TestBeadsWorkspace::new().await;
    ws.seed_issue("bd-1", "Stable", "open", 2).await;
    let beads = Arc::new(BeadsCrateAdapter::open(ws.path()).await.unwrap());
    let engine = GraphEngine::new(beads, GraphEngineConfig::default());
    let r1 = engine.triage(None).await.unwrap();
    let r2 = engine.triage(None).await.unwrap();
    assert_eq!(r1.data_hash, r2.data_hash);
}
```

> **If `TestBeadsWorkspace` API names differ** (`seed_issue`, `seed_dep`, `path`, `open`), adjust to the actual signatures landed by the companion plan. The intent is unchanged.

- [ ] **Step 2: Run integration tests**

Run: `cargo test -p spur-pm --test graph_engine_integration`
Expected: 4 passed.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-pm/tests/graph_engine_integration.rs
git commit -m "test(graph-engine): integration tests via TestBeadsWorkspace"
```

---

## Task 16: `BvAdapter` internal swap

**Files:**
- Rewrite: `crates/spur-pm/src/bv.rs`

**Strict prerequisite:** companion plan must have completed its cutover. `BeadsCrateAdapter` must be the unconditional `IssueTracker`. Verify with: `git grep 'pub struct BeadsAdapter' crates/spur-pm/src/` returns no matches.

- [ ] **Step 1: Replace `bv.rs` contents**

Overwrite `crates/spur-pm/src/bv.rs`:

```rust
//! `BvAdapter` — wraps the native `GraphEngine` and provides the historical
//! API surface that MCP, orchestrator, TUI, and tests already call.
//!
//! All methods return typed structs with a `raw: serde_json::Value` field
//! containing the report's JSON for MCP passthrough.

use std::path::Path;
use std::sync::Arc;

use crate::beads_crate::BeadsCrateAdapter;
use crate::graph::{AlertReport, DependencyGraph, ExecutionPlan, GraphInsights, TriageReport};
use crate::graph_engine::{GraphEngine, GraphEngineConfig};

pub struct BvAdapter {
    engine: GraphEngine,
}

impl BvAdapter {
    /// Construct a BvAdapter from a connected BeadsCrateAdapter and graph config.
    /// Panics if the BeadsCrateAdapter is not yet open.
    pub fn from_beads(beads: Arc<BeadsCrateAdapter>, cfg: GraphEngineConfig) -> Self {
        Self {
            engine: GraphEngine::new(beads, cfg),
        }
    }

    /// Compatibility constructor matching the historical signature.
    /// `_repo_root` is ignored; the BeadsCrateAdapter that wraps `.beads/` is
    /// the source of truth.
    pub async fn connect(_repo_root: &Path, beads: Arc<BeadsCrateAdapter>) -> anyhow::Result<Self> {
        Ok(Self::from_beads(beads, GraphEngineConfig::default()))
    }

    pub async fn triage(&self, label: Option<&str>) -> anyhow::Result<TriageReport> {
        self.engine.triage(label).await
    }

    pub async fn plan(&self, label: Option<&str>) -> anyhow::Result<ExecutionPlan> {
        self.engine.plan(label).await
    }

    pub async fn insights(&self, label: Option<&str>) -> anyhow::Result<GraphInsights> {
        self.engine.insights(label).await
    }

    pub async fn alerts(&self) -> anyhow::Result<AlertReport> {
        self.engine.alerts().await
    }

    pub async fn subgraph(
        &self,
        root_id: &str,
        depth: Option<u32>,
        format: Option<&str>,
    ) -> anyhow::Result<DependencyGraph> {
        self.engine.subgraph(root_id, depth, format).await
    }

    pub async fn graph_by_label(
        &self,
        label: &str,
        format: Option<&str>,
    ) -> anyhow::Result<DependencyGraph> {
        self.engine.graph_by_label(label, format).await
    }
}
```

- [ ] **Step 2: Verify build**

Run: `cargo check -p spur-pm`
Expected: clean.

Then: `cargo check --workspace`
Expected: clean. If `BvAdapter::connect`'s old `(repo_root: &Path)` signature is called by any caller that doesn't yet have a `BeadsCrateAdapter`, adjust those call sites in T17.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-pm/src/bv.rs
git commit -m "refactor(spur-pm): BvAdapter delegates to native GraphEngine (no subprocess)"
```

---

## Task 17: `PmService` wiring

**Files:**
- Modify: `crates/spur-pm/src/service.rs`

- [ ] **Step 1: Locate the bv probe in PmService::try_new**

Run: `grep -n 'BvAdapter::connect' crates/spur-pm/src/service.rs`
Expected: one or more matches.

- [ ] **Step 2: Update construction**

Replace the previous bv-binary probe with a direct construction using the already-connected `BeadsCrateAdapter`. Concretely, in `PmService::try_new` (or wherever the `bv` field is populated):

```rust
let bv = Some(Arc::new(crate::bv::BvAdapter::from_beads(
    beads_crate_adapter.clone(),
    crate::graph_engine::GraphEngineConfig::default(),
)));
```

Or, if you wish to make `bv` unconditional rather than `Option`, change the field type to `Arc<BvAdapter>` — but that's a wider refactor; keep `Option<Arc<BvAdapter>>` for now and always populate it.

- [ ] **Step 3: Build and run existing tests**

Run: `cargo test -p spur-pm`
Expected: all green. Existing tests in `crates/spur-pm/tests/bv_triage.rs` still need to compile; they use the old `BvAdapter::connect(&path)` signature, so adjust them in this task.

- [ ] **Step 4: Update `bv_triage.rs` to use `TestBeadsWorkspace`**

Modify `crates/spur-pm/tests/bv_triage.rs` to construct the `BeadsCrateAdapter` from `TestBeadsWorkspace` and pass it to `BvAdapter::from_beads(...)`. Remove `bv` binary install dependency.

- [ ] **Step 5: Run full workspace check**

Run: `cargo test --workspace`
Expected: all green.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-pm/src/service.rs crates/spur-pm/tests/bv_triage.rs
git commit -m "refactor(spur-pm): PmService wires BvAdapter from BeadsCrateAdapter"
```

---

## Task 18: MCP-passthrough snapshot tests

**Files:**
- Create: `crates/spur-pm/tests/snapshots/triage.json`
- Create: `crates/spur-pm/tests/snapshots/plan.json`
- Create: `crates/spur-pm/tests/snapshots/insights.json`
- Create: `crates/spur-pm/tests/snapshots/alerts.json`
- Create: `crates/spur-pm/tests/snapshots/subgraph.json`
- Create: `crates/spur-pm/tests/mcp_passthrough_snapshots.rs`

- [ ] **Step 1: Write the snapshot test scaffolding**

Create `crates/spur-pm/tests/mcp_passthrough_snapshots.rs`:

```rust
//! Regression suite for the `raw` JSON passed to MCP brain agents.
//! When this drifts, brain prompts that parse specific fields may break.
//! Update intentionally via `cargo test ... -- --ignored --update` (manual review).

use std::sync::Arc;

use spur_pm::beads_crate::BeadsCrateAdapter;
use spur_pm::graph_engine::{GraphEngine, GraphEngineConfig};
use spur_pm::test_workspace::TestBeadsWorkspace;

async fn canonical_engine() -> GraphEngine {
    let ws = TestBeadsWorkspace::new().await;
    ws.seed_issue("bd-1", "Top task", "open", 0).await;
    ws.seed_issue("bd-2", "Blocked task", "open", 1).await;
    ws.seed_issue("bd-3", "Closed parent", "closed", 2).await;
    ws.seed_dep("bd-3", "bd-1", "blocks").await;
    ws.seed_dep("bd-1", "bd-2", "blocks").await;
    let beads = Arc::new(BeadsCrateAdapter::open(ws.path()).await.unwrap());
    GraphEngine::new(beads, GraphEngineConfig::default())
}

fn assert_snapshot(actual: &serde_json::Value, name: &str) {
    let path = format!("tests/snapshots/{name}.json");
    if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
        std::fs::write(&path, serde_json::to_string_pretty(actual).unwrap()).unwrap();
        return;
    }
    let expected_str = std::fs::read_to_string(&path).expect("snapshot file present");
    let mut expected: serde_json::Value = serde_json::from_str(&expected_str).unwrap();
    let mut actual_norm = actual.clone();
    // Normalize volatile fields (timestamps).
    normalize(&mut expected);
    normalize(&mut actual_norm);
    assert_eq!(expected, actual_norm, "snapshot drift in {name}");
}

fn normalize(v: &mut serde_json::Value) {
    if let Some(obj) = v.as_object_mut() {
        if obj.contains_key("generated_at") {
            obj["generated_at"] = serde_json::json!("<normalized>");
        }
        for (_, vv) in obj.iter_mut() {
            normalize(vv);
        }
    } else if let Some(arr) = v.as_array_mut() {
        for vv in arr {
            normalize(vv);
        }
    }
}

#[tokio::test]
async fn triage_snapshot() {
    let e = canonical_engine().await;
    let r = e.triage(None).await.unwrap();
    assert_snapshot(&r.raw, "triage");
}

#[tokio::test]
async fn plan_snapshot() {
    let e = canonical_engine().await;
    let r = e.plan(None).await.unwrap();
    assert_snapshot(&r.raw, "plan");
}

#[tokio::test]
async fn insights_snapshot() {
    let e = canonical_engine().await;
    let r = e.insights(None).await.unwrap();
    assert_snapshot(&r.raw, "insights");
}

#[tokio::test]
async fn alerts_snapshot() {
    let e = canonical_engine().await;
    let r = e.alerts().await.unwrap();
    assert_snapshot(&r.raw, "alerts");
}

#[tokio::test]
async fn subgraph_snapshot() {
    let e = canonical_engine().await;
    let r = e.subgraph("bd-1", Some(1), Some("json")).await.unwrap();
    assert_snapshot(&r.raw, "subgraph");
}
```

- [ ] **Step 2: Generate initial snapshots**

Run: `UPDATE_SNAPSHOTS=1 cargo test -p spur-pm --test mcp_passthrough_snapshots`
Expected: all 5 snapshots created in `tests/snapshots/`.

- [ ] **Step 3: Re-run without env var to confirm stability**

Run: `cargo test -p spur-pm --test mcp_passthrough_snapshots`
Expected: 5 passed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-pm/tests/mcp_passthrough_snapshots.rs crates/spur-pm/tests/snapshots/
git commit -m "test(graph-engine): MCP raw-passthrough snapshot regression suite"
```

---

## Task 19: Install scripts + docs cleanup

**Files:**
- Modify: any `scripts/install*.sh`, `scripts/setup*.sh`, `Brewfile`, `Justfile`, `Makefile`, or developer-onboarding docs that reference `bv`.
- Modify: any README that documents installing `bv`.

- [ ] **Step 1: Find all references to `bv` install**

Run: `grep -rn 'tap/bv\|brew install.*bv\|cargo install.*beads_viewer\|bv binary' --include='*.sh' --include='*.toml' --include='*.md' .`
Expected: small list of files (install scripts, README, possibly CI config).

- [ ] **Step 2: Remove `bv` install steps**

For each file: delete the `bv` install line / paragraph. If a README has a "Prerequisites" section, drop the `bv` bullet. Keep the `br` install line — it's still required for the companion `BeadsCrateAdapter` (until decommission).

- [ ] **Step 3: Verify SPUR boots without `bv` on PATH**

Run: `which bv 2>/dev/null && rm "$(which bv)"; cargo run --bin spur-mcp -- --help`
(only if you're comfortable removing the binary from your dev environment; otherwise just confirm `cargo test --workspace` passes without `bv` being probed.)

- [ ] **Step 4: Commit**

```bash
git add <list of modified files>
git commit -m "docs/scripts: drop bv install — graph engine is now in-process"
```

---

## Self-Review

After writing the plan, re-check against the spec:

**Spec coverage** — every section maps to one or more tasks:

| Spec section | Task(s) |
|---|---|
| Module layout | T1, T4–T14 (each module file) |
| `GraphSnapshot` value type | T1 |
| Loader | T2 |
| `data_hash` strategy | T3 |
| `triage` SPUR-owned semantics | T6 (score), T7 (triage) |
| `plan` topological generations | T8 |
| `insights` categories | T9 (HITS), T10 (Brandes), T11 (k-core), T12 (assemble) |
| `alerts` thresholds & types | T5 |
| `subgraph` formats | T4 |
| `raw` field generation | T13 |
| `GraphEngine` facade | T14 |
| `BvAdapter` swap | T16 |
| `PmService` wiring | T17 |
| Integration tests | T15 |
| MCP passthrough snapshot tests | T18 |
| Install / docs cleanup | T19 |
| Sequencing prerequisite (companion plan) | Documented in T15 + T16 preconditions |

**Placeholders**: none — every step has concrete code, exact commands, and expected output.

**Type consistency**: `GraphSnapshot`, `NodeData`, `EdgeData`, `DependencyKind`, `ScoreConfig`, `ScoreBreakdown`, `AlertConfig`, `InsightConfig`, `GraphEngine`, `GraphEngineConfig`, `BvAdapter`, `BeadsCrateAdapter`, `TestBeadsWorkspace`, `GraphFormat`, `SubgraphRoot` are used consistently throughout. Wire types (`TriageReport`, `ExecutionPlan`, `GraphInsights`, `AlertReport`, `DependencyGraph`, `Recommendation`, `QuickWin`, `BlockerInfo`, `ProjectHealth`, `HealthCounts`, `GraphHealth`, `Alert`, `AlertSummary`, `WhatIfEntry`, `InsightItem`, `GraphNode`, `GraphEdge`, `AdjacencyData`, `TriageResult`, `QuickRef`, `TopPick`, `ExecutionTrack`, `TrackItem`, `PlanBody`, `PlanSummary`) all come from the existing `crates/spur-pm/src/graph.rs` and are used unchanged.

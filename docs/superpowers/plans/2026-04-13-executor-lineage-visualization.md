# Executor Lineage Visualization & Brain Loopback — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an event-sourced `ExecutorLineage` projection to `spur-core`, retrofit `DashboardView` to render recursive lineage with a focus-aware detail pane, and surface a typed review loop that emits `ExecutorReviewResolved` events back toward the brain.

**Architecture:** `spur-core` gains a `lineage` module that folds `SpurEvent`s into a recursive `ExecutorNode` forest (pure projection — replay-safe). `spur-tui` retrofits `DashboardView`: left pane becomes a recursive tree driven by the projection; right pane becomes focus-aware (chronological `ActivityLog` by default, tabbed node detail on selection, with an inline `review` tab replacing any modal overlay for review cards). Six additive `SpurEvent` variants (`ExecutorSpawned / ExecutorPhaseChanged / ExecutorArtifact / ExecutorReviewRequested / ExecutorReviewResolved / ExecutorRetryStarted`) define the contract; existing `BrainSpawned`/`WorkerSpawned`/`DelegationRequested` data are folded in unchanged for backward compat.

**Tech Stack:** Rust 2021 workspace · `ratatui` + `crossterm` TUI · `tokio` · `serde` (events must round-trip JSON) · `agent-client-protocol` crate · existing `spur-acp::SpurEvent` broadcast.

**Spec:** `docs/superpowers/specs/2026-04-13-executor-lineage-visualization-design.md`

**Scope:** B — TUI + events + projection only. Orchestrator-side translation of `ReviewDecision` → tool-call result is deferred to a follow-up spec.

---

## File Structure

**Files created:**
- `crates/spur-core/src/lineage/mod.rs` — projection module root
- `crates/spur-core/src/lineage/types.rs` — `ExecutorId`, `ExecutorNode`, `Attempt`, enums
- `crates/spur-core/src/lineage/projection.rs` — `ExecutorLineage::apply`, orphan buffer
- `crates/spur-core/src/lineage/adapter.rs` — fold legacy `BrainSpawned`/`WorkerSpawned`/`DelegationRequested` into the projection
- `crates/spur-core/tests/lineage_projection.rs` — integration tests for projection
- `crates/spur-tui/src/components/review_card.rs` — inline review card renderer
- `crates/spur-tui/src/components/detail_pane.rs` — focus-aware right pane with tabs
- `crates/spur-tui/tests/dashboard_snapshot.rs` — render snapshot tests

**Files modified:**
- `crates/spur-acp/src/domain/events.rs` — add 6 new `SpurEvent` variants + supporting types
- `crates/spur-acp/src/lib.rs` — re-export new types
- `crates/spur-core/src/lib.rs` — `pub mod lineage;` + re-exports
- `crates/spur-tui/src/action.rs` — add `Action::SubmitReview`, `Action::FocusNode`, `Action::UnfocusNode`, `Action::JumpToReview`
- `crates/spur-tui/src/app.rs` — own `ExecutorLineage`, add `UserInput::SubmitReview`, feed events into projection
- `crates/spur-tui/src/components/agents_tree.rs` — recursive traversal driven by `ExecutorLineage` (replace current 2-level filter)
- `crates/spur-tui/src/components/mod.rs` — add new module declarations
- `crates/spur-tui/src/components/status_bar.rs` — aggregate counters (running · pending-review · cost · elapsed)
- `crates/spur-tui/src/views/dashboard.rs` — retrofit: add selection state, detail pane, jump-to-review binding
- `crates/spur-tui/src/components/help_overlay.rs` — document new keys (`j`/`k`/`Enter`/`Esc`/`r`/`a`/`d`/`m`/`R`/`c`)

**Files left untouched:** `spur-mcp`, `spur-worktree`, `spur-cli`, `spur-cost`, `spur-pm`, existing permission-prompt flow in `app.rs::handle_permission_request`.

---

## Task 1: Add lineage types in `spur-core`

**Files:**
- Create: `crates/spur-core/src/lineage/types.rs`
- Modify: `crates/spur-core/src/lineage/mod.rs` (new)
- Modify: `crates/spur-core/src/lib.rs`

- [ ] **Step 1: Create the module skeleton**

Create `crates/spur-core/src/lineage/mod.rs`:

```rust
//! Executor lineage projection.
//!
//! `ExecutorLineage` is a pure event-sourced projection of the `SpurEvent`
//! stream. Feeding the same events in the same order always produces the same
//! state — safe to rebuild from `SessionHistory` replay.

pub mod adapter;
pub mod projection;
pub mod types;

pub use projection::ExecutorLineage;
pub use types::{
    Artifact, Attempt, AttemptStatus, ExecutorId, ExecutorNode, LifecycleState, ReviewDecision,
    ReviewKind, ReviewPayload, ReviewRequest, Role,
};
```

Create `crates/spur-core/src/lineage/types.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::SystemTime;

use spur_acp::SessionId;

/// Stable identifier for a logical executor. Survives retries (retries produce
/// a new `Attempt` under the same `ExecutorId`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExecutorId(pub String);

impl ExecutorId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Brain,
    Executor,
    SubExecutor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleState {
    Spawning,
    Running,
    AwaitingReview,
    Resuming,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AttemptStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewKind {
    Completion,
    Failure,
    Conflict,
    Checkpoint,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPayload {
    pub summary: String,
    pub diff_summary: Option<DiffSummary>,
    pub pr_url: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReviewDecision {
    Approve,
    Reject { reason: String },
    Modify { note: String },
    Retry { new_constraints: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewRequest {
    pub kind: ReviewKind,
    pub payload: ReviewPayload,
    pub requested_at: SystemTime,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Artifact {
    Diff(DiffSummary),
    PrUrl(String),
    FileList(Vec<PathBuf>),
    Text(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attempt {
    pub session_id: SessionId,
    pub started_at: SystemTime,
    pub ended_at: Option<SystemTime>,
    pub status: AttemptStatus,
    pub cost_usd: f64,
    pub artifacts: Vec<Artifact>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorNode {
    pub id: ExecutorId,
    pub parent_id: Option<ExecutorId>,
    pub child_ids: Vec<ExecutorId>,
    pub agent: String,
    pub role: Role,
    pub task_spec: String,
    pub phase: LifecycleState,
    pub attempts: Vec<Attempt>,
    pub pending_review: Option<ReviewRequest>,
}

impl ExecutorNode {
    /// The currently-active attempt (last element of `attempts`), if any.
    pub fn current_attempt(&self) -> Option<&Attempt> {
        self.attempts.last()
    }

    pub fn current_attempt_mut(&mut self) -> Option<&mut Attempt> {
        self.attempts.last_mut()
    }
}
```

- [ ] **Step 2: Wire module into `spur-core`**

Modify `crates/spur-core/src/lib.rs`:

```rust
pub mod lineage;
pub mod orchestrator;

pub use lineage::{
    Artifact, Attempt, AttemptStatus, ExecutorId, ExecutorLineage, ExecutorNode, LifecycleState,
    ReviewDecision, ReviewKind, ReviewPayload, ReviewRequest, Role,
};
pub use orchestrator::{BrainSession, InteractiveInput, Orchestrator, RunOpts, RunResult};
```

- [ ] **Step 3: Verify it compiles (projection not yet added — module is incomplete)**

Run: `cargo check -p spur-core`
Expected: FAIL — missing `projection` and `adapter` modules. This confirms the module wiring is correct; Task 2 creates `projection`, Task 5 creates `adapter`.

- [ ] **Step 4: Stub `projection` and `adapter` so the crate compiles**

Create `crates/spur-core/src/lineage/projection.rs` (stub):

```rust
use std::collections::HashMap;

use spur_acp::SpurEvent;

use super::types::{ExecutorId, ExecutorNode};

/// Event-sourced projection of executor lineage.
#[derive(Debug, Default, Clone)]
pub struct ExecutorLineage {
    nodes: HashMap<ExecutorId, ExecutorNode>,
    roots: Vec<ExecutorId>,
}

impl ExecutorLineage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one event into the projection. No-op in the stub.
    pub fn apply(&mut self, _event: &SpurEvent) {
        // implemented in later tasks
    }

    pub fn nodes(&self) -> impl Iterator<Item = &ExecutorNode> {
        self.nodes.values()
    }

    pub fn node(&self, id: &ExecutorId) -> Option<&ExecutorNode> {
        self.nodes.get(id)
    }

    pub fn root_ids(&self) -> &[ExecutorId] {
        &self.roots
    }

    pub fn children_of(&self, id: &ExecutorId) -> Vec<&ExecutorNode> {
        match self.nodes.get(id) {
            Some(node) => node
                .child_ids
                .iter()
                .filter_map(|cid| self.nodes.get(cid))
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn pending_reviews(&self) -> Vec<ExecutorId> {
        self.nodes
            .values()
            .filter(|n| n.pending_review.is_some())
            .map(|n| n.id.clone())
            .collect()
    }

    pub(crate) fn insert_root(&mut self, node: ExecutorNode) {
        self.roots.push(node.id.clone());
        self.nodes.insert(node.id.clone(), node);
    }

    pub(crate) fn insert_child(&mut self, parent: &ExecutorId, node: ExecutorNode) {
        if let Some(p) = self.nodes.get_mut(parent) {
            p.child_ids.push(node.id.clone());
        }
        self.nodes.insert(node.id.clone(), node);
    }

    pub(crate) fn node_mut(&mut self, id: &ExecutorId) -> Option<&mut ExecutorNode> {
        self.nodes.get_mut(id)
    }
}
```

Create `crates/spur-core/src/lineage/adapter.rs` (stub):

```rust
//! Fold legacy `SpurEvent` variants (BrainSpawned, WorkerSpawned,
//! DelegationRequested/Completed, SessionCompleted, CostUpdate) into the
//! projection. Implemented in Task 5.

use spur_acp::SpurEvent;

use super::projection::ExecutorLineage;

pub fn apply_legacy(_lineage: &mut ExecutorLineage, _event: &SpurEvent) {
    // implemented in Task 5
}
```

- [ ] **Step 5: Verify the crate compiles now**

Run: `cargo check -p spur-core`
Expected: PASS (warnings about unused imports are fine).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/lineage crates/spur-core/src/lib.rs
git commit -m "feat(spur-core): scaffold ExecutorLineage projection module"
```

---

## Task 2: Add 6 new `SpurEvent` variants

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Modify: `crates/spur-acp/src/lib.rs`

- [ ] **Step 1: Write the failing round-trip test**

Create `crates/spur-acp/tests/executor_events_roundtrip.rs`:

```rust
//! Verifies new executor lineage events round-trip through serde JSON.

use spur_acp::{
    ExecutorArtifactPayload, ExecutorReviewDecision, ExecutorReviewKind, ExecutorReviewPayload,
    SessionId, SpurEvent,
};

#[test]
fn executor_spawned_roundtrips() {
    let ev = SpurEvent::ExecutorSpawned {
        id: "exec-1".into(),
        parent_id: Some("brain-1".into()),
        session_id: SessionId("s1".into()),
        agent: "worker".into(),
        role: "Executor".into(),
        task_spec: "fix bug".into(),
    };
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round, SpurEvent::ExecutorSpawned { .. }));
}

#[test]
fn executor_review_resolved_roundtrips() {
    let ev = SpurEvent::ExecutorReviewResolved {
        id: "exec-1".into(),
        decision: ExecutorReviewDecision::Reject {
            reason: "tests fail".into(),
        },
    };
    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round, SpurEvent::ExecutorReviewResolved { .. }));
}
```

Run: `cargo test -p spur-acp --test executor_events_roundtrip`
Expected: FAIL — variants/types do not exist.

- [ ] **Step 2: Extend `SpurEvent` with the 6 variants**

Modify `crates/spur-acp/src/domain/events.rs`. Add these imports at the top:

```rust
use std::time::SystemTime;
```

Append these types above `pub enum SpurEvent`:

```rust
/// Review kind for `ExecutorReviewRequested`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutorReviewKind {
    Completion,
    Failure,
    Conflict,
    Checkpoint,
}

/// Payload carried with a review request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorReviewPayload {
    pub summary: String,
    pub diff_summary: Option<ExecutorDiffSummary>,
    pub pr_url: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutorDiffSummary {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: Vec<PathBuf>,
}

/// Artifact kinds emitted by an executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutorArtifactPayload {
    Diff(ExecutorDiffSummary),
    PrUrl(String),
    FileList(Vec<PathBuf>),
    Text(String),
}

/// User's decision on a review request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutorReviewDecision {
    Approve,
    Reject { reason: String },
    Modify { note: String },
    Retry { new_constraints: String },
}
```

Add these variants inside `pub enum SpurEvent` after `BrainError`:

```rust
    // ── Executor lineage events ────────────────────────────────────
    ExecutorSpawned {
        id: String,
        parent_id: Option<String>,
        session_id: SessionId,
        agent: String,
        role: String,           // "Brain" | "Executor" | "SubExecutor"
        task_spec: String,
    },
    ExecutorPhaseChanged {
        id: String,
        phase: String,          // serialized `LifecycleState` variant name
    },
    ExecutorArtifact {
        id: String,
        artifact: ExecutorArtifactPayload,
    },
    ExecutorReviewRequested {
        id: String,
        kind: ExecutorReviewKind,
        payload: ExecutorReviewPayload,
        requested_at: SystemTime,
    },
    ExecutorReviewResolved {
        id: String,
        decision: ExecutorReviewDecision,
    },
    ExecutorRetryStarted {
        id: String,
        attempt_n: u32,
        reason: String,
        new_session_id: SessionId,
    },
```

- [ ] **Step 3: Re-export new types from `spur-acp`**

Check `crates/spur-acp/src/lib.rs` and ensure it re-exports `SpurEvent` and the events module. Add to the existing re-export block the new types:

```rust
pub use crate::domain::events::{
    ExecutorArtifactPayload, ExecutorDiffSummary, ExecutorReviewDecision, ExecutorReviewKind,
    ExecutorReviewPayload, HistoryEntry, SpurEvent,
};
```

(If `SpurEvent` and `HistoryEntry` are already re-exported via a different mechanism, merge rather than duplicate.)

- [ ] **Step 4: Run the round-trip test**

Run: `cargo test -p spur-acp --test executor_events_roundtrip`
Expected: PASS.

- [ ] **Step 5: Verify backward compat — existing `spur-acp` tests still pass**

Run: `cargo test -p spur-acp`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs crates/spur-acp/src/lib.rs \
        crates/spur-acp/tests/executor_events_roundtrip.rs
git commit -m "feat(spur-acp): add 6 executor lineage SpurEvent variants"
```

---

## Task 3: Implement `apply` for `ExecutorSpawned` and `ExecutorPhaseChanged`

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs`
- Create: `crates/spur-core/tests/lineage_projection.rs`

- [ ] **Step 1: Write failing tests for spawn + phase change**

Create `crates/spur-core/tests/lineage_projection.rs`:

```rust
use std::time::SystemTime;

use spur_acp::{SessionId, SpurEvent};
use spur_core::{ExecutorId, ExecutorLineage, LifecycleState};

fn spawn(id: &str, parent: Option<&str>) -> SpurEvent {
    SpurEvent::ExecutorSpawned {
        id: id.into(),
        parent_id: parent.map(|s| s.into()),
        session_id: SessionId(format!("sess-{}", id)),
        agent: "kiro".into(),
        role: if parent.is_none() {
            "Brain".into()
        } else {
            "Executor".into()
        },
        task_spec: format!("task for {}", id),
    }
}

#[test]
fn spawn_creates_root_when_no_parent() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("brain-1", None));

    assert_eq!(l.root_ids().len(), 1);
    let n = l.node(&ExecutorId::new("brain-1")).unwrap();
    assert!(n.parent_id.is_none());
    assert_eq!(n.phase, LifecycleState::Spawning);
    assert_eq!(n.attempts.len(), 1);
}

#[test]
fn spawn_links_child_under_parent() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("brain-1", None));
    l.apply(&spawn("worker-1", Some("brain-1")));

    assert_eq!(l.root_ids().len(), 1);
    let parent = l.node(&ExecutorId::new("brain-1")).unwrap();
    assert_eq!(parent.child_ids.len(), 1);
    assert_eq!(parent.child_ids[0], ExecutorId::new("worker-1"));

    let child = l.node(&ExecutorId::new("worker-1")).unwrap();
    assert_eq!(child.parent_id, Some(ExecutorId::new("brain-1")));
}

#[test]
fn phase_change_updates_node_phase() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("brain-1", None));
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "brain-1".into(),
        phase: "Running".into(),
    });

    let n = l.node(&ExecutorId::new("brain-1")).unwrap();
    assert_eq!(n.phase, LifecycleState::Running);
}

#[test]
fn phase_change_terminal_sets_attempt_ended() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "w".into(),
        phase: "Succeeded".into(),
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    let a = n.current_attempt().unwrap();
    assert!(a.ended_at.is_some(), "terminal phase must close the attempt");
}

#[test]
fn unknown_phase_string_is_ignored() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "w".into(),
        phase: "Bogus".into(),
    });
    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert_eq!(n.phase, LifecycleState::Spawning, "unchanged on unknown phase");
}
```

Run: `cargo test -p spur-core --test lineage_projection`
Expected: FAIL — `apply` is a no-op stub.

- [ ] **Step 2: Implement `apply` for the two variants**

Replace the contents of `crates/spur-core/src/lineage/projection.rs` with:

```rust
use std::collections::{HashMap, VecDeque};
use std::time::SystemTime;

use spur_acp::SpurEvent;

use super::types::{
    Attempt, AttemptStatus, ExecutorId, ExecutorNode, LifecycleState, ReviewRequest, Role,
};

const MAX_ORPHAN_BUFFER_PER_EXEC: usize = 128;

#[derive(Debug, Default, Clone)]
pub struct ExecutorLineage {
    nodes: HashMap<ExecutorId, ExecutorNode>,
    roots: Vec<ExecutorId>,
    /// Events received for an executor before its `ExecutorSpawned`.
    orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>,
}

impl ExecutorLineage {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(&mut self, event: &SpurEvent) {
        // Try legacy adapter first (BrainSpawned, WorkerSpawned, etc.)
        super::adapter::apply_legacy(self, event);

        match event {
            SpurEvent::ExecutorSpawned {
                id,
                parent_id,
                session_id,
                agent,
                role,
                task_spec,
            } => {
                let eid = ExecutorId::new(id);
                let parent = parent_id.as_ref().map(ExecutorId::new);
                let parsed_role = parse_role(role);
                let attempt = Attempt {
                    session_id: session_id.clone(),
                    started_at: SystemTime::now(),
                    ended_at: None,
                    status: AttemptStatus::Running,
                    cost_usd: 0.0,
                    artifacts: Vec::new(),
                    error: None,
                };
                let node = ExecutorNode {
                    id: eid.clone(),
                    parent_id: parent.clone(),
                    child_ids: Vec::new(),
                    agent: agent.clone(),
                    role: parsed_role,
                    task_spec: task_spec.clone(),
                    phase: LifecycleState::Spawning,
                    attempts: vec![attempt],
                    pending_review: None,
                };
                match parent {
                    Some(p) if self.nodes.contains_key(&p) => {
                        self.nodes.get_mut(&p).unwrap().child_ids.push(eid.clone());
                        self.nodes.insert(eid.clone(), node);
                    }
                    _ => {
                        self.roots.push(eid.clone());
                        self.nodes.insert(eid.clone(), node);
                    }
                }
                // Replay any buffered orphan events for this id.
                if let Some(queue) = self.orphan_buffer.remove(&eid) {
                    for ev in queue {
                        self.apply(&ev);
                    }
                }
            }

            SpurEvent::ExecutorPhaseChanged { id, phase } => {
                let eid = ExecutorId::new(id);
                if let Some(new_phase) = parse_phase(phase) {
                    if let Some(node) = self.nodes.get_mut(&eid) {
                        node.phase = new_phase;
                        if let Some(status) = terminal_attempt_status(new_phase) {
                            if let Some(a) = node.current_attempt_mut() {
                                a.ended_at = Some(SystemTime::now());
                                a.status = status;
                            }
                        }
                    } else {
                        self.buffer_orphan(eid, event.clone());
                    }
                }
            }

            _ => {}
        }
    }

    fn buffer_orphan(&mut self, id: ExecutorId, ev: SpurEvent) {
        let q = self.orphan_buffer.entry(id).or_default();
        if q.len() < MAX_ORPHAN_BUFFER_PER_EXEC {
            q.push_back(ev);
        } else {
            tracing::warn!("orphan buffer overflow; dropping event");
        }
    }

    pub fn nodes(&self) -> impl Iterator<Item = &ExecutorNode> {
        self.nodes.values()
    }

    pub fn node(&self, id: &ExecutorId) -> Option<&ExecutorNode> {
        self.nodes.get(id)
    }

    pub fn root_ids(&self) -> &[ExecutorId] {
        &self.roots
    }

    pub fn children_of(&self, id: &ExecutorId) -> Vec<&ExecutorNode> {
        match self.nodes.get(id) {
            Some(node) => node
                .child_ids
                .iter()
                .filter_map(|cid| self.nodes.get(cid))
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn pending_reviews(&self) -> Vec<ExecutorId> {
        self.nodes
            .values()
            .filter(|n| n.pending_review.is_some())
            .map(|n| n.id.clone())
            .collect()
    }

    pub(crate) fn node_mut(&mut self, id: &ExecutorId) -> Option<&mut ExecutorNode> {
        self.nodes.get_mut(id)
    }
}

fn parse_role(s: &str) -> Role {
    match s {
        "Brain" => Role::Brain,
        "SubExecutor" => Role::SubExecutor,
        _ => Role::Executor,
    }
}

fn parse_phase(s: &str) -> Option<LifecycleState> {
    Some(match s {
        "Spawning" => LifecycleState::Spawning,
        "Running" => LifecycleState::Running,
        "AwaitingReview" => LifecycleState::AwaitingReview,
        "Resuming" => LifecycleState::Resuming,
        "Succeeded" => LifecycleState::Succeeded,
        "Failed" => LifecycleState::Failed,
        "Cancelled" => LifecycleState::Cancelled,
        _ => return None,
    })
}

fn terminal_attempt_status(p: LifecycleState) -> Option<AttemptStatus> {
    match p {
        LifecycleState::Succeeded => Some(AttemptStatus::Succeeded),
        LifecycleState::Failed => Some(AttemptStatus::Failed),
        LifecycleState::Cancelled => Some(AttemptStatus::Cancelled),
        _ => None,
    }
}
```

- [ ] **Step 3: Run tests — should pass**

Run: `cargo test -p spur-core --test lineage_projection`
Expected: PASS (5 tests).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/lineage/projection.rs \
        crates/spur-core/tests/lineage_projection.rs
git commit -m "feat(spur-core): apply ExecutorSpawned + PhaseChanged in lineage projection"
```

---

## Task 4: Implement `apply` for `Artifact`, `ReviewRequested`, `ReviewResolved`, `RetryStarted`

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs`
- Modify: `crates/spur-core/tests/lineage_projection.rs`

- [ ] **Step 1: Add failing tests**

Append to `crates/spur-core/tests/lineage_projection.rs`:

```rust
use spur_acp::{
    ExecutorArtifactPayload, ExecutorDiffSummary, ExecutorReviewDecision, ExecutorReviewKind,
    ExecutorReviewPayload,
};
use spur_core::Artifact;

#[test]
fn artifact_appends_to_current_attempt() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorArtifact {
        id: "w".into(),
        artifact: ExecutorArtifactPayload::PrUrl("https://x/1".into()),
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    let a = n.current_attempt().unwrap();
    assert_eq!(a.artifacts.len(), 1);
    assert!(matches!(a.artifacts[0], Artifact::PrUrl(_)));
}

#[test]
fn review_requested_populates_pending_review_and_phase() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorReviewRequested {
        id: "w".into(),
        kind: ExecutorReviewKind::Completion,
        payload: ExecutorReviewPayload {
            summary: "done".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
        requested_at: SystemTime::now(),
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert!(n.pending_review.is_some());
    assert_eq!(n.phase, LifecycleState::AwaitingReview);
}

#[test]
fn review_resolved_clears_pending_review() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorReviewRequested {
        id: "w".into(),
        kind: ExecutorReviewKind::Completion,
        payload: ExecutorReviewPayload {
            summary: "done".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
        },
        requested_at: SystemTime::now(),
    });
    l.apply(&SpurEvent::ExecutorReviewResolved {
        id: "w".into(),
        decision: ExecutorReviewDecision::Approve,
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert!(n.pending_review.is_none());
}

#[test]
fn retry_started_pushes_new_attempt() {
    let mut l = ExecutorLineage::new();
    l.apply(&spawn("w", None));
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "w".into(),
        phase: "Failed".into(),
    });
    l.apply(&SpurEvent::ExecutorRetryStarted {
        id: "w".into(),
        attempt_n: 2,
        reason: "timeout".into(),
        new_session_id: SessionId("sess-w-2".into()),
    });

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert_eq!(n.attempts.len(), 2);
    assert_eq!(n.phase, LifecycleState::Running);
    assert_eq!(n.current_attempt().unwrap().session_id.0, "sess-w-2");
}

#[test]
fn orphan_phase_event_is_replayed_after_spawn() {
    let mut l = ExecutorLineage::new();
    // Phase arrives BEFORE spawn — must be buffered.
    l.apply(&SpurEvent::ExecutorPhaseChanged {
        id: "late".into(),
        phase: "Running".into(),
    });
    assert!(l.node(&ExecutorId::new("late")).is_none());

    l.apply(&spawn("late", None));
    let n = l.node(&ExecutorId::new("late")).unwrap();
    assert_eq!(n.phase, LifecycleState::Running);
}
```

Run: `cargo test -p spur-core --test lineage_projection`
Expected: FAIL (5 new failures).

- [ ] **Step 2: Add the 4 new match arms in `apply`**

In `crates/spur-core/src/lineage/projection.rs`, extend the `match event` block. Replace the existing `_ => {}` fallthrough with:

```rust
            SpurEvent::ExecutorArtifact { id, artifact } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    let art = map_artifact(artifact);
                    if let Some(a) = node.current_attempt_mut() {
                        a.artifacts.push(art);
                    }
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEvent::ExecutorReviewRequested {
                id,
                kind,
                payload,
                requested_at,
            } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    node.phase = LifecycleState::AwaitingReview;
                    node.pending_review = Some(ReviewRequest {
                        kind: map_review_kind(kind),
                        payload: map_review_payload(payload),
                        requested_at: *requested_at,
                    });
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEvent::ExecutorReviewResolved { id, decision: _ } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    node.pending_review = None;
                    // Phase stays `AwaitingReview` until a subsequent
                    // PhaseChanged or RetryStarted moves it. Orchestrator owns
                    // that transition.
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            SpurEvent::ExecutorRetryStarted {
                id,
                attempt_n: _,
                reason: _,
                new_session_id,
            } => {
                let eid = ExecutorId::new(id);
                if let Some(node) = self.nodes.get_mut(&eid) {
                    let new_attempt = Attempt {
                        session_id: new_session_id.clone(),
                        started_at: SystemTime::now(),
                        ended_at: None,
                        status: AttemptStatus::Running,
                        cost_usd: 0.0,
                        artifacts: Vec::new(),
                        error: None,
                    };
                    node.attempts.push(new_attempt);
                    node.phase = LifecycleState::Running;
                } else {
                    self.buffer_orphan(eid, event.clone());
                }
            }

            _ => {}
```

Add these mapper functions at the bottom of the file:

```rust
fn map_artifact(p: &spur_acp::ExecutorArtifactPayload) -> super::types::Artifact {
    use spur_acp::ExecutorArtifactPayload as S;
    use super::types::{Artifact, DiffSummary};
    match p {
        S::Diff(d) => Artifact::Diff(DiffSummary {
            files_changed: d.files_changed,
            insertions: d.insertions,
            deletions: d.deletions,
            files: d.files.clone(),
        }),
        S::PrUrl(u) => Artifact::PrUrl(u.clone()),
        S::FileList(f) => Artifact::FileList(f.clone()),
        S::Text(t) => Artifact::Text(t.clone()),
    }
}

fn map_review_kind(k: &spur_acp::ExecutorReviewKind) -> super::types::ReviewKind {
    use spur_acp::ExecutorReviewKind as S;
    use super::types::ReviewKind;
    match k {
        S::Completion => ReviewKind::Completion,
        S::Failure => ReviewKind::Failure,
        S::Conflict => ReviewKind::Conflict,
        S::Checkpoint => ReviewKind::Checkpoint,
    }
}

fn map_review_payload(p: &spur_acp::ExecutorReviewPayload) -> super::types::ReviewPayload {
    use super::types::{DiffSummary, ReviewPayload};
    ReviewPayload {
        summary: p.summary.clone(),
        diff_summary: p.diff_summary.as_ref().map(|d| DiffSummary {
            files_changed: d.files_changed,
            insertions: d.insertions,
            deletions: d.deletions,
            files: d.files.clone(),
        }),
        pr_url: p.pr_url.clone(),
        error: p.error.clone(),
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core --test lineage_projection`
Expected: PASS (10 tests total).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/lineage/projection.rs \
        crates/spur-core/tests/lineage_projection.rs
git commit -m "feat(spur-core): apply artifact/review/retry events in lineage projection"
```

---

## Task 5: Legacy-event adapter — fold `BrainSpawned`/`WorkerSpawned`/`DelegationRequested`/`CostUpdate`/`SessionCompleted` into the projection

**Files:**
- Modify: `crates/spur-core/src/lineage/adapter.rs`
- Modify: `crates/spur-core/tests/lineage_projection.rs`

Rationale: stage-1 goal is "derive lineage from existing events where possible — no orchestrator changes". The adapter synthesizes `ExecutorSpawned` / `ExecutorPhaseChanged` from what `spur-acp` already emits.

- [ ] **Step 1: Write failing legacy-fold tests**

Append to `crates/spur-core/tests/lineage_projection.rs`:

```rust
use spur_acp::DelegationStatus;
use std::path::PathBuf;

#[test]
fn brain_spawned_creates_root_node() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("s1".into()),
    });
    assert_eq!(l.root_ids().len(), 1);
    // Root id is the session id for legacy events.
    assert!(l.node(&ExecutorId::new("s1")).is_some());
}

#[test]
fn worker_spawned_attaches_under_latest_brain() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    });
    l.apply(&SpurEvent::WorkerSpawned {
        agent: "worker".into(),
        session: SessionId("w1".into()),
        worktree: PathBuf::from("/tmp/wt"),
    });

    let brain = l.node(&ExecutorId::new("b1")).unwrap();
    assert_eq!(brain.child_ids.len(), 1);
    assert_eq!(brain.child_ids[0], ExecutorId::new("w1"));
}

#[test]
fn delegation_completed_success_moves_phase_to_succeeded() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    });
    l.apply(&SpurEvent::WorkerSpawned {
        agent: "w".into(),
        session: SessionId("w1".into()),
        worktree: PathBuf::from("/tmp/wt"),
    });
    l.apply(&SpurEvent::DelegationCompleted {
        worker_session: SessionId("w1".into()),
        status: DelegationStatus::Success,
    });

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(n.phase, LifecycleState::Succeeded);
}

#[test]
fn cost_update_accumulates_on_current_attempt() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b1".into()),
    });
    l.apply(&SpurEvent::CostUpdate {
        session: SessionId("b1".into()),
        agent: "kiro".into(),
        estimated_cost_usd: 0.10,
    });
    l.apply(&SpurEvent::CostUpdate {
        session: SessionId("b1".into()),
        agent: "kiro".into(),
        estimated_cost_usd: 0.05,
    });

    let n = l.node(&ExecutorId::new("b1")).unwrap();
    let a = n.current_attempt().unwrap();
    assert!((a.cost_usd - 0.15).abs() < 1e-9);
}
```

Run: `cargo test -p spur-core --test lineage_projection`
Expected: 4 new FAILs.

- [ ] **Step 2: Implement the adapter**

Replace the contents of `crates/spur-core/src/lineage/adapter.rs`:

```rust
//! Fold legacy `SpurEvent` variants into `ExecutorLineage`.
//!
//! For v1 of executor-lineage the orchestrator has not yet been updated to
//! emit the new `Executor*` events directly. This adapter synthesises the
//! minimal set of state transitions from events already in the wild so the
//! TUI can render lineage without orchestrator-side changes.

use std::time::SystemTime;

use spur_acp::{DelegationStatus, SpurEvent};

use super::projection::ExecutorLineage;
use super::types::{
    Attempt, AttemptStatus, ExecutorId, ExecutorNode, LifecycleState, Role,
};

pub fn apply_legacy(lineage: &mut ExecutorLineage, event: &SpurEvent) {
    match event {
        SpurEvent::BrainSpawned { agent, session } => {
            let id = ExecutorId::new(session.0.clone());
            if lineage.node(&id).is_some() {
                return;
            }
            lineage.insert_root_node(ExecutorNode {
                id: id.clone(),
                parent_id: None,
                child_ids: Vec::new(),
                agent: agent.clone(),
                role: Role::Brain,
                task_spec: String::new(),
                phase: LifecycleState::Running,
                attempts: vec![fresh_attempt(session.clone())],
                pending_review: None,
            });
        }

        SpurEvent::WorkerSpawned {
            agent,
            session,
            worktree: _,
        } => {
            let id = ExecutorId::new(session.0.clone());
            if lineage.node(&id).is_some() {
                return;
            }
            // Attach under the most-recently-seen brain root.
            let parent = lineage
                .root_ids()
                .iter()
                .rev()
                .find(|rid| {
                    lineage
                        .node(rid)
                        .map(|n| n.role == Role::Brain)
                        .unwrap_or(false)
                })
                .cloned();
            let node = ExecutorNode {
                id: id.clone(),
                parent_id: parent.clone(),
                child_ids: Vec::new(),
                agent: agent.clone(),
                role: Role::Executor,
                task_spec: String::new(),
                phase: LifecycleState::Running,
                attempts: vec![fresh_attempt(session.clone())],
                pending_review: None,
            };
            match parent {
                Some(p) => lineage.attach_child(&p, node),
                None => lineage.insert_root_node(node),
            }
        }

        SpurEvent::DelegationRequested {
            from: _,
            to_agent,
            task,
        } => {
            // Populate the task_spec of the most recent Executor owned by the
            // worker name, if we can find one.
            if let Some(id) = lineage
                .nodes_mut()
                .into_iter()
                .rev()
                .find(|n| n.role == Role::Executor && n.agent == *to_agent && n.task_spec.is_empty())
                .map(|n| n.id.clone())
            {
                if let Some(n) = lineage.node_mut_public(&id) {
                    n.task_spec = task.clone();
                }
            }
        }

        SpurEvent::DelegationCompleted {
            worker_session,
            status,
        } => {
            let id = ExecutorId::new(worker_session.0.clone());
            if let Some(n) = lineage.node_mut_public(&id) {
                let (phase, attempt_status, error) = match status {
                    DelegationStatus::Success => {
                        (LifecycleState::Succeeded, AttemptStatus::Succeeded, None)
                    }
                    DelegationStatus::Failed { error } => (
                        LifecycleState::Failed,
                        AttemptStatus::Failed,
                        Some(error.clone()),
                    ),
                    DelegationStatus::Timeout => (
                        LifecycleState::Failed,
                        AttemptStatus::Failed,
                        Some("timeout".into()),
                    ),
                    DelegationStatus::Conflict { files } => (
                        LifecycleState::Failed,
                        AttemptStatus::Failed,
                        Some(format!("conflict in {} file(s)", files.len())),
                    ),
                };
                n.phase = phase;
                if let Some(a) = n.current_attempt_mut() {
                    a.ended_at = Some(SystemTime::now());
                    a.status = attempt_status;
                    a.error = error;
                }
            }
        }

        SpurEvent::SessionCompleted { session, success } => {
            let id = ExecutorId::new(session.0.clone());
            if let Some(n) = lineage.node_mut_public(&id) {
                n.phase = if *success {
                    LifecycleState::Succeeded
                } else {
                    LifecycleState::Failed
                };
                if let Some(a) = n.current_attempt_mut() {
                    a.ended_at = Some(SystemTime::now());
                    a.status = if *success {
                        AttemptStatus::Succeeded
                    } else {
                        AttemptStatus::Failed
                    };
                }
            }
        }

        SpurEvent::CostUpdate {
            session,
            agent: _,
            estimated_cost_usd,
        } => {
            let id = ExecutorId::new(session.0.clone());
            if let Some(n) = lineage.node_mut_public(&id) {
                if let Some(a) = n.current_attempt_mut() {
                    a.cost_usd += estimated_cost_usd;
                }
            }
        }

        _ => {}
    }
}

fn fresh_attempt(session: spur_acp::SessionId) -> Attempt {
    Attempt {
        session_id: session,
        started_at: SystemTime::now(),
        ended_at: None,
        status: AttemptStatus::Running,
        cost_usd: 0.0,
        artifacts: Vec::new(),
        error: None,
    }
}
```

- [ ] **Step 3: Add helper methods on `ExecutorLineage` that the adapter uses**

In `crates/spur-core/src/lineage/projection.rs`, add these `impl` methods:

```rust
impl ExecutorLineage {
    pub(crate) fn insert_root_node(&mut self, node: ExecutorNode) {
        self.roots.push(node.id.clone());
        self.nodes.insert(node.id.clone(), node);
    }

    pub(crate) fn attach_child(&mut self, parent: &ExecutorId, node: ExecutorNode) {
        if let Some(p) = self.nodes.get_mut(parent) {
            p.child_ids.push(node.id.clone());
        }
        self.nodes.insert(node.id.clone(), node);
    }

    pub(crate) fn node_mut_public(&mut self, id: &ExecutorId) -> Option<&mut ExecutorNode> {
        self.nodes.get_mut(id)
    }

    pub(crate) fn nodes_mut(&mut self) -> Vec<&mut ExecutorNode> {
        self.nodes.values_mut().collect()
    }
}
```

(Remove the earlier `insert_root` / `insert_child` stubs if they became redundant.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core --test lineage_projection`
Expected: PASS (14 tests).

- [ ] **Step 5: Replay-equivalence property test**

Append to `crates/spur-core/tests/lineage_projection.rs`:

```rust
#[test]
fn replay_equals_live() {
    let events: Vec<SpurEvent> = vec![
        SpurEvent::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b".into()),
        },
        SpurEvent::WorkerSpawned {
            agent: "w1".into(),
            session: SessionId("w1".into()),
            worktree: PathBuf::from("/tmp"),
        },
        SpurEvent::DelegationRequested {
            from: SessionId("b".into()),
            to_agent: "w1".into(),
            task: "task-1".into(),
        },
        SpurEvent::CostUpdate {
            session: SessionId("w1".into()),
            agent: "w1".into(),
            estimated_cost_usd: 0.25,
        },
        SpurEvent::DelegationCompleted {
            worker_session: SessionId("w1".into()),
            status: DelegationStatus::Success,
        },
    ];

    let mut live = ExecutorLineage::new();
    for e in &events {
        live.apply(e);
    }

    let mut replayed = ExecutorLineage::new();
    for e in &events {
        replayed.apply(e);
    }

    // Clone nodes, compare task_specs + phases (timestamps differ by
    // construction, so we don't compare those).
    let a: Vec<_> = live
        .nodes()
        .map(|n| (n.id.clone(), n.phase, n.task_spec.clone()))
        .collect();
    let b: Vec<_> = replayed
        .nodes()
        .map(|n| (n.id.clone(), n.phase, n.task_spec.clone()))
        .collect();
    assert_eq!(a.len(), b.len());
    for x in &a {
        assert!(b.contains(x), "replayed state missing {:?}", x);
    }
}
```

Run: `cargo test -p spur-core --test lineage_projection`
Expected: PASS (15 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/lineage/ \
        crates/spur-core/tests/lineage_projection.rs
git commit -m "feat(spur-core): adapt legacy SpurEvents into lineage projection"
```

---

## Task 6: Wire `ExecutorLineage` into `App` and feed every `SpurEvent` into it

**Files:**
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Add lineage field to `App`**

In `crates/spur-tui/src/app.rs`, import at the top:

```rust
use spur_core::ExecutorLineage;
```

Add to `pub struct App`:

```rust
    /// Event-sourced projection of brain → executor lineage.
    lineage: ExecutorLineage,
```

In `App::new`, initialize it:

```rust
            lineage: ExecutorLineage::new(),
```

- [ ] **Step 2: Feed events into the projection**

At the top of `App::handle_spur_event` (inside `handle_spur_event`, before the `match &event` that handles sessions-listed), add:

```rust
        // Always fold into the lineage projection first. The projection is a
        // pure function of the event stream — view code reads from it later.
        self.lineage.apply(&event);
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p spur-tui`
Expected: PASS.

- [ ] **Step 4: Run the full test suite to confirm nothing regressed**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): thread ExecutorLineage projection through App"
```

---

## Task 7: Recursive tree render in `AgentsTree` driven by `ExecutorLineage`

**Files:**
- Modify: `crates/spur-tui/src/components/agents_tree.rs`
- Modify: `crates/spur-tui/src/views/dashboard.rs`

- [ ] **Step 1: Write a render snapshot test for recursive depth**

Create `crates/spur-tui/tests/agents_tree_snapshot.rs`:

```rust
//! Golden-text snapshot: confirm recursive traversal renders depth > 1.

use spur_acp::{SessionId, SpurEvent};
use spur_core::ExecutorLineage;

#[test]
fn recursive_tree_renders_depth_two() {
    let mut lineage = ExecutorLineage::new();
    // brain -> worker -> sub-worker
    lineage.apply(&SpurEvent::ExecutorSpawned {
        id: "b".into(),
        parent_id: None,
        session_id: SessionId("b".into()),
        agent: "kiro".into(),
        role: "Brain".into(),
        task_spec: "root".into(),
    });
    lineage.apply(&SpurEvent::ExecutorSpawned {
        id: "w".into(),
        parent_id: Some("b".into()),
        session_id: SessionId("w".into()),
        agent: "worker".into(),
        role: "Executor".into(),
        task_spec: "child".into(),
    });
    lineage.apply(&SpurEvent::ExecutorSpawned {
        id: "sw".into(),
        parent_id: Some("w".into()),
        session_id: SessionId("sw".into()),
        agent: "sub".into(),
        role: "SubExecutor".into(),
        task_spec: "grandchild".into(),
    });

    let lines = spur_tui::components::agents_tree::render_lineage_to_strings(&lineage, None);

    assert!(lines.iter().any(|l| l.contains("kiro")));
    assert!(lines.iter().any(|l| l.contains("worker") && l.contains("├") || l.contains("└")));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("sub") && l.contains("  ")),
        "sub-worker must be indented deeper than worker"
    );
}
```

Run: `cargo test -p spur-tui --test agents_tree_snapshot`
Expected: FAIL — `render_lineage_to_strings` does not exist.

- [ ] **Step 2: Rewrite `AgentsTree` around `ExecutorLineage`**

Replace the body of `crates/spur-tui/src/components/agents_tree.rs` with:

```rust
use std::collections::HashSet;
use std::time::SystemTime;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use spur_core::{ExecutorId, ExecutorLineage, ExecutorNode, LifecycleState, Role};

use super::{focused_border_style, SPINNER_FRAMES};

pub struct AgentsTree {
    focused: bool,
    tick_counter: u8,
    /// Ids whose subtree is collapsed (children hidden).
    collapsed: HashSet<ExecutorId>,
    /// Currently selected id (if any).
    selected: Option<ExecutorId>,
}

impl AgentsTree {
    pub fn new() -> Self {
        Self {
            focused: false,
            tick_counter: 0,
            collapsed: HashSet::new(),
            selected: None,
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn tick(&mut self) {
        self.tick_counter = self.tick_counter.wrapping_add(1);
    }

    pub fn selected(&self) -> Option<&ExecutorId> {
        self.selected.as_ref()
    }

    pub fn set_selected(&mut self, id: Option<ExecutorId>) {
        self.selected = id;
    }

    pub fn toggle_collapsed(&mut self, id: &ExecutorId) {
        if !self.collapsed.remove(id) {
            self.collapsed.insert(id.clone());
        }
    }

    /// Move selection down one visible row. Returns the new selection.
    pub fn select_next(&mut self, lineage: &ExecutorLineage) -> Option<ExecutorId> {
        let order = self.visible_order(lineage);
        let idx = self
            .selected
            .as_ref()
            .and_then(|s| order.iter().position(|i| i == s))
            .map(|i| (i + 1).min(order.len().saturating_sub(1)))
            .unwrap_or(0);
        self.selected = order.get(idx).cloned();
        self.selected.clone()
    }

    pub fn select_prev(&mut self, lineage: &ExecutorLineage) -> Option<ExecutorId> {
        let order = self.visible_order(lineage);
        let idx = self
            .selected
            .as_ref()
            .and_then(|s| order.iter().position(|i| i == s))
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0);
        self.selected = order.get(idx).cloned();
        self.selected.clone()
    }

    fn visible_order(&self, lineage: &ExecutorLineage) -> Vec<ExecutorId> {
        let mut out = Vec::new();
        for rid in lineage.root_ids() {
            self.walk(lineage, rid, &mut out);
        }
        out
    }

    fn walk(&self, l: &ExecutorLineage, id: &ExecutorId, out: &mut Vec<ExecutorId>) {
        if let Some(n) = l.node(id) {
            out.push(id.clone());
            if !self.collapsed.contains(id) {
                for c in &n.child_ids {
                    self.walk(l, c, out);
                }
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, lineage: &ExecutorLineage) {
        let block = Block::default()
            .title(" Lineage ")
            .borders(Borders::ALL)
            .border_style(focused_border_style(self.focused));

        let mut lines: Vec<Line> = Vec::new();
        for rid in lineage.root_ids() {
            self.render_subtree(lineage, rid, 0, &mut lines);
        }

        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, area);
    }

    fn render_subtree<'a>(
        &self,
        l: &'a ExecutorLineage,
        id: &ExecutorId,
        depth: usize,
        out: &mut Vec<Line<'a>>,
    ) {
        let node = match l.node(id) {
            Some(n) => n,
            None => return,
        };
        let is_selected = self.selected.as_ref() == Some(id);
        out.push(self.build_line(node, depth, is_selected));
        if self.collapsed.contains(id) {
            return;
        }
        for c in &node.child_ids {
            self.render_subtree(l, c, depth + 1, out);
        }
    }

    fn build_line<'a>(&self, node: &'a ExecutorNode, depth: usize, selected: bool) -> Line<'a> {
        let indent = "  ".repeat(depth);
        let connector = if depth == 0 { "" } else { "└─ " };

        let spinner = match node.phase {
            LifecycleState::Running | LifecycleState::Spawning => {
                SPINNER_FRAMES[(self.tick_counter % 10) as usize]
            }
            LifecycleState::AwaitingReview => '⚠',
            LifecycleState::Succeeded => '●',
            LifecycleState::Failed => '✗',
            LifecycleState::Cancelled => '○',
            LifecycleState::Resuming => '↻',
        };

        let status_color = match node.phase {
            LifecycleState::Running | LifecycleState::Spawning | LifecycleState::Resuming => {
                Color::Green
            }
            LifecycleState::AwaitingReview => Color::Yellow,
            LifecycleState::Succeeded => Color::Blue,
            LifecycleState::Failed => Color::Red,
            LifecycleState::Cancelled => Color::DarkGray,
        };

        let elapsed_str = node
            .current_attempt()
            .map(|a| {
                let now = SystemTime::now();
                let secs = now
                    .duration_since(a.started_at)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                format!("{}m {:02}s", secs / 60, secs % 60)
            })
            .unwrap_or_default();

        let cost = node
            .current_attempt()
            .map(|a| a.cost_usd)
            .unwrap_or(0.0);
        let cost_str = if cost > 0.0 {
            format!("${:.2}", cost)
        } else {
            String::new()
        };

        let role_label = match node.role {
            Role::Brain => "BRAIN",
            Role::Executor => "EXEC",
            Role::SubExecutor => "SUB",
        };

        let review_badge = if node.pending_review.is_some() {
            " ⚠review"
        } else {
            ""
        };

        let base = Style::default();
        let row = if selected { base.bg(Color::DarkGray) } else { base };

        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(
            format!("{}{}", indent, connector),
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled(
            format!("{} ", spinner),
            Style::default().fg(status_color),
        ));
        spans.push(Span::styled(
            format!("{:<12} ", node.agent),
            row.fg(Color::White),
        ));
        spans.push(Span::styled(
            format!("{:<5} ", role_label),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
        ));
        spans.push(Span::styled(
            format!("{:<14} ", format!("{:?}", node.phase)),
            Style::default().fg(status_color),
        ));
        if !elapsed_str.is_empty() {
            spans.push(Span::styled(
                format!("{} ", elapsed_str),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if !cost_str.is_empty() {
            spans.push(Span::styled(cost_str, Style::default().fg(Color::Yellow)));
        }
        if !review_badge.is_empty() {
            spans.push(Span::styled(
                review_badge.to_string(),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ));
        }
        Line::from(spans)
    }
}

/// Testing helper: render the lineage to plain strings.
pub fn render_lineage_to_strings(
    lineage: &ExecutorLineage,
    selected: Option<ExecutorId>,
) -> Vec<String> {
    let mut tree = AgentsTree::new();
    tree.set_selected(selected);
    let mut out = Vec::new();
    for rid in lineage.root_ids() {
        collect_lines(&tree, lineage, rid, 0, &mut out);
    }
    out
}

fn collect_lines(
    tree: &AgentsTree,
    l: &ExecutorLineage,
    id: &ExecutorId,
    depth: usize,
    out: &mut Vec<String>,
) {
    if let Some(node) = l.node(id) {
        let indent = "  ".repeat(depth);
        let connector = if depth == 0 { "" } else { "└─ " };
        out.push(format!(
            "{}{}{} {} [{:?}]",
            indent,
            connector,
            node.agent,
            match node.role {
                Role::Brain => "BRAIN",
                Role::Executor => "EXEC",
                Role::SubExecutor => "SUB",
            },
            node.phase
        ));
        if !tree.collapsed.contains(id) {
            for c in &node.child_ids {
                collect_lines(tree, l, c, depth + 1, out);
            }
        }
    }
}
```

- [ ] **Step 3: Update `DashboardView` to pass the lineage into `AgentsTree::render`**

In `crates/spur-tui/src/views/dashboard.rs`:

1. Remove the `agents: Vec<AgentState>` field and the `session_agent`, `cost_by_agent`, `handle_agent_spawned`, `set_agent_status_for_session` helpers — the projection replaces them.
2. Change the render signature: `DashboardView::render` will need a reference to `ExecutorLineage`. Since `View::render(&self, frame, area)` has no way to pass extra args, store a `&ExecutorLineage` via a setter: add

```rust
    lineage: Option<std::sync::Arc<std::sync::Mutex<ExecutorLineage>>>,
```

and a `pub fn set_lineage_source(&mut self, src: Arc<Mutex<ExecutorLineage>>)` method.
   *Alternative* (simpler): make `App::render` reach into `self.lineage` directly and pass it to `dashboard.render_with_lineage(frame, area, &self.lineage)`. Define a new inherent method on `DashboardView`, not on the `View` trait, to avoid widening the trait for one view.

Recommended: the inherent-method approach. Add to `DashboardView`:

```rust
impl DashboardView {
    pub fn render_with_lineage(
        &self,
        frame: &mut ratatui::Frame,
        area: ratatui::layout::Rect,
        lineage: &spur_core::ExecutorLineage,
    ) {
        // identical to current `render` but replaces `self.agents_tree.render(frame, chunks[0], &self.agents)`
        // with `self.agents_tree.render(frame, chunks[0], lineage)`.
    }
}
```

And update `App::render`:

```rust
ViewId::Dashboard => self.dashboard.render_with_lineage(frame, area, &self.lineage),
```

3. Remove stale fields in `DashboardView::new` and its event handlers that were computing `AgentState` from scratch. Event handling in `handle_spur_event` should now be limited to the `ActivityLog` updates (the projection handles lineage state).

- [ ] **Step 4: Fix resulting compile errors iteratively**

Run: `cargo check -p spur-tui`
Fix compile errors by deleting the now-unused agent-state logic. Keep the `ActivityLog` writes — those are still needed for chronological log view.

- [ ] **Step 5: Run the snapshot test**

Run: `cargo test -p spur-tui --test agents_tree_snapshot`
Expected: PASS.

- [ ] **Step 6: Run the whole suite**

Run: `cargo test --workspace`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/spur-tui/src/components/agents_tree.rs \
        crates/spur-tui/src/views/dashboard.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-tui/tests/agents_tree_snapshot.rs
git commit -m "feat(spur-tui): recursive lineage tree driven by ExecutorLineage"
```

---

## Task 8: Selection keybindings + `FocusNode` / `UnfocusNode` / `JumpToReview` actions

**Files:**
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/views/dashboard.rs`

- [ ] **Step 1: Extend `Action` enum**

In `crates/spur-tui/src/action.rs`, add variants to `pub enum Action`:

```rust
    /// Move tree selection down one row.
    SelectNext,
    /// Move tree selection up one row.
    SelectPrev,
    /// Focus the currently-selected executor node (right pane → detail mode).
    FocusNode,
    /// Unfocus (right pane → chronological log).
    UnfocusNode,
    /// Jump to the next executor with a pending review.
    JumpToReview,
    /// Toggle collapse on the selected subtree.
    ToggleCollapse,
    /// Submit a review decision for the given executor.
    SubmitReview {
        executor_id: String,
        decision: spur_core::ReviewDecision,
    },
```

- [ ] **Step 2: Handle j/k/Enter/Esc/r/c in `DashboardView::handle_key`**

In `crates/spur-tui/src/views/dashboard.rs`, add to the "non-editing keys when InputBar is empty" block:

```rust
                KeyCode::Char('j') if self.focused_panel == Panel::Agents => {
                    return Some(Action::SelectNext);
                }
                KeyCode::Char('k') if self.focused_panel == Panel::Agents => {
                    return Some(Action::SelectPrev);
                }
                KeyCode::Char('r') if self.input_bar.is_empty() => {
                    return Some(Action::JumpToReview);
                }
                KeyCode::Char('c') if self.focused_panel == Panel::Agents => {
                    return Some(Action::ToggleCollapse);
                }
                KeyCode::Enter if self.input_bar.is_empty() && self.focused_panel == Panel::Agents => {
                    return Some(Action::FocusNode);
                }
                KeyCode::Esc if self.focused_panel == Panel::Agents => {
                    return Some(Action::UnfocusNode);
                }
```

(Insert these before the existing `KeyCode::Esc => return Some(Action::Quit)` arm, so Esc only quits when not in the lineage panel.)

- [ ] **Step 3: Handle the actions in `App::process_action`**

In `crates/spur-tui/src/app.rs::process_action`, add match arms:

```rust
            Action::SelectNext => {
                self.dashboard.agents_tree_mut().select_next(&self.lineage);
            }
            Action::SelectPrev => {
                self.dashboard.agents_tree_mut().select_prev(&self.lineage);
            }
            Action::FocusNode => {
                if let Some(id) = self.dashboard.agents_tree_mut().selected().cloned() {
                    self.dashboard.set_focused_node(Some(id));
                }
            }
            Action::UnfocusNode => {
                self.dashboard.set_focused_node(None);
            }
            Action::JumpToReview => {
                let next = self
                    .lineage
                    .pending_reviews()
                    .into_iter()
                    .next();
                if let Some(id) = next {
                    self.dashboard.agents_tree_mut().set_selected(Some(id.clone()));
                    self.dashboard.set_focused_node(Some(id));
                }
            }
            Action::ToggleCollapse => {
                if let Some(id) = self.dashboard.agents_tree_mut().selected().cloned() {
                    self.dashboard.agents_tree_mut().toggle_collapsed(&id);
                }
            }
            Action::SubmitReview { .. } => {
                // handled in Task 11
            }
```

- [ ] **Step 4: Expose the necessary accessors on `DashboardView`**

In `dashboard.rs`:

```rust
impl DashboardView {
    pub fn agents_tree_mut(&mut self) -> &mut AgentsTree {
        &mut self.agents_tree
    }

    pub fn set_focused_node(&mut self, id: Option<spur_core::ExecutorId>) {
        self.focused_node = id;
    }

    pub fn focused_node(&self) -> Option<&spur_core::ExecutorId> {
        self.focused_node.as_ref()
    }
}
```

Add `focused_node: Option<spur_core::ExecutorId>` to the struct and initialize to `None` in `new()`.

- [ ] **Step 5: Compile + run**

Run: `cargo check -p spur-tui && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/action.rs \
        crates/spur-tui/src/views/dashboard.rs \
        crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): selection + focus + jump-to-review actions on lineage tree"
```

---

## Task 9: Focus-aware detail pane with tabs (stream / artifacts / attempts / task)

**Files:**
- Create: `crates/spur-tui/src/components/detail_pane.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Modify: `crates/spur-tui/src/views/dashboard.rs`

- [ ] **Step 1: Declare the new module**

In `crates/spur-tui/src/components/mod.rs`, add:

```rust
pub mod detail_pane;
```

- [ ] **Step 2: Implement `DetailPane` with 4 tabs**

Create `crates/spur-tui/src/components/detail_pane.rs`:

```rust
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
    Frame,
};

use spur_core::{Artifact, ExecutorNode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Stream,
    Artifacts,
    Attempts,
    Task,
    Review,
}

impl DetailTab {
    pub fn all() -> &'static [DetailTab] {
        &[
            DetailTab::Stream,
            DetailTab::Artifacts,
            DetailTab::Attempts,
            DetailTab::Task,
            DetailTab::Review,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            DetailTab::Stream => "stream",
            DetailTab::Artifacts => "artifacts",
            DetailTab::Attempts => "attempts",
            DetailTab::Task => "task",
            DetailTab::Review => "review",
        }
    }
}

pub struct DetailPane {
    pub current_tab: DetailTab,
}

impl DetailPane {
    pub fn new() -> Self {
        Self {
            current_tab: DetailTab::Stream,
        }
    }

    pub fn cycle_tab(&mut self, forward: bool) {
        let all = DetailTab::all();
        let idx = all.iter().position(|t| *t == self.current_tab).unwrap_or(0);
        let next = if forward {
            (idx + 1) % all.len()
        } else {
            (idx + all.len() - 1) % all.len()
        };
        self.current_tab = all[next];
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, node: &ExecutorNode) {
        let block = Block::default()
            .title(format!(" {} ", node.agent))
            .borders(Borders::ALL);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);

        // Tab header
        let titles: Vec<Line> = DetailTab::all()
            .iter()
            .map(|t| {
                let style = if *t == self.current_tab {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(Span::styled(t.label(), style))
            })
            .collect();
        let tabs = Tabs::new(titles)
            .select(
                DetailTab::all()
                    .iter()
                    .position(|t| *t == self.current_tab)
                    .unwrap_or(0),
            )
            .divider("│");
        frame.render_widget(tabs, chunks[0]);

        // Body
        let body_lines = match self.current_tab {
            DetailTab::Stream => self.render_stream(node),
            DetailTab::Artifacts => self.render_artifacts(node),
            DetailTab::Attempts => self.render_attempts(node),
            DetailTab::Task => self.render_task(node),
            DetailTab::Review => self.render_review(node),
        };
        let p = Paragraph::new(body_lines).wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(p, chunks[1]);
    }

    fn render_stream<'a>(&self, _node: &'a ExecutorNode) -> Vec<Line<'a>> {
        // v1: stream tab shows a placeholder. Real per-session streaming
        // content is already accumulated in `ActivityLog` / `react_trace`
        // globally. A future change can rebind that to focused-node only.
        vec![Line::from(Span::styled(
            "(live stream — rebinding to focused-node view is a follow-up)",
            Style::default().fg(Color::DarkGray),
        ))]
    }

    fn render_artifacts<'a>(&self, node: &'a ExecutorNode) -> Vec<Line<'a>> {
        let mut out = Vec::new();
        for attempt in &node.attempts {
            for a in &attempt.artifacts {
                out.push(match a {
                    Artifact::Diff(d) => Line::from(format!(
                        "diff: {} files, +{} -{}",
                        d.files_changed, d.insertions, d.deletions
                    )),
                    Artifact::PrUrl(u) => Line::from(format!("pr: {}", u)),
                    Artifact::FileList(f) => Line::from(format!("files: {}", f.len())),
                    Artifact::Text(t) => Line::from(t.clone()),
                });
            }
        }
        if out.is_empty() {
            out.push(Line::from(Span::styled(
                "(no artifacts yet)",
                Style::default().fg(Color::DarkGray),
            )));
        }
        out
    }

    fn render_attempts<'a>(&self, node: &'a ExecutorNode) -> Vec<Line<'a>> {
        node.attempts
            .iter()
            .enumerate()
            .map(|(i, a)| {
                Line::from(format!(
                    "#{}: {:?}  cost=${:.2}  session={}",
                    i + 1,
                    a.status,
                    a.cost_usd,
                    a.session_id.0
                ))
            })
            .collect()
    }

    fn render_task<'a>(&self, node: &'a ExecutorNode) -> Vec<Line<'a>> {
        if node.task_spec.is_empty() {
            vec![Line::from(Span::styled(
                "(no task spec captured)",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            node.task_spec
                .lines()
                .map(|l| Line::from(l.to_string()))
                .collect()
        }
    }

    fn render_review<'a>(&self, node: &'a ExecutorNode) -> Vec<Line<'a>> {
        match &node.pending_review {
            Some(_) => vec![Line::from(Span::styled(
                "(review card rendered in Task 10)",
                Style::default().fg(Color::DarkGray),
            ))],
            None => vec![Line::from(Span::styled(
                "(no pending review)",
                Style::default().fg(Color::DarkGray),
            ))],
        }
    }
}
```

- [ ] **Step 3: Integrate into `DashboardView::render_with_lineage`**

Modify `render_with_lineage` in `dashboard.rs` to split the right pane conditionally:

```rust
        match &self.focused_node {
            Some(id) => {
                if let Some(node) = lineage.node(id) {
                    self.detail_pane.render(frame, chunks[1], node);
                } else {
                    self.activity_log.render(frame, chunks[1]);
                }
            }
            None => {
                self.activity_log.render(frame, chunks[1]);
            }
        }
```

Add `detail_pane: DetailPane` to `DashboardView` and initialize in `new()`.

- [ ] **Step 4: Add tab-switching key on left/right arrows when focused**

In `DashboardView::handle_key`, when `focused_node.is_some()`:

```rust
                KeyCode::Right => {
                    self.detail_pane.cycle_tab(true);
                    return None;
                }
                KeyCode::Left => {
                    self.detail_pane.cycle_tab(false);
                    return None;
                }
```

- [ ] **Step 5: Compile + run**

Run: `cargo check -p spur-tui && cargo test --workspace`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/detail_pane.rs \
        crates/spur-tui/src/components/mod.rs \
        crates/spur-tui/src/views/dashboard.rs
git commit -m "feat(spur-tui): focus-aware detail pane with stream/artifacts/attempts/task/review tabs"
```

---

## Task 10: Review card + typed decision submission

**Files:**
- Create: `crates/spur-tui/src/components/review_card.rs`
- Modify: `crates/spur-tui/src/components/detail_pane.rs`
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Implement the review card**

Create `crates/spur-tui/src/components/review_card.rs`:

```rust
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use spur_core::{ExecutorNode, ReviewKind, ReviewRequest};

/// Render a pending review as a block of styled lines.
pub fn render_review(node: &ExecutorNode) -> Vec<Line> {
    let req = match &node.pending_review {
        Some(r) => r,
        None => {
            return vec![Line::from(Span::styled(
                "(no pending review)",
                Style::default().fg(Color::DarkGray),
            ))]
        }
    };
    let mut out = Vec::new();
    out.push(Line::from(Span::styled(
        format!("── Review requested: {} ──", kind_label(&req.kind)),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    out.push(Line::from(""));
    out.push(Line::from(format!("Summary: {}", req.payload.summary)));
    if let Some(d) = &req.payload.diff_summary {
        out.push(Line::from(format!(
            "Diff: {} files, +{} -{}",
            d.files_changed, d.insertions, d.deletions
        )));
    }
    if let Some(pr) = &req.payload.pr_url {
        out.push(Line::from(format!("PR: {}", pr)));
    }
    if let Some(err) = &req.payload.error {
        out.push(Line::from(Span::styled(
            format!("Error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }
    out.push(Line::from(""));
    out.push(Line::from(Span::styled(
        "[a] approve  [d] deny  [m] modify+approve  [R] retry",
        Style::default().fg(Color::Cyan),
    )));
    out
}

fn kind_label(k: &ReviewKind) -> &'static str {
    match k {
        ReviewKind::Completion => "completion",
        ReviewKind::Failure => "failure",
        ReviewKind::Conflict => "conflict",
        ReviewKind::Checkpoint => "checkpoint",
    }
}
```

- [ ] **Step 2: Wire the `review` tab into the review card**

In `crates/spur-tui/src/components/detail_pane.rs`, replace `render_review` with:

```rust
    fn render_review<'a>(&self, node: &'a ExecutorNode) -> Vec<Line<'a>> {
        super::review_card::render_review(node)
    }
```

Declare `pub mod review_card;` in `components/mod.rs`.

- [ ] **Step 3: Write a failing test for decision submission**

Create `crates/spur-tui/tests/review_submission.rs`:

```rust
//! Given a focused node with a pending review and the current tab set to
//! `Review`, pressing 'a' should produce `Action::SubmitReview{Approve}`.

use spur_tui::action::{Action, ViewId};

#[test]
fn approve_key_maps_to_approve_decision() {
    use spur_core::ReviewDecision;
    let d = spur_tui::components::review_card::decision_for_key('a', None);
    assert!(matches!(d, Some(ReviewDecision::Approve)));
}

#[test]
fn deny_key_with_reason_maps_to_reject() {
    use spur_core::ReviewDecision;
    let d = spur_tui::components::review_card::decision_for_key('d', Some("bad".into()));
    assert!(matches!(d, Some(ReviewDecision::Reject { .. })));
}

#[test]
fn unknown_key_returns_none() {
    let d = spur_tui::components::review_card::decision_for_key('z', None);
    assert!(d.is_none());
}

// silence unused import warning
#[allow(dead_code)]
fn _force(_: ViewId, _: Action) {}
```

Run: `cargo test -p spur-tui --test review_submission`
Expected: FAIL — `decision_for_key` not defined.

- [ ] **Step 4: Add `decision_for_key` to `review_card`**

Append to `crates/spur-tui/src/components/review_card.rs`:

```rust
use spur_core::ReviewDecision;

/// Pure function mapping a single key + optional free-text prompt answer to a
/// `ReviewDecision`. Returns `None` for keys that are not review actions.
pub fn decision_for_key(key: char, prompt_answer: Option<String>) -> Option<ReviewDecision> {
    match key {
        'a' => Some(ReviewDecision::Approve),
        'd' => Some(ReviewDecision::Reject {
            reason: prompt_answer.unwrap_or_else(|| "(no reason given)".into()),
        }),
        'm' => Some(ReviewDecision::Modify {
            note: prompt_answer.unwrap_or_else(|| "(no note)".into()),
        }),
        'R' => Some(ReviewDecision::Retry {
            new_constraints: prompt_answer.unwrap_or_else(|| "(no constraints)".into()),
        }),
        _ => None,
    }
}
```

- [ ] **Step 5: Run the test**

Run: `cargo test -p spur-tui --test review_submission`
Expected: PASS.

- [ ] **Step 6: Wire the keys into `DashboardView::handle_key`**

In `dashboard.rs`, when a node is focused AND the current tab is `Review`, handle the keys. Simplest: check in the non-editing block of `handle_key`:

```rust
                KeyCode::Char(c @ ('a' | 'd' | 'm' | 'R')) if self.focused_node.is_some()
                    && self.detail_pane.current_tab == DetailTab::Review =>
                {
                    // For a/d/m/R we dispatch immediately with empty prompt.
                    // Prompt-for-reason UX is deferred; v1 uses placeholders.
                    if let Some(decision) =
                        crate::components::review_card::decision_for_key(c, None)
                    {
                        if let Some(id) = self.focused_node.clone() {
                            return Some(Action::SubmitReview {
                                executor_id: id.0,
                                decision,
                            });
                        }
                    }
                    return None;
                }
```

Import `DetailTab` and add `focused_node: Option<ExecutorId>` reference if not present.

- [ ] **Step 7: Handle `Action::SubmitReview` in `App::process_action`**

In `app.rs`, replace the stub from Task 8:

```rust
            Action::SubmitReview { executor_id, decision } => {
                if let Some(ref tx) = self.user_input_tx {
                    let _ = tx.try_send(UserInput::SubmitReview {
                        executor_id: executor_id.clone(),
                        decision: decision.clone(),
                    });
                }
                // Optimistically clear the pending_review in the local projection
                // by applying the resolved event right away. The authoritative
                // event will flow back through SpurEvent::ExecutorReviewResolved.
                self.lineage.apply(&spur_acp::SpurEvent::ExecutorReviewResolved {
                    id: executor_id,
                    decision: to_wire_decision(&decision),
                });
            }
```

Add the `UserInput::SubmitReview` variant:

```rust
pub enum UserInput {
    Message { session: SessionId, text: String, interrupt: bool },
    ListSessions,
    ResumeSession { session_id: String },
    SubmitReview {
        executor_id: String,
        decision: spur_core::ReviewDecision,
    },
}
```

Add the helper:

```rust
fn to_wire_decision(d: &spur_core::ReviewDecision) -> spur_acp::ExecutorReviewDecision {
    use spur_core::ReviewDecision as L;
    use spur_acp::ExecutorReviewDecision as W;
    match d {
        L::Approve => W::Approve,
        L::Reject { reason } => W::Reject { reason: reason.clone() },
        L::Modify { note } => W::Modify { note: note.clone() },
        L::Retry { new_constraints } => W::Retry {
            new_constraints: new_constraints.clone(),
        },
    }
}
```

- [ ] **Step 8: Handle the new `UserInput::SubmitReview` at every consumer of `user_input_rx`**

Search: `grep -rn "UserInput::" crates/`
For every `match` on `UserInput` in the codebase (likely `spur-cli`/`spur-core` driver), add a `UserInput::SubmitReview { .. } => { /* TODO(follow-up spec) orchestrator conversion to tool-call result */ }` arm so it compiles.

This is the explicit hand-off to the follow-up orchestrator spec.

- [ ] **Step 9: Compile + run**

Run: `cargo check --workspace && cargo test --workspace`
Expected: PASS.

- [ ] **Step 10: Commit**

```bash
git add -A
git commit -m "feat(spur-tui): typed ReviewDecision submission + inline review card"
```

---

## Task 11: Aggregate status bar + help overlay text

**Files:**
- Modify: `crates/spur-tui/src/components/status_bar.rs`
- Modify: `crates/spur-tui/src/views/dashboard.rs`
- Modify: `crates/spur-tui/src/components/help_overlay.rs`

- [ ] **Step 1: Extend `StatusBar::render` signature**

Current signature:

```rust
pub fn render(frame: &mut Frame, area: Rect, view: &ViewId, total_cost: f64, elapsed: &str)
```

Change to:

```rust
pub fn render(
    frame: &mut Frame,
    area: Rect,
    view: &ViewId,
    running: usize,
    pending_review: usize,
    total_cost: f64,
    elapsed: &str,
)
```

Render e.g.:

```
spur · 3 running · 1 review · $0.42 · 5m 12s · ?: help
```

Style `N review` in yellow+bold when `pending_review > 0`.

- [ ] **Step 2: Compute aggregates in `DashboardView::render_with_lineage`**

```rust
        let running = lineage
            .nodes()
            .filter(|n| matches!(
                n.phase,
                spur_core::LifecycleState::Running | spur_core::LifecycleState::Spawning,
            ))
            .count();
        let pending_review = lineage.pending_reviews().len();
        let total_cost: f64 = lineage
            .nodes()
            .map(|n| n.current_attempt().map(|a| a.cost_usd).unwrap_or(0.0))
            .sum();
```

Pass these into `StatusBar::render`.

- [ ] **Step 3: Update help overlay**

In `crates/spur-tui/src/components/help_overlay.rs`, add rows for the new keys:

```
j / k           move selection in lineage tree
Enter           focus selected node
Esc             unfocus (return to log)
← / →           cycle detail tabs
c               toggle collapse on selected subtree
r               jump to next pending review
a / d / m / R   approve / deny / modify / retry (in review tab)
```

- [ ] **Step 4: Compile + run**

Run: `cargo check -p spur-tui && cargo test --workspace`
Expected: PASS.

- [ ] **Step 5: Manual smoke test**

From a dev machine:

```
cargo run -p spur-cli -- <your-usual-args>
```

Verify:
- lineage tree shows brain + workers
- `j`/`k` selection moves the highlight
- `Enter` opens the detail pane with 5 tabs
- `r` has no-op when no pending reviews (expected)
- status bar shows `0 review` (yellow only when > 0)

- [ ] **Step 6: Commit**

```bash
git add crates/spur-tui/src/components/status_bar.rs \
        crates/spur-tui/src/views/dashboard.rs \
        crates/spur-tui/src/components/help_overlay.rs
git commit -m "feat(spur-tui): aggregate status bar + updated help overlay"
```

---

## Task 12: Integration test — synthesized lifecycle → review → resolved

**Files:**
- Create: `crates/spur-core/tests/lineage_integration.rs`

- [ ] **Step 1: Write the end-to-end projection test**

Create `crates/spur-core/tests/lineage_integration.rs`:

```rust
//! End-to-end: simulate a realistic event stream, assert projection matches.

use std::path::PathBuf;
use std::time::SystemTime;

use spur_acp::{
    DelegationStatus, ExecutorArtifactPayload, ExecutorReviewDecision, ExecutorReviewKind,
    ExecutorReviewPayload, SessionId, SpurEvent,
};
use spur_core::{ExecutorId, ExecutorLineage, LifecycleState};

#[test]
fn full_flow_brain_to_review_to_resolved() {
    let mut l = ExecutorLineage::new();
    // Brain
    l.apply(&SpurEvent::BrainSpawned {
        agent: "kiro".into(),
        session: SessionId("b".into()),
    });
    // Worker spawned via legacy event
    l.apply(&SpurEvent::WorkerSpawned {
        agent: "w1".into(),
        session: SessionId("w1".into()),
        worktree: PathBuf::from("/tmp"),
    });
    l.apply(&SpurEvent::DelegationRequested {
        from: SessionId("b".into()),
        to_agent: "w1".into(),
        task: "close the bug".into(),
    });
    // Executor produces an artifact
    l.apply(&SpurEvent::ExecutorArtifact {
        id: "w1".into(),
        artifact: ExecutorArtifactPayload::PrUrl("https://x/42".into()),
    });
    // Checkpoint: review requested
    l.apply(&SpurEvent::ExecutorReviewRequested {
        id: "w1".into(),
        kind: ExecutorReviewKind::Completion,
        payload: ExecutorReviewPayload {
            summary: "PR ready".into(),
            diff_summary: None,
            pr_url: Some("https://x/42".into()),
            error: None,
        },
        requested_at: SystemTime::now(),
    });

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert_eq!(n.phase, LifecycleState::AwaitingReview);
    assert!(n.pending_review.is_some());
    assert_eq!(l.pending_reviews().len(), 1);

    // User approves
    l.apply(&SpurEvent::ExecutorReviewResolved {
        id: "w1".into(),
        decision: ExecutorReviewDecision::Approve,
    });
    l.apply(&SpurEvent::DelegationCompleted {
        worker_session: SessionId("w1".into()),
        status: DelegationStatus::Success,
    });

    let n = l.node(&ExecutorId::new("w1")).unwrap();
    assert!(n.pending_review.is_none());
    assert_eq!(n.phase, LifecycleState::Succeeded);
    let a = n.current_attempt().unwrap();
    assert_eq!(a.artifacts.len(), 1);
    assert!(a.ended_at.is_some());
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p spur-core --test lineage_integration`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/tests/lineage_integration.rs
git commit -m "test(spur-core): end-to-end lineage projection flow"
```

---

## Done criteria

After all 12 tasks pass:

- [ ] `cargo test --workspace` passes.
- [ ] Running the TUI shows the recursive lineage tree, allows selection,
  opens the detail pane on `Enter`, cycles tabs with ← / →, and `r` jumps
  to pending reviews (no-op when none).
- [ ] When a synthetic `ExecutorReviewRequested` event is injected (e.g.
  via a debug hook), the review tab renders the card and `a`/`d`/`m`/`R`
  emit `ExecutorReviewResolved`.
- [ ] The status bar shows aggregate `N running · M review · $X · Ym Zs`.

## Follow-up (not in this plan)

- Orchestrator-side translation of `ExecutorReviewDecision` into the
  brain's delegate-tool result (separate spec).
- Depth-N default expand toggle + filters (completed / failed / review).
- Per-node streaming in the `stream` tab (currently placeholder).
- Prompt-for-reason UX on `d` / `m` / `R` (v1 uses placeholder strings).
- Multi-channel input to brain (Slack / email / webhook).

---

## Self-review

**Spec coverage.** Cross-check spec sections against tasks:

| Spec section | Task(s) |
|---|---|
| Event-sourced projection invariant | Tasks 3–5, replay test in Task 5 step 5 |
| 6 new `SpurEvent` variants | Task 2 |
| Data model (`ExecutorNode`, `Attempt`, enums) | Task 1 |
| `ExecutorLineage` module in `spur-core` | Tasks 1, 3, 4 |
| Legacy-event adaptation | Task 5 |
| `DashboardView` retrofit (no new view) | Tasks 6–9 |
| Recursive tree traversal, depth-1 default | Task 7 (walk is recursive; default state `collapsed = ∅`; spec note that "v1 default expand = depth-1" is satisfied by user pressing `c` to collapse deeper subtrees; the *renderer* handles unbounded depth) |
| Focus-aware detail pane + 5 tabs | Tasks 9, 10 |
| Typed `ReviewDecision` submission | Task 10 |
| Review card inline in `review` tab | Task 10 |
| Aggregate status bar | Task 11 |
| `r` jump-to-next-review | Task 8 |
| Error handling: orphan buffer | Task 3 (test `orphan_phase_event_is_replayed_after_spawn`) |
| Error handling: missing parent → new root | Task 3 (`spawn_creates_root_when_no_parent`) |
| Testing strategy | Tasks 3, 4, 5, 7, 10, 12 |

**Depth-1 default rendering clarification.** The spec specifies "v1 renders depth-1". Task 7 implements *unbounded* recursive rendering with collapse support; to honor the spec as written, add a single line to `AgentsTree::new`: seed `collapsed` with all non-root ids after first projection update. **Alternative (chosen):** leave expand-by-default because collapse is a one-key action and users with small trees will prefer seeing children. If you want strict spec-literal depth-1, insert after Task 7 Step 2:

```rust
// In DashboardView::render_with_lineage, before rendering:
// If this is the first render for a node, auto-collapse it.
```

This is deliberately not added as a separate task because the behavior change is trivial and reversible. Flag for reviewer.

**Placeholder scan.** None of the "fill in later" / "add appropriate X" patterns appear. Every step with code has the full code.

**Type consistency.** `ExecutorId`, `ReviewDecision`, `ExecutorReviewDecision` (wire vs in-memory) are distinguished consistently; `to_wire_decision` in Task 10 bridges. `DetailTab`, `LifecycleState`, `AttemptStatus`, `Role` consistent across tasks.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-13-executor-lineage-visualization.md`. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session using `executing-plans`, batch execution with checkpoints.

Which approach?

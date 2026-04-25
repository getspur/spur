# Worker Peer Mailbox — Stage 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship Stage 1 of the worker peer mailbox: a durable, mediated channel that lets workers send peer messages (questions, handoffs, warnings) to other in-flight workers, persisted in a ledger, audited via beads, and injected into target prompts as orchestrator-authored context — all without keeping ACP sessions alive between turns.

**Architecture:** A new `peer_mailbox` module in `spur-core` adds a router (validates against an immutable plan-DAG snapshot, transitions a ledger, emits events through `EventFunnel`) and a prompt-context builder (generates a `target_prompt_id`, records per-prompt injection idempotence). Wire types live in `spur-acp`. The orchestrator's `run_one_worker_attempt` gains a pre-prompt injection hook and a post-dispatch ledger transition. A forced-terminal-timeout drain replaces the original funnel-idle barrier; a startup reconciler resolves crash-recovery state. Stage 2 (`WorkerRuntime`) is **out of scope** for this plan.

**Tech Stack:**
- Rust 2021 edition, tokio runtime
- `serde` (serde_json) for wire types
- `tokio::sync::Mutex` and `tokio::sync::mpsc` for concurrency primitives
- `uuid` (v4 for `message_id`, `target_prompt_id`)
- `tracing` for logs
- `async-trait` for the ledger trait
- Existing crates: `spur-acp` (wire), `spur-core` (logic), `spur-mcp` (plan + labels)

**Spec reference:** `docs/superpowers/specs/2026-04-25-worker-peer-mailbox-design.md`

---

## Scope

**In scope (Stage 1):**

- New wire types: `PeerMessageEnvelope`, `LedgerState`, `MessageKind`, `TerminalOutcome`.
- 11 new `SpurEventBody` variants for peer lifecycle.
- Forward-compat replay deserialization wrapper.
- Beads label constructors for compact peer audit references.
- `PlanScopeSnapshot` immutable-snapshot abstraction.
- `PeerMailboxLedger` trait + in-memory implementation (no persistence backing in v1).
- `PeerMessageGuard` RAII with explicit `async fn finalize()` and sync-only `Drop`.
- `PeerMailboxRouter` (validation, ledger transitions, event emission).
- `_spur/peer_message`, `_spur/peer_message_consumed`, `_spur/peer_message_ignored` allowlist parsing.
- `PeerPromptContextBuilder` with `target_prompt_id` generation and per-prompt injection idempotence.
- Pre-prompt injection hook + post-dispatch ledger transition in `run_one_worker_attempt`.
- Forced-terminal-timeout drain replacing the previous review barrier.
- Startup reconciler emitting `WorkerPeerMailboxReconciled`.
- Lineage projection: peer events become edges between executor nodes.
- Review payload extension: peer influence summary in `ReviewPayload`.
- Tiered safety limits + feature flag (default off).

**Out of scope (separate plans):**

- Stage 2 stateful `WorkerRuntime`.
- TUI rendering of peer edges (data is exposed; rendering polish is a follow-up).
- Persistence backing for the ledger (in-memory for v1).
- Cost-tracker SQLite integration (peer cost lives on the `Delivered` event for v1).

---

## File Structure

**Create:**

| Path | Responsibility |
|---|---|
| `crates/spur-acp/src/domain/peer_message.rs` | Wire envelope, ledger states, kinds, terminal outcomes; serde |
| `crates/spur-acp/src/domain/replay_compat.rs` | Forward-compat `SpurEventBody` deserialization wrapper |
| `crates/spur-core/src/peer_mailbox/mod.rs` | Module root; re-exports |
| `crates/spur-core/src/peer_mailbox/ledger.rs` | `PeerMailboxLedger` trait + `InMemoryLedger` |
| `crates/spur-core/src/peer_mailbox/guard.rs` | `PeerMessageGuard` + sync-Drop reconciler mpsc |
| `crates/spur-core/src/peer_mailbox/router.rs` | `PeerMailboxRouter` |
| `crates/spur-core/src/peer_mailbox/prompt_builder.rs` | `PeerPromptContextBuilder` + injection records |
| `crates/spur-core/src/peer_mailbox/reconciler.rs` | Startup reconciliation pass |
| `crates/spur-core/src/peer_mailbox/limits.rs` | Tiered context-window bound + per-source quotas |
| `crates/spur-mcp/src/plan/scope_snapshot.rs` | `PlanScopeSnapshot` |
| `crates/spur-core/tests/peer_mailbox_e2e.rs` | End-to-end integration test |

**Modify:**

| Path | Change |
|---|---|
| `crates/spur-acp/src/domain/events.rs` | Add 11 peer event variants |
| `crates/spur-acp/src/domain/mod.rs` | Re-export peer types |
| `crates/spur-mcp/src/plan/labels.rs` | Add `peer_message_label` constructor |
| `crates/spur-mcp/src/plan/mod.rs` | Add `pub mod scope_snapshot;` and a `snapshot_for_peer()` method on `PlanState` |
| `crates/spur-core/src/lib.rs` | Add `pub mod peer_mailbox;` |
| `crates/spur-core/src/spur_ext_interp.rs` | Add three peer method arms |
| `crates/spur-core/src/orchestrator.rs` | Pre-prompt hook, post-dispatch transition, drain replacement, reconciler boot |
| `crates/spur-core/src/lineage/projection.rs` | Project peer events to edges |
| `crates/spur-core/src/lineage/types.rs` | Add `PeerEdge` type to executor node |

---

## Conventions used in this plan

- `cargo test -p <crate>` runs tests for that crate. Use `--test <name>` for a specific integration test file.
- Async tests use `#[tokio::test]` (canonical pattern lives at `crates/spur-core/src/event_funnel.rs:78–143`).
- Tests for new modules go in inline `#[cfg(test)] mod tests { ... }` blocks at the bottom of the source file unless noted otherwise.
- Each task ends with a commit. Commit messages use the Conventional Commits style already used in the repo (e.g. `feat(spur-core):`, `test(spur-acp):`).
- `DelegationId` is `crates/spur-acp/src/domain/delegation.rs:21` — `pub struct DelegationId(pub String)`.

---

## Task 1: Wire types — `PeerMessageEnvelope` and ledger states

**Files:**
- Create: `crates/spur-acp/src/domain/peer_message.rs`
- Modify: `crates/spur-acp/src/domain/mod.rs` (add `pub mod peer_message;` + re-exports)

- [ ] **Step 1: Create the file with the failing test**

```rust
// crates/spur-acp/src/domain/peer_message.rs

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::delegation::DelegationId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerMessageId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerMessageEnvelope {
    pub schema: String,
    pub message_id: PeerMessageId,
    pub source_delegation_id: DelegationId,
    pub target_delegation_id: DelegationId,
    pub source_issue_id: String,
    pub target_issue_id: String,
    pub source_plan_task_id: String,
    pub target_plan_task_id: String,
    pub source_executor_id: String,
    pub plan_version: u64,
    pub kind: MessageKind,
    pub body: String,
    pub sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Question,
    Answer,
    Handoff,
    Warning,
    Constraint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerState {
    Accepted,
    Rejected,
    Queued,
    DeliveredInflight,
    Delivered,
    Consumed,
    Ignored,
    Expired,
    Dropped,
    Undeliverable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalOutcome {
    Consumed,
    Ignored { reason: String },
    Expired,
    Dropped { reason: String },
    Undeliverable { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrips_through_serde_json() {
        let envelope = PeerMessageEnvelope {
            schema: "spur-peer-message/v1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            source_delegation_id: DelegationId("src-1".into()),
            target_delegation_id: DelegationId("tgt-1".into()),
            source_issue_id: "bd-100".into(),
            target_issue_id: "bd-200".into(),
            source_plan_task_id: "task-a".into(),
            target_plan_task_id: "task-b".into(),
            source_executor_id: "exec-x".into(),
            plan_version: 7,
            kind: MessageKind::Handoff,
            body: "Done with reqwest probe; B to wire timeout".into(),
            sequence: 1,
        };
        let json = serde_json::to_string(&envelope).unwrap();
        let back: PeerMessageEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(envelope, back);
    }

    #[test]
    fn ledger_state_serializes_snake_case() {
        let s = serde_json::to_string(&LedgerState::DeliveredInflight).unwrap();
        assert_eq!(s, "\"delivered_inflight\"");
    }
}
```

- [ ] **Step 2: Wire the module into `domain/mod.rs`**

Add to `crates/spur-acp/src/domain/mod.rs`:

```rust
pub mod peer_message;

pub use peer_message::{
    LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId, TerminalOutcome,
};
```

- [ ] **Step 3: Run the tests and verify they pass**

Run: `cargo test -p spur-acp peer_message`
Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/domain/peer_message.rs crates/spur-acp/src/domain/mod.rs
git commit -m "feat(spur-acp): add peer message envelope and ledger state types"
```

---

## Task 2: Eleven new `SpurEventBody` variants

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`

The variants attach to the existing `#[non_exhaustive]` `SpurEventBody` enum (defined at `crates/spur-acp/src/domain/events.rs:307`).

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `events.rs`:

```rust
#[test]
fn worker_peer_message_accepted_roundtrips() {
    use crate::domain::peer_message::{MessageKind, PeerMessageId};
    use uuid::Uuid;

    let body = SpurEventBody::WorkerPeerMessageAccepted {
        brain_session_id: "bs-1".into(),
        message_id: PeerMessageId(Uuid::new_v4()),
        source_delegation_id: crate::domain::delegation::DelegationId("src".into()),
        target_delegation_id: crate::domain::delegation::DelegationId("tgt".into()),
        kind: MessageKind::Question,
        sequence: 1,
    };
    let json = serde_json::to_string(&body).unwrap();
    let back: SpurEventBody = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, SpurEventBody::WorkerPeerMessageAccepted { .. }));
}

#[test]
fn worker_peer_message_delivered_carries_injected_chars() {
    use crate::domain::peer_message::PeerMessageId;
    use uuid::Uuid;

    let body = SpurEventBody::WorkerPeerMessageDelivered {
        brain_session_id: "bs-1".into(),
        message_id: PeerMessageId(Uuid::new_v4()),
        target_delegation_id: crate::domain::delegation::DelegationId("tgt".into()),
        target_prompt_id: "prompt-uuid".into(),
        injected_chars: 1234,
    };
    let json = serde_json::to_string(&body).unwrap();
    let back: SpurEventBody = serde_json::from_str(&json).unwrap();
    if let SpurEventBody::WorkerPeerMessageDelivered { injected_chars, .. } = back {
        assert_eq!(injected_chars, 1234);
    } else {
        panic!("wrong variant");
    }
}

#[test]
fn worker_peer_mailbox_reconciled_carries_counts() {
    let body = SpurEventBody::WorkerPeerMailboxReconciled {
        audit_failed_emitted: 2,
        inflight_forced_to_delivered: 1,
        inflight_reverted_to_queued: 0,
        guards_re_wrapped: 3,
    };
    let json = serde_json::to_string(&body).unwrap();
    let back: SpurEventBody = serde_json::from_str(&json).unwrap();
    assert!(matches!(back, SpurEventBody::WorkerPeerMailboxReconciled { .. }));
}
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test -p spur-acp worker_peer_message`
Expected: compile error — variants don't exist yet.

- [ ] **Step 3: Add the variants to `SpurEventBody`**

Insert into the `SpurEventBody` enum body (alphabetically grouped near other `Worker*` variants):

```rust
WorkerPeerMessageAccepted {
    brain_session_id: String,
    message_id: crate::domain::peer_message::PeerMessageId,
    source_delegation_id: crate::domain::delegation::DelegationId,
    target_delegation_id: crate::domain::delegation::DelegationId,
    kind: crate::domain::peer_message::MessageKind,
    sequence: u64,
},
WorkerPeerMessageRejected {
    brain_session_id: String,
    message_id: crate::domain::peer_message::PeerMessageId,
    source_delegation_id: crate::domain::delegation::DelegationId,
    reason: String,
},
WorkerPeerMessageQueued {
    brain_session_id: String,
    message_id: crate::domain::peer_message::PeerMessageId,
    target_delegation_id: crate::domain::delegation::DelegationId,
},
WorkerPeerMessageDelivered {
    brain_session_id: String,
    message_id: crate::domain::peer_message::PeerMessageId,
    target_delegation_id: crate::domain::delegation::DelegationId,
    target_prompt_id: String,
    injected_chars: u32,
},
WorkerPeerMessageConsumed {
    brain_session_id: String,
    message_id: crate::domain::peer_message::PeerMessageId,
    target_delegation_id: crate::domain::delegation::DelegationId,
},
WorkerPeerMessageIgnored {
    brain_session_id: String,
    message_id: crate::domain::peer_message::PeerMessageId,
    target_delegation_id: crate::domain::delegation::DelegationId,
    reason: String,
},
WorkerPeerMessageExpired {
    brain_session_id: String,
    message_id: crate::domain::peer_message::PeerMessageId,
    target_delegation_id: crate::domain::delegation::DelegationId,
},
WorkerPeerMessageDropped {
    brain_session_id: String,
    message_id: crate::domain::peer_message::PeerMessageId,
    reason: String,
},
WorkerPeerMessageUndeliverable {
    brain_session_id: String,
    message_id: crate::domain::peer_message::PeerMessageId,
    target_delegation_id: crate::domain::delegation::DelegationId,
    reason: String,
},
WorkerPeerMessageAuditFailed {
    brain_session_id: String,
    message_id: crate::domain::peer_message::PeerMessageId,
    transition_kind: String,
    error: String,
},
WorkerPeerMailboxReconciled {
    audit_failed_emitted: u32,
    inflight_forced_to_delivered: u32,
    inflight_reverted_to_queued: u32,
    guards_re_wrapped: u32,
},
```

- [ ] **Step 4: Run the tests and verify they pass**

Run: `cargo test -p spur-acp worker_peer`
Expected: 3 tests pass.

- [ ] **Step 5: Run the full spur-acp test suite to catch any non-exhaustive match breakage**

Run: `cargo test -p spur-acp`
Expected: all tests pass. If any external `match` on `SpurEventBody` outside `spur-acp` becomes a compile error in dependent crates, the `#[non_exhaustive]` attribute prevents that — but still run `cargo build` workspace-wide to be sure:

Run: `cargo build --workspace`
Expected: success.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs
git commit -m "feat(spur-acp): add 11 peer mailbox event variants"
```

---

## Task 3: Forward-compat replay deserialization wrapper

**Files:**
- Create: `crates/spur-acp/src/domain/replay_compat.rs`
- Modify: `crates/spur-acp/src/domain/mod.rs` (add `pub mod replay_compat;`)

The `SpurEventBody` enum uses externally tagged serde with no `#[serde(other)]` fallback, so an old binary reading a new NDJSON log fails to deserialize unknown variants. This task adds a wrapper that captures unknowns instead of failing.

- [ ] **Step 1: Write the failing test**

Create `crates/spur-acp/src/domain/replay_compat.rs`:

```rust
use serde::{Deserialize, Serialize};

use super::events::SpurEventBody;

/// Replay-compatible body that captures unknown variants without failing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReplayBody {
    Known(SpurEventBody),
    Unknown(serde_json::Value),
}

impl ReplayBody {
    pub fn as_known(&self) -> Option<&SpurEventBody> {
        match self {
            ReplayBody::Known(b) => Some(b),
            ReplayBody::Unknown(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_variant_deserializes_as_known() {
        let json = r#"{"WorkerHeartbeat":{"brain_session_id":"bs","executor_id":"ex","worker_ts":null}}"#;
        let body: ReplayBody = serde_json::from_str(json).unwrap();
        assert!(body.as_known().is_some());
    }

    #[test]
    fn unknown_variant_deserializes_as_unknown_not_error() {
        let json = r#"{"FutureVariantThatDoesNotExist":{"x":1}}"#;
        let body: ReplayBody = serde_json::from_str(json).unwrap();
        assert!(body.as_known().is_none());
    }
}
```

- [ ] **Step 2: Wire into `domain/mod.rs`**

Add: `pub mod replay_compat;` and `pub use replay_compat::ReplayBody;`

- [ ] **Step 3: Run the tests**

Run: `cargo test -p spur-acp replay_compat`
Expected: 2 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/domain/replay_compat.rs crates/spur-acp/src/domain/mod.rs
git commit -m "feat(spur-acp): add forward-compat replay body wrapper"
```

---

## Task 4: Beads label constructor for peer audit references

**Files:**
- Modify: `crates/spur-mcp/src/plan/labels.rs`

The label `spur:peer:{compact_uuid}` is `10 + 32 = 42 chars` — under the 50-char `br create --label` cap (verified at `labels.rs:18-23`). It can use `br create --label` directly, unlike `signal_processed_label` which is 54 chars.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `labels.rs`:

```rust
#[test]
fn peer_message_label_is_under_50_chars_and_uses_compact_uuid() {
    let id = uuid::Uuid::parse_str("0123456789abcdef0123456789abcdef").unwrap();
    let label = peer_message_label(&id);
    assert_eq!(label, "spur:peer:0123456789abcdef0123456789abcdef");
    assert!(
        label.len() <= 50,
        "label exceeds 50-char br create cap: {} chars",
        label.len()
    );
    // Grammar: [A-Za-z0-9_:-]+
    assert!(label.chars().all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '_' || c == '-'));
}
```

- [ ] **Step 2: Run the test, verify it fails**

Run: `cargo test -p spur-mcp peer_message_label`
Expected: compile error — function doesn't exist.

- [ ] **Step 3: Add the constructor**

Add to `labels.rs`, immediately below `signal_processed_label` (~line 146):

```rust
/// Beads audit-reference label for a peer mailbox message.
///
/// Format: `spur:peer:{compact_uuid}` (42 chars). Fits the 50-char
/// `br create --label` cap, unlike `signal_processed_label`.
pub fn peer_message_label(message_id: &uuid::Uuid) -> String {
    format!("spur:peer:{}", message_id.simple())
}
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p spur-mcp peer_message_label`
Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan/labels.rs
git commit -m "feat(spur-mcp): add peer_message_label audit-reference constructor"
```

---

## Task 5: `PlanScopeSnapshot` immutable plan-DAG snapshot

**Files:**
- Create: `crates/spur-mcp/src/plan/scope_snapshot.rs`
- Modify: `crates/spur-mcp/src/plan/mod.rs` (add `pub mod scope_snapshot;` and a `snapshot_for_peer` method on `PlanState`)

The router needs a brief read of `PlanState`'s `tokio::sync::Mutex` (`mod.rs:1045`) to extract only the data it needs, then drop the lock. The snapshot is consumed by `PeerMailboxRouter::validate`.

- [ ] **Step 1: Create the snapshot type with a failing test**

Create `crates/spur-mcp/src/plan/scope_snapshot.rs`:

```rust
use spur_acp::domain::delegation::DelegationId;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct PlanScopeSnapshot {
    pub plan_version: u64,
    /// Set of `(source_task_id, target_task_id)` pairs allowed by the DAG +
    /// brain-approved peer edges.
    pub peer_edges: HashSet<(String, String)>,
    /// Maps `delegation_id` to the task it executes.
    pub delegation_to_task: HashMap<DelegationId, String>,
    /// Maps `delegation_id` to its issue id.
    pub delegation_to_issue: HashMap<DelegationId, String>,
    /// Set of plan task ids that are superseded.
    pub superseded_tasks: HashSet<String>,
    /// Set of plan task ids whose lifecycle is terminal (succeeded, failed, cancelled).
    pub terminal_tasks: HashSet<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum EdgeCheck {
    Allowed,
    NotInDag,
    SourceMissing,
    TargetMissing,
    SourceSuperseded,
    TargetSuperseded,
    SourceTerminal,
}

impl PlanScopeSnapshot {
    pub fn check_peer_edge(
        &self,
        source: &DelegationId,
        target: &DelegationId,
    ) -> EdgeCheck {
        let src_task = match self.delegation_to_task.get(source) {
            Some(t) => t,
            None => return EdgeCheck::SourceMissing,
        };
        let tgt_task = match self.delegation_to_task.get(target) {
            Some(t) => t,
            None => return EdgeCheck::TargetMissing,
        };
        if self.superseded_tasks.contains(src_task) {
            return EdgeCheck::SourceSuperseded;
        }
        if self.superseded_tasks.contains(tgt_task) {
            return EdgeCheck::TargetSuperseded;
        }
        if self.terminal_tasks.contains(src_task) {
            return EdgeCheck::SourceTerminal;
        }
        if !self.peer_edges.contains(&(src_task.clone(), tgt_task.clone())) {
            return EdgeCheck::NotInDag;
        }
        EdgeCheck::Allowed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PlanScopeSnapshot {
        let mut delegation_to_task = HashMap::new();
        delegation_to_task.insert(DelegationId("src-1".into()), "task-a".into());
        delegation_to_task.insert(DelegationId("tgt-1".into()), "task-b".into());
        let mut peer_edges = HashSet::new();
        peer_edges.insert(("task-a".into(), "task-b".into()));
        PlanScopeSnapshot {
            plan_version: 1,
            peer_edges,
            delegation_to_task,
            delegation_to_issue: HashMap::new(),
            superseded_tasks: HashSet::new(),
            terminal_tasks: HashSet::new(),
        }
    }

    #[test]
    fn allowed_edge_returns_allowed() {
        let snap = fixture();
        assert_eq!(
            snap.check_peer_edge(&DelegationId("src-1".into()), &DelegationId("tgt-1".into())),
            EdgeCheck::Allowed
        );
    }

    #[test]
    fn missing_source_returns_source_missing() {
        let snap = fixture();
        assert_eq!(
            snap.check_peer_edge(&DelegationId("nope".into()), &DelegationId("tgt-1".into())),
            EdgeCheck::SourceMissing
        );
    }

    #[test]
    fn superseded_target_blocks_edge() {
        let mut snap = fixture();
        snap.superseded_tasks.insert("task-b".into());
        assert_eq!(
            snap.check_peer_edge(&DelegationId("src-1".into()), &DelegationId("tgt-1".into())),
            EdgeCheck::TargetSuperseded
        );
    }

    #[test]
    fn edge_not_in_dag_blocks_communication() {
        let mut snap = fixture();
        snap.peer_edges.clear();
        assert_eq!(
            snap.check_peer_edge(&DelegationId("src-1".into()), &DelegationId("tgt-1".into())),
            EdgeCheck::NotInDag
        );
    }
}
```

- [ ] **Step 2: Wire module into `plan/mod.rs`**

Add `pub mod scope_snapshot;` near the other `pub mod` lines.

- [ ] **Step 3: Add a `snapshot_for_peer` method on `PlanState`**

Find the `impl PlanState` block in `mod.rs` and add:

```rust
impl PlanState {
    /// Build an immutable snapshot for the peer mailbox router. The caller
    /// briefly holds the `PlanState` lock to construct this; afterwards the
    /// snapshot is read without contention.
    pub fn snapshot_for_peer(&self) -> crate::plan::scope_snapshot::PlanScopeSnapshot {
        // Extract minimal projection from current plan state.
        // Walk plan tasks → build delegation_to_task, delegation_to_issue,
        // superseded_tasks, terminal_tasks.
        // Walk plan DAG → build peer_edges (direct edges + brain-approved peer edges).
        crate::plan::scope_snapshot::PlanScopeSnapshot {
            plan_version: self.version(),
            peer_edges: self.compute_peer_edges(),
            delegation_to_task: self.delegation_to_task_map(),
            delegation_to_issue: self.delegation_to_issue_map(),
            superseded_tasks: self.superseded_task_ids(),
            terminal_tasks: self.terminal_task_ids(),
        }
    }
}
```

The four helper methods (`version`, `compute_peer_edges`, `delegation_to_task_map`, `delegation_to_issue_map`, `superseded_task_ids`, `terminal_task_ids`) likely already partially exist on `PlanState`. If a helper does not exist, add it as a private method that walks `self.tasks` (or whatever the field is named) and returns the projection. **Follow whatever access patterns already exist in `PlanState` for similar projections.** No new field on `PlanState` should be added.

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-mcp scope_snapshot`
Expected: 4 tests pass.

Run: `cargo build --workspace`
Expected: success.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan/scope_snapshot.rs crates/spur-mcp/src/plan/mod.rs
git commit -m "feat(spur-mcp): add PlanScopeSnapshot for peer mailbox routing"
```

---

## Task 6: `PeerMailboxLedger` trait + `InMemoryLedger`

**Files:**
- Create: `crates/spur-core/src/peer_mailbox/mod.rs`
- Create: `crates/spur-core/src/peer_mailbox/ledger.rs`
- Modify: `crates/spur-core/src/lib.rs` (add `pub mod peer_mailbox;`)
- Modify: `crates/spur-core/Cargo.toml` (add `async-trait` if not present)

- [ ] **Step 1: Add `async-trait` to `crates/spur-core/Cargo.toml` if absent**

Check `crates/spur-core/Cargo.toml` `[dependencies]`. If `async-trait` is missing, add: `async-trait = "0.1"`.

- [ ] **Step 2: Create the module root**

Create `crates/spur-core/src/peer_mailbox/mod.rs`:

```rust
pub mod ledger;
pub mod guard;
pub mod limits;
pub mod prompt_builder;
pub mod reconciler;
pub mod router;

pub use ledger::{InMemoryLedger, LedgerEntry, LedgerError, PeerMailboxLedger};
pub use guard::{PeerMessageGuard, GuardOutcome};
pub use router::{PeerMailboxRouter, RouterError};
pub use prompt_builder::{PeerPromptContextBuilder, InjectionRecord};
```

- [ ] **Step 3: Create `ledger.rs` with the failing test**

Create `crates/spur-core/src/peer_mailbox/ledger.rs`:

```rust
use async_trait::async_trait;
use spur_acp::domain::peer_message::{LedgerState, PeerMessageEnvelope, PeerMessageId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("invalid transition from {from:?} to {to:?}")]
    InvalidTransition { from: LedgerState, to: LedgerState },
}

#[derive(Debug, Clone)]
pub struct LedgerEntry {
    pub envelope: PeerMessageEnvelope,
    pub state: LedgerState,
    /// Set of `target_prompt_id`s into which this message has been recorded
    /// as injected. Used for at-most-once injection per prompt.
    pub injected_into_prompts: HashSet<String>,
}

#[async_trait]
pub trait PeerMailboxLedger: Send + Sync {
    async fn accept(&self, envelope: PeerMessageEnvelope) -> Result<(), LedgerError>;
    async fn transition(
        &self,
        message_id: &PeerMessageId,
        next: LedgerState,
    ) -> Result<LedgerState, LedgerError>;
    async fn record_injection(
        &self,
        message_id: &PeerMessageId,
        target_prompt_id: &str,
    ) -> Result<bool, LedgerError>;
    async fn get(&self, message_id: &PeerMessageId) -> Option<LedgerEntry>;
    async fn pending_for_target(
        &self,
        target_delegation_id: &spur_acp::domain::delegation::DelegationId,
    ) -> Vec<LedgerEntry>;
    async fn non_terminal_entries(&self) -> Vec<LedgerEntry>;
}

#[derive(Default)]
pub struct InMemoryLedger {
    inner: Arc<Mutex<HashMap<PeerMessageId, LedgerEntry>>>,
}

impl InMemoryLedger {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl PeerMailboxLedger for InMemoryLedger {
    async fn accept(&self, envelope: PeerMessageEnvelope) -> Result<(), LedgerError> {
        let mut g = self.inner.lock().await;
        let id = envelope.message_id.clone();
        // Idempotent: if already accepted (or further along), no-op.
        if g.contains_key(&id) {
            return Ok(());
        }
        g.insert(
            id,
            LedgerEntry {
                envelope,
                state: LedgerState::Accepted,
                injected_into_prompts: HashSet::new(),
            },
        );
        Ok(())
    }

    async fn transition(
        &self,
        message_id: &PeerMessageId,
        next: LedgerState,
    ) -> Result<LedgerState, LedgerError> {
        let mut g = self.inner.lock().await;
        let entry = g.get_mut(message_id).ok_or(LedgerError::InvalidTransition {
            from: LedgerState::Rejected,
            to: next,
        })?;
        // Idempotency: same-state transitions are no-ops.
        if entry.state == next {
            return Ok(next);
        }
        entry.state = next;
        Ok(next)
    }

    async fn record_injection(
        &self,
        message_id: &PeerMessageId,
        target_prompt_id: &str,
    ) -> Result<bool, LedgerError> {
        let mut g = self.inner.lock().await;
        if let Some(entry) = g.get_mut(message_id) {
            Ok(entry.injected_into_prompts.insert(target_prompt_id.into()))
        } else {
            Err(LedgerError::InvalidTransition {
                from: LedgerState::Rejected,
                to: LedgerState::DeliveredInflight,
            })
        }
    }

    async fn get(&self, message_id: &PeerMessageId) -> Option<LedgerEntry> {
        self.inner.lock().await.get(message_id).cloned()
    }

    async fn pending_for_target(
        &self,
        target_delegation_id: &spur_acp::domain::delegation::DelegationId,
    ) -> Vec<LedgerEntry> {
        self.inner
            .lock()
            .await
            .values()
            .filter(|e| {
                &e.envelope.target_delegation_id == target_delegation_id
                    && matches!(
                        e.state,
                        LedgerState::Accepted | LedgerState::Queued
                    )
            })
            .cloned()
            .collect()
    }

    async fn non_terminal_entries(&self) -> Vec<LedgerEntry> {
        self.inner
            .lock()
            .await
            .values()
            .filter(|e| {
                !matches!(
                    e.state,
                    LedgerState::Rejected
                        | LedgerState::Consumed
                        | LedgerState::Ignored
                        | LedgerState::Expired
                        | LedgerState::Dropped
                        | LedgerState::Undeliverable
                )
            })
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::peer_message::{MessageKind, PeerMessageEnvelope};
    use uuid::Uuid;

    fn envelope(msg: &str) -> PeerMessageEnvelope {
        PeerMessageEnvelope {
            schema: "spur-peer-message/v1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            source_delegation_id: DelegationId("src".into()),
            target_delegation_id: DelegationId("tgt".into()),
            source_issue_id: "i1".into(),
            target_issue_id: "i2".into(),
            source_plan_task_id: "ta".into(),
            target_plan_task_id: "tb".into(),
            source_executor_id: "ex".into(),
            plan_version: 1,
            kind: MessageKind::Question,
            body: msg.into(),
            sequence: 1,
        }
    }

    #[tokio::test]
    async fn accept_is_idempotent() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        ledger.accept(env.clone()).await.unwrap();
        let entry = ledger.get(&env.message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Accepted);
    }

    #[tokio::test]
    async fn record_injection_returns_false_on_duplicate() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        let first = ledger.record_injection(&env.message_id, "prompt-1").await.unwrap();
        let second = ledger.record_injection(&env.message_id, "prompt-1").await.unwrap();
        assert!(first);
        assert!(!second);
    }

    #[tokio::test]
    async fn pending_for_target_excludes_terminal_states() {
        let ledger = InMemoryLedger::new();
        let env = envelope("hi");
        ledger.accept(env.clone()).await.unwrap();
        assert_eq!(ledger.pending_for_target(&env.target_delegation_id).await.len(), 1);
        ledger.transition(&env.message_id, LedgerState::Consumed).await.unwrap();
        assert_eq!(ledger.pending_for_target(&env.target_delegation_id).await.len(), 0);
    }
}
```

- [ ] **Step 4: Wire module into `lib.rs`**

Append to `crates/spur-core/src/lib.rs`:

```rust
pub mod peer_mailbox;
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-core peer_mailbox::ledger`
Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/peer_mailbox/ crates/spur-core/src/lib.rs crates/spur-core/Cargo.toml
git commit -m "feat(spur-core): add PeerMailboxLedger trait and in-memory impl"
```

---

## Task 7: `PeerMessageGuard` with sync-only Drop

**Files:**
- Create: `crates/spur-core/src/peer_mailbox/guard.rs`

The guard's `Drop` impl must NOT do async work (no `block_on`, no `tokio::spawn` from a runtime that may be shutting down). It enqueues onto a long-lived unbounded mpsc that a separate reconciler task drains. If the runtime is fully gone, the entries are recovered on next startup.

- [ ] **Step 1: Write the file with the failing test**

Create `crates/spur-core/src/peer_mailbox/guard.rs`:

```rust
use spur_acp::domain::peer_message::{PeerMessageId, TerminalOutcome};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

/// Sent to the reconciler when a `PeerMessageGuard` drops without finalize.
#[derive(Debug, Clone)]
pub struct StrandedMessage {
    pub message_id: PeerMessageId,
    pub default_outcome: TerminalOutcome,
}

/// Outcome that `finalize` records on the ledger.
#[derive(Debug, Clone)]
pub enum GuardOutcome {
    Terminal(TerminalOutcome),
}

/// RAII guard that ensures every accepted-but-not-terminal peer message
/// reaches a terminal state, even on panic or task abort.
///
/// Construct via `PeerMessageGuard::wrap`. Resolve via `finalize().await`.
/// If dropped without finalize, the guard enqueues a `StrandedMessage` onto
/// the reconciler mpsc (sync, non-blocking) and logs a `tracing::error!`.
/// **Never** performs async work in `Drop`.
pub struct PeerMessageGuard {
    message_id: PeerMessageId,
    reconciler_tx: UnboundedSender<StrandedMessage>,
    default_outcome: TerminalOutcome,
    finalized: bool,
}

impl PeerMessageGuard {
    pub fn wrap(
        message_id: PeerMessageId,
        reconciler_tx: UnboundedSender<StrandedMessage>,
        default_outcome: TerminalOutcome,
    ) -> Self {
        Self {
            message_id,
            reconciler_tx,
            default_outcome,
            finalized: false,
        }
    }

    /// Normal-path resolution. Marks the guard finalized; `Drop` becomes a no-op.
    /// The caller is responsible for performing the actual ledger transition,
    /// beads write, and event emission *before* calling this.
    pub async fn finalize(mut self, _outcome: GuardOutcome) {
        self.finalized = true;
        // The actual ledger/beads/event work is done by the caller before
        // calling `finalize`. This method only flips the flag so Drop is a no-op.
        drop(self);
    }

    pub fn message_id(&self) -> &PeerMessageId {
        &self.message_id
    }
}

impl Drop for PeerMessageGuard {
    fn drop(&mut self) {
        if self.finalized {
            return;
        }
        tracing::error!(
            message_id = ?self.message_id,
            "PeerMessageGuard dropped without finalize; enqueueing stranded recovery"
        );
        let _ = self.reconciler_tx.send(StrandedMessage {
            message_id: self.message_id.clone(),
            default_outcome: self.default_outcome.clone(),
        });
    }
}

/// Long-lived task that drains the stranded-message mpsc and applies recovery
/// transitions. Spawned at orchestrator boot; survives across attempts.
pub async fn run_reconciler_loop(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<StrandedMessage>,
    ledger: Arc<dyn crate::peer_mailbox::ledger::PeerMailboxLedger>,
    funnel: crate::event_funnel::FunnelHandle,
    brain_session_id: String,
) {
    while let Some(stranded) = rx.recv().await {
        let reason = match &stranded.default_outcome {
            TerminalOutcome::Undeliverable { reason } => reason.clone(),
            _ => "guard_dropped_unfinalized".into(),
        };
        let _ = ledger
            .transition(
                &stranded.message_id,
                spur_acp::domain::peer_message::LedgerState::Undeliverable,
            )
            .await;
        // Look up to get target_delegation_id for the event payload.
        if let Some(entry) = ledger.get(&stranded.message_id).await {
            funnel.emit(spur_acp::SpurEventBody::WorkerPeerMessageUndeliverable {
                brain_session_id: brain_session_id.clone(),
                message_id: stranded.message_id,
                target_delegation_id: entry.envelope.target_delegation_id,
                reason,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    #[tokio::test]
    async fn drop_without_finalize_enqueues_stranded() {
        let (tx, mut rx) = unbounded_channel();
        let id = PeerMessageId(uuid::Uuid::new_v4());
        {
            let _guard = PeerMessageGuard::wrap(
                id.clone(),
                tx,
                TerminalOutcome::Undeliverable {
                    reason: "test".into(),
                },
            );
        } // guard dropped here without finalize
        let stranded = rx.recv().await.expect("expected stranded message");
        assert_eq!(stranded.message_id, id);
    }

    #[tokio::test]
    async fn finalize_prevents_stranded_enqueue() {
        let (tx, mut rx) = unbounded_channel();
        let id = PeerMessageId(uuid::Uuid::new_v4());
        let guard = PeerMessageGuard::wrap(
            id,
            tx,
            TerminalOutcome::Undeliverable {
                reason: "test".into(),
            },
        );
        guard.finalize(GuardOutcome::Terminal(TerminalOutcome::Consumed)).await;
        // No message should arrive.
        assert!(rx.try_recv().is_err());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-core peer_mailbox::guard`
Expected: 2 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/peer_mailbox/guard.rs crates/spur-core/src/peer_mailbox/mod.rs
git commit -m "feat(spur-core): add PeerMessageGuard with sync-only Drop"
```

---

## Task 8: `PeerMailboxRouter` with validation pipeline

**Files:**
- Create: `crates/spur-core/src/peer_mailbox/router.rs`

The router validates against `PlanScopeSnapshot`, transitions the ledger, emits events, and returns a `PeerMessageGuard` to the caller. **Backpressure is enforced at validation, before any event is enqueued onto the funnel** (per the spec's Safety Limits section).

- [ ] **Step 1: Write the file with failing tests**

Create `crates/spur-core/src/peer_mailbox/router.rs`:

```rust
use crate::event_funnel::FunnelHandle;
use crate::peer_mailbox::guard::{PeerMessageGuard, StrandedMessage};
use crate::peer_mailbox::ledger::PeerMailboxLedger;
use crate::peer_mailbox::limits::Limits;
use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{
    LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId, TerminalOutcome,
};
use spur_acp::SpurEventBody;
use spur_mcp::plan::scope_snapshot::{EdgeCheck, PlanScopeSnapshot};
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RouterError {
    #[error("rejected: {reason}")]
    Rejected { reason: String },
    #[error("ledger error: {0}")]
    Ledger(String),
}

pub struct PeerMailboxRouter {
    ledger: Arc<dyn PeerMailboxLedger>,
    funnel: FunnelHandle,
    reconciler_tx: UnboundedSender<StrandedMessage>,
    limits: Limits,
    brain_session_id: String,
}

impl PeerMailboxRouter {
    pub fn new(
        ledger: Arc<dyn PeerMailboxLedger>,
        funnel: FunnelHandle,
        reconciler_tx: UnboundedSender<StrandedMessage>,
        limits: Limits,
        brain_session_id: String,
    ) -> Self {
        Self {
            ledger,
            funnel,
            reconciler_tx,
            limits,
            brain_session_id,
        }
    }

    /// Validate, persist, and emit. Returns a `PeerMessageGuard` if accepted;
    /// the caller is responsible for finalizing or letting it stranded-recover.
    pub async fn accept_or_reject(
        &self,
        request: PeerMessageEnvelope,
        snapshot: &PlanScopeSnapshot,
    ) -> Result<PeerMessageGuard, RouterError> {
        // Body size cap (backpressure before any funnel emit).
        if request.body.len() > self.limits.max_peer_message_size {
            return Err(self.reject(
                request,
                "body_size_exceeded",
            ));
        }

        // Plan version match.
        if request.plan_version != snapshot.plan_version {
            return Err(self.reject(request, "plan_version_changed"));
        }

        // DAG / supersession check.
        match snapshot.check_peer_edge(
            &request.source_delegation_id,
            &request.target_delegation_id,
        ) {
            EdgeCheck::Allowed => {}
            EdgeCheck::NotInDag => return Err(self.reject(request, "not_in_dag")),
            EdgeCheck::SourceMissing => return Err(self.reject(request, "source_missing")),
            EdgeCheck::TargetMissing => return Err(self.reject(request, "target_missing")),
            EdgeCheck::SourceSuperseded => {
                return Err(self.reject(request, "source_superseded"))
            }
            EdgeCheck::TargetSuperseded => {
                return Err(self.reject(request, "target_superseded"))
            }
            EdgeCheck::SourceTerminal => return Err(self.reject(request, "source_terminal")),
        }

        // Idempotent accept.
        let envelope = request.clone();
        self.ledger
            .accept(envelope.clone())
            .await
            .map_err(|e| RouterError::Ledger(format!("{e}")))?;

        // Emit `Accepted` event.
        self.funnel.emit(SpurEventBody::WorkerPeerMessageAccepted {
            brain_session_id: self.brain_session_id.clone(),
            message_id: envelope.message_id.clone(),
            source_delegation_id: envelope.source_delegation_id.clone(),
            target_delegation_id: envelope.target_delegation_id.clone(),
            kind: envelope.kind,
            sequence: envelope.sequence,
        });

        // Construct guard. Default outcome is Undeliverable on stranded drop.
        Ok(PeerMessageGuard::wrap(
            envelope.message_id,
            self.reconciler_tx.clone(),
            TerminalOutcome::Undeliverable {
                reason: "guard_dropped_unfinalized".into(),
            },
        ))
    }

    fn reject(&self, request: PeerMessageEnvelope, reason: &str) -> RouterError {
        self.funnel.emit(SpurEventBody::WorkerPeerMessageRejected {
            brain_session_id: self.brain_session_id.clone(),
            message_id: request.message_id,
            source_delegation_id: request.source_delegation_id,
            reason: reason.into(),
        });
        RouterError::Rejected {
            reason: reason.into(),
        }
    }

    pub async fn record_terminal(
        &self,
        message_id: &PeerMessageId,
        outcome: TerminalOutcome,
    ) -> Result<(), RouterError> {
        let next = match &outcome {
            TerminalOutcome::Consumed => LedgerState::Consumed,
            TerminalOutcome::Ignored { .. } => LedgerState::Ignored,
            TerminalOutcome::Expired => LedgerState::Expired,
            TerminalOutcome::Dropped { .. } => LedgerState::Dropped,
            TerminalOutcome::Undeliverable { .. } => LedgerState::Undeliverable,
        };
        self.ledger
            .transition(message_id, next)
            .await
            .map_err(|e| RouterError::Ledger(format!("{e}")))?;
        // Emit lifecycle event matching the outcome.
        if let Some(entry) = self.ledger.get(message_id).await {
            let body = match outcome {
                TerminalOutcome::Consumed => SpurEventBody::WorkerPeerMessageConsumed {
                    brain_session_id: self.brain_session_id.clone(),
                    message_id: message_id.clone(),
                    target_delegation_id: entry.envelope.target_delegation_id,
                },
                TerminalOutcome::Ignored { reason } => SpurEventBody::WorkerPeerMessageIgnored {
                    brain_session_id: self.brain_session_id.clone(),
                    message_id: message_id.clone(),
                    target_delegation_id: entry.envelope.target_delegation_id,
                    reason,
                },
                TerminalOutcome::Expired => SpurEventBody::WorkerPeerMessageExpired {
                    brain_session_id: self.brain_session_id.clone(),
                    message_id: message_id.clone(),
                    target_delegation_id: entry.envelope.target_delegation_id,
                },
                TerminalOutcome::Dropped { reason } => SpurEventBody::WorkerPeerMessageDropped {
                    brain_session_id: self.brain_session_id.clone(),
                    message_id: message_id.clone(),
                    reason,
                },
                TerminalOutcome::Undeliverable { reason } => {
                    SpurEventBody::WorkerPeerMessageUndeliverable {
                        brain_session_id: self.brain_session_id.clone(),
                        message_id: message_id.clone(),
                        target_delegation_id: entry.envelope.target_delegation_id,
                        reason,
                    }
                }
            };
            self.funnel.emit(body);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_mailbox::ledger::InMemoryLedger;
    use crate::peer_mailbox::limits::Limits;
    use spur_mcp::plan::scope_snapshot::PlanScopeSnapshot;
    use std::collections::{HashMap, HashSet};
    use tokio::sync::mpsc::unbounded_channel;
    use uuid::Uuid;

    fn snapshot_allowing(src: &str, tgt: &str) -> PlanScopeSnapshot {
        let mut delegation_to_task = HashMap::new();
        delegation_to_task.insert(DelegationId(src.into()), "ta".into());
        delegation_to_task.insert(DelegationId(tgt.into()), "tb".into());
        let mut peer_edges = HashSet::new();
        peer_edges.insert(("ta".into(), "tb".into()));
        PlanScopeSnapshot {
            plan_version: 1,
            peer_edges,
            delegation_to_task,
            delegation_to_issue: HashMap::new(),
            superseded_tasks: HashSet::new(),
            terminal_tasks: HashSet::new(),
        }
    }

    fn envelope(src: &str, tgt: &str) -> PeerMessageEnvelope {
        PeerMessageEnvelope {
            schema: "spur-peer-message/v1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            source_delegation_id: DelegationId(src.into()),
            target_delegation_id: DelegationId(tgt.into()),
            source_issue_id: "i1".into(),
            target_issue_id: "i2".into(),
            source_plan_task_id: "ta".into(),
            target_plan_task_id: "tb".into(),
            source_executor_id: "ex".into(),
            plan_version: 1,
            kind: MessageKind::Question,
            body: "hi".into(),
            sequence: 1,
        }
    }

    async fn fixture() -> (PeerMailboxRouter, Arc<InMemoryLedger>) {
        let ledger = Arc::new(InMemoryLedger::new());
        let funnel = crate::event_funnel::spawn_funnel(4096).0;
        let (tx, _rx) = unbounded_channel();
        let limits = Limits::default();
        let router = PeerMailboxRouter::new(
            ledger.clone(),
            funnel,
            tx,
            limits,
            "bs".into(),
        );
        (router, ledger)
    }

    #[tokio::test]
    async fn accept_succeeds_for_allowed_edge() {
        let (router, _ledger) = fixture().await;
        let snap = snapshot_allowing("src", "tgt");
        let env = envelope("src", "tgt");
        let guard = router.accept_or_reject(env, &snap).await.unwrap();
        // Finalize so guard doesn't strand.
        guard.finalize(crate::peer_mailbox::guard::GuardOutcome::Terminal(
            TerminalOutcome::Consumed,
        )).await;
    }

    #[tokio::test]
    async fn rejects_when_not_in_dag() {
        let (router, _ledger) = fixture().await;
        let mut snap = snapshot_allowing("src", "tgt");
        snap.peer_edges.clear();
        let env = envelope("src", "tgt");
        let err = router.accept_or_reject(env, &snap).await.unwrap_err();
        assert_eq!(err, RouterError::Rejected { reason: "not_in_dag".into() });
    }

    #[tokio::test]
    async fn rejects_oversized_body_before_any_validation() {
        let (router, _ledger) = fixture().await;
        let snap = snapshot_allowing("src", "tgt");
        let mut env = envelope("src", "tgt");
        env.body = "x".repeat(100_000);
        let err = router.accept_or_reject(env, &snap).await.unwrap_err();
        assert_eq!(err, RouterError::Rejected { reason: "body_size_exceeded".into() });
    }
}
```

- [ ] **Step 2: Verify the test fixture's funnel-spawn matches the actual API**

Open `crates/spur-core/src/event_funnel.rs` and verify the public function is `spawn_funnel(capacity: usize) -> (FunnelHandle, ...)`. If the signature differs, adjust the test fixture. The test only needs a valid `FunnelHandle`; do not exercise the broadcast side.

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core peer_mailbox::router`
Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/peer_mailbox/router.rs
git commit -m "feat(spur-core): add PeerMailboxRouter with validation pipeline"
```

---

## Task 9: Tiered safety limits

**Files:**
- Create: `crates/spur-core/src/peer_mailbox/limits.rs`

- [ ] **Step 1: Write the file with failing tests**

Create `crates/spur-core/src/peer_mailbox/limits.rs`:

```rust
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_peer_message_size: usize,
    pub max_pending_mailbox_depth: usize,
    pub max_messages_per_source_delegation: usize,
    pub max_fanout_per_message: usize,
    pub drain_quiet_window_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_peer_message_size: 2_048,
            max_pending_mailbox_depth: 8,
            max_messages_per_source_delegation: 32,
            max_fanout_per_message: 4,
            drain_quiet_window_ms: 2_000,
        }
    }
}

/// Returns the aggregate peer-context budget in chars for the given target
/// context window size.
pub fn aggregate_budget_for_context_window(window_chars: u64) -> u64 {
    let pct = if window_chars < 64_000 {
        10
    } else if window_chars < 128_000 {
        7
    } else {
        5
    };
    window_chars * pct / 100
}

/// Returns the effective per-message size cap given a configured cap and the
/// target's aggregate budget. Bounded by `aggregate / max_depth`.
pub fn effective_max_message_size(
    configured_cap: usize,
    aggregate_budget: u64,
    max_depth: usize,
) -> usize {
    let derived = (aggregate_budget / max_depth.max(1) as u64) as usize;
    configured_cap.min(derived)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_is_10pct_under_64k() {
        assert_eq!(aggregate_budget_for_context_window(32_000), 3_200);
    }

    #[test]
    fn budget_is_7pct_at_64k_to_128k() {
        assert_eq!(aggregate_budget_for_context_window(64_000), 4_480);
        assert_eq!(aggregate_budget_for_context_window(127_999), 8_959);
    }

    #[test]
    fn budget_is_5pct_at_or_above_128k() {
        assert_eq!(aggregate_budget_for_context_window(128_000), 6_400);
        assert_eq!(aggregate_budget_for_context_window(200_000), 10_000);
    }

    #[test]
    fn effective_max_message_size_is_min_of_configured_and_derived() {
        // 32k window → 3200 budget, 8 depth → 400 derived. Config 2048.
        assert_eq!(effective_max_message_size(2_048, 3_200, 8), 400);
        // 200k window → 10000 budget, 8 depth → 1250 derived. Config 2048 → 1250.
        assert_eq!(effective_max_message_size(2_048, 10_000, 8), 1_250);
        // 200k window → 10000 budget, 1 depth → 10000. Config 2048 → 2048.
        assert_eq!(effective_max_message_size(2_048, 10_000, 1), 2_048);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-core peer_mailbox::limits`
Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/peer_mailbox/limits.rs
git commit -m "feat(spur-core): add tiered context-window budget limits"
```

---

## Task 10: `_spur/peer_message*` allowlist parsing

**Files:**
- Modify: `crates/spur-core/src/spur_ext_interp.rs`

The pattern follows the existing `_spur/heartbeat` arm at line 23. Three new method names are added; payload parsing handed off to the router via a callback or a router reference passed in.

- [ ] **Step 1: Inspect the current `interpret` function signature**

Open `crates/spur-core/src/spur_ext_interp.rs:22` and note the function signature. The router needs to be reachable; the simplest extension is to add the router as a parameter to `interpret` (or via a context struct). Examine existing parameters and follow the pattern.

- [ ] **Step 2: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `spur_ext_interp.rs` (or create one if absent):

```rust
#[tokio::test]
async fn peer_message_method_routes_to_router() {
    use crate::peer_mailbox::ledger::InMemoryLedger;
    use crate::peer_mailbox::limits::Limits;
    use crate::peer_mailbox::router::PeerMailboxRouter;
    use spur_mcp::plan::scope_snapshot::PlanScopeSnapshot;
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;
    use tokio::sync::mpsc::unbounded_channel;

    let ledger = Arc::new(InMemoryLedger::new());
    let funnel = crate::event_funnel::spawn_funnel(4096).0;
    let (tx, _rx) = unbounded_channel();
    let router = Arc::new(PeerMailboxRouter::new(
        ledger.clone(),
        funnel.clone(),
        tx,
        Limits::default(),
        "bs".into(),
    ));

    let mut delegation_to_task = HashMap::new();
    delegation_to_task.insert(spur_acp::domain::delegation::DelegationId("src".into()), "ta".into());
    delegation_to_task.insert(spur_acp::domain::delegation::DelegationId("tgt".into()), "tb".into());
    let mut peer_edges = HashSet::new();
    peer_edges.insert(("ta".into(), "tb".into()));
    let snapshot = Arc::new(PlanScopeSnapshot {
        plan_version: 1,
        peer_edges,
        delegation_to_task,
        delegation_to_issue: HashMap::new(),
        superseded_tasks: HashSet::new(),
        terminal_tasks: HashSet::new(),
    });

    let payload = serde_json::json!({
        "schema": "spur-peer-message/v1",
        "message_id": uuid::Uuid::new_v4(),
        "target_delegation_id": "tgt",
        "target_issue_id": "i2",
        "target_plan_task_id": "tb",
        "kind": "question",
        "body": "test",
        "sequence": 1
    });

    let result = interpret_peer_message(
        &router,
        &snapshot,
        spur_acp::domain::delegation::DelegationId("src".into()),
        "ex".into(),
        "i1".into(),
        "ta".into(),
        payload,
    ).await;

    assert!(result.is_ok());
}
```

- [ ] **Step 3: Add the parsing arms and a helper for the test**

Inside `interpret` (the existing match block at line 22), after the existing three arms and before `other =>`:

```rust
"_spur/peer_message" => {
    // Source identity is stamped from orchestrator context (NOT from worker payload).
    // The router will be invoked by the orchestrator's notification consumer.
    funnel.emit(SpurEventBody::WorkerNotification {
        brain_session_id,
        executor_id,
        method: "_spur/peer_message".into(),
        params: payload.params.clone(),
    });
    // The actual routing happens in the orchestrator's peer_notification_handler
    // (see Task 11), which has access to the router and snapshot.
}
"_spur/peer_message_consumed" | "_spur/peer_message_ignored" => {
    funnel.emit(SpurEventBody::WorkerNotification {
        brain_session_id,
        executor_id,
        method: payload.method.clone(),
        params: payload.params.clone(),
    });
    // Same handoff pattern: the orchestrator picks these up via WorkerNotification
    // and dispatches to router.record_terminal.
}
```

Then add the helper used by the test (place at the bottom of the file, outside `interpret`):

```rust
pub async fn interpret_peer_message(
    router: &std::sync::Arc<crate::peer_mailbox::router::PeerMailboxRouter>,
    snapshot: &std::sync::Arc<spur_mcp::plan::scope_snapshot::PlanScopeSnapshot>,
    source_delegation_id: spur_acp::domain::delegation::DelegationId,
    source_executor_id: String,
    source_issue_id: String,
    source_plan_task_id: String,
    payload: serde_json::Value,
) -> Result<crate::peer_mailbox::guard::PeerMessageGuard, crate::peer_mailbox::router::RouterError> {
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::peer_message::{MessageKind, PeerMessageEnvelope, PeerMessageId};

    let message_id: uuid::Uuid = serde_json::from_value(payload["message_id"].clone())
        .map_err(|e| crate::peer_mailbox::router::RouterError::Rejected {
            reason: format!("malformed_message_id: {e}"),
        })?;
    let target_delegation_id: String = payload["target_delegation_id"]
        .as_str()
        .ok_or(crate::peer_mailbox::router::RouterError::Rejected {
            reason: "missing_target_delegation_id".into(),
        })?
        .into();
    let target_issue_id: String = payload["target_issue_id"]
        .as_str()
        .unwrap_or("")
        .into();
    let target_plan_task_id: String = payload["target_plan_task_id"]
        .as_str()
        .unwrap_or("")
        .into();
    let kind: MessageKind = serde_json::from_value(payload["kind"].clone())
        .map_err(|e| crate::peer_mailbox::router::RouterError::Rejected {
            reason: format!("malformed_kind: {e}"),
        })?;
    let body: String = payload["body"].as_str().unwrap_or("").into();
    let sequence: u64 = payload["sequence"].as_u64().unwrap_or(0);

    let envelope = PeerMessageEnvelope {
        schema: "spur-peer-message/v1".into(),
        message_id: PeerMessageId(message_id),
        source_delegation_id,
        target_delegation_id: DelegationId(target_delegation_id),
        source_issue_id,
        target_issue_id,
        source_plan_task_id,
        target_plan_task_id,
        source_executor_id,
        plan_version: snapshot.plan_version,
        kind,
        body,
        sequence,
    };

    router.accept_or_reject(envelope, snapshot).await
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core peer_message_method`
Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/spur_ext_interp.rs
git commit -m "feat(spur-core): wire _spur/peer_message* methods through interp"
```

---

## Task 11: `PeerPromptContextBuilder` with `target_prompt_id`

**Files:**
- Create: `crates/spur-core/src/peer_mailbox/prompt_builder.rs`

- [ ] **Step 1: Write the file with failing tests**

Create `crates/spur-core/src/peer_mailbox/prompt_builder.rs`:

```rust
use crate::peer_mailbox::ledger::{LedgerEntry, PeerMailboxLedger};
use crate::peer_mailbox::limits::{aggregate_budget_for_context_window, effective_max_message_size};
use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::PeerMessageId;
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct InjectionRecord {
    pub message_id: PeerMessageId,
    pub injected_chars: u32,
}

#[derive(Debug, Clone)]
pub struct BuiltContext {
    pub target_prompt_id: String,
    pub orchestrator_authored_text: String,
    pub injection_records: Vec<InjectionRecord>,
}

pub struct PeerPromptContextBuilder {
    ledger: Arc<dyn PeerMailboxLedger>,
}

impl PeerPromptContextBuilder {
    pub fn new(ledger: Arc<dyn PeerMailboxLedger>) -> Self {
        Self { ledger }
    }

    pub async fn build_for_target(
        &self,
        target_delegation_id: &DelegationId,
        target_context_window_chars: u64,
        max_pending_mailbox_depth: usize,
        configured_max_message_size: usize,
    ) -> BuiltContext {
        let target_prompt_id = format!("prompt-{}", Uuid::new_v4().simple());
        let pending = self.ledger.pending_for_target(target_delegation_id).await;

        let aggregate_budget = aggregate_budget_for_context_window(target_context_window_chars);
        let per_msg_cap = effective_max_message_size(
            configured_max_message_size,
            aggregate_budget,
            max_pending_mailbox_depth,
        );

        let mut text = String::new();
        let mut injections = Vec::new();
        let mut budget_remaining = aggregate_budget as usize;

        for entry in pending.into_iter().take(max_pending_mailbox_depth) {
            // Skip if already injected to a previous prompt of THIS target_prompt_id.
            // (target_prompt_id is brand-new here, so nothing to filter; this guard
            // catches retries/replays where the same prompt id is reused.)
            if entry.injected_into_prompts.contains(&target_prompt_id) {
                continue;
            }
            let truncated_body = if entry.envelope.body.len() > per_msg_cap {
                &entry.envelope.body[..per_msg_cap]
            } else {
                entry.envelope.body.as_str()
            };
            let block = format!(
                "\n\n[peer:{kind:?} from={src} seq={seq}]\n{body}\n",
                kind = entry.envelope.kind,
                src = entry.envelope.source_executor_id,
                seq = entry.envelope.sequence,
                body = truncated_body,
            );
            if block.len() > budget_remaining {
                break;
            }
            budget_remaining -= block.len();
            text.push_str(&block);
            injections.push(InjectionRecord {
                message_id: entry.envelope.message_id.clone(),
                injected_chars: block.len() as u32,
            });
        }

        BuiltContext {
            target_prompt_id,
            orchestrator_authored_text: text,
            injection_records: injections,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_mailbox::ledger::InMemoryLedger;
    use spur_acp::domain::peer_message::{MessageKind, PeerMessageEnvelope};

    fn envelope(body: &str) -> PeerMessageEnvelope {
        PeerMessageEnvelope {
            schema: "spur-peer-message/v1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            source_delegation_id: DelegationId("src".into()),
            target_delegation_id: DelegationId("tgt".into()),
            source_issue_id: "i1".into(),
            target_issue_id: "i2".into(),
            source_plan_task_id: "ta".into(),
            target_plan_task_id: "tb".into(),
            source_executor_id: "ex".into(),
            plan_version: 1,
            kind: MessageKind::Handoff,
            body: body.into(),
            sequence: 1,
        }
    }

    #[tokio::test]
    async fn builder_generates_unique_target_prompt_id_per_call() {
        let ledger = Arc::new(InMemoryLedger::new());
        let builder = PeerPromptContextBuilder::new(ledger);
        let a = builder
            .build_for_target(&DelegationId("tgt".into()), 200_000, 8, 2_048)
            .await;
        let b = builder
            .build_for_target(&DelegationId("tgt".into()), 200_000, 8, 2_048)
            .await;
        assert_ne!(a.target_prompt_id, b.target_prompt_id);
    }

    #[tokio::test]
    async fn builder_returns_pending_messages_within_budget() {
        let ledger = Arc::new(InMemoryLedger::new());
        ledger.accept(envelope("first")).await.unwrap();
        ledger.accept(envelope("second")).await.unwrap();
        let builder = PeerPromptContextBuilder::new(ledger);
        let ctx = builder
            .build_for_target(&DelegationId("tgt".into()), 200_000, 8, 2_048)
            .await;
        assert_eq!(ctx.injection_records.len(), 2);
        assert!(ctx.orchestrator_authored_text.contains("first"));
        assert!(ctx.orchestrator_authored_text.contains("second"));
    }

    #[tokio::test]
    async fn builder_truncates_oversized_messages_to_per_msg_cap() {
        let ledger = Arc::new(InMemoryLedger::new());
        // 32k window → 3200 budget; depth 8 → 400 derived per-msg cap.
        ledger.accept(envelope(&"X".repeat(2_000))).await.unwrap();
        let builder = PeerPromptContextBuilder::new(ledger);
        let ctx = builder
            .build_for_target(&DelegationId("tgt".into()), 32_000, 8, 2_048)
            .await;
        assert_eq!(ctx.injection_records.len(), 1);
        assert!(ctx.injection_records[0].injected_chars <= 500);
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-core peer_mailbox::prompt_builder`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/peer_mailbox/prompt_builder.rs
git commit -m "feat(spur-core): add PeerPromptContextBuilder with target_prompt_id"
```

---

## Task 12: Orchestrator pre-prompt + post-dispatch hooks

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

The hooks live inside `run_one_worker_attempt` (signature at line 4538). The pre-prompt hook runs immediately before `PromptRequest` construction; the post-dispatch hook runs after `connection.prompt()` returns `Ok`.

This task assumes a `PeerMailboxRouter` and a `PeerPromptContextBuilder` are reachable through the orchestrator's existing context (likely via `WorkerAttemptCtx` or a new `peer_mailbox` field on `Orchestrator`). The exact wiring follows whatever pattern the orchestrator already uses for similar service handles.

- [ ] **Step 1: Add a peer_mailbox bundle field to `Orchestrator`**

Find the `pub struct Orchestrator` definition in `orchestrator.rs`. Add a field:

```rust
pub(crate) peer_mailbox: Option<crate::peer_mailbox::PeerMailboxBundle>,
```

Where `PeerMailboxBundle` is a small struct grouping the router, builder, and ledger (added in Task 6's `mod.rs`):

```rust
// Add to crates/spur-core/src/peer_mailbox/mod.rs
use std::sync::Arc;

pub struct PeerMailboxBundle {
    pub router: Arc<router::PeerMailboxRouter>,
    pub builder: Arc<prompt_builder::PeerPromptContextBuilder>,
    pub ledger: Arc<dyn ledger::PeerMailboxLedger>,
    pub plan_state: Arc<tokio::sync::Mutex<spur_mcp::plan::PlanState>>,
}
```

The `Option<...>` matches the feature-flag-default-off requirement: when peer mailbox is disabled, the bundle is `None` and all hooks are skipped.

- [ ] **Step 2: Add the pre-prompt injection hook**

Locate the `PromptRequest::new(...)` construction site inside `run_one_worker_attempt`. Immediately before that call, insert:

```rust
// Pre-prompt peer-mailbox injection hook.
let peer_context = match &self.peer_mailbox {
    Some(bundle) => {
        // Find the worker's target context window from agent config.
        let context_window = ctx.agent_config.context_window_chars.unwrap_or(200_000);
        let target_delegation = spur_acp::domain::delegation::DelegationId(ctx.request_id.clone());
        let built = bundle
            .builder
            .build_for_target(
                &target_delegation,
                context_window,
                bundle.router.limits().max_pending_mailbox_depth,
                bundle.router.limits().max_peer_message_size,
            )
            .await;
        // Record injection in the ledger BEFORE prompt dispatch.
        for inj in &built.injection_records {
            let _ = bundle
                .ledger
                .record_injection(&inj.message_id, &built.target_prompt_id)
                .await;
        }
        Some(built)
    }
    None => None,
};
```

- [ ] **Step 3: Inject the orchestrator-authored text into the prompt**

Where `PromptRequest::new(...)` builds its `prompt: Vec<ContentBlock>`, append the peer context as a leading orchestrator-authored block:

```rust
let mut prompt_blocks: Vec<ContentBlock> = vec![/* ... existing prompt construction ... */];
if let Some(pc) = &peer_context {
    if !pc.orchestrator_authored_text.is_empty() {
        prompt_blocks.insert(
            0,
            ContentBlock::text(format!(
                "## Peer messages (orchestrator-authored)\n{}",
                pc.orchestrator_authored_text
            )),
        );
    }
}
let prompt_request = PromptRequest::new(worker_session.clone(), prompt_blocks);
```

(The exact `ContentBlock::text` constructor name follows the existing pattern in `agent-client-protocol-schema`. Cross-reference an existing prompt construction call to mirror it.)

- [ ] **Step 4: Add the post-dispatch ledger transition**

Immediately after `drive_prompt_notifications(...)` returns `Ok(...)` (around `orchestrator.rs:4670` per earlier verification), insert:

```rust
// Post-dispatch peer-mailbox ledger transition.
if let (Some(bundle), Some(pc)) = (&self.peer_mailbox, peer_context) {
    use spur_acp::domain::peer_message::LedgerState;
    for inj in pc.injection_records {
        // Move from Accepted/Queued → Delivered_inflight → Delivered.
        let _ = bundle
            .ledger
            .transition(&inj.message_id, LedgerState::DeliveredInflight)
            .await;
        let _ = bundle
            .ledger
            .transition(&inj.message_id, LedgerState::Delivered)
            .await;
        funnel.emit(spur_acp::SpurEventBody::WorkerPeerMessageDelivered {
            brain_session_id: ctx.brain_session_id.clone(),
            message_id: inj.message_id,
            target_delegation_id: spur_acp::domain::delegation::DelegationId(
                ctx.request_id.clone(),
            ),
            target_prompt_id: pc.target_prompt_id.clone(),
            injected_chars: inj.injected_chars,
        });
        // Beads audit reference (best-effort; failure → audit-failed event).
        // Wire to spur-pm here if the peer-pm path is established; otherwise
        // emit WorkerPeerMessageAuditFailed and continue.
    }
}
```

- [ ] **Step 5: Add a `limits()` accessor on `PeerMailboxRouter`**

In `router.rs`, add:

```rust
impl PeerMailboxRouter {
    pub fn limits(&self) -> &crate::peer_mailbox::limits::Limits {
        &self.limits
    }
}
```

- [ ] **Step 6: Build and verify the orchestrator compiles**

Run: `cargo build --workspace`
Expected: success.

If `ctx.agent_config.context_window_chars` does not exist as a field, either (a) add it as a new optional field on `AgentConfig` or (b) hard-code `200_000` for v1 and add a TODO comment marking the follow-up. Choose (a) if the change is small; otherwise (b).

- [ ] **Step 7: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-core/src/peer_mailbox/mod.rs crates/spur-core/src/peer_mailbox/router.rs
git commit -m "feat(spur-core): wire peer mailbox hooks into run_one_worker_attempt"
```

---

## Task 13: Forced-terminal-timeout drain in review handoff

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

Replace the bounded post-prompt ack drain (if it exists in any preliminary form) with a timeout-based drain that resets on each peer-ack notification.

- [ ] **Step 1: Locate the review-handoff code path**

Search `orchestrator.rs` for `ExecutorReviewRequested` emission. The drain inserts immediately before that emission inside `run_one_worker_attempt`.

- [ ] **Step 2: Add the drain helper**

Add a private helper near the bottom of `orchestrator.rs`:

```rust
/// Forced-terminal-timeout drain. Waits up to `quiet_window` for any further
/// peer-ack notifications scoped to `delegation_id`. Each ack resets the
/// window. After the window elapses, any non-terminal peer messages are
/// forced to `Ignored` with reason `drain_timeout`.
async fn drain_peer_acks_with_timeout(
    bundle: &crate::peer_mailbox::PeerMailboxBundle,
    delegation_id: &spur_acp::domain::delegation::DelegationId,
    quiet_window: std::time::Duration,
    mut ack_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
) {
    use spur_acp::domain::peer_message::{LedgerState, TerminalOutcome};

    loop {
        match tokio::time::timeout(quiet_window, ack_rx.recv()).await {
            Ok(Some(())) => continue, // ack observed, reset window
            Ok(None) | Err(_) => break, // sender dropped or window elapsed
        }
    }
    // Force any still-non-terminal entries for this delegation to Ignored.
    let pending = bundle.ledger.pending_for_target(delegation_id).await;
    for entry in pending {
        if matches!(
            entry.state,
            LedgerState::Delivered | LedgerState::DeliveredInflight
        ) {
            let _ = bundle
                .router
                .record_terminal(
                    &entry.envelope.message_id,
                    TerminalOutcome::Ignored {
                        reason: "drain_timeout".into(),
                    },
                )
                .await;
        }
    }
}
```

- [ ] **Step 3: Call the helper before review emission**

Insert immediately before `ExecutorReviewRequested` emission:

```rust
if let Some(bundle) = &self.peer_mailbox {
    let quiet = std::time::Duration::from_millis(
        bundle.router.limits().drain_quiet_window_ms,
    );
    // The ack_rx must be created and fed by a notification consumer; for v1
    // we use a freshly-constructed channel that receives `()` whenever a
    // `_spur/peer_message_consumed` or `_spur/peer_message_ignored` matching
    // this delegation arrives. Wire it where worker notifications are demuxed.
    let (ack_tx, ack_rx) = tokio::sync::mpsc::unbounded_channel();
    // ... wire ack_tx into the worker notification demux for this delegation ...
    drain_peer_acks_with_timeout(
        bundle,
        &spur_acp::domain::delegation::DelegationId(ctx.request_id.clone()),
        quiet,
        ack_rx,
    )
    .await;
}
```

- [ ] **Step 4: Add a unit test for the helper**

Add to the orchestrator's test module:

```rust
#[tokio::test]
async fn drain_completes_after_quiet_window_with_no_acks() {
    use crate::peer_mailbox::{ledger::InMemoryLedger, limits::Limits, router::PeerMailboxRouter};
    use std::sync::Arc;

    let ledger = Arc::new(InMemoryLedger::new());
    let funnel = crate::event_funnel::spawn_funnel(4096).0;
    let (recon_tx, _recon_rx) = tokio::sync::mpsc::unbounded_channel();
    let router = Arc::new(PeerMailboxRouter::new(
        ledger.clone(),
        funnel,
        recon_tx,
        Limits::default(),
        "bs".into(),
    ));
    let bundle = crate::peer_mailbox::PeerMailboxBundle {
        router,
        builder: Arc::new(crate::peer_mailbox::PeerPromptContextBuilder::new(ledger.clone())),
        ledger,
        plan_state: Arc::new(tokio::sync::Mutex::new(/* fixture or dummy */)),
    };
    let (_ack_tx, ack_rx) = tokio::sync::mpsc::unbounded_channel();
    let start = std::time::Instant::now();
    drain_peer_acks_with_timeout(
        &bundle,
        &spur_acp::domain::delegation::DelegationId("tgt".into()),
        std::time::Duration::from_millis(50),
        ack_rx,
    ).await;
    assert!(start.elapsed() >= std::time::Duration::from_millis(50));
    assert!(start.elapsed() < std::time::Duration::from_millis(500));
}
```

If the `plan_state` fixture is awkward to construct in a unit test, refactor `drain_peer_acks_with_timeout` to take only the parts it needs (`router`, `ledger`) and update the call site accordingly.

- [ ] **Step 5: Run tests**

Run: `cargo test -p spur-core drain_completes`
Expected: 1 test passes.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): add forced-terminal-timeout drain before review"
```

---

## Task 14: Startup reconciliation pass

**Files:**
- Create: `crates/spur-core/src/peer_mailbox/reconciler.rs`

- [ ] **Step 1: Write the file with failing tests**

Create `crates/spur-core/src/peer_mailbox/reconciler.rs`:

```rust
use crate::event_funnel::FunnelHandle;
use crate::peer_mailbox::ledger::PeerMailboxLedger;
use spur_acp::domain::peer_message::LedgerState;
use spur_acp::SpurEventBody;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Default)]
pub struct ReconcileCounts {
    pub audit_failed_emitted: u32,
    pub inflight_forced_to_delivered: u32,
    pub inflight_reverted_to_queued: u32,
    pub guards_re_wrapped: u32,
}

pub async fn run_startup_reconcile(
    ledger: Arc<dyn PeerMailboxLedger>,
    funnel: FunnelHandle,
    drain_quiet_window: Duration,
) -> ReconcileCounts {
    let entries = ledger.non_terminal_entries().await;
    let mut counts = ReconcileCounts::default();

    for entry in entries {
        match entry.state {
            LedgerState::DeliveredInflight => {
                // If injection record was written, force to Delivered; else revert to Queued.
                if !entry.injected_into_prompts.is_empty() {
                    let _ = ledger
                        .transition(&entry.envelope.message_id, LedgerState::Delivered)
                        .await;
                    counts.inflight_forced_to_delivered += 1;
                } else {
                    let _ = ledger
                        .transition(&entry.envelope.message_id, LedgerState::Queued)
                        .await;
                    counts.inflight_reverted_to_queued += 1;
                }
            }
            LedgerState::Accepted | LedgerState::Queued => {
                // Guards must be re-wrapped before any prompt dispatch.
                // The orchestrator constructs the guard when it loads the entry
                // pre-prompt; here we count the work that will be needed.
                counts.guards_re_wrapped += 1;
            }
            _ => {}
        }
        // For audit-failed detection: in v1 (no beads writes from this module yet),
        // skip. When beads writes are wired, compare ledger state against beads
        // labels and emit WorkerPeerMessageAuditFailed for missing references.
    }

    funnel.emit(SpurEventBody::WorkerPeerMailboxReconciled {
        audit_failed_emitted: counts.audit_failed_emitted,
        inflight_forced_to_delivered: counts.inflight_forced_to_delivered,
        inflight_reverted_to_queued: counts.inflight_reverted_to_queued,
        guards_re_wrapped: counts.guards_re_wrapped,
    });

    let _ = drain_quiet_window;
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_mailbox::ledger::InMemoryLedger;
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::domain::peer_message::{MessageKind, PeerMessageEnvelope, PeerMessageId};
    use uuid::Uuid;

    fn envelope() -> PeerMessageEnvelope {
        PeerMessageEnvelope {
            schema: "spur-peer-message/v1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            source_delegation_id: DelegationId("src".into()),
            target_delegation_id: DelegationId("tgt".into()),
            source_issue_id: "i1".into(),
            target_issue_id: "i2".into(),
            source_plan_task_id: "ta".into(),
            target_plan_task_id: "tb".into(),
            source_executor_id: "ex".into(),
            plan_version: 1,
            kind: MessageKind::Question,
            body: "hi".into(),
            sequence: 1,
        }
    }

    #[tokio::test]
    async fn reconcile_forces_inflight_with_injection_to_delivered() {
        let ledger = Arc::new(InMemoryLedger::new());
        let env = envelope();
        ledger.accept(env.clone()).await.unwrap();
        ledger.record_injection(&env.message_id, "p1").await.unwrap();
        ledger
            .transition(&env.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        let funnel = crate::event_funnel::spawn_funnel(4096).0;
        let counts = run_startup_reconcile(ledger.clone(), funnel, Duration::from_millis(100)).await;
        assert_eq!(counts.inflight_forced_to_delivered, 1);
        let entry = ledger.get(&env.message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Delivered);
    }

    #[tokio::test]
    async fn reconcile_reverts_inflight_without_injection_to_queued() {
        let ledger = Arc::new(InMemoryLedger::new());
        let env = envelope();
        ledger.accept(env.clone()).await.unwrap();
        // No injection record written.
        ledger
            .transition(&env.message_id, LedgerState::DeliveredInflight)
            .await
            .unwrap();
        let funnel = crate::event_funnel::spawn_funnel(4096).0;
        let counts = run_startup_reconcile(ledger.clone(), funnel, Duration::from_millis(100)).await;
        assert_eq!(counts.inflight_reverted_to_queued, 1);
        let entry = ledger.get(&env.message_id).await.unwrap();
        assert_eq!(entry.state, LedgerState::Queued);
    }
}
```

- [ ] **Step 2: Wire the reconcile call into orchestrator boot**

In `Orchestrator::start` (or wherever the runtime is bootstrapped), add a call to `run_startup_reconcile` BEFORE any `run_one_worker_attempt` is allowed to run. If the orchestrator already has a `pre_run` or `init` async path, insert there. Otherwise wrap the main loop's entry:

```rust
if let Some(bundle) = &self.peer_mailbox {
    let _counts = crate::peer_mailbox::reconciler::run_startup_reconcile(
        bundle.ledger.clone(),
        funnel.clone(),
        std::time::Duration::from_millis(bundle.router.limits().drain_quiet_window_ms),
    )
    .await;
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p spur-core peer_mailbox::reconciler`
Expected: 2 tests pass.

Run: `cargo build --workspace`
Expected: success.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/peer_mailbox/reconciler.rs crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): add startup reconciliation pass for peer mailbox"
```

---

## Task 15: Lineage projection — peer events as edges

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs`
- Modify: `crates/spur-core/src/lineage/types.rs`

- [ ] **Step 1: Add a `PeerEdge` type in `lineage/types.rs`**

```rust
use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{MessageKind, PeerMessageId};

#[derive(Debug, Clone)]
pub struct PeerEdge {
    pub message_id: PeerMessageId,
    pub source_delegation_id: DelegationId,
    pub target_delegation_id: DelegationId,
    pub kind: MessageKind,
    pub state: PeerEdgeState,
    pub injected_chars: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerEdgeState {
    Accepted,
    Delivered,
    Consumed,
    Ignored,
    Expired,
    Dropped,
    Undeliverable,
    Rejected,
}
```

Then, on the existing `ExecutorNode` struct (or `Attempt`, depending on layering), add:

```rust
pub peer_edges: Vec<PeerEdge>,
```

(Default to empty `Vec`. If `ExecutorNode` has a `Default` impl, this is automatic; otherwise add it to the constructor.)

- [ ] **Step 2: Write the failing test**

Add to a test module under `crates/spur-core/tests/` (or wherever `lineage` tests live):

```rust
#[test]
fn peer_message_accepted_creates_edge_on_source_node() {
    use spur_core::lineage::ExecutorLineage;
    use spur_acp::domain::peer_message::{MessageKind, PeerMessageId};
    use spur_acp::domain::delegation::DelegationId;
    use spur_acp::{SpurEvent, SpurEventBody};

    let mut lineage = ExecutorLineage::default();
    let event = SpurEvent {
        seq: 1,
        body: SpurEventBody::WorkerPeerMessageAccepted {
            brain_session_id: "bs".into(),
            message_id: PeerMessageId(uuid::Uuid::new_v4()),
            source_delegation_id: DelegationId("src".into()),
            target_delegation_id: DelegationId("tgt".into()),
            kind: MessageKind::Handoff,
            sequence: 1,
        },
        // ... other SpurEvent fields per existing test pattern
    };
    lineage.apply(&event);
    // Check that an edge was created.
    let edges = lineage.peer_edges_for_delegation(&DelegationId("src".into()));
    assert_eq!(edges.len(), 1);
}
```

- [ ] **Step 3: Add projection logic in `projection.rs::apply_inner`**

Inside the existing `match &event.body` arm in `apply_inner`, add:

```rust
SpurEventBody::WorkerPeerMessageAccepted {
    message_id,
    source_delegation_id,
    target_delegation_id,
    kind,
    ..
} => {
    let edge = crate::lineage::types::PeerEdge {
        message_id: message_id.clone(),
        source_delegation_id: source_delegation_id.clone(),
        target_delegation_id: target_delegation_id.clone(),
        kind: *kind,
        state: crate::lineage::types::PeerEdgeState::Accepted,
        injected_chars: 0,
    };
    self.attach_peer_edge(edge);
}
SpurEventBody::WorkerPeerMessageDelivered {
    message_id,
    target_delegation_id,
    injected_chars,
    ..
} => {
    self.update_peer_edge_state(
        message_id,
        crate::lineage::types::PeerEdgeState::Delivered,
        Some(*injected_chars),
    );
    let _ = target_delegation_id;
}
SpurEventBody::WorkerPeerMessageConsumed { message_id, .. } => {
    self.update_peer_edge_state(
        message_id,
        crate::lineage::types::PeerEdgeState::Consumed,
        None,
    );
}
SpurEventBody::WorkerPeerMessageIgnored { message_id, .. } => {
    self.update_peer_edge_state(
        message_id,
        crate::lineage::types::PeerEdgeState::Ignored,
        None,
    );
}
SpurEventBody::WorkerPeerMessageRejected { .. }
| SpurEventBody::WorkerPeerMessageExpired { .. }
| SpurEventBody::WorkerPeerMessageDropped { .. }
| SpurEventBody::WorkerPeerMessageUndeliverable { .. }
| SpurEventBody::WorkerPeerMessageQueued { .. }
| SpurEventBody::WorkerPeerMessageAuditFailed { .. }
| SpurEventBody::WorkerPeerMailboxReconciled { .. } => {
    // Lifecycle events that don't currently mutate the edge graph. Leave for
    // a follow-up plan to surface in TUI/lineage if needed.
}
```

Add helpers `attach_peer_edge` and `update_peer_edge_state` and a public reader `peer_edges_for_delegation` on `ExecutorLineage`:

```rust
impl ExecutorLineage {
    fn attach_peer_edge(&mut self, edge: crate::lineage::types::PeerEdge) {
        // Attach to the source executor's node.
        if let Some(node) = self.find_node_mut_by_delegation(&edge.source_delegation_id) {
            node.peer_edges.push(edge);
        }
    }

    fn update_peer_edge_state(
        &mut self,
        message_id: &spur_acp::domain::peer_message::PeerMessageId,
        new_state: crate::lineage::types::PeerEdgeState,
        new_injected_chars: Option<u32>,
    ) {
        for node in self.nodes.values_mut() {
            for edge in node.peer_edges.iter_mut() {
                if &edge.message_id == message_id {
                    edge.state = new_state;
                    if let Some(c) = new_injected_chars {
                        edge.injected_chars = c;
                    }
                    return;
                }
            }
        }
    }

    pub fn peer_edges_for_delegation(
        &self,
        delegation_id: &spur_acp::domain::delegation::DelegationId,
    ) -> Vec<crate::lineage::types::PeerEdge> {
        if let Some(node) = self.find_node_by_delegation(delegation_id) {
            node.peer_edges.clone()
        } else {
            Vec::new()
        }
    }
}
```

`find_node_mut_by_delegation` and `find_node_by_delegation` likely already exist or have close analogues. If not, add them as small helpers walking `self.nodes` for a node whose attempts include this delegation id.

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core peer_message_accepted_creates_edge`
Expected: test passes.

Run: `cargo test -p spur-core lineage`
Expected: all existing lineage tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/lineage/
git commit -m "feat(spur-core): project peer events as edges in lineage"
```

---

## Task 16: Review payload extension — peer summary

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs` (extend `ReviewPayload`)
- Modify: `crates/spur-core/src/orchestrator.rs` (populate the new field)

- [ ] **Step 1: Add `peer_influence` field to `ReviewPayload`**

In `crates/spur-acp/src/domain/events.rs`, find the `ReviewPayload` struct (around line 32-46 per earlier verification) and add:

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub peer_influence: Option<PeerInfluenceSummary>,
```

Then add the new struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PeerInfluenceSummary {
    pub inbound_consumed: u32,
    pub inbound_ignored: u32,
    pub outbound_emitted: u32,
    pub undelivered: u32,
    pub from_unreviewed_source: bool,
}
```

- [ ] **Step 2: Write the failing test**

Add to the `events.rs` test module:

```rust
#[test]
fn peer_influence_round_trips_through_serde() {
    let p = PeerInfluenceSummary {
        inbound_consumed: 2,
        inbound_ignored: 1,
        outbound_emitted: 0,
        undelivered: 0,
        from_unreviewed_source: false,
    };
    let json = serde_json::to_string(&p).unwrap();
    let back: PeerInfluenceSummary = serde_json::from_str(&json).unwrap();
    assert_eq!(p, back);
}

#[test]
fn review_payload_default_has_no_peer_influence() {
    let payload = ReviewPayload::default();
    assert!(payload.peer_influence.is_none());
}
```

- [ ] **Step 3: Populate the field in the orchestrator**

In `run_one_worker_attempt`, where `ReviewPayload` is constructed before `ExecutorReviewRequested` emission, populate `peer_influence` from the lineage's peer-edge view of the current delegation:

```rust
let peer_influence = if let Some(_bundle) = &self.peer_mailbox {
    use crate::lineage::types::PeerEdgeState;
    let target_delegation = spur_acp::domain::delegation::DelegationId(ctx.request_id.clone());
    let edges = self.lineage.peer_edges_for_delegation(&target_delegation);
    let mut s = spur_acp::PeerInfluenceSummary::default();
    for edge in edges {
        match edge.state {
            PeerEdgeState::Consumed => s.inbound_consumed += 1,
            PeerEdgeState::Ignored => s.inbound_ignored += 1,
            PeerEdgeState::Undeliverable | PeerEdgeState::Dropped | PeerEdgeState::Expired
            | PeerEdgeState::Rejected => s.undelivered += 1,
            _ => {}
        }
    }
    Some(s)
} else {
    None
};
let review_payload = ReviewPayload {
    // ... existing fields ...
    peer_influence,
    // ... existing fields ...
};
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-acp peer_influence`
Expected: 2 tests pass.

Run: `cargo build --workspace`
Expected: success.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-acp): add peer influence summary to ReviewPayload"
```

---

## Task 17: Feature flag — default-off

**Files:**
- Modify: `crates/spur-acp/src/config.rs` (or wherever `SpurConfig` lives)
- Modify: `crates/spur-core/src/orchestrator.rs` (gate bundle construction on flag)

- [ ] **Step 1: Add the flag**

In `SpurConfig` (verify location with `rg "struct SpurConfig"`), add:

```rust
#[serde(default)]
pub peer_mailbox_enabled: bool,
```

The `Default` impl for `SpurConfig` should produce `peer_mailbox_enabled: false`.

- [ ] **Step 2: Gate the bundle**

In `Orchestrator::new` (or wherever the orchestrator is constructed), build the `PeerMailboxBundle` only when the flag is on:

```rust
let peer_mailbox = if config.peer_mailbox_enabled {
    Some(crate::peer_mailbox::PeerMailboxBundle::new(/* ... */))
} else {
    None
};
```

- [ ] **Step 3: Write the test**

```rust
#[tokio::test]
async fn orchestrator_without_peer_flag_has_no_peer_bundle() {
    let mut config = SpurConfig::default();
    assert!(!config.peer_mailbox_enabled);
    let orch = Orchestrator::new(config /*, ... other fixtures */).await.unwrap();
    assert!(orch.peer_mailbox.is_none());
}
```

(Adjust `Orchestrator::new` invocation to match its actual signature.)

- [ ] **Step 4: Run tests**

Run: `cargo test -p spur-core orchestrator_without_peer_flag`
Expected: 1 test passes.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/config.rs crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur): add peer_mailbox_enabled feature flag (default off)"
```

---

## Task 18: End-to-end integration test

**Files:**
- Create: `crates/spur-core/tests/peer_mailbox_e2e.rs`

This test exercises the full Stage 1 flow without a real worker: simulate the ACP notification handoff, verify the ledger transitions, the events on the funnel, and the prompt-context built for the target.

- [ ] **Step 1: Write the integration test**

Create `crates/spur-core/tests/peer_mailbox_e2e.rs`:

```rust
use spur_acp::domain::delegation::DelegationId;
use spur_acp::domain::peer_message::{
    LedgerState, MessageKind, PeerMessageEnvelope, PeerMessageId, TerminalOutcome,
};
use spur_acp::SpurEventBody;
use spur_core::peer_mailbox::{
    InMemoryLedger, Limits, PeerMailboxRouter, PeerPromptContextBuilder,
};
use spur_mcp::plan::scope_snapshot::PlanScopeSnapshot;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::mpsc::unbounded_channel;
use uuid::Uuid;

fn snapshot() -> PlanScopeSnapshot {
    let mut delegation_to_task = HashMap::new();
    delegation_to_task.insert(DelegationId("src".into()), "ta".into());
    delegation_to_task.insert(DelegationId("tgt".into()), "tb".into());
    let mut peer_edges = HashSet::new();
    peer_edges.insert(("ta".into(), "tb".into()));
    PlanScopeSnapshot {
        plan_version: 1,
        peer_edges,
        delegation_to_task,
        delegation_to_issue: HashMap::new(),
        superseded_tasks: HashSet::new(),
        terminal_tasks: HashSet::new(),
    }
}

fn envelope(body: &str) -> PeerMessageEnvelope {
    PeerMessageEnvelope {
        schema: "spur-peer-message/v1".into(),
        message_id: PeerMessageId(Uuid::new_v4()),
        source_delegation_id: DelegationId("src".into()),
        target_delegation_id: DelegationId("tgt".into()),
        source_issue_id: "i1".into(),
        target_issue_id: "i2".into(),
        source_plan_task_id: "ta".into(),
        target_plan_task_id: "tb".into(),
        source_executor_id: "ex".into(),
        plan_version: 1,
        kind: MessageKind::Handoff,
        body: body.into(),
        sequence: 1,
    }
}

#[tokio::test]
async fn full_stage1_flow_accept_inject_consume() {
    // Setup
    let ledger = Arc::new(InMemoryLedger::new());
    let (funnel, mut bcast_rx) = spur_core::event_funnel::spawn_funnel(4096);
    let (recon_tx, _recon_rx) = unbounded_channel();
    let router = Arc::new(PeerMailboxRouter::new(
        ledger.clone(),
        funnel,
        recon_tx,
        Limits::default(),
        "bs".into(),
    ));
    let builder = PeerPromptContextBuilder::new(ledger.clone());
    let snap = snapshot();

    // 1. Worker A emits a handoff.
    let env = envelope("Worker B: please handle config validation");
    let mid = env.message_id.clone();
    let guard = router.accept_or_reject(env, &snap).await.unwrap();
    assert_eq!(ledger.get(&mid).await.unwrap().state, LedgerState::Accepted);

    // 2. Orchestrator builds prompt context for target B.
    let ctx = builder
        .build_for_target(&DelegationId("tgt".into()), 200_000, 8, 2_048)
        .await;
    assert_eq!(ctx.injection_records.len(), 1);
    assert!(ctx.orchestrator_authored_text.contains("config validation"));

    // 3. Orchestrator records injection (idempotent).
    let first = ledger.record_injection(&mid, &ctx.target_prompt_id).await.unwrap();
    let second = ledger.record_injection(&mid, &ctx.target_prompt_id).await.unwrap();
    assert!(first);
    assert!(!second);

    // 4. Post-dispatch transition to Delivered.
    ledger.transition(&mid, LedgerState::DeliveredInflight).await.unwrap();
    ledger.transition(&mid, LedgerState::Delivered).await.unwrap();

    // 5. Worker B emits consumed.
    router.record_terminal(&mid, TerminalOutcome::Consumed).await.unwrap();
    assert_eq!(ledger.get(&mid).await.unwrap().state, LedgerState::Consumed);

    // 6. Finalize the guard so it doesn't strand.
    guard.finalize(spur_core::peer_mailbox::guard::GuardOutcome::Terminal(
        TerminalOutcome::Consumed,
    )).await;

    // 7. Verify the broadcast saw the expected lifecycle.
    let mut seen = Vec::new();
    while let Ok(event) = bcast_rx.try_recv() {
        seen.push(format!("{:?}", event.body));
    }
    assert!(seen.iter().any(|s| s.contains("WorkerPeerMessageAccepted")));
    assert!(seen.iter().any(|s| s.contains("WorkerPeerMessageConsumed")));
}

#[tokio::test]
async fn rejected_message_is_not_in_pending() {
    let ledger = Arc::new(InMemoryLedger::new());
    let (funnel, _bcast_rx) = spur_core::event_funnel::spawn_funnel(4096);
    let (recon_tx, _recon_rx) = unbounded_channel();
    let router = Arc::new(PeerMailboxRouter::new(
        ledger.clone(),
        funnel,
        recon_tx,
        Limits::default(),
        "bs".into(),
    ));
    let mut snap = snapshot();
    snap.peer_edges.clear(); // Force NotInDag rejection.
    let env = envelope("blocked");
    let _ = router.accept_or_reject(env, &snap).await.unwrap_err();
    let pending = ledger
        .pending_for_target(&DelegationId("tgt".into()))
        .await;
    assert!(pending.is_empty());
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p spur-core --test peer_mailbox_e2e`
Expected: 2 tests pass.

- [ ] **Step 3: Run the full workspace test suite to catch regressions**

Run: `cargo test --workspace`
Expected: all tests pass. Investigate any regressions before proceeding.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/tests/peer_mailbox_e2e.rs
git commit -m "test(spur-core): add peer mailbox stage 1 end-to-end integration"
```

---

## Self-Review Checklist (run after all tasks complete)

**1. Spec coverage:**

| Spec section | Implemented in task |
|---|---|
| Component Boundaries (envelope, ledger, router, builder, scope snapshot, guard, prompt builder) | Tasks 1, 5, 6, 7, 8, 11 |
| Stage 1 Orchestrator Integration (pre-prompt hook, post-dispatch transition, scope snapshot, recovery) | Task 12, 14 |
| Peer Message Envelope (kinds, sequence, plan_version, no causal_parent_id) | Task 1 |
| Validation (DAG, supersession, sequence, body size, plan version) | Tasks 5, 8 |
| Delivery Guarantees (at-least-once + at-most-once injection, Delivered_inflight) | Tasks 6, 11, 12 |
| Durable State (ledger states, label format, idempotent transitions) | Tasks 1, 4, 6 |
| Events (10 lifecycle + 1 reconciled + replay forward-compat) | Tasks 2, 3, 14 |
| Review Behavior (forced-terminal-timeout drain, late acks idempotent) | Task 13 |
| Cost Behavior (injected_chars on Delivered) | Tasks 2, 12 |
| Safety Limits (tiered context-window bound, feature flag) | Tasks 9, 17 |
| Lineage projection (peer edges) | Task 15 |
| Review payload (peer_influence summary) | Task 16 |
| Acceptance Criteria | Verified by Task 18 e2e + per-task unit tests |

**2. Placeholder scan:** No "TODO", "TBD", or "fill in" outside the explicit step bodies. Two places where the plan defers to existing patterns ("follow whatever pattern the orchestrator uses" in Tasks 5 and 12, Step 6) are acceptable because the surrounding code lives in the same file the engineer is already modifying.

**3. Type consistency:** `target_prompt_id: String` everywhere; `PeerMessageId(Uuid)` everywhere; `LedgerState::DeliveredInflight` (snake_case = `"delivered_inflight"`) consistent across Tasks 1, 6, 14, 18.

---

**Plan complete and saved to `docs/superpowers/plans/2026-04-25-worker-peer-mailbox-stage1.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**

# Lineage Event-Contract Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harden the `SpurEvent` contract — introduce an envelope carrying `occurred_at`, replace stringly-typed wire fields with typed enums, collapse duplicate wire/state types, and fix the `SystemTime::now()`-in-`apply()` invariant violation.

**Architecture:** `SpurEvent` becomes `struct SpurEvent { occurred_at: SystemTime, body: SpurEventBody }`. `LifecycleState` and `Role` move from `spur-core` to `spur-acp`; 5 duplicate wire/state type pairs collapse into single canonical definitions in `spur-acp`; `spur-core` re-exports. The projection's `apply` is split into `apply` (calls `apply_legacy` then `apply_inner`) and `apply_inner` (body-only, orphan-drain safe). `Attempt.started_at` is set from `event.occurred_at`, never from `SystemTime::now()`.

**Tech Stack:** Rust 2021 · serde · `spur-acp` / `spur-core` / `spur-tui` / `spur-cli` crates. 115 `SpurEvent::` call sites across 17 files will be touched by the envelope refactor.

**Spec:** `docs/superpowers/specs/2026-04-13-lineage-event-contract-hardening-design.md`

---

## File Structure

**Files modified (schema + projection):**
- `crates/spur-acp/src/domain/events.rs` — envelope struct, body enum rename, typed `phase`/`role` fields, new canonical enums (`LifecycleState`, `Role`), rename `Executor*Payload` → shorter canonical names
- `crates/spur-acp/src/lib.rs` — re-exports (drop `Executor*Payload` prefixed names, add `LifecycleState`, `Role`)
- `crates/spur-acp/src/domain/mod.rs` — possibly re-exports
- `crates/spur-core/src/lineage/types.rs` — delete 5 duplicate types (`ReviewKind`, `ReviewPayload`, `ReviewDecision`, `Artifact`, `DiffSummary`), remove `LifecycleState` + `Role` (moved); keep `Attempt`, `AttemptStatus`, `ExecutorId`, `ExecutorNode`, `ReviewRequest`
- `crates/spur-core/src/lineage/projection.rs` — split `apply` / `apply_inner`; delete 5 mapper fns; wire timestamps from envelope; add parent-orphan buffer; add `attempt_n` validation warn
- `crates/spur-core/src/lineage/adapter.rs` — signature takes `&SpurEvent`; matches `&event.body`; uses `event.occurred_at` for synthesized `Attempt.started_at`
- `crates/spur-core/src/lib.rs` — re-export wire types

**Files modified (consumers — envelope destructuring):**
- `crates/spur-core/src/orchestrator.rs` — every `SpurEvent::X{…}` construct → envelope; every match on `SpurEvent` → match on `&event.body`
- `crates/spur-tui/src/app.rs` — same
- `crates/spur-tui/src/views/dashboard.rs` — same
- `crates/spur-tui/src/views/session_detail.rs` — same
- `crates/spur-tui/src/views/session_picker.rs` — same (quick check — probably only uses `SessionsListed`)
- `crates/spur-tui/src/views/mod.rs` — trait signature may need updating if `View::handle_spur_event(&SpurEvent)` matches on structure

**Files modified (test fixtures — all use envelope):**
- `crates/spur-acp/tests/executor_events_roundtrip.rs`
- `crates/spur-core/tests/lineage_projection.rs`
- `crates/spur-core/tests/lineage_integration.rs`
- `crates/spur-tui/tests/state_machine_regressions.rs`
- `crates/spur-tui/tests/agents_tree_snapshot.rs`

**Files left untouched:** `spur-mcp`, `spur-worktree`, `spur-cost`, `spur-pm`. No `SpurEvent` usage in those.

---

## Task 1: Envelope refactor (atomic, single commit)

**Goal:** `SpurEvent` becomes an envelope; existing enum renames to `SpurEventBody`. Every emitter constructs the envelope; every matcher destructures `&event.body`. Keep stringly-typed fields (typed fields come in Task 2). Keep duplicate types (collapse comes in Task 3).

This task is mechanical but wide. Work through it in steps, compile after each batch, commit at the end.

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Modify: `crates/spur-acp/src/lib.rs`
- Modify: 15 other files that reference `SpurEvent` (see File Structure above)

- [ ] **Step 1: Restructure `SpurEvent` in `crates/spur-acp/src/domain/events.rs`**

Replace the current `pub enum SpurEvent { … }` with this exact shape. Keep all existing variants unchanged — just move them under `SpurEventBody`:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use agent_client_protocol::{SessionInfo, SessionNotification};
use crate::types::SessionId;
use crate::domain::delegation::DelegationStatus;

/// Envelope wrapping every `SpurEventBody` with a wall-clock timestamp.
///
/// `occurred_at` is captured at emission time (the orchestrator / connection
/// layer that produces the event). Consumers must **not** call
/// `SystemTime::now()` when applying the event — they must read
/// `event.occurred_at`. This preserves the replay-purity invariant of
/// `ExecutorLineage`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpurEvent {
    pub occurred_at: SystemTime,
    pub body: SpurEventBody,
}

impl SpurEvent {
    /// Convenience constructor using `SystemTime::now()`. Use at emission
    /// sites. Do NOT use inside `apply` / projection code — timestamps
    /// there must come from the arriving event, not re-captured.
    pub fn now(body: SpurEventBody) -> Self {
        Self { occurred_at: SystemTime::now(), body }
    }
}

/// Event body — payload after envelope unwrap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpurEventBody {
    BrainSpawned { agent: String, session: SessionId },
    WorkerSpawned { agent: String, session: SessionId, worktree: PathBuf },
    SessionCompleted { session: SessionId, success: bool },
    AgentNotification { session: SessionId, notification: SessionNotification },
    DelegationRequested { from: SessionId, to_agent: String, task: String },
    DelegationCompleted { worker_session: SessionId, status: DelegationStatus },
    ConflictDetected { files: Vec<PathBuf> },
    RateLimitDetected { agent: String, retry_after: Option<Duration> },
    BrainFailover { from: String, to: String },
    CostUpdate { session: SessionId, agent: String, estimated_cost_usd: f64 },
    IssueReceived { source: String, id: String },
    PrCreated { url: String },
    IssueUpdated { source: String, id: String, status: String },
    TurnComplete { session: SessionId },
    BrainError { session: SessionId, message: String },
    SessionsListed { agent: String, sessions: Vec<SessionInfo> },
    SessionsListError { message: String },
    SessionHistory { session: SessionId, entries: Vec<HistoryEntry> },
    // ── Executor lineage events ────────────────────────────────────
    ExecutorSpawned {
        id: String,
        parent_id: Option<String>,
        session_id: SessionId,
        agent: String,
        role: String,          // still stringly in Task 1; typed in Task 2
        task_spec: String,
    },
    ExecutorPhaseChanged {
        id: String,
        phase: String,         // still stringly in Task 1
    },
    ExecutorArtifact {
        id: String,
        artifact: ExecutorArtifactPayload,
    },
    ExecutorReviewRequested {
        id: String,
        kind: ExecutorReviewKind,
        payload: ExecutorReviewPayload,
        // requested_at removed — envelope carries it
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
}

// HistoryEntry, ExecutorReviewKind, ExecutorReviewPayload, ExecutorDiffSummary,
// ExecutorArtifactPayload, ExecutorReviewDecision definitions stay unchanged
// below this point.
```

Note: `ExecutorReviewRequested` drops its `requested_at` field. The envelope carries it now.

- [ ] **Step 2: Update `crates/spur-acp/src/lib.rs` re-exports**

Make sure `SpurEvent` AND `SpurEventBody` are both re-exported. Also keep the existing executor-lineage-supporting type re-exports. Find the existing `pub use crate::domain::events::{…, SpurEvent, …};` and adjust to include `SpurEventBody`:

```rust
pub use crate::domain::events::{
    ExecutorArtifactPayload, ExecutorDiffSummary, ExecutorReviewDecision, ExecutorReviewKind,
    ExecutorReviewPayload, HistoryEntry, SpurEvent, SpurEventBody,
};
```

- [ ] **Step 3: Update every construction site — pattern `SpurEvent::X{…}` → `SpurEvent::now(SpurEventBody::X{…})`**

Search and replace across production code. The exact grep: `grep -rn "SpurEvent::" crates/*/src --include="*.rs"`.

Work file-by-file. Production files to update (tests are step 6):

- `crates/spur-core/src/orchestrator.rs`
- `crates/spur-core/src/lineage/projection.rs` (emits nothing but may reference variants in comments — skip if so)
- `crates/spur-core/src/lineage/adapter.rs` (reads only — skip for construction)
- `crates/spur-tui/src/app.rs` — check if it ever constructs a SpurEvent (optimistic apply does — `SpurEvent::ExecutorReviewResolved`). Find this line in the `Action::SubmitReview` arm and change:

```rust
// Before:
self.lineage.apply(&spur_acp::SpurEvent::ExecutorReviewResolved { id, decision });

// After:
self.lineage.apply(&spur_acp::SpurEvent::now(
    spur_acp::SpurEventBody::ExecutorReviewResolved { id, decision },
));
```

Rule of thumb: anywhere you see `SpurEvent::<VariantName>{…}` — wrap it.

- [ ] **Step 4: Update every match site — `match event { SpurEvent::X … }` → `match &event.body { SpurEventBody::X … }`**

Search: `grep -rn "SpurEvent::" crates/*/src --include="*.rs"` AFTER step 3 completes. Remaining hits are match arms.

For each match block, change the scrutinee from `event` (or `&event`) to `&event.body` (or `&ev.body`), and the variant patterns from `SpurEvent::X` to `SpurEventBody::X`. Example:

```rust
// Before:
match &event {
    SpurEvent::BrainSpawned { agent, session } => { … }
    SpurEvent::ExecutorSpawned { id, parent_id, session_id, agent, role, task_spec } => { … }
    _ => {}
}

// After:
match &event.body {
    SpurEventBody::BrainSpawned { agent, session } => { … }
    SpurEventBody::ExecutorSpawned { id, parent_id, session_id, agent, role, task_spec } => { … }
    _ => {}
}
```

If a site needs the envelope's `occurred_at` (for example, when synthesizing an `Attempt.started_at` from a legacy event — but that's Task 4, leave it alone for now), remember the envelope is still in scope as `event`.

Key files to touch:
- `crates/spur-core/src/lineage/projection.rs` — `apply` function
- `crates/spur-core/src/lineage/adapter.rs` — `apply_legacy` — signature changes to `pub fn apply_legacy(lineage: &mut ExecutorLineage, event: &SpurEvent)` and match body is `&event.body`
- `crates/spur-core/src/orchestrator.rs` — any match on events
- `crates/spur-tui/src/app.rs` — `handle_spur_event` (outer routing + inner match)
- `crates/spur-tui/src/views/dashboard.rs` — `handle_spur_event`
- `crates/spur-tui/src/views/session_detail.rs` — `handle_spur_event`
- `crates/spur-tui/src/views/session_picker.rs` — `handle_spur_event`

For tui view traits: the `View::handle_spur_event(&SpurEvent)` signature does NOT change (`&SpurEvent` still refers to the envelope); only the match body inside each impl changes.

- [ ] **Step 5: Remove `requested_at` usage on `ExecutorReviewRequested`**

Now that the envelope carries `occurred_at`, the `ExecutorReviewRequested { requested_at, … }` field is gone. Find every construction and match site for `ExecutorReviewRequested` and drop the `requested_at` field.

```
grep -rn "ExecutorReviewRequested" crates
```

Two production sites likely:
- `crates/spur-core/src/lineage/projection.rs` — in the match arm, drop the `requested_at,` destructure and don't reference it. Inside the arm, replace `*requested_at` with `event.occurred_at` (projection has the envelope in scope as `event`). Actually — this is the Task 4 job (see below). For Task 1, just do: `ReviewRequest { kind: …, payload: …, requested_at: SystemTime::now() }` temporarily. Leave the comment `// TODO Task 4: use event.occurred_at`.
- Tests (Task 1 Step 6).

- [ ] **Step 6: Update all test fixtures**

Files to touch:
- `crates/spur-acp/tests/executor_events_roundtrip.rs`
- `crates/spur-core/tests/lineage_projection.rs`
- `crates/spur-core/tests/lineage_integration.rs`
- `crates/spur-tui/tests/state_machine_regressions.rs`
- `crates/spur-tui/tests/agents_tree_snapshot.rs`

In every test fixture, change constructors from `SpurEvent::X{…}` to `SpurEvent::now(SpurEventBody::X{…})`. Drop `requested_at` from `ExecutorReviewRequested` construction.

Example:

```rust
// Before:
let ev = SpurEvent::ExecutorSpawned { … };
let ev2 = SpurEvent::ExecutorReviewRequested {
    id: "w".into(),
    kind: ExecutorReviewKind::Completion,
    payload: ExecutorReviewPayload { … },
    requested_at: SystemTime::now(),
};

// After:
let ev = SpurEvent::now(SpurEventBody::ExecutorSpawned { … });
let ev2 = SpurEvent::now(SpurEventBody::ExecutorReviewRequested {
    id: "w".into(),
    kind: ExecutorReviewKind::Completion,
    payload: ExecutorReviewPayload { … },
});
```

Update match assertions in `executor_events_roundtrip.rs`:

```rust
// Before:
assert!(matches!(round, SpurEvent::ExecutorSpawned { .. }));

// After:
assert!(matches!(round.body, SpurEventBody::ExecutorSpawned { .. }));
```

- [ ] **Step 7: Verify**

Run: `cargo check --workspace`
Expected: PASS (0 errors; warnings about dead mapper functions are fine — we delete them in Tasks 2–3).

Run: `cargo test --workspace`
Expected: PASS with current test count (49 tests as of commit 68b23a6).

- [ ] **Step 8: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs \
        crates/spur-acp/src/lib.rs \
        crates/spur-core/src/lineage/projection.rs \
        crates/spur-core/src/lineage/adapter.rs \
        crates/spur-core/src/orchestrator.rs \
        crates/spur-tui/src/app.rs \
        crates/spur-tui/src/views/dashboard.rs \
        crates/spur-tui/src/views/session_detail.rs \
        crates/spur-tui/src/views/session_picker.rs \
        crates/spur-tui/src/views/mod.rs \
        crates/spur-acp/tests/executor_events_roundtrip.rs \
        crates/spur-core/tests/lineage_projection.rs \
        crates/spur-core/tests/lineage_integration.rs \
        crates/spur-tui/tests/state_machine_regressions.rs \
        crates/spur-tui/tests/agents_tree_snapshot.rs
git commit -m "refactor(spur-acp): SpurEvent envelope with occurred_at timestamp

SpurEvent becomes a struct wrapping a renamed SpurEventBody enum. Every
event now carries the wall-clock timestamp at which it occurred, enabling
replay-pure projection application. ExecutorReviewRequested drops its
redundant requested_at field (envelope carries it).

This is the first of 7 stages in the lineage event-contract hardening
spec. Subsequent stages replace stringly-typed fields with typed enums,
collapse duplicate wire/state types, and wire Attempt timestamps from
the envelope instead of SystemTime::now()."
```

---

## Task 2: Move `LifecycleState`, `Role` to spur-acp; typed event fields

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Modify: `crates/spur-acp/src/lib.rs`
- Modify: `crates/spur-core/src/lineage/types.rs`
- Modify: `crates/spur-core/src/lib.rs`
- Modify: `crates/spur-core/src/lineage/projection.rs`

- [ ] **Step 1: Write a failing deserialize-rejection test**

Add to `crates/spur-acp/tests/executor_events_roundtrip.rs`:

```rust
#[test]
fn executor_phase_changed_rejects_invalid_variant() {
    // Invalid phase name — should fail to deserialize, not silently accept.
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"ExecutorPhaseChanged": {"id": "x", "phase": "running"}}
    }"#;
    let result: Result<SpurEvent, _> = serde_json::from_str(json);
    assert!(result.is_err(), "lowercase 'running' must fail to deserialize");
}

#[test]
fn executor_spawned_rejects_invalid_role() {
    let json = r#"{
        "occurred_at": {"secs_since_epoch": 1000, "nanos_since_epoch": 0},
        "body": {"ExecutorSpawned": {
            "id": "x", "parent_id": null,
            "session_id": "s",
            "agent": "a", "role": "brain", "task_spec": ""
        }}
    }"#;
    let result: Result<SpurEvent, _> = serde_json::from_str(json);
    assert!(result.is_err(), "lowercase 'brain' must fail to deserialize");
}
```

Run: `cargo test -p spur-acp --test executor_events_roundtrip`
Expected: the two new tests FAIL (they currently pass because fields are `String`).

- [ ] **Step 2: Add `LifecycleState` and `Role` to `crates/spur-acp/src/domain/events.rs`**

After the other supporting types (`ExecutorReviewKind`, etc.) and before `pub struct SpurEvent`, add:

```rust
/// Lifecycle state of an executor node in the lineage.
///
/// This is the canonical wire-contract enum. `spur-core` re-exports it.
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

/// Role of an executor node in the lineage tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Brain,
    Executor,
    SubExecutor,
}
```

- [ ] **Step 3: Change `SpurEventBody` field types**

In `SpurEventBody::ExecutorPhaseChanged`:

```rust
// Before:
ExecutorPhaseChanged { id: String, phase: String },

// After:
ExecutorPhaseChanged { id: String, phase: LifecycleState },
```

In `SpurEventBody::ExecutorSpawned`:

```rust
// Before:
ExecutorSpawned { …, role: String, … },

// After:
ExecutorSpawned { …, role: Role, … },
```

- [ ] **Step 4: Re-export from `crates/spur-acp/src/lib.rs`**

Add `LifecycleState` and `Role` to the existing re-export block:

```rust
pub use crate::domain::events::{
    ExecutorArtifactPayload, ExecutorDiffSummary, ExecutorReviewDecision, ExecutorReviewKind,
    ExecutorReviewPayload, HistoryEntry, LifecycleState, Role, SpurEvent, SpurEventBody,
};
```

- [ ] **Step 5: Remove `LifecycleState` and `Role` from `spur-core`**

In `crates/spur-core/src/lineage/types.rs`, delete:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Brain,
    Executor,
    SubExecutor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleState { … }
```

And change uses of these in the same file (e.g. on `ExecutorNode.phase`, `ExecutorNode.role`) to point at the `spur_acp` versions:

```rust
use spur_acp::{LifecycleState, Role};
```

- [ ] **Step 6: Re-export from `spur-core` for consumers**

In `crates/spur-core/src/lib.rs`, modify the existing `pub use lineage::{…}` line so that `LifecycleState` and `Role` come from `spur_acp` (either via a `pub use spur_acp::{LifecycleState, Role};` line or removing them from the lineage re-export and adding a parallel re-export from `spur_acp`):

```rust
pub use spur_acp::{LifecycleState, Role};
pub use lineage::{
    Artifact, Attempt, AttemptStatus, ExecutorId, ExecutorLineage, ExecutorNode,
    ReviewDecision, ReviewKind, ReviewPayload, ReviewRequest,
};
```

(Artifact/ReviewKind/etc. get collapsed in Task 3; that's fine.)

- [ ] **Step 7: Delete `parse_phase` and `parse_role` in `projection.rs`**

They're no longer needed — fields are typed. Find:

```rust
fn parse_role(s: &str) -> Role { … }
fn parse_phase(s: &str) -> Option<LifecycleState> { … }
```

Delete both.

Update the `apply` (or `apply_inner`, depending on Task 5 order — if not yet split, still `apply`) body. Find:

```rust
SpurEventBody::ExecutorSpawned { …, role, … } => {
    let parsed_role = parse_role(role);
    …
    role: parsed_role,
}
```

Change to:

```rust
SpurEventBody::ExecutorSpawned { …, role, … } => {
    …
    role: *role,
}
```

Similarly for `ExecutorPhaseChanged`:

```rust
// Before:
SpurEventBody::ExecutorPhaseChanged { id, phase } => {
    let eid = ExecutorId::new(id);
    if let Some(new_phase) = parse_phase(phase) {
        if let Some(node) = self.nodes.get_mut(&eid) { … }
    }
}

// After:
SpurEventBody::ExecutorPhaseChanged { id, phase } => {
    let eid = ExecutorId::new(id);
    if let Some(node) = self.nodes.get_mut(&eid) {
        node.phase = *phase;
        if let Some(status) = terminal_attempt_status(*phase) { … }
    } else {
        self.buffer_orphan(eid, event.clone());
    }
}
```

`terminal_attempt_status` keeps its existing signature; it already takes `LifecycleState`.

Important: the `unknown_phase_string_is_ignored` test in `lineage_projection.rs` no longer makes sense — the field is typed, so an unknown string can't be constructed. DELETE that test.

- [ ] **Step 8: Update test fixtures that build typed events**

Find every test that constructs `ExecutorPhaseChanged { phase: "…" }` or `ExecutorSpawned { role: "…" }` and change to typed enums.

Example in `lineage_projection.rs`:

```rust
// Before:
SpurEvent::now(SpurEventBody::ExecutorPhaseChanged { id: "w".into(), phase: "Running".into() })

// After:
SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
    id: "w".into(),
    phase: LifecycleState::Running,
})
```

Similarly for the `spawn` helper in `lineage_projection.rs`:

```rust
// Before:
role: if parent.is_none() { "Brain".into() } else { "Executor".into() },

// After:
role: if parent.is_none() { Role::Brain } else { Role::Executor },
```

Import `LifecycleState` and `Role` at the top of each test file — they come from `spur_acp` or `spur_core` (either works; pick `spur_core` if the file already imports other types from there).

- [ ] **Step 9: Verify**

Run: `cargo check --workspace`
Expected: PASS.

Run: `cargo test --workspace`
Expected: 49 − 1 (deleted `unknown_phase_string_is_ignored`) + 2 (new rejection tests) = 50 tests passing.

- [ ] **Step 10: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs \
        crates/spur-acp/src/lib.rs \
        crates/spur-acp/tests/executor_events_roundtrip.rs \
        crates/spur-core/src/lineage/types.rs \
        crates/spur-core/src/lineage/projection.rs \
        crates/spur-core/src/lib.rs \
        crates/spur-core/tests/lineage_projection.rs \
        crates/spur-core/tests/lineage_integration.rs \
        crates/spur-tui/tests/state_machine_regressions.rs \
        crates/spur-tui/tests/agents_tree_snapshot.rs
git commit -m "refactor(spur-lineage): typed LifecycleState + Role in events

Move LifecycleState and Role from spur-core to spur-acp (the wire crate)
and change ExecutorPhaseChanged.phase / ExecutorSpawned.role from String
to the typed enums. Orchestrator typos that previously silently no-op'd
now fail loud at serde deserialize.

Delete parse_phase and parse_role — no longer needed with typed fields.
Add two regression tests verifying that invalid JSON variant names
deserialize to Err."
```

---

## Task 3: Collapse 5 duplicate wire/state type pairs

**Context:** these duplicate pairs exist today:

| Projection side (spur-core::lineage::types) | Wire side (spur-acp) |
|---|---|
| `Artifact` | `ExecutorArtifactPayload` |
| `DiffSummary` | `ExecutorDiffSummary` |
| `ReviewKind` | `ExecutorReviewKind` |
| `ReviewPayload` | `ExecutorReviewPayload` |
| `ReviewDecision` | `ExecutorReviewDecision` |

Each pair has a mapper function in `projection.rs`: `map_artifact`, `map_review_kind`, `map_review_payload`. (There were 5 mappers total; Task 2 deleted 2.)

We collapse by:
- renaming the `spur-acp` side to drop the `Executor*` prefix (canonical name)
- deleting the `spur-core` side
- deleting the mapper functions
- updating `ExecutorNode`, `Attempt`, `ReviewRequest` to use the canonical types directly

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Modify: `crates/spur-acp/src/lib.rs`
- Modify: `crates/spur-core/src/lineage/types.rs`
- Modify: `crates/spur-core/src/lineage/projection.rs`
- Modify: `crates/spur-core/src/lib.rs`
- Modify: test fixtures

- [ ] **Step 1: Rename wire types in `events.rs`**

Find the definitions:

```rust
pub enum ExecutorReviewKind { … }
pub struct ExecutorReviewPayload { … }
pub struct ExecutorDiffSummary { … }
pub enum ExecutorArtifactPayload { … }
pub enum ExecutorReviewDecision { … }
```

Rename to:

```rust
pub enum ReviewKind { … }
pub struct ReviewPayload { … }
pub struct DiffSummary { … }
pub enum Artifact { … }
pub enum ReviewDecision { … }
```

The fields inside them keep their current shapes (inside references to e.g. `ExecutorDiffSummary` update to `DiffSummary`).

- [ ] **Step 2: Update `SpurEventBody` variants referencing these types**

`ExecutorArtifact { id: String, artifact: ExecutorArtifactPayload }` → `… artifact: Artifact`.
`ExecutorReviewRequested { kind: ExecutorReviewKind, payload: ExecutorReviewPayload, … }` → `kind: ReviewKind, payload: ReviewPayload`.
`ExecutorReviewResolved { decision: ExecutorReviewDecision, … }` → `decision: ReviewDecision`.

Inside `ReviewPayload`, change `diff_summary: Option<ExecutorDiffSummary>` → `Option<DiffSummary>`.

Inside `Artifact::Diff(ExecutorDiffSummary)` → `Artifact::Diff(DiffSummary)`.

- [ ] **Step 3: Update `spur-acp` re-exports in `lib.rs`**

Drop the `Executor*Payload`-prefixed names, export the new canonical names:

```rust
pub use crate::domain::events::{
    Artifact, DiffSummary, HistoryEntry, LifecycleState, ReviewDecision, ReviewKind,
    ReviewPayload, Role, SpurEvent, SpurEventBody,
};
```

- [ ] **Step 4: Delete duplicate types in `crates/spur-core/src/lineage/types.rs`**

Delete these definitions entirely:

```rust
pub enum ReviewKind { … }
pub struct ReviewPayload { … }
pub struct DiffSummary { … }
pub enum Artifact { … }
pub enum ReviewDecision { … }
```

At the top of the file, add:

```rust
use spur_acp::{Artifact, DiffSummary, ReviewDecision, ReviewKind, ReviewPayload};
```

`ReviewRequest` keeps its definition, but its `kind` and `payload` fields now reference the imported `spur_acp::ReviewKind` and `spur_acp::ReviewPayload`:

```rust
pub struct ReviewRequest {
    pub kind: ReviewKind,      // from spur_acp
    pub payload: ReviewPayload,
    pub requested_at: SystemTime,
}
```

`ExecutorNode.pending_review: Option<ReviewRequest>` stays. `Attempt.artifacts: Vec<Artifact>` — the `Artifact` here now refers to the imported `spur_acp::Artifact`.

- [ ] **Step 5: Delete the 3 remaining mapper functions in `projection.rs`**

Find and delete:

```rust
fn map_artifact(p: &spur_acp::ExecutorArtifactPayload) -> super::types::Artifact { … }
fn map_review_kind(k: &spur_acp::ExecutorReviewKind) -> super::types::ReviewKind { … }
fn map_review_payload(p: &spur_acp::ExecutorReviewPayload) -> super::types::ReviewPayload { … }
```

Update the three sites that called them to use the wire types directly:

```rust
// Before (in ExecutorArtifact arm):
let art = map_artifact(artifact);
if let Some(a) = node.current_attempt_mut() {
    a.artifacts.push(art);
}

// After:
if let Some(a) = node.current_attempt_mut() {
    a.artifacts.push(artifact.clone());
}
```

```rust
// Before (in ExecutorReviewRequested arm):
node.pending_review = Some(ReviewRequest {
    kind: map_review_kind(kind),
    payload: map_review_payload(payload),
    requested_at: SystemTime::now(), // task-4 TODO
});

// After:
node.pending_review = Some(ReviewRequest {
    kind: kind.clone(),
    payload: payload.clone(),
    requested_at: SystemTime::now(), // task-4 TODO
});
if !self.pending_review_order.contains(&eid) {
    self.pending_review_order.push_back(eid.clone());
}
```

(Make sure the VecDeque push is preserved from the Track 1 fixes.)

- [ ] **Step 6: Update `spur-core/src/lib.rs` re-exports**

Drop core-owned `Artifact`, `ReviewKind`, `ReviewPayload`, `ReviewDecision` from lineage re-export. Add them to the `spur_acp` re-export instead:

```rust
pub use spur_acp::{Artifact, DiffSummary, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role};
pub use lineage::{
    Attempt, AttemptStatus, ExecutorId, ExecutorLineage, ExecutorNode, ReviewRequest,
};
```

- [ ] **Step 7: Update test fixtures**

Any test using `ExecutorReviewKind`/`ExecutorReviewPayload`/etc by the old prefixed name — rename to `ReviewKind`/`ReviewPayload`. Imports will be affected.

Grep: `grep -rn "ExecutorReviewKind\|ExecutorReviewPayload\|ExecutorReviewDecision\|ExecutorArtifactPayload\|ExecutorDiffSummary" crates/`

For each hit, rename. For test imports like `use spur_acp::{ExecutorArtifactPayload, …};` change to `use spur_acp::{Artifact, …};`.

- [ ] **Step 8: Verify**

Run: `cargo check --workspace`
Expected: PASS.

Run: `cargo test --workspace`
Expected: 50 tests passing.

- [ ] **Step 9: Commit**

```bash
git add -u
git commit -m "refactor(spur-lineage): collapse 5 duplicate wire/state type pairs

Move canonical Artifact/DiffSummary/ReviewKind/ReviewPayload/ReviewDecision
to spur-acp (dropping the ExecutorReview/ExecutorArtifact/ExecutorDiff
prefixes). Delete spur-core duplicates and the 3 map_* mapper functions
that translated wire→state.

ExecutorNode.pending_review, Attempt.artifacts now hold the wire types
directly. Net deletion: ~70 lines across types.rs + projection.rs."
```

---

## Task 4: Wire `Attempt.started_at` / `ended_at` from `event.occurred_at`

**Goal:** remove every `SystemTime::now()` call from `apply` / `apply_legacy`. The projection becomes a pure function of the event stream.

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs`
- Modify: `crates/spur-core/src/lineage/adapter.rs`

- [ ] **Step 1: Write the failing test for replay-purity**

Add to `crates/spur-core/tests/lineage_integration.rs`:

```rust
#[test]
fn replay_produces_identical_timestamps() {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    // Build events with fixed timestamps — then applying the same list to
    // two fresh projections must produce identical `started_at` values.
    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let events: Vec<SpurEvent> = vec![
        SpurEvent {
            occurred_at: t0,
            body: SpurEventBody::ExecutorSpawned {
                id: "w".into(),
                parent_id: None,
                session_id: SessionId("s1".into()),
                agent: "worker".into(),
                role: Role::Executor,
                task_spec: "task".into(),
            },
        },
        SpurEvent {
            occurred_at: t0 + Duration::from_secs(10),
            body: SpurEventBody::ExecutorPhaseChanged {
                id: "w".into(),
                phase: LifecycleState::Succeeded,
            },
        },
    ];

    let mut a = ExecutorLineage::new();
    for e in &events { a.apply(e); }

    std::thread::sleep(Duration::from_millis(10));

    let mut b = ExecutorLineage::new();
    for e in &events { b.apply(e); }

    let na = a.node(&ExecutorId::new("w")).unwrap();
    let nb = b.node(&ExecutorId::new("w")).unwrap();

    let aa = na.current_attempt().unwrap();
    let ab = nb.current_attempt().unwrap();

    assert_eq!(aa.started_at, ab.started_at, "started_at must be identical on replay");
    assert_eq!(aa.ended_at, ab.ended_at, "ended_at must be identical on replay");
    assert_eq!(aa.started_at, t0, "started_at must come from event.occurred_at");
}
```

Run: `cargo test -p spur-core --test lineage_integration replay_produces_identical_timestamps`
Expected: FAIL (projection currently uses `SystemTime::now()`).

- [ ] **Step 2: Thread `occurred_at` into `apply`**

In `projection.rs`, modify the `apply` method to use `event.occurred_at` for every place it currently calls `SystemTime::now()`.

Search: `grep -n "SystemTime::now" crates/spur-core/src/lineage/projection.rs`. Replace each with `event.occurred_at`:

```rust
// ExecutorSpawned arm — change the Attempt construction:
let attempt = Attempt {
    session_id: session_id.clone(),
    started_at: event.occurred_at,
    ended_at: None,
    …
};

// ExecutorPhaseChanged arm — change the terminal-close:
if let Some(status) = terminal_attempt_status(*phase) {
    if let Some(a) = node.current_attempt_mut() {
        a.ended_at = Some(event.occurred_at);
        a.status = status;
    }
}

// ExecutorReviewRequested arm — change ReviewRequest.requested_at:
node.pending_review = Some(ReviewRequest {
    kind: kind.clone(),
    payload: payload.clone(),
    requested_at: event.occurred_at,
});

// ExecutorRetryStarted arm — Attempt construction:
let new_attempt = Attempt {
    session_id: new_session_id.clone(),
    started_at: event.occurred_at,
    …
};
```

- [ ] **Step 3: Thread `occurred_at` into `apply_legacy`**

Open `crates/spur-core/src/lineage/adapter.rs`. The signature changed in Task 1 to `pub fn apply_legacy(lineage: &mut ExecutorLineage, event: &SpurEvent)`. Inside, references to `SystemTime::now()` must become `event.occurred_at`:

```rust
// fresh_attempt helper takes a second arg:
fn fresh_attempt(session: SessionId, started_at: SystemTime) -> Attempt {
    Attempt {
        session_id: session,
        started_at,
        ended_at: None,
        status: AttemptStatus::Running,
        cost_usd: 0.0,
        artifacts: Vec::new(),
        error: None,
    }
}
```

Update all call sites — every `fresh_attempt(session.clone())` becomes `fresh_attempt(session.clone(), event.occurred_at)`.

Update the terminal-close sites (`DelegationCompleted`, `SessionCompleted` arms) similarly:

```rust
// Before:
a.ended_at = Some(SystemTime::now());

// After:
a.ended_at = Some(event.occurred_at);
```

- [ ] **Step 4: Run all tests**

Run: `cargo test --workspace`
Expected: 51 passing (50 prior + 1 new replay-purity test).

- [ ] **Step 5: Grep for stragglers**

Run: `grep -rn "SystemTime::now" crates/spur-core/src/lineage/`
Expected: zero hits inside `apply` or `apply_legacy`. Only `SystemTime::now()` that remains in the crate (if any) should be outside the projection path (e.g., test-only helpers).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-core/src/lineage/projection.rs \
        crates/spur-core/src/lineage/adapter.rs \
        crates/spur-core/tests/lineage_integration.rs
git commit -m "fix(spur-lineage): wire Attempt timestamps from event.occurred_at

Previously apply() called SystemTime::now() to stamp Attempt.started_at /
ended_at, making the projection non-deterministic — replaying the same
event stream at different wall-clock times produced different state.

Now every timestamp comes from event.occurred_at (captured at emission
time). The projection is a pure function of the event stream, restoring
the load-bearing replay-purity invariant."
```

---

## Task 5: Parent-orphan buffer + `apply_inner` split

**Goal:** handle `ExecutorSpawned` events that arrive before their parent's spawn. Today such a child becomes a spurious root. New behavior: child events are buffered under the parent id, drained on parent arrival.

Also: split `apply` into `apply` + `apply_inner`. The orphan-drain loop uses `apply_inner` to skip `apply_legacy`, preventing future re-entry risk.

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs`

- [ ] **Step 1: Failing test for parent-orphan replay**

Add to `crates/spur-core/tests/lineage_projection.rs`:

```rust
#[test]
fn child_spawn_before_parent_spawn_attaches_on_parent_arrival() {
    use spur_acp::{SpurEvent, SpurEventBody};
    use std::time::SystemTime;

    let mut l = ExecutorLineage::new();

    // Child arrives FIRST — names parent that doesn't yet exist.
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "child".into(),
        parent_id: Some("parent".into()),
        session_id: SessionId("s-child".into()),
        agent: "c".into(),
        role: Role::Executor,
        task_spec: "".into(),
    }));

    // Before parent exists, child must NOT be a root.
    assert!(l.node(&ExecutorId::new("child")).is_none(),
            "child should be buffered, not attached as root");
    assert_eq!(l.root_ids().len(), 0);

    // Parent arrives.
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "parent".into(),
        parent_id: None,
        session_id: SessionId("s-parent".into()),
        agent: "p".into(),
        role: Role::Brain,
        task_spec: "".into(),
    }));

    // Now both exist and child is attached under parent.
    let p = l.node(&ExecutorId::new("parent")).unwrap();
    let c = l.node(&ExecutorId::new("child")).unwrap();
    assert_eq!(p.child_ids.len(), 1);
    assert_eq!(p.child_ids[0], ExecutorId::new("child"));
    assert_eq!(c.parent_id, Some(ExecutorId::new("parent")));
    assert_eq!(l.root_ids().len(), 1, "only parent is a root");
}
```

Run: `cargo test -p spur-core --test lineage_projection child_spawn_before_parent_spawn_attaches_on_parent_arrival`
Expected: FAIL.

- [ ] **Step 2: Add parent-orphan buffer and refactor `apply` into `apply` + `apply_inner`**

In `projection.rs`, add a field to `ExecutorLineage`:

```rust
#[derive(Debug, Default, Clone)]
pub struct ExecutorLineage {
    nodes: HashMap<ExecutorId, ExecutorNode>,
    roots: Vec<ExecutorId>,
    orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>,
    /// Parent-orphan buffer: `ExecutorSpawned` events whose `parent_id` is not
    /// yet in `nodes` are stashed here under the parent id, drained on parent
    /// arrival.
    parent_orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>,
    pending_review_order: VecDeque<ExecutorId>,
}
```

Refactor `apply` so the orphan drain path calls `apply_inner` directly, skipping `apply_legacy`:

```rust
pub fn apply(&mut self, event: &SpurEvent) {
    // Legacy adapter runs only on top-level apply — NOT on orphan replay.
    super::adapter::apply_legacy(self, event);
    self.apply_inner(event);
}

fn apply_inner(&mut self, event: &SpurEvent) {
    match &event.body {
        SpurEventBody::ExecutorSpawned {
            id, parent_id, session_id, agent, role, task_spec,
        } => {
            let eid = ExecutorId::new(id);
            let parent = parent_id.as_ref().map(ExecutorId::new);

            // Parent exists check: buffer if named but missing.
            if let Some(ref p) = parent {
                if !self.nodes.contains_key(p) {
                    self.buffer_parent_orphan(p.clone(), event.clone());
                    return;
                }
            }

            let attempt = Attempt { …, started_at: event.occurred_at, … };
            let node = ExecutorNode { … };
            match parent {
                Some(p) => {
                    self.nodes.get_mut(&p).unwrap().child_ids.push(eid.clone());
                    self.nodes.insert(eid.clone(), node);
                }
                None => {
                    self.roots.push(eid.clone());
                    self.nodes.insert(eid.clone(), node);
                }
            }
            // Replay child-orphans buffered under this new node.
            if let Some(queue) = self.orphan_buffer.remove(&eid) {
                for ev in queue { self.apply_inner(&ev); }
            }
            // Replay parent-orphans buffered under this new node.
            if let Some(queue) = self.parent_orphan_buffer.remove(&eid) {
                for ev in queue { self.apply_inner(&ev); }
            }
        }

        // (other variants use the same `apply_inner` path — copy their
        // current bodies from `apply`, removing the outer match wrapper.)

        _ => {}
    }
}

fn buffer_parent_orphan(&mut self, parent_id: ExecutorId, event: SpurEvent) {
    let q = self.parent_orphan_buffer.entry(parent_id).or_default();
    if q.len() < MAX_ORPHAN_BUFFER_PER_EXEC {
        q.push_back(event);
    } else {
        tracing::warn!("parent-orphan buffer overflow; dropping event");
    }
}
```

Move every match arm from the old `apply` to `apply_inner`. The `buffer_orphan` (child-orphan) helper stays as-is but is now only reachable from `apply_inner`.

- [ ] **Step 3: Run tests**

Run: `cargo test --workspace`
Expected: 52 passing (51 + 1 new parent-orphan test).

- [ ] **Step 4: Regression test for `apply_legacy` non-reentrancy**

Add to `lineage_projection.rs`:

```rust
#[test]
fn orphan_replay_does_not_retrigger_legacy_adapter() {
    // Buffer a child-orphan phase change, then trigger spawn.
    // Legacy adapter must NOT fire on the replay (today's apply_legacy is a
    // no-op for Executor* variants, but a future change must not break this).
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorPhaseChanged {
        id: "x".into(),
        phase: LifecycleState::Running,
    }));
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "x".into(),
        parent_id: None,
        session_id: SessionId("s".into()),
        agent: "a".into(),
        role: Role::Brain,
        task_spec: "".into(),
    }));

    let n = l.node(&ExecutorId::new("x")).unwrap();
    // One attempt only — no legacy-path duplicate spawn.
    assert_eq!(n.attempts.len(), 1);
    assert_eq!(n.phase, LifecycleState::Running);
}
```

Run the test. Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/lineage/projection.rs \
        crates/spur-core/tests/lineage_projection.rs
git commit -m "feat(spur-lineage): parent-orphan buffer + apply_inner re-entry guard

Child ExecutorSpawned events arriving before their parent's spawn are
now buffered and drained on parent arrival, instead of silently becoming
spurious roots.

apply() is split into apply() (runs apply_legacy + apply_inner) and
apply_inner (body-only). Orphan-drain loops call apply_inner, preventing
future re-entry into apply_legacy that could double-apply events."
```

---

## Task 6: `attempt_n` validation warn

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs`

- [ ] **Step 1: Add the validation**

In `apply_inner`'s `ExecutorRetryStarted` arm, add:

```rust
SpurEventBody::ExecutorRetryStarted {
    id, attempt_n, reason: _, new_session_id,
} => {
    let eid = ExecutorId::new(id);
    if let Some(node) = self.nodes.get_mut(&eid) {
        let expected = node.attempts.len() as u32 + 1;
        if *attempt_n != expected {
            tracing::warn!(
                executor_id = %id,
                got = *attempt_n,
                expected,
                "attempt_n mismatch — orchestrator may have dropped retry events"
            );
        }
        let new_attempt = Attempt { …, started_at: event.occurred_at, … };
        node.attempts.push(new_attempt);
        node.phase = LifecycleState::Running;
    } else {
        self.buffer_orphan(eid, event.clone());
    }
}
```

- [ ] **Step 2: Test the warn**

Add to `lineage_projection.rs`:

```rust
#[test]
fn attempt_n_mismatch_still_appends_attempt() {
    let mut l = ExecutorLineage::new();
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorSpawned {
        id: "w".into(),
        parent_id: None,
        session_id: SessionId("s1".into()),
        agent: "w".into(),
        role: Role::Brain,
        task_spec: "".into(),
    }));
    // Skip to attempt 5 — orchestrator dropped retry events 2..4.
    l.apply(&SpurEvent::now(SpurEventBody::ExecutorRetryStarted {
        id: "w".into(),
        attempt_n: 5,
        reason: "drop".into(),
        new_session_id: SessionId("s5".into()),
    }));

    let n = l.node(&ExecutorId::new("w")).unwrap();
    // Still appends — validation is observability-only.
    assert_eq!(n.attempts.len(), 2, "retry appends even on mismatch");
    assert_eq!(n.phase, LifecycleState::Running);
}
```

Run: `cargo test -p spur-core --test lineage_projection attempt_n_mismatch_still_appends_attempt`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-core/src/lineage/projection.rs \
        crates/spur-core/tests/lineage_projection.rs
git commit -m "feat(spur-lineage): validate attempt_n with observability warn

ExecutorRetryStarted now compares attempt_n against the projection's
known attempt count. Mismatches produce a tracing::warn but the retry
is still appended — validation is observability-only, not a gate."
```

---

## Task 7: Replay-purity acceptance test + doc invariant

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs` (doc comment)
- Modify: `crates/spur-core/tests/lineage_integration.rs`

- [ ] **Step 1: Add the doc invariant comment**

At the top of `projection.rs`, replace the existing module doc with:

```rust
//! Executor lineage projection.
//!
//! ## Load-bearing invariant: replay-purity
//!
//! `ExecutorLineage::apply` is a **pure function** of the `SpurEvent` stream.
//! Feeding the same events in the same order to two fresh projections always
//! produces byte-for-byte identical state — including `started_at`, `ended_at`,
//! `cost_usd`, and every other field.
//!
//! Two rules enforce this:
//! 1. Never call `SystemTime::now()` inside `apply` or `apply_legacy` or
//!    helpers they transitively call. Timestamps come from `event.occurred_at`.
//! 2. Never depend on `HashMap` iteration order for observable output. Use
//!    `Vec`/`VecDeque` when order matters (e.g., `pending_review_order`).
//!
//! ## Idempotency
//!
//! Every event arm is idempotent — applying the same event twice produces
//! the same state as applying it once. Exception: `SpurEventBody::CostUpdate`
//! is deliberately additive (two updates accumulate). Tests enforce both
//! invariants.
```

- [ ] **Step 2: Add the full-equality replay test**

Add to `lineage_integration.rs`:

```rust
#[test]
fn replay_produces_byte_identical_state() {
    use std::time::{Duration, UNIX_EPOCH};

    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let mk = |offset_secs: u64, body: SpurEventBody| SpurEvent {
        occurred_at: t0 + Duration::from_secs(offset_secs),
        body,
    };

    let events: Vec<SpurEvent> = vec![
        mk(0, SpurEventBody::BrainSpawned {
            agent: "kiro".into(),
            session: SessionId("b".into()),
        }),
        mk(1, SpurEventBody::WorkerSpawned {
            agent: "w".into(),
            session: SessionId("w1".into()),
            worktree: PathBuf::from("/tmp"),
        }),
        mk(2, SpurEventBody::DelegationRequested {
            from: SessionId("b".into()),
            to_agent: "w".into(),
            task: "task".into(),
        }),
        mk(3, SpurEventBody::CostUpdate {
            session: SessionId("w1".into()),
            agent: "w".into(),
            estimated_cost_usd: 0.25,
        }),
        mk(4, SpurEventBody::ExecutorArtifact {
            id: "w1".into(),
            artifact: Artifact::PrUrl("https://x".into()),
        }),
        mk(5, SpurEventBody::ExecutorReviewRequested {
            id: "w1".into(),
            kind: ReviewKind::Completion,
            payload: ReviewPayload {
                summary: "".into(), diff_summary: None, pr_url: None, error: None,
            },
        }),
        mk(6, SpurEventBody::ExecutorReviewResolved {
            id: "w1".into(),
            decision: ReviewDecision::Approve,
        }),
        mk(7, SpurEventBody::DelegationCompleted {
            worker_session: SessionId("w1".into()),
            status: DelegationStatus::Success,
        }),
    ];

    let mut a = ExecutorLineage::new();
    for e in &events { a.apply(e); }

    std::thread::sleep(std::time::Duration::from_millis(10));

    let mut b = ExecutorLineage::new();
    for e in &events { b.apply(e); }

    // Collect full node state for comparison (timestamps included).
    let collect = |l: &ExecutorLineage| -> Vec<(ExecutorId, LifecycleState, Vec<(SystemTime, Option<SystemTime>)>)> {
        let mut out: Vec<_> = l.nodes().map(|n| {
            let attempts: Vec<_> = n.attempts.iter().map(|a| (a.started_at, a.ended_at)).collect();
            (n.id.clone(), n.phase, attempts)
        }).collect();
        out.sort_by(|x, y| x.0.0.cmp(&y.0.0)); // deterministic ordering
        out
    };

    assert_eq!(collect(&a), collect(&b), "replay must produce identical state including timestamps");
}
```

Imports at the top of the file may need updates (pull in `Artifact`, `ReviewKind`, `ReviewPayload`, `ReviewDecision`, `LifecycleState`).

- [ ] **Step 3: Idempotency test**

Add to `lineage_integration.rs`:

```rust
#[test]
fn applying_same_event_twice_is_idempotent_except_cost() {
    use std::time::{Duration, UNIX_EPOCH};
    let t0 = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let spawn = SpurEvent {
        occurred_at: t0,
        body: SpurEventBody::ExecutorSpawned {
            id: "w".into(), parent_id: None,
            session_id: SessionId("s".into()),
            agent: "a".into(), role: Role::Brain, task_spec: "".into(),
        },
    };
    let phase = SpurEvent {
        occurred_at: t0 + Duration::from_secs(1),
        body: SpurEventBody::ExecutorPhaseChanged {
            id: "w".into(), phase: LifecycleState::Running,
        },
    };

    let mut l = ExecutorLineage::new();
    l.apply(&spawn); l.apply(&phase);
    l.apply(&spawn); l.apply(&phase); // re-apply — idempotent

    let n = l.node(&ExecutorId::new("w")).unwrap();
    assert_eq!(n.attempts.len(), 1, "duplicate spawn must not create new node/attempt");
    assert_eq!(n.phase, LifecycleState::Running);
}
```

Run: `cargo test --workspace`
Expected: 54 passing (52 + 2 new).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/lineage/projection.rs \
        crates/spur-core/tests/lineage_integration.rs
git commit -m "docs(spur-lineage): replay-purity invariant + acceptance tests

Document the load-bearing invariant at the top of projection.rs and add
the acceptance test that compares full node state (including timestamps)
between two replays of the same event log. Also add an idempotency test
covering spawn + phase duplicates."
```

---

## Done criteria

- [ ] `cargo test --workspace` passes with 54 tests.
- [ ] `grep -n "SystemTime::now" crates/spur-core/src/lineage/` returns no hits inside `apply`, `apply_legacy`, or helpers they call.
- [ ] `grep -rn "parse_phase\|parse_role\|map_artifact\|map_review_kind\|map_review_payload" crates/` returns zero hits.
- [ ] `grep -rn "pub enum Role\|pub enum LifecycleState" crates/spur-core/` returns zero hits (moved to spur-acp).
- [ ] `grep -rn "ExecutorReviewKind\|ExecutorReviewPayload\|ExecutorReviewDecision\|ExecutorArtifactPayload\|ExecutorDiffSummary" crates/` returns zero hits (renamed to canonical names).
- [ ] Serializing then deserializing an `ExecutorPhaseChanged` with invalid JSON variant name returns `Err` (not silent accept).
- [ ] Replaying the same event list twice produces byte-identical `Attempt.started_at` / `ended_at` values.

## Follow-up (not in this plan)

- Orchestrator-side spec (translating `ReviewDecision` into tool-call result) — still pending.
- Envelope could later grow `source` / `correlation_id` fields for distributed observability. Out of scope for v1.

---

## Self-review

**Spec coverage:**

| Spec item | Plan task |
|---|---|
| Envelope struct + SpurEventBody rename | Task 1 |
| Emitter migration | Task 1 Step 3 |
| Matcher migration | Task 1 Step 4 |
| `requested_at` removal | Task 1 Step 5 |
| `LifecycleState`/`Role` move to spur-acp | Task 2 |
| Typed `phase`/`role` fields | Task 2 Step 3 |
| `parse_phase`/`parse_role` deletion | Task 2 Step 7 |
| 5 duplicate type collapse | Task 3 |
| 3 mapper function deletion | Task 3 Step 5 |
| `Attempt.started_at` from envelope | Task 4 |
| Replay-purity regression test | Task 4 Step 1, Task 7 Step 2 |
| Parent-orphan buffer | Task 5 |
| `apply_inner` re-entry guard | Task 5 Step 2 |
| `attempt_n` validation | Task 6 |
| Idempotency test | Task 7 Step 3 |
| Invariant doc comment | Task 7 Step 1 |

All spec requirements covered.

**Placeholder scan:** No TBD / TODO / vague requirements in the plan. The `// task-4 TODO` literal comment in Task 3 Step 5 is a deliberate in-code marker that Task 4 Step 2 resolves — not a plan placeholder.

**Type consistency:** `SpurEvent::now(SpurEventBody::X{…})` constructor pattern used consistently. `&event.body` match pattern used consistently. `event.occurred_at` field access pattern used consistently. Renamed wire types (`Artifact`, `ReviewKind`, etc.) used consistently after Task 3.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-04-13-lineage-event-contract-hardening.md`. Two execution options:

1. **Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.
2. **Inline Execution** — execute tasks in this session using `executing-plans`, batch execution with checkpoints.

Which approach?

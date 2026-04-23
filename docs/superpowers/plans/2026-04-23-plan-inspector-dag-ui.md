# Plan Inspector DAG UI Implementation Plan

> **For SPUR orchestrator:** This plan is designed for `submit_plan(persist_as_epic=true)`.
> Each task becomes a beads issue with `spur:plan-task-id` and `spur:plan-id` labels.

**Source spec:** `docs/superpowers/specs/2026-04-23-plan-inspector-dag-ui-design.md`
**Design epic:** `N/A (PM backend unavailable in this environment; spec approved in-session)`

**Goal:** Add a beads-first plan inspector flow to SPUR: a compact `PlanPulse` in `SessionDetail`, a dedicated `PlanInspectorView`, and the event/projection plumbing required to render durable plan state with live executor overlays.

**Architecture:** Introduce a typed `PlanSnapshotUpdated` ACP event that carries the canonical beads-backed plan snapshot. `spur-mcp` emits that snapshot whenever persisted plan state changes, `spur-core` folds it into a `PlanProjectionStore`, and `spur-tui` consumes that store to render `PlanPulse` and the full-screen inspector. Executor lineage remains a secondary live overlay joined by `issue_id`, never the source of truth.

**Tech Stack:** Rust 2021 workspace | `serde` ACP events | `spur-mcp` persisted-plan projection | `spur-core` event-sourced stores | `ratatui` + `crossterm` TUI | existing `ExecutorLineage` overlay

---

## File Structure

**Files created:**
- `crates/spur-mcp/src/plan/snapshot.rs` - canonical `PlanState -> spur_acp::PlanSnapshot` conversion
- `crates/spur-mcp/tests/plan_snapshot_events.rs` - persisted-plan snapshot emission coverage
- `crates/spur-core/src/plan_projection/mod.rs` - plan projection module root
- `crates/spur-core/src/plan_projection/types.rs` - tracked plan/task types
- `crates/spur-core/src/plan_projection/projection.rs` - `PlanProjectionStore`
- `crates/spur-core/tests/plan_projection.rs` - projection behavior tests
- `crates/spur-tui/src/components/plan_pulse.rs` - one-line `SessionDetail` plan summary
- `crates/spur-tui/src/components/plan_stage_board.rs` - lane-board renderer for the inspector
- `crates/spur-tui/src/components/plan_task_detail.rs` - selected-task detail pane
- `crates/spur-tui/src/views/plan_inspector.rs` - dedicated inspector view
- `crates/spur-tui/tests/plan_inspector_snapshot.rs` - wide/narrow render snapshots

**Files modified:**
- `crates/spur-acp/src/domain/events.rs` - add typed plan snapshot event and payload structs
- `crates/spur-acp/src/lib.rs` - re-export snapshot types
- `crates/spur-acp/tests/executor_events_roundtrip.rs` - snapshot serde coverage
- `crates/spur-mcp/src/plan/mod.rs` - call snapshot emitter after durable plan mutations
- `crates/spur-mcp/src/plan/reconciler.rs` - emit refreshed snapshots during durable reconciler transitions
- `crates/spur-mcp/src/server.rs` - emit seed/recovery snapshots for persisted plans
- `crates/spur-core/src/lib.rs` - export plan projection module
- `crates/spur-tui/src/action.rs` - add `ViewId::PlanInspector(SessionId)`
- `crates/spur-tui/src/app.rs` - own `PlanProjectionStore`, route events, toggle inspector view
- `crates/spur-tui/src/views/mod.rs` - expose `PlanProjectionStore` through `ViewContext`
- `crates/spur-tui/src/views/session_detail.rs` - render `PlanPulse`, gate `Alt+P`
- `crates/spur-tui/src/components/mod.rs` - register new plan components
- `crates/spur-tui/src/components/react_trace/dispatch.rs` - demote plan checklist duplication when snapshot-backed UI is present

**Files left untouched:**
- `crates/spur-cli`
- `crates/spur-cost`
- `crates/spur-worktree`
- `crates/spur-pm`
- review submission flow (`review_task`) beyond snapshot refresh hooks

---

## Task 1: Add the ACP plan snapshot contract

**Task ID:** `plan-snapshot-contract`

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`
- Modify: `crates/spur-acp/src/lib.rs`
- Modify: `crates/spur-acp/tests/executor_events_roundtrip.rs`

**Depends on:** none

**Acceptance Criteria:**
- [ ] `SpurEventBody` includes a typed `PlanSnapshotUpdated` variant with `session_id` and snapshot payload
- [ ] Snapshot payload carries enough durable state for the inspector without calling `get_plan_status`
- [ ] ACP round-trip tests cover the new event and reject malformed payloads

**Suggested Worker:** `codex`

**Scope Boundary:**
- IN scope: ACP event schema, payload structs, serde tests, re-exports
- OUT of scope: emitting snapshots from `spur-mcp`, TUI rendering, beads projection logic
- If you discover additional lifecycle events are needed beyond `PlanSnapshotUpdated`, emit `scope_drift` before changing multiple event families

**Implementation:**
- [ ] **Step 1: Write the failing round-trip test**

```rust
#[test]
fn plan_snapshot_updated_roundtrips() {
    use spur_acp::{PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SessionId, SpurEvent, SpurEventBody};

    let ev = SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: SessionId("brain-1".into()),
        snapshot: Box::new(PlanSnapshot {
            plan_id: "p-123".into(),
            status: "running".into(),
            progress: "1/3 reviewed, 1 running, 1 pending".into(),
            next_action: "Workers still running. Poll get_plan_status to monitor.".into(),
            ready_to_merge: false,
            counts: PlanSnapshotCounts {
                pending: 1,
                ready: 0,
                dispatched: 1,
                awaiting_review: 1,
                approved: 0,
                rejected: 0,
                failed: 0,
                cancelled: 0,
            },
            tasks: vec![PlanSnapshotTask {
                task_id: "task-projection".into(),
                task_name: "Build PlanProjection".into(),
                agent: "claude-code".into(),
                issue_id: Some("BEADS-42".into()),
                status: "awaiting_review".into(),
                attempt: 1,
                max_attempts: 3,
                depends_on: vec!["task-contract".into()],
                blocked_by: Vec::new(),
                summary: Some("projects plan status into UI".into()),
                feedback: None,
                error: None,
                worker_branch: Some("spur/worker-123".into()),
                delegation_id: Some("del-123".into()),
            }],
        }),
    });

    let json = serde_json::to_string(&ev).unwrap();
    let round: SpurEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(round.body, SpurEventBody::PlanSnapshotUpdated { .. }));
}
```

- [ ] **Step 2: Run the ACP tests to verify the contract is missing**

Run: `cargo test -p spur-acp plan_snapshot_updated_roundtrips -- --nocapture`
Expected: FAIL with unknown variant / missing snapshot types

- [ ] **Step 3: Add the typed payload structs and event variant**

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSnapshot {
    pub plan_id: String,
    pub status: String,
    pub progress: String,
    pub next_action: String,
    pub ready_to_merge: bool,
    pub counts: PlanSnapshotCounts,
    pub tasks: Vec<PlanSnapshotTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PlanSnapshotCounts {
    pub pending: u32,
    pub ready: u32,
    pub dispatched: u32,
    pub awaiting_review: u32,
    pub approved: u32,
    pub rejected: u32,
    pub failed: u32,
    pub cancelled: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSnapshotTask {
    pub task_id: String,
    pub task_name: String,
    pub agent: String,
    pub issue_id: Option<String>,
    pub status: String,
    pub attempt: u32,
    pub max_attempts: u32,
    pub depends_on: Vec<String>,
    pub blocked_by: Vec<String>,
    pub summary: Option<String>,
    pub feedback: Option<String>,
    pub error: Option<String>,
    pub worker_branch: Option<String>,
    pub delegation_id: Option<String>,
}
```

- [ ] **Step 4: Re-export the new types from `spur-acp`**

```rust
pub use domain::events::{
    PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SpurEvent, SpurEventBody,
};
```

- [ ] **Step 5: Re-run ACP tests and commit**

Run: `cargo test -p spur-acp plan_snapshot_updated_roundtrips -- --nocapture`
Expected: PASS

Run: `cargo test -p spur-acp executor_events_roundtrip -- --nocapture`
Expected: PASS

```bash
git add crates/spur-acp/src/domain/events.rs crates/spur-acp/src/lib.rs crates/spur-acp/tests/executor_events_roundtrip.rs
git commit -m "feat(spur-acp): P1 add plan snapshot event contract"
```

---

## Task 2: Emit beads-backed plan snapshots from `spur-mcp`

**Task ID:** `plan-snapshot-emission`

**Files:**
- Create: `crates/spur-mcp/src/plan/snapshot.rs`
- Modify: `crates/spur-mcp/src/plan/mod.rs`
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`
- Modify: `crates/spur-mcp/src/server.rs`
- Create: `crates/spur-mcp/tests/plan_snapshot_events.rs`

**Depends on:** `plan-snapshot-contract`

**Acceptance Criteria:**
- [ ] Persisted `submit_plan` emits one seed `PlanSnapshotUpdated` event after beads persistence succeeds
- [ ] Durable plan state changes refresh the snapshot after dispatch, completion persistence, review, and ready-to-merge transitions
- [ ] Startup recovery emits snapshots for recovered persisted plans so the TUI can rebuild state after restart

**Suggested Worker:** `claude-code-acp`

**Scope Boundary:**
- IN scope: snapshot builder, event emission hooks, persisted-plan tests
- OUT of scope: new plan task semantics, `get_plan_status` response shape changes, TUI consumers
- If you need to mutate beads labels or status semantics beyond existing persisted-plan transitions, emit `scope_drift`

**Implementation:**
- [ ] **Step 1: Write the failing persisted-plan emission tests**

```rust
#[tokio::test]
async fn persisted_submit_plan_emits_plan_snapshot_updated() {
    let events = Arc::new(TestEventSink::default());
    let server = test_server_with_beads(events.clone()).await;

    let response = server.__test_call_submit_plan(json!({
        "persist_as_epic": true,
        "epic_title": "Plan inspector",
        "epic_body": "seed snapshot contract",
        "tasks": [{
            "task_id": "task-contract",
            "agent": "codex",
            "task": "CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT",
        }]
    })).await;

    assert!(response.get("error").is_none(), "submit_plan should succeed: {response}");
    assert!(events.iter().any(|event| matches!(
        event.body,
        spur_acp::SpurEventBody::PlanSnapshotUpdated { .. }
    )));
}
```

```rust
#[tokio::test]
async fn review_task_refreshes_plan_snapshot() {
    // submit persisted plan, drive one task to awaiting_review, then approve
    // and assert the sink saw a later PlanSnapshotUpdated with ready_to_merge=true
}
```

- [ ] **Step 2: Run the `spur-mcp` tests to confirm snapshots are not emitted yet**

Run: `cargo test -p spur-mcp plan_snapshot_events -- --nocapture`
Expected: FAIL because no `PlanSnapshotUpdated` event is emitted

- [ ] **Step 3: Add a single snapshot builder that mirrors `build_plan_status`**

```rust
pub fn build_plan_snapshot(state: &crate::plan::PlanState) -> spur_acp::PlanSnapshot {
    let status = crate::plan::build_plan_status(&state.plan_id, state);
    spur_acp::PlanSnapshot {
        plan_id: state.plan_id.clone(),
        status: status["status"].as_str().unwrap_or("partial").to_string(),
        progress: status["progress"].as_str().unwrap_or_default().to_string(),
        next_action: status["next_action"].as_str().unwrap_or_default().to_string(),
        ready_to_merge: status["ready_to_merge"].as_bool().unwrap_or(false),
        counts: snapshot_counts(&status["counts"]),
        tasks: state.tasks.iter().map(snapshot_task).collect(),
    }
}
```

- [ ] **Step 4: Emit snapshots from the durable plan lifecycle**

```rust
fn emit_plan_snapshot(
    sink: Option<&dyn crate::events::McpEventSink>,
    state: &crate::plan::PlanState,
) {
    let Some(sink) = sink else { return };
    let snapshot = crate::plan::snapshot::build_plan_snapshot(state);
    sink.emit(spur_acp::SpurEventBody::PlanSnapshotUpdated {
        session_id: state.brain_session_id.clone(),
        snapshot: Box::new(snapshot),
    });
}
```

Hook `emit_plan_snapshot(...)` after:
- persisted `submit_plan` installs the beads-backed plan
- reconciler records a dispatch
- persisted completion changes a task status
- `handle_review_task` mutates a task
- startup recovery re-installs persisted plans

- [ ] **Step 5: Re-run tests and commit**

Run: `cargo test -p spur-mcp plan_snapshot_events -- --nocapture`
Expected: PASS

Run: `cargo test -p spur-mcp submit_plan_persist -- --nocapture`
Expected: PASS

```bash
git add crates/spur-mcp/src/plan/snapshot.rs crates/spur-mcp/src/plan/mod.rs crates/spur-mcp/src/plan/reconciler.rs crates/spur-mcp/src/server.rs crates/spur-mcp/tests/plan_snapshot_events.rs
git commit -m "feat(spur-mcp): P2 emit beads-backed plan snapshots"
```

---

## Task 3: Add a reusable `PlanProjectionStore` in `spur-core`

**Task ID:** `plan-projection-store`

**Files:**
- Create: `crates/spur-core/src/plan_projection/mod.rs`
- Create: `crates/spur-core/src/plan_projection/types.rs`
- Create: `crates/spur-core/src/plan_projection/projection.rs`
- Modify: `crates/spur-core/src/lib.rs`
- Create: `crates/spur-core/tests/plan_projection.rs`

**Depends on:** `plan-snapshot-contract`

**Acceptance Criteria:**
- [ ] `spur-core` exposes a read-only `PlanProjectionStore` keyed by session and plan id
- [ ] The store derives deterministic stage indices from `depends_on`
- [ ] The store keeps durable task identity (`issue_id`) available for executor-lineage joins

**Suggested Worker:** `claude-code-acp`

**Scope Boundary:**
- IN scope: projection types, event folding, stage derivation, projection tests, public exports
- OUT of scope: TUI rendering, ACP schema changes, server emission logic
- If you need additional runtime data that is not present in `PlanSnapshotUpdated`, emit `scope_drift`

**Implementation:**
- [ ] **Step 1: Write the failing projection tests**

```rust
#[test]
fn current_for_session_prefers_active_plan() {
    use spur_acp::{PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, SessionId, SpurEvent, SpurEventBody};
    use spur_core::plan_projection::PlanProjectionStore;

    let session = SessionId("brain-1".into());
    let mut store = PlanProjectionStore::default();
    store.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: session.clone(),
        snapshot: Box::new(sample_snapshot("p-old", "approved")),
    }));
    store.apply(&SpurEvent::now(SpurEventBody::PlanSnapshotUpdated {
        session_id: session.clone(),
        snapshot: Box::new(sample_snapshot("p-new", "running")),
    }));

    assert_eq!(store.current_for_session(&session).unwrap().plan_id, "p-new");
}
```

```rust
#[test]
fn projection_derives_stage_depth_from_dependencies() {
    let store = store_with_snapshot(sample_snapshot_with_deps());
    let plan = store.plan("p-123").unwrap();
    assert_eq!(plan.tasks["task-contract"].stage_idx, 0);
    assert_eq!(plan.tasks["task-ui"].stage_idx, 1);
}
```

- [ ] **Step 2: Run the `spur-core` tests to verify the store does not exist yet**

Run: `cargo test -p spur-core plan_projection -- --nocapture`
Expected: FAIL because `plan_projection` module is missing

- [ ] **Step 3: Add the projection types and folding logic**

```rust
#[derive(Debug, Clone, Default)]
pub struct PlanProjectionStore {
    by_plan: HashMap<String, TrackedPlan>,
    current_by_session: HashMap<SessionId, String>,
}

impl PlanProjectionStore {
    pub fn apply(&mut self, event: &SpurEvent) {
        if let SpurEventBody::PlanSnapshotUpdated { session_id, snapshot } = &event.body {
            let plan = TrackedPlan::from_snapshot(session_id.clone(), snapshot.as_ref());
            self.current_by_session
                .insert(session_id.clone(), plan.plan_id.clone());
            self.by_plan.insert(plan.plan_id.clone(), plan);
        }
    }
}
```

```rust
fn derive_stage_idx(task_id: &str, deps: &HashMap<String, Vec<String>>) -> usize {
    deps.get(task_id)
        .map(|parents| parents.iter().map(|parent| derive_stage_idx(parent, deps) + 1).max().unwrap_or(0))
        .unwrap_or(0)
}
```

- [ ] **Step 4: Export the module from `spur-core`**

```rust
pub mod plan_projection;

pub use plan_projection::{PlanProjectionStore, TrackedPlan, TrackedTask};
```

- [ ] **Step 5: Re-run tests and commit**

Run: `cargo test -p spur-core plan_projection -- --nocapture`
Expected: PASS

Run: `cargo test -p spur-core -- --nocapture`
Expected: PASS

```bash
git add crates/spur-core/src/plan_projection/mod.rs crates/spur-core/src/plan_projection/types.rs crates/spur-core/src/plan_projection/projection.rs crates/spur-core/src/lib.rs crates/spur-core/tests/plan_projection.rs
git commit -m "feat(spur-core): P3 add plan projection store"
```

---

## Task 4: Plumb plan projection and inspector routing into the TUI app

**Task ID:** `plan-app-plumbing`

**Files:**
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs`
- Modify: `crates/spur-tui/src/views/mod.rs`
- Create: `crates/spur-tui/src/views/plan_inspector.rs`

**Depends on:** `plan-projection-store`

**Acceptance Criteria:**
- [ ] `App` owns a `PlanProjectionStore` and folds `PlanSnapshotUpdated` events into it
- [ ] `ViewContext` exposes read-only plan projection access next to lineage
- [ ] `Alt+P` can target `ViewId::PlanInspector(SessionId)` and `NavigateBack` returns to `SessionDetail`

**Suggested Worker:** `claude-code-acp`

**Scope Boundary:**
- IN scope: app state, view context, top-level navigation, minimal inspector shell
- OUT of scope: actual inspector board rendering, `SessionDetail` pulse rendering, trace formatting
- If `NavigateBack` semantics require broader app-stack changes, emit `risk` before rewriting unrelated navigation flows

**Implementation:**
- [ ] **Step 1: Write failing TUI navigation tests**

```rust
#[test]
fn navigate_to_plan_inspector_and_back_returns_to_session_detail() {
    let session = SessionId("brain-1".into());
    let mut app = test_app_with_session(session.clone());

    app.process_action(Action::NavigateTo(ViewId::PlanInspector(session.clone())));
    assert!(matches!(app.current_view(), ViewId::PlanInspector(_)));

    app.process_action(Action::NavigateBack);
    assert!(matches!(app.current_view(), ViewId::SessionDetail(_)));
}
```

```rust
#[test]
fn plan_snapshot_event_updates_plan_store() {
    let mut app = test_app();
    app.handle_spur_event(sample_plan_snapshot_event(), &test_ctx());
    assert!(app.plan_projection().current_for_session(&SessionId("brain-1".into())).is_some());
}
```

- [ ] **Step 2: Run the TUI tests to verify plan projection plumbing is missing**

Run: `cargo test -p spur-tui navigate_to_plan_inspector_and_back_returns_to_session_detail -- --nocapture`
Expected: FAIL with missing `PlanInspector` view or plan store accessors

- [ ] **Step 3: Add app-level plan projection state and view routing**

```rust
pub enum ViewId {
    Dashboard,
    SessionPicker,
    SessionDetail(SessionId),
    MermaidOverlay(SessionId),
    PlanInspector(SessionId),
}

pub struct ViewContext<'a> {
    pub lineage: &'a spur_core::lineage::projection::ExecutorLineage,
    pub plan_projection: &'a spur_core::plan_projection::PlanProjectionStore,
    pub brain_status: &'a crate::app::BrainStatus,
    pub license_badge: Option<&'a LicenseBadge>,
    pub flag_summary: Option<(usize, usize)>,
}
```

```rust
if matches!(&event.body, spur_acp::SpurEventBody::PlanSnapshotUpdated { .. }) {
    self.plan_projection.apply(&event);
}
```

- [ ] **Step 4: Add the minimal `PlanInspectorView` shell**

```rust
pub struct PlanInspectorView {
    session_id: SessionId,
}

impl View for PlanInspectorView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &ViewContext) -> Option<Action> {
        match normalize_macos_option(key) {
            KeyEvent { code: KeyCode::Esc, .. } => Some(Action::NavigateBack),
            KeyEvent { code: KeyCode::Char('p'), modifiers, .. } if modifiers.contains(KeyModifiers::ALT) => {
                Some(Action::NavigateBack)
            }
            _ => None,
        }
    }
}
```

- [ ] **Step 5: Re-run tests and commit**

Run: `cargo test -p spur-tui navigate_to_plan_inspector_and_back_returns_to_session_detail -- --nocapture`
Expected: PASS

Run: `cargo test -p spur-tui plan_snapshot_event_updates_plan_store -- --nocapture`
Expected: PASS

```bash
git add crates/spur-tui/src/action.rs crates/spur-tui/src/app.rs crates/spur-tui/src/views/mod.rs crates/spur-tui/src/views/plan_inspector.rs
git commit -m "feat(spur-tui): P4 add plan inspector routing"
```

---

## Task 5: Render `PlanPulse` in `SessionDetail` and demote duplicate trace output

**Task ID:** `session-detail-plan-pulse`

**Files:**
- Create: `crates/spur-tui/src/components/plan_pulse.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/components/react_trace/dispatch.rs`

**Depends on:** `plan-app-plumbing`

**Acceptance Criteria:**
- [ ] `SessionDetail` shows a one-line plan summary when a tracked plan exists for the session
- [ ] `Alt+P` opens the inspector only when the current session has a tracked plan, otherwise it emits a brief no-op hint
- [ ] Plan checklist text no longer duplicates the same state in the main trace once snapshot-backed UI is available

**Suggested Worker:** `codex`

**Scope Boundary:**
- IN scope: `PlanPulse` rendering, `Alt+P` gating, trace-policy adjustment for plan checklist text
- OUT of scope: inspector lane-board rendering, dashboard activity log changes, new ACP events
- If you need new app-wide actions beyond `NavigateTo(ViewId::PlanInspector(_))`, emit `scope_drift`

**Implementation:**
- [ ] **Step 1: Write the failing `SessionDetail` tests**

```rust
#[test]
fn alt_p_noops_without_tracked_plan() {
    let mut view = make_view();
    let key = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT);
    assert!(view.handle_key(key, &test_ctx()).is_none());
}
```

```rust
#[test]
fn alt_p_opens_plan_inspector_when_plan_is_tracked() {
    let mut view = make_view();
    let mut plans = spur_core::plan_projection::PlanProjectionStore::default();
    plans.apply(&sample_plan_snapshot_event());
    let ctx = ViewContext {
        lineage: &spur_core::lineage::projection::ExecutorLineage::default(),
        plan_projection: &plans,
        brain_status: &crate::app::BrainStatus::Idle,
        license_badge: None,
        flag_summary: None,
    };
    let key = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT);
    assert!(matches!(
        view.handle_key(key, &ctx),
        Some(Action::NavigateTo(ViewId::PlanInspector(_)))
    ));
}
```

- [ ] **Step 2: Run the `SessionDetail` tests to verify pulse handling is missing**

Run: `cargo test -p spur-tui alt_p_opens_plan_inspector_when_plan_is_tracked -- --nocapture`
Expected: FAIL because `SessionDetail` does not consult `plan_projection`

- [ ] **Step 3: Add the `PlanPulse` component**

```rust
pub fn pulse_text(plan: &spur_core::plan_projection::TrackedPlan) -> String {
    format!(
        "Plan {}  {}  {}/{}  review:{}  fail:{}  next: {}  Alt+P",
        plan.plan_id,
        plan.status_label(),
        plan.reviewed_count(),
        plan.total_tasks(),
        plan.counts.awaiting_review,
        plan.counts.failed,
        plan.next_action_short(),
    )
}
```

- [ ] **Step 4: Wire `SessionDetail` to render and gate on the current tracked plan**

```rust
let tracked_plan = ctx.plan_projection.current_for_session(self.session_id());
if let Some(plan) = tracked_plan {
    render_plan_pulse(frame, header_area, plan);
}
```

```rust
if alt_p_pressed {
    return ctx
        .plan_projection
        .current_for_session(self.session_id())
        .map(|_| Action::NavigateTo(ViewId::PlanInspector(self.session_id().clone())));
}
```

In `react_trace/dispatch.rs`, skip rendering `SessionUpdate::Plan(_)` when a snapshot-backed tracked plan already exists for the same session.

- [ ] **Step 5: Re-run tests and commit**

Run: `cargo test -p spur-tui alt_p_opens_plan_inspector_when_plan_is_tracked -- --nocapture`
Expected: PASS

Run: `cargo test -p spur-tui plan_sets_stream_in_flight -- --nocapture`
Expected: PASS

```bash
git add crates/spur-tui/src/components/plan_pulse.rs crates/spur-tui/src/components/mod.rs crates/spur-tui/src/views/session_detail.rs crates/spur-tui/src/components/react_trace/dispatch.rs
git commit -m "feat(spur-tui): P5 add session detail plan pulse"
```

---

## Task 6: Build the full-screen `PlanInspectorView`

**Task ID:** `plan-inspector-view`

**Files:**
- Modify: `crates/spur-tui/src/views/plan_inspector.rs`
- Create: `crates/spur-tui/src/components/plan_stage_board.rs`
- Create: `crates/spur-tui/src/components/plan_task_detail.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Create: `crates/spur-tui/tests/plan_inspector_snapshot.rs`

**Depends on:** `plan-app-plumbing`

**Acceptance Criteria:**
- [ ] Wide terminals render a two-pane stage-lane board with selected-task detail
- [ ] Narrow terminals below 90 columns degrade to a stacked grouped-list layout
- [ ] Selected tasks show durable beads-backed state plus live executor overlay joined by `issue_id`
- [ ] `Alt+P` and `Esc` both return to `SessionDetail`

**Suggested Worker:** `kiro`

**Scope Boundary:**
- IN scope: inspector rendering, selection state, keyboard navigation, responsive fallback, lineage overlay chips
- OUT of scope: review buttons, merge actions, Mermaid graph mode, dashboard-side plan browsing
- If the lane-board layout cannot stay stable below 90 columns without adding a new dependency, emit `risk` instead of forcing graph drawing

**Implementation:**
- [ ] **Step 1: Write the failing render and key-navigation tests**

```rust
#[test]
fn plan_inspector_renders_wide_lane_board() {
    let backend = TestBackend::new(120, 32);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let ctx = test_ctx_with_plan(sample_tracked_plan());

    terminal.draw(|frame| view.render(frame, frame.area(), &ctx)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    assert!(buffer_to_string(&buffer).contains("Stage 0"));
    assert!(buffer_to_string(&buffer).contains("Task detail"));
}
```

```rust
#[test]
fn plan_inspector_renders_stacked_layout_below_90_cols() {
    let backend = TestBackend::new(80, 28);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut view = PlanInspectorView::new(SessionId("brain-1".into()));
    let ctx = test_ctx_with_plan(sample_tracked_plan());

    terminal.draw(|frame| view.render(frame, frame.area(), &ctx)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    assert!(buffer_to_string(&buffer).contains("Selected:"));
    assert!(buffer_to_string(&buffer).contains("Stage 1"));
}
```

- [ ] **Step 2: Run the inspector tests to verify the full UI is not implemented**

Run: `cargo test -p spur-tui plan_inspector_snapshot -- --nocapture`
Expected: FAIL because the view shell has no lane board or detail pane

- [ ] **Step 3: Add the stage-board and detail components**

```rust
pub fn render_stage_board(
    frame: &mut Frame,
    area: Rect,
    plan: &TrackedPlan,
    selected_task_id: &str,
    lineage: &spur_core::lineage::projection::ExecutorLineage,
) {
    // derive per-stage columns, render [QUE]/[RUN]/[REV]/[PAS]/[ERR] cards,
    // and append a live chip when lineage.nodes_for_issue(issue_id) is non-empty
}
```

```rust
pub fn render_task_detail(
    frame: &mut Frame,
    area: Rect,
    task: &TrackedTask,
    live_node: Option<&spur_core::lineage::types::ExecutorNode>,
) {
    // render task id, status, agent, attempt, deps, blocked_by, summary,
    // feedback/error, worker_branch, diff hint, next action
}
```

- [ ] **Step 4: Finish the inspector view behavior**

```rust
match normalize_macos_option(key) {
    KeyEvent { code: KeyCode::Char('h'), .. } | KeyEvent { code: KeyCode::Left, .. } => self.move_lane(-1),
    KeyEvent { code: KeyCode::Char('l'), .. } | KeyEvent { code: KeyCode::Right, .. } => self.move_lane(1),
    KeyEvent { code: KeyCode::Char('j'), .. } | KeyEvent { code: KeyCode::Down, .. } => self.move_task(1),
    KeyEvent { code: KeyCode::Char('k'), .. } | KeyEvent { code: KeyCode::Up, .. } => self.move_task(-1),
    KeyEvent { code: KeyCode::Char('g'), modifiers, .. } if modifiers.is_empty() => self.jump_lane_start(),
    KeyEvent { code: KeyCode::Char('G'), .. } => self.jump_lane_end(),
    KeyEvent { code: KeyCode::Esc, .. } => return Some(Action::NavigateBack),
    KeyEvent { code: KeyCode::Char('p'), modifiers, .. } if modifiers.contains(KeyModifiers::ALT) => {
        return Some(Action::NavigateBack);
    }
    _ => {}
}
```

Render wide mode at `>= 90` columns and stacked grouped-list mode below that threshold.

- [ ] **Step 5: Re-run tests and commit**

Run: `cargo test -p spur-tui plan_inspector_snapshot -- --nocapture`
Expected: PASS

Run: `cargo test -p spur-tui --test plan_inspector_snapshot -- --nocapture`
Expected: PASS

```bash
git add crates/spur-tui/src/views/plan_inspector.rs crates/spur-tui/src/components/plan_stage_board.rs crates/spur-tui/src/components/plan_task_detail.rs crates/spur-tui/src/components/mod.rs crates/spur-tui/tests/plan_inspector_snapshot.rs
git commit -m "feat(spur-tui): P6 add plan inspector view"
```

---

## Dependency Graph

```text
plan-snapshot-contract
+-- plan-snapshot-emission
\-- plan-projection-store
    \-- plan-app-plumbing
        +-- session-detail-plan-pulse
        \-- plan-inspector-view
```

Parallelism notes:
- `plan-snapshot-emission` and `plan-projection-store` can run in parallel after the ACP contract lands.
- `session-detail-plan-pulse` and `plan-inspector-view` can run in parallel after app plumbing lands.

---

## Spec Coverage Check

- Source-of-truth and join model: `plan-snapshot-contract`, `plan-snapshot-emission`, `plan-projection-store`
- `SessionDetail` one-line `PlanPulse`: `session-detail-plan-pulse`
- `Alt+P` toggle / `Esc` back behavior: `plan-app-plumbing`, `session-detail-plan-pulse`, `plan-inspector-view`
- Dedicated full-screen inspector: `plan-inspector-view`
- Responsive stage-lane board instead of literal graph: `plan-inspector-view`
- Trace demotion policy for `SessionUpdate::Plan`: `session-detail-plan-pulse`

No spec sections are left without a corresponding task. The remaining intentional gap is explicit in the spec and plan: this implementation does not add review or merge actions inside the inspector.

---

## Submission Notes

This plan is ready to save and review. In a fully configured SPUR environment, the next step would be:

```text
submit_plan(persist_as_epic=true)
```

with the six task blocks above converted into beads-backed plan tasks.

In this session, the PM backend is not configured, so the plan can be reviewed locally but not submitted to beads from here.

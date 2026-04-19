# Phase 3b — Iceberg Correctness Spine

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the seven structural / mental-model gaps identified by the Iceberg review of the Phase 3a worker-dispatch RCA (`docs/rca/2026-04-19-phase3a-worker-dispatch-failure-modes.md`). Each task collapses one of three shared mental models into explicit structure:

- **MM-1** "Model narration is a trustworthy artifact" → corroborate every worker claim against git / AST / execution.
- **MM-2** "Task contracts are prose; compliance is behavioral" → lift scope into typed `DelegationRequest` fields.
- **MM-3** "Git + async message-passing compose reliably" → make every transition at the boundary observable.

**Architecture:** Additive, boundary-preserving. No tool surface changes except one new optional field on `DelegationRequest` / delegate-* tool schemas (`expected_files: Option<Vec<String>>`, `allows_no_change: Option<bool>`). One new helper in `spur-mcp/src/plan.rs` (`is_dep_satisfied`). One instrumentation event in `spur-acp/src/domain/events.rs` (`DelegationResultDelivery`). One per-repo async mutex in `spur-worktree`. One test-compile fix. Total estimated surface: ~140 LoC across 7 files plus tests.

**Tech Stack:** Rust 2024, tokio, rmcp 1.4, schemars, tracing. No new workspace deps.

**Parent context:** Phase 3a landed `010a8e2..89973b9` (DN-2 / DN-4 / DN-5 / DN-6 + UP-1 / UP-4) on main. This plan implements the RCA's §7 "must-fix" recommendations plus the two "should-fix" items whose cost is low and whose value is high.

---

## Dispatch policy (RCA-derived)

Per RCA §3 evidence (codex: 33% useful-work rate with 2 silent no-ops; gemini: 50% with hallucination + scope creep; claude-code-acp: highest signal/noise):

- **Worker assignment:** `claude-code-acp` for **every** task in this plan.
- **Fan-out cap:** at most **2 parallel dispatches** at any time (RCA §2.4 stash contention). Chain via `depends_on` even when logically independent.
- **Wall-clock budget:** treat any dispatched task as hung after **15 min without commit** (RCA §3.3) and manually cancel + re-dispatch.
- **Reviewer checklist (RCA §7.6) — enforced before any `review_task(approve)`:**
  1. Diff file set is a **subset** of the task's declared `expected_files`.
  2. Summary file list matches the actual diff (MM-1 corroboration).
  3. Tests exercise behavior, not types (anti-pattern: `assert!(matches!(X, X))`).
  4. `cargo test -p <crate>` runs green on the worker branch.

---

## File Structure

Files touched (net):

- `crates/spur-mcp/tests/rmcp_streamable_http.rs` — fix `BrainSessionId` precondition (B7).
- `crates/spur-mcp/src/plan.rs` — add `fn is_dep_satisfied`, replace 3 call sites (B1).
- `crates/spur-core/src/orchestrator.rs` — `candidate_status` gating on `diff.is_some() || allows_no_change` (B2); wrap `finalize` in wall-clock budget; emit `DelegationResultDelivery` around `respond_to.send` (B3).
- `crates/spur-acp/src/domain/delegation.rs` — add optional `allows_no_change: Option<bool>` to `DelegationRequest` + derive on any delegate-* input struct (B2, B5).
- `crates/spur-acp/src/domain/events.rs` — new `DelegationResultDelivery { delegation_id, constructed, send_ok, delivery_latency_ms }` variant (B3); add `last_notification_at` field to existing `WorkerNotification` bookkeeping or new `WorkerLivenessTick` event (B6).
- `crates/spur-worktree/src/manager.rs` — per-repo `tokio::sync::Mutex` guarding `snapshot_brain_state` critical section (B4); retries become inert but retained for defence.
- `crates/spur-mcp/src/server.rs` — expose `last_notification_at` in `check_delegation_status` (B6).
- `crates/spur-mcp/src/tools.rs` — extend delegate-* input schemas with optional `expected_files` and `allows_no_change` (B2, B5).
- Tests: one integration test per task; one shared helper for the reviewer file-set check (B5).

Out of scope (explicit non-goals):
- God-file decomposition (Phase 3c).
- CancelMode integration (Phase 3c).
- Brain-side breaking changes (UP-2 / UP-3, Phase 3d).
- Protocol-level ACP heartbeat (deferred — B6 uses the already-present `WorkerNotification` stream as a de-facto liveness signal).

---

### Task B7: Fix `rmcp_streamable_http.rs` `BrainSessionId` compile break — **precondition for all others**

**Files:**
- Modify: `crates/spur-mcp/tests/rmcp_streamable_http.rs:1-11`

**Expected files allow-list:** `crates/spur-mcp/tests/rmcp_streamable_http.rs`

**Context:** Post-INV-2 merge `010a8e2` changed `McpCallbackServer::new` to take `&BrainSessionId` instead of `&SessionId`. The test constructs a raw `SessionId` and passes it, so the spur-mcp test binary fails to compile, which gates every other task's `cargo test -p spur-mcp` run.

**Goal:** One-line wrap so the test binary compiles.

- [ ] **Step 1: Confirm the failure**

Run `cargo check -p spur-mcp --tests`. Expect:

```
error[E0308]: mismatched types
   --> crates/spur-mcp/tests/rmcp_streamable_http.rs:11:48
    |
 11 |     let (mut server, _channel) = McpCallbackServer::new(&session_id, None, None);
    |                                                         ^^^^^^^^^^^ expected `&BrainSessionId`, found `&SessionId`
```

- [ ] **Step 2: Apply fix**

Wrap `SessionId` in `BrainSessionId` and use that reference:

```rust
use spur_acp::{BrainSessionId, SessionId};

#[tokio::test]
async fn rmcp_client_can_initialize_list_tools_and_call_tool(
) -> Result<(), Box<dyn std::error::Error>> {
    let session_id = SessionId::new();
    let brain_sid = BrainSessionId::from(session_id.clone());
    let (mut server, _channel) = McpCallbackServer::new(&brain_sid, None, None);
    // ... rest unchanged
```

If `BrainSessionId::from(SessionId)` is not a defined `From` impl, use whatever constructor `BrainSessionId` exposes (grep for `impl BrainSessionId` or existing call sites in `server.rs`). Do not invent a new constructor.

- [ ] **Step 3: Verify**

```bash
cargo check -p spur-mcp --tests   # must succeed
cargo test  -p spur-mcp --test rmcp_streamable_http  # must pass
```

**Review criteria:**
- Diff is exactly the test file (1 import line + 1-2 body lines).
- No production code touched.
- `cargo test -p spur-mcp` reaches plan tests (previously blocked by this file).

**Dependencies:** none. This runs first.

---

### Task B1: Unify `is_dep_satisfied` across three scheduler sites (RCA §4.5)

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs:559` (run_plan schedule)
- Modify: `crates/spur-mcp/src/plan.rs:897-898` (build_plan_status blocked_by)
- Modify: `crates/spur-mcp/src/plan.rs:2036` (dispatch_newly_ready)
- Test: `crates/spur-mcp/tests/plan_is_dep_satisfied.rs` (new)

**Expected files allow-list:**
- `crates/spur-mcp/src/plan.rs`
- `crates/spur-mcp/tests/plan_is_dep_satisfied.rs`

**Context:** RCA §4.5 confirms three inline `matches!` predicates answering the same question — "is this dep satisfied?" — but one site (`run_plan:559`) still says Approved-only while the other two say `Approved | Cancelled`. A downstream task behind a Cancelled dep is reported as non-blocked by `get_plan_status` but never promoted by `run_plan`'s initial scheduling pass.

**Semantic:** `Approved | Cancelled` is the intended post-DN-4 semantic. `run_plan:559` is the missed update.

- [ ] **Step 1: Write the failing test**

```rust
// crates/spur-mcp/tests/plan_is_dep_satisfied.rs
//! B1: run_plan's initial schedule must treat Cancelled deps as satisfied,
//! matching dispatch_newly_ready / build_plan_status.

use spur_mcp::plan::{PlanState, PlanTask, PlanTaskStatus, run_plan, /* ... */};
// ... construct a plan with t1 Cancelled, t2 Pending depends_on [t1]
// ... spawn run_plan, assert t2 transitions to Dispatched within a short timeout,
//     not stuck in Pending.

#[tokio::test]
async fn run_plan_dispatches_task_with_cancelled_dep() {
    // Setup: PlanState with t1: Cancelled{reason: "brain cancel"}, t2: Pending depends_on=[t1]
    // Spawn run_plan with a mock delegation_tx that records DelegationRequests.
    // Assert: within 2s, a request for t2 is observed (t2 must have been promoted to Dispatched).
    // On current main, this times out because run_plan:559 refuses Cancelled.
}
```

The test must drive `run_plan`, not `dispatch_newly_ready`, and must assert via a side-effect (a captured DelegationRequest) — NOT by `matches!(status, Dispatched{..})` on the status alone, which would pass even if only `dispatch_newly_ready` did the work (see anti-pattern in RCA §2.3).

- [ ] **Step 2: Commit red**

```
git commit -m "test(spur-mcp): B1 — run_plan must treat Cancelled dep as satisfied (RED)"
```

- [ ] **Step 3: Introduce helper and replace all three sites**

```rust
// at top of plan.rs, near PlanTaskStatus definition
#[inline]
pub(crate) fn is_dep_satisfied(status: &PlanTaskStatus) -> bool {
    matches!(status, PlanTaskStatus::Approved { .. } | PlanTaskStatus::Cancelled { .. })
}
```

Replace:
- `plan.rs:559`: `matches!(t.status, PlanTaskStatus::Approved { .. })` → `is_dep_satisfied(&t.status)`
- `plan.rs:895-899` (blocked_by): inline match on `o.status` → `is_dep_satisfied(&o.status)`
- `plan.rs:2034-2038` (dispatch_newly_ready ready_ids): inline match → `is_dep_satisfied(&o.status)`

- [ ] **Step 4: Verify**

```bash
cargo test -p spur-mcp plan_is_dep_satisfied  # green
cargo test -p spur-mcp plan                    # all plan tests green
grep -n "matches!(.*PlanTaskStatus::Approved" crates/spur-mcp/src/plan.rs
# must return zero hits outside the helper definition
```

**Review criteria:**
- Exactly one definition of `is_dep_satisfied` exists.
- Three sites replaced. Final `grep` returns 0 matches on the old pattern outside the helper.
- Test fails on HEAD, passes after the change (red-then-green).

**Dependencies:** B7.

---

### Task B2: `candidate_status` gating by observed diff + `allows_no_change` contract (RCA §2.5)

**Files:**
- Modify: `crates/spur-acp/src/domain/delegation.rs` — add `allows_no_change: Option<bool>` to `DelegationRequest` (serde `#[serde(default)]`).
- Modify: `crates/spur-mcp/src/tools.rs` — add optional `allows_no_change` to delegate-* input schemas; forward into `DelegationRequest`.
- Modify: `crates/spur-core/src/orchestrator.rs:3860` — gate the `Success` construction.
- Test: `crates/spur-core/tests/candidate_status_requires_diff.rs` (new)

**Expected files allow-list:**
- `crates/spur-acp/src/domain/delegation.rs`
- `crates/spur-mcp/src/tools.rs`
- `crates/spur-core/src/orchestrator.rs`
- `crates/spur-core/tests/candidate_status_requires_diff.rs`

**Context:** RCA §2.5 confirms at `orchestrator.rs:3860`:

```rust
let candidate_status = if worker_success { DelegationStatus::Success } else { ... };
```

`worker_success` only flips false on ACP transport error. A clean session that produced zero commits is classified `Success`. For tasks contracted to produce code, this is a silent-no-op failure (observed twice with codex in Phase 3a).

The `get_task_diff` "legitimate no-change" pathway (docs/rca/2026-04-18-get-task-diff-empty.md) must be preserved — some tasks (investigation, verification) legitimately produce no diff. Therefore the gating is **contract-opt-in**, not a blanket hard-fail.

**Semantic:**
- If `allows_no_change == Some(true)`: retain current behavior (`Success` even with null diff).
- Else (`None` or `Some(false)`): `Success` requires `diff.is_some()`. Otherwise `Failed { error: "worker exited without producing expected changes (suspected silent no-op)" }`.

- [ ] **Step 1: Write the failing test**

```rust
// crates/spur-core/tests/candidate_status_requires_diff.rs
//! B2: a worker exiting with clean ACP but zero diff is classified Failed
//! when allows_no_change is false/absent, and Success when allows_no_change=true.

// Two test cases via a minimal orchestrator harness:
//   case A: allows_no_change=None, diff=None ⇒ DelegationStatus::Failed { error contains "silent no-op" }
//   case B: allows_no_change=Some(true), diff=None ⇒ DelegationStatus::Success
```

Use whatever test harness pattern already exists for `orchestrator.rs` (grep for `tests/` files importing from `spur_core::orchestrator::test_support`).

- [ ] **Step 2: Commit red**

- [ ] **Step 3: Implement**

Add to `DelegationRequest`:

```rust
#[serde(default)]
pub allows_no_change: Option<bool>,
```

In `orchestrator.rs` near line 3860:

```rust
let candidate_status = if !worker_success {
    // unchanged: transport-error path
    DelegationStatus::Failed { error: /* existing tail-500-byte extraction */ }
} else if diff.is_none() && !ctx.request.allows_no_change.unwrap_or(false) {
    DelegationStatus::Failed {
        error: format!(
            "worker exited without producing expected changes \
             (suspected silent no-op); if this task legitimately makes no \
             code changes, set allows_no_change=true on the DelegationRequest"
        ),
    }
} else {
    DelegationStatus::Success
};
```

Adjust the exact field accessor path to match current `ctx` shape.

Extend delegate-* tool schemas in `tools.rs` with `allows_no_change: Option<bool>` fields where `DelegationRequest` is constructed. Plumb through `submit_plan` task specs if reachable (search for `DelegationRequest { ...` construction sites).

- [ ] **Step 4: Verify**

```bash
cargo test -p spur-core candidate_status_requires_diff
cargo test -p spur-acp --all
cargo test -p spur-mcp --all
```

**Review criteria:**
- `allows_no_change: Option<bool>` serializes correctly (absent field deserializes to `None`, backward-compatible with pre-B2 plans).
- The RCA §2.5 reproducer (zero-commit + Success) now produces `Failed` with the exact error string above.
- Legitimate no-change path (with `allows_no_change=true`) still returns `Success`.
- No existing test breaks because of changed defaults.

**Dependencies:** B7.

---

### Task B3: Instrument `respond_to.send` delivery + finalize wall-clock budget (RCA §2.6, §4.1)

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs` — add `DelegationResultDelivery { delegation_id, constructed_at, send_ok, delivery_latency_ms, delivery_error: Option<String> }` to `SpurEventBody`.
- Modify: `crates/spur-core/src/orchestrator.rs` — at every `respond_to.send` / `tx.send(DelegationResult...)` site (known sites: `:2540, :2623, :2689, :2709, :3344`), emit `DelegationResultDelivery`. Wrap the interval from `DelegationResult` construction to send in a wall-clock measurement.
- Modify: `crates/spur-core/src/orchestrator.rs` finalize path (lines ~3810-3920) — wrap diff-collection + diff_summary in a `tokio::time::timeout(Duration::from_secs(90), ...)`; on elapsed, emit `Failed { error: "finalize exceeded budget" }` and DelegationResultDelivery with `send_ok=false, delivery_error=Some("finalize timeout")`.
- Test: `crates/spur-core/tests/delegation_result_delivery_observed.rs` (new)

**Expected files allow-list:**
- `crates/spur-acp/src/domain/events.rs`
- `crates/spur-core/src/orchestrator.rs`
- `crates/spur-core/tests/delegation_result_delivery_observed.rs`

**Context:** RCA §2.6 (post-merge corrected) narrows the blind spot to: DN-5 v2 worker-branch had the commit, `rx.await` neither resolved nor dropped, so the orchestrator's worker-spawn task was holding `respond_to` open while wedged somewhere between worker exit and `finalize` / `respond_to.send`. No lineage event records this.

Five `DelegationResult` construction sites exist (orchestrator.rs:2540, 2689, 2709, 3344 guard-Drop, 3377 `finalize`). Only `2623` uses error-handled send (`cleanup_cancelled_review`); `3344` uses `let _ = tx.send(...)` (intentional: Drop can't recover, but the silence is load-bearing).

**Goal:** Every `DelegationResult` construction is followed by an observable `DelegationResultDelivery` event, including the Drop-guard path. Plus a bounded finalize that can't hang indefinitely.

- [ ] **Step 1: Write the failing test**

```rust
// crates/spur-core/tests/delegation_result_delivery_observed.rs
//! B3: every DelegationResult construction emits a DelegationResultDelivery
//! event, whether the send succeeded, failed, or the sender was dropped.

// Test case 1 (happy path):
//   dispatch a minimal delegation, let it complete, assert exactly one
//   DelegationResultDelivery event with send_ok=true.
//
// Test case 2 (receiver dropped):
//   dispatch, drop rx before send completes, assert DelegationResultDelivery
//   with send_ok=false, delivery_error=Some("receiver dropped").
//
// Test case 3 (finalize timeout):
//   dispatch with a synthetic stalling worktree hook (if injectable) or
//   simulate via a shorter timeout — assert DelegationResultDelivery with
//   delivery_error=Some("finalize timeout").
```

If case 3 requires harness surgery beyond reasonable scope, gate it behind `#[cfg(feature = "...")]` or leave as an ignored test and document why.

- [ ] **Step 2: Commit red**

- [ ] **Step 3: Implement**

Add to `SpurEventBody` in `spur-acp/src/domain/events.rs`:

```rust
DelegationResultDelivery {
    delegation_id: String,
    constructed_at_ms: u64,         // epoch ms when DelegationResult struct was built
    delivery_latency_ms: u64,       // from construction to send attempt
    send_ok: bool,
    delivery_error: Option<String>, // None on send_ok=true
},
```

In `orchestrator.rs`, introduce a small helper:

```rust
#[inline]
fn emit_delivery(
    funnel: &FunnelHandle,
    delegation_id: &str,
    constructed_at: std::time::Instant,
    send_result: Result<(), /* returned result or error */>,
) {
    // compute latency, emit DelegationResultDelivery
}
```

Call at each of the five construction sites. For the Drop guard at :3344, emit before `let _ = tx.send(...)`. For the normal path at :2623, emit inside both the Ok and Err arms.

Wrap the finalize interval:

```rust
let finalize_fut = async {
    // existing diff collection + diff_summary + finalize call
};
match tokio::time::timeout(Duration::from_secs(90), finalize_fut).await {
    Ok(result) => result,
    Err(_elapsed) => DelegationResult {
        status: DelegationStatus::Failed {
            error: "finalize exceeded 90s budget".into(),
        },
        diff: None, diff_summary: None, summary: None,
        estimated_cost_usd: 0.0, worker_branch: None,
    },
}
```

- [ ] **Step 4: Verify**

```bash
cargo test -p spur-core delegation_result_delivery_observed
cargo test -p spur-core --all
cargo test -p spur-acp --all
```

**Review criteria:**
- Exactly 5 `emit_delivery` call sites, corresponding 1:1 to the 5 DelegationResult constructions.
- Finalize wraps in `tokio::time::timeout` with documented 90s budget.
- Event schema changes preserve serde backward-compat (existing consumers ignoring unknown variants still work — confirm via `deny_unknown_fields` not set on consumer side).

**Dependencies:** B7. (Should not depend on B2, but if diff merge-conflicts with B2 at `orchestrator.rs:3860`, resolve manually.)

---

### Task B4: Per-repo snapshot mutex in `SpurWorktreeManager` (RCA §2.4)

**Files:**
- Modify: `crates/spur-worktree/src/manager.rs:75-141`
- Test: `crates/spur-worktree/tests/snapshot_serializes_under_contention.rs` (new)

**Expected files allow-list:**
- `crates/spur-worktree/src/manager.rs`
- `crates/spur-worktree/tests/snapshot_serializes_under_contention.rs`

**Context:** RCA §2.4 grounded at `manager.rs:92-111`: retry loop is **3 attempts, fixed backoff 50/100 ms, no jitter**. Parallel dispatches (N≥3) collide on `index.lock` and at least one loses by pigeonhole. First-principles fix: git's index is a whole-repo single-writer resource; the snapshot critical section must serialize at the application layer rather than rely on git's per-call retry.

**Semantic:**
- Add a `snapshot_mu: Arc<tokio::sync::Mutex<()>>` field to `SpurWorktreeManager` (or a global per-repo-path static map if manager is constructed per-call).
- Acquire the mutex around the whole `stash create` → `rev-parse` → `commit-tree` → `branch` sequence.
- Keep the existing 3-attempt retry loop as defence in depth (it should now rarely fire).

- [ ] **Step 1: Write the failing test**

```rust
// crates/spur-worktree/tests/snapshot_serializes_under_contention.rs
//! B4: N parallel snapshot_brain_state calls all succeed; zero exhausted retries.

#[tokio::test]
async fn parallel_snapshots_all_succeed() {
    // Setup: tempdir git repo with a dirty tracked file.
    // Spawn 8 concurrent snapshot_brain_state() calls via try_join_all.
    // Assert: all 8 return Ok, and all returned branch names are distinct.
    // On current main, at least one returns
    //   "failed to create stash after retries" under high load.
}
```

This test must drive ≥4 concurrent calls to reproduce the original failure mode.

- [ ] **Step 2: Commit red**

- [ ] **Step 3: Implement**

```rust
use tokio::sync::Mutex;

pub struct SpurWorktreeManager {
    // ... existing fields
    snapshot_mu: Arc<Mutex<()>>,
}

impl SpurWorktreeManager {
    pub async fn snapshot_brain_state(&self) -> Result<String> {
        let _guard = self.snapshot_mu.lock().await;
        // ... existing body unchanged
    }
}
```

If `SpurWorktreeManager` is constructed per-call (grep `SpurWorktreeManager::new`), promote `snapshot_mu` to a module-level `OnceLock<Mutex<()>>` or a per-repo-path `DashMap<PathBuf, Arc<Mutex<()>>>`. Choose whichever matches existing ownership patterns.

- [ ] **Step 4: Verify**

```bash
cargo test -p spur-worktree snapshot_serializes_under_contention
cargo test -p spur-worktree --all
```

**Review criteria:**
- Test fails on HEAD (original 3-attempt retry insufficient under 8-way parallelism).
- Test passes after the mutex.
- Critical section actually covers `stash create` onward — not just `stash create` alone.
- If module-level mutex chosen, document why it's keyed the way it is (single-repo assumption vs multi-repo).

**Dependencies:** B7.

---

### Task B6: Expose `last_notification_at` for worker liveness (RCA §4.4)

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:3788` — in the `funnel.emit(WorkerNotification)` closure, also update a per-delegation `last_notification_at: Arc<DashMap<String, Instant>>` (or similar).
- Modify: `crates/spur-mcp/src/server.rs` — `check_delegation_status` response includes `last_notification_at_ms: Option<i64>` (epoch-ms Unix timestamp, None if delegation unknown to notification map).
- Modify: `crates/spur-mcp/src/server.rs` `active_delegations` / `completed_delegations` — needs access to the shared map; either wire via new Arc field on `McpCallbackServer` or a shared registry.
- Test: `crates/spur-mcp/tests/liveness_last_notification_at.rs` (new)

**Expected files allow-list:**
- `crates/spur-core/src/orchestrator.rs`
- `crates/spur-mcp/src/server.rs`
- `crates/spur-mcp/tests/liveness_last_notification_at.rs`

**Context:** RCA §4.4 — no heartbeat/liveness signal anywhere. The `WorkerNotification` stream is already a de-facto liveness signal; it just isn't timestamped per delegation observable from outside orchestrator. A 15-min-with-no-notification state is unambiguously hung; 15-min-with-recent-AgentThoughtChunks is mid-thinking.

**Semantic:** Best-effort. If the notification map isn't wired all the way through MCP, return `None` — do not invent a new value.

- [ ] **Step 1: Write the failing test**

```rust
// crates/spur-mcp/tests/liveness_last_notification_at.rs
//! B6: check_delegation_status reports last_notification_at_ms that advances
//! as WorkerNotifications are emitted.

// Dispatch a delegation against a stub worker that emits WorkerNotifications
// at 100ms intervals. After 200ms, poll check_delegation_status twice with a
// 150ms gap. Assert: both responses have Some(ms), and the second is strictly
// greater than the first.
```

- [ ] **Step 2: Commit red**

- [ ] **Step 3: Implement**

Introduce a shared `DashMap<String /* delegation_id */, i64 /* epoch ms */>` owned by the MCP server. Pass a handle to the orchestrator at dispatch time (through the existing `DelegationRequest` plumbing or a new `AsyncLivenessSink` trait). In the `funnel.emit(WorkerNotification { ... })` call, also call `liveness_sink.tick(&delegation_id)`.

In `check_delegation_status` response, add:

```rust
"last_notification_at_ms": self.liveness.get(&request_id).map(|e| *e.value()),
```

- [ ] **Step 4: Verify**

```bash
cargo test -p spur-mcp liveness_last_notification_at
cargo test -p spur-mcp --all
```

**Review criteria:**
- `last_notification_at_ms` is present in `check_delegation_status` JSON output (nullable).
- Test asserts monotonic advance, not just presence.
- If the worker notification stream is empty (native ACP workers), the value may remain `None` until the first tick — document this behavior in a comment.

**Dependencies:** B7.

---

### Task B5: `expected_files` allow-list contract + reviewer corroboration (RCA §2.2, §2.3)

**Files:**
- Modify: `crates/spur-acp/src/domain/delegation.rs` — add `expected_files: Option<Vec<String>>` to `DelegationRequest`.
- Modify: `crates/spur-mcp/src/tools.rs` — add to delegate-* input schemas.
- Modify: `crates/spur-mcp/src/plan.rs` — `submit_plan` task spec accepts and forwards `expected_files`.
- Modify: `crates/spur-mcp/src/server.rs` review_task helper or plan.rs — at review time, compare the diff's file set to `expected_files` if present; on mismatch, return a structured warning to the reviewer (not auto-reject, but **surface**).
- Test: `crates/spur-mcp/tests/expected_files_mismatch_warning.rs` (new)

**Expected files allow-list:**
- `crates/spur-acp/src/domain/delegation.rs`
- `crates/spur-mcp/src/tools.rs`
- `crates/spur-mcp/src/plan.rs`
- `crates/spur-mcp/src/server.rs`
- `crates/spur-mcp/tests/expected_files_mismatch_warning.rs`

**Context:** RCA §2.2 — gemini's DN-2 attempt 2 produced a 6-file diff on a 3-file-scope task with no mechanical check. Scope was prose; compliance was behavioral (MM-2). The fix is to lift scope into a typed `expected_files: Vec<String>` and have the reviewer path verify that the diff's file set is a **subset** (superset is also fine — worker touching fewer files than expected is not a scope violation).

**Semantic:** Warning-only in this phase, not hard-rejection. Auto-rejection risks false-positives (path normalization, newly-created test file the task contract forgot). Escalation to hard-rejection is a Phase 3c follow-up after real-world calibration.

- [ ] **Step 1: Write the failing test**

```rust
// crates/spur-mcp/tests/expected_files_mismatch_warning.rs
//! B5: when diff touches files outside expected_files, review response
//! includes "scope_warnings" listing the offending paths.

// Construct a minimal delegation with expected_files=["src/a.rs"] and a
// mock diff touching "src/a.rs" AND "src/b.rs". Invoke the review path
// (whatever the existing review helper is) and assert the response contains
// a non-empty scope_warnings: Vec<String>.
```

- [ ] **Step 2: Commit red**

- [ ] **Step 3: Implement**

Add to `DelegationRequest`:

```rust
#[serde(default)]
pub expected_files: Option<Vec<String>>,
```

In plan.rs / server.rs review path, after diff collection:

```rust
if let Some(ref expected) = req.expected_files {
    let expected_set: HashSet<&str> = expected.iter().map(|s| s.as_str()).collect();
    let actual: Vec<&str> = diff_summary.files_touched.iter()
        .map(|s| s.as_str())
        .filter(|p| !expected_set.contains(p))
        .collect();
    if !actual.is_empty() {
        review_response.scope_warnings = actual.into_iter().map(String::from).collect();
    }
}
```

Path normalization note: compare canonicalized relative paths from repo root. If the worker uses absolute paths anywhere, normalize both sides before the subset check.

- [ ] **Step 4: Verify**

```bash
cargo test -p spur-mcp expected_files_mismatch_warning
cargo test -p spur-mcp --all
```

**Review criteria:**
- `expected_files: Option<Vec<String>>` present on `DelegationRequest` with `#[serde(default)]` for backward compatibility.
- When field is `None`, no check runs (backward-compat).
- When present and diff matches subset: no warning.
- When present and diff exceeds: warning surfaced — but review does **not** auto-fail. Brain decides.
- Path normalization works for both repo-relative and absolute paths.

**Dependencies:** B7.

---

## Dependency DAG and dispatch order

```
B7 (precondition)
  ├→ B1 (is_dep_satisfied unification)
  ├→ B2 (candidate_status diff gating)
  ├→ B3 (delivery instrumentation + finalize budget)
  ├→ B4 (snapshot serialization mutex)
  ├→ B6 (last_notification_at liveness)
  └→ B5 (expected_files scope contract)
```

All six post-B7 tasks are logically independent. To respect RCA §2.4's stash-contention finding, dispatch them in pairs (at most 2 parallel):

- Batch 1: B7 alone.
- Batch 2: B1 + B2.
- Batch 3: B3 + B4.
- Batch 4: B6 + B5.

The `depends_on` chain encodes this: B1..B5, B6 all `depends_on: ["B7"]`; additional chaining between batches is handled by the dispatcher via fan-out cap (not required in the plan graph).

---

## Integration strategy

After all 7 tasks approve:

1. Run full workspace tests: `cargo test` (nothing crate-specific — must pass across spur-acp, spur-core, spur-mcp, spur-worktree).
2. Cherry-pick each worker branch onto a `phase3b-integration` branch in dependency order (B7 first). Resolve trivial conflicts (likely only in `orchestrator.rs` if B2 and B3 both touch the finalize region).
3. Squash-merge `phase3b-integration` into main with a single merge commit; the individual task commits remain visible via the merge's first-parent history.
4. Update the RCA file with a closing addendum noting which MM layers are now structurally addressed vs still open.

---

## Success criteria

- `cargo test` passes workspace-wide (baseline: 368 tests → 368+ with B1–B7 additions).
- `grep -n "matches!(.*PlanTaskStatus::Approved" crates/spur-mcp/src/plan.rs` returns zero hits outside the `is_dep_satisfied` definition.
- The RCA §2.5 reproducer (clean ACP + zero commits + `allows_no_change=None`) produces `DelegationStatus::Failed`, not `Success`.
- Every `DelegationResult` construction has a paired `DelegationResultDelivery` lineage event.
- 8-way parallel `snapshot_brain_state` succeeds without retries exhausting.
- `check_delegation_status` JSON includes `last_notification_at_ms`.
- Reviewer path surfaces `scope_warnings` when diff escapes `expected_files`.

---

## Non-goals (explicit)

- Hard-reject on `expected_files` violation (B5 is warning-only this phase).
- Full ACP-protocol heartbeat (B6 reuses the existing notification stream).
- Unifying the 5 `DelegationResult` construction sites into one (B3 instruments them but doesn't refactor — that's a Phase 3c decomposition concern).
- Replacing `let _ = tx.send(...)` at the Drop guard (orchestrator.rs:3344) — the Drop contract makes explicit recovery impossible; B3 adds observability there, not semantics.

---

## Appendix — cross-reference to Iceberg mental models

| Task | Layer-4 mental model collapsed | RCA section |
|------|-------------------------------|-------------|
| B1   | MM-3 duplication-is-cheap     | §4.5        |
| B2   | MM-1 + MM-3: trust + transport=work  | §2.5 |
| B3   | MM-3 implicit plan completion | §2.6, §4.1  |
| B4   | MM-3 parallel-dispatch = parallel-git | §2.4 |
| B5   | MM-2 scope-is-prose            | §2.2, §2.3 |
| B6   | MM-3 hang ≡ thinking           | §4.4        |
| B7   | (enabling)                    | §9          |

End of plan.

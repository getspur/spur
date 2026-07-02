# Loop Engine (Phases 1–3) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the Loop Engine's governors, durable cost/run records, cadence scheduler, and L1 loop lifecycle on top of the existing plan engine, per `docs/superpowers/specs/2026-07-02-loop-engine-design.md` (phases 1–3).

**Architecture:** No new executor. Governors are new `PlanDispatchState` arms in the reconciler's guard choke point; loop identity/memory/trigger state live entirely in beads (labels + `[[spur-loop v1]]` / `loop-run` sentinels); a `LoopScheduler` sweep inside `Reconciler::tick_once` re-arms due loops by pushing a brain continuation (L1/L2). Generations are ordinary plans.

**Tech Stack:** Rust 2021, tokio (paused-clock tests), serde, existing `spur-pm` PmLike/beads substrate, `spur-license` feature gate. Build/test ONLY via `scripts/spur-cargo` (remote-default). All tests: `scripts/spur-cargo test -p spur-core <filter>`.

**Worker ground rules (every task):**
- TDD cadence: `test(...)` commit first (failing), then `fix(...)`/`feat(...)` commit.
- Commit format: `<type>(spur-core): <short imperative>` (subject < 72 chars).
- Never use bare `cargo`. Run `scripts/spur-cargo test -p spur-core` before claiming done; `SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings` for lint.
- Labels must be br-legal `[A-Za-z0-9_:-]+`, ≤ 50 chars at create time.
- New serialized enum variants/fields need round-trip tests (repo convention).

---

## File Structure

| Path | Responsibility | Task |
|---|---|---|
| `crates/spur-core/src/plan/reconciler/mod.rs` | `PlanDispatchState` new arms; wire scheduler sweep into `tick_once` | T1, T3, T6 |
| `crates/spur-core/src/plan/reconciler/guards.rs` | pause / report-only / budget checks in `plan_allows_dispatch` | T1, T3 |
| `crates/spur-core/src/plan/outcomes.rs` | `SkipReason` new variants | T1, T3 |
| `crates/spur-core/src/plan/audit_sentinel.rs` | `estimated_cost_micros` on Completion; new `LoopRun` variant | T2, T5 |
| `crates/spur-core/src/plan/labels.rs` | loop label vocabulary + parsers | T4 |
| `crates/spur-core/src/plan/loops/mod.rs` | module root | T4 |
| `crates/spur-core/src/plan/loops/spec.rs` | `LoopSpec` + `[[spur-loop v1]]` parse/serialize | T4 |
| `crates/spur-core/src/plan/loops/run_record.rs` | run-record computation from epic outcome | T5 |
| `crates/spur-core/src/plan/loops/scheduler.rs` | due-check sweep, overlap skip, re-arm | T6 |
| `crates/spur-core/src/plan/reconciler/terminal.rs` | loop-run emission hook on `EpicCompletion` | T5 |
| `crates/spur-core/src/mcp/plan.rs` + `crates/spur-core/src/server/handlers/plan.rs` | `submit_loop`/`get_loop_status`/`pause_loop`/`resume_loop` tools | T7 |
| `crates/spur-core/tests/loop_engine_governors.rs` | integration tests for T1/T3 | T1, T3 |
| `crates/spur-core/tests/loop_scheduler.rs` | integration tests for T6 | T6 |

Task DAG: T1, T2, T4 independent → T3 needs T1+T2 → T5 needs T2+T4 → T6 needs T4+T5 → T7 needs T4+T6.

---

### Task T1: Kill switch + report-only dispatch states

**Files:**
- Modify: `crates/spur-core/src/plan/outcomes.rs:48` (`SkipReason`)
- Modify: `crates/spur-core/src/plan/reconciler/mod.rs:818` (`PlanDispatchState`)
- Modify: `crates/spur-core/src/plan/reconciler/guards.rs:8` (`plan_allows_dispatch`)
- Modify: `crates/spur-core/src/plan/labels.rs` (three constants)
- Test: `crates/spur-core/src/plan/reconciler/tests.rs`

- [ ] **Step 1: Add label constants (no test needed — constants only)**

In `labels.rs` next to `PLAN_COMPLETE` (`labels.rs:186`):

```rust
pub const LOOP_PAUSED: &str = "spur:loop-paused";
pub const PAUSE_ALL_LOOPS: &str = "spur:pause-all-loops";
pub const AUTONOMY_PREFIX: &str = "spur:autonomy:";

/// Parses `spur:autonomy:l1|l2|l3`. Unknown suffixes return None.
pub fn parse_autonomy(label: &str) -> Option<AutonomyLevel> {
    match label.strip_prefix(AUTONOMY_PREFIX)? {
        "l1" => Some(AutonomyLevel::L1),
        "l2" => Some(AutonomyLevel::L2),
        "l3" => Some(AutonomyLevel::L3),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AutonomyLevel { L1, L2, L3 }
```

- [ ] **Step 2: Write failing guard tests**

In `plan/reconciler/tests.rs`, following the existing mock-PM test pattern in that file (reuse its helpers for building a Reconciler over a mock `PmLike`):

```rust
#[tokio::test(start_paused = true)]
async fn epic_with_loop_paused_label_suppresses_dispatch() {
    // Arrange: plan epic labeled spur:plan-complete + spur:loop-paused,
    // one ready task. Act: tick_once. Assert: no delegation sent; skip
    // recorded with SkipReason::LoopsPaused { scope: "loop" }.
}

#[tokio::test(start_paused = true)]
async fn l1_autonomy_suppresses_non_triage_tasks() {
    // Arrange: epic labeled spur:autonomy:l1; task A labeled
    // spur:loop-triage-task, task B unlabeled, both Ready.
    // Act: tick_once. Assert: A dispatched, B skipped ReportOnly.
}

#[tokio::test(start_paused = true)]
async fn l2_autonomy_dispatches_all_ready_tasks() {
    // Same arrangement with spur:autonomy:l2 → both dispatch.
}
```

Fill the bodies concretely against the existing helpers (see neighbouring tests such as the lease-sweep tests in the same file for the arrange/act/assert idiom — mock PM issue construction, `Reconciler::tick_once`, dispatch-channel assertion).

- [ ] **Step 3: Run tests, verify they fail**

Run: `scripts/spur-cargo test -p spur-core reconciler::tests::epic_with_loop_paused`
Expected: FAIL (compile error — variants don't exist yet).

- [ ] **Step 4: Commit failing tests**

`test(spur-core): loop pause and report-only guard suppression`

- [ ] **Step 5: Add enum variants**

`outcomes.rs` `SkipReason` (extend enum at line 48; keep serde attrs consistent with existing variants):

```rust
    LoopsPaused { scope: String },          // "loop" | "global"
    ReportOnly,
```

`reconciler/mod.rs` `PlanDispatchState` (line 818) + `skip_reason()` arms:

```rust
    LoopsPaused { epic_id: String, scope: String },
    ReportOnly { epic_id: String },
```

```rust
    Self::LoopsPaused { scope, .. } => Some(SkipReason::LoopsPaused { scope: scope.clone() }),
    Self::ReportOnly { .. } => Some(SkipReason::ReportOnly),
```

- [ ] **Step 6: Implement guard checks**

In `plan_allows_dispatch` (`guards.rs`), inside the epic loop after the ownership check passes and before `open_complete_epic` is set: if the epic (or its plan-level labels) carries `labels::LOOP_PAUSED`, return `LoopsPaused { epic_id, scope: "loop".into() }` (cache + return, same shape as the ownership early-returns). Global pause: at fn entry, consult a new `ReconcilerConfig` flag `pause_all_loops: bool` (add to `ReconcilerConfig` with `false` default) OR presence of `PAUSE_ALL_LOOPS` on the epic — check config first (cheap), label second.

`ReportOnly` is per-task, not per-plan, so it cannot come from `plan_allows_dispatch` alone: in `tick_once` (`reconciler/mod.rs`, right after the `task.status != Ready` check), if the epic's labels carry `spur:autonomy:l1` (thread the projected plan's epic labels through — `projected` already carries them) and the task's issue labels do NOT include `spur:loop-triage-task`, call `self.record_skipped(Some(plan_id), &task.spec.task_id, SkipReason::ReportOnly)` and `continue`. Add label constant `pub const LOOP_TRIAGE_TASK: &str = "spur:loop-triage-task";` in `labels.rs`.

- [ ] **Step 7: Run tests, verify pass; run crate test suite**

Run: `scripts/spur-cargo test -p spur-core reconciler`
Expected: PASS, no regressions.

- [ ] **Step 8: Commit**

`feat(spur-core): loop pause and report-only dispatch governors`

---

### Task T2: Durable cost on Completion audit sentinel

**Files:**
- Modify: `crates/spur-core/src/plan/audit_sentinel.rs:35` (`CompletionAuditFields`), `:71` (Completion variant)
- Modify: `crates/spur-core/src/plan/mod.rs:1677` (`emit_completion_audit`) and its callers (grep `CompletionAuditFields {` — update struct literals)
- Test: audit sentinel round-trip tests in `audit_sentinel.rs` `#[cfg(test)]` module

- [ ] **Step 1: Write failing round-trip test**

```rust
#[test]
fn completion_sentinel_roundtrips_cost_micros() {
    let kind = AuditSentinelKind::Completion {
        delegation_id: "del-A".into(),
        completion_state: CompletionState::Completed,
        superseded: false,
        worker_branch: Some("w/b".into()),
        result_summary: None,
        artifact_uri: None,
        dispatched_base_oid: None,
        estimated_cost_micros: Some(812_000),
    };
    let text = kind.to_comment_body();       // use the module's existing serialize fn name
    let parsed = parse_audit_sentinel(&text).unwrap();
    assert_eq!(parsed, kind);
}

#[test]
fn completion_sentinel_without_cost_field_still_parses() {
    // Take an existing serialized Completion fixture string from the current
    // tests (pre-change format, no estimated_cost_micros key) and assert it
    // parses with estimated_cost_micros == None.
}
```

Match the module's actual serialize/parse function names (read the existing tests in the same file first).

- [ ] **Step 2: Run, verify fail (compile error)**

Run: `scripts/spur-cargo test -p spur-core audit_sentinel`

- [ ] **Step 3: Commit failing test** — `test(spur-core): completion sentinel cost round-trip`

- [ ] **Step 4: Add the field**

`CompletionAuditFields` gets `pub estimated_cost_micros: Option<u64>` with `#[serde(default, skip_serializing_if = "Option::is_none")]` (mirror the attrs used on `artifact_uri`); same optional field on the `Completion` enum variant. Populate at the emission site: in the reconciler completion path the value comes from `DelegationResult.estimated_cost_usd` converted via the existing `usd_to_micros_saturating` (`outcome_materializer.rs:558` — make it `pub(crate)` if needed). Update every `CompletionAuditFields { .. }` literal (compiler will list them).

- [ ] **Step 5: Run tests + full crate suite; commit**

Run: `scripts/spur-cargo test -p spur-core`
`feat(spur-core): persist delegation cost on completion audit`

---

### Task T3: Budget-exhausted dispatch gate (needs T1, T2)

**Files:**
- Modify: `crates/spur-core/src/plan/reconciler/guards.rs`, `crates/spur-core/src/plan/reconciler/mod.rs`, `crates/spur-core/src/plan/outcomes.rs`
- Create: helper `plan_spent_micros` in `crates/spur-core/src/plan/projector.rs`
- Test: `crates/spur-core/src/plan/reconciler/tests.rs`

- [ ] **Step 1: Failing test**

```rust
#[tokio::test(start_paused = true)]
async fn budget_exhausted_plan_suppresses_dispatch() {
    // Arrange: epic labeled spur:plan-complete + spur:loop-budget-micros:1000000;
    // one closed task with a Completion audit carrying
    // estimated_cost_micros = 1_200_000; a second Ready task.
    // Act: tick_once.
    // Assert: no dispatch; SkipReason::BudgetExhausted { spent_micros: 1200000, cap_micros: 1000000 }.
}
```

- [ ] **Step 2: Run/fail/commit** — `test(spur-core): budget gate suppresses over-cap plan`

- [ ] **Step 3: Implement**

`labels.rs`: `pub const LOOP_BUDGET_MICROS_PREFIX: &str = "spur:loop-budget-micros:";` + `parse_loop_budget_micros(label) -> Option<u64>` (mirror `parse_lease_expires_at`).

`projector.rs`: 

```rust
/// Sums `estimated_cost_micros` over all Completion audits of a plan's task
/// issues. Saturating; missing fields count as 0.
pub async fn plan_spent_micros(
    pm: &dyn PmLike,
    plan_id: &str,
) -> anyhow::Result<u64> { /* list task issues by plan label incl. closed,
    collect_sorted_audits_for_issue, sum Completion.estimated_cost_micros */ }
```

`guards.rs`: after the existing gates produce `Allowed`, if the epic carries a budget label, call `plan_spent_micros`; over cap → `PlanDispatchState::BudgetExhausted { spent_micros, cap_micros }` (cached, so it's computed once per plan per tick). New `SkipReason::BudgetExhausted { spent_micros: u64, cap_micros: u64 }`.

- [ ] **Step 4: Run `scripts/spur-cargo test -p spur-core`; commit**

`feat(spur-core): budget-exhausted dispatch gate from completion audits`

---

### Task T4: Loop vocabulary — labels + `[[spur-loop v1]]` spec sentinel

**Files:**
- Modify: `crates/spur-core/src/plan/labels.rs`, `crates/spur-core/src/plan/mod.rs` (declare `pub mod loops;`)
- Create: `crates/spur-core/src/plan/loops/mod.rs`, `crates/spur-core/src/plan/loops/spec.rs`
- Test: inline `#[cfg(test)]` in `spec.rs`

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn loop_spec_sentinel_roundtrips() {
    let spec = LoopSpec {
        loop_id: "0f47ac10b58cc4372".into(),
        goal: "Keep CI green".into(),
        pattern: Some("ci-sweeper".into()),
        cadence_secs: 3600,
        autonomy: AutonomyLevel::L1,
        template: serde_json::json!({"tasks": []}),
        governors: LoopGovernors {
            max_cost_micros_per_generation: Some(2_000_000),
            max_generations_per_day: Some(24),
            max_tasks_per_generation: Some(5),
            denylist_globs: vec!["**/auth/**".into()],
            consecutive_failure_backoff: Some(FailureBackoff { k: 2, factor: 2, auto_pause_after: 4 }),
        },
        escalation: Some(LoopEscalation { after_unresolved_generations: 3 }),
    };
    let body = spec.to_sentinel_body();
    assert!(body.starts_with("[[spur-loop v1]]"));
    assert_eq!(LoopSpec::parse(&body).unwrap(), spec);
}

#[test]
fn loop_labels_roundtrip_and_fit_cap() {
    let id_label = loop_id_label("0f47ac10b58cc4372");
    assert!(id_label.len() <= 50);
    assert_eq!(parse_loop_id(&id_label), Some("0f47ac10b58cc4372"));
    let due = loop_next_run_label(1_782_950_000);
    assert_eq!(parse_loop_next_run(&due), Some(1_782_950_000));
    let gen = loop_generation_label(7);
    assert_eq!(parse_loop_generation(&gen), Some(7));
}
```

- [ ] **Step 2: Run/fail/commit** — `test(spur-core): loop spec sentinel and label round-trips`

- [ ] **Step 3: Implement**

`labels.rs` additions (mirror the `LEASE_EXPIRES_AT_PREFIX` idiom exactly):

```rust
pub const LOOP_ID_PREFIX: &str = "spur:loop-id:";
pub const LOOP_NEXT_RUN_PREFIX: &str = "spur:loop-next-run:";
pub const LOOP_GENERATION_PREFIX: &str = "spur:loop-generation:";
pub fn loop_id_label(id: &str) -> String { format!("{LOOP_ID_PREFIX}{}", compact_label_component(id)) }
pub fn parse_loop_id(label: &str) -> Option<&str> { label.strip_prefix(LOOP_ID_PREFIX) }
pub fn loop_next_run_label(ts: i64) -> String { format!("{LOOP_NEXT_RUN_PREFIX}{ts}") }
pub fn parse_loop_next_run(label: &str) -> Option<i64> { label.strip_prefix(LOOP_NEXT_RUN_PREFIX)?.parse().ok() }
pub fn loop_generation_label(n: u32) -> String { format!("{LOOP_GENERATION_PREFIX}{n}") }
pub fn parse_loop_generation(label: &str) -> Option<u32> { label.strip_prefix(LOOP_GENERATION_PREFIX)?.parse().ok() }
```

`loops/spec.rs`: plain serde structs (`LoopSpec`, `LoopGovernors`, `FailureBackoff`, `LoopEscalation`) with `#[serde(default)]` on every optional; sentinel fence `[[spur-loop v1]]\n<json>` — reuse the fence-parsing approach from `audit_sentinel.rs` (same file layout: `SENTINEL_HEADER` const, `to_sentinel_body`, `parse`). `AutonomyLevel` lives in `labels.rs` (T1) — re-export from `loops::spec`.

- [ ] **Step 4: Run `scripts/spur-cargo test -p spur-core loops::`; commit**

`feat(spur-core): loop spec sentinel and label vocabulary`

---

### Task T5: `loop-run` records on generation completion (needs T2, T4)

**Files:**
- Modify: `crates/spur-core/src/plan/audit_sentinel.rs` (new variant), `crates/spur-core/src/plan/reconciler/terminal.rs` (hook)
- Create: `crates/spur-core/src/plan/loops/run_record.rs`
- Test: sentinel round-trip inline; terminal hook test in `plan/reconciler/tests.rs`

- [ ] **Step 1: Failing tests**

Round-trip for the new variant:

```rust
AuditSentinelKind::LoopRun {
    loop_id: String, generation: u32, plan_id: String,
    outcome: String,   // "approved"|"partial"|"failed"|"skipped_overlap"|"budget_exhausted"|"report_only"
    tasks_discovered: u32, approved: u32, rejected: u32, failed: u32, cancelled: u32,
    escalations: u32, cost_micros: u64, started_at: i64, ended_at: i64,
}
```

Terminal-hook test: epic labeled `spur:loop-id:X` + `spur:loop-generation:1` reaching all-approved terminal state → exactly one `LoopRun` comment appended to the **loop issue** (the issue whose labels carry `loop_id_label(X)` and issue_type task), idempotent across a second `tick_once` (guard on existing LoopRun with same generation, mirroring the `has_epic_completion` idempotence idiom at `terminal.rs`).

- [ ] **Step 2: Run/fail/commit** — `test(spur-core): loop run record emission on epic completion`

- [ ] **Step 3: Implement**

`run_record.rs`: `pub fn build_loop_run(outcome: &EpicOutcome-ish, audits: &[AuditSentinelKind], clock_now: i64) -> AuditSentinelKind` — counts come from the same `classify_epic_completion` outcome already computed in `reconcile_terminal_epics`; `cost_micros` sums Completion costs via T3's `plan_spent_micros` logic (factor the summation over a provided audit slice so it's reusable: `sum_completion_cost_micros(&[AuditSentinelKind]) -> u64`). In `terminal.rs`, inside the branch that emits `EpicCompletion` (both the already-closed and the closing paths), when the epic carries a `spur:loop-id:*` label: locate the loop issue, check idempotence, append the `LoopRun` comment via `adv.add_comment` (same call shape as `emit_epic_completion_audit`).

- [ ] **Step 4: Run `scripts/spur-cargo test -p spur-core`; commit**

`feat(spur-core): emit loop-run records for loop generations`

---

### Task T6: LoopScheduler sweep — due-check, overlap skip, brain re-arm (needs T4, T5)

**Files:**
- Create: `crates/spur-core/src/plan/loops/scheduler.rs`
- Modify: `crates/spur-core/src/plan/reconciler/mod.rs` (`tick_once` calls the sweep; `ReconcilerConfig` gains `loops_enabled: bool` default `true`, `pause_all_loops: bool` default `false` if not already added in T1)
- Modify: `crates/spur-core/src/plan/continuation.rs` / `plan/mod.rs` (loop-due continuation, sibling of `push_plan_completed_continuation` at `plan/mod.rs:3141`)
- Test: `crates/spur-core/tests/loop_scheduler.rs`

- [ ] **Step 1: Failing integration tests** (model the file header/harness on `crates/spur-core/tests/reconciler_tick.rs`)

```rust
#[tokio::test(start_paused = true)]
async fn due_loop_pushes_loop_due_continuation_and_bumps_next_run() { }

#[tokio::test(start_paused = true)]
async fn undue_loop_is_untouched() { }

#[tokio::test(start_paused = true)]
async fn live_generation_causes_skipped_overlap_run_record_and_rearm() { }

#[tokio::test(start_paused = true)]
async fn paused_loop_and_global_pause_never_fire() { }
```

Assertions: continuation observed on the test continuation ctx (same observation point the resume/continuation tests use — see `crates/spur-core/tests/continuation_integration.rs` for the harness); `spur:loop-next-run` label replaced with `now + cadence_secs` (read issue back from mock PM); overlap case appends a `LoopRun { outcome: "skipped_overlap", .. }` comment.

- [ ] **Step 2: Run/fail/commit** — `test(spur-core): loop scheduler due, overlap, and pause behavior`

- [ ] **Step 3: Implement `scheduler.rs`**

```rust
impl super::super::reconciler::Reconciler {
    /// One pass over open loop issues. Returns true if any loop was armed
    /// or a record was written (did_work).
    pub(crate) async fn run_loop_scheduler_sweep(&self) -> anyhow::Result<bool> { ... }
}
```

Algorithm (per spec §3.1): list open issues with label prefix `spur:loop-id:` (PmLike lacks prefix query → list by `issue_type: task` + filter labels in-process, same in-process filtering style the reconciler already uses); skip when `!config.loops_enabled`, global pause, `spur:loop-paused`; parse `LoopSpec` from the issue body sentinel (unparseable spec → tracing::warn + skip, never panic); due iff `clock.now() >= parse_loop_next_run` (missing next-run label = due now); overlap iff any OPEN epic carries `loop_id_label(id)` (list epics by label, `include_closed: false`) → append `skipped_overlap` LoopRun + re-arm; otherwise L1/L2 → push loop-due continuation containing loop_id, goal, generation number (max existing generation + 1), and the template JSON, prompting the brain to review the loop issue and call `submit_plan` with the generation labels; re-arm = update_issue removing old `spur:loop-next-run:*` and adding the new one (single `IssueUpdate` with both `remove_labels`/`add_labels`). Backoff multiplier: count trailing consecutive `LoopRun` records with outcome `failed` (read loop issue comments); effective interval = `cadence_secs * factor^min(consecutive/k, ceil)`; after `auto_pause_after` consecutive failures add `spur:loop-paused` + push escalation continuation. Call the sweep from `tick_once` right after `sweep_expired_dispatch_leases` (dispatch-present branch only, like the lease sweep — the sweep needs `dispatch` for continuations).

Use the reconciler's existing `self.clock` for time — never `SystemTime::now`.

- [ ] **Step 4: Run `scripts/spur-cargo test -p spur-core --test loop_scheduler` then full `-p spur-core`; commit**

`feat(spur-core): loop scheduler sweep with overlap skip and backoff`

---

### Task T7: MCP surface — submit_loop / get_loop_status / pause_loop / resume_loop (needs T4, T6)

**Files:**
- Modify: `crates/spur-core/src/mcp/plan.rs` (tool defs — copy the `submit_plan_def` shape at `mcp/plan.rs:390`), `crates/spur-core/src/server/handlers/plan.rs` (handlers), server tool-dispatch match, `crates/spur-core/tests/tool_catalog.rs` (catalog expectations)
- Test: handler tests alongside existing ones in `crates/spur-core/src/server/handlers/plan_tests.rs`

- [ ] **Step 1: Failing tests**

- `tool_catalog.rs`: assert the four tool names appear with stable descriptions.
- `plan_tests.rs`:
  - `submit_loop_creates_loop_issue_with_sentinel_and_next_run` — call handler with a valid LoopSpec JSON (template containing one task labeled triage); assert created issue has `spur:loop-id:*`, `spur:autonomy:l1`, `spur:loop-next-run:*`, body contains `[[spur-loop v1]]`.
  - `submit_loop_rejects_template_without_triage_task` — error mentions "triage".
  - `pause_and_resume_toggle_label_and_reset_backoff` — pause adds `spur:loop-paused`; resume removes it and rewrites `spur:loop-next-run` to now.
  - `get_loop_status_returns_spec_and_recent_runs` — returns parsed spec + last N `LoopRun` records.

- [ ] **Step 2: Run/fail/commit** — `test(spur-core): loop lifecycle mcp tools`

- [ ] **Step 3: Implement**

Tool input schemas (serde structs in `tool_schemas.rs`, following existing submit_plan schema style): `SubmitLoopParams { spec: LoopSpec }`, `LoopIdParams { loop_id: String }`, `GetLoopStatusParams { loop_id: String, recent_runs: Option<u32> }`. Handlers on `McpCallbackServer`:

- `handle_submit_loop`: validate (cadence ≥ 60s; autonomy defaults l1; template has ≥1 task marked triage; governor sanity — caps > 0 when present); mint compact loop id (reuse the compact-uuid approach of `mint_delegation_id`/`mutation_id_label`); create issue via `self.pm` with sentinel body + labels + first next-run = now (fire immediately).
- `handle_pause_loop` / `handle_resume_loop`: single `IssueUpdate` label toggles as tested.
- `handle_get_loop_status`: read issue, parse spec, collect `LoopRun` audits (reuse `collect_sorted_audits_for_issue`), compute effective backoff + paused flags into a JSON response.
- v1 ratchet note: `set_loop_autonomy` is deliberately **not** in this plan (spec phases 4–5).

- [ ] **Step 4: Run full suite + lint; commit**

Run: `scripts/spur-cargo test -p spur-core && SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings`
`feat(spur-core): submit_loop and loop lifecycle mcp tools`

---

## Verification (whole plan)

1. `scripts/spur-cargo test --workspace` — green.
2. `SPUR_REMOTE=1 scripts/spur-cargo clippy --workspace -- -D warnings` — clean.
3. Grep check: no `SystemTime::now()` in `plan/loops/` (clock discipline).
4. Spec cross-check: spec §1 D4/D5/D6 (governors, cost, overlap/backoff), §2 (data model), §3.1/3.2/3.5 (scheduler, terminal hook, MCP tools) each map to T1–T7. Phases 4–5 (L2 ratchet tooling, L3 engine-armed instantiation, auto-merge integration) are explicitly out of scope.

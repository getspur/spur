# E2E Closure v0e — Normalized End-State Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Finish the persisted-plan closure story from the current `main` baseline by removing the last persisted direct-execution path, making `run_plan` explicitly ephemeral-only, adding an opt-in auto-merge/PR bridge on top of durable epic completion, and tightening reconciler wakeup + legacy-reclaim behavior without regressing the persisted-authority model.

**Architecture:** `v0c` and `v0d` already removed more of the old flow than the original `v0e` draft assumed. This normalized plan treats current `main` as ground truth: persisted `execute_epic` and persisted review approvals already flow through the reconciler, so they are not re-implemented. The remaining `v0e` work is the real delta: persisted `submit_plan` retirement, explicit ephemeral-only `run_plan`, optional auto-merge/PR on top of durable `EpicCompletion`, hybrid wakeups that do not change `tick_once()` semantics, and a short-lived legacy-reclaim detector that must exist before any final shim deletion.

**Tech Stack:** Rust 2021, tokio, serde/toml, existing `PmService` + `BeadsAdvanced`, existing `SpurEventBody::{PlanCompleted, PlanReadyToMerge}`, existing `TaskTracker`, existing `Notify`, optional `.beads/journal` append probe without new dependencies.

**Spec source:** `docs/superpowers/specs/2026-04-21-e2e-closure-design.md`

**Normalized against:** local `main` at `1932795`

---

## Current Main Baseline

These behaviors are already landed on `main` and MUST NOT be reimplemented:

- persisted `execute_epic` already writes durable scope and wakes the reconciler instead of directly spawning `run_plan`
- persisted `review_task(..., "approve")` already leaves follow-on dispatch to the reconciler
- `dispatch_newly_ready` is already retired from the persisted path
- durable epic completion, `spur:integration-pending`, persisted merge-base bootstrap, and cache-miss projection all already exist from `v0d`

The remaining `v0e` delta is:

1. add the `[spur] auto_merge_approved_plans` gate
2. retire persisted `submit_plan` direct execution
3. make `run_plan` explicitly ephemeral-only
4. add the opt-in auto-merge / auto-PR hook on top of durable epic completion
5. add hybrid reconciler wakeups
6. gate legacy reclaim on durable bootstrap detection

Final compatibility-shim deletion is **not** part of the first implementation batch unless staging/prod prove zero legacy epics remain. Shipping the detector/gate first is the correct staff-level path.

---

## File Structure

**Modify:**
- `crates/spur-acp/src/config/mod.rs` — add `[spur]` runtime config and parse/default tests
- `crates/spur-core/src/orchestrator.rs` — pass the new config gate into MCP server wiring
- `crates/spur-mcp/src/server.rs` — retire persisted `submit_plan` direct execution, add reusable merge/PR helpers, pass automation + wake config into the reconciler, and gate startup reclaim
- `crates/spur-mcp/src/plan/mod.rs` — make `run_plan` explicitly ephemeral-only while keeping the ephemeral path intact
- `crates/spur-mcp/src/plan/reconciler.rs` — add automation boundary, PR parameter derivation, hybrid wake model, and optional legacy detector helpers
- `crates/spur-mcp/tests/submit_plan_persist.rs` — persisted submit retirement regression
- `crates/spur-mcp/tests/reconciler_tick.rs` — hybrid wakeup and automation behavior
- `crates/spur-mcp/tests/plan_cancelled_task_semantics.rs` — persisted review still no direct follow-on dispatch
- `crates/spur-cli/tests/init_ux.rs` — only if pretty-printed default config output changes

**Create:**
- `crates/spur-mcp/tests/e2e_closure_v0e.rs` — normalized acceptance coverage for the remaining `v0e` delta

**Do not delete yet:**
- the startup reclaim helper path in `server.rs` — gate it first, remove it only after a clean deploy window proves no pre-`v0c` epics remain

---

## Phase 1 — Config Gate + Persisted `submit_plan` Retirement (Tasks 1–5)

### Task 1: Add `[spur] auto_merge_approved_plans` with default `false`

**Files:**
- Modify: `crates/spur-acp/src/config/mod.rs`
- Modify: `crates/spur-core/src/orchestrator.rs`
- Modify: `crates/spur-mcp/src/server.rs`

- [ ] **Step 1: Write the failing config tests**

```rust
#[test]
fn spur_runtime_defaults_auto_merge_to_false() {
    let cfg: SpurConfig = toml::from_str("").unwrap();
    assert!(!cfg.spur.auto_merge_approved_plans);
}

#[test]
fn spur_runtime_parses_auto_merge_true() {
    let cfg: SpurConfig = toml::from_str(
        r#"
        [spur]
        auto_merge_approved_plans = true
        "#,
    )
    .unwrap();
    assert!(cfg.spur.auto_merge_approved_plans);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-acp spur_runtime_ -- --nocapture`
Expected: FAIL because `SpurConfig` does not yet expose a `[spur]` runtime block.

- [ ] **Step 3: Add the minimal config + wiring**

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SpurRuntimeConfig {
    pub auto_merge_approved_plans: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpurConfig {
    // existing fields...
    #[serde(default)]
    pub delegation: DelegationConfig,
    #[serde(default)]
    pub spur: SpurRuntimeConfig,
}
```

```rust
pub fn set_auto_merge_approved_plans(&mut self, enabled: bool) {
    self.auto_merge_approved_plans = enabled;
}
```

```rust
mcp_server.set_auto_merge_approved_plans(
    self.config.spur.auto_merge_approved_plans,
);
```

- [ ] **Step 4: Re-run the targeted tests**

Run: `cargo test -p spur-acp spur_runtime_ -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-acp/src/config/mod.rs crates/spur-core/src/orchestrator.rs crates/spur-mcp/src/server.rs
git commit -m "feat(spur-acp): add auto-merge gate to spur runtime config"
```

### Task 2: Pin the remaining persisted direct-execution path in `submit_plan`

**Files:**
- Modify: `crates/spur-mcp/tests/submit_plan_persist.rs`
- Read: `crates/spur-mcp/src/server.rs`

- [ ] **Step 1: Add the failing persisted-submit test**

```rust
#[tokio::test]
async fn persisted_submit_plan_does_not_enqueue_delegation_request() {
    let fixture = persisted_submit_fixture().await;

    fixture.submit_persisted_plan().await;

    let recv = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        fixture.channel.request_rx.recv(),
    )
    .await;

    assert!(recv.is_err(), "persisted submit_plan must not dispatch directly");
}
```

- [ ] **Step 2: Run the test to verify the current bug**

Run: `cargo test -p spur-mcp persisted_submit_plan_does_not_enqueue_delegation_request -- --nocapture`
Expected: FAIL because persisted `submit_plan` still spawns `run_plan`.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-mcp/tests/submit_plan_persist.rs
git commit -m "test(spur-mcp): pin persisted submit against direct execution"
```

### Task 3: Route persisted `submit_plan` through the reconciler only

**Files:**
- Modify: `crates/spur-mcp/src/server.rs`

- [ ] **Step 1: Extract the ephemeral-only spawn helper**

```rust
fn spawn_ephemeral_plan_runner(
    &self,
    state: Arc<tokio::sync::Mutex<crate::plan::PlanState>>,
) {
    let delegation_tx = self.delegation_tx.clone();
    let plan_sink = self.event_sink.clone();
    let plan_pm = self
        .pm_service
        .clone()
        .map(|p| p as Arc<dyn crate::plan::PmLike>);
    self.task_tracker.spawn(crate::plan::run_plan(
        state,
        delegation_tx,
        plan_sink,
        plan_pm,
        self.reconciler_fast_forward.as_ref().cloned(),
    ));
}
```

- [ ] **Step 2: Use it only for ephemeral plans**

```rust
if epic_subgraph.is_some() {
    self.fast_forward_reconciler();
} else {
    self.spawn_ephemeral_plan_runner(state);
}
```

- [ ] **Step 3: Verify the persisted-submit regression is green**

Run: `cargo test -p spur-mcp persisted_submit_plan_does_not_enqueue_delegation_request -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "refactor(spur-mcp): retire persisted submit direct execution"
```

### Task 4: Add failing tests for the explicit ephemeral-only `run_plan` contract

**Files:**
- Modify: `crates/spur-mcp/src/plan/mod.rs`

- [ ] **Step 1: Add the red tests**

```rust
#[tokio::test]
async fn run_plan_with_epic_id_does_not_dispatch() {
    let (plan, tx, mut rx) = build_run_plan_fixture(Some("bd-epic".into()));
    run_plan(plan, tx, None, None, None).await;
    let recv = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        rx.recv(),
    )
    .await;
    assert!(recv.is_err());
}

#[tokio::test]
async fn run_plan_without_epic_id_still_dispatches() {
    let (plan, tx, mut rx) = build_run_plan_fixture(None);
    tokio::spawn(async move { run_plan(plan, tx, None, None, None).await });
    let recv = tokio::time::timeout(
        std::time::Duration::from_millis(100),
        rx.recv(),
    )
    .await;
    assert!(recv.is_ok());
}
```

- [ ] **Step 2: Run the persisted-path test**

Run: `cargo test -p spur-mcp run_plan_with_epic_id_does_not_dispatch -- --nocapture`
Expected: FAIL because `run_plan` still dispatches persisted plans.

- [ ] **Step 3: Commit the red test**

```bash
git add crates/spur-mcp/src/plan/mod.rs
git commit -m "test(spur-mcp): pin run_plan as ephemeral-only"
```

### Task 5: Make `run_plan` explicitly ephemeral-only

**Files:**
- Modify: `crates/spur-mcp/src/plan/mod.rs`

- [ ] **Step 1: Add the early persisted-plan guard**

```rust
pub async fn run_plan(
    plan: Arc<Mutex<PlanState>>,
    delegation_tx: mpsc::Sender<DelegationRequest>,
    event_sink: Option<Arc<dyn crate::events::McpEventSink>>,
    pm: Option<Arc<dyn PmLike>>,
    fast_forward: Option<Arc<tokio::sync::Notify>>,
) {
    if plan.lock().await.epic_id.is_some() {
        tracing::warn!("run_plan is ephemeral-only in v0e; persisted plans must use the reconciler");
        return;
    }

    // existing ephemeral executor loop...
}
```

- [ ] **Step 2: Re-run the `run_plan` contract tests**

Run: `cargo test -p spur-mcp run_plan_ -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Re-run the persisted-submit regression**

Run: `cargo test -p spur-mcp persisted_submit_plan_does_not_enqueue_delegation_request -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/plan/mod.rs
git commit -m "refactor(spur-mcp): make run_plan explicitly ephemeral-only"
```

---

## Phase 2 — Opt-In Auto-Merge / Auto-PR (Tasks 6–9)

### Task 6: Pin PR parameter derivation and config-off behavior

**Files:**
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`

- [ ] **Step 1: Add the pure helper tests**

```rust
#[test]
fn auto_pr_params_include_plan_id_and_summary() {
    let params = build_auto_pr_params("plan-123", "Epic title", "All approved", "spur/merge-1");
    assert!(params.title.contains("plan-123"));
    assert!(params.body.contains("All approved"));
    assert_eq!(params.head_branch, "spur/merge-1");
}
```

```rust
#[tokio::test]
async fn auto_merge_config_off_produces_zero_actions() {
    let fixture = automation_fixture(false);
    fixture.reconciler.tick_once().await.unwrap();
    assert!(fixture.actions.lock().await.is_empty());
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-mcp auto_ -- --nocapture`
Expected: FAIL or compile error until the helper and automation boundary exist.

- [ ] **Step 3: Commit the red tests**

```bash
git add crates/spur-mcp/src/plan/reconciler.rs
git commit -m "test(spur-mcp): pin auto-merge policy and pr params"
```

### Task 7: Extract reusable merge / PR helpers and automation boundary

**Files:**
- Modify: `crates/spur-mcp/src/server.rs`
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`

- [ ] **Step 1: Extract the reusable server helpers**

```rust
async fn merge_plan_impl(&self, plan_id: &str) -> anyhow::Result<crate::plan::PlanMergeState> {
    // move the core of handle_merge_plan here
}

async fn create_pr_impl(&self, params: spur_pm::PrParams) -> anyhow::Result<String> {
    self.pm_service
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("No PR service configured"))?
        .create_pr(params)
        .await
}
```

- [ ] **Step 2: Add the reconciler automation trait**

```rust
#[async_trait::async_trait]
pub trait ReconcilerAutomation: Send + Sync {
    async fn merge_plan(&self, plan_id: &str) -> anyhow::Result<crate::plan::PlanMergeState>;
    async fn create_pr(&self, params: spur_pm::PrParams) -> anyhow::Result<String>;
}
```

- [ ] **Step 3: Re-run the targeted automation tests**

Run: `cargo test -p spur-mcp auto_ -- --nocapture`
Expected: still FAIL until the hook is wired.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/src/plan/reconciler.rs
git commit -m "refactor(spur-mcp): extract automation boundary for v0e"
```

### Task 8: Implement the opt-in hook on durable all-approved epic completion

**Files:**
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`
- Modify: `crates/spur-core/src/orchestrator.rs`
- Modify: `crates/spur-mcp/src/server.rs`

- [ ] **Step 1: Add PR parameter derivation**

```rust
fn build_auto_pr_params(
    plan_id: &str,
    epic_title: &str,
    outcome_summary: &str,
    merge_branch: &str,
) -> spur_pm::PrParams {
    spur_pm::PrParams {
        title: format!("[SPUR] {epic_title} ({plan_id})"),
        body: format!(
            "Auto-created for plan `{plan_id}`.\n\nOutcome: {outcome_summary}\nMerge branch: {merge_branch}"
        ),
        head_branch: merge_branch.to_string(),
        base_branch: None,
        repo: None,
    }
}
```

- [ ] **Step 2: Wire the hook only on the durable all-approved path**

```rust
if self.auto_merge_approved_plans && epic.labels.contains(&crate::plan::labels::INTEGRATION_PENDING.to_string()) {
    if let Some(automation) = self.automation.as_ref() {
        let outcome_summary = self.durable_outcome_summary(plan_id, &epic.id).await?;
        let merge_state = automation.merge_plan(plan_id).await?;
        if let crate::plan::PlanMergeState::Succeeded { merge_branch, .. } = merge_state {
            let params = build_auto_pr_params(plan_id, &epic.title, &outcome_summary, &merge_branch);
            let _ = automation.create_pr(params).await?;
        }
    }
}
```

- [ ] **Step 3: Re-run the targeted automation tests**

Run: `cargo test -p spur-mcp auto_ -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs crates/spur-mcp/src/server.rs crates/spur-mcp/src/plan/reconciler.rs
git commit -m "feat(spur-mcp): add opt-in auto-merge and auto-pr hook"
```

### Task 9: Add acceptance coverage for the opt-in automation path

**Files:**
- Create: `crates/spur-mcp/tests/e2e_closure_v0e.rs`

- [ ] **Step 1: Add `t_v0e_2_auto_merge_pr_is_opt_in`**

```rust
#[tokio::test]
async fn t_v0e_2_auto_merge_pr_is_opt_in() {
    // config=false => zero automation calls
    // config=true  => exactly one merge + one PR
    // title/body include plan_id + durable outcome summary
}
```

- [ ] **Step 2: Run the acceptance test**

Run: `cargo test -p spur-mcp --test e2e_closure_v0e t_v0e_2_auto_merge_pr_is_opt_in -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/tests/e2e_closure_v0e.rs
git commit -m "test(spur-mcp): add v0e auto-merge opt-in acceptance coverage"
```

---

## Phase 3 — Hybrid Wakeups + Legacy Reclaim Gating (Tasks 10–14)

### Task 10: Pin hybrid wakeup equivalence and journal capability gating

**Files:**
- Modify: `crates/spur-mcp/tests/reconciler_tick.rs`
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`

- [ ] **Step 1: Add the failing wakeup tests**

```rust
#[tokio::test]
async fn hybrid_fast_forward_matches_polling_projection() {
    // same ready-task progression under timer-only and timer+notify
}

#[tokio::test]
async fn hybrid_journal_probe_disables_itself_when_missing() {
    // absent .beads/journal does not fail startup
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-mcp hybrid_ -- --nocapture`
Expected: FAIL until the wake abstraction exists.

- [ ] **Step 3: Commit the red tests**

```bash
git add crates/spur-mcp/tests/reconciler_tick.rs crates/spur-mcp/src/plan/reconciler.rs
git commit -m "test(spur-mcp): pin hybrid reconciler wakeups"
```

### Task 11: Implement interval + fast-forward + optional journal-append wakeups

**Files:**
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`
- Modify: `crates/spur-mcp/src/server.rs`

- [ ] **Step 1: Add the pure wake helpers**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakeReason {
    Interval,
    FastForward,
    JournalAppend,
}

fn beads_journal_path(repo_root: &std::path::Path) -> std::path::PathBuf {
    repo_root.join(".beads").join("journal")
}
```

- [ ] **Step 2: Add the optional journal monitor**

```rust
async fn monitor_journal_appends(path: std::path::PathBuf, notify: Arc<Notify>) {
    let mut last_len = tokio::fs::metadata(&path).await.ok().map(|m| m.len()).unwrap_or(0);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let next_len = match tokio::fs::metadata(&path).await {
            Ok(meta) => meta.len(),
            Err(_) => break,
        };
        if next_len > last_len {
            last_len = next_len;
            notify.notify_one();
        } else {
            last_len = next_len;
        }
    }
}
```

- [ ] **Step 3: Pass repo-root-aware wake config from server startup**

```rust
let reconciler = Reconciler::new(
    ReconcilerConfig::default(),
    pm,
    fast,
    Some(dispatch),
    None,
    self.repo_root.clone(),
    self.auto_merge_approved_plans,
    automation,
);
```

- [ ] **Step 4: Re-run the wakeup tests**

Run: `cargo test -p spur-mcp hybrid_ -- --nocapture`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan/reconciler.rs crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): add hybrid reconciler wakeups"
```

### Task 12: Pin the legacy-reclaim detector before gating startup reclaim

**Files:**
- Modify: `crates/spur-mcp/src/server.rs`

- [ ] **Step 1: Add the failing detector tests**

```rust
#[test]
fn legacy_reclaim_needed_when_rev1_bootstrap_metadata_is_missing() {
    assert!(legacy_reclaim_needed(false));
}

#[test]
fn legacy_reclaim_skipped_when_rev1_bootstrap_metadata_exists() {
    assert!(!legacy_reclaim_needed(true));
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p spur-mcp legacy_reclaim_ -- --nocapture`
Expected: FAIL until the helper exists.

- [ ] **Step 3: Commit the red tests**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "test(spur-mcp): pin legacy reclaim detector"
```

### Task 13: Gate startup reclaim on durable bootstrap detection

**Files:**
- Modify: `crates/spur-mcp/src/server.rs`

- [ ] **Step 1: Add the explicit mode + detector**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyReclaimMode {
    Skip,
    DetectAndRun,
}

fn legacy_reclaim_needed(has_rev1_merge_base_metadata: bool) -> bool {
    !has_rev1_merge_base_metadata
}
```

- [ ] **Step 2: Replace unconditional startup reclaim with detect-and-run**

```rust
let mode = if found_legacy_epic_without_rev1_metadata {
    LegacyReclaimMode::DetectAndRun
} else {
    LegacyReclaimMode::Skip
};

if matches!(mode, LegacyReclaimMode::DetectAndRun) {
    self.reclaim_persisted_plans_on_startup(Arc::clone(pm)).await?;
}
```

- [ ] **Step 3: Add the integration test**

Run: `cargo test -p spur-mcp legacy_reclaim_ -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): gate legacy reclaim on durable bootstrap detection"
```

### Task 14: Add the wakeup-equivalence acceptance test

**Files:**
- Modify: `crates/spur-mcp/tests/e2e_closure_v0e.rs`

- [ ] **Step 1: Add `t_v0e_3_fast_forward_matches_polling`**

```rust
#[tokio::test]
async fn t_v0e_3_fast_forward_matches_polling() {
    // same ready progression and same terminal outcome under
    // timer-only vs timer+fast-forward; journal probe is optional
}
```

- [ ] **Step 2: Run the acceptance test**

Run: `cargo test -p spur-mcp --test e2e_closure_v0e t_v0e_3_fast_forward_matches_polling -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/tests/e2e_closure_v0e.rs
git commit -m "test(spur-mcp): add v0e wakeup equivalence acceptance coverage"
```

---

## Phase 4 — Acceptance + Exit Verification (Tasks 15–17)

### Task 15: Add the persisted-direct-dispatch retirement acceptance test

**Files:**
- Modify: `crates/spur-mcp/tests/e2e_closure_v0e.rs`
- Read: `crates/spur-mcp/tests/plan_cancelled_task_semantics.rs`

- [ ] **Step 1: Add `t_v0e_1_no_persisted_direct_dispatch`**

```rust
#[tokio::test]
async fn t_v0e_1_no_persisted_direct_dispatch() {
    // submit_plan(persist_as_epic=true) => no direct delegation
    // execute_epic                        => no direct delegation
    // persisted review approve           => no direct delegation
}
```

- [ ] **Step 2: Run the acceptance test**

Run: `cargo test -p spur-mcp --test e2e_closure_v0e t_v0e_1_no_persisted_direct_dispatch -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/tests/e2e_closure_v0e.rs
git commit -m "test(spur-mcp): add persisted-dispatch retirement acceptance"
```

### Task 16: Run the normalized `v0e` exit suite

**Files:**
- Modify only if a regression forces a targeted fix

- [ ] **Step 1: Run the full normalized acceptance file**

Run: `cargo test -p spur-mcp --test e2e_closure_v0e -- --nocapture`
Expected: PASS.

- [ ] **Step 2: Run the targeted regression suite**

Run: `cargo test -p spur-mcp run_plan_ -- --nocapture`
Expected: PASS.

Run: `cargo test -p spur-mcp persisted_submit_plan_does_not_enqueue_delegation_request -- --nocapture`
Expected: PASS.

Run: `cargo test -p spur-mcp hybrid_ -- --nocapture`
Expected: PASS.

Run: `cargo test -p spur-acp spur_runtime_ -- --nocapture`
Expected: PASS.

- [ ] **Step 3: Run lint + focused crate tests**

Run: `cargo clippy -p spur-mcp --tests -- -D warnings`
Expected: PASS.

Run: `cargo test -p spur-mcp --lib -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git commit --allow-empty -m "test(spur-mcp): close normalized v0e verification"
```

### Task 17: Carry the deploy-window note forward and defer final shim deletion

**Files:**
- Modify: `docs/superpowers/plans/2026-04-21-e2e-closure-v0e.md` only if the implementation changes the exact post-deploy criteria

- [ ] **Step 1: Record the deletion gate explicitly**

Do **not** delete the compatibility shim in the same batch. The deletion follow-up is only safe after:

1. staging/prod observe zero open persisted epics lacking rev-1 merge-base metadata
2. startup reclaim stays unused for one deploy cycle
3. no restart-recovery regression appears in `merge_plan` / `get_task_diff`

- [ ] **Step 2: Verify the codebase still contains the gated helper**

Run: `git grep -n "LegacyReclaimMode\\|legacy_reclaim_needed" -- crates/spur-mcp/src/server.rs`
Expected: non-empty output until the separate post-deploy cleanup change.

- [ ] **Step 3: Commit only if wording changed**

```bash
git add docs/superpowers/plans/2026-04-21-e2e-closure-v0e.md
git commit -m "docs(spur-mcp): record post-deploy gate for final v0e shim deletion"
```

---

## Acceptance Mapping

- `T-v0e-1 no persisted path calls direct dispatcher helpers` → Tasks 2–5 build the regression/fix path; Task 15 is the acceptance test
- `T-v0e-2 optional auto-merge/PR path stays behind configuration` → Tasks 1 and 6–9 build the surface; Task 9 is the acceptance test
- `T-v0e-3 event-driven fast-forward does not change correctness relative to polling` → Tasks 10–14 build the wake model; Task 14 is the acceptance test

---

## Top 3 Risks

1. Auto-merge can create duplicate work if the hook is not durably gated. Keep the trigger on the durable all-approved + `spur:integration-pending` state, so successful merge clears the label and naturally suppresses repeat execution.
2. Journal-based wakeups can accidentally become a second state source. Keep `tick_once()` as the only projector and treat interval / fast-forward / journal append as timing signals only.
3. Final legacy-shim deletion is operational, not just code-local. Gating reclaim now is safe; removing the shim before a clean deploy window is not.

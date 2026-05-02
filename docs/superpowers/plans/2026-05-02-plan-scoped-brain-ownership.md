# Plan-Scoped Brain Ownership Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace repo-global brain startup ownership with plan-scoped brain ownership so multiple brain sessions can open one `.beads/` repo while only the owning brain writes each plan.

**Architecture:** Add durable plan owner labels and ownership audit sentinels to persisted plan epics. Gate reconciler dispatch by `spur:plan-owner:<brain>` before removing the `.beads/.spur-brain.pid` startup lock. Add a minimal explicit `resume_plan` path for unowned/inactive plan ownership transfer, leaving CAS leases and active handoff for follow-up phases.

**Tech Stack:** Rust 2021, `spur-mcp`, `spur-pm`, beads (`br` CLI), `serde`, `tokio`, `rmcp`, `scripts/spur-cargo`.

---

## Design Reference

Spec: `docs/superpowers/specs/2026-05-02-plan-scoped-brain-ownership-design.md`

The MVP implements:

- plan owner label helpers
- ownership audit sentinels
- owner persisted during `submit_plan`
- reconciler owner gate
- concurrent MCP callback server startup
- minimal explicit `resume_plan`

The MVP intentionally does not implement:

- active handoff
- CAS-backed transfer
- owner lease renewal
- endpoint discovery
- per-task multi-brain scheduling

## File Structure

- Modify: `crates/spur-mcp/src/plan/labels.rs`
  - Owns label vocabulary. Add plan owner label constructors/parsers.
- Modify: `crates/spur-mcp/src/plan/audit_sentinel.rs`
  - Owns `[[spur-audit v1]]` variants. Add ownership sentinel variants.
- Create: `crates/spur-mcp/src/plan/ownership.rs`
  - Owns plan ownership helpers, owner matching, and MVP transfer logic.
- Modify: `crates/spur-mcp/src/plan/mod.rs`
  - Export the new `ownership` module.
- Modify: `crates/spur-mcp/src/server.rs`
  - Persist initial plan owner during `submit_plan`.
  - Expose `resume_plan`.
  - Remove beads-backed brain pidfile startup acquisition after owner gate exists.
- Modify: `crates/spur-mcp/src/tools.rs`
  - Add `resume_plan` MCP tool definition.
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`
  - Add ownership-aware dispatch state and skip reason.
- Modify: `crates/spur-mcp/src/plan/outcomes.rs`
  - Add `SkipReason::PlanOwnedByAnotherBrain`.
- Modify: `crates/spur-mcp/tests/submit_plan_persist.rs`
  - Keep existing pure submit-plan helper tests passing after ownership changes.
- Modify: `crates/spur-mcp/tests/submit_plan_audit.rs`
  - Add ownership audit integration coverage.
- Modify: `crates/spur-mcp/tests/reconciler_tick.rs`
  - Add owner/non-owner dispatch tests.
- Modify: `crates/spur-mcp/tests/server_start_pidfile.rs`
  - Replace pidfile release regression with concurrent server startup test.
- Create: `crates/spur-mcp/tests/plan_ownership.rs`
  - Focused integration coverage for `resume_plan` and owner helper behavior.

## Task 1: Label Vocabulary

**Files:**
- Modify: `crates/spur-mcp/src/plan/labels.rs`

- [ ] **Step 1: Write failing label tests**

Add tests inside `mod tests` in `crates/spur-mcp/src/plan/labels.rs`:

```rust
#[test]
fn plan_owner_labels_normalize_uuid_components() {
    assert_eq!(
        plan_owner("550e8400-e29b-41d4-a716-446655440000"),
        "spur:plan-owner:550e8400e29b41d4a716446655440000"
    );
    assert_eq!(
        plan_owner_token("7c6258f1-6a67-4f6a-a9b4-5ea1ef59ff7a"),
        "spur:plan-owner-token:7c6258f16a674f6aa9b45ea1ef59ff7a"
    );
    assert_eq!(
        plan_owner_lease_expires_at(1_777_777_777),
        "spur:plan-owner-lease-expires-at:1777777777"
    );
}

#[test]
fn plan_owner_parsers_invert_constructors() {
    assert_eq!(
        parse_plan_owner(&plan_owner("550e8400-e29b-41d4-a716-446655440000")),
        Some("550e8400e29b41d4a716446655440000")
    );
    assert_eq!(
        parse_plan_owner_token(&plan_owner_token("7c6258f1-6a67-4f6a-a9b4-5ea1ef59ff7a")),
        Some("7c6258f16a674f6aa9b45ea1ef59ff7a")
    );
    assert_eq!(
        parse_plan_owner_lease_expires_at(&plan_owner_lease_expires_at(1_777_777_777)),
        Some(1_777_777_777)
    );
    assert_eq!(parse_plan_owner("unrelated"), None);
    assert_eq!(parse_plan_owner_token("unrelated"), None);
    assert_eq!(parse_plan_owner_lease_expires_at("unrelated"), None);
}
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
scripts/spur-cargo test -p spur-mcp plan::labels::tests::plan_owner -- --nocapture
```

Expected: compile failure for missing `plan_owner`, `plan_owner_token`, and parser functions.

- [ ] **Step 3: Implement label helpers**

In `crates/spur-mcp/src/plan/labels.rs`, add constants beside the existing delegation/lease constants:

```rust
pub const PLAN_OWNER_PREFIX: &str = "spur:plan-owner:";
pub const PLAN_OWNER_TOKEN_PREFIX: &str = "spur:plan-owner-token:";
pub const PLAN_OWNER_LEASE_EXPIRES_AT_PREFIX: &str = "spur:plan-owner-lease-expires-at:";
```

Add helpers near `lease_expires_at`:

```rust
pub fn compact_label_component(value: &str) -> String {
    value.replace('-', "")
}

pub fn plan_owner(owner: &str) -> String {
    format!("{PLAN_OWNER_PREFIX}{}", compact_label_component(owner))
}

pub fn plan_owner_token(token: &str) -> String {
    format!("{PLAN_OWNER_TOKEN_PREFIX}{}", compact_label_component(token))
}

pub fn plan_owner_lease_expires_at(ts: i64) -> String {
    format!("{PLAN_OWNER_LEASE_EXPIRES_AT_PREFIX}{ts}")
}
```

Add parsers near the existing parsers:

```rust
pub fn parse_plan_owner(label: &str) -> Option<&str> {
    label.strip_prefix(PLAN_OWNER_PREFIX)
}

pub fn parse_plan_owner_token(label: &str) -> Option<&str> {
    label.strip_prefix(PLAN_OWNER_TOKEN_PREFIX)
}

pub fn parse_plan_owner_lease_expires_at(label: &str) -> Option<i64> {
    label
        .strip_prefix(PLAN_OWNER_LEASE_EXPIRES_AT_PREFIX)?
        .parse()
        .ok()
}
```

- [ ] **Step 4: Run label tests to verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-mcp plan::labels::tests::plan_owner -- --nocapture
```

Expected: the new label tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan/labels.rs
git commit -m "feat(spur-mcp): add plan owner label helpers"
```

## Task 2: Ownership Audit Sentinels

**Files:**
- Modify: `crates/spur-mcp/src/plan/audit_sentinel.rs`

- [ ] **Step 1: Write failing audit round-trip test**

Add to `encode_then_parse_round_trips_all_variants()` in `crates/spur-mcp/src/plan/audit_sentinel.rs`:

```rust
AuditSentinelKind::PlanOwnershipAcquired {
    plan_id: "P1".into(),
    owner: "brain-A".into(),
    token: "token-A".into(),
    reason: "submit_plan".into(),
},
AuditSentinelKind::PlanOwnershipTransferred {
    plan_id: "P1".into(),
    from: "brain-A".into(),
    to: "brain-B".into(),
    mode: "inactive-reclaim".into(),
    previous_token: "token-A".into(),
    new_token: "token-B".into(),
},
AuditSentinelKind::PlanHandoffReady {
    plan_id: "P1".into(),
    owner: "brain-A".into(),
    token: "token-A".into(),
    progress_cursor: "cursor-1".into(),
},
```

Add to `kind_str_matches_serde_tag()`:

```rust
AuditSentinelKind::PlanOwnershipAcquired {
    plan_id: "P1".into(),
    owner: "brain-A".into(),
    token: "token-A".into(),
    reason: "submit_plan".into(),
},
AuditSentinelKind::PlanOwnershipTransferred {
    plan_id: "P1".into(),
    from: "brain-A".into(),
    to: "brain-B".into(),
    mode: "inactive-reclaim".into(),
    previous_token: "token-A".into(),
    new_token: "token-B".into(),
},
AuditSentinelKind::PlanHandoffReady {
    plan_id: "P1".into(),
    owner: "brain-A".into(),
    token: "token-A".into(),
    progress_cursor: "cursor-1".into(),
},
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
scripts/spur-cargo test -p spur-mcp plan::audit_sentinel::tests::encode_then_parse_round_trips_all_variants -- --nocapture
```

Expected: compile failure for missing ownership sentinel variants.

- [ ] **Step 3: Add sentinel variants**

In `AuditSentinelKind`, add variants before `Unknown`:

```rust
PlanOwnershipAcquired {
    plan_id: String,
    owner: String,
    token: String,
    reason: String,
},
PlanOwnershipTransferred {
    plan_id: String,
    from: String,
    to: String,
    mode: String,
    previous_token: String,
    new_token: String,
},
PlanHandoffReady {
    plan_id: String,
    owner: String,
    token: String,
    progress_cursor: String,
},
```

Update `kind_str()`:

```rust
Self::PlanOwnershipAcquired { .. } => "plan-ownership-acquired",
Self::PlanOwnershipTransferred { .. } => "plan-ownership-transferred",
Self::PlanHandoffReady { .. } => "plan-handoff-ready",
```

- [ ] **Step 4: Run audit tests to verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-mcp plan::audit_sentinel::tests::encode_then_parse_round_trips_all_variants plan::audit_sentinel::tests::kind_str_matches_serde_tag -- --nocapture
```

Expected: both tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan/audit_sentinel.rs
git commit -m "feat(spur-mcp): add plan ownership audit sentinels"
```

## Task 3: Ownership Helper Module

**Files:**
- Create: `crates/spur-mcp/src/plan/ownership.rs`
- Modify: `crates/spur-mcp/src/plan/mod.rs`

- [ ] **Step 1: Write the ownership helper tests**

Create `crates/spur-mcp/src/plan/ownership.rs` with tests first:

```rust
use spur_acp::SessionId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOwnerMatch {
    OwnedByCurrent,
    OwnedByOther { owner: String },
    Unowned,
}

pub fn classify_owner(labels: &[String], current: &SessionId) -> PlanOwnerMatch {
    let _ = (labels, current);
    panic!("red test sentinel: classify_owner has no implementation yet")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::labels;

    #[test]
    fn classify_owner_matches_current_session() {
        let current = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
        let labels = vec![labels::plan_owner(&current.0)];
        assert_eq!(classify_owner(&labels, &current), PlanOwnerMatch::OwnedByCurrent);
    }

    #[test]
    fn classify_owner_detects_other_session() {
        let current = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
        let other = SessionId("550e8400-e29b-41d4-a716-aaaaaaaaaaaa".into());
        let labels = vec![labels::plan_owner(&other.0)];
        assert_eq!(
            classify_owner(&labels, &current),
            PlanOwnerMatch::OwnedByOther {
                owner: labels::compact_label_component(&other.0)
            }
        );
    }

    #[test]
    fn classify_owner_treats_missing_owner_as_unowned() {
        let current = SessionId("550e8400-e29b-41d4-a716-446655440000".into());
        assert_eq!(classify_owner(&[], &current), PlanOwnerMatch::Unowned);
    }
}
```

Add to `crates/spur-mcp/src/plan/mod.rs` near other module declarations:

```rust
pub mod ownership;
```

- [ ] **Step 2: Run the test to verify RED**

Run:

```bash
scripts/spur-cargo test -p spur-mcp plan::ownership::tests -- --nocapture
```

Expected: tests compile and fail with `red test sentinel: classify_owner has no implementation yet`.

- [ ] **Step 3: Implement `classify_owner`**

Replace `classify_owner` with:

```rust
pub fn classify_owner(labels: &[String], current: &SessionId) -> PlanOwnerMatch {
    let Some(owner) = labels
        .iter()
        .find_map(|label| crate::plan::labels::parse_plan_owner(label))
    else {
        return PlanOwnerMatch::Unowned;
    };

    let current = crate::plan::labels::compact_label_component(&current.0);
    if owner == current {
        PlanOwnerMatch::OwnedByCurrent
    } else {
        PlanOwnerMatch::OwnedByOther {
            owner: owner.to_string(),
        }
    }
}
```

- [ ] **Step 4: Run ownership helper tests to verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-mcp plan::ownership::tests -- --nocapture
```

Expected: all ownership helper tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan/ownership.rs crates/spur-mcp/src/plan/mod.rs
git commit -m "feat(spur-mcp): add plan ownership helpers"
```

## Task 4: Persist Owner on Submit

**Files:**
- Modify: `crates/spur-mcp/src/server.rs`
- Modify: `crates/spur-mcp/tests/submit_plan_audit.rs`

- [ ] **Step 1: Write failing integration test**

In `crates/spur-mcp/tests/submit_plan_audit.rs`, add a test following the local setup pattern in the file:

```rust
#[tokio::test]
async fn submit_plan_persists_plan_owner_on_epic() {
    if !br_available() {
        eprintln!("skipping submit_plan_persists_plan_owner_on_epic: `br` not on PATH");
        return;
    }

    let dir = tempfile::TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = std::sync::Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    );
    let brain_session = spur_acp::BrainSessionId::new(spur_acp::SessionId(
        "550e8400-e29b-41d4-a716-446655440000".into(),
    ));
    let (mut server, _channel) = spur_mcp::McpCallbackServer::new(
        &brain_session,
        Some(std::sync::Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        std::sync::Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.set_repo_root(dir.path().to_path_buf());

    let response = server
        .__test_call_submit_plan(serde_json::json!({
            "persist_as_epic": true,
            "epic_title": "Owned plan",
            "tasks": [{
                "task_id": "T1",
                "agent": "codex",
                "task": "Do T1",
                "depends_on": [],
                "context_files": []
            }]
        }))
        .await;
    assert!(response.get("error").is_none(), "submit_plan failed: {response:#?}");

    let epics = pm
        .list_issues(spur_pm::IssueFilter {
            issue_type: Some("epic".into()),
            limit: Some(10),
            ..Default::default()
        })
        .await
        .expect("list epics");
    let epic = pm.get_issue(&epics[0].id).await.expect("get epic");
    assert!(
        epic.labels.iter().any(|label| {
            label == &spur_mcp::plan::labels::plan_owner(brain_session.as_session_id().0.as_str())
        }),
        "epic should include plan owner label: {:?}",
        epic.labels
    );
}
```

If `submit_plan_audit.rs` lacks `test_continuation_ctx`, copy the existing helper from nearby MCP server integration tests instead of creating a new abstraction.

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test submit_plan_audit submit_plan_persists_plan_owner_on_epic -- --exact --nocapture
```

Expected: test fails because the epic has no `spur:plan-owner:*` label.

- [ ] **Step 3: Persist owner after successful epic creation**

In `handle_submit_plan`, inside `match build_epic_subgraph(...) { Ok(sg) => { ... } }`, add before the `info!` block:

```rust
let owner_label = crate::plan::labels::plan_owner(&self.brain_session_id.as_session_id().0);
pm.update_issue(
    &sg.epic_id,
    spur_pm::IssueUpdate {
        add_labels: vec![owner_label],
        ..Default::default()
    },
)
.await
.map_err(|error| {
    error!(
        plan_id = %plan_id,
        epic_id = %sg.epic_id,
        "failed to persist plan owner: {error}"
    );
    error
})
.map_err(|error| {
    JsonRpcResponse::error(
        id.clone(),
        -32000,
        format!("submit_plan: failed to persist plan owner: {error}"),
    )
})?;
```

If `?` is awkward inside the handler branch, use an explicit `if let Err(error)` and `return JsonRpcResponse::error(...)`.

- [ ] **Step 4: Emit ownership acquired audit**

In the same success path, after the owner label update and before `emit_plan_submit_audit`, add:

```rust
if let Some(adv) = self.pm_service.as_deref().and_then(|pm| pm.advanced()) {
    let token = uuid::Uuid::new_v4().to_string();
    let kind = crate::plan::audit_sentinel::AuditSentinelKind::PlanOwnershipAcquired {
        plan_id: plan_id.clone(),
        owner: self.brain_session_id.to_string(),
        token,
        reason: "submit_plan".to_string(),
    };
    if let Err(error) = adv
        .add_comment(&sg.epic_id, &crate::plan::audit_sentinel::encode_comment(&kind))
        .await
    {
        tracing::warn!(
            plan_id = %plan_id,
            epic_id = %sg.epic_id,
            "plan ownership audit emission failed: {error}"
        );
    }
}
```

- [ ] **Step 5: Run test to verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test submit_plan_audit submit_plan_persists_plan_owner_on_epic -- --exact --nocapture
```

Expected: test passes and epic labels include `spur:plan-owner:<compact brain id>`.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/tests/submit_plan_audit.rs
git commit -m "feat(spur-mcp): persist plan owner on submit"
```

## Task 5: Reconciler Ownership Gate

**Files:**
- Modify: `crates/spur-mcp/src/plan/outcomes.rs`
- Modify: `crates/spur-mcp/src/plan/reconciler.rs`
- Modify: `crates/spur-mcp/tests/reconciler_tick.rs`

- [ ] **Step 1: Write failing non-owner skip test**

In `crates/spur-mcp/tests/reconciler_tick.rs`, add a test near existing dispatch tests:

```rust
#[tokio::test]
async fn tick_once_skips_plan_owned_by_another_brain() {
    if !br_available() {
        eprintln!("skipping tick_once_skips_plan_owned_by_another_brain: `br` not on PATH");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    init_git_repo(dir.path());
    run_br(dir.path(), &["init"]);

    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    );
    let feature_gate = common::server_builder::pro_feature_gate();
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "owned-by-other",
        "Owned by Other",
        None,
        &[plan_task("T1")],
    )
    .await
    .expect("build epic subgraph");
    pm.update_issue(
        &subgraph.epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![spur_mcp::plan::labels::plan_owner(
                "550e8400-e29b-41d4-a716-aaaaaaaaaaaa",
            )],
            ..Default::default()
        },
    )
    .await
    .expect("add owner label");

    let (delegation_tx, mut delegation_rx) = tokio::sync::mpsc::channel(8);
    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        Some(ReconcilerDispatchCtx {
            delegation_tx,
            task_tracker: tokio_util::task::TaskTracker::new(),
            brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId(
                "550e8400-e29b-41d4-a716-446655440000".into(),
            )),
            event_sink: None,
            materializer: test_materializer(),
        }),
        Some("owned-by-other".into()),
        feature_gate,
    );

    let did_work = reconciler.tick_once().await.expect("tick_once");
    assert!(!did_work, "non-owner reconciler must not dispatch");
    assert!(
        delegation_rx.try_recv().is_err(),
        "non-owner reconciler must not enqueue delegation"
    );
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test reconciler_tick tick_once_skips_plan_owned_by_another_brain -- --exact --nocapture
```

Expected: test fails because the reconciler dispatches despite another owner.

- [ ] **Step 3: Add skip reason**

In `crates/spur-mcp/src/plan/outcomes.rs`, add:

```rust
PlanOwnedByAnotherBrain { owner: String },
```

to `SkipReason`, and add:

```rust
PlanOwnedByAnotherBrain,
```

to `SkipReasonKey`.

Update `impl From<&SkipReason> for SkipReasonKey`:

```rust
SkipReason::PlanOwnedByAnotherBrain { .. } => Self::PlanOwnedByAnotherBrain,
```

- [ ] **Step 4: Add dispatch state**

In `crates/spur-mcp/src/plan/reconciler.rs`, add to `PlanDispatchState`:

```rust
PlanOwnedByAnotherBrain { epic_id: String, owner: String },
```

Update `skip_reason()`:

```rust
Self::PlanOwnedByAnotherBrain { owner, .. } => Some(SkipReason::PlanOwnedByAnotherBrain {
    owner: owner.clone(),
}),
```

- [ ] **Step 5: Gate `plan_allows_dispatch` by owner**

In `plan_allows_dispatch`, after loading each epic and before setting `open_complete_epic`, add:

```rust
if epic
    .labels
    .iter()
    .any(|label| label == crate::plan::labels::PLAN_COMPLETE)
{
    if let Some(dispatch) = self.dispatch.as_ref() {
        match crate::plan::ownership::classify_owner(
            &epic.labels,
            dispatch.brain_session_id.as_session_id(),
        ) {
            crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {}
            crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
                let state = PlanDispatchState::PlanOwnedByAnotherBrain {
                    epic_id: epic.id.clone(),
                    owner,
                };
                cache.insert(plan_id.to_string(), state.clone());
                return Ok(state);
            }
            crate::plan::ownership::PlanOwnerMatch::Unowned => {
                let state = PlanDispatchState::PlanOwnedByAnotherBrain {
                    epic_id: epic.id.clone(),
                    owner: "unowned".to_string(),
                };
                cache.insert(plan_id.to_string(), state.clone());
                return Ok(state);
            }
        }
    }
}
```

This implements the end-state policy that legacy unowned plans require explicit resume.

- [ ] **Step 6: Run non-owner test to verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test reconciler_tick tick_once_skips_plan_owned_by_another_brain -- --exact --nocapture
```

Expected: test passes.

- [ ] **Step 7: Run existing owner dispatch regression**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test reconciler_tick tick_once_dispatches_ready_task_with_approved_dep_overlay -- --exact --nocapture
```

Expected: if this fails because the fixture has no owner label, update the fixture to add `spur:plan-owner:<brain>` matching the test reconciler before `tick_once`.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-mcp/src/plan/outcomes.rs crates/spur-mcp/src/plan/reconciler.rs crates/spur-mcp/tests/reconciler_tick.rs
git commit -m "feat(spur-mcp): gate reconciler dispatch by plan owner"
```

## Task 6: Concurrent MCP Server Startup

**Files:**
- Modify: `crates/spur-mcp/src/server.rs`
- Modify: `crates/spur-mcp/tests/server_start_pidfile.rs`

- [ ] **Step 1: Write failing concurrent startup test**

Replace `dropping_server_handle_releases_pidfile_for_next_start` in `crates/spur-mcp/tests/server_start_pidfile.rs` with:

```rust
#[tokio::test]
async fn beads_backed_start_allows_concurrent_brain_servers() {
    if !br_available() {
        eprintln!("skipping beads_backed_start_allows_concurrent_brain_servers: `br` not on PATH");
        return;
    }
    skip_if_no_loopback!("beads_backed_start_allows_concurrent_brain_servers");

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);

    let pm = beads_pm(dir.path()).await;
    let first_sid = BrainSessionId::new(SessionId::new());
    let second_sid = BrainSessionId::new(SessionId::new());

    let (mut first_server, _channel) = McpCallbackServer::new(
        &first_sid,
        Some(pm.clone()),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    first_server.set_repo_root(dir.path().to_path_buf());
    first_server.set_reconciler_enabled(true, None);
    let (_first_url, first_handle) = Arc::new(first_server)
        .start()
        .await
        .expect("first start should succeed");

    let (mut second_server, _channel) = McpCallbackServer::new(
        &second_sid,
        Some(pm.clone()),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    second_server.set_repo_root(dir.path().to_path_buf());
    second_server.set_reconciler_enabled(true, None);
    let (_second_url, second_handle) = Arc::new(second_server)
        .start()
        .await
        .expect("second start in same beads repo should succeed");

    drop(second_handle);
    drop(first_handle);
}
```

Remove the now-unused `use std::time::Duration;`.

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test server_start_pidfile beads_backed_start_allows_concurrent_brain_servers -- --exact --nocapture
```

Expected: failure containing `another SPUR brain session already owns this .beads/`.

- [ ] **Step 3: Remove startup pidfile gate from MCP server**

In `crates/spur-mcp/src/server.rs`:

1. Remove `pidfile::PidFileGuard` from the `use spur_pm::{...}` import.
2. Remove the `brain_pidfile` field from `McpCallbackServer`.
3. Remove `brain_pidfile: None` from `McpCallbackServer::new`.
4. Remove the `PidFileGuard::acquire` block in `start()`.
5. Remove the `_brain_pidfile` capture inside the spawned root server task.

Leave the `crates/spur-pm/src/pidfile.rs` module intact because community TUI singleton uses it separately.

- [ ] **Step 4: Run startup test to verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test server_start_pidfile beads_backed_start_allows_concurrent_brain_servers -- --exact --nocapture
```

Expected: test passes.

- [ ] **Step 5: Run full startup pidfile test file**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test server_start_pidfile -- --nocapture
```

Expected: both tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/tests/server_start_pidfile.rs
git commit -m "feat(spur-mcp): allow concurrent brain servers per repo"
```

## Task 7: Minimal `resume_plan` Tool

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs`
- Modify: `crates/spur-mcp/src/server.rs`
- Create: `crates/spur-mcp/tests/plan_ownership.rs`

- [ ] **Step 1: Write tool list test**

Create `crates/spur-mcp/tests/plan_ownership.rs`:

```rust
#[test]
fn resume_plan_appears_in_tools_list() {
    let tool = spur_mcp::tools_list()
        .into_iter()
        .find(|tool| tool.name == "resume_plan")
        .expect("resume_plan tool must be advertised");
    assert!(
        tool.input_schema["required"]
            .as_array()
            .expect("required array")
            .iter()
            .any(|value| value == "plan_id"),
        "resume_plan should require plan_id: {:?}",
        tool.input_schema
    );
}
```

- [ ] **Step 2: Run test to verify RED**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test plan_ownership resume_plan_appears_in_tools_list -- --exact --nocapture
```

Expected: test fails because `resume_plan` is not advertised.

- [ ] **Step 3: Add tool definition**

In `crates/spur-mcp/src/tools.rs`, add:

```rust
fn resume_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "resume_plan".into(),
        description: "Explicitly claim or resume ownership of a persisted beads plan for this brain session. MVP claims unowned plans and refuses active owners; future phases add active handoff.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The persisted plan_id to resume on this brain session"
                }
            },
            "required": ["plan_id"]
        }),
    }
}
```

Add `resume_plan_def()` to `tools_list()` near other plan tools.

- [ ] **Step 4: Add handler dispatch**

In `handle_tool_call` in `crates/spur-mcp/src/server.rs`, add:

```rust
"resume_plan" => self.handle_resume_plan(id, arguments).await,
```

- [ ] **Step 5: Implement MVP handler**

Add to `impl McpCallbackServer` near other plan handlers:

```rust
async fn handle_resume_plan(&self, id: Value, args: Value) -> JsonRpcResponse {
    let Some(plan_id) = args.get("plan_id").and_then(|v| v.as_str()) else {
        return JsonRpcResponse::invalid_params(id, "resume_plan: missing plan_id");
    };
    let Some(pm) = self.pm_service.as_ref() else {
        return JsonRpcResponse::internal_error(id, "resume_plan requires PM service");
    };

    let summaries = match pm
        .list_issues(spur_pm::IssueFilter {
            labels: vec![crate::plan::labels::plan_id(plan_id)],
            issue_type: Some("epic".into()),
            include_closed: true,
            limit: Some(10),
            ..Default::default()
        })
        .await
    {
        Ok(summaries) => summaries,
        Err(error) => return JsonRpcResponse::internal_error(id, format!("resume_plan: {error}")),
    };
    let Some(summary) = summaries.first() else {
        return JsonRpcResponse::error(id, -32004, format!("resume_plan: plan not found: {plan_id}"));
    };
    let epic = match pm.get_issue(&summary.id).await {
        Ok(epic) => epic,
        Err(error) => return JsonRpcResponse::internal_error(id, format!("resume_plan: {error}")),
    };

    match crate::plan::ownership::classify_owner(
        &epic.labels,
        self.brain_session_id.as_session_id(),
    ) {
        crate::plan::ownership::PlanOwnerMatch::OwnedByCurrent => {
            return JsonRpcResponse::success(id, json!({
                "status": "already_owner",
                "plan_id": plan_id,
                "epic_id": epic.id,
            }));
        }
        crate::plan::ownership::PlanOwnerMatch::OwnedByOther { owner } => {
            return JsonRpcResponse::error(
                id,
                -32009,
                format!("resume_plan: plan is owned by active or unknown brain {owner}; active handoff is not implemented in MVP"),
            );
        }
        crate::plan::ownership::PlanOwnerMatch::Unowned => {}
    }

    let owner_label = crate::plan::labels::plan_owner(&self.brain_session_id.as_session_id().0);
    if let Err(error) = pm
        .update_issue(
            &epic.id,
            spur_pm::IssueUpdate {
                add_labels: vec![owner_label],
                ..Default::default()
            },
        )
        .await
    {
        return JsonRpcResponse::internal_error(id, format!("resume_plan: {error}"));
    }

    self.fast_forward_reconciler();
    JsonRpcResponse::success(id, json!({
        "status": "claimed",
        "plan_id": plan_id,
        "epic_id": epic.id,
    }))
}
```

- [ ] **Step 6: Run tool list test to verify GREEN**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test plan_ownership resume_plan_appears_in_tools_list -- --exact --nocapture
```

Expected: test passes.

- [ ] **Step 7: Add integration tests for unowned/owned behavior**

Replace `crates/spur-mcp/tests/plan_ownership.rs` with this complete test file:

```rust
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::json;
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::server::DetachedContinuationCtx;
use spur_mcp::{McpCallbackServer, tools_list};
use spur_mcp::plan::{labels, PlanTask};

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) {
    let output = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    assert!(
        output.status.success(),
        "br {args:?} failed: stderr={} stdout={}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
}

fn test_continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_cont, _worker| Box::pin(async {})),
    }
}

fn one_task() -> Vec<PlanTask> {
    vec![PlanTask {
        task_id: "T1".into(),
        agent: "codex".into(),
        task: "Do T1".into(),
        depends_on: Vec::new(),
        issue_id: None,
        context_files: Vec::new(),
    }]
}

#[tokio::test]
async fn resume_plan_claims_unowned_plan() {
    if !br_available() {
        eprintln!("skipping resume_plan_claims_unowned_plan: `br` not on PATH");
        return;
    }

    let dir = tempfile::TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    );
    let feature_gate = common::server_builder::pro_feature_gate();
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "resume-unowned",
        "Resume unowned",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");

    let brain_session = BrainSessionId::new(SessionId(
        "550e8400-e29b-41d4-a716-446655440000".into(),
    ));
    let (mut server, _channel) = McpCallbackServer::new(
        &brain_session,
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        feature_gate,
    );
    server.set_repo_root(dir.path().to_path_buf());

    let response = server
        .__test_call_tool("resume_plan", json!({ "plan_id": "resume-unowned" }))
        .await;
    assert!(response.get("error").is_none(), "resume_plan failed: {response:#?}");

    let epic = pm.get_issue(&subgraph.epic_id).await.expect("get epic");
    assert!(
        epic.labels.iter().any(|label| {
            label == &labels::plan_owner(brain_session.as_session_id().0.as_str())
        }),
        "resume_plan should add current owner label: {:?}",
        epic.labels
    );
}

#[tokio::test]
async fn resume_plan_refuses_plan_owned_by_other_brain() {
    if !br_available() {
        eprintln!("skipping resume_plan_refuses_plan_owned_by_other_brain: `br` not on PATH");
        return;
    }

    let dir = tempfile::TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, dir.path(), None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    );
    let feature_gate = common::server_builder::pro_feature_gate();
    let subgraph = spur_mcp::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        "resume-owned",
        "Resume owned",
        None,
        &one_task(),
    )
    .await
    .expect("build epic subgraph");
    pm.update_issue(
        &subgraph.epic_id,
        spur_pm::IssueUpdate {
            add_labels: vec![labels::plan_owner("550e8400-e29b-41d4-a716-aaaaaaaaaaaa")],
            ..Default::default()
        },
    )
    .await
    .expect("add other owner");

    let brain_session = BrainSessionId::new(SessionId(
        "550e8400-e29b-41d4-a716-446655440000".into(),
    ));
    let (mut server, _channel) = McpCallbackServer::new(
        &brain_session,
        Some(Arc::clone(&pm)),
        None,
        test_continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        feature_gate,
    );
    server.set_repo_root(dir.path().to_path_buf());

    let response = server
        .__test_call_tool("resume_plan", json!({ "plan_id": "resume-owned" }))
        .await;
    let message = response["error"]["message"]
        .as_str()
        .expect("error message");
    assert!(
        message.contains("active handoff is not implemented in MVP"),
        "unexpected error: {response:#?}"
    );
}
```

Add `mod common;` at the top of this test file so `common::server_builder::pro_feature_gate()` resolves.

- [ ] **Step 8: Run plan ownership tests**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test plan_ownership -- --nocapture
```

Expected: all plan ownership tests pass.

- [ ] **Step 9: Commit**

```bash
git add crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs crates/spur-mcp/tests/plan_ownership.rs
git commit -m "feat(spur-mcp): add explicit resume_plan MVP"
```

## Task 8: Focused MVP Regression Suite

**Files:**
- No production file changes expected.

- [x] **Step 1: Run focused tests**

Run:

```bash
scripts/spur-cargo test -p spur-mcp --test submit_plan_persist -- --nocapture
scripts/spur-cargo test -p spur-mcp --test submit_plan_audit -- --nocapture
scripts/spur-cargo test -p spur-mcp --test reconciler_tick -- --nocapture
scripts/spur-cargo test -p spur-mcp --test server_start_pidfile -- --nocapture
scripts/spur-cargo test -p spur-mcp --test plan_ownership -- --nocapture
```

Expected: all focused tests pass. If `reconciler_tick` fixtures fail because owner-missing legacy plans are now blocked, add explicit owner labels to tests that expect dispatch.

- [x] **Step 2: Run formatting**

Run:

```bash
scripts/spur-cargo fmt --all
```

Expected: formatting completes with exit 0.

- [x] **Step 3: Run crate tests**

Run:

```bash
scripts/spur-cargo test -p spur-mcp
```

Expected: all `spur-mcp` tests pass.

- [x] **Step 4: Run clippy for touched crate**

Run:

```bash
scripts/spur-cargo clippy -p spur-mcp -- -D warnings
```

Expected: clippy exits 0.

- [x] **Step 5: Commit verification-only adjustments if any**

If formatting changed files:

```bash
git add crates/spur-mcp
git commit -m "chore(spur-mcp): format plan ownership changes"
```

If no files changed, do not create an empty commit.

## Task 9: Phase 6 Hardening Plan Stub

**Files:**
- Create: `docs/superpowers/plans/2026-05-02-plan-ownership-cas-hardening.md`

- [x] **Step 1: Create follow-up plan for CAS and active handoff**

Write a short follow-up plan with these tasks:

1. Add CAS mutation support to beads adapter.
2. Add owner token and lease labels to initial acquisition.
3. Add token-fenced write checks to dispatch/review/merge/signal mutation paths.
4. Add owner heartbeat renewal.
5. Add active handoff request and `plan-handoff-ready` audit.
6. Add force reclaim with explicit user confirmation.

Use this exact header:

```markdown
# Plan Ownership CAS Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Harden plan-scoped ownership with CAS, owner leases, active handoff, and stale-owner fencing.

**Architecture:** Build on the MVP owner label gate by adding compare-and-set ownership transfer and owner-token checks on all write paths.

**Tech Stack:** Rust 2021, `spur-mcp`, `spur-pm`, beads (`br` CLI), SQLite/Dolt-backed beads internals as exposed by project APIs.

---
```

- [x] **Step 2: Commit follow-up plan**

```bash
git add docs/superpowers/plans/2026-05-02-plan-ownership-cas-hardening.md
git commit -m "docs: plan CAS hardening for plan ownership"
```

## Final Verification

Run before claiming the MVP is complete:

```bash
scripts/spur-cargo fmt --all
scripts/spur-cargo test -p spur-mcp
scripts/spur-cargo clippy -p spur-mcp -- -D warnings
```

Expected:

- formatting exits 0
- `spur-mcp` tests pass
- clippy exits 0 with no warnings

## Self-Review Checklist

- Spec coverage:
  - multiple callback servers: Task 6
  - owner labels: Task 1 and Task 4
  - owner audit sentinels: Task 2 and Task 4
  - reconciler owner gate: Task 5
  - explicit resume/reclaim MVP: Task 7
  - end-state CAS/handoff tracked: Task 9
- Placeholder scan:
  - No unfinished markers or unchecked vague instructions should remain.
- Type consistency:
  - Label helpers live in `plan::labels`.
  - Owner classification lives in `plan::ownership`.
  - Ownership write gate uses `BrainSessionId::as_session_id()`.

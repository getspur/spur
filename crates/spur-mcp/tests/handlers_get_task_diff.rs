//! Phase 2 Task 11: freestanding `get_task_diff` handler.
//!
//! Asserts the freestanding handler still produces the historical JSON shape
//! for an `Approved` task with a cached `result`, and that a feature-gate
//! denial for the recovery code path surfaces as
//! `McpHandlerError::Unauthorized` (not Internal/UpstreamPm) so the dispatcher
//! can map it to the `-32001` JSON-RPC code.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use spur_mcp::handlers::{get_task_diff, McpHandlerError, PlanResolver, WorkerCallContext};
use spur_mcp::plan::{
    AttemptRecord, PlanMergeState, PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus,
};
use spur_pm::PmService;
use tempfile::TempDir;
use tokio::sync::Mutex;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_br(repo: &Path, args: &[&str]) {
    let out = Command::new("br")
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .expect("br invocation failed");
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
        panic!(
            "br {args:?} failed (exit {}): stderr={stderr} stdout={stdout}",
            out.status
        );
    }
}

async fn test_pm_service(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    )
}

struct FixedPlanResolver {
    plan: Arc<Mutex<PlanState>>,
}

#[async_trait]
impl PlanResolver for FixedPlanResolver {
    async fn load_or_project_plan(&self, plan_id: &str) -> Result<Arc<Mutex<PlanState>>, String> {
        let p = self.plan.lock().await;
        if p.plan_id == plan_id {
            Ok(self.plan.clone())
        } else {
            Err(format!("unknown plan '{plan_id}'"))
        }
    }
}

fn ctx() -> WorkerCallContext {
    WorkerCallContext {
        delegation_id: "d".into(),
        brain_session_id: "b".into(),
    }
}

fn community_gate() -> Arc<spur_license::FeatureGate> {
    spur_mcp::server::community_feature_gate()
}

fn make_plan_with_cached_result(plan_id: &str) -> PlanState {
    let result = spur_acp::DelegationResult {
        status: spur_acp::DelegationStatus::Success,
        diff: Some("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n".to_string()),
        diff_summary: None,
        summary: Some("worker summary".into()),
        estimated_cost_usd: 0.0,
        worker_branch: Some("spur/worker-cached".into()),
        artifact: None,
    };
    PlanState {
        plan_id: plan_id.to_string(),
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: PlanMergeState::NotStarted,
        epic_id: None,
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "task-cached".into(),
                agent: "codex".into(),
                task: "do thing".into(),
                depends_on: vec![],
                issue_id: None,
                context_files: vec![],
            },
            status: PlanTaskStatus::Approved {
                summary: Some("approved".into()),
            },
            result: Some(result),
            worker_branch: Some("spur/worker-cached".into()),
            attempt: 1,
            history: vec![],
            last_delegation_id: None,
            dispatched_base_oid: None,
        }],
    }
}

fn make_plan_needing_recovery(plan_id: &str) -> PlanState {
    PlanState {
        plan_id: plan_id.to_string(),
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: PlanMergeState::NotStarted,
        // epic_id + issue_id present forces the recovery branch into
        // `read_persisted_plan_bootstrap` / `read_latest_task_completion`,
        // which require PM_PRO_BEADS_ADVANCED.
        epic_id: Some("issue-epic".into()),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "task-recover".into(),
                agent: "codex".into(),
                task: "do thing".into(),
                depends_on: vec![],
                issue_id: Some("issue-task".into()),
                context_files: vec![],
            },
            status: PlanTaskStatus::Approved { summary: None },
            // No cached result — handler must walk the recovery branch.
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
            last_delegation_id: None,
            dispatched_base_oid: None,
        }],
    }
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn get_task_diff_returns_cached_diff_shape() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = test_pm_service(dir.path()).await;
    let gate = community_gate();
    let resolver = FixedPlanResolver {
        plan: Arc::new(Mutex::new(make_plan_with_cached_result("plan-cached"))),
    };

    let value = get_task_diff(
        Some(pm.as_ref()),
        &gate,
        Some(dir.path()),
        &resolver,
        &ctx(),
        json!({ "plan_id": "plan-cached", "task_id": "task-cached" }),
    )
    .await
    .expect("cached-result path must succeed without invoking pm");

    assert_eq!(value["task_id"], "task-cached");
    assert_eq!(value["agent"], "codex");
    assert_eq!(value["status"], "approved");
    assert_eq!(value["worker_branch"], "spur/worker-cached");
    // build_task_diff_fields overrides the status_summary with result.summary
    // when result is cached. Preserved from the historical brain method.
    assert_eq!(value["summary"], "worker summary");
    assert!(
        value["diff"]
            .as_str()
            .map(|s| s.contains("+new"))
            .unwrap_or(false),
        "diff text from cached DelegationResult must surface in response: {value}"
    );
}

/// Fix-1 regression guard: cached-result reads MUST succeed when the brain
/// has no PmService and no repo_root configured. The pre-refactor brain method
/// only required these inside the recovery branch; an attempt-2 wrapper that
/// hoisted them to upfront `.ok_or_else()?` checks broke this code path.
#[tokio::test]
async fn get_task_diff_cached_result_succeeds_without_pm_or_repo_root() {
    let resolver = FixedPlanResolver {
        plan: Arc::new(Mutex::new(make_plan_with_cached_result("plan-cached"))),
    };
    // Note: no `br_available` skip — this test deliberately constructs no PM
    // and no repo_root. The cached-result branch must not need either.
    let gate = community_gate();

    let value = get_task_diff(
        None,
        &gate,
        None,
        &resolver,
        &ctx(),
        json!({ "plan_id": "plan-cached", "task_id": "task-cached" }),
    )
    .await
    .expect(
        "cached-result path must succeed when brain has no PmService and no repo_root configured",
    );

    assert_eq!(value["status"], "approved");
    assert_eq!(value["worker_branch"], "spur/worker-cached");
    assert!(
        value["diff"]
            .as_str()
            .map(|s| s.contains("+new"))
            .unwrap_or(false),
        "diff text from cached DelegationResult must surface even without pm/repo_root: {value}"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn get_task_diff_unauthorized_when_recovery_needs_pro_feature() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = test_pm_service(dir.path()).await;
    // Community gate denies PM_PRO_BEADS_ADVANCED — the helpers used by the
    // recovery branch (`read_latest_task_completion`) reject up-front, and
    // the freestanding handler must surface that as Unauthorized so the
    // dispatcher returns -32001 instead of -32603.
    let gate = community_gate();
    let resolver = FixedPlanResolver {
        plan: Arc::new(Mutex::new(make_plan_needing_recovery("plan-recover"))),
    };

    let err = get_task_diff(
        Some(pm.as_ref()),
        &gate,
        Some(dir.path()),
        &resolver,
        &ctx(),
        json!({ "plan_id": "plan-recover", "task_id": "task-recover" }),
    )
    .await
    .expect_err("recovery branch under community gate must error");

    assert!(
        matches!(err, McpHandlerError::Unauthorized(_)),
        "feature-gate denial must surface as Unauthorized (preserves -32001 wire code); got {err:?}"
    );
}

#[ignore = "requires br on PATH; run with --ignored"]
#[tokio::test]
async fn get_task_diff_missing_plan_id_invalid_params() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );

    let dir = TempDir::new().expect("tempdir");
    run_br(dir.path(), &["init"]);
    let pm = test_pm_service(dir.path()).await;
    let gate = community_gate();
    let resolver = FixedPlanResolver {
        plan: Arc::new(Mutex::new(make_plan_with_cached_result("plan-cached"))),
    };

    let err = get_task_diff(
        Some(pm.as_ref()),
        &gate,
        Some(dir.path()),
        &resolver,
        &ctx(),
        json!({ "task_id": "task-cached" }),
    )
    .await
    .expect_err("missing plan_id must error");

    assert!(
        matches!(err, McpHandlerError::InvalidParams(_)),
        "got {err:?}"
    );
}

/// Fix-3 coverage: the `attempt != current_attempt` historical-lookup branch
/// (handlers.rs ~ line 418) is the largest untested code path in this handler.
/// Builds a plan with a populated in-memory `history` (so the recovery branch
/// is bypassed) and asserts the response carries `status == "historical"`,
/// the requested `attempt`, the `feedback`, and the `note`.
#[tokio::test]
async fn get_task_diff_historical_lookup_returns_attempt_record_shape() {
    let resolver = FixedPlanResolver {
        plan: Arc::new(Mutex::new(plan_with_in_memory_history())),
    };
    let gate = community_gate();

    let value = get_task_diff(
        None,
        &gate,
        None,
        &resolver,
        &ctx(),
        json!({ "plan_id": "plan-history", "task_id": "task-h", "attempt": 1 }),
    )
    .await
    .expect("in-memory history lookup must not require pm/repo_root");

    assert_eq!(value["status"], "historical");
    assert_eq!(value["attempt"], 1);
    assert_eq!(value["task_id"], "task-h");
    assert_eq!(value["agent"], "codex");
    assert_eq!(value["worker_branch"], "spur/worker-h-attempt-1");
    assert_eq!(value["summary"], "attempt 1 partial summary");
    assert_eq!(value["feedback"], "needs error handling on null input");
    assert!(
        value["note"]
            .as_str()
            .map(|note| note.contains("Historical attempt"))
            .unwrap_or(false),
        "historical responses must explain the summary-only contract: {value}"
    );
    assert!(
        value.get("diff").is_none(),
        "historical responses must remain summary-only: {value}"
    );
}

fn plan_with_in_memory_history() -> PlanState {
    PlanState {
        plan_id: "plan-history".into(),
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: PlanMergeState::NotStarted,
        epic_id: None,
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "task-h".into(),
                agent: "codex".into(),
                task: "do thing".into(),
                depends_on: vec![],
                issue_id: None,
                context_files: vec![],
            },
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("attempt 2 in flight".into()),
            },
            result: None,
            worker_branch: Some("spur/worker-h-attempt-2".into()),
            attempt: 2,
            history: vec![AttemptRecord {
                attempt: 1,
                worker_branch: Some("spur/worker-h-attempt-1".into()),
                diff_summary: None,
                summary: Some("attempt 1 partial summary".into()),
                feedback: "needs error handling on null input".into(),
                dispatched_base_oid: None,
            }],
            last_delegation_id: None,
            dispatched_base_oid: None,
        }],
    }
}

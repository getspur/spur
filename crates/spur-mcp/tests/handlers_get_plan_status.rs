//! Phase 2 Task 9: freestanding `get_plan_status` handler + `PlanResolver` trait.
//!
//! Asserts the freestanding handler returns the same JSON shape that
//! `handle_get_plan_status` historically produced — via a mock
//! `PlanResolver` so the test does not depend on `McpCallbackServer`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use spur_mcp::handlers::{get_plan_status, McpHandlerError, PlanResolver, WorkerCallContext};
use spur_mcp::plan::outcomes::OutcomeStore;
use spur_mcp::plan::{PlanMergeState, PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus};
use tokio::sync::Mutex;

fn make_plan_state(plan_id: &str) -> PlanState {
    PlanState {
        plan_id: plan_id.to_string(),
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-test".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: PlanMergeState::NotStarted,
        epic_id: None,
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t-0".into(),
                agent: "codex".into(),
                task: "do the thing".into(),
                depends_on: vec![],
                issue_id: None,
                context_files: vec![],
            },
            status: PlanTaskStatus::Approved { summary: None },
            result: None,
            worker_branch: Some("spur/worker-t0".into()),
            attempt: 1,
            history: vec![],
            last_delegation_id: None,
            dispatched_base_oid: None,
        }],
    }
}

struct FixedPlanResolver {
    plan: Arc<Mutex<PlanState>>,
}

#[async_trait]
impl PlanResolver for FixedPlanResolver {
    async fn load_or_project_plan(
        &self,
        plan_id: &str,
    ) -> Result<Arc<Mutex<PlanState>>, String> {
        let p = self.plan.lock().await;
        if p.plan_id == plan_id {
            Ok(self.plan.clone())
        } else {
            Err(format!("unknown plan '{plan_id}'"))
        }
    }
}

struct MissingPlanResolver;

#[async_trait]
impl PlanResolver for MissingPlanResolver {
    async fn load_or_project_plan(
        &self,
        plan_id: &str,
    ) -> Result<Arc<Mutex<PlanState>>, String> {
        Err(format!("unknown plan '{plan_id}'"))
    }
}

fn ctx() -> WorkerCallContext {
    WorkerCallContext {
        delegation_id: "d".into(),
        brain_session_id: "b".into(),
    }
}

#[tokio::test]
async fn get_plan_status_returns_status_with_outcomes_fields() {
    let plan = Arc::new(Mutex::new(make_plan_state("p-1")));
    let resolver = FixedPlanResolver { plan };
    let outcomes = Mutex::new(OutcomeStore::default());

    let value = get_plan_status(&resolver, &outcomes, &ctx(), json!({ "plan_id": "p-1" }))
        .await
        .expect("handler should succeed");

    // Plan-level shape from build_plan_status.
    assert_eq!(value["plan_id"], "p-1");
    assert_eq!(value["status"], "approved");
    assert_eq!(value["ready_to_merge"], true);
    assert!(value["tasks"].is_array());
    assert_eq!(value["tasks"][0]["task_id"], "t-0");

    // Outcome enrichment must be merged into the top-level object.
    assert!(
        value["recent_outcomes"].is_array(),
        "recent_outcomes must be merged into the status object"
    );
    assert_eq!(value["recent_outcomes"].as_array().unwrap().len(), 0);
    assert!(
        value["stuck_tasks"].is_array(),
        "stuck_tasks must be merged into the status object"
    );
    assert_eq!(value["stuck_tasks"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn get_plan_status_missing_plan_id_invalid_params() {
    let plan = Arc::new(Mutex::new(make_plan_state("p-1")));
    let resolver = FixedPlanResolver { plan };
    let outcomes = Mutex::new(OutcomeStore::default());

    let err = get_plan_status(&resolver, &outcomes, &ctx(), json!({}))
        .await
        .expect_err("missing plan_id must be InvalidParams");

    assert!(matches!(err, McpHandlerError::InvalidParams(_)), "got {err:?}");
}

#[tokio::test]
async fn get_plan_status_unknown_plan_id_invalid_params() {
    let resolver = MissingPlanResolver;
    let outcomes = Mutex::new(OutcomeStore::default());

    let err = get_plan_status(
        &resolver,
        &outcomes,
        &ctx(),
        json!({ "plan_id": "does-not-exist" }),
    )
    .await
    .expect_err("unknown plan must surface as an error");

    assert!(
        matches!(err, McpHandlerError::InvalidParams(_)),
        "preserves -32602 (invalid_params) wire behavior; got {err:?}"
    );
}

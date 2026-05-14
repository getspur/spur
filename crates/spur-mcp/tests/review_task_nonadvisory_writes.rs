use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind, CompletionState};
use spur_mcp::plan::labels;
use spur_mcp::plan::test_util::MockPm;
use spur_mcp::plan::{
    handle_review_task_with_write_mode, PlanMergeState, PlanState, PlanTask, PlanTaskEntry,
    PlanTaskStatus, PmLike, ReviewWriteMode,
};
use spur_mcp::server::McpCallbackServer;
use tempfile::TempDir;
use tokio::sync::Mutex as AsyncMutex;

mod common;

#[derive(Default)]
struct FaultyReviewPm {
    calls: Mutex<usize>,
    fail_call: usize,
    comments: Mutex<BTreeMap<String, Vec<spur_pm::Comment>>>,
}

impl FaultyReviewPm {
    fn fail_on(fail_call: usize) -> Self {
        Self {
            fail_call,
            ..Self::default()
        }
    }

    fn comments_for(&self, issue_id: &str) -> Vec<AuditSentinelKind> {
        self.comments
            .lock()
            .expect("comments lock")
            .get(issue_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|comment| audit_sentinel::parse_comment(&comment.body)?.ok())
            .collect()
    }
}

struct FailingMockPm {
    inner: Arc<MockPm>,
}

impl FailingMockPm {
    fn new(inner: Arc<MockPm>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl PmLike for FaultyReviewPm {
    async fn update_issue(&self, id: &str, update: spur_pm::IssueUpdate) -> anyhow::Result<()> {
        let call_no = {
            let mut calls = self.calls.lock().expect("calls lock");
            *calls += 1;
            *calls
        };
        if call_no == self.fail_call {
            anyhow::bail!("injected review write failure on call {call_no}");
        }
        if let Some(body) = update.comment {
            let mut comments = self.comments.lock().expect("comments lock");
            let issue_comments = comments.entry(id.to_string()).or_default();
            issue_comments.push(spur_pm::Comment {
                id: format!("{id}-{}", issue_comments.len() + 1),
                body,
                actor: "test".into(),
                created_at: chrono::Utc::now(),
            });
        }
        Ok(())
    }

    fn closed_status(&self) -> &str {
        "closed"
    }

    fn advanced(&self) -> Option<&dyn spur_pm::BeadsAdvanced> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl PmLike for FailingMockPm {
    async fn get_issue(&self, id: &str) -> anyhow::Result<spur_pm::Issue> {
        self.inner.get_issue(id).await
    }

    async fn list_issues(
        &self,
        filter: spur_pm::IssueFilter,
    ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
        self.inner.list_issues(filter).await
    }

    async fn create_issue(&self, params: spur_pm::IssueCreate) -> anyhow::Result<String> {
        self.inner.create_issue(params).await
    }

    async fn update_issue(&self, id: &str, _update: spur_pm::IssueUpdate) -> anyhow::Result<()> {
        anyhow::bail!("injected review write failure for {id}");
    }

    async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
        self.inner.add_dependency(issue_id, depends_on_id).await
    }

    async fn issue_labels(&self, id: &str) -> anyhow::Result<Vec<String>> {
        self.inner.issue_labels(id).await
    }

    fn closed_status(&self) -> &str {
        "closed"
    }

    fn source_str(&self) -> &'static str {
        "beads"
    }

    fn advanced(&self) -> Option<&dyn spur_pm::BeadsAdvanced> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl spur_pm::BeadsAdvanced for FailingMockPm {
    async fn list_ready(
        &self,
        filter: spur_pm::ReadyFilter,
    ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
        self.inner
            .advanced()
            .expect("MockPm exposes advanced")
            .list_ready(filter)
            .await
    }

    async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<spur_pm::Comment>> {
        self.inner
            .advanced()
            .expect("MockPm exposes advanced")
            .list_comments(issue_id)
            .await
    }

    async fn add_comment(&self, issue_id: &str, body: &str) -> anyhow::Result<spur_pm::CommentId> {
        self.inner
            .advanced()
            .expect("MockPm exposes advanced")
            .add_comment(issue_id, body)
            .await
    }

    async fn remove_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
        self.inner
            .advanced()
            .expect("MockPm exposes advanced")
            .remove_dependency(issue_id, depends_on_id)
            .await
    }

    async fn dep_cycles(&self) -> anyhow::Result<Vec<spur_pm::DependencyCycle>> {
        self.inner
            .advanced()
            .expect("MockPm exposes advanced")
            .dep_cycles()
            .await
    }
}

#[async_trait::async_trait]
impl spur_pm::BeadsAdvanced for FaultyReviewPm {
    async fn list_ready(
        &self,
        _filter: spur_pm::ReadyFilter,
    ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
        Ok(Vec::new())
    }

    async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<spur_pm::Comment>> {
        Ok(self
            .comments
            .lock()
            .expect("comments lock")
            .get(issue_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn add_comment(
        &self,
        _issue_id: &str,
        _body: &str,
    ) -> anyhow::Result<spur_pm::CommentId> {
        unreachable!("review_task non-advisory path writes comments through update_issue")
    }

    async fn remove_dependency(&self, _issue_id: &str, _depends_on_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn dep_cycles(&self) -> anyhow::Result<Vec<spur_pm::DependencyCycle>> {
        Ok(Vec::new())
    }
}

fn awaiting_review_plan() -> Arc<AsyncMutex<PlanState>> {
    Arc::new(AsyncMutex::new(PlanState {
        plan_id: "nonadvisory-review".into(),
        brain_session_id: BrainSessionId::new(SessionId("brain".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: PlanMergeState::NotStarted,
        epic_id: Some("bd-epic".into()),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t1".into(),
                agent: "codex".into(),
                task: "Do the thing".into(),
                depends_on: vec![],
                issue_id: Some("bd-1".into()),
                issue_title: None,
                context_files: vec![],
            },
            status: PlanTaskStatus::AwaitingReview {
                summary: Some("ready".into()),
            },
            result: None,
            worker_branch: Some("feat/worker".into()),
            attempt: 1,
            history: vec![],
            last_delegation_id: Some("del-1".into()),
            dispatched_base_oid: Some("0000000000000000000000000000000000000001".into()),
        }],
    }))
}

#[tokio::test]
async fn nonadvisory_retry_does_not_duplicate_successful_audit_ops() {
    let plan = awaiting_review_plan();
    let pm = Arc::new(FaultyReviewPm::fail_on(3));
    let pm_trait: Arc<dyn PmLike> = pm.clone();

    handle_review_task_with_write_mode(
        Arc::clone(&plan),
        "nonadvisory-review",
        "t1",
        "approve",
        None,
        false,
        Some(pm_trait),
        None,
        None,
        None,
        common::server_builder::pro_feature_gate(),
        ReviewWriteMode::NonAdvisory,
    )
    .await
    .expect("retry should finish after injected partial failure");

    let task_approvals = pm
        .comments_for("bd-1")
        .into_iter()
        .filter(|audit| matches!(audit, AuditSentinelKind::Approval { .. }))
        .count();
    assert_eq!(
        task_approvals, 1,
        "successful task audit write must not be replayed on retry"
    );
}

#[tokio::test]
async fn nonadvisory_review_writes_epic_task_transition_audit() {
    let plan = awaiting_review_plan();
    let pm = Arc::new(FaultyReviewPm::fail_on(usize::MAX));
    let pm_trait: Arc<dyn PmLike> = pm.clone();

    handle_review_task_with_write_mode(
        Arc::clone(&plan),
        "nonadvisory-review",
        "t1",
        "approve",
        None,
        false,
        Some(pm_trait),
        None,
        None,
        None,
        common::server_builder::pro_feature_gate(),
        ReviewWriteMode::NonAdvisory,
    )
    .await
    .expect("review should persist");

    assert!(
        pm.comments_for("bd-epic").into_iter().any(|audit| {
            matches!(
                audit,
                AuditSentinelKind::TaskTransition {
                    plan_id,
                    task_id,
                    from_status,
                    to_status
                } if plan_id == "nonadvisory-review"
                    && task_id == "t1"
                    && from_status == "awaiting_review"
                    && to_status == "approved"
            )
        }),
        "non-advisory task transition must advance the epic audit sequence"
    );
}

fn decode_error_message(response: &Value) -> String {
    response["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn extract_submit_plan_id(response: &Value) -> String {
    assert!(
        response.get("error").is_none(),
        "submit_plan should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    text.lines()
        .find_map(|line| line.trim().strip_prefix("plan_id: "))
        .map(str::to_string)
        .expect("submit_plan response must include plan_id line")
}

fn run_git(repo: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation failed");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_git_repo(repo: &Path) {
    run_git(repo, &["init", "-q"]);
    run_git(repo, &["config", "user.email", "test@spur"]);
    run_git(repo, &["config", "user.name", "spur-test"]);
    std::fs::write(repo.join("seed.txt"), "seed\n").expect("write seed");
    run_git(repo, &["add", "seed.txt"]);
    run_git(repo, &["commit", "-q", "-m", "seed"]);
}

#[tokio::test]
async fn server_nonadvisory_review_error_invalidates_active_plan_cache() {
    let dir = TempDir::new().expect("tempdir");
    init_git_repo(dir.path());
    let mock_pm = MockPm::new().arc();
    let session_id = BrainSessionId::new(SessionId("brain".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        None,
        None,
        common::server_builder::continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        common::server_builder::pro_feature_gate(),
    );
    server.__test_set_pm_like(Arc::clone(&mock_pm) as Arc<dyn PmLike>);
    server.set_repo_root(dir.path().to_path_buf());
    server.set_nonadvisory_review_writes(true);
    let submit_response = server
        .__test_call_tool(
            "submit_plan",
            json!({
                "persist_as_epic": true,
                "epic_title": "Nonadvisory Review Failure",
                "tasks": [{
                    "task_id": "t1",
                    "agent": "codex",
                    "task": "Do the thing",
                    "depends_on": [],
                    "context_files": []
                }]
            }),
        )
        .await;
    let plan_id = extract_submit_plan_id(&submit_response);
    let projected = server
        .__test_load_or_project_plan(&plan_id)
        .await
        .expect("project submitted plan");
    let task_issue = projected.lock().await.tasks[0]
        .spec
        .issue_id
        .clone()
        .expect("submitted task issue id");
    mock_pm
        .update_issue(
            &task_issue,
            spur_pm::IssueUpdate {
                add_labels: vec![labels::READY_FOR_REVIEW.to_string()],
                comment: Some(audit_sentinel::encode_comment(
                    &AuditSentinelKind::Completion {
                        delegation_id: "del-1".into(),
                        worker_branch: Some("feat/worker".into()),
                        result_summary: Some("ready".into()),
                        completion_state: CompletionState::AwaitingReview,
                        superseded: false,
                        artifact_uri: None,
                        dispatched_base_oid: Some(
                            "0000000000000000000000000000000000000001".into(),
                        ),
                    },
                )),
                ..Default::default()
            },
        )
        .await
        .expect("seed durable awaiting-review completion");
    let projected = server
        .__test_load_or_project_plan(&plan_id)
        .await
        .expect("project awaiting-review plan");
    {
        let state = projected.lock().await;
        assert!(
            matches!(state.tasks[0].status, PlanTaskStatus::AwaitingReview { .. }),
            "durable MockPm projection should put t1 in AwaitingReview"
        );
    }
    assert_eq!(server.__test_active_plan_count().await, 1);
    let failing_pm: Arc<dyn PmLike> = Arc::new(FailingMockPm::new(Arc::clone(&mock_pm)));
    server.__test_set_pm_like(failing_pm);

    let response = server
        .__test_call_tool(
            "review_task",
            json!({
                "plan_id": plan_id,
                "task_id": "t1",
                "decision": "approve"
            }),
        )
        .await;
    assert!(
        response.get("error").is_some(),
        "injected MockPm write failure must surface through review_task: {response}"
    );
    assert_eq!(
        server.__test_active_plan_count().await,
        0,
        "server wrapper must invalidate cached plan on exhausted non-advisory write retries"
    );
    server.__test_set_pm_like(Arc::clone(&mock_pm) as Arc<dyn PmLike>);

    let status_response = server
        .__test_call_tool("get_plan_status", json!({ "plan_id": plan_id }))
        .await;
    if status_response.get("error").is_some() {
        assert!(
            !decode_error_message(&status_response).is_empty(),
            "error response should include a message: {status_response}"
        );
    } else {
        let text = status_response["result"]["content"][0]["text"]
            .as_str()
            .expect("get_plan_status response text");
        let status: Value = serde_json::from_str(text).expect("status json");
        assert_eq!(
            status["tasks"][0]["status"], "awaiting_review",
            "get_plan_status must re-project durable beads state after eviction"
        );
    }
}

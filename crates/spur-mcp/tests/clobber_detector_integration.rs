//! Approval-time clobber detector integration coverage.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind, CompletionState};
use spur_mcp::plan::{labels, signals};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use spur_pm::{IssueUpdate, PmService};
use tempfile::TempDir;

mod common;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git invocation failed");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
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

async fn beads_pm(repo: &Path) -> Arc<PmService> {
    Arc::new(
        PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

struct Fixture {
    _dir: TempDir,
    repo: std::path::PathBuf,
    pm: Arc<PmService>,
    server: McpCallbackServer,
}

impl Fixture {
    async fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        run_git(dir.path(), &["init", "-q", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@spur"]);
        run_git(dir.path(), &["config", "user.name", "spur-test"]);
        std::fs::write(dir.path().join("README.md"), "seed\n").expect("write seed");
        run_git(dir.path(), &["add", "README.md"]);
        run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
        run_br(dir.path(), &["init"]);

        let pm = beads_pm(dir.path()).await;
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let (mut server, _channel) = McpCallbackServer::new(
            &session_id,
            Some(Arc::clone(&pm)),
            None,
            continuation_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            common::server_builder::pro_feature_gate(),
        );
        server.set_repo_root(dir.path().to_path_buf());

        Self {
            repo: dir.path().to_path_buf(),
            _dir: dir,
            pm,
            server,
        }
    }

    async fn submit_plan(&self) -> (String, HashMap<String, String>) {
        let response = self
            .server
            .__test_call_submit_plan(json!({
                "persist_as_epic": true,
                "epic_title": "Clobber Detector Epic",
                "tasks": [
                    {
                        "task_id": "T1",
                        "agent": "codex",
                        "task": "create foo.rs with A content",
                        "depends_on": []
                    },
                    {
                        "task_id": "T2",
                        "agent": "codex",
                        "task": "create foo.rs with B content",
                        "depends_on": []
                    }
                ]
            }))
            .await;
        assert!(
            response.get("error").is_none(),
            "submit_plan should succeed: {response}"
        );
        extract_submit_details(&response)
    }

    async fn record_awaiting_review(
        &self,
        issue_id: &str,
        delegation_id: &str,
        worker_branch: &str,
        dispatched_base_oid: &str,
    ) {
        let body = audit_sentinel::encode_comment(&AuditSentinelKind::Completion {
            delegation_id: delegation_id.to_string(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some(worker_branch.to_string()),
            result_summary: Some("worker completed".to_string()),
            artifact_uri: None,
            dispatched_base_oid: Some(dispatched_base_oid.to_string()),
        });
        self.pm
            .advanced()
            .expect("beads advanced")
            .add_comment(issue_id, &body)
            .await
            .expect("completion audit comment");
        self.pm
            .update_issue(
                issue_id,
                IssueUpdate {
                    add_labels: vec![labels::READY_FOR_REVIEW.to_string()],
                    ..Default::default()
                },
            )
            .await
            .expect("mark ready for review");
    }

    async fn approve(&self, plan_id: &str, task_id: &str) -> Value {
        let response = self
            .server
            .__test_call_tool(
                "review_task",
                json!({
                    "plan_id": plan_id,
                    "task_id": task_id,
                    "decision": "approve"
                }),
            )
            .await;
        assert!(
            response.get("error").is_none(),
            "review_task approve should succeed: {response}"
        );
        parse_tool_text_json(&response)
    }
}

fn extract_submit_details(response: &Value) -> (String, HashMap<String, String>) {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    let plan_id = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("plan_id: "))
        .map(str::to_string)
        .expect("submit_plan response must include plan_id");
    let task_map = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("task_map: "))
        .map(|json| serde_json::from_str(json).expect("task_map json"))
        .expect("submit_plan response must include task_map");
    (plan_id, task_map)
}

fn parse_tool_text_json(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool response text");
    serde_json::from_str(text).expect("tool response JSON")
}

fn commit_file(repo: &Path, branch: &str, path: &str, content: &str) -> String {
    run_git(repo, &["checkout", "-q", "-B", branch, "main"]);
    std::fs::write(repo.join(path), content).expect("write worker file");
    run_git(repo, &["add", path]);
    run_git(repo, &["commit", "-q", "-m", &format!("write {path}")]);
    run_git(repo, &["rev-parse", "HEAD"])
}

#[tokio::test]
async fn approving_clobbering_worker_emits_potential_clobber_signal() {
    if !br_available() {
        eprintln!(
            "skipping approving_clobbering_worker_emits_potential_clobber_signal: `br` not on PATH"
        );
        return;
    }

    let fixture = Fixture::new().await;
    let (plan_id, task_map) = fixture.submit_plan().await;
    let t1_issue = task_map.get("T1").expect("T1 issue id");
    let t2_issue = task_map.get("T2").expect("T2 issue id");
    let base_oid = run_git(&fixture.repo, &["rev-parse", "main"]);

    commit_file(
        &fixture.repo,
        "spur/test-clobber-t1",
        "foo.rs",
        &"A".repeat(200),
    );
    fixture
        .record_awaiting_review(t1_issue, "del-t1", "spur/test-clobber-t1", &base_oid)
        .await;
    fixture.approve(&plan_id, "T1").await;

    commit_file(
        &fixture.repo,
        "spur/test-clobber-t2",
        "foo.rs",
        &"B".repeat(200),
    );
    fixture
        .record_awaiting_review(t2_issue, "del-t2", "spur/test-clobber-t2", &base_oid)
        .await;
    let approve_result = fixture.approve(&plan_id, "T2").await;

    let signals = approve_result["signals"]
        .as_array()
        .expect("approval response must include signals");
    assert_eq!(signals.len(), 1, "unexpected signals: {signals:?}");
    assert_eq!(signals[0]["kind"], "potential_clobber");
    assert_eq!(signals[0]["conflicting_task_id"], "T1");
    assert_eq!(signals[0]["file"], "foo.rs");

    let issue = fixture.pm.get_issue(t2_issue).await.expect("get T2 issue");
    assert!(
        issue
            .labels
            .iter()
            .any(|label| label == "signal:potential-clobber"),
        "T2 labels must include signal:potential-clobber: {:?}",
        issue.labels
    );

    let comments = fixture
        .pm
        .advanced()
        .expect("beads advanced")
        .list_comments(t2_issue)
        .await
        .expect("T2 comments");
    assert!(
        comments.iter().any(|comment| {
            comment.body.starts_with(signals::SENTINEL_PREFIX)
                && comment.body.contains("\"kind\":\"potential_clobber\"")
                && comment.body.contains("\"file\":\"foo.rs\"")
        }),
        "T2 comments must include potential_clobber sentinel: {:?}",
        comments
            .iter()
            .map(|comment| &comment.body)
            .collect::<Vec<_>>()
    );
}

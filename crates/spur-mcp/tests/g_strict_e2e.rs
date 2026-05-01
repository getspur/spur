//! End-to-end test: a 3-task persisted plan where T2 modifies T1's new
//! file and T3 imports both T1 and T2's symbols.
//!
//! Without G-strict, this is the bd-2dww failure mode: downstream workers
//! are based on stale main and lose upstream-created content. With G-strict,
//! the reconciler dispatches downstream tasks with the full approved
//! dependency overlay closure.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, DelegationResult, DelegationStatus, SessionId};
use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig, ReconcilerDispatchCtx};
use spur_mcp::server::{DetachedContinuationCtx, McpCallbackServer};
use spur_mcp::tools::{BaseSpec, BaseTarget, DelegationRequest};
use tempfile::TempDir;
use tokio::sync::{mpsc, Notify};
use tokio_util::task::TaskTracker;

mod common;

fn br_available() -> bool {
    Command::new("br")
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_command(repo: &Path, program: &str, args: &[&str]) -> String {
    let output = Command::new(program)
        .args(args)
        .current_dir(repo)
        .env("RUST_LOG", "error")
        .output()
        .unwrap_or_else(|error| panic!("{program} {args:?} failed to spawn: {error}"));

    assert!(
        output.status.success(),
        "{program} {args:?} failed (exit {}): stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn run_git(repo: &Path, args: &[&str]) -> String {
    run_command(repo, "git", args)
}

fn run_br(repo: &Path, args: &[&str]) -> String {
    run_command(repo, "br", args)
}

fn init_repo(repo: &Path) {
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(repo, &["config", "user.email", "test@spur"]);
    run_git(repo, &["config", "user.name", "spur-test"]);
    std::fs::write(repo.join("README.md"), "seed\n").expect("write seed");
    run_git(repo, &["add", "README.md"]);
    run_git(repo, &["commit", "-q", "-m", "seed"]);
    run_br(repo, &["init"]);
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

fn test_materializer() -> Arc<spur_mcp::outcome_materializer::OutcomeMaterializer> {
    Arc::new(spur_mcp::outcome_materializer::OutcomeMaterializer::new(
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
    ))
}

fn decode_tool_response(response: &Value) -> Value {
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "tool call should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool response text");
    serde_json::from_str(text).expect("tool response text must be JSON")
}

fn extract_submit_plan_id(response: &Value) -> String {
    assert!(
        response.get("error").is_none() || response["error"].is_null(),
        "submit_plan should succeed: {response}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    text.lines()
        .find_map(|line| line.trim().strip_prefix("plan_id: "))
        .unwrap_or_else(|| panic!("submit_plan response must include plan_id: {text}"))
        .to_string()
}

fn extract_submit_plan_task_map(response: &Value) -> HashMap<String, String> {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("submit_plan response text");
    let task_map_json = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("task_map: "))
        .unwrap_or_else(|| panic!("submit_plan response must include task_map: {text}"));
    serde_json::from_str(task_map_json).expect("task_map line must be valid JSON")
}

fn task_status(status: &Value, task_id: &str) -> Option<String> {
    status["tasks"].as_array().and_then(|tasks| {
        tasks
            .iter()
            .find(|task| task["task_id"] == task_id)
            .and_then(|task| task["status"].as_str())
            .map(str::to_string)
    })
}

struct TestHarness {
    _dir: TempDir,
    original_cwd: PathBuf,
    repo: PathBuf,
    server: McpCallbackServer,
    reconciler: Reconciler,
    request_rx: mpsc::Receiver<DelegationRequest>,
    task_tracker: TaskTracker,
    task_issue_ids: HashMap<String, String>,
}

impl TestHarness {
    async fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        init_repo(dir.path());
        let original_cwd = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(dir.path()).expect("set current dir to test repo");

        let pm = Arc::new(
            spur_pm::PmService::try_new(None, true, false, dir.path(), None)
                .await
                .expect("PmService::try_new failed")
                .expect("expected beads pm"),
        );
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let feature_gate = common::server_builder::pro_feature_gate();
        let (mut server, _unused_channel) = McpCallbackServer::new(
            &session_id,
            Some(Arc::clone(&pm)),
            None,
            continuation_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            Arc::clone(&feature_gate),
        );
        server.set_repo_root(dir.path().to_path_buf());

        let (delegation_tx, request_rx) = mpsc::channel(8);
        let task_tracker = TaskTracker::new();
        let reconciler = Reconciler::new(
            ReconcilerConfig {
                repo_root: dir.path().to_path_buf(),
                ..Default::default()
            },
            pm,
            Arc::new(Notify::new()),
            Some(ReconcilerDispatchCtx {
                delegation_tx,
                task_tracker: task_tracker.clone(),
                brain_session_id: session_id,
                event_sink: None,
                materializer: test_materializer(),
            }),
            None,
            feature_gate,
        );

        let repo = dir.path().to_path_buf();

        Self {
            _dir: dir,
            original_cwd,
            repo,
            server,
            reconciler,
            request_rx,
            task_tracker,
            task_issue_ids: HashMap::new(),
        }
    }

    async fn submit_plan(&mut self) -> String {
        let response = self
            .server
            .__test_call_submit_plan(json!({
                "persist_as_epic": true,
                "epic_title": "G-strict bd-2dww synthetic reproducer",
                "tasks": [
                    {
                        "task_id": "T1",
                        "agent": "mock",
                        "task": "create foo.rs with `pub struct Foo { pub n: u32 }`",
                        "depends_on": [],
                    },
                    {
                        "task_id": "T2",
                        "agent": "mock",
                        "task": "modify foo.rs to add `impl Foo { pub fn new(n: u32) -> Self { Self { n } } }`",
                        "depends_on": ["T1"],
                    },
                    {
                        "task_id": "T3",
                        "agent": "mock",
                        "task": "create main.rs with `use foo::Foo; fn main() { let _ = Foo::new(42); }`",
                        "depends_on": ["T1", "T2"],
                    },
                ]
            }))
            .await;
        let plan_id = extract_submit_plan_id(&response);
        self.task_issue_ids = extract_submit_plan_task_map(&response);
        plan_id
    }

    async fn dispatch_and_approve_with_mock<F>(&mut self, plan_id: &str, task_id: &str, worker: F)
    where
        F: FnOnce(&Path),
    {
        self.reconciler.tick_once().await.expect("reconciler tick");
        let request = tokio::time::timeout(Duration::from_secs(5), self.request_rx.recv())
            .await
            .expect("dispatch request timeout")
            .expect("dispatch request");

        let expected_issue_id = self
            .task_issue_ids
            .get(task_id)
            .unwrap_or_else(|| panic!("missing issue id for {task_id}"));
        assert_eq!(
            request.issue_id.as_deref(),
            Some(expected_issue_id.as_str()),
            "reconciler must dispatch {task_id}"
        );

        self.run_mock_worker(task_id, request, worker).await;
        self.wait_for_task_status(plan_id, task_id, "awaiting_review")
            .await;

        let response = self
            .server
            .__test_call_tool(
                "review_task",
                json!({
                    "plan_id": plan_id,
                    "task_id": task_id,
                    "decision": "approve",
                    "feedback": format!("{task_id} approved for G-strict e2e test"),
                }),
            )
            .await;
        let status = decode_tool_response(&response);
        assert_eq!(
            task_status(&status, task_id).as_deref(),
            Some("approved"),
            "{task_id} should be approved: {status}"
        );
    }

    async fn wait_for_task_status(&self, plan_id: &str, task_id: &str, expected: &str) -> Value {
        for _ in 0..50 {
            let status = self.plan_status(plan_id).await;
            if task_status(&status, task_id).as_deref() == Some(expected) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let status = self.plan_status(plan_id).await;
        panic!("timed out waiting for {task_id}={expected}; final status: {status}");
    }

    async fn plan_status(&self, plan_id: &str) -> Value {
        decode_tool_response(
            &self
                .server
                .__test_call_tool("get_plan_status", json!({ "plan_id": plan_id }))
                .await,
        )
    }

    async fn merge_plan(&self, plan_id: &str) -> Value {
        decode_tool_response(
            &self
                .server
                .__test_call_tool("merge_plan", json!({ "plan_id": plan_id }))
                .await,
        )
    }

    async fn run_mock_worker<F>(&self, task_id: &str, request: DelegationRequest, worker: F)
    where
        F: FnOnce(&Path),
    {
        let branch = format!("spur/g-strict-e2e-{task_id}");
        let worktree = self.repo.join(".spur/worktrees").join(task_id);
        self.create_worker_worktree(&worktree, &branch, request.base.as_ref());

        let dispatched_base_oid = run_git(&worktree, &["rev-parse", "HEAD"]);
        if let Some(tx) = request.dispatched_base_oid_tx.as_ref() {
            tx.send(Some(dispatched_base_oid))
                .expect("publish dispatched base oid");
        }

        worker(&worktree);

        run_git(&worktree, &["add", "."]);
        run_git(
            &worktree,
            &["commit", "-q", "-m", &format!("{task_id} contribution")],
        );

        request
            .respond_to
            .send(DelegationResult {
                status: DelegationStatus::Success,
                diff: None,
                diff_summary: None,
                summary: Some(format!("{task_id} complete")),
                estimated_cost_usd: 0.0,
                worker_branch: Some(branch),
                artifact: None,
            })
            .expect("send delegation result");

        // Keep the linked worktree alive until TempDir cleanup. Removing it
        // immediately after replying races the reconciler's branch invariant
        // checks on some git versions.
    }

    fn create_worker_worktree(&self, worktree: &Path, branch: &str, base: Option<&BaseSpec>) {
        std::fs::create_dir_all(worktree.parent().expect("worktree parent"))
            .expect("create worktree parent");

        match base {
            Some(BaseSpec::WithOverlay { base, overlays }) => {
                let base_ref = base_target_ref(base);
                self.add_worktree(worktree, branch, &base_ref);
                for overlay in overlays {
                    let range = format!("{}..{}", overlay.base_oid, overlay.tip_oid);
                    run_git(worktree, &["cherry-pick", &range]);
                }
            }
            Some(BaseSpec::Branch { name }) => self.add_worktree(worktree, branch, name),
            Some(BaseSpec::Commit { oid }) => self.add_worktree(worktree, branch, oid),
            Some(BaseSpec::RepoMain) | None => self.add_worktree(worktree, branch, "HEAD"),
        }
    }

    fn add_worktree(&self, worktree: &Path, branch: &str, base_ref: &str) {
        let worktree_str = worktree.to_str().expect("worktree path utf8");
        run_git(
            &self.repo,
            &["worktree", "add", worktree_str, "-b", branch, base_ref],
        );
    }

    fn show(&self, branch: &str, path: &str) -> String {
        run_git(&self.repo, &["show", &format!("{branch}:{path}")])
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        self.task_tracker.close();
        let _ = std::env::set_current_dir(&self.original_cwd);
    }
}

fn base_target_ref(base: &BaseTarget) -> String {
    match base {
        BaseTarget::RepoMain => "HEAD".to_string(),
        BaseTarget::Branch { name } => name.clone(),
        BaseTarget::Commit { oid } => oid.clone(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn g_strict_prevents_bd_2dww_class_loss() {
    if !br_available() {
        eprintln!("skipping g_strict_prevents_bd_2dww_class_loss: `br` not on PATH");
        return;
    }

    let mut harness = TestHarness::new().await;
    let plan_id = harness.submit_plan().await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T1", |worktree| {
            std::fs::write(worktree.join("foo.rs"), "pub struct Foo { pub n: u32 }\n")
                .expect("write T1 foo.rs");
        })
        .await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T2", |worktree| {
            let foo_path = worktree.join("foo.rs");
            let existing =
                std::fs::read_to_string(&foo_path).expect("T2 worker must see foo.rs from T1");
            assert!(
                existing.contains("pub struct Foo"),
                "T2 worker must see T1 struct, got: {existing}"
            );
            std::fs::write(
                foo_path,
                format!(
                    "{existing}\nimpl Foo {{ pub fn new(n: u32) -> Self {{ Self {{ n }} }} }}\n"
                ),
            )
            .expect("write T2 foo.rs");
        })
        .await;

    harness
        .dispatch_and_approve_with_mock(&plan_id, "T3", |worktree| {
            let foo = std::fs::read_to_string(worktree.join("foo.rs"))
                .expect("T3 worker must see foo.rs from T1+T2");
            assert!(
                foo.contains("pub struct Foo"),
                "T3 worker must see T1 struct, got: {foo}"
            );
            assert!(
                foo.contains("impl Foo"),
                "T3 worker must see T2 impl, got: {foo}"
            );
            std::fs::write(
                worktree.join("main.rs"),
                "use foo::Foo;\nfn main() { let _ = Foo::new(42); }\n",
            )
            .expect("write T3 main.rs");
        })
        .await;

    let merge_status = harness.merge_plan(&plan_id).await;
    assert_eq!(
        merge_status["merge"]["status"], "succeeded",
        "merge_plan must succeed: {merge_status}"
    );
    let merge_branch = merge_status["merge"]["merge_branch"]
        .as_str()
        .expect("merge branch");

    let foo = harness.show(merge_branch, "foo.rs");
    assert!(
        foo.contains("pub struct Foo"),
        "merged foo.rs must retain T1 struct, got: {foo}"
    );
    assert!(
        foo.contains("impl Foo"),
        "merged foo.rs must retain T2 impl, got: {foo}"
    );

    let main = harness.show(merge_branch, "main.rs");
    assert!(
        main.contains("use foo::Foo"),
        "merged main.rs must retain T3 import, got: {main}"
    );
}

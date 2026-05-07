#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, DelegationResult, DelegationStatus, SessionId};
use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind};
use spur_mcp::plan::reconciler::{Reconciler, ReconcilerConfig, ReconcilerDispatchCtx};
use spur_mcp::server::McpCallbackServer;
use spur_mcp::tools::{BaseSpec, BaseTarget, DelegationRequest};
use tempfile::TempDir;
use tokio::sync::{mpsc, Notify, OwnedMutexGuard};
use tokio_util::task::TaskTracker;

static CWD_LOCK: LazyLock<Arc<tokio::sync::Mutex<()>>> =
    LazyLock::new(|| Arc::new(tokio::sync::Mutex::new(())));
const STATUS_POLL_DEADLINE: Duration = Duration::from_secs(60);

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
    super::beads::run_br(repo, args)
        .unwrap_or_else(|err| panic!("test beads command {args:?} failed: {err}"))
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

fn collect_audit_sentinels(list_json: &str) -> Vec<AuditSentinelKind> {
    let items: Value = serde_json::from_str(list_json).expect("br comments list must be JSON");
    items
        .as_array()
        .expect("comments must be a JSON array")
        .iter()
        .filter_map(|comment| comment.get("text").and_then(Value::as_str))
        .filter_map(audit_sentinel::parse_comment)
        .filter_map(Result::ok)
        .collect()
}

pub struct TestHarness {
    _dir: TempDir,
    _cwd_guard: OwnedMutexGuard<()>,
    original_cwd: PathBuf,
    repo: PathBuf,
    server: McpCallbackServer,
    reconciler: Reconciler,
    request_rx: mpsc::Receiver<DelegationRequest>,
    pending_requests: VecDeque<DelegationRequest>,
    task_tracker: TaskTracker,
    task_issue_ids: HashMap<String, String>,
    fault_injection_hooks: FaultInjectionHooks,
}

#[derive(Default, Debug, Clone)]
pub struct FaultInjectionHooks {
    pub panic_after_overlay_apply: Option<String>,
}

impl TestHarness {
    pub async fn new() -> Self {
        let cwd_guard = CWD_LOCK.clone().lock_owned().await;
        let dir = TempDir::new().expect("tempdir");
        init_repo(dir.path());
        let original_cwd = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(dir.path()).expect("set current dir to test repo");

        let repo = dir.path().to_path_buf();
        let (server, reconciler, request_rx, task_tracker) = Self::new_runtime(&repo).await;

        Self {
            _dir: dir,
            _cwd_guard: cwd_guard,
            original_cwd,
            repo,
            server,
            reconciler,
            request_rx,
            pending_requests: VecDeque::new(),
            task_tracker,
            task_issue_ids: HashMap::new(),
            fault_injection_hooks: FaultInjectionHooks::default(),
        }
    }

    pub fn set_fault_injection(&mut self, hooks: FaultInjectionHooks) {
        self.fault_injection_hooks = hooks;
    }

    pub fn repo_root(&self) -> PathBuf {
        self.repo.clone()
    }

    pub fn beads_db_path(&self) -> PathBuf {
        self.repo.join(".beads")
    }

    pub async fn reopen_existing_beads(mut self, beads_path: PathBuf, repo_path: PathBuf) -> Self {
        assert_eq!(
            repo_path, self.repo,
            "reopen_existing_beads must use the original repo root"
        );
        assert_eq!(
            beads_path,
            self.beads_db_path(),
            "reopen_existing_beads must point at the original beads DB"
        );
        assert!(
            beads_path.is_dir(),
            "reopen_existing_beads requires an existing beads DB at {}",
            beads_path.display()
        );

        self.task_tracker.close();
        let (server, reconciler, request_rx, task_tracker) = Self::new_runtime(&self.repo).await;
        self.server = server;
        self.reconciler = reconciler;
        self.request_rx = request_rx;
        self.pending_requests.clear();
        self.task_tracker = task_tracker;
        self.fault_injection_hooks = FaultInjectionHooks::default();

        assert_eq!(
            self.server.__test_active_plan_count().await,
            0,
            "fresh server must start with an empty active_plans cache"
        );

        self
    }

    async fn new_runtime(
        repo: &Path,
    ) -> (
        McpCallbackServer,
        Reconciler,
        mpsc::Receiver<DelegationRequest>,
        TaskTracker,
    ) {
        let pm = Arc::new(
            spur_pm::PmService::try_new(None, true, false, repo, None)
                .await
                .expect("PmService::try_new failed")
                .expect("expected beads pm"),
        );
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let feature_gate = super::server_builder::pro_feature_gate();
        let (mut server, _unused_channel) = McpCallbackServer::new(
            Some(&session_id),
            Some(Arc::clone(&pm)),
            None,
            super::server_builder::continuation_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            Arc::clone(&feature_gate),
        );
        server.set_repo_root(repo.to_path_buf());

        let (delegation_tx, request_rx) = mpsc::channel(8);
        let task_tracker = TaskTracker::new();
        let reconciler = Reconciler::new(
            ReconcilerConfig {
                repo_root: repo.to_path_buf(),
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
                continuation_ctx: super::server_builder::continuation_ctx_arc(),
            }),
            None,
            feature_gate,
        );

        (server, reconciler, request_rx, task_tracker)
    }

    pub async fn submit_plan_with_tasks(&mut self, epic_title: &str, tasks: Value) -> String {
        let response = self
            .server
            .__test_call_submit_plan(json!({
                "persist_as_epic": true,
                "epic_title": epic_title,
                "tasks": tasks,
            }))
            .await;
        let plan_id = extract_submit_plan_id(&response);
        self.task_issue_ids = extract_submit_plan_task_map(&response);
        plan_id
    }

    pub async fn server_test_submit_plan(&self, args: Value) -> Value {
        self.server.__test_call_submit_plan(args).await
    }

    pub async fn tick_until_request_or_timeout(&mut self) {
        for _ in 0..20 {
            if !self.pending_requests.is_empty() {
                return;
            }

            self.reconciler.tick_once().await.expect("reconciler tick");
            match tokio::time::timeout(Duration::from_millis(100), self.request_rx.recv()).await {
                Ok(Some(request)) => {
                    self.pending_requests.push_back(request);
                    return;
                }
                Ok(None) => panic!("delegation request channel closed"),
                Err(_) => {}
            }
        }

        panic!("timed out waiting for reconciler dispatch request");
    }

    pub fn take_next_dispatch(&mut self) -> Option<DelegationRequest> {
        self.pending_requests.pop_front()
    }

    pub async fn submit_plan(&mut self) -> String {
        self.submit_plan_with_tasks(
            "G-strict bd-2dww synthetic reproducer",
            json!([
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
            ]),
        )
        .await
    }

    pub async fn submit_diamond_plan(&mut self) -> String {
        self.submit_plan_with_tasks(
            "G-strict diamond DAG closure reproducer",
            json!([
                {
                    "task_id": "T1",
                    "agent": "mock",
                    "task": "create a.rs with `pub struct A;`",
                    "depends_on": [],
                },
                {
                    "task_id": "T2",
                    "agent": "mock",
                    "task": "create b.rs with `pub struct B;`",
                    "depends_on": [],
                },
                {
                    "task_id": "T3",
                    "agent": "mock",
                    "task": "create c.rs using both A and B",
                    "depends_on": ["T1", "T2"],
                },
            ]),
        )
        .await
    }

    pub async fn submit_grandparent_plan(&mut self) -> String {
        self.submit_plan_with_tasks(
            "G-strict grandparent depth closure reproducer",
            json!([
                {
                    "task_id": "T1",
                    "agent": "mock",
                    "task": "create lvl1.rs",
                    "depends_on": [],
                },
                {
                    "task_id": "T2",
                    "agent": "mock",
                    "task": "create lvl2.rs after reading lvl1.rs",
                    "depends_on": ["T1"],
                },
                {
                    "task_id": "T3",
                    "agent": "mock",
                    "task": "create lvl3.rs after reading lvl1.rs and lvl2.rs",
                    "depends_on": ["T2"],
                },
                {
                    "task_id": "T4",
                    "agent": "mock",
                    "task": "create lvl4.rs after reading lvl1.rs, lvl2.rs, and lvl3.rs",
                    "depends_on": ["T3"],
                },
            ]),
        )
        .await
    }

    pub async fn dispatch_and_approve_with_mock<F>(
        &mut self,
        plan_id: &str,
        task_id: &str,
        worker: F,
    ) where
        F: FnOnce(&Path) + Send + 'static,
    {
        self.reconciler.tick_once().await.expect("reconciler tick");
        let request = self.dispatch_request_for_task(task_id).await;
        let repo = self.repo.clone();
        let worker_task_id = task_id.to_string();

        tokio::task::spawn_blocking(move || {
            run_mock_worker_sync(&repo, &worker_task_id, request, worker)
        })
        .await
        .expect("mock worker task");
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

    async fn dispatch_request_for_task(&mut self, task_id: &str) -> DelegationRequest {
        let expected_issue_id = self
            .task_issue_ids
            .get(task_id)
            .unwrap_or_else(|| panic!("missing issue id for {task_id}"));

        if let Some(index) = self
            .pending_requests
            .iter()
            .position(|request| request.issue_id.as_deref() == Some(expected_issue_id.as_str()))
        {
            return self
                .pending_requests
                .remove(index)
                .expect("pending request index exists");
        }

        loop {
            let request = tokio::time::timeout(Duration::from_secs(5), self.request_rx.recv())
                .await
                .expect("dispatch request timeout")
                .expect("dispatch request");

            if request.issue_id.as_deref() == Some(expected_issue_id.as_str()) {
                return request;
            }

            self.pending_requests.push_back(request);
        }
    }

    pub async fn dispatch_and_panic_after_overlay_apply(&mut self, task_id: &str) -> String {
        self.reconciler.tick_once().await.expect("reconciler tick");
        let request = self.dispatch_request_for_task(task_id).await;
        let hooks = self.fault_injection_hooks.clone();
        let repo = self.repo.clone();
        let task_id = task_id.to_string();

        let handle = tokio::task::spawn_blocking(move || {
            run_mock_worker_until_oid_send(&repo, &task_id, request, hooks)
        });
        let panic = handle
            .await
            .expect_err("fault injection must panic before OID send");
        assert!(
            panic.is_panic(),
            "fault injection task must panic: {panic:?}"
        );

        let panic_message = panic
            .try_into_panic()
            .ok()
            .and_then(|payload| {
                payload.downcast_ref::<String>().cloned().or_else(|| {
                    payload
                        .downcast_ref::<&'static str>()
                        .map(|s| (*s).to_string())
                })
            })
            .unwrap_or_else(|| "unknown panic payload".to_string());

        self.task_tracker.close();
        tokio::time::timeout(Duration::from_secs(60), self.task_tracker.wait())
            .await
            .expect("dropped completion task should finish after worker panic");

        panic_message
    }

    pub async fn wait_for_task_status(
        &self,
        plan_id: &str,
        task_id: &str,
        expected: &str,
    ) -> Value {
        let start = tokio::time::Instant::now();
        while start.elapsed() < STATUS_POLL_DEADLINE {
            let status = self.plan_status(plan_id).await;
            if task_status(&status, task_id).as_deref() == Some(expected) {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let status = self.plan_status(plan_id).await;
        panic!("timed out waiting for {task_id}={expected}; final status: {status}");
    }

    pub async fn wait_for_terminal(&self, plan_id: &str, task_id: &str) -> Value {
        let start = tokio::time::Instant::now();
        while start.elapsed() < STATUS_POLL_DEADLINE {
            let status = self.plan_status(plan_id).await;
            if let Some(task) = self.task_status_entry(&status, task_id) {
                if matches!(
                    task["status"].as_str(),
                    Some("approved" | "failed" | "cancelled" | "superseded")
                ) {
                    return task.clone();
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        let status = self.plan_status(plan_id).await;
        panic!("timed out waiting for {task_id} terminal; final status: {status}");
    }

    pub fn task_status_entry<'a>(&self, status: &'a Value, task_id: &str) -> Option<&'a Value> {
        status["tasks"]
            .as_array()
            .and_then(|tasks| tasks.iter().find(|task| task["task_id"] == task_id))
    }

    pub async fn plan_status(&self, plan_id: &str) -> Value {
        decode_tool_response(
            &self
                .server
                .__test_call_tool("get_plan_status", json!({ "plan_id": plan_id }))
                .await,
        )
    }

    pub async fn merge_plan(&self, plan_id: &str) -> Value {
        decode_tool_response(
            &self
                .server
                .__test_call_tool("merge_plan", json!({ "plan_id": plan_id }))
                .await,
        )
    }

    pub async fn get_task_diff(&self, plan_id: &str, task_id: &str) -> Value {
        decode_tool_response(
            &self
                .server
                .__test_call_tool(
                    "get_task_diff",
                    json!({ "plan_id": plan_id, "task_id": task_id }),
                )
                .await,
        )
    }

    pub fn completion_dispatched_base_oid(&self, task_id: &str) -> String {
        let issue_id = self
            .task_issue_ids
            .get(task_id)
            .unwrap_or_else(|| panic!("missing issue id for {task_id}"));
        let comments = run_br(&self.repo, &["comments", "list", issue_id, "--json"]);

        collect_audit_sentinels(&comments)
            .into_iter()
            .filter_map(|sentinel| match sentinel {
                AuditSentinelKind::Completion {
                    dispatched_base_oid,
                    ..
                } => dispatched_base_oid,
                _ => None,
            })
            .next_back()
            .unwrap_or_else(|| panic!("missing completion dispatched_base_oid for {task_id}"))
    }

    pub fn latest_completion_audit_for(&self, task_id: &str) -> AuditSentinelKind {
        let issue_id = self
            .task_issue_ids
            .get(task_id)
            .unwrap_or_else(|| panic!("missing issue id for {task_id}"));
        let comments = run_br(&self.repo, &["comments", "list", issue_id, "--json"]);

        collect_audit_sentinels(&comments)
            .into_iter()
            .rfind(|sentinel| matches!(sentinel, AuditSentinelKind::Completion { .. }))
            .unwrap_or_else(|| panic!("missing completion audit for {task_id}"))
    }

    pub fn add_audit_comment_for_task(&self, task_id: &str, audit: &AuditSentinelKind) {
        let issue_id = self
            .task_issue_ids
            .get(task_id)
            .unwrap_or_else(|| panic!("missing issue id for {task_id}"));
        let body = audit_sentinel::encode_comment(audit);
        run_br(&self.repo, &["comments", "add", issue_id, &body]);
    }

    pub async fn tick_reconciler(&self) -> anyhow::Result<bool> {
        self.reconciler.tick_once().await
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

    pub fn show(&self, branch: &str, path: &str) -> String {
        run_git(&self.repo, &["show", &format!("{branch}:{path}")])
    }
}

fn run_mock_worker_until_oid_send(
    repo: &Path,
    task_id: &str,
    request: DelegationRequest,
    hooks: FaultInjectionHooks,
) {
    let branch = format!("spur/g-strict-e2e-{task_id}");
    let worktree = repo.join(".spur/worktrees").join(task_id);
    create_worker_worktree(repo, &worktree, &branch, request.base.as_ref());

    if let Some(message) = hooks.panic_after_overlay_apply {
        panic!("fault injection: {message}");
    }

    let dispatched_base_oid = run_git(&worktree, &["rev-parse", "HEAD"]);
    if let Some(tx) = request.dispatched_base_oid_tx.as_ref() {
        tx.send(Some(dispatched_base_oid))
            .expect("publish dispatched base oid");
    }
}

fn run_mock_worker_sync<F>(repo: &Path, task_id: &str, request: DelegationRequest, worker: F)
where
    F: FnOnce(&Path),
{
    let branch = format!("spur/g-strict-e2e-{task_id}");
    let worktree = repo.join(".spur/worktrees").join(task_id);
    create_worker_worktree(repo, &worktree, &branch, request.base.as_ref());

    let dispatched_base_oid = run_git(&worktree, &["rev-parse", "HEAD"]);
    if let Some(tx) = request.dispatched_base_oid_tx.as_ref() {
        tx.send(Some(dispatched_base_oid.clone()))
            .expect("publish dispatched base oid");
    }

    worker(&worktree);

    run_git(&worktree, &["add", "."]);
    run_git(
        &worktree,
        &["commit", "-q", "-m", &format!("{task_id} contribution")],
    );
    let diff_range = format!("{dispatched_base_oid}..HEAD");
    let diff = run_git(&worktree, &["diff", &diff_range]);

    request
        .respond_to
        .send(DelegationResult {
            status: DelegationStatus::Success,
            diff: Some(diff),
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

fn create_worker_worktree(repo: &Path, worktree: &Path, branch: &str, base: Option<&BaseSpec>) {
    std::fs::create_dir_all(worktree.parent().expect("worktree parent"))
        .expect("create worktree parent");

    match base {
        Some(BaseSpec::WithOverlay { base, overlays }) => {
            let base_ref = base_target_ref(base);
            add_worktree(repo, worktree, branch, &base_ref);
            for overlay in overlays {
                let range = format!("{}..{}", overlay.base_oid, overlay.tip_oid);
                run_git(worktree, &["cherry-pick", &range]);
            }
        }
        Some(BaseSpec::Branch { name }) => add_worktree(repo, worktree, branch, name),
        Some(BaseSpec::Commit { oid }) => add_worktree(repo, worktree, branch, oid),
        Some(BaseSpec::RepoMain) | None => add_worktree(repo, worktree, branch, "HEAD"),
    }
}

fn add_worktree(repo: &Path, worktree: &Path, branch: &str, base_ref: &str) {
    let worktree_str = worktree.to_str().expect("worktree path utf8");
    run_git(
        repo,
        &["worktree", "add", worktree_str, "-b", branch, base_ref],
    );
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

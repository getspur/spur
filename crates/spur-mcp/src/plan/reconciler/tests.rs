use super::*;
use proptest::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

fn pro_feature_gate() -> Arc<spur_license::FeatureGate> {
    let gate = Arc::new(spur_license::FeatureGate::new(
        spur_license::policy::PolicyResolver::embedded(),
    ));
    let features =
        std::collections::BTreeSet::from([spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED
            .as_str()
            .to_string()]);
    gate.update_state(&spur_license::LicenseState::active_validated(
        spur_license::Plan::Pro,
        features,
    ));
    gate
}

fn projected_test_state(plan_id: &str) -> crate::plan::PlanState {
    crate::plan::PlanState {
        plan_id: plan_id.to_string(),
        tasks: Vec::new(),
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("test-brain".into())),
        base_snapshot_branch: None,
        base_snapshot_oid: None,
        merge_state: crate::plan::PlanMergeState::NotStarted,
        epic_id: None,
    }
}

#[tokio::test]
async fn projected_plan_for_ready_reuses_hydrated_state_without_projecting() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let hydrated = Arc::new(projected_test_state("hydrated-plan"));
    let fallback_polled = Arc::new(AtomicBool::new(false));
    let fallback_polled_for_future = Arc::clone(&fallback_polled);

    let projected = projected_plan_for_ready(Some(Arc::clone(&hydrated)), async move {
        fallback_polled_for_future.store(true, Ordering::SeqCst);
        Ok(projected_test_state("fallback-plan"))
    })
    .await
    .expect("hydrated plan state should be returned");

    assert!(Arc::ptr_eq(&projected, &hydrated));
    assert!(!fallback_polled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn projected_plan_for_ready_projects_when_unhydrated() {
    let projected =
        projected_plan_for_ready(None, async { Ok(projected_test_state("fallback-plan")) })
            .await
            .expect("fallback projection should be used");

    assert_eq!(projected.plan_id, "fallback-plan");
}

#[test]
fn reconciler_dispatch_ctx_can_be_cloned_for_server_startup() {
    let (tx, _rx) = tokio::sync::mpsc::channel::<crate::tools::DelegationRequest>(1);
    let ctx = super::ReconcilerDispatchCtx {
        delegation_tx: tx,
        task_tracker: tokio_util::task::TaskTracker::new(),
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into())),
        event_sink: None,
        materializer: Arc::new(crate::outcome_materializer::OutcomeMaterializer::new(
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        )),
        continuation_ctx: Arc::new(crate::server::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }),
    };

    let cloned = ctx.clone();
    assert_eq!(cloned.brain_session_id, ctx.brain_session_id);
}

#[test]
fn prior_branch_for_reuse_uses_last_attempt_only_when_reuse_requested() {
    let task = crate::plan::PlanTaskEntry {
        spec: crate::plan::PlanTask {
            task_id: "T1".into(),
            agent: "codex".into(),
            task: "Task".into(),
            depends_on: vec![],
            issue_id: Some("bd-1".into()),
            issue_title: None,
            context_files: vec![],
        },
        status: crate::plan::PlanTaskStatus::Ready,
        result: None,
        worker_branch: None,
        attempt: 2,
        history: vec![
            crate::plan::AttemptRecord {
                attempt: 1,
                worker_branch: Some("spur/worker-old".into()),
                diff_summary: None,
                summary: None,
                feedback: "first".into(),
                dispatched_base_oid: None,
                reuse_prior_worktree: None,
            },
            crate::plan::AttemptRecord {
                attempt: 2,
                worker_branch: Some("spur/worker-reuse".into()),
                diff_summary: None,
                summary: None,
                feedback: "second".into(),
                dispatched_base_oid: None,
                reuse_prior_worktree: Some(true),
            },
        ],
        last_delegation_id: None,
        dispatched_base_oid: None,
    };

    assert_eq!(
        super::prior_branch_for_reuse(&task),
        Some("spur/worker-reuse".into())
    );
}

fn summary(id: &str, status: &str) -> spur_pm::IssueSummary {
    spur_pm::IssueSummary {
        id: id.into(),
        source: spur_pm::PmSource::Beads,
        title: id.into(),
        status: status.into(),
        labels: vec![],
        url: format!("https://example.invalid/{id}"),
        priority: None,
        issue_type: Some("task".into()),
        assignee: None,
        description: None,
    }
}

#[test]
fn classify_epic_completion_reports_all_approved() {
    let children = vec![summary("bd-1", "closed"), summary("bd-2", "closed")];
    let outcome = super::terminal::classify_epic_completion(&children, "closed").expect("terminal");
    assert_eq!(
        outcome.audit_outcome,
        crate::plan::audit_sentinel::EpicCompletionOutcome::AllApproved
    );
    assert!(outcome.add_integration_pending);
}

#[test]
fn classify_epic_completion_reports_terminal_failures() {
    let mut rejected = summary("bd-2", "closed");
    rejected.labels.push("rejected".into());
    let children = vec![summary("bd-1", "closed"), rejected];
    let outcome = super::terminal::classify_epic_completion(&children, "closed").expect("terminal");
    assert_eq!(
        outcome.audit_outcome,
        crate::plan::audit_sentinel::EpicCompletionOutcome::TerminalWithFailures
    );
    assert!(!outcome.add_integration_pending);
}

/// D1 fix coverage: verify that the biased select! pattern used inside
/// `Reconciler::run` to race `tick_once` against `cancel` actually
/// preempts an in-flight future when cancel fires. Uses a pending future
/// as a stand-in for a stuck `bv.triage`/`br ready` call; without the
/// biased cancel race, the task would hang indefinitely.
#[tokio::test]
async fn biased_select_cancel_preempts_pending_tick() {
    use std::future::pending;
    use tokio::sync::oneshot;

    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    tokio::pin!(cancel_rx);

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        let _ = cancel_tx.send(());
    });

    let blocking = pending::<anyhow::Result<bool>>();
    tokio::pin!(blocking);

    let outcome = tokio::time::timeout(Duration::from_secs(1), async move {
        tokio::select! {
            biased;
            _ = &mut cancel_rx => "cancelled",
            _ = &mut blocking => "tick_completed",
        }
    })
    .await
    .expect("select must not hang when cancel is live");

    assert_eq!(outcome, "cancelled");
}

#[test]
fn cadence_backoff_formula() {
    let cfg = ReconcilerConfig {
        base_interval: Duration::from_secs(1),
        idle_ceiling: Duration::from_secs(8),
        backoff_factor: 2,
        ..Default::default()
    };
    let mut d = cfg.base_interval;
    let mut hist = vec![d];
    for _ in 0..5 {
        d = std::cmp::min(d.saturating_mul(cfg.backoff_factor), cfg.idle_ceiling);
        hist.push(d);
    }
    assert_eq!(
        hist,
        vec![
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(8),
            Duration::from_secs(8),
        ]
    );
}

/// Regression: journal monitor must exit promptly when aborted so that
/// graceful shutdown does not hang forever awaiting the handle and so
/// that abort/drop does not leak a detached polling task.
#[tokio::test]
async fn journal_monitor_exits_on_abort_without_hang() {
    use std::time::Duration;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("journal");
    tokio::fs::write(&path, b"x").await.expect("write");
    let notify = Arc::new(Notify::new());
    let handle = tokio::spawn(monitor_journal_appends(path, notify));
    handle.abort();
    let result = tokio::time::timeout(Duration::from_secs(1), handle)
        .await
        .expect("monitor must exit within 1s of abort");
    assert!(
        result.is_err() && result.unwrap_err().is_cancelled(),
        "monitor must be cancelled, not panic"
    );
}

#[tokio::test]
async fn monitor_journal_appends_survives_transient_metadata_error() {
    use std::time::Duration;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let path = dir.path().join("journal");
    let hidden = dir.path().join("journal.hidden");
    tokio::fs::write(&path, b"seed")
        .await
        .expect("write seed journal");

    let notify = Arc::new(Notify::new());
    let handle = tokio::spawn(monitor_journal_appends(path.clone(), Arc::clone(&notify)));

    tokio::time::sleep(Duration::from_millis(50)).await;
    tokio::fs::rename(&path, &hidden)
        .await
        .expect("hide journal to force metadata failure");
    tokio::time::sleep(Duration::from_millis(350)).await;
    tokio::fs::write(&path, b"seed-after-retry")
        .await
        .expect("recreate journal with appended content");

    tokio::time::timeout(Duration::from_secs(2), notify.notified())
        .await
        .expect("monitor should retry transient metadata failures and wake on later append");

    handle.abort();
    let _ = handle.await;
}

#[test]
fn auto_pr_params_include_plan_id_and_summary() {
    let params = super::terminal::build_auto_pr_params(
        "plan-123",
        "Epic title",
        "All approved",
        "spur/merge-1",
    );
    assert!(
        params.title.contains("plan-123"),
        "title missing plan_id: {}",
        params.title
    );
    assert!(
        params.body.contains("All approved"),
        "body missing outcome: {}",
        params.body
    );
    assert_eq!(params.head_branch, "spur/merge-1");
}

struct MockAutomation {
    actions: Arc<tokio::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl super::ReconcilerAutomation for MockAutomation {
    async fn merge_plan(&self, plan_id: &str) -> anyhow::Result<crate::plan::PlanMergeState> {
        self.actions.lock().await.push(format!("merge:{plan_id}"));
        Ok(crate::plan::PlanMergeState::Succeeded {
            merge_branch: "spur/merge-1".to_string(),
            merged_task_ids: vec![],
        })
    }

    async fn create_pr(&self, params: spur_pm::PrParams) -> anyhow::Result<String> {
        self.actions
            .lock()
            .await
            .push(format!("pr:{}", params.title));
        Ok("https://example.invalid/pr/1".to_string())
    }
}

fn attach_beads_workspace(repo: &std::path::Path, w: &spur_pm::test_workspace::TestBeadsWorkspace) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir(&beads_dir).expect("create test .beads directory");
    // Copy db + WAL + SHM (beads_rust uses WAL mode and skips checkpoint
    // on Drop; bare `fs::copy(beads.db)` loses every uncheckpointed write).
    w.copy_db_to(&beads_dir);
}

fn workspace_with_complete_epic(
    repo: &std::path::Path,
    plan_id: &str,
) -> spur_pm::test_workspace::TestBeadsWorkspace {
    let mut w = spur_pm::test_workspace::TestBeadsWorkspace::init();
    let plan_id_label = format!("spur:plan-id:{plan_id}");
    let epic_id = w.create_epic("Test Epic");

    for title in ["Task A", "Task B"] {
        let task_id = w.create_issue(title);
        w.add_label(&task_id, &plan_id_label);
        w.close_issue(&task_id);
    }

    w.add_label(&epic_id, &plan_id_label);
    w.add_label(&epic_id, "spur:plan-complete");
    w.close_issue(&epic_id);
    w.add_label(&epic_id, "spur:integration-pending");
    attach_beads_workspace(repo, &w);
    w
}

async fn pm_for_beads_repo(repo: &std::path::Path) -> Arc<spur_pm::PmService> {
    Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected beads pm"),
    )
}

fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(repo)
        .args(args)
        .output()
        .expect("git command failed to start");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git stdout should be utf-8")
        .trim()
        .to_string()
}

fn seed_git_repo(repo: &std::path::Path) -> String {
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(repo, &["config", "user.email", "test@spur"]);
    run_git(repo, &["config", "user.name", "spur-test"]);
    std::fs::write(repo.join("seed.txt"), "seed\n").expect("write seed file");
    run_git(repo, &["add", "seed.txt"]);
    run_git(repo, &["commit", "-q", "-m", "seed"]);
    run_git(repo, &["rev-parse", "HEAD"])
}

fn create_worker_branch(repo: &std::path::Path, branch: &str, file: &str) -> String {
    run_git(repo, &["checkout", "-q", "main"]);
    run_git(repo, &["checkout", "-q", "-b", branch]);
    std::fs::write(repo.join(file), format!("{branch}\n")).expect("write worker file");
    run_git(repo, &["add", file]);
    run_git(repo, &["commit", "-q", "-m", branch]);
    run_git(repo, &["checkout", "-q", "main"]);
    branch.to_string()
}

fn test_dispatch_ctx(
    delegation_tx: tokio::sync::mpsc::Sender<crate::tools::DelegationRequest>,
    brain_session_id: spur_acp::BrainSessionId,
) -> ReconcilerDispatchCtx {
    ReconcilerDispatchCtx {
        delegation_tx,
        task_tracker: tokio_util::task::TaskTracker::new(),
        brain_session_id,
        event_sink: None,
        materializer: Arc::new(crate::outcome_materializer::OutcomeMaterializer::new(
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        )),
        continuation_ctx: Arc::new(crate::server::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }),
    }
}

async fn seed_ready_overlay_plan(
    repo: &std::path::Path,
    plan_id: &str,
    brain_session_id: &spur_acp::BrainSessionId,
) -> (Arc<spur_pm::PmService>, String, String) {
    let base_oid = seed_git_repo(repo);
    let x_worker_branch = create_worker_branch(repo, "spur/worker-X", "x.rs");
    let z_worker_branch = create_worker_branch(repo, "spur/worker-Z", "z.rs");
    let empty = spur_pm::test_workspace::TestBeadsWorkspace::init();
    let beads_dir = repo.join(".beads");
    std::fs::create_dir(&beads_dir).expect("create test .beads directory");
    empty.copy_db_to(&beads_dir);
    let pm = pm_for_beads_repo(repo).await;
    let feature_gate = pro_feature_gate();
    crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    )
    .expect("test feature gate should allow beads advanced");
    let adv = pm.advanced().expect("beads advanced");

    let epic_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: "Predispatch Preview Plan".into(),
            description: Some("test plan".into()),
            issue_type: Some("epic".into()),
            labels: vec![
                crate::plan::labels::plan_id(plan_id),
                crate::plan::labels::PLAN_COMPLETE.to_string(),
                crate::plan::labels::plan_owner(&brain_session_id.as_session_id().0),
            ],
            ..Default::default()
        })
        .await
        .expect("create epic");
    let dep_issue_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: "X: approved dep".into(),
            description: Some("approved dependency".into()),
            issue_type: Some("task".into()),
            labels: vec![
                crate::plan::labels::plan_id(plan_id),
                crate::plan::labels::plan_task_id("X"),
                crate::plan::labels::agent("codex"),
            ],
            parent: Some(epic_id.clone()),
            ..Default::default()
        })
        .await
        .expect("create dep task");
    let second_dep_issue_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: "Z: approved dep".into(),
            description: Some("approved dependency".into()),
            issue_type: Some("task".into()),
            labels: vec![
                crate::plan::labels::plan_id(plan_id),
                crate::plan::labels::plan_task_id("Z"),
                crate::plan::labels::agent("codex"),
            ],
            parent: Some(epic_id.clone()),
            ..Default::default()
        })
        .await
        .expect("create second dep task");
    let ready_issue_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: "Y: ready task".into(),
            description: Some("ready task".into()),
            issue_type: Some("task".into()),
            labels: vec![
                crate::plan::labels::plan_id(plan_id),
                crate::plan::labels::plan_task_id("Y"),
                crate::plan::labels::agent("codex"),
            ],
            parent: Some(epic_id.clone()),
            depends_on: vec![dep_issue_id.clone(), second_dep_issue_id.clone()],
            ..Default::default()
        })
        .await
        .expect("create ready task");

    adv.add_comment(
        &epic_id,
        &crate::plan::audit_sentinel::encode_comment(
            &crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
                plan_id: plan_id.to_string(),
                epic_issue_id: epic_id.clone(),
                task_ids: vec![
                    dep_issue_id.clone(),
                    second_dep_issue_id.clone(),
                    ready_issue_id.clone(),
                ],
                base_snapshot_branch: Some("main".to_string()),
                base_snapshot_oid: None,
                execution_mode: None,
                brain_session_id: Some(brain_session_id.as_session_id().0.clone()),
                explicit_base: None,
            },
        ),
    )
    .await
    .expect("plan submit audit");
    crate::plan::emit_task_spec_audit(adv, &dep_issue_id, "X", "codex", &["x.rs".to_string()])
        .await
        .expect("dep task spec audit");
    crate::plan::emit_task_spec_audit(
        adv,
        &second_dep_issue_id,
        "Z",
        "codex",
        &["z.rs".to_string()],
    )
    .await
    .expect("second dep task spec audit");
    crate::plan::emit_task_spec_audit(adv, &ready_issue_id, "Y", "codex", &["y.rs".to_string()])
        .await
        .expect("ready task spec audit");

    for (issue_id, task_id, worker_branch) in [
        (&dep_issue_id, "X", &x_worker_branch),
        (&second_dep_issue_id, "Z", &z_worker_branch),
    ] {
        let delegation_id = format!("del-{task_id}");
        crate::plan::emit_completion_audit(
            Some(pm.as_ref()),
            &Some(issue_id.to_string()),
            feature_gate.as_ref(),
            plan_id,
            &delegation_id,
            crate::plan::audit_sentinel::CompletionState::AwaitingReview,
            false,
            crate::plan::audit_sentinel::CompletionAuditFields {
                worker_branch: Some(worker_branch.to_string()),
                result_summary: Some(format!("approved dep {task_id}")),
                dispatched_base_oid: Some(base_oid.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("completion audit");
        crate::plan::emit_approval_audit(
            Some(pm.as_ref()),
            &Some(issue_id.to_string()),
            feature_gate.as_ref(),
            plan_id,
            &delegation_id,
        )
        .await;
        pm.update_issue(
            issue_id,
            spur_pm::IssueUpdate {
                status: Some(pm.closed_status().to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("close dep task");
    }

    (pm, dep_issue_id, ready_issue_id)
}

#[tokio::test]
async fn tick_once_predicts_overlay_conflict_and_blocks_without_dispatch() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let plan_id = "PREDISPATCH-CONFLICT";
    let brain_session_id =
        spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-conflict".into()));
    let (pm, _dep_issue_id, ready_issue_id) =
        seed_ready_overlay_plan(dir.path(), plan_id, &brain_session_id).await;
    let (delegation_tx, mut delegation_rx) =
        tokio::sync::mpsc::channel::<crate::tools::DelegationRequest>(1);
    let config = ReconcilerConfig {
        repo_root: dir.path().to_path_buf(),
        predispatch_preview: super::PreviewStrategy::AlwaysConflict {
            dep_task_id: "X".into(),
            files: vec!["a.rs".into()],
        },
        ..Default::default()
    };
    let reconciler = Reconciler::new(
        config,
        pm,
        Arc::new(Notify::new()),
        Some(test_dispatch_ctx(delegation_tx, brain_session_id)),
        Some(plan_id.into()),
        pro_feature_gate(),
    );

    let did_work = reconciler.tick_once().await.expect("tick once");

    // `did_work` may be true because index-hygiene reconciliation can write.
    // The real no-dispatch invariant here is the channel check below.
    let _ = did_work;
    assert!(
        delegation_rx.try_recv().is_err(),
        "predicted conflict must not dispatch a worker"
    );
    let projected = reconciler
        .project_plan_from_beads(plan_id)
        .await
        .expect("project plan");
    let ready_task = projected
        .tasks
        .iter()
        .find(|entry| entry.spec.issue_id.as_deref() == Some(ready_issue_id.as_str()))
        .expect("ready task projection");
    assert!(
        matches!(
            &ready_task.status,
            crate::plan::PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files }
                if dep_task_id == "X" && files == &["a.rs".to_string()]
        ),
        "ready task should be blocked on predicted conflict, got {:?}",
        ready_task.status
    );
}

#[tokio::test]
async fn tick_once_with_clean_preview_dispatches_normally() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let plan_id = "PREDISPATCH-CLEAN";
    let brain_session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain-clean".into()));
    let (pm, _dep_issue_id, ready_issue_id) =
        seed_ready_overlay_plan(dir.path(), plan_id, &brain_session_id).await;
    let (delegation_tx, mut delegation_rx) =
        tokio::sync::mpsc::channel::<crate::tools::DelegationRequest>(1);
    let config = ReconcilerConfig {
        repo_root: dir.path().to_path_buf(),
        predispatch_preview: super::PreviewStrategy::AlwaysClean,
        ..Default::default()
    };
    let reconciler = Reconciler::new(
        config,
        pm,
        Arc::new(Notify::new()),
        Some(test_dispatch_ctx(delegation_tx, brain_session_id)),
        Some(plan_id.into()),
        pro_feature_gate(),
    );

    let did_work = reconciler.tick_once().await.expect("tick once");
    let request = delegation_rx.recv().await.expect("dispatch request");

    assert!(did_work, "clean preview should allow normal dispatch");
    assert_eq!(request.issue_id.as_deref(), Some(ready_issue_id.as_str()));
    assert!(matches!(
        request.base,
        Some(crate::tools::BaseSpec::WithOverlay { .. })
    ));
}

#[tokio::test]
async fn auto_merge_config_off_produces_zero_actions() {
    use tempfile::TempDir;

    let dir = TempDir::new().expect("tempdir");
    let repo = dir.path();
    let _beads = workspace_with_complete_epic(repo, "P1");
    let pm = pm_for_beads_repo(repo).await;

    let actions = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let automation = Arc::new(MockAutomation {
        actions: Arc::clone(&actions),
    });

    let mut reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        pm,
        Arc::new(Notify::new()),
        None,
        Some("P1".into()),
        pro_feature_gate(),
    );
    reconciler.set_auto_merge_approved_plans(false);
    reconciler.set_automation(automation);

    reconciler.tick_once().await.unwrap();

    let recorded = actions.lock().await;
    assert!(
        recorded.is_empty(),
        "config-off must produce zero automation actions, got: {:?}",
        *recorded
    );
}

/// Focused regression: when durable EpicCompletion audit emission fails
/// (e.g. disk-full / read-only database), the reconciler must suppress
/// merge_plan / create_pr even though the epic is closed and carries
/// integration-pending. Without this guard the old code would proceed
/// because it unconditionally appended a synthetic EpicCompletion to the
/// local audits vector.
#[tokio::test]
async fn failed_epic_completion_audit_suppresses_automation() {
    use tempfile::TempDir;

    let dir = TempDir::new().expect("tempdir");
    let repo = dir.path();
    let _beads = workspace_with_complete_epic(repo, "P1");
    let pm = pm_for_beads_repo(repo).await;

    // Make the beads database read-only so that add_comment (and therefore
    // emit_epic_completion_audit) fails. Some br versions refuse even read
    // commands against this fixture; that is still acceptable for this
    // regression because automation must not run when the durable audit
    // cannot be established.
    let db_path = repo.join(".beads").join("beads.db");
    let mut perms = std::fs::metadata(&db_path)
        .expect("db metadata")
        .permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(&db_path, perms).expect("set readonly");

    let actions = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let automation = Arc::new(MockAutomation {
        actions: Arc::clone(&actions),
    });

    let mut reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        pm,
        Arc::new(Notify::new()),
        None,
        Some("P1".into()),
        pro_feature_gate(),
    );
    reconciler.set_auto_merge_approved_plans(true);
    reconciler.set_automation(automation);

    match reconciler.tick_once().await {
        Ok(_) => {}
        Err(error)
            if error.to_string().contains("Permission denied")
                || error.to_string().contains("readonly")
                || error.to_string().contains("read-only") => {}
        Err(error) => panic!("unexpected tick_once error: {error:#}"),
    }

    let recorded = actions.lock().await;
    assert!(
        recorded.is_empty(),
        "failed epic-completion audit must suppress automation, got: {:?}",
        *recorded
    );
}

#[tokio::test]
async fn hybrid_journal_probe_disables_itself_when_missing() {
    let notify = Arc::new(Notify::new());
    let path = std::path::PathBuf::from("/nonexistent/path/.beads/journal");
    // The monitor must exit gracefully (not panic or hang) when the journal is absent.
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        super::monitor_journal_appends(path, notify),
    )
    .await;
    assert!(
        result.is_ok(),
        "journal monitor must exit when path is missing, not hang"
    );
}

fn apply_label_delta(
    existing: &[String],
    add_labels: &[String],
    remove_labels: &[String],
) -> Vec<String> {
    let mut next = existing.to_vec();
    next.retain(|label| !remove_labels.contains(label));
    for label in add_labels {
        if !next.contains(label) {
            next.push(label.clone());
        }
    }
    next.sort();
    next.dedup();
    next
}

proptest! {
    #[test]
    fn plan_id_reconcile_converges_idempotently(
        expected in prop_oneof![
            Just(None),
            Just(Some(crate::plan::labels::plan_id("P1"))),
        ],
        has_expected in any::<bool>(),
        has_stale in any::<bool>(),
        junk in prop::collection::vec("[a-z0-9_-]{1,12}", 0..4),
    ) {
        let canonical = crate::plan::labels::plan_id("P1");
        let stale = crate::plan::labels::plan_id("P2");
        let mut existing = junk;
        if has_expected && expected.as_deref() == Some(canonical.as_str()) {
            existing.push(canonical.clone());
        }
        if has_stale {
            existing.push(stale.clone());
        }

        let mut add = Vec::new();
        let mut remove = Vec::new();
        let mut drift = Vec::new();
        super::reconcile_singleton_label(
            "plan_id",
            expected.clone(),
            existing.clone(),
            &mut add,
            &mut remove,
            &mut drift,
        );
        if expected.is_none() && has_stale {
            prop_assert!(remove.contains(&stale));
            prop_assert!(drift.iter().any(|event| event.direction == "stale"));
        }

        let converged = apply_label_delta(&existing, &add, &remove);
        let mut add2 = Vec::new();
        let mut remove2 = Vec::new();
        let mut drift2 = Vec::new();
        super::reconcile_singleton_label(
            "plan_id",
            expected,
            converged,
            &mut add2,
            &mut remove2,
            &mut drift2,
        );
        prop_assert!(add2.is_empty() && remove2.is_empty() && drift2.is_empty());
    }

    #[test]
    fn plan_task_id_reconcile_converges_idempotently(
        expected in prop_oneof![
            Just(None),
            Just(Some(crate::plan::labels::plan_task_id("T1"))),
        ],
        has_expected in any::<bool>(),
        has_stale in any::<bool>(),
        junk in prop::collection::vec("[a-z0-9_-]{1,12}", 0..4),
    ) {
        let canonical = crate::plan::labels::plan_task_id("T1");
        let stale = crate::plan::labels::plan_task_id("T2");
        let mut existing = junk;
        if has_expected && expected.as_deref() == Some(canonical.as_str()) {
            existing.push(canonical.clone());
        }
        if has_stale {
            existing.push(stale.clone());
        }

        let mut add = Vec::new();
        let mut remove = Vec::new();
        let mut drift = Vec::new();
        super::reconcile_singleton_label(
            "plan_task_id",
            expected.clone(),
            existing.clone(),
            &mut add,
            &mut remove,
            &mut drift,
        );
        if expected.is_none() && has_stale {
            prop_assert!(remove.contains(&stale));
            prop_assert!(drift.iter().any(|event| event.direction == "stale"));
        }

        let converged = apply_label_delta(&existing, &add, &remove);
        let mut add2 = Vec::new();
        let mut remove2 = Vec::new();
        let mut drift2 = Vec::new();
        super::reconcile_singleton_label(
            "plan_task_id",
            expected,
            converged,
            &mut add2,
            &mut remove2,
            &mut drift2,
        );
        prop_assert!(add2.is_empty() && remove2.is_empty() && drift2.is_empty());
    }

    #[test]
    fn agent_reconcile_converges_idempotently(
        expected in prop_oneof![
            Just(None),
            Just(Some(crate::plan::labels::agent("codex"))),
        ],
        has_expected in any::<bool>(),
        has_stale in any::<bool>(),
        junk in prop::collection::vec("[a-z0-9_-]{1,12}", 0..4),
    ) {
        let canonical = crate::plan::labels::agent("codex");
        let stale = crate::plan::labels::agent("gemini");
        let mut existing = junk;
        if has_expected && expected.as_deref() == Some(canonical.as_str()) {
            existing.push(canonical.clone());
        }
        if has_stale {
            existing.push(stale.clone());
        }

        let mut add = Vec::new();
        let mut remove = Vec::new();
        let mut drift = Vec::new();
        super::reconcile_singleton_label(
            "agent",
            expected.clone(),
            existing.clone(),
            &mut add,
            &mut remove,
            &mut drift,
        );
        if expected.is_none() && has_stale {
            prop_assert!(remove.contains(&stale));
            prop_assert!(drift.iter().any(|event| event.direction == "stale"));
        }

        let converged = apply_label_delta(&existing, &add, &remove);
        let mut add2 = Vec::new();
        let mut remove2 = Vec::new();
        let mut drift2 = Vec::new();
        super::reconcile_singleton_label(
            "agent",
            expected,
            converged,
            &mut add2,
            &mut remove2,
            &mut drift2,
        );
        prop_assert!(add2.is_empty() && remove2.is_empty() && drift2.is_empty());
    }

    #[test]
    fn delegation_id_reconcile_converges_idempotently(
        expected in prop_oneof![
            Just(None),
            Just(Some(crate::plan::labels::delegation_id("d-current"))),
        ],
        has_expected in any::<bool>(),
        has_stale in any::<bool>(),
        has_legacy_stale in any::<bool>(),
        junk in prop::collection::vec("[a-z0-9_-]{1,12}", 0..4),
    ) {
        let canonical = crate::plan::labels::delegation_id("d-current");
        let stale = crate::plan::labels::delegation_id("d-prev");
        // Legacy non-prefixed form (no `spur:` prefix) — the form
        // `parse_plan_id` / `delegation_label_value` historically accepts.
        let legacy_stale = "delegation-id:d-legacy".to_string();
        let mut existing = junk;
        if has_expected && expected.as_deref() == Some(canonical.as_str()) {
            existing.push(canonical.clone());
        }
        if has_stale {
            existing.push(stale.clone());
        }
        if has_legacy_stale {
            existing.push(legacy_stale.clone());
        }

        let mut add = Vec::new();
        let mut remove = Vec::new();
        let mut drift = Vec::new();
        super::reconcile_singleton_label(
            "delegation_id",
            expected.clone(),
            existing.clone(),
            &mut add,
            &mut remove,
            &mut drift,
        );
        if expected.is_none() && has_stale {
            prop_assert!(remove.contains(&stale));
            prop_assert!(drift.iter().any(|event| event.direction == "stale"));
        }
        if expected.is_none() && has_legacy_stale {
            // The raw legacy label string must appear in remove_labels so the
            // backend can actually match and strip it (no silent canonicalization).
            prop_assert!(remove.contains(&legacy_stale));
        }

        let converged = apply_label_delta(&existing, &add, &remove);
        let mut add2 = Vec::new();
        let mut remove2 = Vec::new();
        let mut drift2 = Vec::new();
        super::reconcile_singleton_label(
            "delegation_id",
            expected,
            converged,
            &mut add2,
            &mut remove2,
            &mut drift2,
        );
        prop_assert!(add2.is_empty() && remove2.is_empty() && drift2.is_empty());
    }
}

#[test]
fn plan_complete_reconcile_missing_emits_drift_then_noops() {
    let existing = vec!["x".to_string()];
    let mut add = Vec::new();
    let mut remove = Vec::new();
    let mut drift = Vec::new();
    let mut buffers = super::LabelReconcileBuffers {
        add_labels: &mut add,
        remove_labels: &mut remove,
        drift_events: &mut drift,
    };
    super::reconcile_presence_label(
        "plan_complete",
        crate::plan::labels::PLAN_COMPLETE,
        |label| label == crate::plan::labels::PLAN_COMPLETE,
        true,
        &existing,
        &mut buffers,
    );
    assert!(add.contains(&crate::plan::labels::PLAN_COMPLETE.to_string()));
    assert!(drift.iter().any(|event| event.direction == "missing"));

    let converged = apply_label_delta(&existing, &add, &remove);
    let mut add2 = Vec::new();
    let mut remove2 = Vec::new();
    let mut drift2 = Vec::new();
    let mut buffers2 = super::LabelReconcileBuffers {
        add_labels: &mut add2,
        remove_labels: &mut remove2,
        drift_events: &mut drift2,
    };
    super::reconcile_presence_label(
        "plan_complete",
        crate::plan::labels::PLAN_COMPLETE,
        |label| label == crate::plan::labels::PLAN_COMPLETE,
        true,
        &converged,
        &mut buffers2,
    );
    assert!(add2.is_empty() && remove2.is_empty() && drift2.is_empty());
}

#[test]
fn plan_pending_reconcile_stale_emits_drift_then_noops() {
    let existing = vec![
        crate::plan::labels::PLAN_PENDING.to_string(),
        "x".to_string(),
    ];
    let mut add = Vec::new();
    let mut remove = Vec::new();
    let mut drift = Vec::new();
    let mut buffers = super::LabelReconcileBuffers {
        add_labels: &mut add,
        remove_labels: &mut remove,
        drift_events: &mut drift,
    };
    super::reconcile_presence_label(
        "plan_pending",
        crate::plan::labels::PLAN_PENDING,
        |label| label == crate::plan::labels::PLAN_PENDING,
        false,
        &existing,
        &mut buffers,
    );
    assert!(remove.contains(&crate::plan::labels::PLAN_PENDING.to_string()));
    assert!(drift.iter().any(|event| event.direction == "stale"));

    let converged = apply_label_delta(&existing, &add, &remove);
    let mut add2 = Vec::new();
    let mut remove2 = Vec::new();
    let mut drift2 = Vec::new();
    let mut buffers2 = super::LabelReconcileBuffers {
        add_labels: &mut add2,
        remove_labels: &mut remove2,
        drift_events: &mut drift2,
    };
    super::reconcile_presence_label(
        "plan_pending",
        crate::plan::labels::PLAN_PENDING,
        |label| label == crate::plan::labels::PLAN_PENDING,
        false,
        &converged,
        &mut buffers2,
    );
    assert!(add2.is_empty() && remove2.is_empty() && drift2.is_empty());
}

#[derive(Clone)]
struct ParentFallbackAdvanced {
    comments_by_issue: std::collections::HashMap<String, Vec<spur_pm::Comment>>,
}

#[async_trait::async_trait]
impl spur_pm::BeadsAdvanced for ParentFallbackAdvanced {
    async fn list_ready(
        &self,
        _filter: spur_pm::ReadyFilter,
    ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
        Ok(Vec::new())
    }

    async fn list_comments(&self, issue_id: &str) -> anyhow::Result<Vec<spur_pm::Comment>> {
        Ok(self
            .comments_by_issue
            .get(issue_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn add_comment(&self, _issue_id: &str, _body: &str) -> anyhow::Result<String> {
        Ok("c1".to_string())
    }

    async fn remove_dependency(&self, _issue_id: &str, _depends_on_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn dep_cycles(&self) -> anyhow::Result<Vec<spur_pm::DependencyCycle>> {
        Ok(Vec::new())
    }
}

struct ParentFallbackPm {
    issue: spur_pm::Issue,
    advanced: ParentFallbackAdvanced,
}

#[async_trait::async_trait]
impl crate::plan::PmLike for ParentFallbackPm {
    async fn get_issue(&self, id: &str) -> anyhow::Result<spur_pm::Issue> {
        if id == self.issue.id {
            return Ok(self.issue.clone());
        }
        anyhow::bail!("unknown issue id: {id}");
    }

    async fn update_issue(&self, _id: &str, _update: spur_pm::IssueUpdate) -> anyhow::Result<()> {
        Ok(())
    }

    fn closed_status(&self) -> &str {
        "closed"
    }

    fn advanced(&self) -> Option<&dyn spur_pm::BeadsAdvanced> {
        Some(&self.advanced)
    }
}

fn parent_fallback_issue(blocked_by: Vec<String>) -> spur_pm::Issue {
    spur_pm::Issue {
        id: "bd-child".to_string(),
        source: spur_pm::PmSource::Beads,
        title: "Child".to_string(),
        body: String::new(),
        status: "open".to_string(),
        labels: vec![],
        assignee: None,
        url: "https://example.invalid/bd-child".to_string(),
        priority: None,
        issue_type: Some("task".to_string()),
        blocked_by,
        due_at: None,
        source_system: None,
        source_repo: None,
        external_ref: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn plan_submit_comment(plan_id: &str, epic_id: &str) -> spur_pm::Comment {
    spur_pm::Comment {
        id: format!("c-{epic_id}"),
        body: crate::plan::audit_sentinel::encode_comment(
            &crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
                plan_id: plan_id.to_string(),
                epic_issue_id: epic_id.to_string(),
                task_ids: Vec::new(),
                base_snapshot_branch: None,
                base_snapshot_oid: None,
                execution_mode: None,
                brain_session_id: None,
                explicit_base: None,
            },
        ),
        actor: "tester".to_string(),
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn expected_plan_id_from_parent_epic_is_deterministic_when_blocked_by_reversed() {
    let issue_summary = spur_pm::IssueSummary {
        id: "bd-child".to_string(),
        source: spur_pm::PmSource::Beads,
        title: "Child".to_string(),
        status: "open".to_string(),
        labels: vec![],
        url: "https://example.invalid/bd-child".to_string(),
        priority: None,
        issue_type: Some("task".to_string()),
        assignee: None,
        description: None,
    };
    let mut comments_by_issue = std::collections::HashMap::new();
    comments_by_issue.insert(
        "bd-parent-a".to_string(),
        vec![plan_submit_comment("PLAN-A", "bd-parent-a")],
    );
    comments_by_issue.insert(
        "bd-parent-b".to_string(),
        vec![plan_submit_comment("PLAN-B", "bd-parent-b")],
    );
    let feature_gate = pro_feature_gate();

    let pm_forward = Arc::new(ParentFallbackPm {
        issue: parent_fallback_issue(vec!["bd-parent-b".to_string(), "bd-parent-a".to_string()]),
        advanced: ParentFallbackAdvanced {
            comments_by_issue: comments_by_issue.clone(),
        },
    });
    let reconciler_forward = Reconciler::new_with_pm_like(
        ReconcilerConfig::default(),
        pm_forward.clone() as Arc<dyn crate::plan::PmLike>,
        Arc::new(Notify::new()),
        None,
        None,
        Arc::clone(&feature_gate),
    );
    let forward = reconciler_forward
        .expected_plan_id_from_parent_epic(
            crate::plan::PmLike::advanced(pm_forward.as_ref()).expect("advanced"),
            &issue_summary,
        )
        .await
        .expect("forward parent fallback");

    let pm_reverse = Arc::new(ParentFallbackPm {
        issue: parent_fallback_issue(vec!["bd-parent-a".to_string(), "bd-parent-b".to_string()]),
        advanced: ParentFallbackAdvanced { comments_by_issue },
    });
    let reconciler_reverse = Reconciler::new_with_pm_like(
        ReconcilerConfig::default(),
        pm_reverse.clone() as Arc<dyn crate::plan::PmLike>,
        Arc::new(Notify::new()),
        None,
        None,
        feature_gate,
    );
    let reverse = reconciler_reverse
        .expected_plan_id_from_parent_epic(
            crate::plan::PmLike::advanced(pm_reverse.as_ref()).expect("advanced"),
            &issue_summary,
        )
        .await
        .expect("reversed parent fallback");

    assert_eq!(forward, reverse);
    assert_eq!(forward.as_deref(), Some("PLAN-A"));
}

#[tokio::test]
async fn tick_once_retains_agent_and_plan_task_id_for_empty_context_files_task() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path();
    let empty = spur_pm::test_workspace::TestBeadsWorkspace::init();
    let beads_dir = repo.join(".beads");
    std::fs::create_dir(&beads_dir).expect("create test .beads directory");
    empty.copy_db_to(&beads_dir);
    let pm = pm_for_beads_repo(repo).await;
    let feature_gate = pro_feature_gate();

    let plan_id = "P-EMPTY-CONTEXT";
    let sg = crate::build_epic_subgraph(
        pm.as_ref(),
        feature_gate.as_ref(),
        plan_id,
        "Empty Context Plan",
        Some("empty-context regression"),
        &[crate::plan::PlanTask {
            task_id: "T1".to_string(),
            agent: "codex".to_string(),
            task: "do work".to_string(),
            depends_on: vec![],
            issue_id: None,
            issue_title: None,
            context_files: Vec::new(),
        }],
    )
    .await
    .expect("persist plan");
    crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    )
    .expect("test feature gate should allow beads advanced");
    let adv = pm.advanced().expect("advanced");
    crate::emit_plan_submit_audit(adv, plan_id, &sg, crate::PlanSubmitAuditContext::default())
        .await;
    let child_id = sg.task_map.get("T1").cloned().expect("task map for T1");
    adv.add_comment(
        &child_id,
        &crate::plan::audit_sentinel::encode_comment(
            &crate::plan::audit_sentinel::AuditSentinelKind::Dispatch {
                delegation_id: "del-T1".to_string(),
                worker: "codex".to_string(),
                attempt: 1,
            },
        ),
    )
    .await
    .expect("dispatch audit");

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        None,
        Some(plan_id.to_string()),
        feature_gate,
    );
    let _ = reconciler.tick_once().await.expect("tick once");

    let child = pm.get_issue(&child_id).await.expect("child issue");
    assert!(
        child
            .labels
            .iter()
            .any(|label| label == &crate::plan::labels::agent("codex")),
        "child must retain spur:agent:* after tick"
    );
    assert!(
        child
            .labels
            .iter()
            .any(|label| label == &crate::plan::labels::plan_task_id("T1")),
        "child must retain spur:plan-task-id:* after tick"
    );
}

#[tokio::test]
async fn tick_once_strips_plan_complete_when_plan_submit_audit_is_absent() {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let repo = dir.path();
    let empty = spur_pm::test_workspace::TestBeadsWorkspace::init();
    let beads_dir = repo.join(".beads");
    std::fs::create_dir(&beads_dir).expect("create test .beads directory");
    empty.copy_db_to(&beads_dir);
    let pm = pm_for_beads_repo(repo).await;
    let feature_gate = pro_feature_gate();

    let issue_id = pm
        .create_issue(spur_pm::IssueCreate {
            title: "orphan plan-complete".to_string(),
            issue_type: Some("epic".to_string()),
            labels: vec![crate::plan::labels::PLAN_COMPLETE.to_string()],
            ..Default::default()
        })
        .await
        .expect("create issue");
    crate::server::require_feature(
        spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
        feature_gate.as_ref(),
    )
    .expect("test feature gate should allow beads advanced");
    let adv = pm.advanced().expect("advanced");
    adv.add_comment(
        &issue_id,
        &crate::plan::audit_sentinel::encode_comment(
            &crate::plan::audit_sentinel::AuditSentinelKind::Dispatch {
                delegation_id: "del-orphan".to_string(),
                worker: "codex".to_string(),
                attempt: 1,
            },
        ),
    )
    .await
    .expect("dispatch audit");

    let reconciler = Reconciler::new(
        ReconcilerConfig::default(),
        Arc::clone(&pm),
        Arc::new(Notify::new()),
        None,
        None,
        feature_gate,
    );
    let _ = reconciler.tick_once().await.expect("tick once");

    let issue = pm.get_issue(&issue_id).await.expect("issue");
    assert!(
        !issue
            .labels
            .iter()
            .any(|label| label == crate::plan::labels::PLAN_COMPLETE),
        "spur:plan-complete must be stripped without a PlanSubmit audit"
    );
}

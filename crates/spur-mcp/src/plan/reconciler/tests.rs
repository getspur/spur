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
    crate::plan::emit_task_spec_audit(adv, &dep_issue_id, "X", &["x.rs".to_string()])
        .await
        .expect("dep task spec audit");
    crate::plan::emit_task_spec_audit(adv, &second_dep_issue_id, "Z", &["z.rs".to_string()])
        .await
        .expect("second dep task spec audit");
    crate::plan::emit_task_spec_audit(adv, &ready_issue_id, "Y", &["y.rs".to_string()])
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

    assert!(
        !did_work,
        "blocked task should not count as dispatched work"
    );
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
                if dep_task_id == "X" && files == &vec!["a.rs".to_string()]
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

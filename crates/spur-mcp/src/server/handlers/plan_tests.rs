#[cfg(test)]
fn attach_beads_workspace(
    repo: &std::path::Path,
    w: &spur_pm::test_workspace::TestBeadsWorkspace,
) {
    let beads_dir = repo.join(".beads");
    std::fs::create_dir(&beads_dir).expect("create test .beads directory");
    w.copy_db_to(&beads_dir);
}

#[cfg(test)]
async fn init_beads_pm(
    repo: &std::path::Path,
) -> (
    spur_pm::test_workspace::TestBeadsWorkspace,
    std::sync::Arc<spur_pm::PmService>,
) {
    let w = spur_pm::test_workspace::TestBeadsWorkspace::init();
    attach_beads_workspace(repo, &w);
    let pm = std::sync::Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo, None)
            .await
            .expect("PmService::try_new failed")
            .expect("expected Some(PmService)"),
    );
    (w, pm)
}

#[cfg(test)]
mod plan_truncate_and_restart_tests {
    use super::*;
    use crate::plan::PmLike;
    use serde_json::json;
    use spur_acp::{BrainSessionId, SessionId};
    use std::sync::Arc;
    use tempfile::TempDir;

    fn no_op_ctx() -> DetachedContinuationCtx {
        DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    async fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
        super::run_git_capture(repo, None, args)
            .await
            .unwrap_or_else(|error| panic!("git {args:?} failed: {error}"))
    }

    async fn init_repo() -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        run_git(dir.path(), &["init", "-q", "-b", "main"]).await;
        run_git(dir.path(), &["config", "user.email", "test@spur"]).await;
        run_git(dir.path(), &["config", "user.name", "spur-test"]).await;
        std::fs::write(dir.path().join("README.md"), "seed\n").expect("write seed");
        run_git(dir.path(), &["add", "README.md"]).await;
        run_git(dir.path(), &["commit", "-q", "-m", "seed"]).await;
        dir
    }

    async fn commit_file_on_branch(
        repo: &std::path::Path,
        branch: &str,
        base: &str,
        path: &str,
        content: &str,
    ) -> String {
        run_git(repo, &["checkout", "-q", "-B", branch, base]).await;
        std::fs::write(repo.join(path), content).expect("write file");
        run_git(repo, &["add", path]).await;
        run_git(repo, &["commit", "-q", "-m", &format!("write {path}")]).await;
        let tip = run_git(repo, &["rev-parse", "--verify", "HEAD"]).await;
        run_git(repo, &["checkout", "-q", "main"]).await;
        tip
    }

    fn entry_for(
        task_id: &str,
        deps: &[&str],
        status: crate::plan::PlanTaskStatus,
    ) -> crate::plan::PlanTaskEntry {
        crate::plan::PlanTaskEntry {
            spec: crate::plan::PlanTask {
                task_id: task_id.into(),
                agent: "codex".into(),
                task: format!("task {task_id}"),
                depends_on: deps.iter().map(|dep| dep.to_string()).collect(),
                issue_id: Some(format!("bd-{task_id}")),
                context_files: Vec::new(),
            },
            status,
            result: None,
            worker_branch: None,
            attempt: 1,
            history: Vec::new(),
            last_delegation_id: None,
            dispatched_base_oid: None,
        }
    }

    fn approved_entry(
        task_id: &str,
        deps: &[&str],
        worker_branch: &str,
        dispatched_base_oid: &str,
    ) -> crate::plan::PlanTaskEntry {
        let mut entry = entry_for(
            task_id,
            deps,
            crate::plan::PlanTaskStatus::Approved { summary: None },
        );
        entry.worker_branch = Some(worker_branch.to_string());
        entry.dispatched_base_oid = Some(dispatched_base_oid.to_string());
        entry
    }

    fn plan_with(
        plan_id: &str,
        entries: Vec<crate::plan::PlanTaskEntry>,
    ) -> crate::plan::PlanState {
        crate::plan::PlanState {
            plan_id: plan_id.into(),
            tasks: entries,
            brain_session_id: BrainSessionId::new(SessionId("brain".into())),
            base_snapshot_branch: Some("main".into()),
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: None,
        }
    }

    async fn persist_plan_fixture_to_mock_pm(
        mock_pm: &crate::plan::test_util::MockPm,
        plan: &crate::plan::PlanState,
    ) {
        let epic_id = plan.epic_id.as_deref().expect("fixture epic id");
        crate::plan::PmLike::update_issue(
            mock_pm,
            epic_id,
            spur_pm::IssueUpdate {
                add_labels: vec![
                    crate::plan::labels::plan_owner("brain"),
                    crate::plan::labels::PLAN_COMPLETE.to_string(),
                ],
                ..Default::default()
            },
        )
        .await
        .expect("mark mock epic as complete");

        let mut issue_by_task = std::collections::HashMap::new();
        for entry in &plan.tasks {
            let depends_on = entry
                .spec
                .depends_on
                .iter()
                .map(|dep| {
                    issue_by_task
                        .get(dep)
                        .cloned()
                        .unwrap_or_else(|| panic!("dependency {dep} must be persisted first"))
                })
                .collect();
            let issue_id = crate::plan::PmLike::create_issue(
                mock_pm,
                spur_pm::IssueCreate {
                    title: format!("Task {}", entry.spec.task_id),
                    description: Some(entry.spec.task.clone()),
                    issue_type: Some("task".to_string()),
                    priority: Some(2),
                    labels: vec![
                        crate::plan::labels::plan_id(&plan.plan_id),
                        crate::plan::labels::plan_task_id(&entry.spec.task_id),
                        crate::plan::labels::agent(&entry.spec.agent),
                    ],
                    parent: Some(epic_id.to_string()),
                    assignee: None,
                    estimate_minutes: None,
                    depends_on,
                },
            )
            .await
            .expect("create mock task issue");
            issue_by_task.insert(entry.spec.task_id.clone(), issue_id.clone());

            let adv = crate::plan::PmLike::advanced(mock_pm).expect("mock advanced PM");
            match &entry.status {
                crate::plan::PlanTaskStatus::Approved { summary } => {
                    let delegation_id = format!("del-{}", entry.spec.task_id);
                    adv.add_comment(
                        &issue_id,
                        &crate::plan::audit_sentinel::encode_comment(
                            &crate::plan::audit_sentinel::AuditSentinelKind::Dispatch {
                                delegation_id: delegation_id.clone(),
                                worker: entry.spec.agent.clone(),
                                attempt: entry.attempt,
                            },
                        ),
                    )
                    .await
                    .expect("seed dispatch audit");
                    adv.add_comment(
                        &issue_id,
                        &crate::plan::audit_sentinel::encode_comment(
                            &crate::plan::audit_sentinel::AuditSentinelKind::Completion {
                                delegation_id: delegation_id.clone(),
                                completion_state:
                                    crate::plan::audit_sentinel::CompletionState::AwaitingReview,
                                superseded: false,
                                worker_branch: entry.worker_branch.clone(),
                                result_summary: summary.clone(),
                                artifact_uri: None,
                                dispatched_base_oid: entry.dispatched_base_oid.clone(),
                            },
                        ),
                    )
                    .await
                    .expect("seed completion audit");
                    adv.add_comment(
                        &issue_id,
                        &crate::plan::audit_sentinel::encode_comment(
                            &crate::plan::audit_sentinel::AuditSentinelKind::Approval {
                                delegation_id,
                            },
                        ),
                    )
                    .await
                    .expect("seed approval audit");
                    crate::plan::PmLike::update_issue(
                        mock_pm,
                        &issue_id,
                        spur_pm::IssueUpdate {
                            status: Some(crate::plan::PmLike::closed_status(mock_pm).to_string()),
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("close approved mock task");
                }
                crate::plan::PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files } => {
                    let reason = serde_json::to_string(&serde_json::json!({
                        "dep_task_id": dep_task_id,
                        "files": files,
                    }))
                    .expect("signal reason json");
                    adv.add_comment(
                        &issue_id,
                        &crate::plan::audit_sentinel::encode_comment(
                            &crate::plan::audit_sentinel::AuditSentinelKind::Signal {
                                signal_id: uuid::Uuid::new_v4().to_string(),
                                delegation_id: String::new(),
                                kind: "integration-conflict".to_string(),
                                severity: 1.0,
                                reason,
                            },
                        ),
                    )
                    .await
                    .expect("seed conflict signal audit");
                    crate::plan::PmLike::update_issue(
                        mock_pm,
                        &issue_id,
                        spur_pm::IssueUpdate {
                            add_labels: vec![
                                crate::plan::labels::SIGNAL_LABEL_INTEGRATION_CONFLICT.to_string(),
                            ],
                            ..Default::default()
                        },
                    )
                    .await
                    .expect("label setup conflict");
                }
                _ => {}
            }
        }
    }

    async fn new_server_with_mock_pm(
        repo: &std::path::Path,
    ) -> (
        Arc<McpCallbackServer>,
        DelegationChannel,
        Arc<crate::plan::test_util::MockPm>,
    ) {
        let session_id = BrainSessionId::new(SessionId("brain".into()));
        let mock_pm = crate::plan::test_util::MockPm::new().arc();
        let (mut server, channel) = McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            pro_feature_gate(),
        );
        server.__test_set_pm_like(mock_pm.clone() as Arc<dyn crate::plan::PmLike>);
        server.set_repo_root(repo.to_path_buf());
        server.set_reconciler_enabled(true, Some(Arc::new(tokio::sync::Notify::new())));
        let server = Arc::new(server);
        Arc::clone(&server)
            .enable_reconciler()
            .await
            .expect("enable mock reconciler");
        (server, channel, mock_pm)
    }

    fn output_json(response: serde_json::Value) -> serde_json::Value {
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("expected success response, got {response}"));
        serde_json::from_str(text).expect("response text is JSON")
    }

    #[tokio::test]
    pub(crate) async fn handle_plan_truncate_and_restart_happy_path() {
        let dir = init_repo().await;
        let base_oid = run_git(dir.path(), &["rev-parse", "--verify", "main"]).await;
        commit_file_on_branch(dir.path(), "spur/test-task-a", "main", "a.txt", "task A\n").await;

        let (server, mut _channel, mock_pm) = new_server_with_mock_pm(dir.path()).await;
        let parent_epic_id = mock_pm
            .create_issue(spur_pm::IssueCreate {
                title: "Parent Recovery Epic".to_string(),
                description: Some("parent body".to_string()),
                issue_type: Some("epic".to_string()),
                labels: vec![crate::plan::labels::plan_id("recover-plan")],
                ..Default::default()
            })
            .await
            .expect("create parent epic");
        let mut parent_plan = plan_with(
            "recover-plan",
            vec![
                approved_entry("A", &[], "spur/test-task-a", &base_oid),
                entry_for(
                    "B",
                    &["A"],
                    crate::plan::PlanTaskStatus::BlockedOnSetupConflict {
                        dep_task_id: "A".into(),
                        files: vec!["a.txt".into()],
                    },
                ),
                entry_for("C", &["B"], crate::plan::PlanTaskStatus::Pending),
            ],
        );
        parent_plan.epic_id = Some(parent_epic_id.clone());
        persist_plan_fixture_to_mock_pm(&mock_pm, &parent_plan).await;

        let response = server
            .__test_call_tool(
                "plan_truncate_and_restart",
                json!({
                    "plan_id": "recover-plan",
                    "blocked_task_id": "B",
                }),
            )
            .await;
        let output = output_json(response);
        assert_eq!(output["staging_branch"], "spur/plan-staging/recover-plan");
        assert_eq!(output["superseded_task_ids"], json!(["B", "C"]));
        assert_eq!(output["conflict"], serde_json::Value::Null);
        let new_plan_id = output["new_plan_id"].as_str().expect("new_plan_id");

        assert_eq!(
            run_git(
                dir.path(),
                &["show", "spur/plan-staging/recover-plan:a.txt"],
            )
            .await,
            "task A"
        );

        let original = server
            .active_plans
            .lock()
            .await
            .get("recover-plan")
            .cloned()
            .expect("original plan");
        let original = original.state.lock().await;
        assert!(matches!(
            original.tasks[1].status,
            crate::plan::PlanTaskStatus::Superseded { .. }
        ));
        assert!(matches!(
            original.tasks[2].status,
            crate::plan::PlanTaskStatus::Superseded { .. }
        ));
        drop(original);

        let restarted = server
            .active_plans
            .lock()
            .await
            .get(new_plan_id)
            .cloned()
            .expect("new plan");
        let restarted = restarted.state.lock().await;
        assert!(
            restarted
                .base_snapshot_branch
                .as_deref()
                .is_some_and(|branch| branch.starts_with("spur/brain-snapshot-")),
            "expected explicit branch base to be captured as snapshot branch, got {:?}",
            restarted.base_snapshot_branch
        );
        let staging_oid = run_git(
            dir.path(),
            &["rev-parse", "--verify", "spur/plan-staging/recover-plan"],
        )
        .await;
        assert_eq!(
            restarted.base_snapshot_oid.as_deref(),
            Some(staging_oid.as_str())
        );
        let restarted_ids: Vec<&str> = restarted
            .tasks
            .iter()
            .map(|entry| entry.spec.task_id.as_str())
            .collect();
        assert_eq!(restarted_ids, vec!["B", "C"]);
        assert_eq!(restarted.tasks[0].spec.depends_on, Vec::<String>::new());
        assert_eq!(restarted.tasks[1].spec.depends_on, vec!["B".to_string()]);
        let restarted_epic_id = restarted.epic_id.clone().expect("child epic id");
        drop(restarted);

        let child_epic = mock_pm.issue(&restarted_epic_id).await;
        assert_eq!(
            child_epic.title,
            "Parent Recovery Epic (spur/plan-staging/recover-plan)"
        );
        assert!(
            child_epic.blocked_by.contains(&parent_epic_id),
            "child epic should be linked to parent epic: {child_epic:?}"
        );
        assert!(
            child_epic
                .labels
                .contains(&crate::plan::labels::PLAN_COMPLETE.to_string()),
            "child epic labels: {:?}",
            child_epic.labels
        );
        let child_issues = mock_pm
            .issues()
            .await
            .into_iter()
            .filter(|issue| {
                issue.issue_type.as_deref() == Some("task")
                    && issue.labels.iter().any(|label| {
                        crate::plan::labels::parse_plan_id(label).as_deref() == Some(new_plan_id)
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(child_issues.len(), 2, "{child_issues:?}");
        let b_issue = child_issues
            .iter()
            .find(|issue| issue.title.contains("B"))
            .expect("B child");
        let c_issue = child_issues
            .iter()
            .find(|issue| issue.title.contains("C"))
            .expect("C child");
        assert!(b_issue.blocked_by.contains(&restarted_epic_id));
        assert!(c_issue.blocked_by.contains(&restarted_epic_id));
        assert!(
            c_issue.blocked_by.contains(&b_issue.id),
            "C should depend on B via beads edge: {c_issue:?}"
        );
        assert!(
            mock_pm.audit_seq().await >= 2,
            "ownership and submit audit comments should be persisted"
        );
    }

    #[tokio::test]
    pub(crate) async fn handle_plan_truncate_and_restart_returns_conflict_when_cherry_pick_fails() {
        let dir = init_repo().await;
        std::fs::write(dir.path().join("conflict.txt"), "base\n").expect("write base");
        run_git(dir.path(), &["add", "conflict.txt"]).await;
        run_git(dir.path(), &["commit", "-q", "-m", "conflict base"]).await;
        let base_oid = run_git(dir.path(), &["rev-parse", "--verify", "main"]).await;
        commit_file_on_branch(
            dir.path(),
            "spur/test-task-a",
            "main",
            "conflict.txt",
            "task A\n",
        )
        .await;
        commit_file_on_branch(
            dir.path(),
            "spur/test-task-b",
            "main",
            "conflict.txt",
            "task B\n",
        )
        .await;

        let (server, _channel, mock_pm) = new_server_with_mock_pm(dir.path()).await;
        let parent_epic_id = mock_pm
            .create_issue(spur_pm::IssueCreate {
                title: "Conflict Parent Epic".to_string(),
                issue_type: Some("epic".to_string()),
                labels: vec![crate::plan::labels::plan_id("conflict-plan")],
                ..Default::default()
            })
            .await
            .expect("create parent epic");
        let mut parent_plan = plan_with(
            "conflict-plan",
            vec![
                approved_entry("A", &[], "spur/test-task-a", &base_oid),
                approved_entry("B", &[], "spur/test-task-b", &base_oid),
                entry_for(
                    "C",
                    &["A", "B"],
                    crate::plan::PlanTaskStatus::BlockedOnSetupConflict {
                        dep_task_id: "B".into(),
                        files: vec!["conflict.txt".into()],
                    },
                ),
            ],
        );
        parent_plan.epic_id = Some(parent_epic_id);
        persist_plan_fixture_to_mock_pm(&mock_pm, &parent_plan).await;

        let response = server
            .__test_call_tool(
                "plan_truncate_and_restart",
                json!({
                    "plan_id": "conflict-plan",
                    "blocked_task_id": "C",
                }),
            )
            .await;
        let output = output_json(response);
        assert_eq!(output["conflict"]["dep_task_id"], "B");
        assert!(output["conflict"]["files"]
            .as_array()
            .expect("conflict files")
            .iter()
            .any(|file| file == "conflict.txt"));
        assert_eq!(output["superseded_task_ids"], json!(["C"]));
        assert!(output["new_plan_id"].as_str().is_some());
        assert_eq!(
            run_git(
                dir.path(),
                &["show", "spur/plan-staging/conflict-plan:conflict.txt"],
            )
            .await,
            "task A"
        );
    }
}

#[cfg(test)]
mod reconciler_fast_forward_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::Notify;

    #[tokio::test]
    async fn notify_fast_forward_wakes_waiter() {
        let notify = Arc::new(Notify::new());
        let waiter = tokio::spawn({
            let notify = Arc::clone(&notify);
            async move { notify.notified().await }
        });

        super::notify_fast_forward(&Some(Arc::clone(&notify)));

        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("waiter must wake")
            .expect("waiter task must not panic");
    }

    #[tokio::test]
    async fn fast_forward_reconciler_uses_configured_notify() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (mut server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let notify = Arc::new(Notify::new());
        server.set_reconciler_enabled(true, Some(Arc::clone(&notify)));

        let waiter = tokio::spawn({
            let notify = Arc::clone(&notify);
            async move { notify.notified().await }
        });

        server.fast_forward_reconciler();

        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("fast-forward must wake the configured reconciler channel")
            .expect("waiter task must not panic");
    }

    #[tokio::test]
    async fn fast_forward_reconciler_uses_default_notify_when_enabled_without_config() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (mut server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        server.set_reconciler_enabled(true, None);
        let notify = server
            .reconciler_fast_forward
            .as_ref()
            .cloned()
            .expect("default fast-forward notify should be allocated");

        let waiter = tokio::spawn({
            let notify = Arc::clone(&notify);
            async move { notify.notified().await }
        });

        server.fast_forward_reconciler();

        tokio::time::timeout(Duration::from_millis(50), waiter)
            .await
            .expect("fast-forward must wake the default reconciler channel")
            .expect("waiter task must not panic");
    }

    #[tokio::test]
    async fn load_or_project_plan_rejects_ephemeral_cache_without_epic() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        let plan = Arc::new(tokio::sync::Mutex::new(crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: Vec::new(),
            brain_session_id: session_id.clone(),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: None,
        }));
        server.active_plans.lock().await.insert(
            "plan-1".into(),
            super::CachedPlan::new(Arc::clone(&plan), super::unknown_beads_version()),
        );

        let error = server
            .load_or_project_plan("plan-1")
            .await
            .expect_err("ephemeral cache entry without durable epic must not load");
        assert_eq!(error, "unknown plan 'plan-1'");
    }

    #[test]
    fn discover_plan_ids_collects_unique_prefix_values() {
        let issues = vec![
            spur_pm::IssueSummary {
                id: "bd-1".into(),
                source: spur_pm::PmSource::Beads,
                title: "Epic A".into(),
                status: "open".into(),
                labels: vec![
                    crate::plan::labels::plan_id("plan-1"),
                    crate::plan::labels::PLAN_COMPLETE.to_string(),
                ],
                url: "beads://bd-1".into(),
                priority: Some(2),
                issue_type: Some("epic".into()),
                assignee: None,
                description: None,
            },
            spur_pm::IssueSummary {
                id: "bd-2".into(),
                source: spur_pm::PmSource::Beads,
                title: "Epic B".into(),
                status: "open".into(),
                labels: vec![
                    crate::plan::labels::plan_id("plan-2"),
                    crate::plan::labels::plan_id("plan-1"),
                ],
                url: "beads://bd-2".into(),
                priority: Some(2),
                issue_type: Some("epic".into()),
                assignee: None,
                description: None,
            },
        ];

        let plan_ids = super::discover_plan_ids(&issues);
        assert_eq!(plan_ids, vec!["plan-1".to_string()]);
    }

    #[test]
    fn mutation_orphan_ids_require_terminal_companion_breadcrumb() {
        use crate::plan::audit_sentinel::AuditSentinelKind;

        let audits = vec![
            AuditSentinelKind::MutationPlan {
                mutation_id: "mut-1".into(),
                op: "split".into(),
                trigger_signal_id: Some("sig-1".into()),
                trigger_task_id: "bd-1".into(),
            },
            AuditSentinelKind::MutationPlan {
                mutation_id: "mut-2".into(),
                op: "split".into(),
                trigger_signal_id: Some("sig-2".into()),
                trigger_task_id: "bd-1".into(),
            },
            AuditSentinelKind::MutationCommit {
                mutation_id: "mut-2".into(),
                children_created: vec!["bd-2".into()],
                op_tags: vec!["split_task".into()],
                affected_task_ids: vec!["bd-1".into(), "bd-2".into()],
            },
        ];

        assert_eq!(
            super::mutation_orphan_ids(&audits),
            vec!["mut-1".to_string()]
        );
    }

    #[test]
    fn execution_label_replacement_removes_old_plan_and_agent_labels() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Task".into(),
            body: "Body".into(),
            status: "open".into(),
            labels: vec![
                crate::plan::labels::plan_id("old-plan"),
                crate::plan::labels::agent("old-agent"),
            ],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("task".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let update = super::replace_execution_labels(&issue, "new-plan", "codex");
        assert!(update
            .add_labels
            .contains(&crate::plan::labels::plan_id("new-plan")));
        assert!(update
            .add_labels
            .contains(&crate::plan::labels::agent("codex")));
        assert!(update
            .remove_labels
            .contains(&crate::plan::labels::plan_id("old-plan")));
        assert!(update
            .remove_labels
            .contains(&crate::plan::labels::agent("old-agent")));
    }

    /// Regression for bd-19od: when an issue already carries the correct
    /// `spur:agent:<name>` and/or `spur:plan-id:<id>` label, the same string
    /// must NOT appear in both `add_labels` and `remove_labels`. The beads
    /// CLI processes adds before removes, so the duplicate would strip the
    /// label we just (idempotently) added.
    #[test]
    fn execution_label_replacement_does_not_strip_already_correct_agent_label() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Task".into(),
            body: "Body".into(),
            status: "open".into(),
            labels: vec![
                crate::plan::labels::plan_id("plan-1"),
                crate::plan::labels::agent("claude-code"),
            ],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("task".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        let update = super::replace_execution_labels(&issue, "plan-1", "claude-code");
        let agent_label = crate::plan::labels::agent("claude-code");
        let plan_label = crate::plan::labels::plan_id("plan-1");
        assert!(
            update.add_labels.contains(&agent_label),
            "add_labels must include the target agent label: {:?}",
            update.add_labels
        );
        assert!(
            !update.remove_labels.contains(&agent_label),
            "remove_labels must NOT contain the agent label that we are also adding: {:?}",
            update.remove_labels
        );
        assert!(
            !update.remove_labels.contains(&plan_label),
            "remove_labels must NOT contain the plan-id label that we are also adding: {:?}",
            update.remove_labels
        );

        let task_update =
            super::replace_task_execution_labels(&issue, "plan-1", "t1", "claude-code");
        assert!(
            !task_update.remove_labels.contains(&agent_label),
            "replace_task_execution_labels must also filter the agent label: {:?}",
            task_update.remove_labels
        );
        assert!(
            !task_update.remove_labels.contains(&plan_label),
            "replace_task_execution_labels must also filter the plan-id label: {:?}",
            task_update.remove_labels
        );
    }

    #[test]
    fn persisted_plan_epic_blocks_execute_epic_relabeling() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Persisted plan epic".into(),
            body: String::new(),
            status: "open".into(),
            labels: vec![
                crate::plan::labels::plan_id("plan-1"),
                crate::plan::labels::PLAN_COMPLETE.to_string(),
            ],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("epic".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(super::persisted_plan_epic_plan_id(&issue), Some("plan-1"));
    }

    #[test]
    fn ordinary_epic_can_still_be_executed() {
        let issue = spur_pm::Issue {
            id: "bd-1".into(),
            source: spur_pm::PmSource::Beads,
            title: "Product epic".into(),
            body: String::new(),
            status: "open".into(),
            labels: vec![],
            assignee: None,
            url: "beads://bd-1".into(),
            priority: Some(2),
            issue_type: Some("epic".into()),
            blocked_by: Vec::new(),
            due_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(super::persisted_plan_epic_plan_id(&issue), None);
    }

    #[tokio::test]
    async fn install_projected_plan_replaces_stale_cache_entry() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );

        let stale = Arc::new(tokio::sync::Mutex::new(crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: Vec::new(),
            brain_session_id: session_id.clone(),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: None,
        }));
        server.active_plans.lock().await.insert(
            "plan-1".into(),
            super::CachedPlan::new(Arc::clone(&stale), super::unknown_beads_version()),
        );

        let fresh = crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: vec![crate::plan::PlanTaskEntry {
                spec: crate::plan::PlanTask {
                    task_id: "t1".into(),
                    agent: "codex".into(),
                    task: "Task".into(),
                    depends_on: Vec::new(),
                    issue_id: Some("bd-1".into()),
                    context_files: Vec::new(),
                },
                status: crate::plan::PlanTaskStatus::Ready,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: None,
                dispatched_base_oid: None,
            }],
            brain_session_id: session_id.clone(),
            base_snapshot_branch: Some("refs/heads/main".into()),
            base_snapshot_oid: Some("0123456789abcdef0123456789abcdef01234567".into()),
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        };

        server.install_projected_plan(fresh, false).await;
        let loaded = server
            .active_plans
            .lock()
            .await
            .get("plan-1")
            .cloned()
            .expect("cached plan");
        assert_eq!(loaded.state.lock().await.tasks.len(), 1);
    }

    #[tokio::test]
    async fn reclaim_persisted_plans_hydrates_empty_cache() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (_beads, pm) = super::init_beads_pm(dir.path()).await;
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let tasks = vec![crate::plan::PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        }];
        let feature_gate = super::pro_feature_gate();
        let subgraph = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            "plan-1",
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");
        pm.update_issue(
            &subgraph.epic_id,
            spur_pm::IssueUpdate {
                add_labels: vec![crate::plan::labels::plan_owner(
                    &session_id.as_session_id().0,
                )],
                ..Default::default()
            },
        )
        .await
        .expect("stamp owner label");

        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            Some(Arc::clone(&pm)),
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            feature_gate,
        );
        assert!(server.active_plans.lock().await.is_empty());

        server
            .reclaim_persisted_plans_on_startup(pm)
            .await
            .expect("reclaim persisted plans");
        assert!(!server.active_plans.lock().await.is_empty());
    }

    #[tokio::test]
    async fn reclaim_replaces_existing_cache_entry_instead_of_merging() {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let continuation_ctx = super::DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        };
        let (server, _channel) = super::McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            continuation_ctx,
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            super::community_feature_gate(),
        );
        server.active_plans.lock().await.insert(
            "plan-1".into(),
            super::CachedPlan::new(
                Arc::new(tokio::sync::Mutex::new(crate::plan::PlanState {
                    plan_id: "plan-1".into(),
                    tasks: Vec::new(),
                    brain_session_id: session_id.clone(),
                    base_snapshot_branch: None,
                    base_snapshot_oid: None,
                    merge_state: crate::plan::PlanMergeState::NotStarted,
                    epic_id: None,
                })),
                super::unknown_beads_version(),
            ),
        );

        let fresh_plan = crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: vec![crate::plan::PlanTaskEntry {
                spec: crate::plan::PlanTask {
                    task_id: "t1".into(),
                    agent: "codex".into(),
                    task: "Task".into(),
                    depends_on: Vec::new(),
                    issue_id: Some("bd-1".into()),
                    context_files: Vec::new(),
                },
                status: crate::plan::PlanTaskStatus::Ready,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
                last_delegation_id: None,
                dispatched_base_oid: None,
            }],
            brain_session_id: session_id.clone(),
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        };
        let replacement_plan = crate::plan::PlanState {
            plan_id: "plan-1".into(),
            tasks: vec![
                crate::plan::PlanTaskEntry {
                    spec: crate::plan::PlanTask {
                        task_id: "t1".into(),
                        agent: "codex".into(),
                        task: "Task".into(),
                        depends_on: Vec::new(),
                        issue_id: Some("bd-1".into()),
                        context_files: Vec::new(),
                    },
                    status: crate::plan::PlanTaskStatus::Ready,
                    result: None,
                    worker_branch: None,
                    attempt: 1,
                    history: Vec::new(),
                    last_delegation_id: None,
                    dispatched_base_oid: None,
                },
                crate::plan::PlanTaskEntry {
                    spec: crate::plan::PlanTask {
                        task_id: "t2".into(),
                        agent: "codex".into(),
                        task: "Task 2".into(),
                        depends_on: Vec::new(),
                        issue_id: Some("bd-2".into()),
                        context_files: Vec::new(),
                    },
                    status: crate::plan::PlanTaskStatus::Pending,
                    result: None,
                    worker_branch: None,
                    attempt: 1,
                    history: Vec::new(),
                    last_delegation_id: None,
                    dispatched_base_oid: None,
                },
            ],
            brain_session_id: session_id,
            base_snapshot_branch: None,
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: Some("bd-epic".into()),
        };

        server.install_projected_plan(fresh_plan, false).await;
        server.install_projected_plan(replacement_plan, false).await;
        let cached = server
            .active_plans
            .lock()
            .await
            .get("plan-1")
            .cloned()
            .expect("cached");
        assert_eq!(cached.state.lock().await.tasks.len(), 2);
    }

    #[tokio::test]
    async fn detector_skips_reclaim_when_all_epics_have_rev1_metadata() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (_beads, pm) = super::init_beads_pm(dir.path()).await;
        let tasks = vec![crate::plan::PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        }];
        let feature_gate = super::pro_feature_gate();
        let sg = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            "plan-1",
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");

        // Emit PlanSubmit audit so the epic carries rev1 bootstrap metadata.
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .expect("pro gate");
        let adv = pm.advanced().expect("advanced");
        crate::emit_plan_submit_audit(
            adv,
            "plan-1",
            &sg,
            crate::PlanSubmitAuditContext {
                base_snapshot_branch: Some("main"),
                base_snapshot_oid: Some("abc123"),
                execution_mode: Some("test"),
                brain_session_id: None,
                explicit_base: None,
            },
        )
        .await;

        // The detector must report that no legacy reclaim is needed.
        let needs_reclaim =
            super::any_open_epic_lacks_rev1_metadata(pm.as_ref(), feature_gate.as_ref())
                .await
                .expect("detector query");
        assert!(
            !needs_reclaim,
            "detector must skip reclaim when all epics have rev1 metadata"
        );
    }

    #[tokio::test]
    async fn detector_reclaims_when_plan_submit_lacks_bootstrap_metadata() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let (_beads, pm) = super::init_beads_pm(dir.path()).await;
        let tasks = vec![crate::plan::PlanTask {
            task_id: "t1".into(),
            agent: "codex".into(),
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        }];
        let feature_gate = super::pro_feature_gate();
        let sg = crate::build_epic_subgraph(
            pm.as_ref(),
            feature_gate.as_ref(),
            "plan-1",
            "Epic",
            None,
            &tasks,
        )
        .await
        .expect("build epic subgraph");

        // Emit PlanSubmit audit WITHOUT base snapshot metadata.
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            feature_gate.as_ref(),
        )
        .expect("pro gate");
        let adv = pm.advanced().expect("advanced");
        crate::emit_plan_submit_audit(adv, "plan-1", &sg, crate::PlanSubmitAuditContext::default())
            .await;

        // The detector must report that legacy reclaim is still needed.
        let needs_reclaim =
            super::any_open_epic_lacks_rev1_metadata(pm.as_ref(), feature_gate.as_ref())
                .await
                .expect("detector query");
        assert!(
            needs_reclaim,
            "detector must reclaim when PlanSubmit lacks rev1 bootstrap metadata"
        );
    }

    #[test]
    fn legacy_reclaim_needed_when_rev1_bootstrap_metadata_is_missing() {
        assert!(super::legacy_reclaim_needed(false));
    }

    #[test]
    fn legacy_reclaim_skipped_when_rev1_bootstrap_metadata_exists() {
        assert!(!super::legacy_reclaim_needed(true));
    }
}

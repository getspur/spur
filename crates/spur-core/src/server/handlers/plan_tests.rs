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
                model: None,
                effort: None,
                config_overrides: None,
                task: format!("task {task_id}"),
                depends_on: deps.iter().map(|dep| dep.to_string()).collect(),
                issue_id: Some(format!("bd-{task_id}")),
                issue_title: None,
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
                    external_ref: None,
                    source_system: None,
                    source_repo: None,
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
                                estimated_cost_micros: None,
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

    fn parse_audit_comments(
        comments: Vec<spur_pm::Comment>,
    ) -> Vec<crate::plan::audit_sentinel::AuditSentinelKind> {
        comments
            .into_iter()
            .filter_map(|comment| {
                crate::plan::audit_sentinel::parse_comment(&comment.body)
                    .map(|parsed| parsed.expect("audit comment parses"))
            })
            .collect()
    }

    #[tokio::test]
    async fn submit_plan_persist_characterizes_epic_labels_audits_and_cache() {
        let dir = init_repo().await;
        let main_oid = run_git(dir.path(), &["rev-parse", "--verify", "main"]).await;
        let (server, _channel, mock_pm) = new_server_with_mock_pm(dir.path()).await;

        let response = server
            .__test_call_submit_plan(json!({
                "epic_title": "Persist Characterization",
                "epic_body": "persisted plan body",
                "base": { "kind": "branch", "name": "main" },
                "tasks": [
                    {
                        "task_id": "A",
                        "agent": "codex",
                        "task": "Implement A",
                        "context_files": ["a.rs"]
                    },
                    {
                        "task_id": "B",
                        "agent": "codex",
                        "task": "Implement B",
                        "depends_on": ["A"],
                        "issue_id": "bd-source",
                        "context_files": ["b.rs"]
                    }
                ]
            }))
            .await;
        assert!(
            response.get("error").is_none(),
            "submit_plan should succeed: {response}"
        );

        let cached_plan = {
            let active = server.active_plans.lock().await;
            assert_eq!(active.len(), 1, "submit_plan should cache one plan");
            active.values().next().cloned().expect("cached plan")
        };
        let cached = cached_plan.state.lock().await;
        let plan_id = cached.plan_id.clone();
        let epic_id = cached.epic_id.clone().expect("persisted epic id");
        assert_eq!(cached.brain_session_id.to_string(), "brain");
        assert_eq!(cached.base_snapshot_oid.as_deref(), Some(main_oid.as_str()));
        assert!(
            cached
                .base_snapshot_branch
                .as_deref()
                .is_some_and(|branch| branch.starts_with("spur/brain-snapshot-")),
            "submit_plan should cache a resolved snapshot branch: {:?}",
            cached.base_snapshot_branch
        );
        assert_eq!(cached.tasks.len(), 2);
        assert_eq!(cached.tasks[0].spec.issue_id.as_deref(), Some("bd-mock-2"));
        assert_eq!(cached.tasks[1].spec.issue_id.as_deref(), Some("bd-source"));
        drop(cached);

        let epic = mock_pm.issue(&epic_id).await;
        assert_eq!(epic.title, "Persist Characterization");
        assert_eq!(epic.body, "persisted plan body");
        assert_eq!(epic.issue_type.as_deref(), Some("epic"));
        assert!(epic.labels.contains(&crate::plan::labels::plan_id(&plan_id)));
        assert!(epic
            .labels
            .contains(&crate::plan::labels::plan_owner("brain")));
        assert!(epic
            .labels
            .contains(&crate::plan::labels::PLAN_COMPLETE.to_string()));
        assert!(!epic
            .labels
            .contains(&crate::plan::labels::PLAN_PENDING.to_string()));

        let issues = mock_pm.issues().await;
        let child_a = issues
            .iter()
            .find(|issue| {
                issue
                    .labels
                    .contains(&crate::plan::labels::plan_task_id("A"))
            })
            .expect("task A issue");
        let child_b = issues
            .iter()
            .find(|issue| {
                issue
                    .labels
                    .contains(&crate::plan::labels::plan_task_id("B"))
            })
            .expect("task B issue");
        assert!(child_a.blocked_by.contains(&epic_id));
        assert!(child_b.blocked_by.contains(&epic_id));
        assert!(child_b.blocked_by.contains(&child_a.id));
        assert!(child_b
            .labels
            .contains(&crate::plan::labels::source_issue("bd-source")));

        let child_a_audits = parse_audit_comments(mock_pm.comments(&child_a.id).await);
        assert!(child_a_audits.iter().any(|audit| matches!(
            audit,
            crate::plan::audit_sentinel::AuditSentinelKind::TaskSpec {
                task_id,
                context_files,
                agent: Some(agent),
                ..
            } if task_id == "A" && agent == "codex" && context_files == &vec!["a.rs".to_string()]
        )));
        let child_b_audits = parse_audit_comments(mock_pm.comments(&child_b.id).await);
        assert!(child_b_audits.iter().any(|audit| matches!(
            audit,
            crate::plan::audit_sentinel::AuditSentinelKind::TaskSpec {
                task_id,
                context_files,
                agent: Some(agent),
                ..
            } if task_id == "B" && agent == "codex" && context_files == &vec!["b.rs".to_string()]
        )));

        let epic_audits = parse_audit_comments(mock_pm.comments(&epic_id).await);
        assert!(epic_audits.iter().any(|audit| matches!(
            audit,
            crate::plan::audit_sentinel::AuditSentinelKind::PlanOwnershipAcquired {
                plan_id: audit_plan_id,
                owner,
                reason,
                ..
            } if audit_plan_id == &plan_id && owner == "brain" && reason == "submit_plan"
        )));
        assert!(epic_audits.iter().any(|audit| matches!(
            audit,
            crate::plan::audit_sentinel::AuditSentinelKind::PlanSubmit {
                plan_id: audit_plan_id,
                epic_issue_id,
                task_ids,
                base_snapshot_branch,
                base_snapshot_oid,
                execution_mode: Some(execution_mode),
                brain_session_id: Some(brain_session_id),
                explicit_base: Some(crate::BaseTarget::Branch { name }),
            } if audit_plan_id == &plan_id
                && epic_issue_id == &epic_id
                && task_ids.contains(&child_a.id)
                && task_ids.contains(&child_b.id)
                && base_snapshot_branch
                    .as_deref()
                    .is_some_and(|branch| branch.starts_with("spur/brain-snapshot-"))
                && base_snapshot_oid.as_deref() == Some(main_oid.as_str())
                && execution_mode == "submit_plan"
                && brain_session_id == "brain"
                && name == "main"
        )));
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
                        crate::plan::labels::parse_plan_id(label) == Some(new_plan_id)
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

    #[tokio::test]
    pub(crate) async fn handle_plan_truncate_and_restart_preserves_parent_state_when_submission_fails(
    ) {
        let dir = init_repo().await;
        let base_oid = run_git(dir.path(), &["rev-parse", "--verify", "main"]).await;
        commit_file_on_branch(dir.path(), "spur/test-task-a", "main", "a.txt", "task A\n").await;

        let (server, _channel, mock_pm) = new_server_with_mock_pm(dir.path()).await;
        let parent_epic_id = mock_pm
            .create_issue(spur_pm::IssueCreate {
                title: "Parent Recovery Epic".to_string(),
                description: Some("parent body".to_string()),
                issue_type: Some("epic".to_string()),
                labels: vec![crate::plan::labels::plan_id("recover-plan-fail")],
                ..Default::default()
            })
            .await
            .expect("create parent epic");
        let mut parent_plan = plan_with(
            "recover-plan-fail",
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
        parent_plan.epic_id = Some(parent_epic_id);
        persist_plan_fixture_to_mock_pm(&mock_pm, &parent_plan).await;
        mock_pm.fail_next_create_issues(1).await;

        let response = server
            .__test_call_tool(
                "plan_truncate_and_restart",
                json!({
                    "plan_id": "recover-plan-fail",
                    "blocked_task_id": "B",
                }),
            )
            .await;
        assert!(
            response.get("error").is_some(),
            "expected submission failure, got {response}"
        );

        let original = server
            .active_plans
            .lock()
            .await
            .get("recover-plan-fail")
            .cloned()
            .expect("original plan");
        let original = original.state.lock().await;
        assert!(matches!(
            original.tasks[1].status,
            crate::plan::PlanTaskStatus::BlockedOnSetupConflict { .. }
        ));
        assert!(matches!(
            original.tasks[2].status,
            crate::plan::PlanTaskStatus::Pending
        ));
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
    async fn load_or_project_plan_serves_ephemeral_cache_when_unversioned() {
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

        let resolved = server
            .load_or_project_plan_with_freshness("plan-1")
            .await
            .expect("unversioned cache hit should not block on durable projection");
        assert!(Arc::ptr_eq(&resolved.state, &plan));
        assert!(matches!(
            resolved.freshness,
            crate::handlers::PlanStateFreshness::Cache {
                beads_version_verified: false,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn plan_mcp_deps_resolves_ephemeral_cache_when_unversioned() {
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

        let deps = server.plan_mcp_deps();
        let resolver: &dyn crate::handlers::PlanResolver = &deps;
        let resolved = resolver
            .load_or_project_plan_with_freshness("plan-1")
            .await
            .expect("deps receiver should resolve through the shared cache");

        assert!(Arc::ptr_eq(&resolved.state, &plan));
        assert!(matches!(
            resolved.freshness,
            crate::handlers::PlanStateFreshness::Cache {
                beads_version_verified: false,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn versioned_cache_rejects_ephemeral_cache_without_epic() {
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
        server.set_versioned_cache_serve(true);
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
            .expect_err("versioned cache entry without durable epic must not load");
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
            external_ref: None,
            source_system: None,
            source_repo: None,
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
            external_ref: None,
            source_system: None,
            source_repo: None,
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
            external_ref: None,
            source_system: None,
            source_repo: None,
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
            external_ref: None,
            source_system: None,
            source_repo: None,
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
                    model: None,
                    effort: None,
                config_overrides: None,
                    task: "Task".into(),
                    depends_on: Vec::new(),
                    issue_id: Some("bd-1".into()),
                    issue_title: None,
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
            model: None,
            effort: None,
                config_overrides: None,
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            issue_title: None,
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
                    model: None,
                    effort: None,
                config_overrides: None,
                    task: "Task".into(),
                    depends_on: Vec::new(),
                    issue_id: Some("bd-1".into()),
                    issue_title: None,
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
                        model: None,
                        effort: None,
                config_overrides: None,
                        task: "Task".into(),
                        depends_on: Vec::new(),
                        issue_id: Some("bd-1".into()),
                        issue_title: None,
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
                        model: None,
                        effort: None,
                config_overrides: None,
                        task: "Task 2".into(),
                        depends_on: Vec::new(),
                        issue_id: Some("bd-2".into()),
                        issue_title: None,
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
            model: None,
            effort: None,
                config_overrides: None,
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            issue_title: None,
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
            model: None,
            effort: None,
                config_overrides: None,
            task: "Task".into(),
            depends_on: Vec::new(),
            issue_id: None,
            issue_title: None,
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

#[cfg(test)]
mod loop_lifecycle_mcp_tests {
    use super::*;
    use crate::plan::PmLike;
    use serde_json::json;
    use std::sync::Arc;

    fn no_op_ctx() -> DetachedContinuationCtx {
        DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    async fn new_server_with_mock_pm() -> (
        Arc<McpCallbackServer>,
        Arc<crate::plan::test_util::MockPm>,
    ) {
        new_server_with_mock_pm_and_gate(super::pro_feature_gate()).await
    }

    async fn new_server_with_mock_pm_and_gate(
        feature_gate: Arc<spur_license::FeatureGate>,
    ) -> (
        Arc<McpCallbackServer>,
        Arc<crate::plan::test_util::MockPm>,
    ) {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let mock_pm = crate::plan::test_util::MockPm::new().arc();
        let (mut server, _channel) = McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            feature_gate,
        );
        server.__test_set_pm_like(mock_pm.clone() as Arc<dyn crate::plan::PmLike>);
        (Arc::new(server), mock_pm)
    }

    struct ListIssuesFailPm;

    #[async_trait::async_trait]
    impl PmLike for ListIssuesFailPm {
        async fn list_issues(
            &self,
            _filter: spur_pm::IssueFilter,
        ) -> anyhow::Result<Vec<spur_pm::IssueSummary>> {
            anyhow::bail!("beads list exploded")
        }

        async fn update_issue(
            &self,
            _id: &str,
            _update: spur_pm::IssueUpdate,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        fn closed_status(&self) -> &str {
            "closed"
        }
    }

    async fn new_server_with_pm_like(
        pm: Arc<dyn crate::plan::PmLike>,
        feature_gate: Arc<spur_license::FeatureGate>,
    ) -> Arc<McpCallbackServer> {
        let session_id = spur_acp::BrainSessionId::new(spur_acp::SessionId("brain".into()));
        let (mut server, _channel) = McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            feature_gate,
        );
        server.__test_set_pm_like(pm);
        Arc::new(server)
    }

    fn valid_loop_spec() -> serde_json::Value {
        json!({
            "goal": "Keep CI green",
            "pattern": "ci-sweeper",
            "cadence_secs": 60,
            "template": {
                "tasks": [{
                    "task_id": "triage",
                    "agent": "codex",
                    "task": "Triage the CI state and report findings",
                    "labels": [crate::plan::labels::LOOP_TRIAGE_TASK]
                }]
            },
            "governors": {
                "max_cost_micros_per_generation": 2_000_000,
                "max_generations_per_day": 24,
                "max_tasks_per_generation": 5,
                "consecutive_failure_backoff": {
                    "k": 2,
                    "factor": 2,
                    "auto_pause_after": 4
                }
            }
        })
    }

    fn valid_loop_spec_with_autonomy(level: &str) -> serde_json::Value {
        let mut spec = valid_loop_spec();
        spec["autonomy"] = json!(level);
        spec
    }

    fn response_text_json(response: &serde_json::Value) -> serde_json::Value {
        let text = response["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("expected text JSON success response, got {response}"));
        serde_json::from_str(text).expect("response text must be JSON")
    }

    fn response_loop_id(response: &serde_json::Value) -> String {
        response["result"]["loop_id"]
            .as_str()
            .unwrap_or_else(|| panic!("expected loop_id in response, got {response}"))
            .to_string()
    }

    fn find_loop_issue<'a>(
        issues: &'a [spur_pm::Issue],
        loop_id: &str,
    ) -> &'a spur_pm::Issue {
        let label = crate::plan::labels::loop_id_label(loop_id);
        issues
            .iter()
            .find(|issue| issue.labels.contains(&label))
            .unwrap_or_else(|| panic!("missing loop issue for {loop_id}; issues={issues:?}"))
    }

    async fn loop_run_audits(
        mock_pm: &crate::plan::test_util::MockPm,
        issue_id: &str,
    ) -> Vec<crate::plan::audit_sentinel::AuditSentinelKind> {
        mock_pm
            .comments(issue_id)
            .await
            .into_iter()
            .filter_map(|comment| crate::plan::audit_sentinel::parse_comment(&comment.body))
            .filter_map(Result::ok)
            .filter(|audit| matches!(audit, crate::plan::audit_sentinel::AuditSentinelKind::LoopRun { .. }))
            .collect()
    }

    async fn seed_loop_run(
        mock_pm: &crate::plan::test_util::MockPm,
        issue_id: &str,
        loop_id: &str,
        generation: u32,
        outcome: &str,
        autonomy: Option<&str>,
    ) {
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            super::pro_feature_gate().as_ref(),
        )
        .expect("pro gate");
        let mut record = json!({
            "kind": "loop-run",
            "loop_id": loop_id,
            "generation": generation,
            "plan_id": format!("plan-{generation}"),
            "outcome": outcome,
            "tasks_discovered": 1,
            "approved": u32::from(outcome == "approved"),
            "rejected": 0,
            "failed": u32::from(outcome == "failed"),
            "cancelled": 0,
            "escalations": 0,
            "cost_micros": 100,
            "started_at": i64::from(generation),
            "ended_at": i64::from(generation),
        });
        if let Some(level) = autonomy {
            record["autonomy"] = json!(level);
        }
        let comment = format!(
            "{}\n{}",
            crate::plan::audit_sentinel::SENTINEL_PREFIX,
            serde_json::to_string(&record).expect("loop-run JSON serializes")
        );
        mock_pm
            .advanced()
            .expect("mock PM supports comments")
            .add_comment(issue_id, &comment)
            .await
            .expect("seed loop-run");
    }

    async fn submit_valid_loop(
        server: &McpCallbackServer,
    ) -> (String, serde_json::Value) {
        let response = server
            .__test_call_tool("submit_loop", json!({ "spec": valid_loop_spec() }))
            .await;
        assert!(
            response.get("error").is_none(),
            "submit_loop should succeed: {response}"
        );
        (response_loop_id(&response), response)
    }

    async fn submit_valid_loop_with_spec(
        server: &McpCallbackServer,
        spec: serde_json::Value,
    ) -> (String, serde_json::Value) {
        let response = server
            .__test_call_tool("submit_loop", json!({ "spec": spec }))
            .await;
        assert!(
            response.get("error").is_none(),
            "submit_loop should succeed: {response}"
        );
        (response_loop_id(&response), response)
    }

    async fn call_set_loop_autonomy(
        server: &McpCallbackServer,
        loop_id: &str,
        level: &str,
    ) -> serde_json::Value {
        server
            .__test_call_tool(
                "set_loop_autonomy",
                json!({ "loop_id": loop_id, "level": level }),
            )
            .await
    }

    #[tokio::test]
    async fn submit_loop_creates_loop_issue_with_sentinel_and_next_run() {
        let (server, mock_pm) = new_server_with_mock_pm().await;

        let (loop_id, response) = submit_valid_loop(&server).await;
        let issues = mock_pm.issues().await;
        let issue = find_loop_issue(&issues, &loop_id);

        assert_eq!(issue.issue_type.as_deref(), Some("task"));
        assert!(issue.body.contains("[[spur-loop v1]]"));
        assert!(issue.labels.contains(&crate::plan::labels::loop_id_label(&loop_id)));
        assert!(issue.labels.contains(&format!(
            "{}l1",
            crate::plan::labels::AUTONOMY_PREFIX
        )));
        assert!(
            issue.labels
                .iter()
                .any(|label| crate::plan::labels::parse_loop_next_run(label).is_some()),
            "loop issue must carry next-run label: {:?}",
            issue.labels
        );
        assert_eq!(response["result"]["issue_id"], issue.id);
        assert_eq!(
            crate::plan::loops::spec::LoopSpec::parse(&issue.body)
                .expect("loop sentinel parses")
                .loop_id,
            loop_id
        );
    }

    #[tokio::test]
    async fn kill_loop_closes_issue_and_writes_retired_record() {
        let (server, mock_pm) = new_server_with_mock_pm().await;
        let (loop_id, _) = submit_valid_loop(&server).await;
        let issue_id = find_loop_issue(&mock_pm.issues().await, &loop_id).id.clone();

        let response = server
            .__test_call_tool("kill_loop", json!({ "loop_id": loop_id }))
            .await;

        assert!(response.get("error").is_none(), "kill_loop failed: {response}");
        let output = response_text_json(&response);
        assert_eq!(output["loop_id"], loop_id);
        assert_eq!(output["issue_id"], issue_id);
        assert_eq!(output["retired"], true);
        let issue = mock_pm.issue(&issue_id).await;
        assert_eq!(issue.status, crate::plan::PmLike::closed_status(mock_pm.as_ref()));
        assert!(
            issue.labels
                .iter()
                .all(|label| crate::plan::labels::parse_loop_next_run(label).is_none()),
            "kill_loop must remove next-run labels: {:?}",
            issue.labels
        );
        let runs = loop_run_audits(mock_pm.as_ref(), &issue_id).await;
        assert_eq!(runs.len(), 1, "kill_loop must write exactly one run record");
        assert!(matches!(
            &runs[0],
            crate::plan::audit_sentinel::AuditSentinelKind::LoopRun {
                loop_id: record_loop_id,
                outcome,
                tasks_discovered: 0,
                approved: 0,
                rejected: 0,
                failed: 0,
                cancelled: 0,
                cost_micros: 0,
                ..
            } if record_loop_id == &loop_id && outcome == "retired"
        ));
    }

    #[tokio::test]
    async fn kill_loop_is_idempotent_on_closed_loop() {
        let (server, mock_pm) = new_server_with_mock_pm().await;
        let (loop_id, _) = submit_valid_loop(&server).await;
        let issue_id = find_loop_issue(&mock_pm.issues().await, &loop_id).id.clone();

        let first = server
            .__test_call_tool("kill_loop", json!({ "loop_id": loop_id }))
            .await;
        assert!(first.get("error").is_none(), "first kill_loop failed: {first}");
        let first_runs = loop_run_audits(mock_pm.as_ref(), &issue_id).await;
        assert_eq!(first_runs.len(), 1);

        let second = server
            .__test_call_tool("kill_loop", json!({ "loop_id": loop_id }))
            .await;

        assert!(
            second.get("error").is_none(),
            "second kill_loop should be idempotent: {second}"
        );
        let output = response_text_json(&second);
        assert_eq!(output["loop_id"], loop_id);
        assert_eq!(output["issue_id"], issue_id);
        assert_eq!(output["retired"], true);
        assert_eq!(
            mock_pm.issue(&issue_id).await.status,
            crate::plan::PmLike::closed_status(mock_pm.as_ref())
        );
        let second_runs = loop_run_audits(mock_pm.as_ref(), &issue_id).await;
        assert_eq!(
            second_runs.len(),
            1,
            "second kill_loop must not duplicate retired run records"
        );
    }

    #[tokio::test]
    async fn submit_loop_rejects_template_without_triage_task() {
        let (server, _mock_pm) = new_server_with_mock_pm().await;
        let mut spec = valid_loop_spec();
        spec["template"]["tasks"][0]["labels"] = json!([]);

        let response = server
            .__test_call_tool("submit_loop", json!({ "spec": spec }))
            .await;

        let message = response["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("expected error response, got {response}"));
        assert!(
            message.contains("triage"),
            "error should mention triage, got {message:?}"
        );
    }

    #[tokio::test]
    async fn pause_and_resume_toggle_label_and_reset_backoff() {
        let (server, mock_pm) = new_server_with_mock_pm().await;
        let (loop_id, _) = submit_valid_loop(&server).await;
        let issue_id = find_loop_issue(&mock_pm.issues().await, &loop_id).id.clone();
        mock_pm
            .update_issue(
                &issue_id,
                spur_pm::IssueUpdate {
                    add_labels: vec![crate::plan::labels::loop_next_run_label(4_000)],
                    remove_labels: vec![crate::plan::labels::loop_next_run_label(0)],
                    ..Default::default()
                },
            )
            .await
            .expect("seed later next-run");

        let paused = server
            .__test_call_tool("pause_loop", json!({ "loop_id": loop_id }))
            .await;
        assert!(paused.get("error").is_none(), "pause_loop failed: {paused}");
        let issue = mock_pm.issue(&issue_id).await;
        assert!(issue.labels.contains(&crate::plan::labels::LOOP_PAUSED.to_string()));

        let before_resume = chrono::Utc::now().timestamp();
        let resumed = server
            .__test_call_tool("resume_loop", json!({ "loop_id": loop_id }))
            .await;
        let after_resume = chrono::Utc::now().timestamp();
        assert!(resumed.get("error").is_none(), "resume_loop failed: {resumed}");
        let issue = mock_pm.issue(&issue_id).await;
        assert!(!issue.labels.contains(&crate::plan::labels::LOOP_PAUSED.to_string()));
        let next_runs: Vec<i64> = issue
            .labels
            .iter()
            .filter_map(|label| crate::plan::labels::parse_loop_next_run(label))
            .collect();
        assert_eq!(next_runs.len(), 1, "resume must leave one next-run label");
        assert!(
            (before_resume..=after_resume).contains(&next_runs[0]),
            "resume must replace old next-run labels with now; next_runs={next_runs:?}, labels={:?}",
            issue.labels
        );
    }

    #[tokio::test]
    async fn call_loop_wrappers_delegate_to_governed_handlers() {
        let (server, mock_pm) = new_server_with_mock_pm().await;
        let (loop_id, _) = submit_valid_loop(&server).await;
        let issue_id = find_loop_issue(&mock_pm.issues().await, &loop_id).id.clone();

        server
            .call_pause_loop(&loop_id)
            .await
            .expect("call_pause_loop succeeds");
        let issue = mock_pm.issue(&issue_id).await;
        assert!(issue
            .labels
            .contains(&crate::plan::labels::LOOP_PAUSED.to_string()));

        server
            .call_resume_loop(&loop_id)
            .await
            .expect("call_resume_loop succeeds");
        let issue = mock_pm.issue(&issue_id).await;
        assert!(!issue
            .labels
            .contains(&crate::plan::labels::LOOP_PAUSED.to_string()));

        server
            .call_kill_loop(&loop_id)
            .await
            .expect("call_kill_loop succeeds");
        let issue = mock_pm.issue(&issue_id).await;
        assert_eq!(issue.status, crate::plan::PmLike::closed_status(mock_pm.as_ref()));
        let runs = loop_run_audits(mock_pm.as_ref(), &issue_id).await;
        assert_eq!(
            runs.len(),
            1,
            "call_kill_loop must use the governed kill handler"
        );
    }

    #[tokio::test]
    async fn get_loop_status_returns_spec_and_recent_runs() {
        let (server, mock_pm) = new_server_with_mock_pm().await;
        let (loop_id, _) = submit_valid_loop(&server).await;
        let issue_id = find_loop_issue(&mock_pm.issues().await, &loop_id).id.clone();
        super::require_feature(
            spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED,
            super::pro_feature_gate().as_ref(),
        )
        .expect("pro gate");
        let adv = mock_pm.advanced().expect("mock PM supports comments");
        for (generation, outcome) in [(1, "approved"), (2, "failed"), (3, "failed")] {
            adv.add_comment(
                &issue_id,
                &crate::plan::audit_sentinel::encode_comment(
                    &crate::plan::audit_sentinel::AuditSentinelKind::LoopRun {
                        loop_id: loop_id.clone(),
                        generation,
                        plan_id: format!("plan-{generation}"),
                        autonomy: Some("l1".to_string()),
                        outcome: outcome.to_string(),
                        tasks_discovered: generation,
                        approved: u32::from(outcome == "approved"),
                        rejected: 0,
                        failed: u32::from(outcome == "failed"),
                        cancelled: 0,
                        escalations: 0,
                        cost_micros: 100 * u64::from(generation),
                        started_at: i64::from(generation),
                        ended_at: i64::from(generation),
                    },
                ),
            )
            .await
            .expect("seed loop-run");
        }

        let response = server
            .__test_call_tool(
                "get_loop_status",
                json!({ "loop_id": loop_id, "recent_runs": 2 }),
            )
            .await;
        assert!(
            response.get("error").is_none(),
            "get_loop_status failed: {response}"
        );
        let status = response_text_json(&response);

        assert_eq!(status["spec"]["goal"], "Keep CI green");
        assert_eq!(status["recent_runs"].as_array().expect("runs").len(), 2);
        assert_eq!(status["recent_runs"][0]["generation"], 2);
        assert_eq!(status["recent_runs"][1]["generation"], 3);
        assert_eq!(status["consecutive_failures"], 2);
        assert_eq!(status["effective_interval_secs"], 120);
        assert_eq!(status["paused"], false);
    }

    #[tokio::test]
    async fn get_loop_status_requires_advanced_feature_before_loading_status() {
        let (server, _mock_pm) =
            new_server_with_mock_pm_and_gate(super::unlicensed_feature_gate()).await;

        let response = server
            .__test_call_tool(
                "get_loop_status",
                json!({ "loop_id": "loop-without-license" }),
            )
            .await;

        let message = response["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("expected gated error response, got {response}"));
        assert!(
            message.contains(spur_license::FeatureKey::PM_PRO_BEADS_ADVANCED.as_str()),
            "expected missing advanced feature error before status lookup, got {response}"
        );
        assert!(
            !message.contains("unknown loop_id"),
            "license gate must run before loop lookup, got {response}"
        );
    }

    #[tokio::test]
    async fn get_loop_status_error_preserves_loop_issue_load_cause() {
        let server =
            new_server_with_pm_like(Arc::new(ListIssuesFailPm), super::pro_feature_gate()).await;

        let response = server
            .__test_call_tool("get_loop_status", json!({ "loop_id": "cause-loop" }))
            .await;

        let message = response["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("expected load error response, got {response}"));
        assert!(
            message.contains("failed to load loop issue"),
            "expected context in get_loop_status error, got {response}"
        );
        assert!(
            message.contains("beads list exploded"),
            "expected underlying cause in get_loop_status error, got {response}"
        );
    }

    #[tokio::test]
    async fn set_loop_autonomy_blocks_promotion_below_threshold_and_names_shortfall() {
        let (server, mock_pm) = new_server_with_mock_pm().await;
        let (loop_id, _) = submit_valid_loop(&server).await;
        let issue_id = find_loop_issue(&mock_pm.issues().await, &loop_id).id.clone();
        seed_loop_run(mock_pm.as_ref(), &issue_id, &loop_id, 1, "approved", Some("l1")).await;
        seed_loop_run(mock_pm.as_ref(), &issue_id, &loop_id, 2, "approved", Some("l1")).await;

        let response = call_set_loop_autonomy(&server, &loop_id, "l2").await;

        let message = response["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("expected ratchet error response, got {response}"));
        assert!(
            message.contains("requires 3") && message.contains("short by 1"),
            "promotion error must name threshold and shortfall, got {message:?}"
        );
    }

    #[tokio::test]
    async fn set_loop_autonomy_allows_promotion_at_exact_threshold() {
        let (server, mock_pm) = new_server_with_mock_pm().await;
        let (loop_id, _) = submit_valid_loop(&server).await;
        let issue_id = find_loop_issue(&mock_pm.issues().await, &loop_id).id.clone();
        for generation in 1..=3 {
            seed_loop_run(
                mock_pm.as_ref(),
                &issue_id,
                &loop_id,
                generation,
                "approved",
                Some("l1"),
            )
            .await;
        }

        let response = call_set_loop_autonomy(&server, &loop_id, "l2").await;

        assert!(
            response.get("error").is_none(),
            "promotion at threshold should succeed: {response}"
        );
        let output = response_text_json(&response);
        assert_eq!(output["loop_id"], loop_id);
        assert_eq!(output["previous_level"], "l1");
        assert_eq!(output["level"], "l2");
        assert_eq!(output["stable_generations"], 3);
    }

    #[tokio::test]
    async fn set_loop_autonomy_allows_demotion_without_ratchet() {
        let (server, mock_pm) = new_server_with_mock_pm().await;
        let (loop_id, _) =
            submit_valid_loop_with_spec(&server, valid_loop_spec_with_autonomy("l2")).await;
        let issue_id = find_loop_issue(&mock_pm.issues().await, &loop_id).id.clone();

        let response = call_set_loop_autonomy(&server, &loop_id, "l1").await;

        assert!(
            response.get("error").is_none(),
            "demotion should not require stable generations: {response}"
        );
        let issue = mock_pm.issue(&issue_id).await;
        assert_eq!(
            crate::plan::loops::spec::LoopSpec::parse(&issue.body)
                .expect("loop spec parses")
                .autonomy,
            crate::plan::labels::AutonomyLevel::L1
        );
    }

    #[tokio::test]
    async fn set_loop_autonomy_rejects_direct_l1_to_l3_promotion() {
        let (server, _mock_pm) = new_server_with_mock_pm().await;
        let (loop_id, _) = submit_valid_loop(&server).await;

        let response = call_set_loop_autonomy(&server, &loop_id, "l3").await;

        let message = response["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("expected direct-promotion error response, got {response}"));
        assert!(
            message.contains("one level"),
            "direct promotion error must explain one-level ratchet, got {message:?}"
        );
    }

    #[tokio::test]
    async fn set_loop_autonomy_updates_body_and_swaps_label_together() {
        let (server, mock_pm) = new_server_with_mock_pm().await;
        let (loop_id, _) = submit_valid_loop(&server).await;
        let issue_id = find_loop_issue(&mock_pm.issues().await, &loop_id).id.clone();
        for generation in 1..=3 {
            seed_loop_run(
                mock_pm.as_ref(),
                &issue_id,
                &loop_id,
                generation,
                "approved",
                Some("l1"),
            )
            .await;
        }

        let response = call_set_loop_autonomy(&server, &loop_id, "l2").await;

        assert!(
            response.get("error").is_none(),
            "set_loop_autonomy should succeed: {response}"
        );
        let issue = mock_pm.issue(&issue_id).await;
        assert_eq!(
            crate::plan::loops::spec::LoopSpec::parse(&issue.body)
                .expect("loop spec parses")
                .autonomy,
            crate::plan::labels::AutonomyLevel::L2
        );
        assert!(
            issue
                .labels
                .contains(&format!("{}l2", crate::plan::labels::AUTONOMY_PREFIX)),
            "updated issue must carry l2 label: {:?}",
            issue.labels
        );
        assert!(
            !issue
                .labels
                .contains(&format!("{}l1", crate::plan::labels::AUTONOMY_PREFIX)),
            "updated issue must remove old l1 label: {:?}",
            issue.labels
        );
    }

    #[tokio::test]
    async fn set_loop_autonomy_legacy_unstamped_records_reset_streak() {
        let (server, mock_pm) = new_server_with_mock_pm().await;
        let (loop_id, _) = submit_valid_loop(&server).await;
        let issue_id = find_loop_issue(&mock_pm.issues().await, &loop_id).id.clone();
        seed_loop_run(mock_pm.as_ref(), &issue_id, &loop_id, 1, "approved", Some("l1")).await;
        seed_loop_run(mock_pm.as_ref(), &issue_id, &loop_id, 2, "approved", Some("l1")).await;
        seed_loop_run(mock_pm.as_ref(), &issue_id, &loop_id, 3, "approved", None).await;
        seed_loop_run(mock_pm.as_ref(), &issue_id, &loop_id, 4, "approved", Some("l1")).await;
        seed_loop_run(mock_pm.as_ref(), &issue_id, &loop_id, 5, "approved", Some("l1")).await;

        let response = call_set_loop_autonomy(&server, &loop_id, "l2").await;

        let message = response["error"]["message"].as_str().unwrap_or_else(|| {
            panic!("legacy unstamped run should reset streak; got {response}")
        });
        assert!(
            message.contains("short by 1"),
            "legacy unstamped run must reset the promotion streak, got {message:?}"
        );
    }
}

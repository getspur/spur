use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use spur_acp::config::SpurConfig;
use spur_core::plan::loops::spec::{AutonomyLevel, LoopGovernors, LoopSpec};
use spur_core::{new_overflow_buf, BaseSpec, BaseTarget, DelegationRequest, Orchestrator};
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan};
use spur_pm::test_workspace::TestBeadsWorkspace;
use spur_pm::{IssueCreate, IssueFilter};
use tempfile::TempDir;

const SYSTEM_OWNER: &str = "spur-loop-runtime";

fn pro_feature_gate() -> Arc<FeatureGate> {
    let gate = Arc::new(FeatureGate::new(PolicyResolver::embedded()));
    let features = BTreeSet::from([FeatureKey::PM_PRO_BEADS_ADVANCED.as_str().to_owned()]);
    gate.update_state(&LicenseState::active_validated(Plan::Pro, features));
    gate
}

async fn temp_beads_pm() -> (TempDir, Arc<spur_pm::PmService>) {
    let repo = TempDir::new().expect("create temporary repository");
    init_git_repo(repo.path());
    let workspace = TestBeadsWorkspace::init();
    let beads_dir = repo.path().join(".beads");
    std::fs::create_dir_all(&beads_dir).expect("create test .beads directory");
    workspace.copy_db_to(&beads_dir);
    let pm = Arc::new(
        spur_pm::PmService::try_new(None, true, false, repo.path(), None)
            .await
            .expect("construct PM service")
            .expect("discover beads PM backend"),
    );
    (repo, pm)
}

fn init_git_repo(repo: &std::path::Path) {
    for args in [
        vec!["init"],
        vec!["config", "user.email", "spur-test@example.com"],
        vec!["config", "user.name", "SPUR Test"],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git setup command");
        assert!(
            output.status.success(),
            "git setup failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::write(repo.join("README.md"), "# L3 runtime test\n").expect("write fixture");
    for args in [vec!["add", "README.md"], vec!["commit", "-m", "seed"]] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn run_git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap_or_else(|error| panic!("run git {args:?}: {error}"));
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

async fn wait_for_issue(
    pm: &spur_pm::PmService,
    labels: Vec<String>,
    issue_type: &str,
) -> spur_pm::IssueSummary {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut issues = pm
                .list_issues(IssueFilter {
                    labels: labels.clone(),
                    issue_type: Some(issue_type.into()),
                    include_closed: true,
                    limit: Some(10),
                    ..Default::default()
                })
                .await
                .unwrap();
            if let Some(issue) = issues.pop() {
                break issue;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("timed out waiting for issue")
}

async fn next_delegation(
    requests: &mut [tokio::sync::mpsc::Receiver<DelegationRequest>; 2],
    context: &str,
) -> (usize, DelegationRequest) {
    let [first, second] = requests;
    let (runtime, request) = tokio::time::timeout(Duration::from_secs(20), async {
        tokio::select! {
            request = first.recv() => (0, request),
            request = second.recv() => (1, request),
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {context}"));
    (
        runtime,
        request
            .unwrap_or_else(|| panic!("runtime delegation channel closed waiting for {context}")),
    )
}

fn complete_maker_with_committed_branch(
    repo: &std::path::Path,
    request: DelegationRequest,
    task_id: &str,
) -> (String, String) {
    let delegation_id = request.id.as_str().to_owned();
    let branch = format!("spur/test-l3/{task_id}-{delegation_id}");
    let (base_ref, overlays) = match request.base.as_ref() {
        Some(BaseSpec::Branch { name }) => (name.clone(), Vec::new()),
        Some(BaseSpec::Commit { oid }) => (oid.clone(), Vec::new()),
        Some(BaseSpec::RepoMain) | None => ("HEAD".to_owned(), Vec::new()),
        Some(BaseSpec::WithOverlay { base, overlays }) => {
            let base_ref = match base {
                BaseTarget::RepoMain => "HEAD".to_owned(),
                BaseTarget::Branch { name } => name.clone(),
                BaseTarget::Commit { oid } => oid.clone(),
            };
            (base_ref, overlays.clone())
        }
    };
    let dispatched_base_oid = run_git(repo, &["rev-parse", &base_ref]);
    let worktree_parent = TempDir::new().expect("create worker worktree parent");
    let worktree = worktree_parent.path().join("checkout");
    run_git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &branch,
            worktree.to_str().expect("UTF-8 worktree path"),
            &base_ref,
        ],
    );
    for overlay in overlays {
        run_git(
            &worktree,
            &[
                "cherry-pick",
                &format!("{}..{}", overlay.base_oid, overlay.tip_oid),
            ],
        );
    }
    if let Some(tx) = request.dispatched_base_oid_tx.as_ref() {
        tx.send(Some(dispatched_base_oid.clone()))
            .expect("publish maker base OID");
    }
    std::fs::write(
        worktree.join(format!("{task_id}.txt")),
        format!("completed {task_id}\n"),
    )
    .expect("write maker output");
    run_git(&worktree, &["add", "."]);
    run_git(
        &worktree,
        &["commit", "-q", "-m", &format!("complete {task_id}")],
    );
    let diff = run_git(
        &worktree,
        &["diff", &format!("{dispatched_base_oid}..HEAD")],
    );
    run_git(
        repo,
        &[
            "worktree",
            "remove",
            "--force",
            worktree.to_str().expect("UTF-8 worktree path"),
        ],
    );
    request
        .respond_to
        .send(spur_acp::DelegationResult {
            resolved_config: None,
            status: spur_acp::DelegationStatus::Success,
            diff: Some(diff),
            diff_summary: None,
            summary: Some(format!("{task_id} completed successfully")),
            estimated_cost_usd: 0.0,
            worker_branch: Some(branch.clone()),
            artifact: None,
        })
        .expect("return maker result");
    (delegation_id, branch)
}

async fn approve_as_authenticated_reviewer(
    pm: &spur_pm::PmService,
    target_issue_id: &str,
    request: DelegationRequest,
) -> String {
    let reviewer_id = request.id.as_str().to_owned();
    assert_eq!(
        request.brain_session_id.as_session_id().0,
        SYSTEM_OWNER,
        "reviewer delegation must use the stable system owner without a BrainSession"
    );
    spur_core::mcp::review_verdict::submit_review_verdict(
        pm,
        &spur_core::handlers::WorkerCallContext {
            delegation_id: reviewer_id.clone(),
            brain_session_id: SYSTEM_OWNER.to_owned(),
        },
        serde_json::json!({
            "target_issue_id": target_issue_id,
            "decision": "approve",
            "feedback": "independent reviewer verified the committed maker diff",
            "evidence": ["get_task_diff matched the acceptance criteria"]
        }),
    )
    .await
    .expect("submit authenticated reviewer verdict");
    request
        .respond_to
        .send(spur_acp::DelegationResult {
            resolved_config: None,
            status: spur_acp::DelegationStatus::Success,
            diff: None,
            diff_summary: None,
            summary: Some("review verdict submitted".into()),
            estimated_cost_usd: 0.0,
            worker_branch: None,
            artifact: None,
        })
        .expect("return reviewer result");
    reviewer_id
}

async fn wait_for_system_approval(pm: &spur_pm::PmService, issue_id: &str) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let issue = pm.get_issue(issue_id).await.expect("hydrate task issue");
            let comments = pm
                .advanced()
                .expect("beads advanced")
                .list_comments(issue_id)
                .await
                .expect("list task comments");
            let audits =
                spur_core::plan::projector::collect_sorted_audits_for_issue(issue_id, comments)
                    .expect("parse task audits");
            let approved = audits.iter().any(|audit| {
                matches!(
                    audit,
                    spur_core::plan::audit_sentinel::AuditSentinelKind::Approval { .. }
                )
            });
            if issue.status == pm.closed_status() && approved {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("system L3 runtime did not approve successful worker completion");
}

#[tokio::test]
async fn due_l3_generation_runs_without_an_active_brain_session() {
    let (repo, pm) = temp_beads_pm().await;
    let gate = pro_feature_gate();
    let loop_id = "no-brain-project-runtime";
    let spec = LoopSpec {
        loop_id: loop_id.into(),
        goal: "Run L3 without a brain".into(),
        pattern: None,
        cadence_secs: 60,
        autonomy: AutonomyLevel::L3,
        template: serde_json::json!({
            "epic_title": "No-brain L3 generation",
            "tasks": [
                {
                    "task_id": "T1",
                    "agent": "codex",
                    "task": "Complete the first unattended task"
                },
                {
                    "task_id": "T2",
                    "agent": "codex",
                    "task": "Complete the dependent unattended task",
                    "depends_on": ["T1"]
                }
            ]
        }),
        governors: LoopGovernors::default(),
        escalation: None,
    };
    pm.create_issue(IssueCreate {
        title: "No-brain L3 loop".into(),
        description: Some(spec.to_sentinel_body()),
        issue_type: Some("loop".into()),
        labels: vec![
            spur_core::plan::labels::loop_id_label(loop_id),
            format!("{}l3", spur_core::plan::labels::AUTONOMY_PREFIX),
        ],
        ..Default::default()
    })
    .await
    .unwrap();

    let mut config = SpurConfig::default();
    config.spur.loops_enabled = true;
    config.spur.pause_all_loops = false;
    config
        .agents
        .entries
        .push(spur_acp::config::AgentConfig::with_defaults("codex"));
    let (delegation_tx_a, delegation_rx_a) = tokio::sync::mpsc::channel(8);
    let (delegation_tx_b, delegation_rx_b) = tokio::sync::mpsc::channel(8);
    let orchestrator_a = Orchestrator::new(
        repo.path().to_path_buf(),
        config.clone(),
        Some(Arc::clone(&gate)),
    )
    .unwrap()
    .with_pm_service(Arc::clone(&pm))
    .with_project_loop_runtime_delegation_capture(delegation_tx_a);
    let orchestrator_b =
        Orchestrator::new(repo.path().to_path_buf(), config, Some(Arc::clone(&gate)))
            .unwrap()
            .with_pm_service(Arc::clone(&pm))
            .with_project_loop_runtime_delegation_capture(delegation_tx_b);
    let (input_tx_a, input_rx_a) = tokio::sync::mpsc::channel(4);
    let (input_tx_b, input_rx_b) = tokio::sync::mpsc::channel(4);
    let interactive_a =
        tokio::spawn(orchestrator_a.run_interactive(input_rx_a, None, None, new_overflow_buf()));
    let interactive_b =
        tokio::spawn(orchestrator_b.run_interactive(input_rx_b, None, None, new_overflow_buf()));
    let mut delegation_rx = [delegation_rx_a, delegation_rx_b];

    let generations = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let generations = pm
                .list_issues(IssueFilter {
                    labels: vec![
                        spur_core::plan::labels::loop_id_label(loop_id),
                        spur_core::plan::labels::loop_generation_label(1),
                    ],
                    issue_type: Some("epic".into()),
                    include_closed: true,
                    limit: Some(10),
                    ..Default::default()
                })
                .await
                .unwrap();
            if !generations.is_empty() {
                break generations;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("project runtime did not persist an L3 generation");

    assert_eq!(generations.len(), 1, "expected exactly one L3 generation");
    assert!(
        generations[0]
            .labels
            .contains(&spur_core::plan::labels::plan_owner("spur-loop-runtime")),
        "generation must use the stable project runtime owner"
    );
    assert!(
        generations[0]
            .labels
            .contains(&format!("{}l3", spur_core::plan::labels::AUTONOMY_PREFIX)),
        "generation must retain L3 autonomy"
    );

    let plan_id = generations[0]
        .labels
        .iter()
        .find_map(|label| spur_core::plan::labels::parse_plan_id(label))
        .expect("generation plan id")
        .to_owned();
    let first_task = wait_for_issue(
        pm.as_ref(),
        vec![
            spur_core::plan::labels::plan_id(&plan_id),
            spur_core::plan::labels::plan_task_id("T1"),
        ],
        "task",
    )
    .await;
    let (leader_runtime, first_maker) = next_delegation(&mut delegation_rx, "T1 maker").await;
    let standby_runtime = 1 - leader_runtime;
    assert_eq!(
        first_maker.issue_id.as_deref(),
        Some(first_task.id.as_str())
    );
    assert_eq!(first_maker.brain_session_id.as_session_id().0, SYSTEM_OWNER);
    let (first_maker_id, first_branch) =
        complete_maker_with_committed_branch(repo.path(), first_maker, "T1");

    let (runtime, first_reviewer) = next_delegation(&mut delegation_rx, "T1 reviewer").await;
    assert_eq!(
        runtime, leader_runtime,
        "standby must not dispatch T1 review"
    );
    let first_review_issue_id = first_reviewer
        .issue_id
        .clone()
        .expect("T1 reviewer companion issue");
    assert_ne!(first_review_issue_id, first_task.id);
    assert_ne!(first_reviewer.id.as_str(), first_maker_id);
    assert_eq!(
        first_reviewer.base,
        Some(BaseSpec::Branch {
            name: first_branch.clone()
        })
    );
    assert_eq!(first_reviewer.enable_worker_mcp, Some(true));
    let first_review_issue = pm
        .get_issue(&first_review_issue_id)
        .await
        .expect("hydrate T1 review companion");
    assert!(first_review_issue
        .labels
        .contains(&spur_core::plan::labels::SYSTEM_REVIEW.to_owned()));
    assert!(
        tokio::time::timeout(
            Duration::from_millis(200),
            delegation_rx[standby_runtime].recv()
        )
        .await
        .is_err(),
        "T2 maker must remain blocked until T1 receives an independent verdict"
    );
    let first_reviewer_id =
        approve_as_authenticated_reviewer(pm.as_ref(), &first_task.id, first_reviewer).await;
    wait_for_system_approval(pm.as_ref(), &first_task.id).await;

    let second_task = wait_for_issue(
        pm.as_ref(),
        vec![
            spur_core::plan::labels::plan_id(&plan_id),
            spur_core::plan::labels::plan_task_id("T2"),
        ],
        "task",
    )
    .await;
    let (runtime, second_maker) = next_delegation(&mut delegation_rx, "T2 maker").await;
    assert_eq!(
        runtime, leader_runtime,
        "standby must not dispatch T2 maker"
    );
    assert_eq!(
        second_maker.issue_id.as_deref(),
        Some(second_task.id.as_str())
    );
    let (second_maker_id, second_branch) =
        complete_maker_with_committed_branch(repo.path(), second_maker, "T2");
    let (runtime, second_reviewer) = next_delegation(&mut delegation_rx, "T2 reviewer").await;
    assert_eq!(
        runtime, leader_runtime,
        "standby must not dispatch T2 review"
    );
    let second_review_issue_id = second_reviewer
        .issue_id
        .clone()
        .expect("T2 reviewer companion issue");
    assert_ne!(second_review_issue_id, second_task.id);
    assert_ne!(second_reviewer.id.as_str(), second_maker_id);
    assert_eq!(
        second_reviewer.base,
        Some(BaseSpec::Branch {
            name: second_branch
        })
    );
    let second_reviewer_id =
        approve_as_authenticated_reviewer(pm.as_ref(), &second_task.id, second_reviewer).await;
    wait_for_system_approval(pm.as_ref(), &second_task.id).await;
    let identities = [
        first_maker_id,
        first_reviewer_id,
        second_maker_id,
        second_reviewer_id,
    ];
    assert_eq!(
        identities.iter().collect::<BTreeSet<_>>().len(),
        identities.len(),
        "each maker and reviewer must have a distinct delegation identity"
    );

    let durable_loop_id =
        spur_core::plan::labels::parse_loop_id(&spur_core::plan::labels::loop_id_label(loop_id))
            .expect("durable loop id")
            .to_owned();
    let terminal_result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let epic = pm
                .get_issue(&generations[0].id)
                .await
                .expect("hydrate generation epic");
            let loop_issue = pm
                .list_issues(IssueFilter {
                    labels: vec![spur_core::plan::labels::loop_id_label(loop_id)],
                    issue_type: Some("loop".into()),
                    include_closed: true,
                    limit: Some(1),
                    ..Default::default()
                })
                .await
                .expect("list loop issues")
                .pop()
                .expect("loop issue");
            let comments = pm
                .advanced()
                .expect("beads advanced")
                .list_comments(&loop_issue.id)
                .await
                .expect("list loop comments");
            let audits = spur_core::plan::projector::collect_sorted_audits_for_issue(
                &loop_issue.id,
                comments,
            )
            .expect("parse loop audits");
            let matching_runs = audits
                .iter()
                .filter(|audit| {
                    matches!(
                        audit,
                        spur_core::plan::audit_sentinel::AuditSentinelKind::LoopRun {
                            loop_id: recorded_loop_id,
                            generation: 1,
                            plan_id: recorded_plan_id,
                            outcome,
                            approved: 2,
                            ..
                        } if recorded_loop_id == &durable_loop_id
                            && recorded_plan_id == &plan_id
                            && outcome == "approved"
                    )
                })
                .count();
            if epic.status == pm.closed_status() && matching_runs == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    if terminal_result.is_err() {
        let epic = pm
            .get_issue(&generations[0].id)
            .await
            .expect("hydrate timed-out epic");
        let first = pm
            .get_issue(&first_task.id)
            .await
            .expect("hydrate timed-out T1");
        let second = pm
            .get_issue(&second_task.id)
            .await
            .expect("hydrate timed-out T2");
        let loop_issue = pm
            .list_issues(IssueFilter {
                labels: vec![spur_core::plan::labels::loop_id_label(loop_id)],
                issue_type: Some("loop".into()),
                include_closed: true,
                limit: Some(1),
                ..Default::default()
            })
            .await
            .expect("list timed-out loop issues")
            .pop()
            .expect("timed-out loop issue");
        let loop_audits = spur_core::plan::projector::collect_sorted_audits_for_issue(
            &loop_issue.id,
            pm.advanced()
                .expect("beads advanced")
                .list_comments(&loop_issue.id)
                .await
                .expect("list timed-out loop comments"),
        )
        .expect("parse timed-out loop audits");
        panic!(
            "system L3 runtime did not project terminal epic and LoopRun: epic={:?}, T1={:?}, T2={:?}, loop_audits={loop_audits:?}",
            epic.status, first.status, second.status,
        );
    }

    assert!(
        {
            let [first, second] = &mut delegation_rx;
            tokio::time::timeout(Duration::from_millis(200), async {
                tokio::select! {
                    request = first.recv() => request,
                    request = second.recv() => request,
                }
            })
            .await
            .is_err()
        },
        "terminal two-task generation must produce exactly two makers and two reviewers"
    );

    let mut input_txs = [Some(input_tx_a), Some(input_tx_b)];
    let mut interactives = [Some(interactive_a), Some(interactive_b)];
    drop(input_txs[leader_runtime].take());
    tokio::time::timeout(
        Duration::from_secs(5),
        interactives[leader_runtime].take().expect("leader task"),
    )
    .await
    .expect("leader interactive shutdown timed out")
    .expect("leader interactive task panicked")
    .expect("leader interactive task failed");

    let loop_issue = pm
        .list_issues(IssueFilter {
            labels: vec![spur_core::plan::labels::loop_id_label(loop_id)],
            issue_type: Some("loop".into()),
            include_closed: true,
            limit: Some(1),
            ..Default::default()
        })
        .await
        .expect("list loop for failover")
        .pop()
        .expect("loop issue for failover");
    let loop_detail = pm.get_issue(&loop_issue.id).await.expect("hydrate loop");
    let old_next_run = loop_detail
        .labels
        .iter()
        .filter(|label| spur_core::plan::labels::parse_loop_next_run(label).is_some())
        .cloned()
        .collect::<Vec<_>>();
    pm.update_issue(
        &loop_issue.id,
        spur_pm::IssueUpdate {
            add_labels: vec![spur_core::plan::labels::loop_next_run_label(0)],
            remove_labels: old_next_run,
            ..Default::default()
        },
    )
    .await
    .expect("re-arm loop for standby promotion proof");

    tokio::time::timeout(
        Duration::from_secs(20),
        delegation_rx[standby_runtime].recv(),
    )
    .await
    .expect("standby did not promote after the former leader released its fencing guard")
    .expect("standby delegation channel closed before promotion");

    drop(input_txs[standby_runtime].take());
    tokio::time::timeout(
        Duration::from_secs(5),
        interactives[standby_runtime].take().expect("standby task"),
    )
    .await
    .expect("standby interactive shutdown timed out")
    .expect("standby interactive task panicked")
    .expect("standby interactive task failed");
}

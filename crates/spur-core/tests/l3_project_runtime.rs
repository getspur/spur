use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use spur_acp::config::SpurConfig;
use spur_core::plan::loops::spec::{AutonomyLevel, LoopGovernors, LoopSpec};
use spur_core::{new_overflow_buf, Orchestrator};
use spur_license::policy::PolicyResolver;
use spur_license::{FeatureGate, FeatureKey, LicenseState, Plan};
use spur_pm::test_workspace::TestBeadsWorkspace;
use spur_pm::{IssueCreate, IssueFilter};
use tempfile::TempDir;

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

async fn wait_for_dispatch(pm: &spur_pm::PmService, issue_id: &str) -> String {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let comments = pm
                .advanced()
                .expect("beads advanced")
                .list_comments(issue_id)
                .await
                .expect("list task comments");
            let audits =
                spur_core::plan::projector::collect_sorted_audits_for_issue(issue_id, comments)
                    .expect("parse task audits");
            if let Some(delegation_id) = audits.iter().rev().find_map(|audit| match audit {
                spur_core::plan::audit_sentinel::AuditSentinelKind::Dispatch {
                    delegation_id,
                    ..
                } => Some(delegation_id.clone()),
                _ => None,
            }) {
                break delegation_id;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("timed out waiting for task dispatch")
}

async fn persist_successful_worker_completion(
    pm: &spur_pm::PmService,
    gate: &FeatureGate,
    plan_id: &str,
    task_id: &str,
    issue_id: &str,
    delegation_id: &str,
) {
    spur_core::plan::emit_completion_audit(
        Some(pm),
        &Some(issue_id.to_owned()),
        gate,
        plan_id,
        delegation_id,
        spur_core::plan::audit_sentinel::CompletionState::AwaitingReview,
        false,
        spur_core::plan::audit_sentinel::CompletionAuditFields {
            worker_branch: Some(format!("spur/worker/{task_id}")),
            result_summary: Some(format!("{task_id} completed successfully")),
            ..Default::default()
        },
    )
    .await
    .expect("persist worker completion audit");
    pm.update_issue(
        issue_id,
        spur_pm::IssueUpdate {
            add_labels: vec![spur_core::plan::labels::READY_FOR_REVIEW.to_owned()],
            ..Default::default()
        },
    )
    .await
    .expect("mark task ready for review");
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
    // Keep real worker execution parked in ACP initialization while the test
    // deterministically writes the same durable completion facts produced by
    // the delegation collector. The process is cancelled during runtime drain.
    let mut parked_agent = spur_acp::config::AgentConfig::with_defaults("codex");
    parked_agent.command = "sleep".to_owned();
    parked_agent.args = vec!["300".to_owned()];
    config.agents.entries.push(parked_agent);
    let orchestrator =
        Orchestrator::new(repo.path().to_path_buf(), config, Some(Arc::clone(&gate)))
            .unwrap()
            .with_pm_service(Arc::clone(&pm));
    let (input_tx, input_rx) = tokio::sync::mpsc::channel(4);
    let interactive =
        tokio::spawn(orchestrator.run_interactive(input_rx, None, None, new_overflow_buf()));

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
    let first_delegation = wait_for_dispatch(pm.as_ref(), &first_task.id).await;
    persist_successful_worker_completion(
        pm.as_ref(),
        gate.as_ref(),
        &plan_id,
        "T1",
        &first_task.id,
        &first_delegation,
    )
    .await;
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
    let second_delegation = wait_for_dispatch(pm.as_ref(), &second_task.id).await;
    persist_successful_worker_completion(
        pm.as_ref(),
        gate.as_ref(),
        &plan_id,
        "T2",
        &second_task.id,
        &second_delegation,
    )
    .await;
    wait_for_system_approval(pm.as_ref(), &second_task.id).await;

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
            let recorded = audits.iter().any(|audit| {
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
            });
            if epic.status == pm.closed_status() && recorded {
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

    drop(input_tx);
    tokio::time::timeout(Duration::from_secs(5), interactive)
        .await
        .expect("interactive shutdown timed out")
        .expect("interactive task panicked")
        .expect("interactive task failed");
}

//! Integration test: [[spur-audit v1]] sentinel comments round-trip through
//! real `br comments add` + `br comments list --json`.

use std::path::Path;

use spur_mcp::plan::audit_sentinel::{self, AuditSentinelKind, CompletionState};
use tempfile::TempDir;

mod common;
fn br_available() -> bool {
    common::beads::br_available()
}

fn run_br(repo: &Path, args: &[&str]) -> Result<String, String> {
    common::beads::run_br(repo, args)
}

fn extract_id(json: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    v.get("id")
        .and_then(|x| x.as_str())
        .expect("id")
        .to_string()
}

#[ignore = "requires br on PATH; run with --ignored"]
#[test]
fn every_audit_sentinel_variant_round_trips_through_br_comments() {
    assert!(
        br_available(),
        "this test requires `br` on PATH; run with `cargo test -- --ignored`"
    );
    let dir = TempDir::new().unwrap();
    run_br(dir.path(), &["init"]).unwrap();
    let id = extract_id(&run_br(dir.path(), &["create", "t", "-t", "task"]).unwrap());

    let variants = vec![
        AuditSentinelKind::PlanSubmit {
            plan_id: "P1".into(),
            epic_issue_id: id.clone(),
            task_ids: vec!["bd-a".into(), "bd-b".into()],
            base_snapshot_branch: Some("spur/brain-snapshot-test".into()),
            base_snapshot_oid: Some("0123456789abcdef0123456789abcdef01234567".into()),
            execution_mode: None,
            brain_session_id: Some("brain-1".into()),
        },
        AuditSentinelKind::EpicCompletion {
            outcome: spur_mcp::plan::audit_sentinel::EpicCompletionOutcome::AllApproved,
            plan_id: "P1".into(),
            epic_id: id.clone(),
        },
        AuditSentinelKind::Dispatch {
            delegation_id: "del-1".into(),
            worker: "codex".into(),
            attempt: 1,
        },
        AuditSentinelKind::DispatchOrphanCleared {
            delegation_id: "del-1".into(),
            reason: "restart-orphan-cleared".into(),
        },
        AuditSentinelKind::Completion {
            delegation_id: "del-1".into(),
            completion_state: CompletionState::AwaitingReview,
            superseded: false,
            worker_branch: Some("feat/x".into()),
            result_summary: Some("worker narrative: three refactors".into()),
            artifact_uri: None,
            dispatched_base_oid: None,
        },
        AuditSentinelKind::Approval {
            delegation_id: "del-1".into(),
        },
        AuditSentinelKind::Rejection {
            delegation_id: "del-1".into(),
            feedback: "needs more tests".into(),
        },
        AuditSentinelKind::ReviewFeedback {
            delegation_id: "del-1".into(),
            attempt: 1,
            feedback: "add null check".into(),
            worker_branch: Some("spur/worker-x".into()),
            summary: Some("did thing".into()),
        },
    ];

    for v in &variants {
        let body = audit_sentinel::encode_comment(v);
        run_br(dir.path(), &["comments", "add", &id, &body]).unwrap();
    }

    let list_out = run_br(dir.path(), &["comments", "list", &id]).unwrap();
    let items: serde_json::Value = serde_json::from_str(&list_out).unwrap();
    let texts: Vec<String> = items
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c.get("text").and_then(|t| t.as_str()).map(String::from))
        .collect();

    for v in &variants {
        let found = texts.iter().any(|t| {
            audit_sentinel::parse_comment(t)
                .and_then(|r| r.ok())
                .is_some_and(|k| k == *v)
        });
        assert!(
            found,
            "variant {v:?} did not round-trip through br comments: {texts:?}"
        );
    }
}

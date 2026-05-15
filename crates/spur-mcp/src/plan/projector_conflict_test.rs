use crate::plan::audit_sentinel::AuditSentinelKind;
use crate::plan::projector::project_status_for_issue;
use crate::plan::PlanTaskStatus;
use spur_pm::Issue;

#[test]
fn test_conflict_panic() {
    let issue = Issue {
        id: "test-1".into(),
        source: spur_pm::PmSource::Beads,
        title: "Task".into(),
        body: "Body".into(),
        status: "open".into(),
        labels: vec![
            "spur:delegation-id:del-123".into(),
            "signal:integration-conflict".into(),
        ],
        assignee: None,
        url: String::new(),
        priority: None,
        issue_type: Some("task".into()),
        external_ref: None,
        source_system: None,
        source_repo: None,
        blocked_by: vec![],
        due_at: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    let audits = vec![AuditSentinelKind::Dispatch {
        delegation_id: "del-123".into(),
        worker: "codex".into(),
        attempt: 1,
    }];
    let status = project_status_for_issue(&issue, &audits, false, "closed");
    assert!(matches!(
        status,
        PlanTaskStatus::BlockedOnSetupConflict { .. }
    ));
}

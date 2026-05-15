use crate::plan::audit_sentinel::AuditSentinelKind;
use crate::plan::projector::project_status_for_issue;
use crate::plan::PlanTaskStatus;
use spur_pm::Issue;

#[test]
fn test_conflict_panic() {
    let issue = Issue {
        id: "test-1".into(),
        status: "open".into(),
        labels: vec![
            "spur:delegation-id:del-123".into(),
            "spur:signal:integration-conflict".into(),
        ],
        ..Default::default()
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

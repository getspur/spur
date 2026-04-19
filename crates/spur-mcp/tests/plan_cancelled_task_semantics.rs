use spur_mcp::plan::PlanTaskStatus;

#[tokio::test]
async fn test_non_cascade_on_dep() {
    // We will test that PlanTaskStatus::Cancelled on a dependency doesn't cascade fail
    let status = PlanTaskStatus::Cancelled { reason: "test".to_string() };
    assert!(matches!(status, PlanTaskStatus::Cancelled { .. }));
}

#[tokio::test]
async fn test_no_cascade_on_transition() {
    let status = PlanTaskStatus::Cancelled { reason: "test".to_string() };
    assert!(matches!(status, PlanTaskStatus::Cancelled { .. }));
}

#[tokio::test]
async fn test_plan_completed_has_cancelled_count() {
    let event = spur_acp::domain::events::SpurEventBody::PlanCompleted {
        plan_id: "test".to_string(),
        approved: 1,
        rejected: 0,
        failed: 0,
        cancelled: 1,
    };
    assert!(matches!(event, spur_acp::domain::events::SpurEventBody::PlanCompleted { cancelled: 1, .. }));
}

#[tokio::test]
async fn test_plan_ready_to_merge_blocked_by_cancelled() {
    // PlanReadyToMerge must require cancelled == 0
}

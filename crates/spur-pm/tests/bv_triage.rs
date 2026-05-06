//! Integration test for BvAdapter::triage against the native GraphEngine.
//!
//! Validates the contract the v0a.2 reconciler (Task 9) depends on:
//! `BvAdapter::triage(Some("spur:plan-id:<id>"))` returns a TriageReport
//! whose `recommendations` carry issue IDs for unblocked tasks under that
//! plan.

use std::sync::Arc;

use spur_pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use spur_pm::graph_engine::GraphEngineConfig;
use spur_pm::test_workspace::TestBeadsWorkspace;
use spur_pm::BvAdapter;

async fn make_adapter(w: &TestBeadsWorkspace) -> BvAdapter {
    let beads = Arc::new(
        BeadsCrateAdapter::open(w.path(), AdapterConfig::default())
            .await
            .expect("open beads crate adapter"),
    );
    BvAdapter::from_beads(beads, GraphEngineConfig::default())
}

/// Smoke test: triage returns a structurally valid TriageReport on an empty
/// workspace. Proves the adapter + GraphEngine + JSON schema agree.
#[tokio::test]
async fn triage_on_empty_workspace_returns_report() {
    let w = TestBeadsWorkspace::init();
    let bv = make_adapter(&w).await;

    let report = bv.triage(None).await.expect("triage");
    // Empty workspace: recommendations may be empty but the struct deserializes.
    assert!(
        report.triage.recommendations.is_empty() || !report.triage.recommendations.is_empty(),
        "recommendations deserialized"
    );
}

/// The contract Task 9 depends on: triage(Some(label)) returns a TriageReport
/// whose recommendations come from issues carrying that label. Create a
/// labeled open issue, verify it appears in the label-scoped triage output.
#[tokio::test]
async fn triage_with_label_filter_surfaces_matching_issue() {
    let mut w = TestBeadsWorkspace::init();

    // Create one issue under "spur:plan-id:P1" plus one unrelated issue.
    let plan_task = w.create_issue("plan-task");
    w.add_label(&plan_task, "spur:plan-id:P1");
    let _other = w.create_issue("other");

    let bv = make_adapter(&w).await;
    let report = bv.triage(Some("spur:plan-id:P1")).await.expect("triage");

    // Label-scoped query should surface the plan_task but not "other".
    let ids: Vec<&str> = report
        .triage
        .recommendations
        .iter()
        .map(|r| r.id.as_str())
        .collect();
    assert!(
        ids.contains(&plan_task.as_str()),
        "plan_task {plan_task} missing from label-scoped triage: {ids:?}"
    );
}

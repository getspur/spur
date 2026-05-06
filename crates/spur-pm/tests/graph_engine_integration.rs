//! End-to-end tests of GraphEngine over a real beads_rust SqliteStorage.

use std::sync::Arc;

use spur_pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use spur_pm::graph::{DependencyGraph, GraphEdge};
use spur_pm::graph_engine::{GraphEngine, GraphEngineConfig};
use spur_pm::test_workspace::TestBeadsWorkspace;

async fn open_engine(w: &TestBeadsWorkspace) -> GraphEngine {
    let beads = Arc::new(
        BeadsCrateAdapter::open(w.path(), AdapterConfig::default())
            .await
            .expect("open beads adapter"),
    );
    GraphEngine::new(beads, GraphEngineConfig::default())
}

fn adjacency_edges(graph: &DependencyGraph) -> &[GraphEdge] {
    graph
        .adjacency
        .as_ref()
        .expect("json graph has adjacency")
        .edges
        .as_deref()
        .expect("graph has edges")
}

fn has_edge(edges: &[GraphEdge], from: &str, to: &str, edge_type: &str) -> bool {
    edges.iter().any(|edge| {
        edge.from == from && edge.to == to && edge.edge_type.as_deref() == Some(edge_type)
    })
}

#[tokio::test(flavor = "current_thread")]
async fn triage_and_plan_observe_blocker_to_blocked_direction() {
    let mut w = TestBeadsWorkspace::init();
    let blocker = w.create_issue("First task");
    let blocked = w.create_issue("Blocked task");
    w.add_dep(&blocked, &blocker);

    let engine = open_engine(&w).await;

    let triage = engine.triage(None).await.expect("triage report");
    assert_eq!(triage.triage.quick_ref.open_count, 2);
    assert_eq!(triage.triage.quick_ref.actionable_count, 1);
    assert_eq!(triage.triage.quick_ref.blocked_count, 1);
    assert_eq!(triage.triage.quick_ref.top_picks[0].id, blocker);
    assert_eq!(triage.triage.quick_ref.top_picks[0].unblocks, 1);

    let blocked_recommendation = triage
        .triage
        .recommendations
        .iter()
        .find(|item| item.id == blocked)
        .expect("blocked issue appears in recommendations");
    assert_eq!(
        blocked_recommendation.blocked_by,
        vec![blocker.clone()],
        "incoming blocking edge must point from blocker to blocked"
    );

    let plan = engine.plan(None).await.expect("execution plan");
    assert_eq!(plan.plan.total_actionable, 1);
    assert_eq!(plan.plan.total_blocked, 1);
    assert_eq!(plan.plan.tracks.len(), 2);
    assert_eq!(plan.plan.tracks[0].items[0].id, blocker);
    assert_eq!(
        plan.plan.tracks[0].items[0].unblocks.as_deref(),
        Some(std::slice::from_ref(&blocked))
    );
    assert_eq!(plan.plan.tracks[1].items[0].id, blocked);
    assert_eq!(
        plan.plan.summary.unwrap().highest_impact.as_deref(),
        Some(blocker.as_str())
    );
    assert!(!plan.data_hash.as_deref().unwrap_or_default().is_empty());
    assert!(!plan.raw.is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn insights_and_alerts_report_cycles_from_sqlite_workspace() {
    let mut w = TestBeadsWorkspace::init();
    let first = w.create_issue("Cycle first");
    let second = w.create_issue("Cycle second");
    w.storage
        .add_dependency(&first, &second, "related", "test")
        .expect("add first related edge");
    w.storage
        .add_dependency(&second, &first, "related", "test")
        .expect("add second related edge");

    let engine = open_engine(&w).await;

    let insights = engine.insights(None).await.expect("graph insights");
    let mut expected_cycle = vec![first.clone(), second.clone()];
    expected_cycle.sort();
    assert!(insights.cycles.contains(&expected_cycle));
    assert!(!insights.data_hash.as_deref().unwrap_or_default().is_empty());
    assert!(!insights.raw.is_null());

    let alerts = engine.alerts().await.expect("alert report");
    let cycle_alerts: Vec<_> = alerts
        .alerts
        .iter()
        .filter(|alert| alert.alert_type == "cycle")
        .collect();
    assert!(!cycle_alerts.is_empty());
    assert!(cycle_alerts
        .iter()
        .all(|alert| alert.severity == "critical"));
    assert_eq!(
        alerts.summary.as_ref().expect("alert summary").critical,
        cycle_alerts.len()
    );
    assert!(!alerts.data_hash.as_deref().unwrap_or_default().is_empty());
    assert!(!alerts.raw.is_null());
}

#[tokio::test(flavor = "current_thread")]
async fn subgraph_methods_return_adjacency_and_label_scoped_graphs() {
    let mut w = TestBeadsWorkspace::init();
    let label = "spur:plan-id:T15";
    let blocker = w.create_issue("Labelled blocker");
    let blocked = w.create_issue("Labelled blocked");
    let outside = w.create_issue("Outside issue");
    w.add_label(&blocker, label);
    w.add_label(&blocked, label);
    w.add_dep(&blocked, &blocker);

    let engine = open_engine(&w).await;

    let issue_graph = engine
        .subgraph(&blocked, Some(1), Some("json"))
        .await
        .expect("issue subgraph");
    let issue_edges = adjacency_edges(&issue_graph);
    assert_eq!(issue_graph.nodes, 2);
    assert_eq!(issue_graph.edges, 1);
    assert!(has_edge(issue_edges, &blocker, &blocked, "blocks"));
    assert_eq!(
        issue_graph.raw["adjacency"]["edges"][0]["from"].as_str(),
        Some(blocker.as_str())
    );

    let label_graph = engine
        .graph_by_label(label, Some("json"))
        .await
        .expect("label graph");
    let label_edges = adjacency_edges(&label_graph);
    let mut node_ids: Vec<&str> = label_graph
        .adjacency
        .as_ref()
        .expect("json graph has adjacency")
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect();
    node_ids.sort();
    let mut expected_ids = vec![blocker.as_str(), blocked.as_str()];
    expected_ids.sort();
    assert_eq!(node_ids, expected_ids);
    assert!(!node_ids.contains(&outside.as_str()));
    assert!(has_edge(label_edges, &blocker, &blocked, "blocks"));
}

#[tokio::test(flavor = "current_thread")]
async fn data_hash_stable_across_two_invocations() {
    let mut w = TestBeadsWorkspace::init();
    let issue = w.create_issue("Stable issue");
    w.add_label(&issue, "stable");

    let engine = open_engine(&w).await;

    let first = engine.triage(None).await.expect("first triage report");
    let second = engine.triage(None).await.expect("second triage report");
    assert_eq!(first.data_hash, second.data_hash);
    assert!(!first.data_hash.as_deref().unwrap_or_default().is_empty());
}

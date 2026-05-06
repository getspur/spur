use crate::graph::{AlertReport, DependencyGraph, ExecutionPlan, GraphInsights, TriageReport};
use serde::Serialize;

pub fn serialize_triage(r: &TriageReport) -> serde_json::Value {
    serialize_report(r)
}

pub fn serialize_plan(r: &ExecutionPlan) -> serde_json::Value {
    serialize_report(r)
}

pub fn serialize_insights(r: &GraphInsights) -> serde_json::Value {
    serialize_report(r)
}

pub fn serialize_alerts(r: &AlertReport) -> serde_json::Value {
    serialize_report(r)
}

pub fn serialize_subgraph(r: &DependencyGraph) -> serde_json::Value {
    serialize_report(r)
}

fn serialize_report<T: Serialize>(report: &T) -> serde_json::Value {
    serde_json::to_value(report).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{
        AdjacencyData, Alert, AlertSummary, ExecutionTrack, GraphEdge, GraphNode, InsightItem,
        PlanBody, QuickRef, TopPick, TrackItem, TriageResult,
    };

    #[test]
    fn triage_serialization_preserves_wire_fields_and_omits_raw() {
        let report = TriageReport {
            generated_at: Some("2026-05-05T00:00:00Z".into()),
            data_hash: Some("abc".into()),
            triage: TriageResult {
                quick_ref: QuickRef {
                    open_count: 1,
                    actionable_count: 1,
                    blocked_count: 0,
                    in_progress_count: 0,
                    top_picks: vec![TopPick {
                        id: "bd-1".into(),
                        title: "T".into(),
                        score: 0.5,
                        reasons: vec!["r".into()],
                        unblocks: 0,
                    }],
                },
                ..TriageResult::default()
            },
            usage_hints: vec![],
            raw: serde_json::json!({"recursive": true}),
        };

        let value = serialize_triage(&report);

        assert_eq!(value["data_hash"], "abc");
        assert_eq!(value["triage"]["quick_ref"]["top_picks"][0]["id"], "bd-1");
        assert!(value.get("raw").is_none());
    }

    #[test]
    fn insights_serialization_uses_go_capitalized_wire_fields() {
        let report = GraphInsights {
            generated_at: Some("2026-05-05T00:00:00Z".into()),
            data_hash: Some("hash".into()),
            bottlenecks: vec![InsightItem {
                id: "bd-b".into(),
                value: 2.0,
            }],
            keystones: vec![InsightItem {
                id: "bd-k".into(),
                value: 1.0,
            }],
            cluster_density: 0.25,
            raw: serde_json::json!({"recursive": true}),
            ..GraphInsights::default()
        };

        let value = serialize_insights(&report);

        assert_eq!(value["Bottlenecks"][0]["ID"], "bd-b");
        assert_eq!(value["Keystones"][0]["Value"], 1.0);
        assert_eq!(value["ClusterDensity"], 0.25);
        assert!(value.get("bottlenecks").is_none());
        assert!(value.get("raw").is_none());
    }

    #[test]
    fn plan_alerts_and_subgraph_serialize_for_raw_passthrough() {
        let plan = ExecutionPlan {
            generated_at: Some("2026-05-05T00:00:00Z".into()),
            data_hash: Some("plan-hash".into()),
            plan: PlanBody {
                tracks: vec![ExecutionTrack {
                    track_id: "track-A".into(),
                    items: vec![TrackItem {
                        id: "bd-1".into(),
                        title: "First".into(),
                        priority: Some(1),
                        status: Some("open".into()),
                        unblocks: Some(vec!["bd-2".into()]),
                    }],
                    reason: Some("root".into()),
                }],
                total_actionable: 1,
                total_blocked: 0,
                summary: None,
            },
            usage_hints: vec![],
            raw: serde_json::json!({"recursive": true}),
        };
        let alerts = AlertReport {
            generated_at: Some("2026-05-05T00:00:00Z".into()),
            data_hash: Some("alerts-hash".into()),
            alerts: vec![Alert {
                alert_type: "cycle".into(),
                severity: "warning".into(),
                message: "cycle found".into(),
                issue_ids: vec!["bd-1".into(), "bd-2".into()],
                ..Alert::default()
            }],
            summary: Some(AlertSummary {
                total: 1,
                warning: 1,
                ..AlertSummary::default()
            }),
            usage_hints: vec![],
            raw: serde_json::json!({"recursive": true}),
        };
        let subgraph = DependencyGraph {
            format: Some("json".into()),
            nodes: 2,
            edges: 1,
            data_hash: Some("graph-hash".into()),
            adjacency: Some(AdjacencyData {
                nodes: vec![GraphNode {
                    id: "bd-1".into(),
                    title: Some("First".into()),
                    status: Some("open".into()),
                    priority: Some(1),
                    labels: vec!["label".into()],
                    pagerank: Some(0.5),
                }],
                edges: Some(vec![GraphEdge {
                    from: "bd-1".into(),
                    to: "bd-2".into(),
                    edge_type: Some("blocks".into()),
                }]),
            }),
            raw: serde_json::json!({"recursive": true}),
            ..DependencyGraph::default()
        };

        let plan_value = serialize_plan(&plan);
        let alerts_value = serialize_alerts(&alerts);
        let subgraph_value = serialize_subgraph(&subgraph);

        assert_eq!(
            plan_value["plan"]["tracks"][0]["items"][0]["unblocks"][0],
            "bd-2"
        );
        assert_eq!(alerts_value["alerts"][0]["type"], "cycle");
        assert_eq!(subgraph_value["adjacency"]["edges"][0]["type"], "blocks");
        assert!(plan_value.get("raw").is_none());
        assert!(alerts_value.get("raw").is_none());
        assert!(subgraph_value.get("raw").is_none());
    }
}

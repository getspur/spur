//! Types for graph analysis results from `bv` (beads_viewer) robot protocol.
//!
//! All types use `#[serde(default)]` liberally so that bv version changes
//! (adding/removing fields) don't break deserialization. Each top-level
//! report carries a `raw: serde_json::Value` field that holds the full bv
//! output for MCP passthrough to brain agents.

use serde::{Deserialize, Serialize};

// ─── Triage (--robot-triage) ─────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TriageReport {
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub data_hash: Option<String>,
    #[serde(default)]
    pub triage: TriageResult,
    #[serde(default)]
    pub usage_hints: Vec<String>,
    /// Full bv JSON output for MCP passthrough.
    #[serde(skip)]
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TriageResult {
    #[serde(default)]
    pub meta: serde_json::Value,
    #[serde(default)]
    pub quick_ref: QuickRef,
    #[serde(default)]
    pub recommendations: Vec<Recommendation>,
    #[serde(default)]
    pub quick_wins: Vec<QuickWin>,
    #[serde(default)]
    pub blockers_to_clear: Vec<BlockerInfo>,
    #[serde(default)]
    pub project_health: ProjectHealth,
    #[serde(default)]
    pub alerts: Vec<TriageAlert>,
    #[serde(default)]
    pub commands: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct QuickRef {
    #[serde(default)]
    pub open_count: usize,
    #[serde(default)]
    pub actionable_count: usize,
    #[serde(default)]
    pub blocked_count: usize,
    #[serde(default)]
    pub in_progress_count: usize,
    #[serde(default)]
    pub top_picks: Vec<TopPick>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TopPick {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub unblocks: usize,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Recommendation {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(rename = "type", default)]
    pub issue_type: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub score: f64,
    /// Full score breakdown — kept as Value (deep nested, not needed in Rust).
    #[serde(default)]
    pub breakdown: serde_json::Value,
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub unblocks_ids: Vec<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct QuickWin {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub unblocks_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct BlockerInfo {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub unblocks_count: usize,
    #[serde(default)]
    pub unblocks_ids: Vec<String>,
    #[serde(default)]
    pub actionable: bool,
    #[serde(default)]
    pub blocked_by: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProjectHealth {
    #[serde(default)]
    pub counts: HealthCounts,
    #[serde(default)]
    pub graph: GraphHealth,
    #[serde(default)]
    pub velocity: Option<VelocitySnapshot>,
    #[serde(default)]
    pub staleness: Option<StalenessInfo>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HealthCounts {
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub open: usize,
    #[serde(default)]
    pub closed: usize,
    #[serde(default)]
    pub blocked: usize,
    #[serde(default)]
    pub actionable: usize,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GraphHealth {
    #[serde(default)]
    pub node_count: usize,
    #[serde(default)]
    pub edge_count: usize,
    #[serde(default)]
    pub density: f64,
    #[serde(default)]
    pub has_cycles: bool,
    #[serde(default)]
    pub cycle_count: usize,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct VelocitySnapshot {
    #[serde(default)]
    pub closed_last_7_days: usize,
    #[serde(default)]
    pub closed_last_30_days: usize,
    #[serde(default)]
    pub avg_days_to_close: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct StalenessInfo {
    #[serde(default)]
    pub stale_count: usize,
    #[serde(default)]
    pub stalest_issue_id: Option<String>,
    #[serde(default)]
    pub stalest_issue_days: usize,
    #[serde(default)]
    pub threshold_days: usize,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TriageAlert {
    #[serde(rename = "type", default)]
    pub alert_type: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub issue_id: Option<String>,
}

// ─── Execution Plan (--robot-plan) ───────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ExecutionPlan {
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub data_hash: Option<String>,
    #[serde(default)]
    pub plan: PlanBody,
    #[serde(default)]
    pub usage_hints: Vec<String>,
    #[serde(skip)]
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PlanBody {
    #[serde(default)]
    pub tracks: Vec<ExecutionTrack>,
    #[serde(default)]
    pub total_actionable: usize,
    #[serde(default)]
    pub total_blocked: usize,
    #[serde(default)]
    pub summary: Option<PlanSummary>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ExecutionTrack {
    #[serde(default)]
    pub track_id: String,
    #[serde(default)]
    pub items: Vec<TrackItem>,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TrackItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub status: Option<String>,
    /// IDs unblocked when this item completes. `null` in bv JSON when empty.
    #[serde(default)]
    pub unblocks: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PlanSummary {
    #[serde(default)]
    pub highest_impact: Option<String>,
    #[serde(default)]
    pub impact_reason: Option<String>,
    #[serde(default)]
    pub unblocks_count: usize,
}

// ─── Graph Insights (--robot-insights) ───────────────────────────────
//
// Note: bv's Go `Insights` struct has NO json tags — fields serialize
// with Go field names (capitalized). InsightItem likewise: `ID`, `Value`.

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GraphInsights {
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub data_hash: Option<String>,

    #[serde(rename = "Bottlenecks", default)]
    pub bottlenecks: Vec<InsightItem>,
    #[serde(rename = "Keystones", default)]
    pub keystones: Vec<InsightItem>,
    #[serde(rename = "Influencers", default)]
    pub influencers: Vec<InsightItem>,
    #[serde(rename = "Hubs", default)]
    pub hubs: Vec<InsightItem>,
    #[serde(rename = "Authorities", default)]
    pub authorities: Vec<InsightItem>,
    #[serde(rename = "Cores", default)]
    pub cores: Vec<InsightItem>,
    #[serde(rename = "Articulation", default)]
    pub articulation: Vec<String>,
    #[serde(rename = "Orphans", default)]
    pub orphans: Vec<String>,
    #[serde(rename = "Cycles", default)]
    pub cycles: Vec<Vec<String>>,
    #[serde(rename = "ClusterDensity", default)]
    pub cluster_density: f64,

    #[serde(default)]
    pub top_what_ifs: Vec<WhatIfEntry>,

    #[serde(skip)]
    pub raw: serde_json::Value,
}

/// Graph metric item. bv serializes with Go field names: `ID`, `Value`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct InsightItem {
    #[serde(rename = "ID", default)]
    pub id: String,
    #[serde(rename = "Value", default)]
    pub value: f64,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WhatIfEntry {
    #[serde(default)]
    pub issue_id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub delta: Option<WhatIfDelta>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WhatIfDelta {
    #[serde(default)]
    pub direct_unblocks: usize,
    #[serde(default)]
    pub transitive_unblocks: usize,
    #[serde(default)]
    pub blocked_reduction: usize,
    #[serde(default)]
    pub depth_reduction: f64,
    #[serde(default)]
    pub estimated_days_saved: Option<f64>,
    #[serde(default)]
    pub explanation: Option<String>,
}

// ─── Alerts (--robot-alerts) ─────────────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AlertReport {
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub data_hash: Option<String>,
    #[serde(default)]
    pub alerts: Vec<Alert>,
    #[serde(default)]
    pub summary: Option<AlertSummary>,
    #[serde(default)]
    pub usage_hints: Vec<String>,
    #[serde(skip)]
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Alert {
    #[serde(rename = "type", default)]
    pub alert_type: String,
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub issue_ids: Vec<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub details: Vec<String>,
    #[serde(default)]
    pub baseline_value: Option<f64>,
    #[serde(default)]
    pub current_value: Option<f64>,
    #[serde(default)]
    pub delta: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AlertSummary {
    #[serde(default)]
    pub total: usize,
    #[serde(default)]
    pub critical: usize,
    #[serde(default)]
    pub warning: usize,
    #[serde(default)]
    pub info: usize,
}

// ─── Dependency Graph (--robot-graph) ────────────────────────────────

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DependencyGraph {
    #[serde(default)]
    pub format: Option<String>,
    /// Populated for dot/mermaid formats; None for json format.
    #[serde(default)]
    pub graph: Option<String>,
    /// Total node count.
    #[serde(default)]
    pub nodes: usize,
    /// Total edge count.
    #[serde(default)]
    pub edges: usize,
    #[serde(default)]
    pub data_hash: Option<String>,
    /// Populated for json format; None for dot/mermaid.
    #[serde(default)]
    pub adjacency: Option<AdjacencyData>,
    #[serde(skip)]
    pub raw: serde_json::Value,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AdjacencyData {
    #[serde(default)]
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub edges: Option<Vec<GraphEdge>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GraphNode {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub pagerank: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GraphEdge {
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub to: String,
    #[serde(rename = "type", default)]
    pub edge_type: Option<String>,
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_triage_report() {
        let json = r#"{
            "generated_at": "2026-04-17T06:31:32Z",
            "data_hash": "abc123",
            "triage": {
                "quick_ref": {
                    "open_count": 6,
                    "actionable_count": 6,
                    "blocked_count": 0,
                    "in_progress_count": 0,
                    "top_picks": [
                        {"id": "bd-1uk", "title": "Test", "score": 0.27, "reasons": ["high priority"], "unblocks": 0}
                    ]
                },
                "recommendations": [],
                "quick_wins": [],
                "blockers_to_clear": [],
                "project_health": {
                    "counts": {"total": 6, "open": 6, "closed": 0, "blocked": 0, "actionable": 6},
                    "graph": {"node_count": 6, "edge_count": 0, "density": 0.0, "has_cycles": false}
                }
            },
            "usage_hints": ["jq '.triage.quick_ref'"]
        }"#;
        let report: TriageReport = serde_json::from_str(json).unwrap();
        assert_eq!(report.triage.quick_ref.open_count, 6);
        assert_eq!(report.triage.quick_ref.top_picks.len(), 1);
        assert_eq!(report.triage.quick_ref.top_picks[0].id, "bd-1uk");
        assert_eq!(report.triage.project_health.counts.total, 6);
    }

    #[test]
    fn deserialize_execution_plan() {
        let json = r#"{
            "generated_at": "2026-04-17T06:31:34Z",
            "data_hash": "abc123",
            "plan": {
                "tracks": [
                    {"track_id": "track-A", "items": [{"id": "bd-1kn", "title": "Test", "priority": 3, "status": "open", "unblocks": null}], "reason": "Single actionable item"}
                ],
                "total_actionable": 6,
                "total_blocked": 0,
                "summary": {"highest_impact": "bd-1kn", "impact_reason": "No downstream", "unblocks_count": 0}
            }
        }"#;
        let plan: ExecutionPlan = serde_json::from_str(json).unwrap();
        assert_eq!(plan.plan.tracks.len(), 1);
        assert_eq!(plan.plan.tracks[0].track_id, "track-A");
        assert!(plan.plan.tracks[0].items[0].unblocks.is_none());
    }

    #[test]
    fn deserialize_graph_insights_go_capitalized() {
        let json = r#"{
            "generated_at": "2026-04-17T00:00:00Z",
            "Bottlenecks": [],
            "Keystones": [{"ID": "bd-1kn", "Value": 1.0}],
            "Influencers": [],
            "Hubs": [],
            "Authorities": [],
            "Cores": [],
            "Articulation": [],
            "Orphans": [],
            "Cycles": [],
            "ClusterDensity": 0.0
        }"#;
        let insights: GraphInsights = serde_json::from_str(json).unwrap();
        assert_eq!(insights.keystones.len(), 1);
        assert_eq!(insights.keystones[0].id, "bd-1kn");
        assert_eq!(insights.keystones[0].value, 1.0);
    }

    #[test]
    fn deserialize_alert_report_empty() {
        let json = r#"{
            "generated_at": "2026-04-17T06:31:33Z",
            "data_hash": "abc",
            "alerts": [],
            "summary": {"total": 0, "critical": 0, "warning": 0, "info": 0}
        }"#;
        let report: AlertReport = serde_json::from_str(json).unwrap();
        assert!(report.alerts.is_empty());
        assert_eq!(report.summary.as_ref().unwrap().total, 0);
    }

    #[test]
    fn deserialize_dependency_graph_json() {
        let json = r#"{
            "format": "json",
            "nodes": 1,
            "edges": 0,
            "data_hash": "abc",
            "adjacency": {
                "nodes": [{"id": "bd-1uk", "title": "Test", "status": "open", "priority": 1, "labels": ["tui"], "pagerank": 0.167}],
                "edges": null
            }
        }"#;
        let graph: DependencyGraph = serde_json::from_str(json).unwrap();
        assert_eq!(graph.nodes, 1);
        let adj = graph.adjacency.unwrap();
        assert_eq!(adj.nodes.len(), 1);
        assert_eq!(adj.nodes[0].id, "bd-1uk");
        assert!(adj.edges.is_none());
    }

    #[test]
    fn deserialize_dependency_graph_mermaid() {
        let json = r#"{
            "format": "mermaid",
            "graph": "graph TD\n  A-->B",
            "nodes": 2,
            "edges": 1
        }"#;
        let graph: DependencyGraph = serde_json::from_str(json).unwrap();
        assert_eq!(graph.format.as_deref(), Some("mermaid"));
        assert!(graph.graph.is_some());
        assert!(graph.adjacency.is_none());
    }
}

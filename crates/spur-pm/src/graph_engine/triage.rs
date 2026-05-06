use crate::graph::{
    BlockerInfo, GraphHealth, HealthCounts, ProjectHealth, QuickRef, QuickWin, Recommendation,
    TopPick, TriageReport, TriageResult,
};
use crate::graph_engine::score::{
    is_actionable, score_all, transitive_unblocks, ScoreBreakdown, ScoreConfig,
};
use crate::graph_engine::snapshot::GraphSnapshot;
use petgraph::algo::{is_cyclic_directed, tarjan_scc};
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::cmp::Ordering;
use std::collections::HashMap;

pub fn compute_triage(snap: &GraphSnapshot, cfg: &ScoreConfig) -> TriageReport {
    let scores = score_all(snap, cfg);
    let unblocks_by_ix = unblocks_by_ix(snap);

    let mut ranked: Vec<(NodeIndex, &ScoreBreakdown)> = scores
        .iter()
        .map(|(ix, breakdown)| (*ix, breakdown))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.normalized
            .partial_cmp(&a.1.normalized)
            .unwrap_or(Ordering::Equal)
            .then_with(|| snap.graph[a.0].priority.cmp(&snap.graph[b.0].priority))
            .then_with(|| {
                unblocks_for(&unblocks_by_ix, b.0).cmp(&unblocks_for(&unblocks_by_ix, a.0))
            })
            .then_with(|| snap.graph[a.0].id.cmp(&snap.graph[b.0].id))
    });

    let top_picks = ranked
        .iter()
        .take(5)
        .map(|(ix, breakdown)| TopPick {
            id: snap.graph[*ix].id.clone(),
            title: snap.graph[*ix].title.clone(),
            score: breakdown.normalized,
            reasons: reasons_for(snap, *ix, breakdown, &unblocks_by_ix),
            unblocks: unblocks_for(&unblocks_by_ix, *ix),
        })
        .collect();

    let recommendations = ranked
        .iter()
        .take(20)
        .map(|(ix, breakdown)| {
            let node = &snap.graph[*ix];
            Recommendation {
                id: node.id.clone(),
                title: node.title.clone(),
                issue_type: Some(node.issue_type.clone()),
                status: Some(node.status.clone()),
                priority: Some(node.priority),
                labels: node.labels.clone(),
                score: breakdown.normalized,
                breakdown: serde_json::json!({
                    "priority": breakdown.priority_component,
                    "unblocks": breakdown.unblocks_component,
                    "actionable": breakdown.actionable_component,
                    "freshness": breakdown.freshness_component,
                    "age_penalty": breakdown.age_penalty_component,
                    "raw": breakdown.raw,
                }),
                action: Some(if is_actionable(snap, *ix) {
                    "start".into()
                } else {
                    "wait".into()
                }),
                reasons: reasons_for(snap, *ix, breakdown, &unblocks_by_ix),
                unblocks_ids: blocking_targets(snap, *ix),
                blocked_by: blocking_sources(snap, *ix),
            }
        })
        .collect();

    let quick_win_threshold = quick_win_threshold(&scores, cfg.quick_win_percentile);
    let quick_wins = ranked
        .iter()
        .filter(|(ix, breakdown)| {
            let node = &snap.graph[*ix];
            is_actionable(snap, *ix)
                && node.priority >= 2
                && breakdown.normalized >= quick_win_threshold
        })
        .take(10)
        .map(|(ix, breakdown)| QuickWin {
            id: snap.graph[*ix].id.clone(),
            title: snap.graph[*ix].title.clone(),
            score: breakdown.normalized,
            reason: Some(reasons_for(snap, *ix, breakdown, &unblocks_by_ix).join("; ")),
            unblocks_ids: blocking_targets(snap, *ix),
        })
        .collect();

    let mut blockers_to_clear: Vec<BlockerInfo> = snap
        .graph
        .node_indices()
        .filter_map(|ix| {
            let node = &snap.graph[ix];
            if node.status == "closed" {
                return None;
            }

            let unblocks = unblocks_for(&unblocks_by_ix, ix);
            if unblocks == 0 {
                return None;
            }

            Some(BlockerInfo {
                id: node.id.clone(),
                title: node.title.clone(),
                unblocks_count: unblocks,
                unblocks_ids: blocking_targets(snap, ix),
                actionable: is_actionable(snap, ix),
                blocked_by: blocking_sources(snap, ix),
            })
        })
        .collect();
    blockers_to_clear.sort_by(|a, b| {
        b.unblocks_count
            .cmp(&a.unblocks_count)
            .then_with(|| a.id.cmp(&b.id))
    });
    blockers_to_clear.truncate(10);

    let project_health = compute_project_health(snap);
    let in_progress_count = snap
        .graph
        .node_indices()
        .filter(|&ix| snap.graph[ix].status == "in_progress")
        .count();

    TriageReport {
        generated_at: Some(snap.generated_at.to_rfc3339()),
        data_hash: Some(snap.data_hash.clone()),
        triage: TriageResult {
            meta: serde_json::json!({
                "engine": "spur-graph-engine",
                "version": "1",
            }),
            quick_ref: QuickRef {
                open_count: project_health.counts.open,
                actionable_count: project_health.counts.actionable,
                blocked_count: project_health.counts.blocked,
                in_progress_count,
                top_picks,
            },
            recommendations,
            quick_wins,
            blockers_to_clear,
            project_health,
            alerts: Vec::new(),
            commands: serde_json::Value::Null,
        },
        usage_hints: vec![
            "jq '.triage.quick_ref.top_picks'".into(),
            "jq '.triage.recommendations[0:3]'".into(),
        ],
        raw: serde_json::Value::Null,
    }
}

fn unblocks_by_ix(snap: &GraphSnapshot) -> HashMap<NodeIndex, usize> {
    snap.graph
        .node_indices()
        .map(|ix| (ix, transitive_unblocks(snap, ix)))
        .collect()
}

fn unblocks_for(unblocks_by_ix: &HashMap<NodeIndex, usize>, ix: NodeIndex) -> usize {
    unblocks_by_ix.get(&ix).copied().unwrap_or_default()
}

fn reasons_for(
    snap: &GraphSnapshot,
    ix: NodeIndex,
    breakdown: &ScoreBreakdown,
    unblocks_by_ix: &HashMap<NodeIndex, usize>,
) -> Vec<String> {
    let node = &snap.graph[ix];
    let mut reasons = Vec::new();
    if node.priority <= 1 {
        reasons.push(format!("P{} priority", node.priority));
    }
    if breakdown.unblocks_component > 0.0 {
        reasons.push(format!(
            "unblocks {} downstream",
            unblocks_for(unblocks_by_ix, ix)
        ));
    }
    if breakdown.actionable_component > 0.0 {
        reasons.push("actionable now".into());
    }
    if breakdown.freshness_component > 0.7 {
        reasons.push("recently updated".into());
    }
    if breakdown.age_penalty_component > 0.7 {
        reasons.push("aging - consider closing or reprioritizing".into());
    }
    reasons
}

fn blocking_targets(snap: &GraphSnapshot, ix: NodeIndex) -> Vec<String> {
    let mut targets: Vec<String> = snap
        .graph
        .edges(ix)
        .filter(|edge| edge.weight().kind.is_blocking())
        .map(|edge| snap.graph[edge.target()].id.clone())
        .collect();
    targets.sort();
    targets
}

fn blocking_sources(snap: &GraphSnapshot, ix: NodeIndex) -> Vec<String> {
    let mut sources: Vec<String> = snap
        .graph
        .edges_directed(ix, petgraph::Direction::Incoming)
        .filter(|edge| edge.weight().kind.is_blocking())
        .map(|edge| snap.graph[edge.source()].id.clone())
        .collect();
    sources.sort();
    sources
}

fn quick_win_threshold(scores: &HashMap<NodeIndex, ScoreBreakdown>, percentile: f64) -> f64 {
    if scores.is_empty() {
        return 0.0;
    }

    let mut values: Vec<f64> = scores
        .values()
        .map(|breakdown| breakdown.normalized)
        .collect();
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));

    let idx = (values.len() as f64 * percentile).floor() as usize;
    values[idx.min(values.len() - 1)]
}

fn compute_project_health(snap: &GraphSnapshot) -> ProjectHealth {
    let mut counts = HealthCounts::default();
    for ix in snap.graph.node_indices() {
        let node = &snap.graph[ix];
        counts.total += 1;
        if node.status == "closed" {
            counts.closed += 1;
            continue;
        }

        counts.open += 1;
        if is_actionable(snap, ix) {
            counts.actionable += 1;
        } else {
            counts.blocked += 1;
        }
    }

    let strongly_connected = tarjan_scc(&snap.graph);
    let self_loop_count = snap
        .graph
        .edge_indices()
        .filter_map(|edge| snap.graph.edge_endpoints(edge))
        .filter(|(source, target)| source == target)
        .count();
    let cycle_count = strongly_connected
        .iter()
        .filter(|component| component.len() > 1)
        .count()
        + self_loop_count;
    let node_count = counts.total;
    let edge_count = snap.graph.edge_count();
    let density = if node_count > 1 {
        edge_count as f64 / (node_count as f64 * (node_count as f64 - 1.0))
    } else {
        0.0
    };

    ProjectHealth {
        counts,
        graph: GraphHealth {
            node_count,
            edge_count,
            density,
            has_cycles: is_cyclic_directed(&snap.graph),
            cycle_count,
        },
        velocity: None,
        staleness: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::snapshot::{DependencyKind, NodeData};
    use chrono::{DateTime, Duration, Utc};

    fn fixed_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-06T00:00:00Z")
            .expect("fixed test time parses")
            .with_timezone(&Utc)
    }

    fn node(id: &str, status: &str, priority: i32) -> NodeData {
        let now = fixed_now();
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: status.into(),
            priority,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: now - Duration::days(10),
            updated_at: now,
            due_at: None,
            content_hash: format!("h-{id}"),
        }
    }

    fn snap_of(nodes: Vec<NodeData>, edges: Vec<(&str, &str, DependencyKind)>) -> GraphSnapshot {
        let mut snap = GraphSnapshot::new(None);
        for node in nodes {
            snap.add_node(node);
        }
        for (from, to, kind) in edges {
            assert!(snap.add_edge(from, to, kind));
        }
        snap.data_hash = snap.compute_data_hash();
        snap.generated_at = fixed_now();
        snap
    }

    fn cfg() -> ScoreConfig {
        ScoreConfig {
            now: fixed_now(),
            ..ScoreConfig::default()
        }
    }

    fn assert_approx_eq(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {actual} to be approximately {expected}"
        );
    }

    #[test]
    fn triage_orders_p0_above_p4() {
        let snap = snap_of(vec![node("h", "open", 0), node("l", "open", 4)], vec![]);

        let report = compute_triage(&snap, &cfg());

        assert_eq!(report.triage.quick_ref.top_picks[0].id, "h");
        assert_eq!(report.triage.quick_ref.top_picks[1].id, "l");
    }

    #[test]
    fn project_health_counts_actionable_and_blocked() {
        let snap = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "closed", 2),
            ],
            vec![("a", "b", DependencyKind::Blocks)],
        );

        let health = compute_triage(&snap, &cfg()).triage.project_health;

        assert_eq!(health.counts.total, 3);
        assert_eq!(health.counts.closed, 1);
        assert_eq!(health.counts.open, 2);
        assert_eq!(health.counts.actionable, 1);
        assert_eq!(health.counts.blocked, 1);
    }

    #[test]
    fn graph_health_detects_cycle() {
        let snap = snap_of(
            vec![node("a", "open", 2), node("b", "open", 2)],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "a", DependencyKind::Blocks),
            ],
        );

        let graph = compute_triage(&snap, &cfg()).triage.project_health.graph;

        assert!(graph.has_cycles);
        assert_eq!(graph.cycle_count, 1);
    }

    #[test]
    fn graph_health_uses_directed_density() {
        let snap = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "open", 2),
                node("d", "open", 2),
            ],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
                ("c", "d", DependencyKind::Blocks),
            ],
        );

        let graph = compute_triage(&snap, &cfg()).triage.project_health.graph;

        assert_eq!(graph.node_count, 4);
        assert_eq!(graph.edge_count, 3);
        assert_approx_eq(graph.density, 3.0 / (4.0 * 3.0));
    }

    #[test]
    fn graph_health_counts_self_loops_and_larger_cycles() {
        let snap = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "open", 2),
            ],
            vec![
                ("a", "a", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
                ("c", "b", DependencyKind::Blocks),
            ],
        );

        let graph = compute_triage(&snap, &cfg()).triage.project_health.graph;

        assert!(graph.has_cycles);
        assert!(graph.cycle_count >= 2);
    }

    #[test]
    fn blockers_to_clear_sorted_by_unblocks_count() {
        let snap = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "open", 2),
                node("d", "open", 2),
                node("e", "open", 2),
                node("f", "open", 2),
            ],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("a", "c", DependencyKind::Blocks),
                ("a", "d", DependencyKind::Blocks),
                ("e", "f", DependencyKind::Blocks),
            ],
        );

        let blockers = compute_triage(&snap, &cfg()).triage.blockers_to_clear;

        assert!(blockers.iter().any(|blocker| blocker.id == "a"));
        assert!(blockers[0].unblocks_count >= blockers.last().unwrap().unblocks_count);
    }
}

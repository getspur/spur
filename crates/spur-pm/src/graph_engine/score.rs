use crate::graph_engine::snapshot::GraphSnapshot;
use chrono::{DateTime, Utc};
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct ScoreConfig {
    pub w_priority: f64,
    pub w_unblocks: f64,
    pub w_actionable: f64,
    pub w_freshness: f64,
    pub w_age_penalty: f64,
    pub quick_win_percentile: f64,
    pub now: DateTime<Utc>,
}

impl Default for ScoreConfig {
    fn default() -> Self {
        Self {
            w_priority: 0.30,
            w_unblocks: 0.25,
            w_actionable: 0.20,
            w_freshness: 0.15,
            w_age_penalty: 0.10,
            quick_win_percentile: 0.75,
            now: Utc::now(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScoreBreakdown {
    pub priority_component: f64,
    pub unblocks_component: f64,
    pub actionable_component: f64,
    pub freshness_component: f64,
    pub age_penalty_component: f64,
    pub raw: f64,
    pub normalized: f64,
}

pub fn is_actionable(snap: &GraphSnapshot, ix: NodeIndex) -> bool {
    let node = &snap.graph[ix];
    if !matches!(node.status.as_str(), "open" | "in_progress") {
        return false;
    }

    for edge in snap.graph.edges_directed(ix, petgraph::Direction::Incoming) {
        if edge.weight().kind.is_blocking() && snap.graph[edge.source()].status != "closed" {
            return false;
        }
    }

    true
}

pub fn transitive_unblocks(snap: &GraphSnapshot, ix: NodeIndex) -> usize {
    let mut visited = HashSet::new();
    let mut stack = vec![ix];

    while let Some(cur) = stack.pop() {
        for edge in snap.graph.edges(cur) {
            if !edge.weight().kind.is_blocking() {
                continue;
            }

            let target = edge.target();
            if target == ix {
                continue;
            }
            if visited.insert(target) {
                stack.push(target);
            }
        }
    }

    visited.len()
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

pub fn score_node(snap: &GraphSnapshot, ix: NodeIndex, cfg: &ScoreConfig) -> ScoreBreakdown {
    let node = &snap.graph[ix];
    let priority_component = ((5 - node.priority).max(0) as f64) / 5.0;
    let unblocks = transitive_unblocks(snap, ix) as f64;
    let unblocks_component = (1.0 + unblocks).log10();
    let actionable_component = if is_actionable(snap, ix) { 1.0 } else { 0.0 };
    let days_since_update = (cfg.now - node.updated_at).num_days() as f64;
    let freshness_component = sigmoid((30.0 - days_since_update) / 30.0);
    let age_days = (cfg.now - node.created_at).num_days() as f64;
    let age_penalty_component = sigmoid((age_days - 60.0) / 30.0);

    let raw = cfg.w_priority * priority_component
        + cfg.w_unblocks * unblocks_component
        + cfg.w_actionable * actionable_component
        + cfg.w_freshness * freshness_component
        - cfg.w_age_penalty * age_penalty_component;

    ScoreBreakdown {
        priority_component,
        unblocks_component,
        actionable_component,
        freshness_component,
        age_penalty_component,
        raw,
        normalized: 0.0,
    }
}

pub fn score_all(snap: &GraphSnapshot, cfg: &ScoreConfig) -> HashMap<NodeIndex, ScoreBreakdown> {
    let mut out = HashMap::new();
    let mut max_raw = 0.0_f64;

    for ix in snap.graph.node_indices() {
        let score = score_node(snap, ix, cfg);
        if score.raw > max_raw {
            max_raw = score.raw;
        }
        out.insert(ix, score);
    }

    if max_raw > 0.0 {
        for score in out.values_mut() {
            score.normalized = (score.raw / max_raw).clamp(0.0, 1.0);
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::snapshot::{DependencyKind, GraphSnapshot, NodeData};
    use chrono::Duration;

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

    fn dated_node(
        id: &str,
        status: &str,
        priority: i32,
        created_days_ago: i64,
        updated_days_ago: i64,
    ) -> NodeData {
        let now = fixed_now();
        NodeData {
            created_at: now - Duration::days(created_days_ago),
            updated_at: now - Duration::days(updated_days_ago),
            ..node(id, status, priority)
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
        snap
    }

    fn cfg() -> ScoreConfig {
        ScoreConfig {
            now: fixed_now(),
            ..ScoreConfig::default()
        }
    }

    fn expected_sigmoid(x: f64) -> f64 {
        1.0 / (1.0 + (-x).exp())
    }

    fn assert_approx_eq(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {actual} to be approximately {expected}"
        );
    }

    #[test]
    fn open_with_no_blockers_is_actionable() {
        let snap = snap_of(vec![node("a", "open", 2)], vec![]);
        let ix = snap.by_id["a"];

        assert!(is_actionable(&snap, ix));
    }

    #[test]
    fn in_progress_with_no_blockers_is_actionable() {
        let snap = snap_of(vec![node("a", "in_progress", 2)], vec![]);
        let ix = snap.by_id["a"];

        assert!(is_actionable(&snap, ix));
    }

    #[test]
    fn closed_node_is_not_actionable() {
        let snap = snap_of(vec![node("a", "closed", 2)], vec![]);
        let ix = snap.by_id["a"];

        assert!(!is_actionable(&snap, ix));
    }

    #[test]
    fn open_with_open_blocker_is_not_actionable() {
        let snap = snap_of(
            vec![node("a", "open", 2), node("b", "open", 2)],
            vec![("a", "b", DependencyKind::Blocks)],
        );
        let b_ix = snap.by_id["b"];

        assert!(!is_actionable(&snap, b_ix));
    }

    #[test]
    fn closed_blocker_does_not_block() {
        let snap = snap_of(
            vec![node("a", "closed", 2), node("b", "open", 2)],
            vec![("a", "b", DependencyKind::Blocks)],
        );
        let b_ix = snap.by_id["b"];

        assert!(is_actionable(&snap, b_ix));
    }

    #[test]
    fn non_blocking_incoming_edge_does_not_block() {
        let snap = snap_of(
            vec![node("a", "open", 2), node("b", "open", 2)],
            vec![("a", "b", DependencyKind::RelatedTo)],
        );
        let b_ix = snap.by_id["b"];

        assert!(is_actionable(&snap, b_ix));
    }

    #[test]
    fn transitive_unblocks_counts_blocking_chain_downstream() {
        let snap = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "open", 2),
            ],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
            ],
        );
        let a_ix = snap.by_id["a"];

        assert_eq!(transitive_unblocks(&snap, a_ix), 2);
    }

    #[test]
    fn transitive_unblocks_ignores_non_blocking_edges_and_self_cycles() {
        let snap = snap_of(
            vec![
                node("a", "open", 2),
                node("b", "open", 2),
                node("c", "open", 2),
            ],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "a", DependencyKind::Blocks),
                ("a", "c", DependencyKind::RelatedTo),
            ],
        );
        let a_ix = snap.by_id["a"];

        assert_eq!(transitive_unblocks(&snap, a_ix), 1);
    }

    #[test]
    fn score_node_uses_expected_weighted_components() {
        let snap = snap_of(
            vec![
                dated_node("a", "open", 0, 90, 15),
                dated_node("b", "open", 2, 10, 0),
            ],
            vec![("a", "b", DependencyKind::Blocks)],
        );
        let cfg = cfg();
        let a_ix = snap.by_id["a"];

        let score = score_node(&snap, a_ix, &cfg);

        let priority = 1.0;
        let unblocks = 2.0_f64.log10();
        let actionable = 1.0;
        let freshness = expected_sigmoid(0.5);
        let age_penalty = expected_sigmoid(1.0);
        let raw = cfg.w_priority * priority
            + cfg.w_unblocks * unblocks
            + cfg.w_actionable * actionable
            + cfg.w_freshness * freshness
            - cfg.w_age_penalty * age_penalty;

        assert_approx_eq(score.priority_component, priority);
        assert_approx_eq(score.unblocks_component, unblocks);
        assert_approx_eq(score.actionable_component, actionable);
        assert_approx_eq(score.freshness_component, freshness);
        assert_approx_eq(score.age_penalty_component, age_penalty);
        assert_approx_eq(score.raw, raw);
        assert_eq!(score.normalized, 0.0);
    }

    #[test]
    fn p0_scores_higher_than_p4() {
        let snap = snap_of(vec![node("h", "open", 0), node("l", "open", 4)], vec![]);
        let scores = score_all(&snap, &cfg());

        assert!(scores[&snap.by_id["h"]].raw > scores[&snap.by_id["l"]].raw);
    }

    #[test]
    fn score_all_normalizes_all_values_into_unit_interval() {
        let snap = snap_of(
            vec![
                node("a", "open", 0),
                node("b", "open", 4),
                dated_node("c", "closed", 4, 365, 365),
            ],
            vec![("a", "b", DependencyKind::Blocks)],
        );
        let scores = score_all(&snap, &cfg());

        assert_eq!(scores.len(), 3);
        assert!(scores
            .values()
            .all(|score| (0.0..=1.0).contains(&score.normalized)));
        assert!(scores
            .values()
            .any(|score| (score.normalized - 1.0).abs() < 1e-9));
    }
}

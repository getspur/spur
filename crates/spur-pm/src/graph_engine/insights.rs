use crate::graph::{GraphInsights, InsightItem, WhatIfDelta, WhatIfEntry};
use crate::graph_engine::metrics::{betweenness_centrality_brandes, hits, k_core_decomposition};
use crate::graph_engine::score::transitive_unblocks;
use crate::graph_engine::snapshot::GraphSnapshot;
use petgraph::algo::tarjan_scc;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct InsightConfig {
    pub top_k: usize,
    pub pagerank_damping: f64,
    pub pagerank_iterations: usize,
}

impl Default for InsightConfig {
    fn default() -> Self {
        Self {
            top_k: 10,
            pagerank_damping: 0.85,
            pagerank_iterations: 100,
        }
    }
}

pub fn compute_insights(snap: &GraphSnapshot, cfg: &InsightConfig) -> GraphInsights {
    let mut cycles: Vec<Vec<String>> = tarjan_scc(&snap.graph)
        .into_iter()
        .filter_map(|component| {
            let ix = component[0];
            let has_self_loop =
                component.len() == 1 && snap.graph.edges(ix).any(|edge| edge.target() == ix);
            if component.len() <= 1 && !has_self_loop {
                return None;
            }

            let mut ids: Vec<String> = component
                .into_iter()
                .map(|ix| snap.graph[ix].id.clone())
                .collect();
            ids.sort();
            Some(ids)
        })
        .collect();
    cycles.sort_by(|a, b| {
        a.first()
            .cmp(&b.first())
            .then_with(|| a.len().cmp(&b.len()))
            .then_with(|| a.cmp(b))
    });

    let articulation = articulation_points(snap);
    let articulation_ids: HashSet<String> = articulation
        .iter()
        .map(|&ix| snap.graph[ix].id.clone())
        .collect();
    let mut articulation: Vec<String> = articulation
        .into_iter()
        .map(|ix| snap.graph[ix].id.clone())
        .collect();
    articulation.sort();

    let pagerank = pagerank_iterative(snap, cfg.pagerank_damping, cfg.pagerank_iterations);
    let influencers = top_k_named(snap, &pagerank, cfg.top_k);

    let (hubs_by_ix, authorities_by_ix) = hits(snap);
    let hubs = top_k_named(snap, &hubs_by_ix, cfg.top_k);
    let authorities = top_k_named(snap, &authorities_by_ix, cfg.top_k);

    let betweenness = betweenness_centrality_brandes(snap);
    let bottlenecks = top_k_named(snap, &betweenness, cfg.top_k);

    let core_scores: HashMap<NodeIndex, f64> = k_core_decomposition(snap)
        .into_iter()
        .map(|(ix, core)| (ix, core as f64))
        .collect();
    let cores = top_k_named(snap, &core_scores, cfg.top_k);

    let keystone_scores: HashMap<NodeIndex, f64> = snap
        .graph
        .node_indices()
        .filter(|&ix| articulation_ids.contains(&snap.graph[ix].id))
        .map(|ix| (ix, snap.graph.edges(ix).count() as f64))
        .collect();
    let keystones = top_k_named(snap, &keystone_scores, cfg.top_k);

    let mut orphans: Vec<String> = snap
        .graph
        .node_indices()
        .filter(|&ix| {
            snap.graph.edges(ix).count() == 0
                && snap
                    .graph
                    .edges_directed(ix, petgraph::Direction::Incoming)
                    .count()
                    == 0
        })
        .map(|ix| snap.graph[ix].id.clone())
        .collect();
    orphans.sort();

    let node_count = snap.graph.node_count();
    let edge_count = snap.graph.edge_count();
    let cluster_density = if node_count > 1 {
        (2.0 * edge_count as f64) / (node_count as f64 * (node_count as f64 - 1.0))
    } else {
        0.0
    };

    GraphInsights {
        generated_at: Some(snap.generated_at.to_rfc3339()),
        data_hash: Some(snap.data_hash.clone()),
        bottlenecks,
        keystones,
        influencers,
        hubs,
        authorities,
        cores,
        articulation,
        orphans,
        cycles,
        cluster_density,
        top_what_ifs: compute_top_what_ifs(snap, cfg),
        raw: serde_json::Value::Null,
    }
}

fn top_k_named(
    snap: &GraphSnapshot,
    scores: &HashMap<NodeIndex, f64>,
    top_k: usize,
) -> Vec<InsightItem> {
    let mut ranked: Vec<(NodeIndex, f64)> =
        scores.iter().map(|(&ix, &score)| (ix, score)).collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| snap.graph[a.0].id.cmp(&snap.graph[b.0].id))
    });

    ranked
        .into_iter()
        .take(top_k)
        .filter(|(_, score)| *score > 0.0)
        .map(|(ix, score)| InsightItem {
            id: snap.graph[ix].id.clone(),
            value: score,
        })
        .collect()
}

fn pagerank_iterative(
    snap: &GraphSnapshot,
    damping: f64,
    iterations: usize,
) -> HashMap<NodeIndex, f64> {
    let node_count = snap.graph.node_count();
    if node_count == 0 {
        return HashMap::new();
    }

    let n = node_count as f64;
    let mut ranks: HashMap<NodeIndex, f64> =
        snap.graph.node_indices().map(|ix| (ix, 1.0 / n)).collect();
    let teleport = (1.0 - damping) / n;

    for _ in 0..iterations {
        let dangling_rank: f64 = snap
            .graph
            .node_indices()
            .filter(|&ix| snap.graph.edges(ix).count() == 0)
            .map(|ix| ranks[&ix])
            .sum();
        let dangling_share = damping * dangling_rank / n;

        let mut next: HashMap<NodeIndex, f64> = snap
            .graph
            .node_indices()
            .map(|ix| (ix, teleport + dangling_share))
            .collect();

        for source in snap.graph.node_indices() {
            let out_degree = snap.graph.edges(source).count();
            if out_degree == 0 {
                continue;
            }

            let share = damping * ranks[&source] / out_degree as f64;
            for edge in snap.graph.edges(source) {
                *next
                    .get_mut(&edge.target())
                    .expect("target rank was initialized") += share;
            }
        }

        ranks = next;
    }

    ranks
}

fn articulation_points(snap: &GraphSnapshot) -> Vec<NodeIndex> {
    let mut adjacency: HashMap<NodeIndex, HashSet<NodeIndex>> = snap
        .graph
        .node_indices()
        .map(|ix| (ix, HashSet::new()))
        .collect();

    for edge in snap.graph.edge_indices() {
        let Some((source, target)) = snap.graph.edge_endpoints(edge) else {
            continue;
        };
        if source == target {
            continue;
        }
        adjacency
            .get_mut(&source)
            .expect("source node was initialized")
            .insert(target);
        adjacency
            .get_mut(&target)
            .expect("target node was initialized")
            .insert(source);
    }

    let mut state = ArticulationState::default();
    let mut nodes: Vec<NodeIndex> = snap.graph.node_indices().collect();
    nodes.sort_by(|a, b| snap.graph[*a].id.cmp(&snap.graph[*b].id));
    for ix in nodes {
        if !state.visited.contains(&ix) {
            articulation_dfs(ix, snap, &adjacency, &mut state);
        }
    }

    let mut points: Vec<NodeIndex> = state.points.into_iter().collect();
    points.sort_by(|a, b| snap.graph[*a].id.cmp(&snap.graph[*b].id));
    points
}

#[derive(Default)]
struct ArticulationState {
    visited: HashSet<NodeIndex>,
    discovery: HashMap<NodeIndex, usize>,
    low: HashMap<NodeIndex, usize>,
    parent: HashMap<NodeIndex, NodeIndex>,
    points: HashSet<NodeIndex>,
    time: usize,
}

fn articulation_dfs(
    ix: NodeIndex,
    snap: &GraphSnapshot,
    adjacency: &HashMap<NodeIndex, HashSet<NodeIndex>>,
    state: &mut ArticulationState,
) {
    state.time += 1;
    state.visited.insert(ix);
    state.discovery.insert(ix, state.time);
    state.low.insert(ix, state.time);

    let mut child_count = 0;
    let mut neighbors: Vec<NodeIndex> = adjacency
        .get(&ix)
        .map(|set| set.iter().copied().collect())
        .unwrap_or_default();
    neighbors.sort_by(|a, b| snap.graph[*a].id.cmp(&snap.graph[*b].id));

    for neighbor in neighbors {
        if !state.visited.contains(&neighbor) {
            child_count += 1;
            state.parent.insert(neighbor, ix);
            articulation_dfs(neighbor, snap, adjacency, state);

            let neighbor_low = state.low[&neighbor];
            let current_low = state.low[&ix].min(neighbor_low);
            state.low.insert(ix, current_low);

            if state.parent.contains_key(&ix) && neighbor_low >= state.discovery[&ix] {
                state.points.insert(ix);
            }
        } else if state.parent.get(&ix).copied() != Some(neighbor) {
            let current_low = state.low[&ix].min(state.discovery[&neighbor]);
            state.low.insert(ix, current_low);
        }
    }

    if !state.parent.contains_key(&ix) && child_count > 1 {
        state.points.insert(ix);
    }
}

fn compute_top_what_ifs(snap: &GraphSnapshot, cfg: &InsightConfig) -> Vec<WhatIfEntry> {
    if cfg.top_k == 0 {
        return Vec::new();
    }

    let Some((ix, transitive)) = highest_impact_node(snap) else {
        return Vec::new();
    };

    let direct = direct_unblocks(snap, ix);
    let blocked_reduction = blocked_reduction_after_closing(snap, ix);
    let depth_reduction = depth_reduction_after_closing(snap, ix);
    let node = &snap.graph[ix];

    vec![WhatIfEntry {
        issue_id: node.id.clone(),
        title: Some(node.title.clone()),
        delta: Some(WhatIfDelta {
            direct_unblocks: direct,
            transitive_unblocks: transitive,
            blocked_reduction,
            depth_reduction,
            estimated_days_saved: None,
            explanation: Some(format!(
                "Closing {} would directly unblock {} issue(s) and reduce blocked work by {} issue(s)",
                node.id, direct, blocked_reduction
            )),
        }),
    }]
}

fn highest_impact_node(snap: &GraphSnapshot) -> Option<(NodeIndex, usize)> {
    // TODO: memoize transitive_unblocks across nodes if N grows; currently O(N·(N+E)).
    snap.graph
        .node_indices()
        .filter(|&ix| snap.graph[ix].status != "closed")
        .map(|ix| (ix, transitive_unblocks(snap, ix)))
        .filter(|(_, unblocks)| *unblocks > 0)
        .max_by(|(left_ix, left_unblocks), (right_ix, right_unblocks)| {
            left_unblocks
                .cmp(right_unblocks)
                .then_with(|| snap.graph[*right_ix].id.cmp(&snap.graph[*left_ix].id))
        })
}

fn direct_unblocks(snap: &GraphSnapshot, ix: NodeIndex) -> usize {
    snap.graph
        .edges(ix)
        .filter(|edge| edge.weight().kind.is_blocking())
        .filter(|edge| snap.graph[edge.target()].status != "closed")
        .count()
}

fn blocked_reduction_after_closing(snap: &GraphSnapshot, closing: NodeIndex) -> usize {
    snap.graph
        .node_indices()
        .filter(|&ix| is_blocked_after_closing(snap, ix, None))
        .filter(|&ix| !is_blocked_after_closing(snap, ix, Some(closing)))
        .count()
}

fn is_blocked_after_closing(
    snap: &GraphSnapshot,
    ix: NodeIndex,
    closing: Option<NodeIndex>,
) -> bool {
    if snap.graph[ix].status == "closed" || closing == Some(ix) {
        return false;
    }

    snap.graph
        .edges_directed(ix, petgraph::Direction::Incoming)
        .filter(|edge| edge.weight().kind.is_blocking())
        .any(|edge| closing != Some(edge.source()) && snap.graph[edge.source()].status != "closed")
}

fn depth_reduction_after_closing(snap: &GraphSnapshot, closing: NodeIndex) -> f64 {
    let before = dependency_depth_sum(snap, None);
    let after = dependency_depth_sum(snap, Some(closing));
    before.saturating_sub(after) as f64
}

fn dependency_depth_sum(snap: &GraphSnapshot, closing: Option<NodeIndex>) -> u32 {
    let open: HashSet<NodeIndex> = snap
        .graph
        .node_indices()
        .filter(|&ix| snap.graph[ix].status != "closed" && closing != Some(ix))
        .collect();
    let mut memo = HashMap::new();
    let mut nodes: Vec<NodeIndex> = open.iter().copied().collect();
    nodes.sort_by(|a, b| snap.graph[*a].id.cmp(&snap.graph[*b].id));

    nodes
        .into_iter()
        .map(|ix| {
            let mut visiting = HashSet::new();
            dependency_depth_for(snap, ix, &open, &mut memo, &mut visiting)
        })
        .sum()
}

fn dependency_depth_for(
    snap: &GraphSnapshot,
    ix: NodeIndex,
    open: &HashSet<NodeIndex>,
    memo: &mut HashMap<NodeIndex, u32>,
    visiting: &mut HashSet<NodeIndex>,
) -> u32 {
    if let Some(&depth) = memo.get(&ix) {
        return depth;
    }
    if !visiting.insert(ix) {
        return 0;
    }

    let mut max_parent_depth = 0;
    for edge in snap.graph.edges_directed(ix, petgraph::Direction::Incoming) {
        if !edge.weight().kind.is_blocking() {
            continue;
        }

        let source = edge.source();
        if !open.contains(&source) || visiting.contains(&source) {
            continue;
        }

        let depth = 1 + dependency_depth_for(snap, source, open, memo, visiting);
        max_parent_depth = max_parent_depth.max(depth);
    }

    visiting.remove(&ix);
    memo.insert(ix, max_parent_depth);
    max_parent_depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_engine::snapshot::{DependencyKind, NodeData};
    use chrono::Utc;

    fn n(id: &str) -> NodeData {
        NodeData {
            id: id.into(),
            title: format!("T{id}"),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: "h".into(),
        }
    }

    fn snap(nodes: Vec<NodeData>, edges: Vec<(&str, &str, DependencyKind)>) -> GraphSnapshot {
        let mut snap = GraphSnapshot::new(None);
        for node in nodes {
            snap.add_node(node);
        }
        for (from, to, kind) in edges {
            assert!(snap.add_edge(from, to, kind));
        }
        snap.data_hash = snap.compute_data_hash();
        snap
    }

    #[test]
    fn isolated_node_appears_in_orphans() {
        let s = snap(
            vec![n("a"), n("b"), n("orphan")],
            vec![("a", "b", DependencyKind::Blocks)],
        );
        let cfg = InsightConfig::default();

        let i = compute_insights(&s, &cfg);

        assert!(i.orphans.contains(&"orphan".to_string()));
        assert!(!i.orphans.contains(&"a".to_string()));
    }

    #[test]
    fn cycle_appears_in_cycles() {
        let s = snap(
            vec![n("a"), n("b")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "a", DependencyKind::Blocks),
            ],
        );
        let cfg = InsightConfig::default();

        let i = compute_insights(&s, &cfg);

        assert_eq!(i.cycles.len(), 1);
        assert_eq!(i.cycles[0], vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn compute_insights_lists_self_loops_as_cycles() {
        let s = snap(
            vec![n("a"), n("b"), n("c")],
            vec![
                ("a", "a", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
                ("c", "b", DependencyKind::Blocks),
            ],
        );
        let cfg = InsightConfig::default();

        let i = compute_insights(&s, &cfg);

        assert_eq!(i.cycles.len(), 2);
        assert!(i.cycles.contains(&vec!["a".to_string()]));
        assert!(i.cycles.contains(&vec!["b".to_string(), "c".to_string()]));
    }

    #[test]
    fn top_what_ifs_is_empty_when_no_node_has_impact() {
        let s = snap(vec![n("a"), n("b")], vec![]);
        let cfg = InsightConfig::default();

        let i = compute_insights(&s, &cfg);

        assert!(i.top_what_ifs.is_empty());
    }

    #[test]
    fn cluster_density_for_dense_triangle() {
        let s = snap(
            vec![n("a"), n("b"), n("c")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
                ("a", "c", DependencyKind::Blocks),
            ],
        );
        let cfg = InsightConfig::default();

        let i = compute_insights(&s, &cfg);

        assert!((i.cluster_density - 1.0).abs() < 1e-9);
    }

    #[test]
    fn top_what_if_describes_closing_highest_impact_node() {
        let s = snap(
            vec![n("a"), n("b"), n("c"), n("d")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("a", "c", DependencyKind::Blocks),
                ("b", "d", DependencyKind::Blocks),
            ],
        );
        let cfg = InsightConfig::default();

        let i = compute_insights(&s, &cfg);

        assert_eq!(i.top_what_ifs.len(), 1);
        assert_eq!(i.top_what_ifs[0].issue_id, "a");
        assert_eq!(i.top_what_ifs[0].title.as_deref(), Some("Ta"));
        let delta = i.top_what_ifs[0].delta.as_ref().expect("delta is present");
        assert_eq!(delta.direct_unblocks, 2);
        assert_eq!(delta.transitive_unblocks, 3);
        assert_eq!(delta.blocked_reduction, 2);
        assert!(delta.depth_reduction > 0.0);
    }
}

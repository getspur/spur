use crate::graph_engine::snapshot::GraphSnapshot;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::HashMap;

/// HITS algorithm - returns (hubs, authorities) maps keyed by NodeIndex.
/// Iterative, normalized at each step. 50 iterations is sufficient for our scale.
pub fn hits(snap: &GraphSnapshot) -> (HashMap<NodeIndex, f64>, HashMap<NodeIndex, f64>) {
    let mut hubs: HashMap<NodeIndex, f64> = snap.graph.node_indices().map(|ix| (ix, 1.0)).collect();
    let mut authorities = hubs.clone();

    for _ in 0..50 {
        let mut next_authorities = HashMap::new();
        for ix in snap.graph.node_indices() {
            let score = snap
                .graph
                .edges_directed(ix, petgraph::Direction::Incoming)
                .map(|edge| hubs[&edge.source()])
                .sum();
            next_authorities.insert(ix, score);
        }

        let mut next_hubs = HashMap::new();
        for ix in snap.graph.node_indices() {
            let score = snap
                .graph
                .edges(ix)
                .map(|edge| next_authorities[&edge.target()])
                .sum();
            next_hubs.insert(ix, score);
        }

        normalize(&mut next_authorities);
        normalize(&mut next_hubs);
        hubs = next_hubs;
        authorities = next_authorities;
    }

    (hubs, authorities)
}

fn normalize(scores: &mut HashMap<NodeIndex, f64>) {
    let norm = scores
        .values()
        .map(|score| score * score)
        .sum::<f64>()
        .sqrt();
    if norm > 0.0 {
        for score in scores.values_mut() {
            *score /= norm;
        }
    }
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
        snap
    }

    #[test]
    fn hub_with_many_outedges_scores_highest() {
        let s = snap(
            vec![n("a"), n("b"), n("c"), n("d"), n("e")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("a", "c", DependencyKind::Blocks),
                ("a", "d", DependencyKind::Blocks),
            ],
        );

        let (hubs, _) = hits(&s);
        let a_hub = hubs[&s.by_id["a"]];

        assert!(a_hub > hubs[&s.by_id["b"]]);
        assert!(a_hub > hubs[&s.by_id["e"]]);
    }

    #[test]
    fn authority_with_many_inedges_scores_highest() {
        let s = snap(
            vec![n("a"), n("b"), n("c"), n("d"), n("e")],
            vec![
                ("a", "d", DependencyKind::Blocks),
                ("b", "d", DependencyKind::Blocks),
                ("c", "d", DependencyKind::Blocks),
            ],
        );

        let (_, authorities) = hits(&s);
        let d_authority = authorities[&s.by_id["d"]];

        assert!(d_authority > authorities[&s.by_id["a"]]);
        assert!(d_authority > authorities[&s.by_id["e"]]);
    }
}

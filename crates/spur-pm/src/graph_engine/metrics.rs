use crate::graph_engine::snapshot::GraphSnapshot;
use petgraph::graph::NodeIndex;
use petgraph::visit::EdgeRef;
use std::collections::{BTreeSet, HashMap, VecDeque};

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

/// Brandes' algorithm for betweenness centrality on a directed graph.
/// Time complexity: O(V * (V + E)).
pub fn betweenness_centrality_brandes(snap: &GraphSnapshot) -> HashMap<NodeIndex, f64> {
    let mut centrality: HashMap<NodeIndex, f64> =
        snap.graph.node_indices().map(|ix| (ix, 0.0)).collect();

    for source in snap.graph.node_indices() {
        let mut stack = Vec::new();
        let mut predecessors: HashMap<NodeIndex, Vec<NodeIndex>> = snap
            .graph
            .node_indices()
            .map(|ix| (ix, Vec::new()))
            .collect();
        let mut shortest_path_counts: HashMap<NodeIndex, f64> =
            snap.graph.node_indices().map(|ix| (ix, 0.0)).collect();
        let mut distances: HashMap<NodeIndex, i64> =
            snap.graph.node_indices().map(|ix| (ix, -1)).collect();

        shortest_path_counts.insert(source, 1.0);
        distances.insert(source, 0);

        let mut queue = VecDeque::new();
        queue.push_back(source);

        while let Some(node) = queue.pop_front() {
            stack.push(node);
            for edge in snap.graph.edges(node) {
                let target = edge.target();
                if distances[&target] < 0 {
                    queue.push_back(target);
                    distances.insert(target, distances[&node] + 1);
                }
                if distances[&target] == distances[&node] + 1 {
                    let path_count = shortest_path_counts[&target] + shortest_path_counts[&node];
                    shortest_path_counts.insert(target, path_count);
                    predecessors
                        .get_mut(&target)
                        .expect("node was initialized")
                        .push(node);
                }
            }
        }

        let mut dependencies: HashMap<NodeIndex, f64> =
            snap.graph.node_indices().map(|ix| (ix, 0.0)).collect();
        while let Some(node) = stack.pop() {
            for predecessor in &predecessors[&node] {
                let contribution = (shortest_path_counts[predecessor]
                    / shortest_path_counts[&node])
                    * (1.0 + dependencies[&node]);
                dependencies.insert(*predecessor, dependencies[predecessor] + contribution);
            }
            if node != source {
                centrality.insert(node, centrality[&node] + dependencies[&node]);
            }
        }
    }

    centrality
}

/// k-core decomposition (treats the graph as undirected for core ranking).
/// Returns each node's coreness (max k for which it remains in the k-core).
pub fn k_core_decomposition(snap: &GraphSnapshot) -> HashMap<NodeIndex, usize> {
    let mut degrees: HashMap<NodeIndex, usize> = snap
        .graph
        .node_indices()
        .map(|ix| (ix, undirected_neighbors(snap, ix).len()))
        .collect();
    let mut remaining: BTreeSet<(usize, NodeIndex)> =
        degrees.iter().map(|(&ix, &degree)| (degree, ix)).collect();
    let mut core = HashMap::new();

    while let Some(&(degree, ix)) = remaining.iter().next() {
        remaining.remove(&(degree, ix));
        core.insert(ix, degree);

        for neighbor in undirected_neighbors(snap, ix) {
            if core.contains_key(&neighbor) {
                continue;
            }
            let old_degree = degrees[&neighbor];
            if old_degree > degree {
                remaining.remove(&(old_degree, neighbor));
                let new_degree = old_degree - 1;
                degrees.insert(neighbor, new_degree);
                remaining.insert((new_degree, neighbor));
            }
        }
    }

    core
}

fn undirected_neighbors(snap: &GraphSnapshot, ix: NodeIndex) -> BTreeSet<NodeIndex> {
    snap.graph
        .edges(ix)
        .map(|edge| edge.target())
        .chain(
            snap.graph
                .edges_directed(ix, petgraph::Direction::Incoming)
                .map(|edge| edge.source()),
        )
        .filter(|&neighbor| neighbor != ix)
        .collect()
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

    #[test]
    fn chain_middle_nodes_have_highest_betweenness() {
        let s = snap(
            vec![n("a"), n("b"), n("c"), n("d")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
                ("c", "d", DependencyKind::Blocks),
            ],
        );

        let centrality = betweenness_centrality_brandes(&s);

        assert_eq!(centrality[&s.by_id["a"]], 0.0);
        assert_eq!(centrality[&s.by_id["b"]], 2.0);
        assert_eq!(centrality[&s.by_id["c"]], 2.0);
        assert_eq!(centrality[&s.by_id["d"]], 0.0);
    }

    #[test]
    fn bidirectional_star_center_has_highest_betweenness() {
        let s = snap(
            vec![n("center"), n("a"), n("b"), n("c"), n("d")],
            vec![
                ("center", "a", DependencyKind::Blocks),
                ("a", "center", DependencyKind::Blocks),
                ("center", "b", DependencyKind::Blocks),
                ("b", "center", DependencyKind::Blocks),
                ("center", "c", DependencyKind::Blocks),
                ("c", "center", DependencyKind::Blocks),
                ("center", "d", DependencyKind::Blocks),
                ("d", "center", DependencyKind::Blocks),
            ],
        );

        let centrality = betweenness_centrality_brandes(&s);

        assert_eq!(centrality[&s.by_id["center"]], 12.0);
        assert_eq!(centrality[&s.by_id["a"]], 0.0);
        assert_eq!(centrality[&s.by_id["b"]], 0.0);
        assert_eq!(centrality[&s.by_id["c"]], 0.0);
        assert_eq!(centrality[&s.by_id["d"]], 0.0);
    }
}

#[cfg(test)]
mod kcore_tests {
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
    fn isolated_node_has_core_zero() {
        let s = snap(vec![n("a"), n("b")], vec![]);
        let c = k_core_decomposition(&s);
        assert_eq!(c[&s.by_id["a"]], 0);
        assert_eq!(c[&s.by_id["b"]], 0);
    }

    #[test]
    fn triangle_yields_core_two() {
        let s = snap(
            vec![n("a"), n("b"), n("c")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
                ("c", "a", DependencyKind::Blocks),
            ],
        );
        let c = k_core_decomposition(&s);
        assert_eq!(c[&s.by_id["a"]], 2);
        assert_eq!(c[&s.by_id["b"]], 2);
        assert_eq!(c[&s.by_id["c"]], 2);
    }

    #[test]
    fn pendant_on_triangle_yields_mixed_cores() {
        // Triangle a-b-c with pendant d attached to a.
        // Undirected degrees: a=3, b=2, c=2, d=1.
        // d gets popped first (degree 1), then a, b, c remain a 2-core.
        let s = snap(
            vec![n("a"), n("b"), n("c"), n("d")],
            vec![
                ("a", "b", DependencyKind::Blocks),
                ("b", "c", DependencyKind::Blocks),
                ("c", "a", DependencyKind::Blocks),
                ("a", "d", DependencyKind::Blocks),
            ],
        );

        let core = k_core_decomposition(&s);

        assert_eq!(core[&s.by_id["d"]], 1);
        assert_eq!(core[&s.by_id["a"]], 2);
        assert_eq!(core[&s.by_id["b"]], 2);
        assert_eq!(core[&s.by_id["c"]], 2);
    }
}

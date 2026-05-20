use std::collections::{HashSet, VecDeque};

use crate::{GraphEdgeArtifact, GraphIndexArtifact, GraphSymbolArtifact, RelationKind};

const MAX_SUBGRAPH_RADIUS: u8 = 5;

#[derive(Debug, Clone, PartialEq)]
pub struct SubgraphView<'a> {
    pub nodes: Vec<&'a GraphSymbolArtifact>,
    pub edges: Vec<&'a GraphEdgeArtifact>,
}

pub fn find_symbol<'a>(
    artifact: &'a GraphIndexArtifact,
    symbol_id: &str,
) -> Option<&'a GraphSymbolArtifact> {
    artifact
        .symbols
        .iter()
        .find(|symbol| symbol.stable_symbol_id == symbol_id)
}

pub fn find_callers<'a>(
    artifact: &'a GraphIndexArtifact,
    symbol_id: &str,
) -> Vec<&'a GraphSymbolArtifact> {
    if find_symbol(artifact, symbol_id).is_none() {
        return Vec::new();
    }

    artifact
        .edges
        .iter()
        .filter(|edge| is_call_relation(edge.relation))
        .filter(|edge| edge.target_stable_symbol_id.as_deref() == Some(symbol_id))
        .filter_map(|edge| find_symbol(artifact, &edge.source_stable_symbol_id))
        .collect()
}

pub fn find_callees<'a>(
    artifact: &'a GraphIndexArtifact,
    symbol_id: &str,
) -> Vec<&'a GraphSymbolArtifact> {
    if find_symbol(artifact, symbol_id).is_none() {
        return Vec::new();
    }

    artifact
        .edges
        .iter()
        .filter(|edge| is_call_relation(edge.relation))
        .filter(|edge| edge.source_stable_symbol_id == symbol_id)
        .filter_map(|edge| {
            edge.target_stable_symbol_id
                .as_deref()
                .and_then(|target_id| find_symbol(artifact, target_id))
        })
        .collect()
}

pub fn bounded_subgraph<'a>(
    artifact: &'a GraphIndexArtifact,
    root_id: &str,
    radius: u8,
    edge_kinds: Option<&[RelationKind]>,
) -> SubgraphView<'a> {
    let Some(root) = find_symbol(artifact, root_id) else {
        return SubgraphView {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
    };

    let radius = radius.min(MAX_SUBGRAPH_RADIUS);
    let mut nodes = vec![root];
    let mut edges = Vec::new();
    let mut visited_nodes = HashSet::new();
    let mut visited_edges = HashSet::new();
    let mut queue = VecDeque::new();

    visited_nodes.insert(root.stable_symbol_id.as_str());
    queue.push_back((root.stable_symbol_id.as_str(), 0));

    while let Some((current_id, depth)) = queue.pop_front() {
        if depth >= radius {
            continue;
        }

        for (edge_index, edge) in artifact.edges.iter().enumerate() {
            if !edge_matches_filter(edge, edge_kinds) {
                continue;
            }

            let Some(neighbor_id) = incident_neighbor_id(edge, current_id) else {
                continue;
            };

            let neighbor = if visited_nodes.contains(neighbor_id) {
                None
            } else {
                let Some(symbol) = find_symbol(artifact, neighbor_id) else {
                    continue;
                };
                Some(symbol)
            };

            if visited_edges.insert(edge_index) {
                edges.push(edge);
            }

            if let Some(symbol) = neighbor {
                visited_nodes.insert(symbol.stable_symbol_id.as_str());
                nodes.push(symbol);
                queue.push_back((symbol.stable_symbol_id.as_str(), depth + 1));
            }
        }
    }

    SubgraphView { nodes, edges }
}

fn is_call_relation(relation: RelationKind) -> bool {
    matches!(relation, RelationKind::Calls)
}

fn edge_matches_filter(edge: &GraphEdgeArtifact, edge_kinds: Option<&[RelationKind]>) -> bool {
    edge_kinds.is_none_or(|kinds| kinds.contains(&edge.relation))
}

fn incident_neighbor_id<'a>(edge: &'a GraphEdgeArtifact, current_id: &str) -> Option<&'a str> {
    let target_id = edge.target_stable_symbol_id.as_deref()?;
    if edge.source_stable_symbol_id == current_id {
        Some(target_id)
    } else if target_id == current_id {
        Some(edge.source_stable_symbol_id.as_str())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Confidence, GraphIndexHeader};

    fn symbol(id: &str) -> GraphSymbolArtifact {
        GraphSymbolArtifact {
            stable_symbol_id: id.to_string(),
            file_path: format!("src/{id}.rs"),
            byte_range: [0, 8],
            line_range: [1, 1],
            entity_name: id.to_string(),
            symbol_kind: "function".to_string(),
            anchor_hash: format!("hash-{id}"),
            enclosing_scope: None,
        }
    }

    fn edge(source: &str, target: &str, relation: RelationKind) -> GraphEdgeArtifact {
        GraphEdgeArtifact {
            source_stable_symbol_id: source.to_string(),
            target_stable_symbol_id: Some(target.to_string()),
            target_label: None,
            relation,
            confidence: Confidence::SyntaxExact,
            confidence_score: 1.0,
            change_kind: None,
        }
    }

    fn artifact() -> GraphIndexArtifact {
        GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: "test".to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            symbols: ["root", "caller_a", "caller_b", "callee_a", "isolated"]
                .into_iter()
                .map(symbol)
                .collect(),
            edges: vec![
                edge("caller_a", "root", RelationKind::Calls),
                edge("caller_b", "root", RelationKind::Calls),
                edge("root", "caller_a", RelationKind::Calls),
                edge("root", "caller_b", RelationKind::Calls),
                edge("root", "callee_a", RelationKind::Calls),
                edge("callee_a", "caller_b", RelationKind::Calls),
                edge("root", "caller_a", RelationKind::References),
            ],
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
            commits: Vec::new(),
            symbol_snapshots: Vec::new(),
            temporal_edges: Vec::new(),
        }
    }

    fn ids<'a>(symbols: &'a [&'a GraphSymbolArtifact]) -> Vec<&'a str> {
        symbols
            .iter()
            .map(|symbol| symbol.stable_symbol_id.as_str())
            .collect()
    }

    #[test]
    fn find_symbol_returns_match_and_none_for_unknown_id() {
        let artifact = artifact();

        assert_eq!(
            find_symbol(&artifact, "root").map(|symbol| symbol.entity_name.as_str()),
            Some("root")
        );
        assert!(find_symbol(&artifact, "missing").is_none());
    }

    #[test]
    fn callers_and_callees_ignore_non_call_edges_and_unknown_targets() {
        let artifact = artifact();

        assert_eq!(
            ids(&find_callers(&artifact, "root")),
            vec!["caller_a", "caller_b"]
        );
        assert_eq!(
            ids(&find_callees(&artifact, "root")),
            vec!["caller_a", "caller_b", "callee_a"]
        );
        assert!(find_callers(&artifact, "missing").is_empty());
        assert!(find_callees(&artifact, "missing").is_empty());
    }

    #[test]
    fn symbol_with_no_edges_returns_empty_callers_and_callees() {
        let artifact = artifact();

        assert!(find_callers(&artifact, "isolated").is_empty());
        assert!(find_callees(&artifact, "isolated").is_empty());
    }

    #[test]
    fn radius_zero_subgraph_returns_just_the_root() {
        let artifact = artifact();

        let view = bounded_subgraph(&artifact, "root", 0, None);

        assert_eq!(ids(&view.nodes), vec!["root"]);
        assert!(view.edges.is_empty());
    }

    #[test]
    fn radius_one_subgraph_returns_immediate_neighbors_in_discovery_order() {
        let artifact = artifact();

        let view = bounded_subgraph(&artifact, "root", 1, Some(&[RelationKind::Calls]));

        assert_eq!(
            ids(&view.nodes),
            vec!["root", "caller_a", "caller_b", "callee_a"]
        );
        assert_eq!(view.edges.len(), 5);

        let unfiltered_view = bounded_subgraph(&artifact, "root", 1, None);

        assert_eq!(unfiltered_view.edges.len(), 6);
    }

    #[test]
    fn radius_two_subgraph_fans_out_one_more_hop() {
        let artifact = artifact();

        let view = bounded_subgraph(&artifact, "caller_a", 2, Some(&[RelationKind::Calls]));

        assert_eq!(
            ids(&view.nodes),
            vec!["caller_a", "root", "caller_b", "callee_a"]
        );
        assert_eq!(view.edges.len(), 5);
    }

    #[test]
    fn subgraph_handles_cycles_without_revisiting_nodes_forever() {
        let artifact = artifact();

        let view = bounded_subgraph(&artifact, "root", 5, Some(&[RelationKind::Calls]));

        assert_eq!(
            ids(&view.nodes),
            vec!["root", "caller_a", "caller_b", "callee_a"]
        );
        assert_eq!(view.edges.len(), 6);
    }

    #[test]
    fn subgraph_for_unknown_root_is_empty() {
        let artifact = artifact();

        let view = bounded_subgraph(&artifact, "missing", 1, None);

        assert!(view.nodes.is_empty());
        assert!(view.edges.is_empty());
    }
}

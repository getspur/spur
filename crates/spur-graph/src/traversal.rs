use std::collections::{HashSet, VecDeque};

use crate::{
    graph_edge_kind_or_default, GraphEdgeArtifact, GraphEdgeKind, GraphIndexArtifact,
    GraphSymbolArtifact, RelationKind,
};

const MAX_SUBGRAPH_RADIUS: u8 = 5;

#[derive(Debug, Clone, PartialEq)]
pub enum CalleeRecord<'a> {
    Resolved {
        symbol: &'a GraphSymbolArtifact,
        edge: &'a GraphEdgeArtifact,
    },
    Unresolved {
        edge: &'a GraphEdgeArtifact,
        target_label: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CallerRecord<'a> {
    Resolved {
        caller: &'a GraphSymbolArtifact,
        edge: &'a GraphEdgeArtifact,
    },
    Unresolved {
        caller: &'a GraphSymbolArtifact,
        edge: &'a GraphEdgeArtifact,
        target_label: String,
    },
}

impl CalleeRecord<'_> {
    pub fn edge(&self) -> &GraphEdgeArtifact {
        match self {
            CalleeRecord::Resolved { edge, .. } | CalleeRecord::Unresolved { edge, .. } => edge,
        }
    }

    pub fn edge_kind(&self) -> GraphEdgeKind {
        edge_kind(self.edge())
    }

    pub fn is_resolved(&self) -> bool {
        matches!(self, CalleeRecord::Resolved { .. })
    }
}

impl CallerRecord<'_> {
    pub fn edge(&self) -> &GraphEdgeArtifact {
        match self {
            CallerRecord::Resolved { edge, .. } | CallerRecord::Unresolved { edge, .. } => edge,
        }
    }

    pub fn edge_kind(&self) -> GraphEdgeKind {
        edge_kind(self.edge())
    }

    pub fn is_resolved(&self) -> bool {
        matches!(self, CallerRecord::Resolved { .. })
    }
}

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
    find_caller_edges(artifact, symbol_id)
        .into_iter()
        .filter_map(|record| match record {
            CallerRecord::Resolved { caller, .. } => Some(caller),
            CallerRecord::Unresolved { .. } => None,
        })
        .collect()
}

pub fn find_callees<'a>(
    artifact: &'a GraphIndexArtifact,
    symbol_id: &str,
) -> Vec<&'a GraphSymbolArtifact> {
    find_callee_edges(artifact, symbol_id)
        .into_iter()
        .filter_map(|record| match record {
            CalleeRecord::Resolved { symbol, .. } => Some(symbol),
            CalleeRecord::Unresolved { .. } => None,
        })
        .collect()
}

pub fn find_caller_edges<'a>(
    artifact: &'a GraphIndexArtifact,
    symbol_id: &str,
) -> Vec<CallerRecord<'a>> {
    let Some(target_symbol) = find_symbol(artifact, symbol_id) else {
        return Vec::new();
    };
    let unresolved_labels = unresolved_target_labels_for_symbol(target_symbol);

    artifact
        .edges
        .iter()
        .filter(|edge| is_caller_relation(edge.relation))
        .filter_map(|edge| {
            let caller = find_symbol(artifact, &edge.source_stable_symbol_id)?;
            if edge.target_stable_symbol_id.as_deref() == Some(symbol_id) {
                Some(CallerRecord::Resolved { caller, edge })
            } else if edge.target_stable_symbol_id.is_none()
                && edge
                    .target_label
                    .as_deref()
                    .is_some_and(|label| unresolved_labels.contains(label))
            {
                Some(CallerRecord::Unresolved {
                    caller,
                    edge,
                    target_label: edge.target_label.clone().unwrap_or_default(),
                })
            } else {
                None
            }
        })
        .collect()
}

pub fn find_callee_edges<'a>(
    artifact: &'a GraphIndexArtifact,
    symbol_id: &str,
) -> Vec<CalleeRecord<'a>> {
    if find_symbol(artifact, symbol_id).is_none() {
        return Vec::new();
    }

    artifact
        .edges
        .iter()
        .filter(|edge| is_caller_relation(edge.relation))
        .filter(|edge| edge.source_stable_symbol_id == symbol_id)
        .filter_map(|edge| match edge.target_stable_symbol_id.as_deref() {
            Some(target_id) => find_symbol(artifact, target_id)
                .map(|symbol| CalleeRecord::Resolved { symbol, edge }),
            None => edge
                .target_label
                .as_ref()
                .map(|target_label| CalleeRecord::Unresolved {
                    edge,
                    target_label: target_label.clone(),
                }),
        })
        .collect()
}

pub fn edge_kind(edge: &GraphEdgeArtifact) -> GraphEdgeKind {
    graph_edge_kind_or_default(edge.relation, edge.edge_kind)
}

pub fn bounded_subgraph<'a>(
    artifact: &'a GraphIndexArtifact,
    root_id: &str,
    radius: u8,
    edge_kinds: Option<&[GraphEdgeKind]>,
    include_unresolved: bool,
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
        let unresolved_current_labels = include_unresolved
            .then(|| find_symbol(artifact, current_id).map(unresolved_target_labels_for_symbol))
            .flatten();

        for (edge_index, edge) in artifact.edges.iter().enumerate() {
            if !edge_matches_filter(edge, edge_kinds) {
                continue;
            }
            if !include_unresolved && edge.target_stable_symbol_id.is_none() {
                continue;
            }

            // Outgoing edges with unresolved targets carry a `target_label` but
            // no symbol to enqueue. Record them as boundary edges so subgraph
            // consumers see the same neighbor set that `find_callee_edges` does;
            // don't advance depth since there is no target node to expand.
            if edge.source_stable_symbol_id == current_id && edge.target_stable_symbol_id.is_none()
            {
                if visited_edges.insert(edge_index) {
                    edges.push(edge);
                }
                continue;
            }

            if edge.target_stable_symbol_id.is_none() {
                let Some(labels) = unresolved_current_labels.as_ref() else {
                    continue;
                };
                if !edge
                    .target_label
                    .as_deref()
                    .is_some_and(|label| labels.contains(label))
                {
                    continue;
                }

                let caller_id = edge.source_stable_symbol_id.as_str();
                let neighbor = if visited_nodes.contains(caller_id) {
                    None
                } else {
                    let Some(symbol) = find_symbol(artifact, caller_id) else {
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

/// Edges that contribute to caller/callee relationships.
///
/// Includes `RelationKind::References` so that function-value passes such as
/// `.map(fn_name)` flow through `find_callers`/`find_callee_edges` — without
/// this, a function that is only ever referenced via higher-order method
/// calls would appear to have zero callers (Flaw A in the code_* tool audit).
/// Consumers that need strict-Calls-only behavior can filter at the
/// `code_subgraph` layer via public `GraphEdgeKind` values such as
/// `edge_kinds=["calls"]`.
fn is_caller_relation(relation: RelationKind) -> bool {
    matches!(relation, RelationKind::Calls | RelationKind::References)
}

fn edge_matches_filter(edge: &GraphEdgeArtifact, edge_kinds: Option<&[GraphEdgeKind]>) -> bool {
    edge_kinds.is_none_or(|kinds| kinds.contains(&edge_kind(edge)))
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

fn unresolved_target_labels_for_symbol(symbol: &GraphSymbolArtifact) -> HashSet<&str> {
    let mut labels = HashSet::new();
    labels.insert(symbol.entity_name.as_str());
    labels.insert(symbol.qualified_name.as_str());
    labels.insert(symbol.stable_symbol_id.as_str());
    labels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Confidence, GraphEdgeKind, GraphIndexHeader};

    fn symbol(id: &str) -> GraphSymbolArtifact {
        GraphSymbolArtifact {
            stable_symbol_id: id.to_string(),
            file_path: format!("src/{id}.rs"),
            byte_range: [0, 8],
            line_range: [1, 1],
            entity_name: id.to_string(),
            qualified_name: id.to_string(),
            symbol_kind: "function".to_string(),
            anchor_hash: format!("hash-{id}"),
            enclosing_scope: None,
        }
    }

    fn edge(source: &str, target: &str, relation: RelationKind) -> GraphEdgeArtifact {
        edge_with_kind(source, target, relation, None)
    }

    fn edge_with_kind(
        source: &str,
        target: &str,
        relation: RelationKind,
        edge_kind: Option<GraphEdgeKind>,
    ) -> GraphEdgeArtifact {
        GraphEdgeArtifact {
            source_stable_symbol_id: source.to_string(),
            target_stable_symbol_id: Some(target.to_string()),
            target_label: None,
            relation,
            confidence: Confidence::SyntaxExact,
            confidence_score: 1.0,
            edge_kind,
        }
    }

    fn unresolved_call_edge(source: &str, target_label: &str) -> GraphEdgeArtifact {
        GraphEdgeArtifact {
            source_stable_symbol_id: source.to_string(),
            target_stable_symbol_id: None,
            target_label: Some(target_label.to_string()),
            relation: RelationKind::Calls,
            confidence: Confidence::SyntaxExact,
            confidence_score: 1.0,
            edge_kind: None,
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
    fn callers_and_callees_follow_calls_and_references_edges() {
        // Fixture has a Calls edge root → caller_a AND a References edge root →
        // caller_a. Both flow through find_callees, so caller_a appears twice.
        let artifact = artifact();

        assert_eq!(
            ids(&find_callers(&artifact, "root")),
            vec!["caller_a", "caller_b"]
        );
        assert_eq!(
            ids(&find_callees(&artifact, "root")),
            vec!["caller_a", "caller_b", "callee_a", "caller_a"]
        );
        assert!(find_callers(&artifact, "missing").is_empty());
        assert!(find_callees(&artifact, "missing").is_empty());
    }

    #[test]
    fn callee_edges_include_unresolved_target_labels() {
        let mut artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: "test".to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            symbols: ["root", "callee"].into_iter().map(symbol).collect(),
            edges: vec![
                edge("root", "callee", RelationKind::Calls),
                unresolved_call_edge("root", "into"),
            ],
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
        };
        artifact
            .edges
            .push(edge("root", "callee", RelationKind::References));

        let callees = find_callee_edges(&artifact, "root");

        // 3 records: one Calls edge to `callee`, one unresolved Calls edge with
        // label "into", and one References edge to `callee` (the same symbol
        // surfaces twice because Calls and References both contribute to
        // find_callee_edges since Flaw-A's API broadening landed).
        assert_eq!(callees.len(), 3);
        assert!(matches!(
            callees[0],
            CalleeRecord::Resolved { symbol, .. } if symbol.stable_symbol_id == "callee"
        ));
        assert_eq!(
            callees[1],
            CalleeRecord::Unresolved {
                edge: &artifact.edges[1],
                target_label: "into".to_string()
            }
        );
        assert!(matches!(
            callees[2],
            CalleeRecord::Resolved { symbol, .. } if symbol.stable_symbol_id == "callee"
        ));
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

        let view = bounded_subgraph(&artifact, "root", 0, None, false);

        assert_eq!(ids(&view.nodes), vec!["root"]);
        assert!(view.edges.is_empty());
    }

    #[test]
    fn radius_one_subgraph_returns_immediate_neighbors_in_discovery_order() {
        let artifact = artifact();

        let view = bounded_subgraph(&artifact, "root", 1, Some(&[GraphEdgeKind::Calls]), false);

        assert_eq!(
            ids(&view.nodes),
            vec!["root", "caller_a", "caller_b", "callee_a"]
        );
        assert_eq!(view.edges.len(), 5);

        let unfiltered_view = bounded_subgraph(&artifact, "root", 1, None, false);

        assert_eq!(unfiltered_view.edges.len(), 6);
    }

    #[test]
    fn radius_two_subgraph_fans_out_one_more_hop() {
        let artifact = artifact();

        let view = bounded_subgraph(
            &artifact,
            "caller_a",
            2,
            Some(&[GraphEdgeKind::Calls]),
            false,
        );

        assert_eq!(
            ids(&view.nodes),
            vec!["caller_a", "root", "caller_b", "callee_a"]
        );
        assert_eq!(view.edges.len(), 5);
    }

    #[test]
    fn subgraph_handles_cycles_without_revisiting_nodes_forever() {
        let artifact = artifact();

        let view = bounded_subgraph(&artifact, "root", 5, Some(&[GraphEdgeKind::Calls]), false);

        assert_eq!(
            ids(&view.nodes),
            vec!["root", "caller_a", "caller_b", "callee_a"]
        );
        assert_eq!(view.edges.len(), 6);
    }

    #[test]
    fn subgraph_for_unknown_root_is_empty() {
        let artifact = artifact();

        let view = bounded_subgraph(&artifact, "missing", 1, None, false);

        assert!(view.nodes.is_empty());
        assert!(view.edges.is_empty());
    }

    #[test]
    fn subgraph_filters_outgoing_unresolved_edges_by_default() {
        let artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: "test".to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            symbols: ["root", "callee"].into_iter().map(symbol).collect(),
            edges: vec![
                edge("root", "callee", RelationKind::Calls),
                unresolved_call_edge("root", "as_ref"),
                unresolved_call_edge("root", "map"),
            ],
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
        };

        let view = bounded_subgraph(&artifact, "root", 1, Some(&[GraphEdgeKind::Calls]), false);

        assert_eq!(ids(&view.nodes), vec!["root", "callee"]);
        assert_eq!(view.edges.len(), 1);
        assert!(view
            .edges
            .iter()
            .all(|edge| edge.target_stable_symbol_id.is_some()));
    }

    #[test]
    fn subgraph_can_include_outgoing_unresolved_edges_without_enqueueing_neighbor() {
        let artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: "test".to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            symbols: ["root", "callee"].into_iter().map(symbol).collect(),
            edges: vec![
                edge("root", "callee", RelationKind::Calls),
                unresolved_call_edge("root", "as_ref"),
                unresolved_call_edge("root", "map"),
            ],
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
        };

        let view = bounded_subgraph(&artifact, "root", 1, Some(&[GraphEdgeKind::Calls]), true);

        assert_eq!(ids(&view.nodes), vec!["root", "callee"]);
        assert_eq!(view.edges.len(), 3);
        let unresolved_labels: Vec<&str> = view
            .edges
            .iter()
            .filter(|edge| edge.target_stable_symbol_id.is_none())
            .filter_map(|edge| edge.target_label.as_deref())
            .collect();
        assert_eq!(unresolved_labels, vec!["as_ref", "map"]);
    }

    #[test]
    fn subgraph_can_include_incoming_unresolved_caller_edges() {
        let artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: "test".to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            symbols: ["root", "caller", "unresolved_caller"]
                .into_iter()
                .map(symbol)
                .collect(),
            edges: vec![
                edge("caller", "root", RelationKind::Calls),
                unresolved_call_edge("unresolved_caller", "root"),
            ],
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
        };

        let default_view =
            bounded_subgraph(&artifact, "root", 1, Some(&[GraphEdgeKind::Calls]), false);
        assert_eq!(ids(&default_view.nodes), vec!["root", "caller"]);
        assert_eq!(default_view.edges.len(), 1);

        let included_view =
            bounded_subgraph(&artifact, "root", 1, Some(&[GraphEdgeKind::Calls]), true);

        assert_eq!(
            ids(&included_view.nodes),
            vec!["root", "caller", "unresolved_caller"]
        );
        assert_eq!(included_view.edges.len(), 2);
        assert!(included_view.edges.iter().any(|edge| {
            edge.source_stable_symbol_id == "unresolved_caller"
                && edge.target_stable_symbol_id.is_none()
                && edge.target_label.as_deref() == Some("root")
        }));
    }

    #[test]
    fn unresolved_edge_does_not_advance_radius_depth() {
        let artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: "test".to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            symbols: ["root"].into_iter().map(symbol).collect(),
            edges: vec![unresolved_call_edge("root", "external")],
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
        };

        let r0 = bounded_subgraph(&artifact, "root", 0, None, true);
        assert!(r0.edges.is_empty(), "radius 0 must skip edge scanning");

        let r1 = bounded_subgraph(&artifact, "root", 1, None, true);
        assert_eq!(r1.edges.len(), 1);
        assert_eq!(ids(&r1.nodes), vec!["root"]);
    }

    #[test]
    fn callers_and_callees_include_references_edges() {
        let artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: "test".to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            symbols: ["caller", "target"].into_iter().map(symbol).collect(),
            edges: vec![edge("caller", "target", RelationKind::References)],
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
        };

        assert_eq!(ids(&find_callers(&artifact, "target")), vec!["caller"]);
        assert_eq!(ids(&find_callees(&artifact, "caller")), vec!["target"]);

        let view = bounded_subgraph(&artifact, "target", 1, None, false);
        assert_eq!(ids(&view.nodes), vec!["target", "caller"]);
    }

    #[test]
    fn subgraph_edge_kind_filter_is_strict_for_static_calls() {
        let artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: "test".to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            symbols: ["root", "direct", "dyn_trait", "hof"]
                .into_iter()
                .map(symbol)
                .collect(),
            edges: vec![
                edge("root", "direct", RelationKind::Calls),
                edge_with_kind(
                    "root",
                    "dyn_trait",
                    RelationKind::Calls,
                    Some(GraphEdgeKind::CallsDyn),
                ),
                edge_with_kind(
                    "root",
                    "hof",
                    RelationKind::References,
                    Some(GraphEdgeKind::ReferencesHof),
                ),
            ],
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
        };

        let static_calls =
            bounded_subgraph(&artifact, "root", 1, Some(&[GraphEdgeKind::Calls]), false);
        assert_eq!(ids(&static_calls.nodes), vec!["root", "direct"]);
        assert_eq!(static_calls.edges.len(), 1);
        assert_eq!(edge_kind(static_calls.edges[0]), GraphEdgeKind::Calls);

        let dyn_calls = bounded_subgraph(
            &artifact,
            "root",
            1,
            Some(&[GraphEdgeKind::CallsDyn]),
            false,
        );
        assert_eq!(ids(&dyn_calls.nodes), vec!["root", "dyn_trait"]);
        assert_eq!(dyn_calls.edges.len(), 1);
        assert_eq!(edge_kind(dyn_calls.edges[0]), GraphEdgeKind::CallsDyn);
    }

    #[test]
    fn legacy_reference_edges_without_edge_kind_default_to_references_other() {
        let legacy_reference = edge("root", "callee", RelationKind::References);

        assert_eq!(edge_kind(&legacy_reference), GraphEdgeKind::ReferencesOther);
    }

    #[test]
    fn caller_and_callee_records_carry_edge_kind_for_all_public_values() {
        let artifact = GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: "test".to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            symbols: ["root", "direct", "dyn_trait", "hof", "other"]
                .into_iter()
                .map(symbol)
                .collect(),
            edges: vec![
                edge("root", "direct", RelationKind::Calls),
                edge_with_kind(
                    "root",
                    "dyn_trait",
                    RelationKind::Calls,
                    Some(GraphEdgeKind::CallsDyn),
                ),
                edge_with_kind(
                    "root",
                    "hof",
                    RelationKind::References,
                    Some(GraphEdgeKind::ReferencesHof),
                ),
                edge_with_kind(
                    "root",
                    "other",
                    RelationKind::References,
                    Some(GraphEdgeKind::ReferencesOther),
                ),
            ],
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
        };

        let callee_kinds: Vec<_> = find_callee_edges(&artifact, "root")
            .into_iter()
            .map(|record| record.edge_kind())
            .collect();
        assert_eq!(
            callee_kinds,
            vec![
                GraphEdgeKind::Calls,
                GraphEdgeKind::CallsDyn,
                GraphEdgeKind::ReferencesHof,
                GraphEdgeKind::ReferencesOther,
            ]
        );

        let caller_records = find_caller_edges(&artifact, "dyn_trait");
        assert_eq!(caller_records.len(), 1);
        assert_eq!(caller_records[0].edge_kind(), GraphEdgeKind::CallsDyn);
        assert!(matches!(
            caller_records[0],
            CallerRecord::Resolved { caller, .. } if caller.stable_symbol_id == "root"
        ));
    }
}

use spur_graph::{build_facts, NodeKind, RelationKind};

fn build(src: &str) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), src).expect("write fixture");
    build_facts(dir.path(), None).expect("build facts").0
}

#[test]
fn relational_edges_never_resolve_out_of_range() {
    let facts = build(
        "enum Marker { Send }\ntrait Base {}\ntrait Derived: Base + Send {}\nstruct S;\nimpl Default for S {}\n",
    );
    let kind_of = |id| facts.nodes.iter().find(|n| n.node_id == id).map(|n| n.kind);

    for edge in &facts.edges {
        if matches!(
            edge.relation,
            RelationKind::Implements | RelationKind::Extends
        ) {
            if let Some(target_id) = edge.target_node_id {
                assert!(
                    matches!(
                        kind_of(target_id),
                        Some(NodeKind::Trait) | Some(NodeKind::Interface) | Some(NodeKind::Class)
                    ),
                    "{:?} edge resolved to out-of-range kind {:?} (target_label={:?})",
                    edge.relation,
                    kind_of(target_id),
                    edge.target_label
                );
            }
        }
    }

    assert!(
        !facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Extends
                && edge.target_node_id.is_some()
                && kind_of(edge.target_node_id.unwrap()) == Some(NodeKind::EnumVariant)
        }),
        "extends bound a std marker name to a local enum variant"
    );
}

#[test]
fn local_supertrait_still_resolves() {
    let facts = build("trait Base {}\ntrait Derived: Base {}\n");
    let base = facts
        .nodes
        .iter()
        .find(|node| node.label == "Base" && node.kind == NodeKind::Trait)
        .expect("Base trait");

    assert!(
        facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Extends && edge.target_node_id == Some(base.node_id)
        }),
        "local supertrait Base should still resolve; edges: {:?}",
        facts
            .edges
            .iter()
            .filter(|edge| edge.relation == RelationKind::Extends)
            .map(|edge| (edge.target_node_id, edge.target_label.clone()))
            .collect::<Vec<_>>()
    );
}

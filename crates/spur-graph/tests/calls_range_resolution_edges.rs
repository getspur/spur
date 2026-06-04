use spur_graph::{build_facts, NodeKind, RelationKind};

fn build(src: &str) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), src).expect("write fixture");
    build_facts(dir.path(), None).expect("build facts").0
}

#[test]
fn calls_never_resolve_to_non_callable_targets() {
    let facts = build(
        "struct Request { status: u32 }\n\
         fn caller() {\n\
             status();\n\
         }\n",
    );
    let kind_of = |id| facts.nodes.iter().find(|n| n.node_id == id).map(|n| n.kind);

    for edge in &facts.edges {
        if edge.relation == RelationKind::Calls {
            if let Some(target_id) = edge.target_node_id {
                assert!(
                    matches!(
                        kind_of(target_id),
                        Some(NodeKind::Function | NodeKind::Method)
                    ),
                    "calls edge resolved to non-callable kind {:?} (target_label={:?})",
                    kind_of(target_id),
                    edge.target_label
                );
            }
        }
    }

    let status_field = facts
        .nodes
        .iter()
        .find(|node| node.label == "status" && node.kind == NodeKind::Field)
        .expect("status field");

    assert!(
        facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Calls
                && edge.target_label.as_deref() == Some("status")
                && edge.target_node_id.is_none()
        }),
        "field-shaped status() call should remain unresolved; edges: {:?}",
        facts
            .edges
            .iter()
            .filter(|edge| edge.target_label.as_deref() == Some("status"))
            .map(|edge| (
                edge.relation,
                edge.target_node_id,
                edge.target_label.clone()
            ))
            .collect::<Vec<_>>()
    );
    assert!(
        !facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Calls
                && edge.target_label.as_deref() == Some("status")
                && edge.target_node_id == Some(status_field.node_id)
        }),
        "status() must not resolve to the same-label field"
    );
}

#[test]
fn local_function_calls_still_resolve() {
    let facts = build("fn h() {}\nfn c() { h(); }\n");
    let h = facts
        .nodes
        .iter()
        .find(|node| node.label == "h" && node.kind == NodeKind::Function)
        .expect("h function");

    assert!(
        facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Calls
                && edge.target_label.as_deref() == Some("h")
                && edge.target_node_id == Some(h.node_id)
        }),
        "local function call h() should still resolve; edges: {:?}",
        facts
            .edges
            .iter()
            .filter(|edge| edge.target_label.as_deref() == Some("h"))
            .map(|edge| (
                edge.relation,
                edge.target_node_id,
                edge.target_label.clone()
            ))
            .collect::<Vec<_>>()
    );
}

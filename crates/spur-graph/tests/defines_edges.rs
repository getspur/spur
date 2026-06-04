use spur_graph::{build_facts, extract::GraphFacts, RelationKind};

fn build(src: &str) -> GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), src).expect("write fixture");
    build_facts(dir.path(), None).expect("build facts").0
}

fn source_label<'a>(facts: &'a GraphFacts, edge: &spur_graph::GraphEdge) -> Option<&'a str> {
    node_label(facts, edge.source_node_id)
}

fn target_label<'a>(facts: &'a GraphFacts, edge: &'a spur_graph::GraphEdge) -> Option<&'a str> {
    edge.target_node_id
        .and_then(|node_id| node_label(facts, node_id))
        .or(edge.target_label.as_deref())
}

fn node_label(facts: &GraphFacts, node_id: spur_graph::NodeId) -> Option<&str> {
    facts
        .nodes
        .iter()
        .find(|node| node.node_id == node_id)
        .map(|node| node.label.as_str())
}

fn has_edge(facts: &GraphFacts, relation: RelationKind, source: &str, target: &str) -> bool {
    facts.edges.iter().any(|edge| {
        edge.relation == relation
            && source_label(facts, edge) == Some(source)
            && target_label(facts, edge) == Some(target)
    })
}

#[test]
fn struct_defines_field_and_enum_defines_variant() {
    let facts = build("struct S { f: i32 }\nenum E { V }\n");

    assert!(
        has_edge(&facts, RelationKind::Defines, "S", "f"),
        "S defines f; got {:?}",
        facts
            .edges
            .iter()
            .filter(|edge| edge.relation == RelationKind::Defines)
            .map(|edge| (source_label(&facts, edge), target_label(&facts, edge)))
            .collect::<Vec<_>>()
    );
    assert!(
        has_edge(&facts, RelationKind::Defines, "E", "V"),
        "E defines V"
    );
    assert!(
        has_edge(&facts, RelationKind::Contains, "S", "f"),
        "Contains S->f still present"
    );
}

#[test]
fn module_defines_fn_but_file_does_not_define_module() {
    let facts = build("mod m { fn g() {} }\n");

    assert!(
        has_edge(&facts, RelationKind::Defines, "m", "g"),
        "m defines g"
    );
    assert!(
        !facts.edges.iter().any(|edge| {
            edge.relation == RelationKind::Defines && target_label(&facts, edge) == Some("m")
        }),
        "file must not define the module"
    );
}

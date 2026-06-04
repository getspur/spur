use spur_graph::{build_facts, NodeKind, RelationKind};

fn build(src: &str) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.rs"), src).expect("write fixture");
    build_facts(dir.path(), None).expect("build facts").0
}

#[test]
fn tuple_struct_and_enum_variant_construction_is_constructs() {
    let facts = build(
        "struct Foo(u32);\nenum E { V(u32) }\nfn f() { let _ = Foo(1); }\nfn g() { let _ = E::V(1); }\n",
    );
    let has = |rel, tgt| {
        facts
            .edges
            .iter()
            .any(|e| e.relation == rel && e.target_label.as_deref() == Some(tgt))
    };
    assert!(
        has(RelationKind::Constructs, "Foo"),
        "Foo(1) should be constructs; edges: {:?}",
        facts
            .edges
            .iter()
            .map(|e| (e.relation, e.target_label.clone()))
            .collect::<Vec<_>>()
    );
    assert!(
        has(RelationKind::Constructs, "V"),
        "E::V(1) should be constructs"
    );

    let kind_of = |id| facts.nodes.iter().find(|n| n.node_id == id).map(|n| n.kind);
    for e in &facts.edges {
        if e.relation == RelationKind::Constructs {
            if let Some(t) = e.target_node_id {
                assert!(
                    matches!(
                        kind_of(t),
                        Some(NodeKind::Struct | NodeKind::EnumVariant | NodeKind::Class)
                    ),
                    "constructs to non-type kind {:?}",
                    kind_of(t)
                );
            }
        }
    }
}

#[test]
fn plain_function_call_stays_calls() {
    let facts = build("fn h() {}\nfn c() { h(); }\n");
    assert!(facts
        .edges
        .iter()
        .any(|e| { e.relation == RelationKind::Calls && e.target_label.as_deref() == Some("h") }));
    assert!(!facts.edges.iter().any(|e| {
        e.relation == RelationKind::Constructs && e.target_label.as_deref() == Some("h")
    }));
}

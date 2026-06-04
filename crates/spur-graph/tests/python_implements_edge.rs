use spur_graph::{build_facts, NodeKind, RelationKind};

fn build(src: &str) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("lib.py"), src).expect("write fixture");
    build_facts(dir.path(), None).expect("build facts").0
}

#[test]
fn python_protocol_base_reclassifies_subclass_edge_as_implements() {
    // The stdlib Protocol symbol is outside the worktree, so this only proves
    // local classes whose own bases are named Protocol/ABC/abc.ABC become
    // interface targets for subclasses.
    let facts = build(
        r#"
class Reader(Protocol):
    def read(self):
        pass

class FileReader(Reader):
    def read(self):
        return "data"

class A:
    pass

class B(A):
    pass
"#,
    );

    let node = |kind, label| {
        facts
            .nodes
            .iter()
            .find(|node| node.kind == kind && node.label == label)
            .unwrap_or_else(|| panic!("missing {kind:?} node {label}"))
    };

    let reader = node(NodeKind::Interface, "Reader");
    let file_reader = node(NodeKind::Class, "FileReader");
    let class_a = node(NodeKind::Class, "A");
    let class_b = node(NodeKind::Class, "B");

    let has_edge = |source, relation, target| {
        facts.edges.iter().any(|edge| {
            edge.source_node_id == source
                && edge.relation == relation
                && edge.target_node_id == Some(target)
        })
    };

    assert!(
        has_edge(
            file_reader.node_id,
            RelationKind::Implements,
            reader.node_id
        ),
        "FileReader should implement local Protocol-like Reader; edges: {:?}",
        facts
            .edges
            .iter()
            .map(|edge| (
                edge.relation,
                edge.target_label.clone(),
                edge.target_node_id
            ))
            .collect::<Vec<_>>()
    );
    assert!(
        !has_edge(file_reader.node_id, RelationKind::Extends, reader.node_id),
        "FileReader -> Reader must be implements, not extends"
    );
    assert!(
        has_edge(class_b.node_id, RelationKind::Extends, class_a.node_id),
        "ordinary class inheritance B(A) should stay extends"
    );
    assert!(
        !has_edge(class_b.node_id, RelationKind::Implements, class_a.node_id),
        "ordinary class inheritance B(A) must not become implements"
    );

    for edge in facts.edges.iter().filter(|edge| {
        matches!(
            edge.relation,
            RelationKind::Extends | RelationKind::Implements
        )
    }) {
        if let Some(target_id) = edge.target_node_id {
            let target_kind = facts
                .nodes
                .iter()
                .find(|node| node.node_id == target_id)
                .map(|node| node.kind);
            assert!(
                matches!(
                    target_kind,
                    Some(NodeKind::Trait | NodeKind::Interface | NodeKind::Class)
                ),
                "{:?} edge resolved to out-of-range kind {:?} (target_label={:?})",
                edge.relation,
                target_kind,
                edge.target_label
            );
        }
    }
}

use spur_graph::{
    Confidence, EdgeId, EvidenceId, FileId, GraphEdge, GraphNode, NodeId, NodeKind, RelationKind,
    RunId, SourceSpan, SpanId,
};

#[test]
fn graph_facts_round_trip_through_json() {
    let node = GraphNode {
        node_id: NodeId(7),
        stable_key: "rust:src/lib.rs:run".to_string(),
        label: "run".to_string(),
        kind: NodeKind::Function,
        file_id: Some(FileId(3)),
        source_span_id: Some(SpanId(11)),
        first_seen_run_id: RunId(19),
    };
    let edge = GraphEdge {
        edge_id: EdgeId(5),
        source_node_id: NodeId(7),
        target_node_id: NodeId(8),
        relation: RelationKind::Calls,
        confidence: Confidence::Exact,
        confidence_score: 1.0,
        evidence_id: EvidenceId(13),
        directed: true,
    };
    let span = SourceSpan {
        span_id: SpanId(11),
        file_id: FileId(3),
        start_byte: 10,
        end_byte: 42,
        start_line: 2,
        end_line: 4,
    };

    let encoded = serde_json::to_string(&(node.clone(), edge.clone(), span.clone())).unwrap();
    let decoded: (GraphNode, GraphEdge, SourceSpan) = serde_json::from_str(&encoded).unwrap();

    assert_eq!(decoded, (node, edge, span));
}

#[test]
fn graph_id_newtypes_are_not_interchangeable_at_runtime() {
    let node_id = NodeId(42);
    let edge_id = EdgeId(42);
    let json = serde_json::to_string(&node_id).unwrap();

    assert_eq!(json, "42");
    assert_eq!(node_id.get(), 42);
    assert_eq!(edge_id.get(), 42);
    assert_ne!(format!("{node_id:?}"), format!("{edge_id:?}"));
}

#[test]
fn node_kind_discriminators_are_stable_contracts() {
    assert_eq!(NodeKind::File.discriminator(), "file");
    assert_eq!(NodeKind::Module.discriminator(), "module");
    assert_eq!(NodeKind::Function.discriminator(), "function");
    assert_eq!(NodeKind::Method.discriminator(), "method");
    assert_eq!(NodeKind::Struct.discriminator(), "struct");
    assert_eq!(NodeKind::Enum.discriminator(), "enum");
    assert_eq!(NodeKind::Trait.discriminator(), "trait");
    assert_eq!(NodeKind::Impl.discriminator(), "impl");
    assert_eq!(NodeKind::Field.discriminator(), "field");
    assert_eq!(NodeKind::Constant.discriminator(), "constant");
    assert_eq!(NodeKind::TypeAlias.discriminator(), "type_alias");
    assert_eq!(NodeKind::Macro.discriminator(), "macro");
}

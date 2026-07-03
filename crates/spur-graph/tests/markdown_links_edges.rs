use spur_graph::{build_facts, NodeKind, RelationKind};

#[test]
fn markdown_reference_style_links_resolve_to_definition_destinations() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("index.md"),
        r#"# Overview

Inline link to [Direct](direct.md).
Full reference link to [Guide][guide-ref].
Collapsed reference [guide-ref][].
Shortcut reference [guide-ref].
Missing shortcut [no-such-label].

[guide-ref]: guide.md
"#,
    )
    .expect("write index.md");
    std::fs::write(dir.path().join("direct.md"), "# Direct\n").expect("write direct.md");
    std::fs::write(dir.path().join("guide.md"), "# Guide\n").expect("write guide.md");

    let facts = build_facts(dir.path(), None).expect("build facts").0;
    let link_targets = facts
        .edges
        .iter()
        .filter(|edge| edge.relation == RelationKind::Links)
        .filter_map(|edge| edge.target_label.as_deref())
        .collect::<Vec<_>>();

    for target in ["direct.md", "guide.md", "no-such-label"] {
        assert!(
            link_targets.contains(&target),
            "missing Markdown link edge to {target}; link targets: {link_targets:?}"
        );
    }
    assert!(
        !link_targets.contains(&"[guide-ref]"),
        "full reference labels must resolve to destinations: {link_targets:?}"
    );
    assert!(
        !link_targets.contains(&"guide-ref"),
        "collapsed and shortcut reference labels must resolve to destinations: {link_targets:?}"
    );
    let guide_file = facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::File && node.label == "guide.md")
        .expect("guide.md file node");
    assert!(facts.edges.iter().any(|edge| {
        edge.relation == RelationKind::Links
            && edge.target_label.as_deref() == Some("guide.md")
            && edge.target_node_id == Some(guide_file.node_id)
    }));
    let missing = facts
        .edges
        .iter()
        .find(|edge| {
            edge.relation == RelationKind::Links
                && edge.target_label.as_deref() == Some("no-such-label")
        })
        .expect("missing label edge");
    assert_eq!(missing.target_node_id, None);
    assert!(
        !facts
            .edges
            .iter()
            .any(|edge| edge.relation == RelationKind::Imports),
        "Markdown @import capture channel must remap to Links, not Imports"
    );
}

use spur_graph::{build_facts, RelationKind};

#[test]
fn markdown_inline_and_reference_style_links_emit_links_not_imports() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        dir.path().join("index.md"),
        r#"# Overview

Inline link to [Guide](guide.md).
Full reference link to [Guide][guide-ref].
Collapsed reference [guide-ref][].
Shortcut reference [guide-ref].

[guide-ref]: guide.md
"#,
    )
    .expect("write index.md");
    std::fs::write(dir.path().join("guide.md"), "# Guide\n").expect("write guide.md");

    let facts = build_facts(dir.path(), None).expect("build facts").0;
    let link_targets = facts
        .edges
        .iter()
        .filter(|edge| edge.relation == RelationKind::Links)
        .filter_map(|edge| edge.target_label.as_deref())
        .collect::<Vec<_>>();

    for target in ["guide.md", "[guide-ref]", "guide-ref"] {
        assert!(
            link_targets.contains(&target),
            "missing Markdown link edge to {target}; link targets: {link_targets:?}"
        );
    }
    assert!(
        !facts
            .edges
            .iter()
            .any(|edge| edge.relation == RelationKind::Imports),
        "Markdown @import capture channel must remap to Links, not Imports"
    );
}

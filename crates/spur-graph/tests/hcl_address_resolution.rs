use std::fs;

use pretty_assertions::assert_eq;
use spur_graph::{build_facts, Confidence, GraphEdge, NodeId, RelationKind};

const WEB_MODULE: &str = r#"
resource "aws_instance" "web" {}

resource "aws_eip" "ip" {
  instance = aws_instance.web.id
}
"#;

const WEB_DUPLICATE: &str = r#"
resource "aws_instance" "web" {}
"#;

const SINGLETON_MODULE: &str = r#"
resource "aws_s3_bucket" "logs" {}
"#;

const ROOT_REFS: &str = r#"
resource "aws_cloudtrail" "trail" {
  s3_bucket_name = aws_s3_bucket.logs.id
  instance_ref   = aws_instance.web.id
}
"#;

const PHANTOM_REF: &str = r#"
resource "aws_eip" "ip" {
  instance = aws_instance.ghost.id
}
"#;

const PHANTOM_MARKDOWN: &str = "# aws_instance.ghost\n\nRunbook notes about the ghost host.\n";

const INTERPOLATION_MODULE: &str = r#"
variable "env" {}

locals {
  name_prefix = "spur-${var.env}"
}
"#;

fn build_fixture(files: &[(&str, &str)]) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    for (path, source) in files {
        let full = dir.path().join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create fixture dirs");
        }
        fs::write(full, source).expect("write fixture");
    }
    build_facts(dir.path(), None).expect("extract").0
}

fn reference_edge<'a>(facts: &'a spur_graph::extract::GraphFacts, target: &str) -> &'a GraphEdge {
    facts
        .edges
        .iter()
        .find(|edge| {
            edge.relation == RelationKind::References
                && edge.target_label.as_deref() == Some(target)
        })
        .unwrap_or_else(|| panic!("missing references edge for `{target}`"))
}

fn node_file_path(facts: &spur_graph::extract::GraphFacts, node_id: NodeId) -> String {
    let node = facts
        .nodes
        .iter()
        .find(|node| node.node_id == node_id)
        .expect("target node");
    facts
        .nodes
        .iter()
        .find(|candidate| {
            candidate.kind == spur_graph::NodeKind::File && candidate.file_id == node.file_id
        })
        .map(|file| file.label.clone())
        .expect("file node for target")
}

#[test]
fn address_refs_bind_within_their_module_directory() {
    let facts = build_fixture(&[
        ("modules/a/main.tf", WEB_MODULE),
        ("modules/b/main.tf", WEB_DUPLICATE),
    ]);

    let edge = reference_edge(&facts, "aws_instance.web");
    let target = edge
        .target_node_id
        .expect("duplicate address must still bind inside its own module directory");
    assert_eq!(node_file_path(&facts, target), "modules/a/main.tf");
    assert_eq!(edge.bind_method.as_deref(), Some("address_module_scope"));
}

#[test]
fn ambiguous_addresses_outside_any_defining_module_stay_unresolved() {
    let facts = build_fixture(&[
        ("main.tf", ROOT_REFS),
        ("modules/a/main.tf", WEB_MODULE),
        ("modules/b/main.tf", WEB_DUPLICATE),
    ]);

    let root_source = reference_edge(&facts, "aws_s3_bucket.logs").source_node_id;
    let unresolved_root_ref = facts
        .edges
        .iter()
        .find(|edge| {
            edge.relation == RelationKind::References
                && edge.target_label.as_deref() == Some("aws_instance.web")
                && edge.source_node_id == root_source
        })
        .expect("root reference edge to the duplicated address");
    assert_eq!(
        unresolved_root_ref.target_node_id, None,
        "two candidate modules and no local definition must stay unresolved"
    );
}

#[test]
fn workspace_singleton_addresses_bind_across_directories() {
    let facts = build_fixture(&[
        ("main.tf", ROOT_REFS),
        ("modules/storage/main.tf", SINGLETON_MODULE),
    ]);

    let edge = reference_edge(&facts, "aws_s3_bucket.logs");
    let target = edge
        .target_node_id
        .expect("unique workspace address must bind via the singleton fallback");
    assert_eq!(node_file_path(&facts, target), "modules/storage/main.tf");
    assert_eq!(edge.bind_method.as_deref(), Some("address_singleton"));
}

#[test]
fn markdown_sections_never_receive_address_binds() {
    let facts = build_fixture(&[("main.tf", PHANTOM_REF), ("notes.md", PHANTOM_MARKDOWN)]);

    let edge = reference_edge(&facts, "aws_instance.ghost");
    assert_eq!(
        edge.target_node_id, None,
        "a markdown section titled `aws_instance.ghost` must never satisfy an address bind"
    );
}

#[test]
fn interpolated_refs_bind_module_scoped_with_raised_confidence() {
    let facts = build_fixture(&[("stack/main.tf", INTERPOLATION_MODULE)]);

    let edge = reference_edge(&facts, "var.env");
    let target = edge
        .target_node_id
        .expect("template interpolation ref must bind to the module variable");
    assert_eq!(node_file_path(&facts, target), "stack/main.tf");
    assert_eq!(edge.bind_method.as_deref(), Some("address_module_scope"));
    assert_eq!(edge.confidence, Confidence::Heuristic);
    assert_eq!(edge.confidence_score, 0.8);
}

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use pretty_assertions::assert_eq;
use spur_graph::extract::languages::{all_supported_extensions, Language};
use spur_graph::{build_facts, NodeKind};
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator as _};

const HCL_TAGS_QUERY: &str = include_str!("../queries/hcl/tags.scm");

const TERRAFORM_FIXTURE: &str = r#"
variable "region" {
  type    = string
  default = "us-east-1"
}

variable "env" {}

locals {
  name_prefix = "spur-${var.env}"
  port        = 8080
}

data "aws_ami" "ubuntu" {
  most_recent = true
}

resource "aws_instance" "web" {
  ami           = data.aws_ami.ubuntu.id
  instance_type = var.instance_type
  subnet_id     = module.vpc.subnet_id
  tags = {
    Name = local.name_prefix
  }
}

module "vpc" {
  source = "./modules/vpc"
  cidr   = var.region
}

output "web_ip" {
  value = aws_instance.web.public_ip
}
"#;

fn parse_hcl(source: &str) -> tree_sitter::Tree {
    let language: tree_sitter::Language = tree_sitter_hcl::LANGUAGE.into();
    let mut parser = Parser::new();
    parser.set_language(&language).expect("configure parser");
    parser.parse(source, None).expect("parse source")
}

fn contained_texts(
    source: &str,
    definition_capture_name: &str,
    inner_capture_name: &str,
) -> Vec<String> {
    let language: tree_sitter::Language = tree_sitter_hcl::LANGUAGE.into();
    let tree = parse_hcl(source);
    let query = Query::new(&language, HCL_TAGS_QUERY).expect("compile query");
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
    let mut texts = Vec::new();

    while let Some(query_match) = matches.next() {
        let has_definition = query_match
            .captures
            .iter()
            .any(|capture| capture_names[capture.index as usize] == definition_capture_name);

        if has_definition {
            texts.extend(query_match.captures.iter().filter_map(|capture| {
                if capture_names[capture.index as usize] == inner_capture_name {
                    Some(
                        capture
                            .node
                            .utf8_text(source.as_bytes())
                            .expect("capture text")
                            .to_owned(),
                    )
                } else {
                    None
                }
            }));
        }
    }

    texts
}

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

#[test]
fn hcl_fixture_parses_without_errors() {
    let tree = parse_hcl(TERRAFORM_FIXTURE);
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn hcl_tags_query_captures_labeled_blocks() {
    assert_eq!(
        contained_texts(TERRAFORM_FIXTURE, "definition.resource", "resource.type"),
        [r#""aws_instance""#]
    );
    assert_eq!(
        contained_texts(TERRAFORM_FIXTURE, "definition.resource", "resource.name"),
        [r#""web""#]
    );
    assert_eq!(
        contained_texts(TERRAFORM_FIXTURE, "definition.data", "resource.type"),
        [r#""aws_ami""#]
    );
    assert_eq!(
        contained_texts(TERRAFORM_FIXTURE, "definition.data", "resource.name"),
        [r#""ubuntu""#]
    );
    assert_eq!(
        contained_texts(TERRAFORM_FIXTURE, "definition.module", "resource.name"),
        [r#""vpc""#]
    );
    assert_eq!(
        contained_texts(TERRAFORM_FIXTURE, "definition.variable", "resource.name"),
        [r#""region""#, r#""env""#]
    );
    assert_eq!(
        contained_texts(TERRAFORM_FIXTURE, "definition.output", "resource.name"),
        [r#""web_ip""#]
    );
}

#[test]
fn hcl_tags_query_explodes_locals_attributes() {
    assert_eq!(
        contained_texts(TERRAFORM_FIXTURE, "definition.local", "name"),
        ["name_prefix", "port"]
    );
}

#[test]
fn terraform_and_hcl_paths_route_to_shared_grammar_languages() {
    let terraform = Language::from_path(Path::new("main.tf"));
    assert!(terraform.is_some(), "main.tf must route to a language");
    let hcl = Language::from_path(Path::new("terragrunt.hcl"));
    assert!(hcl.is_some(), "terragrunt.hcl must route to a language");
    assert_ne!(
        terraform, hcl,
        "tf and hcl must stay distinct variants over the shared grammar"
    );
    assert!(
        Language::from_path(Path::new("main.tf.json")).is_none(),
        ".tf.json is JSON syntax and out of scope"
    );

    let extensions: BTreeSet<_> = all_supported_extensions().into_iter().collect();
    for extension in ["tf", "hcl"] {
        assert!(
            extensions.contains(extension),
            "supported extensions should include {extension}"
        );
    }
}

#[test]
fn terraform_extractor_builds_address_symbols() {
    let facts = build_fixture(&[("main.tf", TERRAFORM_FIXTURE)]);
    let has_node = |kind: NodeKind, label: &str| {
        facts
            .nodes
            .iter()
            .any(|node| node.kind == kind && node.label == label)
    };

    assert!(has_node(NodeKind::Resource, "aws_instance.web"));
    assert!(has_node(NodeKind::Resource, "data.aws_ami.ubuntu"));
    assert!(has_node(NodeKind::Resource, "module.vpc"));
    assert!(has_node(NodeKind::Constant, "var.region"));
    assert!(has_node(NodeKind::Constant, "var.env"));
    assert!(has_node(NodeKind::Constant, "output.web_ip"));
    assert!(has_node(NodeKind::Constant, "local.name_prefix"));
    assert!(has_node(NodeKind::Constant, "local.port"));

    for bare in ["aws_instance", "web", "aws_ami", "ubuntu", "vpc", "region"] {
        assert!(
            !facts.nodes.iter().any(|node| node.label == bare),
            "bare label `{bare}` must not become a symbol; addresses are the identity"
        );
    }
}

#[test]
fn hcl_extension_shares_the_terraform_address_vocabulary() {
    let source = "variable \"cluster\" {}\n\nlocals {\n  retries = 3\n}\n";
    let facts = build_fixture(&[("pipeline.hcl", source)]);
    let has_node = |kind: NodeKind, label: &str| {
        facts
            .nodes
            .iter()
            .any(|node| node.kind == kind && node.label == label)
    };

    assert!(has_node(NodeKind::Constant, "var.cluster"));
    assert!(has_node(NodeKind::Constant, "local.retries"));
}

#[test]
fn duplicate_addresses_in_distinct_modules_get_distinct_stable_ids() {
    let module_source = "resource \"aws_instance\" \"web\" {}\n";
    let facts = build_fixture(&[
        ("modules/a/main.tf", module_source),
        ("modules/b/main.tf", module_source),
    ]);

    let stable_keys: Vec<_> = facts
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Resource && node.label == "aws_instance.web")
        .map(|node| node.stable_key.clone())
        .collect();
    assert_eq!(stable_keys.len(), 2, "one Resource node per module");
    assert_ne!(
        stable_keys[0], stable_keys[1],
        "stable identity must incorporate the module directory via relative_path"
    );
}

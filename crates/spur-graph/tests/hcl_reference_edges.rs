use std::fs;

use pretty_assertions::assert_eq;
use spur_graph::{build_facts, GraphEdge, GraphEdgeKind, RelationKind};

const REFERENCE_FIXTURE: &str = r#"
variable "env" {}

locals {
  name_prefix = "spur-${var.env}"
}

data "aws_ami" "ubuntu" {
  most_recent = true
}

resource "aws_instance" "seed" {}

resource "aws_instance" "web" {
  ami       = data.aws_ami.ubuntu.id
  name      = local.name_prefix
  subnet_id = module.vpc.subnet_ids[0]
  first_id  = aws_instance.seed[0].id
}

module "vpc" {
  source = "./modules/vpc"
}

output "web_ip" {
  value = aws_instance.web.public_ip
}
"#;

const RESERVED_ROOT_FIXTURE: &str = r#"
resource "aws_instance" "seed" {}

resource "aws_instance" "counted" {
  count    = 2
  ami      = count.index
  each_ref = each.value
  self_ref = self.arn
  cwd      = path.module
  ws       = terraform.workspace
  ids      = [for s in aws_instance.seed : s.id]
  bare     = string
}

provider "aws" {
  alias = "west"
}

resource "aws_instance" "aliased" {
  provider = aws.west
}
"#;

fn build_fixture(source: &str) -> spur_graph::extract::GraphFacts {
    let dir = tempfile::tempdir().expect("tempdir");
    fs::write(dir.path().join("main.tf"), source).expect("write main.tf");
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
        .unwrap_or_else(|| {
            panic!(
                "missing references edge for `{target}`; references present: {:?}",
                facts
                    .edges
                    .iter()
                    .filter(|edge| edge.relation == RelationKind::References)
                    .map(|edge| edge.target_label.clone())
                    .collect::<Vec<_>>()
            )
        })
}

#[test]
fn hcl_reference_edges_truncate_to_canonical_addresses() {
    let facts = build_fixture(REFERENCE_FIXTURE);

    for target in [
        "var.env",
        "local.name_prefix",
        "data.aws_ami.ubuntu",
        "module.vpc",
        "aws_instance.seed",
        "aws_instance.web",
    ] {
        let edge = reference_edge(&facts, target);
        assert_eq!(
            edge.edge_kind,
            Some(GraphEdgeKind::ReferencesAddress),
            "`{target}` must carry the address edge kind"
        );
    }

    for over_long in [
        "data.aws_ami.ubuntu.id",
        "module.vpc.subnet_ids",
        "aws_instance.web.public_ip",
        "aws_instance.seed.id",
    ] {
        assert!(
            !facts
                .edges
                .iter()
                .any(|edge| edge.target_label.as_deref() == Some(over_long)),
            "`{over_long}` must be truncated to its canonical address"
        );
    }
}

#[test]
fn hcl_reference_edges_skip_reserved_roots_and_bare_identifiers() {
    let facts = build_fixture(RESERVED_ROOT_FIXTURE);

    for reserved in ["count", "each", "self", "path", "terraform", "provider"] {
        assert!(
            !facts.edges.iter().any(|edge| {
                edge.relation == RelationKind::References
                    && edge.target_label.as_deref().is_some_and(|label| {
                        label == reserved || label.starts_with(&format!("{reserved}."))
                    })
            }),
            "reserved root `{reserved}` must not emit reference edges"
        );
    }

    for bare in ["string", "s"] {
        assert!(
            !facts.edges.iter().any(|edge| {
                edge.relation == RelationKind::References
                    && edge.target_label.as_deref() == Some(bare)
            }),
            "single bare identifier `{bare}` must not emit a reference edge"
        );
    }
}

#[test]
fn hcl_loop_var_and_provider_alias_refs_stay_unresolved_evidence() {
    let facts = build_fixture(RESERVED_ROOT_FIXTURE);

    for target in ["s.id", "aws.west"] {
        let edge = reference_edge(&facts, target);
        assert_eq!(edge.edge_kind, Some(GraphEdgeKind::ReferencesAddress));
        assert_eq!(
            edge.target_node_id, None,
            "`{target}` matches the address shape but must stay unresolved evidence"
        );
    }
}

#[test]
fn hcl_emits_no_call_edges() {
    let facts = build_fixture(REFERENCE_FIXTURE);
    assert!(
        facts
            .edges
            .iter()
            .all(|edge| edge.relation != RelationKind::Calls),
        "terraform functions are builtins; the hcl family must not emit a call channel"
    );
}

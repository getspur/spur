use tree_sitter::{Language, Node};

use crate::extract::tree_sitter::{CaptureHit, FactBuilder, PendingEdge};
use crate::{FileId, NodeId, NodeKind, RelationKind};

pub(crate) struct LanguageConfig {
    pub(crate) language: Language,
    pub(crate) queries: &'static [(&'static str, &'static str)],
}

#[derive(Debug, Clone)]
pub(crate) struct DefinitionBinding<'tree> {
    node: Node<'tree>,
    node_id: NodeId,
    fqn: String,
}

pub(crate) fn rust_config() -> LanguageConfig {
    LanguageConfig {
        language: tree_sitter_rust::LANGUAGE.into(),
        queries: &[
            ("tags", include_str!("../../queries/rust/tags.scm")),
            (
                "spur-edges",
                include_str!("../../queries/rust/spur-edges.scm"),
            ),
        ],
    }
}

pub(crate) fn emit_rust_definitions<'tree>(
    builder: &mut FactBuilder<'_>,
    relative_path: &str,
    file_id: FileId,
    file_node_id: NodeId,
    source: &str,
    captures: &[CaptureHit<'tree>],
) -> Vec<DefinitionBinding<'tree>> {
    let mut definitions: Vec<_> = captures
        .iter()
        .filter_map(|capture| {
            rust_definition_kind(capture.name.as_str(), capture.node)
                .map(|kind| (kind, capture.node))
        })
        .collect();

    definitions
        .sort_by_key(|(kind, node)| (node.start_byte(), node.end_byte(), definition_rank(*kind)));
    definitions.dedup_by(|left, right| {
        left.1.start_byte() == right.1.start_byte()
            && left.1.end_byte() == right.1.end_byte()
            && left.1.kind() == right.1.kind()
    });

    let mut bindings: Vec<DefinitionBinding<'tree>> = Vec::new();
    for (kind, node) in definitions {
        let Some(label) = rust_definition_label(kind, node, source) else {
            continue;
        };
        let parent = nearest_parent(file_node_id, &bindings, node);
        let fqn = scoped_name(parent.fqn.as_deref().unwrap_or(""), &label);
        let node_id = builder.add_node(relative_path, label, fqn.clone(), kind, file_id, node);
        builder.add_edge(parent.node_id, node_id, RelationKind::Contains);
        bindings.push(DefinitionBinding { node, node_id, fqn });
    }
    bindings
}

pub(crate) fn emit_rust_edges(
    builder: &mut FactBuilder<'_>,
    file_node_id: NodeId,
    source: &str,
    definitions: &[DefinitionBinding<'_>],
    captures: &[CaptureHit<'_>],
) {
    for capture in captures {
        match capture.name.as_str() {
            "import.use_declaration" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                for imported in imported_names(child_text(capture.node, source)) {
                    builder.pending_edges.push(PendingEdge {
                        source: source_id,
                        target_name: imported,
                        relation: RelationKind::Imports,
                    });
                }
            }
            "call.call_expression" => {
                let source_id = nearest_parent(file_node_id, definitions, capture.node).node_id;
                if let Some(function) = capture.node.child_by_field_name("function") {
                    let callee = terminal_symbol_name(child_text(function, source));
                    if !callee.is_empty() {
                        builder.pending_edges.push(PendingEdge {
                            source: source_id,
                            target_name: callee,
                            relation: RelationKind::Calls,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

fn rust_definition_kind(capture_name: &str, node: Node<'_>) -> Option<NodeKind> {
    match capture_name {
        "definition.module" => Some(NodeKind::Module),
        "definition.function" if !has_impl_ancestor(node) => Some(NodeKind::Function),
        "definition.method" | "definition.function" => Some(NodeKind::Method),
        "definition.struct" => Some(NodeKind::Struct),
        "definition.enum" => Some(NodeKind::Enum),
        "definition.trait" => Some(NodeKind::Trait),
        "definition.impl" => Some(NodeKind::Impl),
        _ => None,
    }
}

fn rust_definition_label(kind: NodeKind, node: Node<'_>, source: &str) -> Option<String> {
    if kind == NodeKind::Impl {
        return named_child_text(node, "type", source)
            .or_else(|| impl_type_from_text(child_text(node, source)));
    }
    named_child_text(node, "name", source)
}

fn nearest_parent<'a, 'tree>(
    file_node_id: NodeId,
    definitions: &'a [DefinitionBinding<'tree>],
    node: Node<'_>,
) -> Parent<'a> {
    definitions
        .iter()
        .rev()
        .find(|definition| contains(definition.node, node))
        .map(|definition| Parent {
            node_id: definition.node_id,
            fqn: Some(&definition.fqn),
        })
        .unwrap_or(Parent {
            node_id: file_node_id,
            fqn: None,
        })
}

#[derive(Debug, Clone, Copy)]
struct Parent<'a> {
    node_id: NodeId,
    fqn: Option<&'a str>,
}

fn contains(parent: Node<'_>, child: Node<'_>) -> bool {
    parent.start_byte() <= child.start_byte() && child.end_byte() <= parent.end_byte()
}

fn has_impl_ancestor(mut node: Node<'_>) -> bool {
    while let Some(parent) = node.parent() {
        if parent.kind() == "impl_item" {
            return true;
        }
        node = parent;
    }
    false
}

fn definition_rank(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Module => 0,
        NodeKind::Struct => 1,
        NodeKind::Enum => 2,
        NodeKind::Trait => 3,
        NodeKind::Impl => 4,
        NodeKind::Method => 5,
        NodeKind::Function => 6,
        _ => 7,
    }
}

fn named_child_text(node: Node<'_>, field_name: &str, source: &str) -> Option<String> {
    node.child_by_field_name(field_name)
        .map(|child| child_text(child, source).trim().to_string())
        .filter(|text| !text.is_empty())
}

fn child_text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.start_byte()..node.end_byte()]
}

fn imported_names(text: &str) -> Vec<String> {
    text.trim()
        .trim_start_matches("use")
        .trim_end_matches(';')
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|part| !part.is_empty())
        .filter(|part| !matches!(*part, "crate" | "self" | "super" | "pub" | "as"))
        .map(ToString::to_string)
        .collect()
}

fn terminal_symbol_name(text: &str) -> String {
    text.rsplit(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .find(|part| !part.is_empty())
        .unwrap_or("")
        .to_string()
}

fn impl_type_from_text(text: &str) -> Option<String> {
    let after_impl = text.trim_start().strip_prefix("impl")?.trim_start();
    let type_part = after_impl
        .split('{')
        .next()
        .unwrap_or(after_impl)
        .split_whitespace()
        .last()?;
    Some(
        type_part
            .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
            .to_string(),
    )
    .filter(|text| !text.is_empty())
}

fn scoped_name(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}::{name}")
    }
}

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Context;

use crate::extract::GraphFacts;
use crate::validation::compute_anchor_hash;
use crate::{
    GraphFileArtifact, GraphIndexArtifact, GraphIndexHeader, GraphNode, GraphSymbolArtifact,
    NodeKind, RelationKind, SourceSpan,
};

pub const PHASE1_GRAPH_INDEX_VERSION: &str = "spur-graph-phase2";

pub fn artifact_from_facts(
    facts: &GraphFacts,
    worktree_root: &Path,
) -> anyhow::Result<GraphIndexArtifact> {
    let spans_by_id: HashMap<_, _> = facts
        .spans
        .iter()
        .map(|span| (span.span_id, span))
        .collect();
    let nodes_by_id: HashMap<_, _> = facts
        .nodes
        .iter()
        .map(|node| (node.node_id, node))
        .collect();

    let mut files = Vec::new();
    let mut symbols = Vec::new();
    for node in &facts.nodes {
        let Some(span_id) = node.source_span_id else {
            continue;
        };
        let Some(span) = spans_by_id.get(&span_id).copied() else {
            continue;
        };

        match node.kind {
            NodeKind::File => files.push(GraphFileArtifact {
                stable_file_id: node.stable_key.clone(),
                file_path: node.label.clone(),
            }),
            NodeKind::Module
            | NodeKind::Function
            | NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Impl
            | NodeKind::Trait
            | NodeKind::Enum
            | NodeKind::Method
            | NodeKind::TypeAlias
            | NodeKind::Section => {
                let file_path = file_path_for_span(facts, span).unwrap_or_default();
                let anchor_hash = anchor_hash(worktree_root, &file_path, span);
                symbols.push(GraphSymbolArtifact {
                    stable_symbol_id: node.stable_key.clone(),
                    file_path,
                    byte_range: [span.start_byte as usize, span.end_byte as usize],
                    line_range: [span.start_line as usize, span.end_line as usize],
                    entity_name: symbol_entity_name(&node.label),
                    symbol_kind: symbol_kind(node.kind).to_string(),
                    anchor_hash,
                    enclosing_scope: enclosing_scope(facts, &nodes_by_id, node),
                });
            }
            _ => {}
        }
    }

    files.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    symbols.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.byte_range.cmp(&b.byte_range))
            .then(a.entity_name.cmp(&b.entity_name))
    });

    Ok(GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: PHASE1_GRAPH_INDEX_VERSION.to_string(),
        },
        files,
        symbols,
        diagnostics: Vec::new(),
    })
}

pub fn write_artifact(artifact: &GraphIndexArtifact, path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create `{}`", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(artifact).context("failed to encode graph artifact")?;
    fs::write(path, json).with_context(|| format!("failed to write `{}`", path.display()))
}

fn file_path_for_span(facts: &GraphFacts, span: &SourceSpan) -> Option<String> {
    facts
        .nodes
        .iter()
        .find(|node| node.kind == NodeKind::File && node.file_id == Some(span.file_id))
        .map(|node| node.label.clone())
}

fn anchor_hash(root: &Path, file_path: &str, span: &SourceSpan) -> String {
    let full_path = root.join(file_path);
    let content = match fs::read_to_string(&full_path) {
        Ok(content) => content,
        Err(error) => {
            tracing::warn!(
                file_path,
                full_path = %full_path.display(),
                error = %error,
                "spur-graph anchor hash fallback to sentinel: source read failed"
            );
            return "0".to_string();
        }
    };
    let start = span.start_byte as usize;
    let end = span.end_byte as usize;
    let slice = match content.get(start..end) {
        Some(slice) => slice,
        None => {
            tracing::warn!(
                file_path,
                full_path = %full_path.display(),
                span_start = start,
                span_end = end,
                "spur-graph anchor hash fallback to sentinel: UTF-8 boundary mismatch"
            );
            return "0".to_string();
        }
    };
    compute_anchor_hash(slice).to_string()
}

fn symbol_kind(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Module => "module",
        NodeKind::Function => "function",
        NodeKind::Class => "class",
        NodeKind::Interface => "interface",
        NodeKind::Struct => "struct",
        NodeKind::Impl => "impl",
        NodeKind::Trait => "trait",
        NodeKind::Enum => "enum",
        NodeKind::Method => "method",
        NodeKind::TypeAlias => "type_alias",
        NodeKind::Section => "section",
        _ => "symbol",
    }
}

fn symbol_entity_name(label: &str) -> String {
    label.strip_prefix("impl ").unwrap_or(label).to_string()
}

fn enclosing_scope(
    facts: &GraphFacts,
    nodes_by_id: &HashMap<crate::NodeId, &GraphNode>,
    node: &GraphNode,
) -> Option<String> {
    facts
        .edges
        .iter()
        .find(|edge| edge.relation == RelationKind::Contains && edge.target_node_id == node.node_id)
        .and_then(|edge| nodes_by_id.get(&edge.source_node_id).copied())
        .and_then(|parent| match parent.kind {
            NodeKind::File => None,
            NodeKind::Impl => Some(format!("impl {}", parent.label)),
            _ => Some(parent.label.clone()),
        })
}

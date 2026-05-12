use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use sha2::{Digest, Sha256};
use tree_sitter::{Node, Parser};

use crate::discovery::discover_rust_files;
use crate::extract::languages::rust_language;
use crate::extract::GraphFacts;
use crate::{
    Confidence, EdgeId, EvidenceId, FileId, GraphEdge, GraphNode, NodeId, NodeKind, RelationKind,
    RunId, SourceSpan, SpanId,
};

#[derive(Debug, Clone)]
struct PendingEdge {
    source: NodeId,
    target_name: String,
    relation: RelationKind,
}

#[derive(Debug)]
struct FactBuilder<'a> {
    root: &'a Path,
    facts: GraphFacts,
    next_node: u64,
    next_edge: u64,
    next_file: u64,
    next_span: u64,
    pending_edges: Vec<PendingEdge>,
    symbol_index: BTreeMap<String, Vec<NodeId>>,
}

impl<'a> FactBuilder<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            facts: GraphFacts::empty(),
            next_node: 1,
            next_edge: 1,
            next_file: 1,
            next_span: 1,
            pending_edges: Vec::new(),
            symbol_index: BTreeMap::new(),
        }
    }

    fn add_node(
        &mut self,
        relative_path: &str,
        label: String,
        fqn: String,
        kind: NodeKind,
        file_id: FileId,
        node: Node<'_>,
    ) -> NodeId {
        let node_id = NodeId(self.next_node);
        self.next_node += 1;
        let span_id = SpanId(self.next_span);
        self.next_span += 1;
        let range = node.range();
        let stable_key = stable_key(relative_path, &fqn, kind);

        self.facts.spans.push(SourceSpan {
            span_id,
            file_id,
            start_byte: range.start_byte as u32,
            end_byte: range.end_byte as u32,
            start_line: range.start_point.row as u32 + 1,
            end_line: range.end_point.row as u32 + 1,
        });
        self.facts.nodes.push(GraphNode {
            node_id,
            stable_key,
            label: label.clone(),
            kind,
            file_id: Some(file_id),
            source_span_id: Some(span_id),
            first_seen_run_id: RunId(1),
        });
        if kind != NodeKind::File {
            self.symbol_index.entry(label).or_default().push(node_id);
        }
        node_id
    }

    fn add_edge(&mut self, source: NodeId, target: NodeId, relation: RelationKind) {
        if self.facts.edges.iter().any(|edge| {
            edge.source_node_id == source
                && edge.target_node_id == target
                && edge.relation == relation
        }) {
            return;
        }
        let edge_id = EdgeId(self.next_edge);
        self.next_edge += 1;
        self.facts.edges.push(GraphEdge {
            edge_id,
            source_node_id: source,
            target_node_id: target,
            relation,
            confidence: Confidence::Heuristic,
            confidence_score: match relation {
                RelationKind::Contains => 1.0,
                RelationKind::Calls | RelationKind::Imports => 0.8,
                _ => 0.5,
            },
            evidence_id: EvidenceId(edge_id.get()),
            directed: true,
        });
    }

    fn resolve_pending_edges(&mut self) {
        let mut by_label: HashMap<String, NodeId> = HashMap::new();
        for (label, ids) in &self.symbol_index {
            if let Some(id) = ids.first() {
                by_label.insert(label.clone(), *id);
            }
        }
        let pending = std::mem::take(&mut self.pending_edges);
        for edge in pending {
            if let Some(target) = by_label.get(&edge.target_name).copied() {
                if target != edge.source {
                    self.add_edge(edge.source, target, edge.relation);
                }
            }
        }
    }
}

pub fn extract_rust_worktree(root: &Path) -> anyhow::Result<GraphFacts> {
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", root.display()))?;
    let files = discover_rust_files(&root)?;
    extract_rust_files(&root, &files)
}

fn extract_rust_files(root: &Path, files: &[PathBuf]) -> anyhow::Result<GraphFacts> {
    let mut parser = Parser::new();
    let language = rust_language();
    parser
        .set_language(&language)
        .map_err(|err| anyhow!("failed to configure tree-sitter Rust parser: {err}"))?;

    let mut builder = FactBuilder::new(root);
    for path in files {
        let source = fs::read_to_string(path)
            .with_context(|| format!("failed to read Rust source `{}`", path.display()))?;
        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| anyhow!("tree-sitter failed to parse `{}`", path.display()))?;
        extract_file(&mut builder, path, &source, tree.root_node())?;
    }
    builder.resolve_pending_edges();
    Ok(builder.facts)
}

fn extract_file(
    builder: &mut FactBuilder<'_>,
    path: &Path,
    source: &str,
    root_node: Node<'_>,
) -> anyhow::Result<()> {
    let relative_path = relative_path(builder.root, path)?;
    let file_id = FileId(builder.next_file);
    builder.next_file += 1;
    let file_node = builder.add_node(
        &relative_path,
        relative_path.clone(),
        relative_path.clone(),
        NodeKind::File,
        file_id,
        root_node,
    );
    walk_items(
        builder,
        source,
        &relative_path,
        file_id,
        file_node,
        None,
        "",
        root_node,
    );
    Ok(())
}

fn walk_items(
    builder: &mut FactBuilder<'_>,
    source: &str,
    relative_path: &str,
    file_id: FileId,
    parent_id: NodeId,
    enclosing_scope: Option<String>,
    fqn_prefix: &str,
    node: Node<'_>,
) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "mod_item" => {
                if let Some(name) = named_child_text(child, "name", source) {
                    let fqn = scoped_name(fqn_prefix, &name);
                    let module_id = builder.add_node(
                        relative_path,
                        name.clone(),
                        fqn.clone(),
                        NodeKind::Module,
                        file_id,
                        child,
                    );
                    builder.add_edge(parent_id, module_id, RelationKind::Contains);
                    walk_items(
                        builder,
                        source,
                        relative_path,
                        file_id,
                        module_id,
                        Some(name),
                        &fqn,
                        child,
                    );
                }
            }
            "function_item" => {
                if let Some(name) = named_child_text(child, "name", source) {
                    let fqn = scoped_name(fqn_prefix, &name);
                    let kind = if enclosing_scope
                        .as_deref()
                        .is_some_and(|scope| scope.starts_with("impl "))
                    {
                        NodeKind::Method
                    } else {
                        NodeKind::Function
                    };
                    let function_id =
                        builder.add_node(relative_path, name, fqn, kind, file_id, child);
                    builder.add_edge(parent_id, function_id, RelationKind::Contains);
                    collect_calls(builder, source, function_id, child);
                }
            }
            "struct_item" => add_named_symbol(
                builder,
                source,
                relative_path,
                file_id,
                parent_id,
                fqn_prefix,
                child,
                NodeKind::Struct,
            ),
            "enum_item" => add_named_symbol(
                builder,
                source,
                relative_path,
                file_id,
                parent_id,
                fqn_prefix,
                child,
                NodeKind::Enum,
            ),
            "trait_item" => add_named_symbol(
                builder,
                source,
                relative_path,
                file_id,
                parent_id,
                fqn_prefix,
                child,
                NodeKind::Trait,
            ),
            "impl_item" => {
                let type_name = named_child_text(child, "type", source)
                    .or_else(|| impl_type_from_text(child_text(child, source)))
                    .unwrap_or_else(|| "unknown".to_string());
                let label = type_name;
                let impl_scope = format!("impl {label}");
                let fqn = scoped_name(fqn_prefix, &label);
                let impl_id = builder.add_node(
                    relative_path,
                    label.clone(),
                    fqn.clone(),
                    NodeKind::Impl,
                    file_id,
                    child,
                );
                builder.add_edge(parent_id, impl_id, RelationKind::Contains);
                walk_items(
                    builder,
                    source,
                    relative_path,
                    file_id,
                    impl_id,
                    Some(impl_scope),
                    &fqn,
                    child,
                );
            }
            "use_declaration" => {
                for imported in imported_names(child_text(child, source)) {
                    builder.pending_edges.push(PendingEdge {
                        source: parent_id,
                        target_name: imported,
                        relation: RelationKind::Imports,
                    });
                }
            }
            _ => walk_items(
                builder,
                source,
                relative_path,
                file_id,
                parent_id,
                enclosing_scope.clone(),
                fqn_prefix,
                child,
            ),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn add_named_symbol(
    builder: &mut FactBuilder<'_>,
    source: &str,
    relative_path: &str,
    file_id: FileId,
    parent_id: NodeId,
    fqn_prefix: &str,
    node: Node<'_>,
    kind: NodeKind,
) {
    if let Some(name) = named_child_text(node, "name", source) {
        let fqn = scoped_name(fqn_prefix, &name);
        let symbol_id = builder.add_node(relative_path, name, fqn, kind, file_id, node);
        builder.add_edge(parent_id, symbol_id, RelationKind::Contains);
    }
}

fn collect_calls(builder: &mut FactBuilder<'_>, source: &str, function_id: NodeId, node: Node<'_>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if child.kind() == "call_expression" {
            if let Some(function) = child.child_by_field_name("function") {
                let callee = terminal_symbol_name(child_text(function, source));
                if !callee.is_empty() {
                    builder.pending_edges.push(PendingEdge {
                        source: function_id,
                        target_name: callee,
                        relation: RelationKind::Calls,
                    });
                }
            }
        }
        collect_calls(builder, source, function_id, child);
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

fn stable_key(relative_path: &str, fqn: &str, kind: NodeKind) -> String {
    let mut hasher = Sha256::new();
    hasher.update(relative_path.as_bytes());
    hasher.update([0]);
    hasher.update(fqn.as_bytes());
    hasher.update([0]);
    hasher.update(format!("{kind:?}").as_bytes());
    let digest = hasher.finalize();
    format!(
        "{:016x}",
        u64::from_be_bytes(digest[..8].try_into().unwrap())
    )
}

fn relative_path(root: &Path, path: &Path) -> anyhow::Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("`{}` is outside `{}`", path.display(), root.display()))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

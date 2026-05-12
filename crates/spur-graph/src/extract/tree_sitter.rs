use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use sha2::{Digest, Sha256};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::discovery::discover_rust_files;
use crate::extract::languages::{emit_rust_definitions, emit_rust_edges, rust_config};
use crate::extract::GraphFacts;
use crate::{
    Confidence, EdgeId, EvidenceId, FileId, GraphEdge, GraphNode, NodeId, NodeKind, RelationKind,
    RunId, SourceSpan, SpanId,
};

#[derive(Debug, Clone)]
pub(crate) struct PendingEdge {
    pub(crate) source: NodeId,
    pub(crate) target_name: String,
    pub(crate) relation: RelationKind,
}

#[derive(Debug)]
pub(crate) struct FactBuilder<'a> {
    root: &'a Path,
    facts: GraphFacts,
    next_node: u64,
    next_edge: u64,
    next_file: u64,
    next_span: u64,
    pub(crate) pending_edges: Vec<PendingEdge>,
    symbol_index: BTreeMap<String, Vec<NodeId>>,
    edge_index: HashSet<(NodeId, NodeId, RelationKind)>,
}

#[derive(Debug, Clone)]
pub(crate) struct CaptureHit<'tree> {
    pub(crate) name: String,
    pub(crate) node: Node<'tree>,
}

struct CompiledQueries {
    tags: Query,
    spur_edges: Query,
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
            edge_index: HashSet::new(),
        }
    }

    pub(crate) fn add_node(
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

    pub(crate) fn add_edge(&mut self, source: NodeId, target: NodeId, relation: RelationKind) {
        if !self.edge_index.insert((source, target, relation)) {
            return;
        }
        let edge_id = EdgeId(self.next_edge);
        self.next_edge += 1;
        self.facts.edges.push(GraphEdge {
            edge_id,
            source_node_id: source,
            target_node_id: target,
            relation,
            confidence: match relation {
                RelationKind::Contains => Confidence::SyntaxExact,
                RelationKind::Calls | RelationKind::Imports => Confidence::Heuristic,
                _ => Confidence::Heuristic,
            },
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
    let config = rust_config();
    let mut parser = Parser::new();
    parser
        .set_language(&config.language)
        .map_err(|err| anyhow!("failed to configure tree-sitter Rust parser: {err}"))?;
    let queries = compile_queries(&config)?;

    let mut builder = FactBuilder::new(root);
    for path in files {
        let source = fs::read_to_string(path)
            .with_context(|| format!("failed to read Rust source `{}`", path.display()))?;
        let tree = parser
            .parse(&source, None)
            .ok_or_else(|| anyhow!("tree-sitter failed to parse `{}`", path.display()))?;
        extract_file(&mut builder, path, &source, tree.root_node(), &queries)?;
    }
    builder.resolve_pending_edges();
    Ok(builder.facts)
}

fn extract_file(
    builder: &mut FactBuilder<'_>,
    path: &Path,
    source: &str,
    root_node: Node<'_>,
    queries: &CompiledQueries,
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

    let tag_captures = run_query(&queries.tags, root_node, source);
    let definitions = emit_rust_definitions(
        builder,
        &relative_path,
        file_id,
        file_node,
        source,
        &tag_captures,
    );
    let edge_captures = run_query(&queries.spur_edges, root_node, source);
    emit_rust_edges(builder, file_node, source, &definitions, &edge_captures);
    Ok(())
}

fn compile_queries(
    config: &crate::extract::languages::LanguageConfig,
) -> anyhow::Result<CompiledQueries> {
    let mut tags = None;
    let mut spur_edges = None;
    for (name, source) in config.queries {
        let query = Query::new(&config.language, source)
            .with_context(|| format!("failed to compile Rust tree-sitter query `{name}`"))?;
        match *name {
            "tags" => tags = Some(query),
            "spur-edges" => spur_edges = Some(query),
            name => return Err(anyhow!("unknown Rust tree-sitter query name `{name}`")),
        }
    }
    Ok(CompiledQueries {
        tags: tags.context("missing Rust tags query")?,
        spur_edges: spur_edges.context("missing Rust SPUR edge query")?,
    })
}

fn run_query<'tree>(query: &Query, root_node: Node<'tree>, source: &str) -> Vec<CaptureHit<'tree>> {
    let capture_names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut captures = cursor.captures(query, root_node, source.as_bytes());
    let mut hits = Vec::new();
    while let Some((query_match, capture_index)) = captures.next() {
        let capture = query_match.captures[*capture_index];
        hits.push(CaptureHit {
            name: capture_names[capture.index as usize].to_string(),
            node: capture.node,
        });
    }
    hits
}

fn stable_key(relative_path: &str, fqn: &str, kind: NodeKind) -> String {
    let mut hasher = Sha256::new();
    hasher.update(relative_path.as_bytes());
    hasher.update([0]);
    hasher.update(fqn.as_bytes());
    hasher.update([0]);
    hasher.update(kind.discriminator().as_bytes());
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

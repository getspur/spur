use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context};
use sha2::{Digest, Sha256};
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::discovery::discover_files;
use crate::extract::languages::{
    all_supported_extensions, emit_definitions, emit_edges, language_registry, LanguageConfig,
    LanguageDescriptor,
};
use crate::extract::markdown::extract_markdown_file;
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

pub(crate) struct CompiledQueries {
    pub(crate) tags: Query,
    pub(crate) spur_edges: Option<Query>,
    pub(crate) inline_spur_edges: Option<Query>,
}

#[derive(Debug)]
pub(crate) struct LanguageFileGroup {
    pub(crate) label: &'static str,
    pub(crate) config: LanguageConfig,
    pub(crate) files: Vec<PathBuf>,
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

    pub(crate) fn root(&self) -> &'a Path {
        self.root
    }

    pub(crate) fn next_file_id(&mut self) -> u64 {
        let next = self.next_file;
        self.next_file += 1;
        next
    }

    pub(crate) fn add_file_node(
        &mut self,
        relative_path: &str,
        file_id: FileId,
        root_node: Node<'_>,
    ) -> NodeId {
        self.add_node(
            relative_path,
            relative_path.to_string(),
            relative_path.to_string(),
            NodeKind::File,
            file_id,
            root_node,
        )
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
                RelationKind::Calls | RelationKind::Imports | RelationKind::Links => {
                    Confidence::Heuristic
                }
                _ => Confidence::Heuristic,
            },
            confidence_score: match relation {
                RelationKind::Contains => 1.0,
                RelationKind::Calls | RelationKind::Imports | RelationKind::Links => 0.8,
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
        for node in &self.facts.nodes {
            if node.kind == NodeKind::File {
                by_label.entry(node.label.clone()).or_insert(node.node_id);
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

pub fn build_facts(root: &Path) -> anyhow::Result<(GraphFacts, BTreeMap<&'static str, usize>)> {
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", root.display()))?;
    let (groups, file_counts) = discover_language_groups(&root)?;
    let extract_groups: Vec<_> = groups
        .into_iter()
        .map(|group| (group.label, group.config, group.files))
        .collect();
    let facts = extract_files(&root, &extract_groups)?;
    Ok((facts, file_counts))
}

fn extract_files(
    root: &Path,
    groups: &[(&'static str, LanguageConfig, Vec<PathBuf>)],
) -> anyhow::Result<GraphFacts> {
    let mut parser = Parser::new();
    let mut builder = FactBuilder::new(root);
    let mut compiled_queries_cache: HashMap<&'static str, CompiledQueries> = HashMap::new();

    for (label, config, files) in groups {
        parser.set_language(&config.language).map_err(|err| {
            anyhow!("failed to configure tree-sitter parser for `{label}`: {err}")
        })?;
        if !compiled_queries_cache.contains_key(label) {
            compiled_queries_cache.insert(*label, compile_queries(config, label)?);
        }
        let queries = compiled_queries_cache
            .get(label)
            .ok_or_else(|| anyhow!("missing compiled query cache entry for `{label}`"))?;
        let mut markdown_inline_parser = if *label == "markdown" {
            let mut parser = Parser::new();
            if let Some(inline_language) = config.inline_language.as_ref() {
                parser.set_language(inline_language).map_err(|err| {
                    anyhow!("failed to configure markdown inline parser for `{label}`: {err}")
                })?;
            }
            Some(parser)
        } else {
            None
        };
        for path in files {
            let source = fs::read_to_string(path)
                .with_context(|| format!("failed to read source `{}`", path.display()))?;
            let tree = parser
                .parse(&source, None)
                .ok_or_else(|| anyhow!("tree-sitter failed to parse `{}`", path.display()))?;
            if *label == "markdown" {
                extract_markdown_file(
                    &mut builder,
                    config,
                    path,
                    &source,
                    tree.root_node(),
                    queries,
                    markdown_inline_parser.as_mut(),
                )?;
            } else {
                extract_file(
                    &mut builder,
                    config,
                    path,
                    &source,
                    tree.root_node(),
                    queries,
                )?;
            }
        }
    }
    builder.resolve_pending_edges();
    Ok(builder.facts)
}

fn extract_file(
    builder: &mut FactBuilder<'_>,
    config: &crate::extract::languages::LanguageConfig,
    path: &Path,
    source: &str,
    root_node: Node<'_>,
    queries: &CompiledQueries,
) -> anyhow::Result<()> {
    let relative_path = relative_path(builder.root(), path)?;
    let file_id = FileId(builder.next_file_id());
    let file_node = builder.add_file_node(&relative_path, file_id, root_node);

    let tag_captures = run_query(&queries.tags, root_node, source);
    let definitions = emit_definitions(
        config,
        builder,
        &relative_path,
        file_id,
        file_node,
        source,
        &tag_captures,
    );
    if let Some(spur_edges) = queries.spur_edges.as_ref() {
        let edge_captures = run_query(spur_edges, root_node, source);
        emit_edges(
            config,
            builder,
            file_node,
            source,
            &definitions,
            &edge_captures,
        );
    }
    Ok(())
}

fn compile_queries(
    config: &LanguageConfig,
    language_label: &str,
) -> anyhow::Result<CompiledQueries> {
    let mut tags = None;
    let mut spur_edges = None;
    let mut inline_spur_edges = None;
    for (name, source) in config.queries {
        match *name {
            "tags" => {
                tags = Some(Query::new(&config.language, source).with_context(|| {
                    format!("failed to compile tree-sitter query `{name}` for `{language_label}`")
                })?);
            }
            "spur-edges" => {
                spur_edges = Some(Query::new(&config.language, source).with_context(|| {
                    format!("failed to compile tree-sitter query `{name}` for `{language_label}`")
                })?);
            }
            "inline-spur-edges" => {
                let inline_language = config.inline_language.as_ref().ok_or_else(|| {
                    anyhow!(
                        "query `{name}` declared for `{language_label}` but inline language is not configured"
                    )
                })?;
                inline_spur_edges =
                    Some(Query::new(inline_language, source).with_context(|| {
                        format!(
                        "failed to compile inline tree-sitter query `{name}` for `{language_label}`"
                    )
                    })?);
            }
            name => {
                return Err(anyhow!(
                    "unknown tree-sitter query name `{name}` for `{language_label}`"
                ));
            }
        }
    }
    Ok(CompiledQueries {
        tags: tags.with_context(|| format!("missing `{language_label}` tags query"))?,
        spur_edges,
        inline_spur_edges,
    })
}

fn discover_language_groups(
    root: &Path,
) -> anyhow::Result<(Vec<LanguageFileGroup>, BTreeMap<&'static str, usize>)> {
    let allowed_extensions = all_supported_extensions();
    let files = discover_files(root, &allowed_extensions)?;
    let mut groups: BTreeMap<&'static str, (fn() -> LanguageConfig, Vec<PathBuf>)> =
        BTreeMap::new();
    let descriptors: &[LanguageDescriptor] = language_registry();
    for path in files {
        let Some(descriptor) = descriptors.iter().find(|d| (d.matcher)(&path)) else {
            continue;
        };
        groups
            .entry(descriptor.label)
            .or_insert_with(|| (descriptor.factory, Vec::new()))
            .1
            .push(path);
    }

    let file_counts = groups
        .iter()
        .map(|(label, (_, files))| (*label, files.len()))
        .collect();

    let language_groups = groups
        .into_iter()
        .map(|(label, (factory, files))| LanguageFileGroup {
            label,
            config: factory(),
            files,
        })
        .collect();

    Ok((language_groups, file_counts))
}

pub(crate) fn run_query<'tree>(
    query: &Query,
    root_node: Node<'tree>,
    source: &str,
) -> Vec<CaptureHit<'tree>> {
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

pub(crate) fn relative_path(root: &Path, path: &Path) -> anyhow::Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("`{}` is outside `{}`", path.display(), root.display()))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

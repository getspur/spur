use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str;

use anyhow::{anyhow, Context};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::discovery::discover_files;
use crate::extract::languages::{
    all_supported_extensions, emit_definitions, emit_edges, extracted_symbols, language_registry,
    Language, LanguageConfig, LanguageDescriptor,
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
    edge_index: HashSet<(NodeId, Option<NodeId>, RelationKind, Option<String>)>,
    stable_key_ordinals: HashMap<(String, String, &'static str), u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct CaptureHit<'tree> {
    pub(crate) name: String,
    pub(crate) node: Node<'tree>,
}

pub(crate) struct CompiledQueries {
    pub(crate) tags: Query,
    pub(crate) symbols: Query,
    pub(crate) spur_edges: Option<Query>,
    pub(crate) inline_spur_edges: Option<Query>,
}

#[derive(Debug)]
pub(crate) struct LanguageFileGroup {
    pub(crate) label: &'static str,
    pub(crate) language: Language,
    pub(crate) config: LanguageConfig,
    pub(crate) files: Vec<PathBuf>,
}

type GroupAccumulator = BTreeMap<&'static str, (Language, fn() -> LanguageConfig, Vec<PathBuf>)>;

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("invalid utf-8: {0}")]
    InvalidUtf8(str::Utf8Error),
    #[error("parser setup failed: {0}")]
    Setup(String),
    #[error("tree-sitter parse returned no tree")]
    NoTree,
    #[error("extraction failed: {0}")]
    Extraction(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSymbol {
    pub entity_name: String,
    pub symbol_kind: String,
    pub enclosing_scope: Option<String>,
    pub byte_range: [usize; 2],
    pub line_range: [usize; 2],
    pub anchor_hash: String,
    pub tokens: Vec<String>,
}

pub struct BytesExtractor {
    language: Language,
    config: LanguageConfig,
    parser: Parser,
    queries: CompiledQueries,
    markdown_inline_parser: Option<Parser>,
}

impl BytesExtractor {
    pub fn for_language(language: Language) -> Result<Self, ExtractError> {
        Self::new(language, language.config())
    }

    fn new(language: Language, config: LanguageConfig) -> Result<Self, ExtractError> {
        let mut parser = Parser::new();
        parser
            .set_language(&config.language)
            .map_err(|err| ExtractError::Setup(err.to_string()))?;
        let queries = compile_queries(&config, language)
            .map_err(|err| ExtractError::Setup(err.to_string()))?;
        let markdown_inline_parser = if language == Language::Markdown {
            let mut parser = Parser::new();
            if let Some(inline_language) = config.inline_language.as_ref() {
                parser
                    .set_language(inline_language)
                    .map_err(|err| ExtractError::Setup(err.to_string()))?;
            }
            Some(parser)
        } else {
            None
        };
        Ok(Self {
            language,
            config,
            parser,
            queries,
            markdown_inline_parser,
        })
    }

    pub fn extract(
        &mut self,
        _logical_path: &Path,
        bytes: &[u8],
    ) -> Result<Vec<ExtractedSymbol>, ExtractError> {
        let source = str::from_utf8(bytes).map_err(ExtractError::InvalidUtf8)?;
        let tree = self
            .parser
            .parse(source.as_bytes(), None)
            .ok_or(ExtractError::NoTree)?;
        let captures = run_query(&self.queries.symbols, tree.root_node(), source);
        Ok(extracted_symbols(&self.config, source, &captures))
    }

    pub(crate) fn extract_graph_facts(
        &mut self,
        builder: &mut FactBuilder<'_>,
        path: &Path,
        bytes: &[u8],
    ) -> Result<(), ExtractError> {
        let source = str::from_utf8(bytes).map_err(ExtractError::InvalidUtf8)?;
        let tree = self
            .parser
            .parse(source.as_bytes(), None)
            .ok_or(ExtractError::NoTree)?;
        if self.language == Language::Markdown {
            extract_markdown_file(
                builder,
                &self.config,
                path,
                source,
                tree.root_node(),
                &self.queries,
                self.markdown_inline_parser.as_mut(),
            )
        } else {
            extract_file_from_tree(
                builder,
                &self.config,
                path,
                source,
                tree.root_node(),
                &self.queries,
            )
        }
        .map_err(|err| ExtractError::Extraction(err.to_string()))
    }
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
            stable_key_ordinals: HashMap::new(),
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
        let kind_discriminator = kind.discriminator();
        let ordinal = {
            let key = (relative_path.to_string(), label.clone(), kind_discriminator);
            let count = self.stable_key_ordinals.entry(key).or_insert(0);
            *count += 1;
            *count
        };
        let stable_key = stable_key(relative_path, &fqn, kind, ordinal);

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

    pub(crate) fn add_edge(
        &mut self,
        source: NodeId,
        target: Option<NodeId>,
        relation: RelationKind,
        target_label: Option<String>,
    ) {
        if !self
            .edge_index
            .insert((source, target, relation, target_label.clone()))
        {
            return;
        }
        let edge_id = EdgeId(self.next_edge);
        self.next_edge += 1;
        self.facts.edges.push(GraphEdge {
            edge_id,
            source_node_id: source,
            target_node_id: target,
            relation,
            target_label,
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
            change_kind: None,
        });
    }

    fn resolve_pending_edges(&mut self) {
        let mut by_label: HashMap<String, NodeId> = HashMap::new();
        let mut ambiguous_labels: HashSet<String> = HashSet::new();
        for (label, ids) in &self.symbol_index {
            if let Some(id) = ids.first() {
                if ids.len() > 1 {
                    tracing::warn!(
                        label = %label,
                        candidates = ids.len(),
                        "spur-graph: ambiguous symbol; edges to this label resolve to first occurrence only"
                    );
                    ambiguous_labels.insert(label.clone());
                }
                by_label.insert(label.clone(), *id);
            }
        }
        for node in &self.facts.nodes {
            if node.kind == NodeKind::File {
                by_label.entry(node.label.clone()).or_insert(node.node_id);
            }
        }
        let pending = std::mem::take(&mut self.pending_edges);
        let mut ambiguous_hits = 0usize;
        for edge in pending {
            if let Some(target) = by_label.get(&edge.target_name).copied() {
                if ambiguous_labels.contains(&edge.target_name) {
                    ambiguous_hits += 1;
                }
                if target != edge.source {
                    self.add_edge(
                        edge.source,
                        Some(target),
                        edge.relation,
                        Some(edge.target_name),
                    );
                }
            } else {
                self.add_edge(edge.source, None, edge.relation, Some(edge.target_name));
            }
        }
        if ambiguous_hits > 0 {
            tracing::warn!(
                "spur-graph: {} pending edges hit ambiguous symbol labels",
                ambiguous_hits
            );
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
        .map(|group| (group.language, group.label, group.config, group.files))
        .collect();
    let facts = extract_files(&root, extract_groups)?;
    Ok((facts, file_counts))
}

pub fn build_facts_for_paths(root: &Path, files: &[PathBuf]) -> anyhow::Result<GraphFacts> {
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", root.display()))?;
    let groups = language_groups_for_paths(&root, files)?;
    let extract_groups: Vec<_> = groups
        .into_iter()
        .map(|group| (group.language, group.label, group.config, group.files))
        .collect();
    extract_files(&root, extract_groups)
}

fn extract_files(
    root: &Path,
    groups: Vec<(Language, &'static str, LanguageConfig, Vec<PathBuf>)>,
) -> anyhow::Result<GraphFacts> {
    let mut builder = FactBuilder::new(root);

    for (language, label, config, files) in groups {
        let mut extractor = BytesExtractor::new(language, config).map_err(|err| {
            anyhow!("failed to configure tree-sitter parser for `{label}`: {err}")
        })?;
        for path in files {
            let source_bytes = match fs::read(&path) {
                Ok(source) => source,
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "spur-graph: skipping file (read failed)"
                    );
                    continue;
                }
            };
            if let Err(err) = extractor.extract_graph_facts(&mut builder, &path, &source_bytes) {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "spur-graph: skipping file (extraction failed)"
                );
            }
        }
    }
    builder.resolve_pending_edges();
    Ok(builder.facts)
}

fn extract_file_from_tree(
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

fn compile_queries(config: &LanguageConfig, language: Language) -> anyhow::Result<CompiledQueries> {
    let language_label = language.label();
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
    let symbols_source = match language {
        Language::Rust => include_str!("../../queries/rust/symbols.scm"),
        Language::Python => include_str!("../../queries/python/symbols.scm"),
        Language::TypeScript | Language::Tsx => {
            include_str!("../../queries/typescript/symbols.scm")
        }
        Language::Markdown => include_str!("../../queries/markdown/symbols.scm"),
    };
    let symbols = Query::new(&config.language, symbols_source).with_context(|| {
        format!("failed to compile tree-sitter query `symbols` for `{language_label}`")
    })?;
    Ok(CompiledQueries {
        tags: tags.with_context(|| format!("missing `{language_label}` tags query"))?,
        symbols,
        spur_edges,
        inline_spur_edges,
    })
}

fn discover_language_groups(
    root: &Path,
) -> anyhow::Result<(Vec<LanguageFileGroup>, BTreeMap<&'static str, usize>)> {
    let allowed_extensions = all_supported_extensions();
    let files = discover_files(root, &allowed_extensions)?;
    let mut groups: GroupAccumulator = BTreeMap::new();
    let descriptors: &[LanguageDescriptor] = language_registry();
    for path in files {
        let Some(descriptor) = descriptors.iter().find(|d| (d.matcher)(&path)) else {
            continue;
        };
        groups
            .entry(descriptor.label)
            .or_insert_with(|| (descriptor.language, descriptor.factory, Vec::new()))
            .2
            .push(path);
    }

    let file_counts = groups
        .iter()
        .map(|(label, (_, _, files))| (*label, files.len()))
        .collect();

    let language_groups = groups
        .into_iter()
        .map(|(label, (language, factory, files))| LanguageFileGroup {
            label,
            language,
            config: factory(),
            files,
        })
        .collect();

    Ok((language_groups, file_counts))
}

fn language_groups_for_paths(
    root: &Path,
    files: &[PathBuf],
) -> anyhow::Result<Vec<LanguageFileGroup>> {
    let mut groups: GroupAccumulator = BTreeMap::new();
    let descriptors: &[LanguageDescriptor] = language_registry();
    for path in files {
        let full_path = if path.is_absolute() {
            path.clone()
        } else {
            root.join(path)
        };
        if !full_path.is_file() {
            continue;
        }
        let Some(descriptor) = descriptors.iter().find(|d| (d.matcher)(&full_path)) else {
            continue;
        };
        groups
            .entry(descriptor.label)
            .or_insert_with(|| (descriptor.language, descriptor.factory, Vec::new()))
            .2
            .push(full_path);
    }

    Ok(groups
        .into_iter()
        .map(|(label, (language, factory, files))| LanguageFileGroup {
            label,
            language,
            config: factory(),
            files,
        })
        .collect())
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

fn stable_key(relative_path: &str, fqn: &str, kind: NodeKind, ordinal: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(relative_path.as_bytes());
    hasher.update([0]);
    hasher.update(fqn.as_bytes());
    hasher.update([0]);
    hasher.update(kind.discriminator().as_bytes());
    hasher.update([0]);
    hasher.update(ordinal.to_le_bytes());
    let digest = hasher.finalize();
    let mut prefix = [0; 8];
    prefix.copy_from_slice(&digest[..8]);
    format!("{:016x}", u64::from_be_bytes(prefix))
}

pub(crate) fn relative_path(root: &Path, path: &Path) -> anyhow::Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("`{}` is outside `{}`", path.display(), root.display()))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

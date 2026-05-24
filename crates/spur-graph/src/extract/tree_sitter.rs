use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str;

use anyhow::{anyhow, Context};
use thiserror::Error;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::discovery::discover_files;
use crate::extract::languages::{
    all_supported_extensions, emit_definitions, emit_edges, emit_rust_dyn_trait_edges,
    extracted_symbols, language_registry, Language, LanguageConfig, LanguageDescriptor,
};
use crate::extract::markdown::extract_markdown_file;
use crate::extract::mcp_tools::emit_mcp_tools;
use crate::extract::GraphFacts;
use crate::{
    Confidence, EdgeId, EvidenceId, FileId, GraphEdge, GraphEdgeKind, GraphNode, NodeId, NodeKind,
    RelationKind, RunId, SourceSpan, SpanId,
};

#[derive(Debug, Clone)]
pub(crate) struct PendingEdge {
    pub(crate) source: NodeId,
    pub(crate) target_name: String,
    pub(crate) relation: RelationKind,
    pub(crate) edge_kind: Option<GraphEdgeKind>,
    pub(crate) origin: CallOrigin,
    #[allow(dead_code)]
    pub(crate) receiver_text: Option<String>,
    #[allow(dead_code)]
    pub(crate) scope_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CallOrigin {
    Expression,
    MacroBody,
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
    edge_index: HashSet<EdgeDedupKey>,
    qualified_symbol_index: BTreeMap<String, Vec<NodeId>>,
}

#[derive(Debug, Clone)]
pub(crate) struct CaptureHit<'tree> {
    pub(crate) name: String,
    pub(crate) node: Node<'tree>,
    pub(crate) pattern_index: usize,
    pub(crate) match_index: usize,
}

pub(crate) struct CompiledQueries {
    pub(crate) tags: Query,
    pub(crate) symbols: Query,
    pub(crate) spur_edges: Option<Query>,
    pub(crate) inline_spur_edges: Option<Query>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymbolQueryPolicy {
    ReuseTags,
    Dedicated(&'static str),
}

impl SymbolQueryPolicy {
    fn source(self, tags_source: &'static str) -> &'static str {
        match self {
            Self::ReuseTags => tags_source,
            Self::Dedicated(source) => source,
        }
    }
}

fn symbol_query_policy(language: Language) -> SymbolQueryPolicy {
    match language {
        Language::Rust | Language::TypeScript | Language::Tsx | Language::Cpp => {
            SymbolQueryPolicy::ReuseTags
        }
        Language::Python => {
            SymbolQueryPolicy::Dedicated(include_str!("../../queries/python/symbols.scm"))
        }
        Language::Markdown => {
            SymbolQueryPolicy::Dedicated(include_str!("../../queries/markdown/symbols.scm"))
        }
    }
}

#[derive(Debug)]
pub(crate) struct LanguageFileGroup {
    pub(crate) label: &'static str,
    pub(crate) language: Language,
    pub(crate) config: LanguageConfig,
    pub(crate) files: Vec<PathBuf>,
}

type GroupAccumulator = BTreeMap<&'static str, (Language, fn() -> LanguageConfig, Vec<PathBuf>)>;
type EdgeDedupKey = (
    NodeId,
    Option<NodeId>,
    RelationKind,
    Option<String>,
    Option<GraphEdgeKind>,
);

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
                self.language.label(),
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
            qualified_symbol_index: BTreeMap::new(),
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
        let impl_identity;
        let identity_fqn = if kind == NodeKind::Impl {
            impl_identity = impl_identity_fqn(&fqn);
            impl_identity.as_str()
        } else {
            &fqn
        };
        let stable_key = crate::identity::stable_symbol_id_for(
            relative_path,
            identity_fqn,
            kind,
            range.start_byte as u64,
        );

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
        if !matches!(kind, NodeKind::File | NodeKind::McpTool) {
            self.symbol_index.entry(label).or_default().push(node_id);
            self.qualified_symbol_index
                .entry(fqn)
                .or_default()
                .push(node_id);
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
        self.add_edge_with_kind(source, target, relation, target_label, None);
    }

    pub(crate) fn add_edge_with_kind(
        &mut self,
        source: NodeId,
        target: Option<NodeId>,
        relation: RelationKind,
        target_label: Option<String>,
        edge_kind: Option<GraphEdgeKind>,
    ) {
        let (confidence, confidence_score) = confidence_for_edge(relation, edge_kind);
        self.add_edge_with_metadata(
            source,
            target,
            relation,
            target_label,
            edge_kind,
            confidence,
            confidence_score,
            None,
        );
    }

    fn add_pending_edge(&mut self, edge: &PendingEdge, target: Option<NodeId>) {
        let (confidence, confidence_score, bind_method) = metadata_for_pending_edge(edge, target);
        self.add_edge_with_metadata(
            edge.source,
            target,
            edge.relation,
            Some(edge.target_name.clone()),
            edge.edge_kind,
            confidence,
            confidence_score,
            bind_method,
        );
    }

    fn add_edge_with_metadata(
        &mut self,
        source: NodeId,
        target: Option<NodeId>,
        relation: RelationKind,
        target_label: Option<String>,
        edge_kind: Option<GraphEdgeKind>,
        confidence: Confidence,
        confidence_score: f32,
        bind_method: Option<&'static str>,
    ) {
        if !self
            .edge_index
            .insert((source, target, relation, target_label.clone(), edge_kind))
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
            confidence,
            confidence_score,
            edge_kind,
            bind_method: bind_method.map(str::to_string),
            evidence_id: EvidenceId(edge_id.get()),
            directed: true,
            change_kind: None,
        });
    }

    fn resolve_pending_edges(&mut self) {
        let mut singleton_symbols_by_label: HashMap<String, NodeId> = HashMap::new();
        let mut ambiguous_symbols_by_label: HashMap<String, usize> = HashMap::new();
        for (label, ids) in &self.symbol_index {
            match ids.as_slice() {
                [id] => {
                    singleton_symbols_by_label.insert(label.clone(), *id);
                }
                [] => {}
                _ => {
                    ambiguous_symbols_by_label.insert(label.clone(), ids.len());
                }
            }
        }
        let mut files_by_label: HashMap<String, NodeId> = HashMap::new();
        for node in &self.facts.nodes {
            if node.kind == NodeKind::File {
                files_by_label
                    .entry(node.label.clone())
                    .or_insert(node.node_id);
            }
        }
        let node_kind_by_id: HashMap<NodeId, NodeKind> = self
            .facts
            .nodes
            .iter()
            .map(|node| (node.node_id, node.kind))
            .collect();
        let qualified_symbols_by_name = qualified_symbols_by_name(&self.facts);
        let pending = std::mem::take(&mut self.pending_edges);
        let mut ambiguous_unresolved = 0usize;
        for edge in pending {
            if edge.edge_kind == Some(GraphEdgeKind::CallsDyn) {
                let mut candidates = Vec::new();
                if let Some(indexed) = self.qualified_symbol_index.get(&edge.target_name) {
                    candidates.extend(indexed.iter().copied());
                }
                if let Some(indexed) = qualified_symbols_by_name.get(&edge.target_name) {
                    candidates.extend(indexed.iter().copied());
                }
                candidates.extend(trait_method_candidates(&self.facts, &edge.target_name));
                candidates.sort_by_key(|id| id.get());
                candidates.dedup();
                match candidates.as_slice() {
                    [target]
                        if *target != edge.source
                            && matches!(
                                node_kind_by_id.get(target).copied(),
                                Some(NodeKind::Method)
                            ) =>
                    {
                        self.add_pending_edge(&edge, Some(*target));
                    }
                    candidates if candidates.len() > 1 => {
                        ambiguous_unresolved += 1;
                        tracing::debug!(
                            target_label = %edge.target_name,
                            candidates = candidates.len(),
                            "spur-graph: ambiguous dyn trait call target; leaving unresolved"
                        );
                        self.add_pending_edge(&edge, None);
                    }
                    _ => {
                        self.add_pending_edge(&edge, None);
                    }
                }
            } else if edge.relation == RelationKind::References {
                if let Some(target) = singleton_symbols_by_label.get(&edge.target_name).copied() {
                    if target != edge.source
                        && matches!(
                            node_kind_by_id.get(&target).copied(),
                            Some(NodeKind::Function | NodeKind::Method)
                        )
                    {
                        self.add_pending_edge(&edge, Some(target));
                    }
                }
            } else if let Some(candidates) =
                ambiguous_symbols_by_label.get(&edge.target_name).copied()
            {
                ambiguous_unresolved += 1;
                tracing::debug!(
                    target_label = %edge.target_name,
                    candidates,
                    "spur-graph: ambiguous pending edge target; leaving unresolved"
                );
                self.add_pending_edge(&edge, None);
            } else if let Some(target) = singleton_symbols_by_label
                .get(&edge.target_name)
                .copied()
                .or_else(|| files_by_label.get(&edge.target_name).copied())
            {
                if target != edge.source {
                    self.add_pending_edge(&edge, Some(target));
                }
            } else {
                self.add_pending_edge(&edge, None);
            }
        }
        if ambiguous_unresolved > 0 {
            tracing::info!(
                ambiguous_unresolved,
                "spur-graph: left ambiguous pending edges unresolved"
            );
        }
    }
}

fn metadata_for_pending_edge(
    edge: &PendingEdge,
    target: Option<NodeId>,
) -> (Confidence, f32, Option<&'static str>) {
    if edge.origin == CallOrigin::MacroBody
        && edge.relation == RelationKind::Calls
        && target.is_some()
    {
        return (Confidence::Heuristic, 0.8, Some("macro_body_singleton"));
    }

    let (confidence, confidence_score) = confidence_for_edge(edge.relation, edge.edge_kind);
    (confidence, confidence_score, None)
}

fn confidence_for_edge(
    relation: RelationKind,
    edge_kind: Option<GraphEdgeKind>,
) -> (Confidence, f32) {
    if edge_kind == Some(GraphEdgeKind::CallsDyn) {
        return (Confidence::Heuristic, 0.8);
    }
    match relation {
        RelationKind::Contains | RelationKind::Calls => (Confidence::SyntaxExact, 1.0),
        RelationKind::Imports | RelationKind::Links => (Confidence::Heuristic, 0.8),
        _ => (Confidence::Heuristic, 0.5),
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
    language_label: &str,
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
    if language_label == "rust" {
        emit_mcp_tools(
            builder,
            &relative_path,
            file_id,
            file_node,
            source,
            root_node,
            &definitions,
        );
    }
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
    if language_label == "rust" {
        emit_rust_dyn_trait_edges(builder, file_node, source, &definitions, root_node);
    }
    Ok(())
}

fn compile_queries(config: &LanguageConfig, language: Language) -> anyhow::Result<CompiledQueries> {
    let language_label = language.label();
    let mut tags = None;
    let mut tags_source = None;
    let mut spur_edges = None;
    let mut inline_spur_edges = None;
    for (name, source) in config.queries {
        match *name {
            "tags" => {
                tags_source = Some(source);
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
    let tags_source =
        tags_source.with_context(|| format!("missing `{language_label}` tags query"))?;
    let symbols_source = symbol_query_policy(language).source(tags_source);
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
    let mut matches = cursor.matches(query, root_node, source.as_bytes());
    let mut hits = Vec::new();
    let mut match_index = 0usize;
    while let Some(query_match) = matches.next() {
        let current_match_index = match_index;
        match_index += 1;
        for capture in query_match.captures {
            if is_string_literal_context(capture.node) {
                continue;
            }
            hits.push(CaptureHit {
                name: capture_names[capture.index as usize].to_string(),
                node: capture.node,
                pattern_index: query_match.pattern_index,
                match_index: current_match_index,
            });
        }
    }
    hits
}

fn is_string_literal_context(node: Node<'_>) -> bool {
    let mut current = Some(node);
    while let Some(node) = current {
        if matches!(
            node.kind(),
            "string_literal"
                | "raw_string_literal"
                | "string_content"
                | "interpreted_string_literal"
                | "raw_string_literal_content"
        ) {
            return true;
        }
        current = node.parent();
    }
    false
}

fn impl_identity_fqn(fqn: &str) -> String {
    if let Some((prefix, segment)) = fqn.rsplit_once("::") {
        if let Some(stripped) = segment.strip_prefix("impl ") {
            return format!("{prefix}::{stripped}");
        }
    }
    fqn.strip_prefix("impl ").unwrap_or(fqn).to_string()
}

fn qualified_symbols_by_name(facts: &GraphFacts) -> HashMap<String, Vec<NodeId>> {
    let nodes_by_id: HashMap<_, _> = facts
        .nodes
        .iter()
        .map(|node| (node.node_id, node))
        .collect();
    let mut parent_by_target = HashMap::new();
    for edge in &facts.edges {
        if edge.relation != RelationKind::Contains {
            continue;
        }
        let Some(target) = edge.target_node_id else {
            continue;
        };
        parent_by_target
            .entry(target)
            .or_insert(edge.source_node_id);
    }

    let mut index: HashMap<String, Vec<NodeId>> = HashMap::new();
    for node in &facts.nodes {
        if matches!(node.kind, NodeKind::File | NodeKind::McpTool) {
            continue;
        }
        let qualified_name = qualified_node_name(node, &nodes_by_id, &parent_by_target);
        index.entry(qualified_name).or_default().push(node.node_id);
    }
    index
}

fn qualified_node_name(
    node: &GraphNode,
    nodes_by_id: &HashMap<NodeId, &GraphNode>,
    parent_by_target: &HashMap<NodeId, NodeId>,
) -> String {
    let mut segments = vec![qualified_node_segment(node)];
    let mut current = node;
    let mut seen = HashSet::new();
    seen.insert(node.node_id);

    while let Some(parent) = parent_by_target
        .get(&current.node_id)
        .and_then(|id| nodes_by_id.get(id).copied())
    {
        if !seen.insert(parent.node_id) || parent.kind == NodeKind::File {
            break;
        }
        segments.push(qualified_node_segment(parent));
        current = parent;
    }

    segments.reverse();
    segments.join("::")
}

fn qualified_node_segment(node: &GraphNode) -> String {
    match node.kind {
        NodeKind::Impl => format!(
            "impl {}",
            node.label.strip_prefix("impl ").unwrap_or(&node.label)
        ),
        _ => node
            .label
            .strip_prefix("impl ")
            .unwrap_or(&node.label)
            .to_string(),
    }
}

fn trait_method_candidates(facts: &GraphFacts, target_name: &str) -> Vec<NodeId> {
    let Some((trait_name, method_name)) = target_name.rsplit_once("::") else {
        return Vec::new();
    };
    let nodes_by_id: HashMap<_, _> = facts
        .nodes
        .iter()
        .map(|node| (node.node_id, node))
        .collect();
    let mut parent_by_target = HashMap::new();
    for edge in &facts.edges {
        if edge.relation != RelationKind::Contains {
            continue;
        }
        let Some(target) = edge.target_node_id else {
            continue;
        };
        parent_by_target
            .entry(target)
            .or_insert(edge.source_node_id);
    }

    facts
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Method && node.label == method_name)
        .filter_map(|node| {
            let parent = parent_by_target
                .get(&node.node_id)
                .and_then(|id| nodes_by_id.get(id).copied())?;
            (parent.kind == NodeKind::Trait
                && (parent.label == trait_name
                    || qualified_node_name(parent, &nodes_by_id, &parent_by_target) == trait_name))
                .then_some(node.node_id)
        })
        .collect()
}

pub(crate) fn relative_path(root: &Path, path: &Path) -> anyhow::Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("`{}` is outside `{}`", path.display(), root.display()))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_query_policy_documents_shared_and_dedicated_sources() {
        assert_eq!(
            symbol_query_policy(Language::Rust),
            SymbolQueryPolicy::ReuseTags
        );
        assert_eq!(
            symbol_query_policy(Language::TypeScript),
            SymbolQueryPolicy::ReuseTags
        );
        assert_eq!(
            symbol_query_policy(Language::Tsx),
            SymbolQueryPolicy::ReuseTags
        );
        assert_eq!(
            symbol_query_policy(Language::Cpp),
            SymbolQueryPolicy::ReuseTags
        );
        assert!(matches!(
            symbol_query_policy(Language::Python),
            SymbolQueryPolicy::Dedicated(_)
        ));
        assert!(matches!(
            symbol_query_policy(Language::Markdown),
            SymbolQueryPolicy::Dedicated(_)
        ));
    }

    #[test]
    fn ambiguous_pending_bare_name_edge_remains_unresolved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("configure parser");
        let tree = parser
            .parse(
                "fn caller() { flush(); }\nfn flush() {}\nmod inner { fn flush() {} }\n",
                None,
            )
            .expect("parse source");
        let root_node = tree.root_node();

        let mut builder = FactBuilder::new(dir.path());
        let file_id = FileId(builder.next_file_id());
        let source = builder.add_node(
            "src/lib.rs",
            "caller".to_string(),
            "caller".to_string(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        builder.add_node(
            "src/lib.rs",
            "flush".to_string(),
            "flush".to_string(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        builder.add_node(
            "src/lib.rs",
            "flush".to_string(),
            "inner::flush".to_string(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        builder.pending_edges.push(PendingEdge {
            source,
            target_name: "flush".to_string(),
            relation: RelationKind::Calls,
            edge_kind: None,
            origin: CallOrigin::Expression,
            receiver_text: None,
            scope_text: None,
        });

        builder.resolve_pending_edges();

        assert_eq!(builder.facts.edges.len(), 1);
        let edge = &builder.facts.edges[0];
        assert_eq!(edge.source_node_id, source);
        assert_eq!(edge.target_node_id, None);
        assert_eq!(edge.target_label.as_deref(), Some("flush"));
    }

    #[test]
    fn macro_body_singleton_pending_edge_is_heuristic_and_stamped() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("configure parser");
        let tree = parser
            .parse(
                "fn caller() { json!({ \"x\": helper() }); }\nfn helper() {}\n",
                None,
            )
            .expect("parse source");
        let root_node = tree.root_node();

        let mut builder = FactBuilder::new(dir.path());
        let file_id = FileId(builder.next_file_id());
        let source = builder.add_node(
            "src/lib.rs",
            "caller".to_string(),
            "caller".to_string(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        let target = builder.add_node(
            "src/lib.rs",
            "helper".to_string(),
            "helper".to_string(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        builder.pending_edges.push(PendingEdge {
            source,
            target_name: "helper".to_string(),
            relation: RelationKind::Calls,
            edge_kind: None,
            origin: CallOrigin::MacroBody,
            receiver_text: None,
            scope_text: None,
        });

        builder.resolve_pending_edges();

        assert_eq!(builder.facts.edges.len(), 1);
        let edge = &builder.facts.edges[0];
        assert_eq!(edge.target_node_id, Some(target));
        assert_eq!(edge.confidence, Confidence::Heuristic);
        assert_eq!(edge.confidence_score, 0.8);
        assert_eq!(edge.bind_method.as_deref(), Some("macro_body_singleton"));
    }

    #[test]
    fn pending_edges_group_receivers_by_query_match() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        let path = root.join("src/lib.rs");
        let source = "pub struct Outer;\n\
             pub struct Inner;\n\
             impl Outer {\n\
                 pub fn foo(&self, _handler: fn(&Inner)) {}\n\
             }\n\
             impl Inner {\n\
                 pub fn bar(&self) {}\n\
             }\n\
             pub fn caller(outer: &Outer) {\n\
                 outer.foo({\n\
                     fn nested(inner: &Inner) {\n\
                         inner.bar();\n\
                     }\n\
                     nested\n\
                 });\n\
             }\n";
        fs::write(&path, source).expect("write lib.rs");

        let config = crate::extract::languages::rust_config();
        let queries = compile_queries(&config, Language::Rust).expect("compile queries");
        let mut parser = Parser::new();
        parser
            .set_language(&config.language)
            .expect("configure parser");
        let tree = parser.parse(source.as_bytes(), None).expect("parse source");
        let mut builder = FactBuilder::new(root);

        extract_file_from_tree(
            &mut builder,
            "rust",
            &config,
            &path,
            source,
            tree.root_node(),
            &queries,
        )
        .expect("extract file");

        let caller = builder
            .facts
            .nodes
            .iter()
            .find(|node| node.label == "caller" && node.kind == NodeKind::Function)
            .expect("caller function");
        let outer_edge = builder
            .pending_edges
            .iter()
            .find(|edge| edge.source == caller.node_id && edge.target_name == "foo")
            .expect("outer foo edge");

        assert_eq!(outer_edge.receiver_text.as_deref(), Some("outer"));
        assert_ne!(outer_edge.receiver_text.as_deref(), Some("inner"));
        assert!(!builder
            .pending_edges
            .iter()
            .any(|edge| edge.source == caller.node_id && edge.target_name == "bar"));
    }
}

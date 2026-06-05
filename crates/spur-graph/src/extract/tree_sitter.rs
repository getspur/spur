use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::str;

use anyhow::{anyhow, Context as _};
use indicatif::ProgressBar;
use thiserror::Error;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator as _};

use crate::discovery::discover_files;
use crate::extract::languages::{
    all_supported_extensions, emit_definitions, emit_definitions_with_parents, emit_edges,
    emit_rust_dyn_trait_edges, extracted_symbols, language_registry, Language, LanguageConfig,
    LanguageDescriptor,
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
    pub(crate) jsx_edges: Option<Query>,
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
        Language::Rust
        | Language::Python
        | Language::TypeScript
        | Language::Tsx
        | Language::Cpp => SymbolQueryPolicy::ReuseTags,
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

#[derive(Debug, Clone, Copy)]
struct EdgeMetadata {
    confidence: Confidence,
    confidence_score: f32,
    bind_method: Option<&'static str>,
}

struct PendingResolutionIndexes<'a> {
    singleton_symbols_by_label: &'a HashMap<String, NodeId>,
    ambiguous_symbols_by_label: &'a HashMap<String, usize>,
    files_by_label: &'a HashMap<String, NodeId>,
    file_by_id: &'a HashMap<NodeId, &'a str>,
    node_kind_by_id: &'a HashMap<NodeId, NodeKind>,
    enclosing_scope_by_id: &'a HashMap<NodeId, String>,
    qualified_name_by_id: &'a HashMap<NodeId, String>,
}

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
        let mut captures = run_query(&self.queries.symbols, tree.root_node(), source);
        if let Some(spur_edges) = self.queries.spur_edges.as_ref() {
            captures.extend(run_query(spur_edges, tree.root_node(), source));
        }
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
            relative_path.to_owned(),
            relative_path.to_owned(),
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
        self.add_node_with_range(relative_path, label, fqn, kind, file_id, node.range())
    }

    pub(crate) fn add_node_with_range(
        &mut self,
        relative_path: &str,
        label: String,
        fqn: String,
        kind: NodeKind,
        file_id: FileId,
        range: tree_sitter::Range,
    ) -> NodeId {
        let node_id = NodeId(self.next_node);
        self.next_node += 1;
        let span_id = SpanId(self.next_span);
        self.next_span += 1;
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
            EdgeMetadata {
                confidence,
                confidence_score,
                bind_method: None,
            },
        );
    }

    fn add_pending_edge(&mut self, edge: &PendingEdge, target: Option<NodeId>) {
        self.add_pending_edge_with_bind_method(edge, target, None);
    }

    fn add_pending_edge_as(
        &mut self,
        edge: &PendingEdge,
        target: Option<NodeId>,
        relation: RelationKind,
        bind_method: Option<&'static str>,
    ) {
        let metadata = metadata_for_pending_edge(edge, target, bind_method);
        self.add_edge_with_metadata(
            edge.source,
            target,
            relation,
            Some(edge.target_name.clone()),
            edge.edge_kind,
            metadata,
        );
    }

    fn add_pending_edge_with_bind_method(
        &mut self,
        edge: &PendingEdge,
        target: Option<NodeId>,
        bind_method: Option<&'static str>,
    ) {
        let metadata = metadata_for_pending_edge(edge, target, bind_method);
        self.add_edge_with_metadata(
            edge.source,
            target,
            edge.relation,
            Some(edge.target_name.clone()),
            edge.edge_kind,
            metadata,
        );
    }

    fn add_edge_with_metadata(
        &mut self,
        source: NodeId,
        target: Option<NodeId>,
        relation: RelationKind,
        target_label: Option<String>,
        edge_kind: Option<GraphEdgeKind>,
        metadata: EdgeMetadata,
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
            confidence: metadata.confidence,
            confidence_score: metadata.confidence_score,
            edge_kind,
            bind_method: metadata.bind_method.map(str::to_string),
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
        let file_path_by_file_node: HashMap<NodeId, String> = self
            .facts
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::File)
            .map(|node| (node.node_id, node.label.clone()))
            .collect();
        let (qualified_symbols_by_name, enclosing_scope_by_id, file_by_id) = {
            let nodes_by_id: HashMap<_, _> = self
                .facts
                .nodes
                .iter()
                .map(|node| (node.node_id, node))
                .collect();
            let parent_by_target = parent_by_target(&self.facts);
            (
                qualified_symbols_by_name_from_maps(&self.facts, &nodes_by_id, &parent_by_target),
                enclosing_scope_by_id(&self.facts, &nodes_by_id, &parent_by_target),
                file_by_id_from_maps(
                    &self.facts,
                    &nodes_by_id,
                    &parent_by_target,
                    &file_path_by_file_node,
                ),
            )
        };
        let qualified_name_by_id = qualified_name_by_id_from_index(&qualified_symbols_by_name);
        let pending = std::mem::take(&mut self.pending_edges);
        let indexes = PendingResolutionIndexes {
            singleton_symbols_by_label: &singleton_symbols_by_label,
            ambiguous_symbols_by_label: &ambiguous_symbols_by_label,
            files_by_label: &files_by_label,
            file_by_id: &file_by_id,
            node_kind_by_id: &node_kind_by_id,
            enclosing_scope_by_id: &enclosing_scope_by_id,
            qualified_name_by_id: &qualified_name_by_id,
        };
        let mut ambiguous_unresolved = 0usize;
        let mut phantom_blocked_references = 0usize;
        let mut phantom_blocked_calls = 0usize;
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
                        let provably_unsafe = matches!(
                            (file_by_id.get(&edge.source).copied(), file_by_id.get(&target).copied()),
                            (Some(src_file), Some(tgt_file))
                                if !function_singleton_safe(src_file, tgt_file)
                        );
                        if provably_unsafe {
                            phantom_blocked_references += 1;
                            self.add_pending_edge(&edge, None);
                        } else {
                            self.add_pending_edge(&edge, Some(target));
                        }
                    }
                }
            } else if edge.relation == RelationKind::Calls {
                let candidates = qualified_edge_candidates(&edge, &qualified_symbols_by_name);
                match candidates.as_slice() {
                    [target] if *target != edge.source => {
                        let kind = node_kind_by_id.get(target).copied();
                        if matches!(
                            kind,
                            Some(NodeKind::Struct | NodeKind::EnumVariant | NodeKind::Class)
                        ) {
                            self.add_pending_edge_as(
                                &edge,
                                Some(*target),
                                RelationKind::Constructs,
                                None,
                            );
                        } else if matches!(kind, Some(NodeKind::Function | NodeKind::Method)) {
                            self.add_pending_edge_with_bind_method(
                                &edge,
                                Some(*target),
                                Some("fqn"),
                            );
                        } else {
                            self.add_pending_edge(&edge, None);
                        }
                    }
                    candidates if candidates.len() > 1 => {
                        ambiguous_unresolved += 1;
                        tracing::debug!(
                            target_label = %edge.target_name,
                            candidates = candidates.len(),
                            "spur-graph: ambiguous qualified pending edge target; leaving unresolved"
                        );
                        self.add_pending_edge(&edge, None);
                    }
                    _ => {
                        resolve_call_edge_after_qualified_miss(
                            self,
                            &edge,
                            &indexes,
                            &mut ambiguous_unresolved,
                            &mut phantom_blocked_calls,
                        );
                    }
                }
            } else if relational_target_kinds(edge.relation).is_some()
                || matches!(
                    edge.relation,
                    RelationKind::Imports | RelationKind::Constructs
                )
            {
                resolve_bare_pending_edge(
                    self,
                    &edge,
                    &indexes,
                    &mut ambiguous_unresolved,
                    &mut phantom_blocked_calls,
                );
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
            } else {
                resolve_bare_pending_edge(
                    self,
                    &edge,
                    &indexes,
                    &mut ambiguous_unresolved,
                    &mut phantom_blocked_calls,
                );
            }
        }
        if ambiguous_unresolved > 0 {
            tracing::info!(
                ambiguous_unresolved,
                "spur-graph: left ambiguous pending edges unresolved"
            );
        }
        if phantom_blocked_references > 0 || phantom_blocked_calls > 0 {
            tracing::info!(
                phantom_blocked_references,
                phantom_blocked_calls,
                "spur-graph: crate-scope gate (measurement-only) would have blocked phantom singleton binds"
            );
        }
    }
}

fn resolve_call_edge_after_qualified_miss(
    builder: &mut FactBuilder<'_>,
    edge: &PendingEdge,
    indexes: &PendingResolutionIndexes<'_>,
    ambiguous_unresolved: &mut usize,
    phantom_blocked_calls: &mut usize,
) {
    let candidates = method_scope_candidates(
        edge,
        builder.symbol_index.get(&edge.target_name),
        indexes.node_kind_by_id,
        indexes.enclosing_scope_by_id,
    );
    match candidates.as_slice() {
        [target] => {
            builder.add_pending_edge_with_bind_method(edge, Some(*target), Some("scope_match"));
        }
        candidates if candidates.len() > 1 => {
            *ambiguous_unresolved += 1;
            tracing::debug!(
                target_label = %edge.target_name,
                candidates = candidates.len(),
                "spur-graph: ambiguous scoped method pending edge target; leaving unresolved"
            );
            builder.add_pending_edge(edge, None);
        }
        _ => {
            resolve_bare_pending_edge(
                builder,
                edge,
                indexes,
                ambiguous_unresolved,
                phantom_blocked_calls,
            );
        }
    }
}

fn resolve_bare_pending_edge(
    builder: &mut FactBuilder<'_>,
    edge: &PendingEdge,
    indexes: &PendingResolutionIndexes<'_>,
    ambiguous_unresolved: &mut usize,
    phantom_blocked_calls: &mut usize,
) {
    if edge.relation == RelationKind::Calls {
        let candidates = callable_symbol_candidates(builder, edge, indexes);
        if let Some(target) =
            same_file_duplicate_function_candidate(edge, Some(&candidates), indexes)
        {
            builder.add_pending_edge_with_bind_method(
                edge,
                Some(target),
                Some("same_file_duplicate"),
            );
            return;
        }

        match candidates.as_slice() {
            [target] if *target != edge.source => {
                resolve_singleton_bare_target(
                    builder,
                    edge,
                    *target,
                    indexes,
                    phantom_blocked_calls,
                );
                return;
            }
            candidates if candidates.len() > 1 => {
                *ambiguous_unresolved += 1;
                tracing::debug!(
                    target_label = %edge.target_name,
                    candidates = candidates.len(),
                    "spur-graph: ambiguous callable pending edge target; leaving unresolved"
                );
                builder.add_pending_edge(edge, None);
                return;
            }
            _ => {}
        }
    }

    if edge.relation == RelationKind::Imports {
        let candidates = import_resolution_candidates(builder, edge, indexes);
        match candidates.as_slice() {
            [target] if *target != edge.source => {
                builder.add_pending_edge(edge, Some(*target));
                return;
            }
            candidates if candidates.len() > 1 => {
                *ambiguous_unresolved += 1;
                tracing::debug!(
                    target_label = %edge.target_name,
                    candidates = candidates.len(),
                    "spur-graph: ambiguous import pending edge target; leaving unresolved"
                );
                builder.add_pending_edge(edge, None);
                return;
            }
            _ => {}
        }
    }

    if edge.relation == RelationKind::Constructs {
        let candidates = constructs_symbol_candidates(builder, edge, indexes);
        match candidates.as_slice() {
            [target] => {
                if constructs_language_family_allows(builder, edge, *target, indexes) {
                    builder.add_pending_edge_with_bind_method(
                        edge,
                        Some(*target),
                        Some("constructs_type_singleton"),
                    );
                } else {
                    builder.add_pending_edge(edge, None);
                }
                return;
            }
            candidates if candidates.len() > 1 => {
                *ambiguous_unresolved += 1;
                tracing::debug!(
                    target_label = %edge.target_name,
                    candidates = candidates.len(),
                    "spur-graph: ambiguous constructs pending edge target; leaving unresolved"
                );
                builder.add_pending_edge(edge, None);
                return;
            }
            _ => {
                builder.add_pending_edge(edge, None);
                return;
            }
        }
    }

    if let Some(allowed) = relational_target_kinds(edge.relation) {
        let candidates = relational_symbol_candidates(builder, edge, indexes, allowed);
        match candidates.as_slice() {
            [target] if *target != edge.source => {
                builder.add_pending_relational_edge(edge, *target, indexes);
                return;
            }
            cands if cands.len() > 1 => {
                *ambiguous_unresolved += 1;
                builder.add_pending_edge(edge, None);
                return;
            }
            _ => {
                builder.add_pending_edge(edge, None);
                return;
            }
        }
    }

    if let Some(candidates) = indexes
        .ambiguous_symbols_by_label
        .get(&edge.target_name)
        .copied()
    {
        if let Some(target) = same_file_duplicate_function_candidate(
            edge,
            builder.symbol_index.get(&edge.target_name),
            indexes,
        ) {
            builder.add_pending_edge_with_bind_method(
                edge,
                Some(target),
                Some("same_file_duplicate"),
            );
            return;
        }

        *ambiguous_unresolved += 1;
        tracing::debug!(
            target_label = %edge.target_name,
            candidates,
            "spur-graph: ambiguous pending edge target; leaving unresolved"
        );
        builder.add_pending_edge(edge, None);
        return;
    }

    let Some(target) = indexes
        .singleton_symbols_by_label
        .get(&edge.target_name)
        .copied()
        .or_else(|| indexes.files_by_label.get(&edge.target_name).copied())
    else {
        builder.add_pending_edge(edge, None);
        return;
    };

    if target == edge.source {
        return;
    }

    resolve_singleton_bare_target(builder, edge, target, indexes, phantom_blocked_calls);
}

fn resolve_singleton_bare_target(
    builder: &mut FactBuilder<'_>,
    edge: &PendingEdge,
    target: NodeId,
    indexes: &PendingResolutionIndexes<'_>,
    phantom_blocked_calls: &mut usize,
) {
    match indexes.node_kind_by_id.get(&target).copied() {
        Some(NodeKind::Method) => {
            if edge.relation == RelationKind::Calls
                && method_scope_matches(edge, target, indexes.enclosing_scope_by_id)
            {
                builder.add_pending_edge_with_bind_method(edge, Some(target), Some("scope_match"));
            } else if edge.relation == RelationKind::Calls {
                let file_for_node = |node_id| {
                    indexes.file_by_id.get(&node_id).copied().or_else(|| {
                        let file_id = builder
                            .facts
                            .nodes
                            .iter()
                            .find(|node| node.node_id == node_id)?
                            .file_id?;
                        builder
                            .facts
                            .nodes
                            .iter()
                            .find(|node| {
                                node.kind == NodeKind::File && node.file_id == Some(file_id)
                            })
                            .map(|node| node.label.as_str())
                    })
                };
                let same_crate_safe = matches!(
                    (file_for_node(edge.source), file_for_node(target)),
                    (Some(src_file), Some(tgt_file)) if function_singleton_safe(src_file, tgt_file)
                );
                let builtin_method = file_for_node(edge.source)
                    .and_then(|src_file| {
                        crate::extract::languages::Language::from_path(std::path::Path::new(
                            src_file,
                        ))
                    })
                    .is_some_and(|lang| {
                        lang.builtin_method_names()
                            .binary_search(&edge.target_name.as_str())
                            .is_ok()
                    });
                if same_crate_safe && !builtin_method {
                    builder.add_pending_edge_with_bind_method(
                        edge,
                        Some(target),
                        Some("method_crate_singleton"),
                    );
                } else {
                    builder.add_pending_edge(edge, None);
                }
            } else {
                builder.add_pending_edge(edge, Some(target));
            }
        }
        Some(NodeKind::Function) if edge.relation == RelationKind::Calls => {
            let file_for_node = |node_id| {
                indexes.file_by_id.get(&node_id).copied().or_else(|| {
                    let file_id = builder
                        .facts
                        .nodes
                        .iter()
                        .find(|node| node.node_id == node_id)?
                        .file_id?;
                    builder
                        .facts
                        .nodes
                        .iter()
                        .find(|node| node.kind == NodeKind::File && node.file_id == Some(file_id))
                        .map(|node| node.label.as_str())
                })
            };
            let provably_unsafe = matches!(
                (file_for_node(edge.source), file_for_node(target)),
                (Some(src_file), Some(tgt_file)) if !function_singleton_safe(src_file, tgt_file)
            );
            if provably_unsafe {
                *phantom_blocked_calls += 1;
                builder.add_pending_edge(edge, None);
            } else {
                builder.add_pending_edge_with_bind_method(edge, Some(target), Some("singleton"));
            }
        }
        _ => {
            if edge.relation == RelationKind::Calls
                && matches!(
                    indexes.node_kind_by_id.get(&target).copied(),
                    Some(NodeKind::Struct | NodeKind::EnumVariant | NodeKind::Class)
                )
            {
                builder.add_pending_edge_as(edge, Some(target), RelationKind::Constructs, None);
            } else if should_reclassify_python_extends_as_implements(edge, target, indexes) {
                builder.add_pending_edge_as(
                    edge,
                    Some(target),
                    RelationKind::Implements,
                    Some("relational"),
                );
            } else if edge.relation == RelationKind::Calls {
                // P1a: calls only resolve to callable symbols (handled above) or
                // constructible types. Other singleton kinds are misresolutions.
                builder.add_pending_edge(edge, None);
            } else {
                builder.add_pending_edge(edge, Some(target));
            }
        }
    }
}

fn callable_symbol_candidates(
    builder: &FactBuilder<'_>,
    edge: &PendingEdge,
    indexes: &PendingResolutionIndexes<'_>,
) -> Vec<NodeId> {
    let mut candidates = builder
        .symbol_index
        .get(&edge.target_name)
        .into_iter()
        .flat_map(|ids| ids.iter().copied())
        .filter(|target| *target != edge.source)
        .filter(|target| {
            indexes
                .node_kind_by_id
                .get(target)
                .copied()
                .is_some_and(is_callable_target_kind)
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|id| id.get());
    candidates.dedup();
    candidates
}

fn relational_target_kinds(relation: RelationKind) -> Option<&'static [NodeKind]> {
    match relation {
        RelationKind::Implements => Some(&[NodeKind::Trait, NodeKind::Interface]),
        RelationKind::Extends => Some(&[NodeKind::Trait, NodeKind::Interface, NodeKind::Class]),
        _ => None,
    }
}

fn constructs_target_kinds() -> &'static [NodeKind] {
    &[
        NodeKind::Struct,
        NodeKind::Enum,
        NodeKind::EnumVariant,
        NodeKind::Class,
    ]
}

fn constructs_symbol_candidates(
    builder: &FactBuilder<'_>,
    edge: &PendingEdge,
    indexes: &PendingResolutionIndexes<'_>,
) -> Vec<NodeId> {
    let allowed = constructs_target_kinds();
    let mut candidates = builder
        .symbol_index
        .get(&edge.target_name)
        .into_iter()
        .flat_map(|ids| ids.iter().copied())
        .filter(|target| *target != edge.source)
        .filter(|target| {
            indexes
                .node_kind_by_id
                .get(target)
                .copied()
                .is_some_and(|kind| allowed.contains(&kind))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|id| id.get());
    candidates.dedup();
    candidates
}

fn relational_symbol_candidates(
    builder: &FactBuilder<'_>,
    edge: &PendingEdge,
    indexes: &PendingResolutionIndexes<'_>,
    allowed: &[NodeKind],
) -> Vec<NodeId> {
    let source_language_family =
        file_path_for_node(builder, edge.source, indexes).and_then(|path| language_family(&path));
    let mut candidates = builder
        .symbol_index
        .get(&edge.target_name)
        .into_iter()
        .flat_map(|ids| ids.iter().copied())
        .filter(|target| *target != edge.source)
        .filter(|target| {
            indexes
                .node_kind_by_id
                .get(target)
                .copied()
                .is_some_and(|kind| allowed.contains(&kind))
        })
        .filter(|target| match source_language_family {
            None => true,
            Some(source_family) => {
                file_path_for_node(builder, *target, indexes)
                    .as_deref()
                    .and_then(language_family)
                    == Some(source_family)
            }
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|id| id.get());
    candidates.dedup();
    candidates
}

impl FactBuilder<'_> {
    fn add_pending_relational_edge(
        &mut self,
        edge: &PendingEdge,
        target: NodeId,
        indexes: &PendingResolutionIndexes<'_>,
    ) {
        if should_reclassify_python_extends_as_implements(edge, target, indexes) {
            self.add_pending_edge_as(
                edge,
                Some(target),
                RelationKind::Implements,
                Some("relational"),
            );
        } else {
            self.add_pending_edge_with_bind_method(edge, Some(target), Some("relational"));
        }
    }
}

fn constructs_language_family_allows(
    builder: &FactBuilder<'_>,
    edge: &PendingEdge,
    target: NodeId,
    indexes: &PendingResolutionIndexes<'_>,
) -> bool {
    let Some(src_file) = file_path_for_node(builder, edge.source, indexes) else {
        return false;
    };
    let Some(tgt_file) = file_path_for_node(builder, target, indexes) else {
        return false;
    };
    if src_file == tgt_file {
        return true;
    }
    matches!(
        (language_family(&src_file), language_family(&tgt_file)),
        (Some(src_family), Some(tgt_family)) if src_family == tgt_family
    )
}

fn file_path_for_node(
    builder: &FactBuilder<'_>,
    node_id: NodeId,
    indexes: &PendingResolutionIndexes<'_>,
) -> Option<String> {
    indexes
        .file_by_id
        .get(&node_id)
        .map(|path| (*path).to_owned())
        .or_else(|| {
            let file_id = builder
                .facts
                .nodes
                .iter()
                .find(|node| node.node_id == node_id)?
                .file_id?;
            builder
                .facts
                .nodes
                .iter()
                .find(|node| node.kind == NodeKind::File && node.file_id == Some(file_id))
                .map(|node| node.label.clone())
        })
}

fn should_reclassify_python_extends_as_implements(
    edge: &PendingEdge,
    target: NodeId,
    indexes: &PendingResolutionIndexes<'_>,
) -> bool {
    edge.relation == RelationKind::Extends
        && indexes.node_kind_by_id.get(&target).copied() == Some(NodeKind::Interface)
        && indexes
            .file_by_id
            .get(&edge.source)
            .is_some_and(|path| is_python_path(path))
        && indexes
            .file_by_id
            .get(&target)
            .is_some_and(|path| is_python_path(path))
}

fn is_python_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("py"))
}

fn import_resolution_candidates(
    builder: &FactBuilder<'_>,
    edge: &PendingEdge,
    indexes: &PendingResolutionIndexes<'_>,
) -> Vec<NodeId> {
    let mut candidates = builder
        .symbol_index
        .get(&edge.target_name)
        .into_iter()
        .flat_map(|ids| ids.iter().copied())
        .filter(|target| *target != edge.source)
        .filter(|target| {
            indexes
                .node_kind_by_id
                .get(target)
                .copied()
                .is_some_and(is_import_resolution_candidate_kind)
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|id| id.get());
    candidates.dedup();
    candidates
}

fn is_callable_target_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Class
            | NodeKind::Struct
            | NodeKind::EnumVariant
            | NodeKind::Macro
    )
}

fn is_import_resolution_candidate_kind(kind: NodeKind) -> bool {
    // Keep impls in the ambiguity set for historical Rust type imports
    // (`struct Helper` plus `impl Helper`); this filter exists to keep member
    // symbols such as fields and methods from shadowing imports.
    matches!(
        kind,
        NodeKind::Module
            | NodeKind::Function
            | NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Impl
            | NodeKind::Enum
            | NodeKind::EnumVariant
            | NodeKind::Trait
            | NodeKind::TypeAlias
            | NodeKind::Macro
            | NodeKind::Constant
    )
}

fn same_file_duplicate_function_candidate(
    edge: &PendingEdge,
    candidates: Option<&Vec<NodeId>>,
    indexes: &PendingResolutionIndexes<'_>,
) -> Option<NodeId> {
    if edge.relation != RelationKind::Calls {
        return None;
    }

    let mut candidates = candidates?
        .iter()
        .copied()
        .filter(|target| *target != edge.source)
        .collect::<Vec<_>>();
    candidates.sort_by_key(|id| id.get());
    candidates.dedup();
    if candidates.len() < 2 {
        return None;
    }

    if !candidates.iter().all(|target| {
        matches!(
            indexes.node_kind_by_id.get(target),
            Some(NodeKind::Function)
        )
    }) {
        return None;
    }

    let first_file = indexes.file_by_id.get(&candidates[0]).copied()?;
    if !candidates
        .iter()
        .all(|target| indexes.file_by_id.get(target).copied() == Some(first_file))
    {
        return None;
    }

    let first_qualified_name = indexes.qualified_name_by_id.get(&candidates[0])?;
    if !candidates
        .iter()
        .all(|target| indexes.qualified_name_by_id.get(target) == Some(first_qualified_name))
    {
        return None;
    }

    Some(candidates[0])
}

pub(crate) fn function_singleton_safe(src_file: &str, tgt_file: &str) -> bool {
    if src_file == tgt_file {
        return true;
    }
    let src_crate = path_scope(src_file);
    let tgt_crate = path_scope(tgt_file);
    if src_crate.is_none() || src_crate != tgt_crate {
        return false;
    }
    // Same crate scope is not enough: one crates/<name> tree can hold multiple
    // languages (spur-notebook = Rust rest-table-gateway + TS jute-notebook).
    // A call never crosses a language boundary, so require the same family.
    matches!(
        (language_family(src_file), language_family(tgt_file)),
        (Some(a), Some(b)) if a == b
    )
}

fn language_family(path: &str) -> Option<&'static str> {
    let ext = path.rsplit('.').next()?;
    if ext == path {
        return None;
    }
    Some(match ext {
        "rs" => "rust",
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => "js",
        "py" | "pyi" => "python",
        "cpp" | "cc" | "cxx" | "c" | "h" | "hpp" | "hxx" => "cpp",
        "md" | "markdown" => "markdown",
        _ => return None,
    })
}

fn path_crate(path: &str) -> Option<&str> {
    let stripped = path.strip_prefix("crates/")?;
    let end = stripped.find('/')?;
    if end == 0 {
        return None;
    }
    Some(&stripped[..end])
}

fn path_scope(path: &str) -> Option<&str> {
    if let Some(krate) = path_crate(path) {
        return Some(krate);
    }
    if path.starts_with("crates/") {
        return None;
    }
    path.split('/').next().filter(|segment| !segment.is_empty())
}

fn method_scope_candidates(
    edge: &PendingEdge,
    candidates: Option<&Vec<NodeId>>,
    node_kind_by_id: &HashMap<NodeId, NodeKind>,
    enclosing_scope_by_id: &HashMap<NodeId, String>,
) -> Vec<NodeId> {
    if edge.scope_text.is_none() {
        return Vec::new();
    }
    let Some(candidates) = candidates else {
        return Vec::new();
    };

    let mut matches = candidates
        .iter()
        .copied()
        .filter(|target| *target != edge.source)
        .filter(|target| matches!(node_kind_by_id.get(target).copied(), Some(NodeKind::Method)))
        .filter(|target| method_scope_matches(edge, *target, enclosing_scope_by_id))
        .collect::<Vec<_>>();
    matches.sort_by_key(|id| id.get());
    matches.dedup();
    matches
}

fn qualified_edge_candidates(
    edge: &PendingEdge,
    qualified_symbols_by_name: &HashMap<String, Vec<NodeId>>,
) -> Vec<NodeId> {
    let mut candidates = Vec::new();
    if edge.target_name.contains("::") {
        if let Some(indexed) = qualified_symbols_by_name.get(&edge.target_name) {
            candidates.extend(indexed.iter().copied());
        }
    }
    if let Some(scope_text) = edge.scope_text.as_deref() {
        let qualified_name = format!("{}::{}", scope_text.trim(), edge.target_name);
        if let Some(indexed) = qualified_symbols_by_name.get(&qualified_name) {
            candidates.extend(indexed.iter().copied());
        }
    }
    candidates.sort_by_key(|id| id.get());
    candidates.dedup();
    candidates
}

fn method_scope_matches(
    edge: &PendingEdge,
    target: NodeId,
    enclosing_scope_by_id: &HashMap<NodeId, String>,
) -> bool {
    let Some(scope_text) = edge.scope_text.as_deref() else {
        return false;
    };
    let Some(enclosing_scope) = enclosing_scope_by_id.get(&target) else {
        return false;
    };
    let matched_scope = if scope_text.trim() == "Self" {
        enclosing_scope_by_id
            .get(&edge.source)
            .cloned()
            .unwrap_or_else(|| canonical_method_scope_text(scope_text))
    } else {
        canonical_method_scope_text(scope_text)
    };
    method_scope_text_matches(&matched_scope, enclosing_scope)
}

fn method_scope_text_matches(matched_scope: &str, enclosing_scope: &str) -> bool {
    if matched_scope == enclosing_scope {
        return true;
    }
    let Some(receiver_type) = matched_scope.strip_prefix("impl ") else {
        return false;
    };
    let Some((_, impl_self_type)) = enclosing_scope
        .strip_prefix("impl ")
        .and_then(|scope| scope.rsplit_once(" for "))
    else {
        return false;
    };
    receiver_type == impl_self_type
}

fn canonical_method_scope_text(scope_text: &str) -> String {
    let trimmed = scope_text.trim();
    if trimmed.starts_with("impl ") {
        return trimmed.to_owned();
    }
    if let Some((self_ty, trait_ty)) = qualified_trait_scope(trimmed) {
        return format!("impl {trait_ty} for {self_ty}");
    }
    format!("impl {trimmed}")
}

fn qualified_trait_scope(scope_text: &str) -> Option<(&str, &str)> {
    let inner = scope_text.strip_prefix('<')?.strip_suffix('>')?.trim();
    let (self_ty, trait_ty) = inner.split_once(" as ")?;
    Some((self_ty.trim(), trait_ty.trim()))
}

fn enclosing_scope_by_id(
    facts: &GraphFacts,
    nodes_by_id: &HashMap<NodeId, &GraphNode>,
    parent_by_target: &HashMap<NodeId, NodeId>,
) -> HashMap<NodeId, String> {
    facts
        .nodes
        .iter()
        .filter_map(|node| {
            let parent = parent_by_target
                .get(&node.node_id)
                .and_then(|id| nodes_by_id.get(id).copied())?;
            match parent.kind {
                NodeKind::File => None,
                NodeKind::Impl => Some((
                    node.node_id,
                    format!(
                        "impl {}",
                        parent.label.strip_prefix("impl ").unwrap_or(&parent.label)
                    ),
                )),
                _ => Some((node.node_id, parent.label.clone())),
            }
        })
        .collect()
}

fn file_by_id_from_maps<'a>(
    facts: &GraphFacts,
    nodes_by_id: &HashMap<NodeId, &GraphNode>,
    parent_by_target: &HashMap<NodeId, NodeId>,
    file_path_by_file_node: &'a HashMap<NodeId, String>,
) -> HashMap<NodeId, &'a str> {
    facts
        .nodes
        .iter()
        .filter_map(|node| {
            let file_node_id = file_node_id_for(node, nodes_by_id, parent_by_target)?;
            let file_path = file_path_by_file_node.get(&file_node_id)?;
            Some((node.node_id, file_path.as_str()))
        })
        .collect()
}

fn file_node_id_for(
    node: &GraphNode,
    nodes_by_id: &HashMap<NodeId, &GraphNode>,
    parent_by_target: &HashMap<NodeId, NodeId>,
) -> Option<NodeId> {
    if node.kind == NodeKind::File {
        return Some(node.node_id);
    }

    let mut current = node;
    let mut seen = HashSet::new();
    seen.insert(node.node_id);
    while let Some(parent) = parent_by_target
        .get(&current.node_id)
        .and_then(|id| nodes_by_id.get(id).copied())
    {
        if !seen.insert(parent.node_id) {
            return None;
        }
        if parent.kind == NodeKind::File {
            return Some(parent.node_id);
        }
        current = parent;
    }
    None
}

fn metadata_for_pending_edge(
    edge: &PendingEdge,
    target: Option<NodeId>,
    bind_method: Option<&'static str>,
) -> EdgeMetadata {
    if edge.origin == CallOrigin::MacroBody
        && edge.relation == RelationKind::Calls
        && target.is_some()
    {
        return EdgeMetadata {
            confidence: Confidence::Heuristic,
            confidence_score: 0.8,
            bind_method: Some("macro_body_singleton"),
        };
    }

    let (confidence, confidence_score) = confidence_for_edge(edge.relation, edge.edge_kind);
    EdgeMetadata {
        confidence,
        confidence_score,
        bind_method,
    }
}

fn confidence_for_edge(
    relation: RelationKind,
    edge_kind: Option<GraphEdgeKind>,
) -> (Confidence, f32) {
    if edge_kind == Some(GraphEdgeKind::CallsDyn) {
        return (Confidence::Heuristic, 0.8);
    }
    match relation {
        RelationKind::Contains | RelationKind::Calls | RelationKind::Constructs => {
            (Confidence::SyntaxExact, 1.0)
        }
        RelationKind::Imports | RelationKind::Links => (Confidence::Heuristic, 0.8),
        _ => (Confidence::Heuristic, 0.5),
    }
}

pub fn build_facts(
    root: &Path,
    progress: Option<ProgressBar>,
) -> anyhow::Result<(GraphFacts, BTreeMap<&'static str, usize>)> {
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize `{}`", root.display()))?;
    let (groups, file_counts) = discover_language_groups(&root)?;
    let extract_groups: Vec<_> = groups
        .into_iter()
        .map(|group| (group.language, group.label, group.config, group.files))
        .collect();
    let facts = extract_files(&root, extract_groups, progress.as_ref())?;
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
    extract_files(&root, extract_groups, None)
}

fn extract_files(
    root: &Path,
    groups: Vec<(Language, &'static str, LanguageConfig, Vec<PathBuf>)>,
    progress: Option<&ProgressBar>,
) -> anyhow::Result<GraphFacts> {
    let mut builder = FactBuilder::new(root);

    for (language, label, config, files) in groups {
        let mut extractor = BytesExtractor::new(language, config).map_err(|err| {
            anyhow!("failed to configure tree-sitter parser for `{label}`: {err}")
        })?;
        for path in files {
            if let Some(progress) = progress {
                progress.set_message(progress_file_message(root, &path));
            }
            let source_bytes = match fs::read(&path) {
                Ok(source) => source,
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "spur-graph: skipping file (read failed)"
                    );
                    if let Some(progress) = progress {
                        progress.inc(1);
                    }
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
            if let Some(progress) = progress {
                progress.inc(1);
            }
        }
    }
    builder.resolve_pending_edges();
    Ok(builder.facts)
}

fn progress_file_message(root: &Path, path: &Path) -> String {
    let display_path = path
        .strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string();
    truncate_progress_message(&display_path)
}

fn truncate_progress_message(message: &str) -> String {
    const MAX_CHARS: usize = 96;
    if message.chars().count() <= MAX_CHARS {
        return message.to_owned();
    }

    let tail: String = message
        .chars()
        .rev()
        .take(MAX_CHARS.saturating_sub(3))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("...{tail}")
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
    let mut definitions = emit_definitions(
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
        let edge_definitions = emit_definitions_with_parents(
            config,
            builder,
            &relative_path,
            file_id,
            file_node,
            source,
            &edge_captures,
            &definitions,
        );
        definitions.extend(edge_definitions);
        emit_edges(
            config,
            builder,
            file_node,
            source,
            &definitions,
            &edge_captures,
        );
    }
    if let Some(jsx_edges) = queries.jsx_edges.as_ref() {
        let edge_captures = run_query(jsx_edges, root_node, source);
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
    let mut jsx_edges = None;
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
            "jsx-edges" => {
                jsx_edges = Some(Query::new(&config.language, source).with_context(|| {
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
        jsx_edges,
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
                name: capture_names[capture.index as usize].to_owned(),
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
    fqn.strip_prefix("impl ").unwrap_or(fqn).to_owned()
}

fn qualified_symbols_by_name_from_maps(
    facts: &GraphFacts,
    nodes_by_id: &HashMap<NodeId, &GraphNode>,
    parent_by_target: &HashMap<NodeId, NodeId>,
) -> HashMap<String, Vec<NodeId>> {
    let mut index: HashMap<String, Vec<NodeId>> = HashMap::new();
    for node in &facts.nodes {
        if matches!(node.kind, NodeKind::File | NodeKind::McpTool) {
            continue;
        }
        let qualified_name = qualified_node_name(node, nodes_by_id, parent_by_target);
        index.entry(qualified_name).or_default().push(node.node_id);
    }
    index
}

fn qualified_name_by_id_from_index(
    index: &HashMap<String, Vec<NodeId>>,
) -> HashMap<NodeId, String> {
    let mut qualified_name_by_id = HashMap::new();
    for (qualified_name, ids) in index {
        for id in ids {
            qualified_name_by_id.insert(*id, qualified_name.clone());
        }
    }
    qualified_name_by_id
}

fn parent_by_target(facts: &GraphFacts) -> HashMap<NodeId, NodeId> {
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
    parent_by_target
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
            .to_owned(),
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
    let parent_by_target = parent_by_target(facts);

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
        assert_eq!(
            symbol_query_policy(Language::Python),
            SymbolQueryPolicy::ReuseTags
        );
        assert!(matches!(
            symbol_query_policy(Language::Markdown),
            SymbolQueryPolicy::Dedicated(_)
        ));
    }

    #[test]
    fn function_singleton_safe_same_file_allows() {
        assert!(function_singleton_safe(
            "crates/spur-graph/src/git_walk.rs",
            "crates/spur-graph/src/git_walk.rs"
        ));
    }

    #[test]
    fn function_singleton_safe_same_crate_allows() {
        assert!(function_singleton_safe(
            "crates/spur-graph/src/git_walk.rs",
            "crates/spur-graph/src/temporal.rs"
        ));
    }

    #[test]
    fn function_singleton_safe_cross_crate_blocks() {
        assert!(!function_singleton_safe(
            "crates/spur-bot/src/foo.rs",
            "crates/spur-graph/src/git_walk.rs"
        ));
    }

    #[test]
    fn function_singleton_safe_non_crate_path() {
        assert!(!function_singleton_safe(
            "xtask/src/main.rs",
            "crates/spur-graph/src/git_walk.rs"
        ));
    }

    #[test]
    fn function_singleton_safe_cross_language_blocks() {
        assert!(!function_singleton_safe(
            "crates/spur-notebook/jute-notebook/x.ts",
            "crates/spur-notebook/rest-table-gateway/y.rs"
        ));
    }

    #[test]
    fn function_singleton_safe_same_language_family_allows() {
        assert!(function_singleton_safe(
            "crates/foo/src/a.ts",
            "crates/foo/web/b.tsx"
        ));
    }

    #[test]
    fn singleton_function_call_respects_crate_safety() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("crates/source/src")).expect("source crate dir");
        std::fs::create_dir_all(dir.path().join("crates/callee/src")).expect("callee crate dir");
        std::fs::write(
            dir.path().join("crates/source/src/lib.rs"),
            r#"
pub fn caller() {
    cross_crate_helper();
    same_crate_helper();
    same_file_helper();
}

pub fn same_file_helper() {}
"#,
        )
        .expect("write source lib");
        std::fs::write(
            dir.path().join("crates/source/src/helpers.rs"),
            "pub fn same_crate_helper() {}\n",
        )
        .expect("write source helper");
        std::fs::write(
            dir.path().join("crates/callee/src/lib.rs"),
            "pub fn cross_crate_helper() {}\n",
        )
        .expect("write callee lib");

        let (facts, _counts) = build_facts(dir.path(), None).expect("build facts");
        let call = |label: &str| {
            facts
                .edges
                .iter()
                .find(|edge| {
                    edge.relation == RelationKind::Calls
                        && edge.target_label.as_deref() == Some(label)
                })
                .unwrap_or_else(|| panic!("missing call edge for {label}"))
        };

        let cross_crate = call("cross_crate_helper");
        assert_eq!(cross_crate.target_node_id, None);
        assert_eq!(cross_crate.bind_method.as_deref(), None);

        let same_crate = call("same_crate_helper");
        assert!(same_crate.target_node_id.is_some());
        assert_eq!(same_crate.bind_method.as_deref(), Some("singleton"));

        let same_file = call("same_file_helper");
        assert!(same_file.target_node_id.is_some());
        assert_eq!(same_file.bind_method.as_deref(), Some("singleton"));
    }

    #[test]
    fn method_crate_singleton_recovers_cross_module_same_crate() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("crates/foo/src/sub")).expect("foo sub dir");
        std::fs::create_dir_all(dir.path().join("crates/bar/src")).expect("bar crate dir");
        std::fs::write(
            dir.path().join("crates/foo/src/a.rs"),
            r#"
pub struct Widget;

impl Widget {
    pub fn repaint_panel(&self) {}
    pub fn clone(&self) {}
}
"#,
        )
        .expect("write foo a");
        std::fs::write(
            dir.path().join("crates/foo/src/sub/b.rs"),
            r#"
pub fn caller(panel: &Panel, shadow: &Shadow) {
    panel.repaint_panel();
    panel.repaint_from_bar();
    shadow.clone();
}
"#,
        )
        .expect("write foo b");
        std::fs::write(
            dir.path().join("crates/bar/src/lib.rs"),
            r#"
pub struct ExternalPanel;

impl ExternalPanel {
    pub fn repaint_from_bar(&self) {}
}
"#,
        )
        .expect("write bar lib");

        let (facts, _counts) = build_facts(dir.path(), None).expect("build facts");
        let call = |label: &str| {
            facts
                .edges
                .iter()
                .find(|edge| {
                    edge.relation == RelationKind::Calls
                        && edge.target_label.as_deref() == Some(label)
                })
                .unwrap_or_else(|| panic!("missing call edge for {label}"))
        };

        let same_crate = call("repaint_panel");
        assert!(same_crate.target_node_id.is_some());
        assert_eq!(
            same_crate.bind_method.as_deref(),
            Some("method_crate_singleton")
        );

        let cross_crate = call("repaint_from_bar");
        assert_eq!(cross_crate.target_node_id, None);
        assert_eq!(cross_crate.bind_method.as_deref(), None);

        let std_named = call("clone");
        assert_eq!(std_named.target_node_id, None);
        assert_eq!(std_named.bind_method.as_deref(), None);
    }

    #[test]
    fn method_crate_singleton_does_not_capture_ts_builtin_methods() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("crates/foo/web")).expect("foo web dir");
        std::fs::write(
            dir.path().join("crates/foo/web/grid.ts"),
            r#"
export class GridApi {
    forEach(callback: (value: number) => void) {}
    setCellType(kind: string) {}
}
"#,
        )
        .expect("write grid ts");
        std::fs::write(
            dir.path().join("crates/foo/web/app.ts"),
            r#"
export function caller(values: number[], grid: GridApi) {
    values.forEach((value) => value);
    grid.setCellType("text");
}
"#,
        )
        .expect("write app ts");

        let (facts, _counts) = build_facts(dir.path(), None).expect("build facts");
        let call = |label: &str| {
            facts
                .edges
                .iter()
                .find(|edge| {
                    edge.relation == RelationKind::Calls
                        && edge.target_label.as_deref() == Some(label)
                })
                .unwrap_or_else(|| panic!("missing call edge for {label}"))
        };

        let builtin_named = call("forEach");
        assert_eq!(builtin_named.target_node_id, None);
        assert_eq!(builtin_named.bind_method.as_deref(), None);

        let domain_named = call("setCellType");
        assert!(domain_named.target_node_id.is_some());
        assert_eq!(
            domain_named.bind_method.as_deref(),
            Some("method_crate_singleton")
        );
    }

    #[test]
    fn singleton_function_call_blocks_cross_language_same_crate() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("crates/mixed/web")).expect("mixed web dir");
        std::fs::create_dir_all(dir.path().join("crates/mixed/src")).expect("mixed src dir");
        std::fs::write(
            dir.path().join("crates/mixed/web/app.ts"),
            r#"
export function caller() {
    rust_only_helper();
}
"#,
        )
        .expect("write mixed ts");
        std::fs::write(
            dir.path().join("crates/mixed/src/lib.rs"),
            "pub fn rust_only_helper() {}\n",
        )
        .expect("write mixed rust");

        let (facts, _counts) = build_facts(dir.path(), None).expect("build facts");
        let call = facts
            .edges
            .iter()
            .find(|edge| {
                edge.relation == RelationKind::Calls
                    && edge.target_label.as_deref() == Some("rust_only_helper")
            })
            .expect("missing call edge for rust_only_helper");

        assert_eq!(call.target_node_id, None);
        assert_eq!(call.bind_method.as_deref(), None);
    }

    #[test]
    fn singleton_hof_reference_respects_crate_safety() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("crates/source/src")).expect("source crate dir");
        std::fs::create_dir_all(dir.path().join("crates/callee/src")).expect("callee crate dir");
        std::fs::write(
            dir.path().join("crates/source/src/lib.rs"),
            r#"
pub fn caller(items: Vec<i32>) {
    let _ = items.iter().copied().map(cross_crate_mapper).collect::<Vec<_>>();
    let _ = items.iter().copied().map(same_crate_mapper).collect::<Vec<_>>();
    let _ = items.iter().copied().map(same_file_mapper).collect::<Vec<_>>();
}

pub fn same_file_mapper(value: i32) -> i32 { value }
"#,
        )
        .expect("write source lib");
        std::fs::write(
            dir.path().join("crates/source/src/helpers.rs"),
            "pub fn same_crate_mapper(value: i32) -> i32 { value }\n",
        )
        .expect("write source helper");
        std::fs::write(
            dir.path().join("crates/callee/src/lib.rs"),
            "pub fn cross_crate_mapper(value: i32) -> i32 { value }\n",
        )
        .expect("write callee lib");

        let (facts, _counts) = build_facts(dir.path(), None).expect("build facts");
        let reference = |label: &str| {
            facts
                .edges
                .iter()
                .find(|edge| {
                    edge.relation == RelationKind::References
                        && edge.target_label.as_deref() == Some(label)
                })
                .unwrap_or_else(|| panic!("missing reference edge for {label}"))
        };

        let cross_crate = reference("cross_crate_mapper");
        assert_eq!(cross_crate.target_node_id, None);

        let same_crate = reference("same_crate_mapper");
        assert!(same_crate.target_node_id.is_some());

        let same_file = reference("same_file_mapper");
        assert!(same_file.target_node_id.is_some());
    }

    #[test]
    fn path_crate_extracts_correctly() {
        assert_eq!(
            path_crate("crates/spur-graph/src/git_walk.rs"),
            Some("spur-graph")
        );
        assert_eq!(path_crate("xtask/src/main.rs"), None);
        assert_eq!(path_crate("README.md"), None);
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
            "caller".to_owned(),
            "caller".to_owned(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        builder.add_node(
            "src/lib.rs",
            "flush".to_owned(),
            "flush".to_owned(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        builder.add_node(
            "src/lib.rs",
            "flush".to_owned(),
            "inner::flush".to_owned(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        builder.pending_edges.push(PendingEdge {
            source,
            target_name: "flush".to_owned(),
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
    fn bare_call_ignores_non_callable_same_label_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("configure parser");
        let tree = parser
            .parse(
                "fn caller() { helper(); }\nfn helper() {}\nstruct App { helper: Helper }\n",
                None,
            )
            .expect("parse source");
        let root_node = tree.root_node();

        let mut builder = FactBuilder::new(dir.path());
        let file_id = FileId(builder.next_file_id());
        let source = builder.add_node(
            "src/lib.rs",
            "caller".to_owned(),
            "caller".to_owned(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        let function = builder.add_node(
            "src/lib.rs",
            "helper".to_owned(),
            "helper".to_owned(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        builder.add_node(
            "src/lib.rs",
            "helper".to_owned(),
            "App::helper".to_owned(),
            NodeKind::Field,
            file_id,
            root_node,
        );
        builder.pending_edges.push(PendingEdge {
            source,
            target_name: "helper".to_owned(),
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
        assert_eq!(edge.target_node_id, Some(function));
        assert_eq!(edge.target_label.as_deref(), Some("helper"));
        assert_eq!(edge.bind_method.as_deref(), Some("singleton"));
    }

    #[test]
    fn constructs_type_singleton_binds_unique_type() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("configure parser");
        let tree = parser
            .parse(
                "fn caller() { let _ = Widget { value: 1 }; }\nstruct Widget { value: i32 }\nfn Widget() {}\n",
                None,
            )
            .expect("parse source");
        let root_node = tree.root_node();

        let mut builder = FactBuilder::new(dir.path());
        let file_id = FileId(builder.next_file_id());
        let file = builder.add_file_node("src/lib.rs", file_id, root_node);
        let source = builder.add_node(
            "src/lib.rs",
            "caller".to_owned(),
            "caller".to_owned(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        let target = builder.add_node(
            "src/lib.rs",
            "Widget".to_owned(),
            "Widget".to_owned(),
            NodeKind::Struct,
            file_id,
            root_node,
        );
        builder.add_node(
            "src/lib.rs",
            "Widget".to_owned(),
            "Widget".to_owned(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        builder.add_edge(file, Some(source), RelationKind::Contains, None);
        builder.add_edge(file, Some(target), RelationKind::Contains, None);
        builder.pending_edges.push(PendingEdge {
            source,
            target_name: "Widget".to_owned(),
            relation: RelationKind::Constructs,
            edge_kind: None,
            origin: CallOrigin::Expression,
            receiver_text: None,
            scope_text: None,
        });

        builder.resolve_pending_edges();

        let edge = builder
            .facts
            .edges
            .iter()
            .find(|edge| {
                edge.relation == RelationKind::Constructs
                    && edge.target_label.as_deref() == Some("Widget")
            })
            .expect("constructs edge");
        assert_eq!(edge.source_node_id, source);
        assert_eq!(edge.target_node_id, Some(target));
        assert_eq!(
            edge.bind_method.as_deref(),
            Some("constructs_type_singleton")
        );
    }

    #[test]
    fn constructs_type_singleton_ambiguous_multi_type_unresolved() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("configure parser");
        let tree = parser
            .parse(
                "fn caller() { let _ = Widget { value: 1 }; }\nstruct Widget { value: i32 }\nenum Widget { Value }\n",
                None,
            )
            .expect("parse source");
        let root_node = tree.root_node();

        let mut builder = FactBuilder::new(dir.path());
        let file_id = FileId(builder.next_file_id());
        let file = builder.add_file_node("src/lib.rs", file_id, root_node);
        let source = builder.add_node(
            "src/lib.rs",
            "caller".to_owned(),
            "caller".to_owned(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        let first = builder.add_node(
            "src/lib.rs",
            "Widget".to_owned(),
            "Widget".to_owned(),
            NodeKind::Struct,
            file_id,
            root_node,
        );
        let second = builder.add_node(
            "src/lib.rs",
            "Widget".to_owned(),
            "Widget".to_owned(),
            NodeKind::Enum,
            file_id,
            root_node,
        );
        builder.add_edge(file, Some(source), RelationKind::Contains, None);
        builder.add_edge(file, Some(first), RelationKind::Contains, None);
        builder.add_edge(file, Some(second), RelationKind::Contains, None);
        builder.pending_edges.push(PendingEdge {
            source,
            target_name: "Widget".to_owned(),
            relation: RelationKind::Constructs,
            edge_kind: None,
            origin: CallOrigin::Expression,
            receiver_text: None,
            scope_text: None,
        });

        builder.resolve_pending_edges();

        let edge = builder
            .facts
            .edges
            .iter()
            .find(|edge| {
                edge.relation == RelationKind::Constructs
                    && edge.target_label.as_deref() == Some("Widget")
            })
            .expect("constructs edge");
        assert_eq!(edge.source_node_id, source);
        assert_eq!(edge.target_node_id, None);
        assert_eq!(edge.bind_method.as_deref(), None);
    }

    #[test]
    fn constructs_type_singleton_blocks_cross_language() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("configure parser");
        let tree = parser
            .parse("fn caller() {}\nstruct Widget;\n", None)
            .expect("parse source");
        let root_node = tree.root_node();

        let mut builder = FactBuilder::new(dir.path());
        let ts_file_id = FileId(builder.next_file_id());
        let rs_file_id = FileId(builder.next_file_id());
        let ts_file = builder.add_file_node("crates/mixed/web/app.ts", ts_file_id, root_node);
        let rs_file = builder.add_file_node("crates/mixed/src/lib.rs", rs_file_id, root_node);
        let source = builder.add_node(
            "crates/mixed/web/app.ts",
            "caller".to_owned(),
            "caller".to_owned(),
            NodeKind::Function,
            ts_file_id,
            root_node,
        );
        let target = builder.add_node(
            "crates/mixed/src/lib.rs",
            "Widget".to_owned(),
            "Widget".to_owned(),
            NodeKind::Struct,
            rs_file_id,
            root_node,
        );
        builder.add_edge(ts_file, Some(source), RelationKind::Contains, None);
        builder.add_edge(rs_file, Some(target), RelationKind::Contains, None);
        builder.pending_edges.push(PendingEdge {
            source,
            target_name: "Widget".to_owned(),
            relation: RelationKind::Constructs,
            edge_kind: None,
            origin: CallOrigin::Expression,
            receiver_text: None,
            scope_text: None,
        });

        builder.resolve_pending_edges();

        let edge = builder
            .facts
            .edges
            .iter()
            .find(|edge| {
                edge.relation == RelationKind::Constructs
                    && edge.target_label.as_deref() == Some("Widget")
            })
            .expect("constructs edge");
        assert_eq!(edge.source_node_id, source);
        assert_eq!(edge.target_node_id, None);
        assert_eq!(edge.bind_method.as_deref(), None);
    }

    #[test]
    fn relational_candidates_excludes_cross_language() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("configure parser");
        let tree = parser
            .parse("class Child(Base): pass\ninterface Base {}\n", None)
            .expect("parse source");
        let root_node = tree.root_node();

        let mut builder = FactBuilder::new(dir.path());
        let py_source_file_id = FileId(builder.next_file_id());
        let py_target_file_id = FileId(builder.next_file_id());
        let ts_file_id = FileId(builder.next_file_id());
        builder.add_file_node("pkg/widgets.py", py_source_file_id, root_node);
        builder.add_file_node("pkg/base.py", py_target_file_id, root_node);
        builder.add_file_node("pkg/types.ts", ts_file_id, root_node);
        let source = builder.add_node(
            "pkg/widgets.py",
            "Child".to_owned(),
            "Child".to_owned(),
            NodeKind::Class,
            py_source_file_id,
            root_node,
        );
        let py_base = builder.add_node(
            "pkg/base.py",
            "Base".to_owned(),
            "Base".to_owned(),
            NodeKind::Class,
            py_target_file_id,
            root_node,
        );
        let ts_base = builder.add_node(
            "pkg/types.ts",
            "Base".to_owned(),
            "Base".to_owned(),
            NodeKind::Interface,
            ts_file_id,
            root_node,
        );
        let edge = PendingEdge {
            source,
            target_name: "Base".to_owned(),
            relation: RelationKind::Extends,
            edge_kind: None,
            origin: CallOrigin::Expression,
            receiver_text: None,
            scope_text: None,
        };
        let singleton_symbols_by_label = HashMap::<String, NodeId>::new();
        let ambiguous_symbols_by_label = HashMap::<String, usize>::new();
        let files_by_label = HashMap::<String, NodeId>::new();
        let file_by_id = HashMap::from([
            (source, "pkg/widgets.py"),
            (py_base, "pkg/base.py"),
            (ts_base, "pkg/types.ts"),
        ]);
        let node_kind_by_id = HashMap::from([
            (source, NodeKind::Class),
            (py_base, NodeKind::Class),
            (ts_base, NodeKind::Interface),
        ]);
        let enclosing_scope_by_id = HashMap::<NodeId, String>::new();
        let qualified_name_by_id = HashMap::<NodeId, String>::new();
        let indexes = PendingResolutionIndexes {
            singleton_symbols_by_label: &singleton_symbols_by_label,
            ambiguous_symbols_by_label: &ambiguous_symbols_by_label,
            files_by_label: &files_by_label,
            file_by_id: &file_by_id,
            node_kind_by_id: &node_kind_by_id,
            enclosing_scope_by_id: &enclosing_scope_by_id,
            qualified_name_by_id: &qualified_name_by_id,
        };

        let candidates = relational_symbol_candidates(
            &builder,
            &edge,
            &indexes,
            relational_target_kinds(edge.relation).expect("relational target kinds"),
        );

        assert_eq!(candidates, vec![py_base]);
    }

    #[test]
    fn relational_language_gate_resolves_in_repo_base_class() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("pkg")).expect("mkdir pkg");
        fs::write(
            root.join("pkg/widget.py"),
            r#"
class Child(Base):
    pass
"#,
        )
        .expect("write widget.py");
        fs::write(
            root.join("pkg/base.py"),
            r#"
class Base:
    pass
"#,
        )
        .expect("write base.py");
        fs::write(
            root.join("pkg/types.ts"),
            r#"
export interface Base {}
"#,
        )
        .expect("write types.ts");

        let facts = build_facts_for_paths(
            root,
            &[
                PathBuf::from("pkg/widget.py"),
                PathBuf::from("pkg/base.py"),
                PathBuf::from("pkg/types.ts"),
            ],
        )
        .expect("build facts");
        let child = facts
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Class && node.label == "Child")
            .expect("Child class");
        let py_base = facts
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Class && node.label == "Base")
            .expect("Python Base class");
        let ts_base = facts
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Interface && node.label == "Base")
            .expect("TypeScript Base interface");

        let edge = facts
            .edges
            .iter()
            .find(|edge| {
                edge.source_node_id == child.node_id
                    && edge.relation == RelationKind::Extends
                    && edge.target_label.as_deref() == Some("Base")
            })
            .expect("Child extends Base edge");

        assert_eq!(edge.target_node_id, Some(py_base.node_id));
        assert_ne!(edge.target_node_id, Some(ts_base.node_id));
        assert_eq!(edge.bind_method.as_deref(), Some("relational"));
    }

    #[test]
    fn constructs_type_singleton_behavior_recovers_type_non_type_collision() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("configure parser");
        let tree = parser
            .parse(
                "fn make_widget() {}\nstruct Widget { value: i32 }\nfn Widget() {}\n",
                None,
            )
            .expect("parse source");
        let root_node = tree.root_node();

        let mut builder = FactBuilder::new(dir.path());
        let source_file_id = FileId(builder.next_file_id());
        let target_file_id = FileId(builder.next_file_id());
        let source_file = builder.add_file_node("crates/app/src/lib.rs", source_file_id, root_node);
        let target_file =
            builder.add_file_node("crates/types/src/lib.rs", target_file_id, root_node);
        let source = builder.add_node(
            "crates/app/src/lib.rs",
            "make_widget".to_owned(),
            "make_widget".to_owned(),
            NodeKind::Function,
            source_file_id,
            root_node,
        );
        let target = builder.add_node(
            "crates/types/src/lib.rs",
            "Widget".to_owned(),
            "Widget".to_owned(),
            NodeKind::Struct,
            target_file_id,
            root_node,
        );
        let non_type = builder.add_node(
            "crates/types/src/lib.rs",
            "Widget".to_owned(),
            "Widget".to_owned(),
            NodeKind::Function,
            target_file_id,
            root_node,
        );
        builder.add_edge(source_file, Some(source), RelationKind::Contains, None);
        builder.add_edge(target_file, Some(target), RelationKind::Contains, None);
        builder.add_edge(target_file, Some(non_type), RelationKind::Contains, None);
        builder.pending_edges.push(PendingEdge {
            source,
            target_name: "Widget".to_owned(),
            relation: RelationKind::Constructs,
            edge_kind: None,
            origin: CallOrigin::Expression,
            receiver_text: None,
            scope_text: None,
        });

        builder.resolve_pending_edges();

        let edge = builder
            .facts
            .edges
            .iter()
            .find(|edge| {
                edge.source_node_id == source
                    && edge.relation == RelationKind::Constructs
                    && edge.target_label.as_deref() == Some("Widget")
            })
            .expect("Widget constructs edge");
        assert_eq!(edge.target_node_id, Some(target));
        assert_eq!(
            edge.bind_method.as_deref(),
            Some("constructs_type_singleton")
        );
    }

    #[test]
    fn import_ignores_non_importable_same_label_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut parser = Parser::new();
        let language: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
        parser.set_language(&language).expect("configure parser");
        let tree = parser
            .parse(
                "use crate::utils::helper;\nfn helper() {}\nstruct App { helper: Helper }\n",
                None,
            )
            .expect("parse source");
        let root_node = tree.root_node();

        let mut builder = FactBuilder::new(dir.path());
        let file_id = FileId(builder.next_file_id());
        let source = builder.add_node(
            "src/lib.rs",
            "src/lib.rs".to_owned(),
            "src/lib.rs".to_owned(),
            NodeKind::File,
            file_id,
            root_node,
        );
        let function = builder.add_node(
            "src/utils.rs",
            "helper".to_owned(),
            "helper".to_owned(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        builder.add_node(
            "src/lib.rs",
            "helper".to_owned(),
            "App::helper".to_owned(),
            NodeKind::Field,
            file_id,
            root_node,
        );
        builder.pending_edges.push(PendingEdge {
            source,
            target_name: "helper".to_owned(),
            relation: RelationKind::Imports,
            edge_kind: None,
            origin: CallOrigin::Expression,
            receiver_text: None,
            scope_text: None,
        });

        builder.resolve_pending_edges();

        assert_eq!(builder.facts.edges.len(), 1);
        let edge = &builder.facts.edges[0];
        assert_eq!(edge.source_node_id, source);
        assert_eq!(edge.target_node_id, Some(function));
        assert_eq!(edge.target_label.as_deref(), Some("helper"));
    }

    #[test]
    fn rust_use_list_import_ignores_same_label_field_during_extraction() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        fs::write(
            root.join("src/lib.rs"),
            r#"
use crate::utils::{helper, Helper};

pub struct App {
    helper: Helper,
}

impl App {
    pub fn run(&self) {
        helper();
    }
}
"#,
        )
        .expect("write lib.rs");
        fs::write(
            root.join("src/utils.rs"),
            r#"
pub struct Helper;

pub fn helper() {}
"#,
        )
        .expect("write utils.rs");

        let facts = crate::build_facts(root, None).expect("extract").0;
        let helper = facts
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Function && node.label == "helper")
            .expect("helper function");

        assert!(
            facts.edges.iter().any(|edge| {
                edge.relation == RelationKind::Imports
                    && edge.target_node_id == Some(helper.node_id)
                    && edge.target_label.as_deref() == Some("helper")
            }),
            "expected helper import to resolve to the function, not the same-label field"
        );
        assert!(
            !facts.edges.iter().any(|edge| {
                edge.relation == RelationKind::Imports
                    && edge.target_node_id.is_none()
                    && edge.target_label.as_deref() == Some("helper")
            }),
            "helper import should not also leave an unresolved duplicate edge"
        );

        let artifact = crate::store::build::artifact_from_facts(&facts, root).expect("artifact");
        let helper_artifact = artifact
            .symbols
            .iter()
            .find(|symbol| symbol.symbol_kind == "function" && symbol.entity_name == "helper")
            .expect("helper artifact symbol");
        assert!(
            artifact.edges.iter().any(|edge| {
                edge.relation == RelationKind::Imports
                    && edge.target_stable_symbol_id.as_deref()
                        == Some(helper_artifact.stable_symbol_id.as_str())
                    && edge.target_label.as_deref() == Some("helper")
            }),
            "expected helper import artifact edge to keep the resolved function target"
        );
    }

    #[test]
    fn bare_call_to_cfg_duplicate_free_function_resolves_to_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        fs::create_dir_all(root.join("src")).expect("mkdir src");
        let path = root.join("src/lib.rs");
        let source = r#"
#[cfg(unix)]
fn send_control() {}

#[cfg(not(unix))]
fn send_control() {}

pub fn caller() {
    send_control();
}
"#;
        fs::write(&path, source).expect("write lib.rs");

        let facts =
            build_facts_for_paths(root, &[PathBuf::from("src/lib.rs")]).expect("build facts");
        let caller = facts
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Function && node.label == "caller")
            .expect("caller node");
        let duplicate_targets = facts
            .nodes
            .iter()
            .filter(|node| node.kind == NodeKind::Function && node.label == "send_control")
            .map(|node| node.node_id)
            .collect::<Vec<_>>();
        assert_eq!(duplicate_targets.len(), 2);

        let call_edge = facts
            .edges
            .iter()
            .find(|edge| {
                edge.source_node_id == caller.node_id
                    && edge.relation == RelationKind::Calls
                    && edge.target_label.as_deref() == Some("send_control")
            })
            .expect("send_control call edge");

        let target = call_edge
            .target_node_id
            .expect("cfg duplicate call should resolve to one target");
        assert!(duplicate_targets.contains(&target));
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
            "caller".to_owned(),
            "caller".to_owned(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        let target = builder.add_node(
            "src/lib.rs",
            "helper".to_owned(),
            "helper".to_owned(),
            NodeKind::Function,
            file_id,
            root_node,
        );
        builder.pending_edges.push(PendingEdge {
            source,
            target_name: "helper".to_owned(),
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

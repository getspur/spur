//! Query clients for code graph MCP request handlers.
//!
//! `GraphQueryClient` is the migration boundary from full `GraphIndexArtifact`
//! residency to query-scoped access. `InMemoryClient` preserves the legacy
//! artifact-backed behavior for tests and JSON artifacts, while `ParquetClient`
//! serves hot-path MCP operations directly from Parquet projections so handlers
//! such as `code_search`, `code_read_symbol`, `code_callers`, and `code_callees`
//! do not deserialize and retain the full graph artifact per request.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, bail, Context};
use arrow_array::{
    Array, BooleanArray, Float32Array, Int32Array, Int64Array, ListArray, RecordBatch, StringArray,
};
use arrow_schema::ArrowError;
use globset::Glob;
use parquet::arrow::arrow_reader::{
    ArrowPredicateFn, ArrowReaderMetadata, ParquetRecordBatchReaderBuilder, RowFilter,
};
use parquet::arrow::ProjectionMask;
use parquet::schema::types::SchemaDescriptor;

use crate::store::parquet::{
    confidence_from_str, edge_kind_from_str, read_temporal_artifact_parquet, relation_from_str,
    PARQUET_ROW_GROUP_SIZE,
};
use crate::temporal::TemporalIndex;
use crate::{
    compare_symbols, find_callee_edges, find_caller_edges, read_artifact_header_parquet,
    resolve_selector, search_symbols, GraphArtifactManifest, GraphEdgeArtifact,
    GraphFileManifestEntry, GraphIndexArtifact, GraphSymbolArtifact, OwnedCalleeRecord,
    OwnedCallerRecord, RelationKind, SearchOptions, SearchResult, SearchSymbol, SelectorResolution,
    CODE_SYMBOL_URI_PREFIX,
};
use crate::{CandidateRow, NodeId, ResolvedSymbol, SearchFilters, SearchMode};

pub type CodeSelectorResolution = SelectorResolution;

pub trait GraphQueryClient {
    fn search_symbols(&self, opts: &SearchOptions) -> anyhow::Result<SearchResult>;
    fn find_caller_edges(&self, sid: &str) -> Vec<OwnedCallerRecord>;
    fn find_callee_edges(&self, sid: &str) -> Vec<OwnedCalleeRecord>;
    fn resolve_selector(&self, selector: &str) -> anyhow::Result<CodeSelectorResolution>;
    fn symbol_by_id(&self, sid: &str) -> anyhow::Result<Option<GraphSymbolArtifact>>;
    fn symbols_by_file(&self, path: &str) -> anyhow::Result<Vec<GraphSymbolArtifact>>;
    fn symbols_by_path_name(
        &self,
        path: &str,
        name: &str,
    ) -> anyhow::Result<Vec<GraphSymbolArtifact>>;
    fn file_manifest_by_path(&self, path: &str) -> anyhow::Result<Option<GraphFileManifestEntry>>;
    fn file_exists(&self, path: &str) -> anyhow::Result<bool>;
    fn temporal_index(&self) -> Arc<TemporalIndex>;
}

#[derive(Clone)]
pub struct InMemoryClient {
    artifact: Arc<GraphIndexArtifact>,
}

impl InMemoryClient {
    pub fn new(artifact: Arc<GraphIndexArtifact>) -> Self {
        Self { artifact }
    }

    pub fn artifact(&self) -> &GraphIndexArtifact {
        &self.artifact
    }
}

impl GraphQueryClient for InMemoryClient {
    fn search_symbols(&self, opts: &SearchOptions) -> anyhow::Result<SearchResult> {
        Ok(search_symbols(&self.artifact, opts))
    }

    fn find_caller_edges(&self, sid: &str) -> Vec<OwnedCallerRecord> {
        find_caller_edges(&self.artifact, sid)
            .into_iter()
            .map(OwnedCallerRecord::from)
            .collect()
    }

    fn find_callee_edges(&self, sid: &str) -> Vec<OwnedCalleeRecord> {
        find_callee_edges(&self.artifact, sid)
            .into_iter()
            .map(OwnedCalleeRecord::from)
            .collect()
    }

    fn resolve_selector(&self, selector: &str) -> anyhow::Result<CodeSelectorResolution> {
        Ok(resolve_selector(&self.artifact, selector))
    }

    fn symbol_by_id(&self, sid: &str) -> anyhow::Result<Option<GraphSymbolArtifact>> {
        Ok(self
            .artifact
            .symbols
            .iter()
            .find(|symbol| symbol.stable_symbol_id == sid)
            .cloned())
    }

    fn symbols_by_file(&self, path: &str) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        Ok(self
            .artifact
            .symbols
            .iter()
            .filter(|symbol| symbol.file_path == path)
            .cloned()
            .collect())
    }

    fn symbols_by_path_name(
        &self,
        path: &str,
        name: &str,
    ) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        Ok(self
            .artifact
            .symbols
            .iter()
            .filter(|symbol| {
                symbol.file_path == path
                    && (symbol.entity_name == name || symbol.qualified_name == name)
            })
            .cloned()
            .collect())
    }

    fn file_manifest_by_path(&self, path: &str) -> anyhow::Result<Option<GraphFileManifestEntry>> {
        Ok(self
            .artifact
            .file_manifests
            .iter()
            .find(|entry| entry.path == path)
            .cloned())
    }

    fn file_exists(&self, path: &str) -> anyhow::Result<bool> {
        Ok(self
            .artifact
            .files
            .iter()
            .any(|entry| entry.file_path == path)
            || self
                .artifact
                .file_manifests
                .iter()
                .any(|entry| entry.path == path))
    }

    fn temporal_index(&self) -> Arc<TemporalIndex> {
        Arc::new(TemporalIndex::new(Arc::clone(&self.artifact)))
    }
}

const SEARCH_COLUMNS: [&str; 8] = [
    "stable_symbol_id",
    "file_path",
    "line_start",
    "line_end",
    "entity_name",
    "qualified_name",
    "symbol_kind",
    "enclosing_scope",
];
const SEARCH_PREDICATE_COLUMNS: [&str; 4] =
    ["entity_name", "qualified_name", "file_path", "symbol_kind"];
const FILE_OID_COLUMNS: [&str; 2] = ["path", "content_oid"];
const FILE_MANIFEST_COLUMNS: [&str; 4] = ["stable_file_id", "path", "content_oid", "node_ids"];
const SYMBOL_COLUMNS: [&str; 11] = [
    "stable_symbol_id",
    "file_path",
    "byte_range_start",
    "byte_range_end",
    "line_start",
    "line_end",
    "entity_name",
    "qualified_name",
    "symbol_kind",
    "anchor_hash",
    "enclosing_scope",
];
const RESOLVED_EDGE_COLUMNS: [&str; 8] = [
    "source_stable_id",
    "target_stable_id",
    "target_label",
    "relation",
    "confidence",
    "confidence_score",
    "edge_kind",
    "bind_method",
];
const UNRESOLVED_EDGE_COLUMNS: [&str; 7] = [
    "source_stable_id",
    "target_label",
    "relation",
    "confidence",
    "confidence_score",
    "edge_kind",
    "bind_method",
];

pub struct ParquetClient {
    dir: PathBuf,
    manifest: GraphArtifactManifest,
    nodes_metadata: ArrowReaderMetadata,
    search_projection: ProjectionMask,
    temporal_index: OnceLock<Arc<TemporalIndex>>,
}

impl ParquetClient {
    pub fn open(dir: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let dir = dir.into();
        if !dir.is_dir() {
            bail!(
                "Parquet artifact directory `{}` does not exist",
                dir.display()
            );
        }
        let manifest = read_artifact_header_parquet(&dir)?;
        if !manifest.complete {
            bail!(
                "refusing to load incomplete Parquet artifact `{}`",
                dir.display()
            );
        }
        let nodes_path = dir.join("nodes.parquet");
        let nodes_file = File::open(&nodes_path)
            .with_context(|| format!("failed to open `{}`", nodes_path.display()))?;
        let nodes_metadata = ArrowReaderMetadata::load(&nodes_file, Default::default())
            .with_context(|| format!("failed to read `{}`", nodes_path.display()))?;
        let search_projection = ProjectionMask::columns(
            nodes_metadata.metadata().file_metadata().schema_descr(),
            SEARCH_COLUMNS,
        );
        Ok(Self {
            dir,
            manifest,
            nodes_metadata,
            search_projection,
            temporal_index: OnceLock::new(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn manifest(&self) -> &GraphArtifactManifest {
        &self.manifest
    }

    pub fn file_oids(&self) -> anyhow::Result<Vec<(String, String)>> {
        let batches =
            projected_batches(&self.dir.join("file_manifests.parquet"), FILE_OID_COLUMNS)?;
        let mut rows = Vec::new();
        for batch in batches {
            let path = string_array_by_name(&batch, "path")?;
            let content_oid = string_array_by_name(&batch, "content_oid")?;
            for row in 0..batch.num_rows() {
                rows.push((
                    required_string_value(path, row, "path")?.to_string(),
                    required_string_value(content_oid, row, "content_oid")?.to_string(),
                ));
            }
        }
        Ok(rows)
    }

    fn search_symbols_inner(&self, opts: &SearchOptions) -> anyhow::Result<SearchResult> {
        let nodes_path = self.dir.join("nodes.parquet");
        let file = File::open(&nodes_path)
            .with_context(|| format!("failed to open `{}`", nodes_path.display()))?;
        let row_filter = search_row_filter(
            self.nodes_metadata
                .metadata()
                .file_metadata()
                .schema_descr(),
            opts,
        );
        let reader =
            ParquetRecordBatchReaderBuilder::new_with_metadata(file, self.nodes_metadata.clone())
                .with_batch_size(PARQUET_ROW_GROUP_SIZE)
                .with_projection(self.search_projection.clone())
                .with_row_filter(row_filter)
                .build()
                .with_context(|| {
                    format!(
                        "failed to build Arrow reader for `{}`",
                        nodes_path.display()
                    )
                })?;

        let mut candidates = Vec::new();
        for batch in reader {
            let batch =
                batch.with_context(|| format!("failed to decode `{}`", nodes_path.display()))?;
            candidates.extend(search_symbols_from_batch(&batch)?);
        }
        candidates.sort_by(|left, right| compare_symbols(left, right, opts));

        let total_matches = candidates.len();
        let limit = opts.limit.clamp(1, 200);
        let truncated = total_matches > limit;
        candidates.truncate(limit);

        Ok(SearchResult {
            candidates,
            total_matches,
            truncated,
        })
    }

    pub fn try_find_caller_edges(&self, sid: &str) -> anyhow::Result<Vec<OwnedCallerRecord>> {
        self.find_caller_edges_inner(sid)
    }

    pub fn try_find_callee_edges(&self, sid: &str) -> anyhow::Result<Vec<OwnedCalleeRecord>> {
        self.find_callee_edges_inner(sid)
    }

    fn find_caller_edges_inner(&self, target_sid: &str) -> anyhow::Result<Vec<OwnedCallerRecord>> {
        let Some(target_symbol) = self.symbol_by_stable_id(target_sid)? else {
            return Ok(Vec::new());
        };
        let unresolved_labels = unresolved_target_labels_for_symbol(&target_symbol);
        let resolved_edges = self.resolved_edges_by_target(target_sid)?;
        let unresolved_edges = self.unresolved_edges_by_target_labels(&unresolved_labels)?;
        if resolved_edges.is_empty() && unresolved_edges.is_empty() {
            return Ok(Vec::new());
        }

        let caller_ids = resolved_edges
            .iter()
            .chain(unresolved_edges.iter())
            .filter(|edge| is_caller_relation(edge.relation))
            .map(|edge| edge.source_stable_symbol_id.clone())
            .collect::<HashSet<_>>();
        let callers = self.symbols_by_ids(&caller_ids)?;

        let mut records = Vec::with_capacity(resolved_edges.len() + unresolved_edges.len());
        for edge in resolved_edges {
            if !is_caller_relation(edge.relation) {
                continue;
            }
            if let Some(caller) = callers.get(&edge.source_stable_symbol_id) {
                records.push(OwnedCallerRecord::Resolved {
                    caller: caller.clone(),
                    edge,
                });
            }
        }
        for edge in unresolved_edges {
            if !is_caller_relation(edge.relation) {
                continue;
            }
            if let Some(caller) = callers.get(&edge.source_stable_symbol_id) {
                records.push(OwnedCallerRecord::Unresolved {
                    caller: caller.clone(),
                    target_label: edge.target_label.clone().unwrap_or_default(),
                    edge,
                });
            }
        }
        Ok(records)
    }

    fn find_callee_edges_inner(&self, source_sid: &str) -> anyhow::Result<Vec<OwnedCalleeRecord>> {
        if self.symbol_by_stable_id(source_sid)?.is_none() {
            return Ok(Vec::new());
        }

        let resolved_edges = self.resolved_edges_by_source(source_sid)?;
        let unresolved_edges = self.unresolved_edges_by_source(source_sid)?;
        if resolved_edges.is_empty() && unresolved_edges.is_empty() {
            return Ok(Vec::new());
        }

        let target_ids = resolved_edges
            .iter()
            .filter(|edge| is_caller_relation(edge.relation))
            .filter_map(|edge| edge.target_stable_symbol_id.clone())
            .collect::<HashSet<_>>();
        let targets = self.symbols_by_ids(&target_ids)?;

        let mut records = Vec::with_capacity(resolved_edges.len() + unresolved_edges.len());
        for edge in resolved_edges {
            if !is_caller_relation(edge.relation) {
                continue;
            }
            if let Some(target_id) = edge.target_stable_symbol_id.as_deref() {
                if let Some(symbol) = targets.get(target_id) {
                    records.push(OwnedCalleeRecord::Resolved {
                        symbol: symbol.clone(),
                        edge,
                    });
                }
            }
        }
        for edge in unresolved_edges {
            if !is_caller_relation(edge.relation) {
                continue;
            }
            if let Some(target_label) = edge.target_label.clone() {
                records.push(OwnedCalleeRecord::Unresolved { edge, target_label });
            }
        }
        Ok(records)
    }

    fn symbol_by_stable_id(&self, sid: &str) -> anyhow::Result<Option<GraphSymbolArtifact>> {
        let ids = HashSet::from([sid.to_string()]);
        Ok(self.symbols_by_ids(&ids)?.remove(sid))
    }

    fn symbols_by_ids(
        &self,
        sids: &HashSet<String>,
    ) -> anyhow::Result<HashMap<String, GraphSymbolArtifact>> {
        if sids.is_empty() {
            return Ok(HashMap::new());
        }
        let batches = filtered_projected_batches(
            &self.dir.join("nodes.parquet"),
            SYMBOL_COLUMNS,
            |schema| string_in_row_filter(schema, "stable_symbol_id", sids.clone()),
        )?;
        let mut symbols = HashMap::new();
        for batch in batches {
            for symbol in symbols_from_batch(&batch)? {
                symbols.insert(symbol.stable_symbol_id.clone(), symbol);
            }
        }
        Ok(symbols)
    }

    fn symbols_where_string_eq(
        &self,
        column: &str,
        value: &str,
    ) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        let batches = filtered_projected_batches(
            &self.dir.join("nodes.parquet"),
            SYMBOL_COLUMNS,
            |schema| string_eq_row_filter(schema, column, value.to_string()),
        )?;
        symbols_from_batches(batches)
    }

    fn symbols_where_all_string_eq(
        &self,
        expected: Vec<(&str, String)>,
    ) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        let batches = filtered_projected_batches(
            &self.dir.join("nodes.parquet"),
            SYMBOL_COLUMNS,
            |schema| string_eq_all_row_filter(schema, expected),
        )?;
        symbols_from_batches(batches)
    }

    fn symbols_by_file_path(&self, path: &str) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        self.symbols_where_string_eq("file_path", path)
    }

    fn symbols_by_file_qualified_name(
        &self,
        path: &str,
        qualified_name: &str,
    ) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        self.symbols_where_all_string_eq(vec![
            ("file_path", path.to_string()),
            ("qualified_name", qualified_name.to_string()),
        ])
    }

    fn symbols_by_file_path_name(
        &self,
        path: &str,
        name: &str,
    ) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        let batches = filtered_projected_batches(
            &self.dir.join("nodes.parquet"),
            SYMBOL_COLUMNS,
            |schema| path_name_row_filter(schema, path.to_string(), name.to_string()),
        )?;
        symbols_from_batches(batches)
    }

    fn file_manifest_by_path_inner(
        &self,
        path: &str,
    ) -> anyhow::Result<Option<GraphFileManifestEntry>> {
        let batches = filtered_projected_batches(
            &self.dir.join("file_manifests.parquet"),
            FILE_MANIFEST_COLUMNS,
            |schema| string_eq_row_filter(schema, "path", path.to_string()),
        )?;
        let mut manifests = file_manifests_from_batches(batches)?;
        Ok(manifests.pop())
    }

    fn resolve_selector_inner(&self, selector: &str) -> anyhow::Result<SelectorResolution> {
        let selector = selector.trim();
        if selector.is_empty() {
            return Ok(SelectorResolution::NotFound);
        }

        if let Some(symbol_id) = selector.strip_prefix(CODE_SYMBOL_URI_PREFIX) {
            return Ok(self
                .resolve_symbol_by_id(symbol_id)?
                .map(SelectorResolution::Resolved)
                .unwrap_or(SelectorResolution::NotFound));
        }

        if is_bare_stable_symbol_id(selector) {
            if let Some(symbol) = self.resolve_symbol_by_id(selector)? {
                return Ok(SelectorResolution::Resolved(symbol));
            }
        }

        if let Some(file_scoped) = selector
            .strip_prefix("file:")
            .or_else(|| selector.strip_prefix("path:"))
        {
            return self.resolve_file_scoped(file_scoped);
        }

        if let Some(line_resolution) = self.resolve_line_locator(selector)? {
            return Ok(line_resolution);
        }

        if let Some(file_resolution) = self.resolve_file_qualified(selector)? {
            return Ok(file_resolution);
        }

        if !first_token_contains_path_separator(selector) {
            let resolution =
                resolution_from_symbols(self.symbols_where_string_eq("qualified_name", selector)?);
            if !matches!(resolution, SelectorResolution::NotFound) {
                return Ok(resolution);
            }
        }

        if selector.contains("::") {
            return Ok(SelectorResolution::NotFound);
        }

        Ok(resolution_from_symbols(
            self.symbols_where_string_eq("entity_name", selector)?,
        ))
    }

    fn resolve_symbol_by_id(&self, symbol_id: &str) -> anyhow::Result<Option<ResolvedSymbol>> {
        if symbol_id.is_empty() {
            return Ok(None);
        }
        Ok(self
            .symbol_by_stable_id(symbol_id)?
            .as_ref()
            .map(resolved_symbol))
    }

    fn resolve_file_scoped(&self, selector: &str) -> anyhow::Result<SelectorResolution> {
        if let Some(resolution) = self.resolve_line_locator(selector)? {
            return Ok(resolution);
        }
        Ok(self
            .resolve_file_qualified(selector)?
            .unwrap_or(SelectorResolution::NotFound))
    }

    fn resolve_line_locator(&self, selector: &str) -> anyhow::Result<Option<SelectorResolution>> {
        for (file_path, line) in split_file_prefixes(selector, ":") {
            if line.starts_with(':') {
                continue;
            }
            if line.is_empty() || !line.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            if !self.file_exists_inner(file_path)? {
                continue;
            }
            let Ok(line) = line.parse::<usize>() else {
                continue;
            };
            let symbol = self
                .symbols_by_file_path(file_path)?
                .into_iter()
                .filter(|symbol| symbol.line_range[0] <= line && line <= symbol.line_range[1])
                .max_by(compare_innermost);
            return Ok(Some(
                symbol
                    .as_ref()
                    .map(resolved_symbol)
                    .map(SelectorResolution::Resolved)
                    .unwrap_or(SelectorResolution::NotFound),
            ));
        }
        Ok(None)
    }

    fn resolve_file_qualified(&self, selector: &str) -> anyhow::Result<Option<SelectorResolution>> {
        for (file_path, chain) in split_file_prefixes(selector, "::")
            .into_iter()
            .chain(split_file_prefixes(selector, ":"))
        {
            if !self.file_exists_inner(file_path)? {
                continue;
            }
            let resolution =
                resolution_from_symbols(self.symbols_by_file_qualified_name(file_path, chain)?);
            if !matches!(resolution, SelectorResolution::NotFound) {
                return Ok(Some(resolution));
            }

            let fallback_matches = self
                .symbols_by_file_path(file_path)?
                .into_iter()
                .filter(|symbol| enclosing_scope_entity_name(symbol).as_deref() == Some(chain))
                .collect();
            return Ok(Some(resolution_from_symbols(fallback_matches)));
        }
        Ok(None)
    }

    fn file_exists_inner(&self, path: &str) -> anyhow::Result<bool> {
        Ok(self.file_manifest_by_path_inner(path)?.is_some())
    }

    fn resolved_edges_by_source(&self, source_sid: &str) -> anyhow::Result<Vec<GraphEdgeArtifact>> {
        self.resolved_edges_where("edges.parquet", "source_stable_id", source_sid)
    }

    fn resolved_edges_by_target(&self, target_sid: &str) -> anyhow::Result<Vec<GraphEdgeArtifact>> {
        self.resolved_edges_where("edges_by_dst.parquet", "target_stable_id", target_sid)
    }

    fn resolved_edges_where(
        &self,
        file_name: &str,
        column: &str,
        value: &str,
    ) -> anyhow::Result<Vec<GraphEdgeArtifact>> {
        let path = self.dir.join(file_name);
        let batches = filtered_projected_batches(&path, RESOLVED_EDGE_COLUMNS, |schema| {
            string_eq_row_filter(schema, column, value.to_string())
        })?;
        let mut edges = Vec::new();
        for batch in batches {
            edges.extend(resolved_edges_from_batch(&batch)?);
        }
        Ok(edges)
    }

    fn unresolved_edges_by_source(
        &self,
        source_sid: &str,
    ) -> anyhow::Result<Vec<GraphEdgeArtifact>> {
        let path = self.dir.join("edges_unresolved.parquet");
        let batches = filtered_projected_batches(&path, UNRESOLVED_EDGE_COLUMNS, |schema| {
            string_eq_row_filter(schema, "source_stable_id", source_sid.to_string())
        })?;
        let mut edges = Vec::new();
        for batch in batches {
            edges.extend(unresolved_edges_from_batch(&batch)?);
        }
        Ok(edges)
    }

    fn unresolved_edges_by_target_labels(
        &self,
        labels: &HashSet<String>,
    ) -> anyhow::Result<Vec<GraphEdgeArtifact>> {
        if labels.is_empty() {
            return Ok(Vec::new());
        }
        let path = self.dir.join("edges_unresolved.parquet");
        let batches = filtered_projected_batches(&path, UNRESOLVED_EDGE_COLUMNS, |schema| {
            string_in_row_filter(schema, "target_label", labels.clone())
        })?;
        let mut edges = Vec::new();
        for batch in batches {
            edges.extend(unresolved_edges_from_batch(&batch)?);
        }
        Ok(edges)
    }
}

impl GraphQueryClient for ParquetClient {
    fn search_symbols(&self, opts: &SearchOptions) -> anyhow::Result<SearchResult> {
        self.search_symbols_inner(opts)
    }

    fn find_caller_edges(&self, sid: &str) -> Vec<OwnedCallerRecord> {
        self.try_find_caller_edges(sid)
            .unwrap_or_else(|error| panic!("failed to query Parquet caller edges: {error:#}"))
    }

    fn find_callee_edges(&self, sid: &str) -> Vec<OwnedCalleeRecord> {
        self.try_find_callee_edges(sid)
            .unwrap_or_else(|error| panic!("failed to query Parquet callee edges: {error:#}"))
    }

    fn resolve_selector(&self, selector: &str) -> anyhow::Result<CodeSelectorResolution> {
        self.resolve_selector_inner(selector)
    }

    fn symbol_by_id(&self, sid: &str) -> anyhow::Result<Option<GraphSymbolArtifact>> {
        self.symbol_by_stable_id(sid)
    }

    fn symbols_by_file(&self, path: &str) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        self.symbols_by_file_path(path)
    }

    fn symbols_by_path_name(
        &self,
        path: &str,
        name: &str,
    ) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
        self.symbols_by_file_path_name(path, name)
    }

    fn file_manifest_by_path(&self, path: &str) -> anyhow::Result<Option<GraphFileManifestEntry>> {
        self.file_manifest_by_path_inner(path)
    }

    fn file_exists(&self, path: &str) -> anyhow::Result<bool> {
        self.file_exists_inner(path)
    }

    fn temporal_index(&self) -> Arc<TemporalIndex> {
        Arc::clone(self.temporal_index.get_or_init(|| {
            let artifact = read_temporal_artifact_parquet(&self.dir).unwrap_or_else(|error| {
                panic!(
                    "failed to build Parquet temporal index from `{}`: {error:#}",
                    self.dir.display()
                )
            });
            Arc::new(TemporalIndex::new(Arc::new(artifact)))
        }))
    }
}

fn projected_batches<const N: usize>(
    path: &Path,
    columns: [&str; N],
) -> anyhow::Result<Vec<RecordBatch>> {
    let file = File::open(path).with_context(|| format!("failed to open `{}`", path.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("failed to read `{}`", path.display()))?;
    let projection = ProjectionMask::columns(builder.parquet_schema(), columns);
    builder
        .with_batch_size(PARQUET_ROW_GROUP_SIZE)
        .with_projection(projection)
        .build()
        .with_context(|| format!("failed to build Arrow reader for `{}`", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to decode `{}`", path.display()))
}

fn filtered_projected_batches<const N: usize>(
    path: &Path,
    columns: [&str; N],
    row_filter: impl FnOnce(&SchemaDescriptor) -> RowFilter,
) -> anyhow::Result<Vec<RecordBatch>> {
    let file = File::open(path).with_context(|| format!("failed to open `{}`", path.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("failed to read `{}`", path.display()))?;
    let projection = ProjectionMask::columns(builder.parquet_schema(), columns);
    let row_filter = row_filter(builder.parquet_schema());
    builder
        .with_batch_size(PARQUET_ROW_GROUP_SIZE)
        .with_projection(projection)
        .with_row_filter(row_filter)
        .build()
        .with_context(|| format!("failed to build Arrow reader for `{}`", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("failed to decode `{}`", path.display()))
}

fn search_row_filter(
    parquet_schema: &parquet::schema::types::SchemaDescriptor,
    opts: &SearchOptions,
) -> RowFilter {
    let predicate_columns = search_predicate_columns(opts);
    let projection = ProjectionMask::columns(parquet_schema, predicate_columns);
    let options = opts.clone();
    let glob = options
        .filters
        .file_glob
        .as_deref()
        .and_then(|pattern| Glob::new(pattern).ok())
        .map(|glob| glob.compile_matcher());
    let predicate = move |batch: RecordBatch| -> Result<BooleanArray, ArrowError> {
        let entity_name = string_array_by_name(&batch, "entity_name")?;
        let qualified_name = if matches!(options.mode, SearchMode::Exact) {
            Some(string_array_by_name(&batch, "qualified_name")?)
        } else {
            None
        };
        let file_path = if options.filters.file.is_some() || options.filters.file_glob.is_some() {
            Some(string_array_by_name(&batch, "file_path")?)
        } else {
            None
        };
        let symbol_kind = if options.filters.symbol_kind.is_some() {
            Some(string_array_by_name(&batch, "symbol_kind")?)
        } else {
            None
        };
        let mut keep = Vec::with_capacity(batch.num_rows());
        for row in 0..batch.num_rows() {
            let entity_name = required_string_value(entity_name, row, "entity_name")?;
            let qualified_name = qualified_name
                .map(|array| required_string_value(array, row, "qualified_name"))
                .transpose()?;
            let file_path = file_path
                .map(|array| required_string_value(array, row, "file_path"))
                .transpose()?;
            let symbol_kind = symbol_kind
                .map(|array| required_string_value(array, row, "symbol_kind"))
                .transpose()?;
            keep.push(row_matches(
                entity_name,
                qualified_name,
                file_path,
                symbol_kind,
                &options,
                glob.as_ref(),
            ));
        }
        Ok(BooleanArray::from(keep))
    };
    RowFilter::new(vec![Box::new(ArrowPredicateFn::new(projection, predicate))])
}

fn search_predicate_columns(opts: &SearchOptions) -> Vec<&'static str> {
    let mut columns = Vec::with_capacity(SEARCH_PREDICATE_COLUMNS.len());
    columns.push("entity_name");
    if matches!(opts.mode, SearchMode::Exact) {
        columns.push("qualified_name");
    }
    if opts.filters.file.is_some() || opts.filters.file_glob.is_some() {
        columns.push("file_path");
    }
    if opts.filters.symbol_kind.is_some() {
        columns.push("symbol_kind");
    }
    columns
}

fn row_matches(
    entity_name: &str,
    qualified_name: Option<&str>,
    file_path: Option<&str>,
    symbol_kind: Option<&str>,
    opts: &SearchOptions,
    glob: Option<&globset::GlobMatcher>,
) -> bool {
    if !row_matches_query(entity_name, qualified_name, opts) {
        return false;
    }
    row_matches_filters(file_path, symbol_kind, &opts.filters, glob)
}

fn row_matches_query(
    entity_name: &str,
    qualified_name: Option<&str>,
    opts: &SearchOptions,
) -> bool {
    match opts.mode {
        SearchMode::Exact => {
            entity_name == opts.query || qualified_name == Some(opts.query.as_str())
        }
        SearchMode::Prefix => entity_name.starts_with(&opts.query),
        SearchMode::Substring => entity_name.contains(&opts.query),
    }
}

fn row_matches_filters(
    file_path: Option<&str>,
    symbol_kind: Option<&str>,
    filters: &SearchFilters,
    glob: Option<&globset::GlobMatcher>,
) -> bool {
    if filters
        .symbol_kind
        .as_deref()
        .is_some_and(|filter| symbol_kind != Some(filter))
    {
        return false;
    }

    if filters
        .file
        .as_deref()
        .is_some_and(|filter| file_path != Some(filter))
    {
        return false;
    }

    if filters.file_glob.is_some()
        && !file_path.is_some_and(|path| glob.is_some_and(|glob| glob.is_match(path)))
    {
        return false;
    }

    true
}

fn string_eq_row_filter(
    parquet_schema: &SchemaDescriptor,
    column: &str,
    expected: String,
) -> RowFilter {
    string_in_row_filter(parquet_schema, column, HashSet::from([expected]))
}

fn string_in_row_filter(
    parquet_schema: &SchemaDescriptor,
    column: &str,
    expected: HashSet<String>,
) -> RowFilter {
    let projection = ProjectionMask::columns(parquet_schema, [column]);
    let column = column.to_string();
    let predicate = move |batch: RecordBatch| -> Result<BooleanArray, ArrowError> {
        let values = string_array_by_name(&batch, &column)?;
        let mut keep = Vec::with_capacity(batch.num_rows());
        for row in 0..batch.num_rows() {
            keep.push(!values.is_null(row) && expected.contains(values.value(row)));
        }
        Ok(BooleanArray::from(keep))
    };
    RowFilter::new(vec![Box::new(ArrowPredicateFn::new(projection, predicate))])
}

fn string_eq_all_row_filter(
    parquet_schema: &SchemaDescriptor,
    expected: Vec<(&str, String)>,
) -> RowFilter {
    let columns = expected
        .iter()
        .map(|(column, _)| *column)
        .collect::<Vec<_>>();
    let projection = ProjectionMask::columns(parquet_schema, columns);
    let expected = expected
        .into_iter()
        .map(|(column, value)| (column.to_string(), value))
        .collect::<Vec<_>>();
    let predicate = move |batch: RecordBatch| -> Result<BooleanArray, ArrowError> {
        let arrays = expected
            .iter()
            .map(|(column, _)| string_array_by_name(&batch, column))
            .collect::<Result<Vec<_>, _>>()?;
        let mut keep = Vec::with_capacity(batch.num_rows());
        for row in 0..batch.num_rows() {
            keep.push(arrays.iter().zip(&expected).all(|(values, (_, expected))| {
                !values.is_null(row) && values.value(row) == expected
            }));
        }
        Ok(BooleanArray::from(keep))
    };
    RowFilter::new(vec![Box::new(ArrowPredicateFn::new(projection, predicate))])
}

fn path_name_row_filter(
    parquet_schema: &SchemaDescriptor,
    expected_path: String,
    expected_name: String,
) -> RowFilter {
    let projection = ProjectionMask::columns(
        parquet_schema,
        ["file_path", "entity_name", "qualified_name"],
    );
    let predicate = move |batch: RecordBatch| -> Result<BooleanArray, ArrowError> {
        let file_path = string_array_by_name(&batch, "file_path")?;
        let entity_name = string_array_by_name(&batch, "entity_name")?;
        let qualified_name = string_array_by_name(&batch, "qualified_name")?;
        let mut keep = Vec::with_capacity(batch.num_rows());
        for row in 0..batch.num_rows() {
            let path_matches = !file_path.is_null(row) && file_path.value(row) == expected_path;
            let name_matches = (!entity_name.is_null(row)
                && entity_name.value(row) == expected_name)
                || (!qualified_name.is_null(row) && qualified_name.value(row) == expected_name);
            keep.push(path_matches && name_matches);
        }
        Ok(BooleanArray::from(keep))
    };
    RowFilter::new(vec![Box::new(ArrowPredicateFn::new(projection, predicate))])
}

fn unresolved_target_labels_for_symbol(symbol: &GraphSymbolArtifact) -> HashSet<String> {
    HashSet::from([
        symbol.entity_name.clone(),
        symbol.qualified_name.clone(),
        symbol.stable_symbol_id.clone(),
    ])
}

fn is_caller_relation(relation: RelationKind) -> bool {
    matches!(relation, RelationKind::Calls | RelationKind::References)
}

fn is_bare_stable_symbol_id(selector: &str) -> bool {
    selector.len() >= 16
        && selector
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn split_file_prefixes<'a>(selector: &'a str, separator: &str) -> Vec<(&'a str, &'a str)> {
    let mut prefixes = selector
        .match_indices(separator)
        .filter_map(|(index, _)| {
            let file_path = &selector[..index];
            let tail = &selector[index + separator.len()..];
            (!file_path.is_empty()).then_some((file_path, tail))
        })
        .collect::<Vec<_>>();
    prefixes.sort_by_key(|item| std::cmp::Reverse(item.0.len()));
    prefixes
}

fn first_token_contains_path_separator(selector: &str) -> bool {
    selector
        .split("::")
        .next()
        .is_some_and(|token| token.contains('/'))
}

fn compare_innermost(
    left: &GraphSymbolArtifact,
    right: &GraphSymbolArtifact,
) -> std::cmp::Ordering {
    left.line_range[0]
        .cmp(&right.line_range[0])
        .then_with(|| right.line_range[1].cmp(&left.line_range[1]))
        .then_with(|| left.stable_symbol_id.cmp(&right.stable_symbol_id))
}

fn resolution_from_symbols(symbols: Vec<GraphSymbolArtifact>) -> SelectorResolution {
    let mcp_tool_matches = symbols
        .iter()
        .filter(|symbol| symbol.symbol_kind == "mcp_tool")
        .collect::<Vec<_>>();
    if let [symbol] = mcp_tool_matches.as_slice() {
        return SelectorResolution::Resolved(resolved_symbol(symbol));
    }

    match symbols.as_slice() {
        [] => SelectorResolution::NotFound,
        [symbol] => SelectorResolution::Resolved(resolved_symbol(symbol)),
        _ => SelectorResolution::Ambiguous {
            candidates: candidate_rows(symbols),
        },
    }
}

fn resolved_symbol(symbol: &GraphSymbolArtifact) -> ResolvedSymbol {
    ResolvedSymbol {
        stable_symbol_id: symbol.stable_symbol_id.clone(),
    }
}

fn candidate_rows(symbols: Vec<GraphSymbolArtifact>) -> Vec<CandidateRow> {
    let mut candidates = symbols.iter().map(candidate_row).collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line_range[0].cmp(&right.line_range[0]))
            .then_with(|| left.line_range[1].cmp(&right.line_range[1]))
            .then_with(|| left.qualified_name.cmp(&right.qualified_name))
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates
}

fn candidate_row(symbol: &GraphSymbolArtifact) -> CandidateRow {
    let uri = format!("{CODE_SYMBOL_URI_PREFIX}{}", symbol.stable_symbol_id);
    let selector = if symbol.qualified_name.is_empty() {
        uri.clone()
    } else {
        format!("{}::{}", symbol.file_path, symbol.qualified_name)
    };

    CandidateRow {
        selector,
        uri,
        id: symbol.stable_symbol_id.clone(),
        entity_name: symbol.entity_name.clone(),
        qualified_name: symbol.qualified_name.clone(),
        file_path: symbol.file_path.clone(),
        line_range: symbol.line_range,
        symbol_kind: symbol.symbol_kind.clone(),
        enclosing_scope: symbol.enclosing_scope.clone(),
    }
}

fn enclosing_scope_entity_name(symbol: &GraphSymbolArtifact) -> Option<String> {
    symbol
        .enclosing_scope
        .as_ref()
        .map(|scope| format!("{scope}::{}", symbol.entity_name))
}

fn symbols_from_batches(batches: Vec<RecordBatch>) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
    let mut symbols = Vec::new();
    for batch in batches {
        symbols.extend(symbols_from_batch(&batch)?);
    }
    Ok(symbols)
}

fn search_symbols_from_batch(batch: &RecordBatch) -> anyhow::Result<Vec<SearchSymbol>> {
    let stable_symbol_id = string_array_by_name(batch, "stable_symbol_id")?;
    let file_path = string_array_by_name(batch, "file_path")?;
    let line_start = i32_array_by_name(batch, "line_start")?;
    let line_end = i32_array_by_name(batch, "line_end")?;
    let entity_name = string_array_by_name(batch, "entity_name")?;
    let qualified_name = string_array_by_name(batch, "qualified_name")?;
    let symbol_kind = string_array_by_name(batch, "symbol_kind")?;
    let enclosing_scope = string_array_by_name(batch, "enclosing_scope")?;

    let mut symbols = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        symbols.push(SearchSymbol {
            stable_symbol_id: required_string_value(stable_symbol_id, row, "stable_symbol_id")?
                .to_string(),
            entity_name: required_string_value(entity_name, row, "entity_name")?.to_string(),
            qualified_name: required_string_value(qualified_name, row, "qualified_name")?
                .to_string(),
            file_path: required_string_value(file_path, row, "file_path")?.to_string(),
            line_range: [
                i32_to_usize(line_start.value(row), "line_start")?,
                i32_to_usize(line_end.value(row), "line_end")?,
            ],
            symbol_kind: required_string_value(symbol_kind, row, "symbol_kind")?.to_string(),
            enclosing_scope: optional_string_value(enclosing_scope, row),
        });
    }
    Ok(symbols)
}

fn symbols_from_batch(batch: &RecordBatch) -> anyhow::Result<Vec<GraphSymbolArtifact>> {
    let stable_symbol_id = string_array_by_name(batch, "stable_symbol_id")?;
    let file_path = string_array_by_name(batch, "file_path")?;
    let byte_range_start = i64_array_by_name(batch, "byte_range_start")?;
    let byte_range_end = i64_array_by_name(batch, "byte_range_end")?;
    let line_start = i32_array_by_name(batch, "line_start")?;
    let line_end = i32_array_by_name(batch, "line_end")?;
    let entity_name = string_array_by_name(batch, "entity_name")?;
    let qualified_name = string_array_by_name(batch, "qualified_name")?;
    let symbol_kind = string_array_by_name(batch, "symbol_kind")?;
    let anchor_hash = string_array_by_name(batch, "anchor_hash")?;
    let enclosing_scope = string_array_by_name(batch, "enclosing_scope")?;

    let mut symbols = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        symbols.push(GraphSymbolArtifact {
            stable_symbol_id: required_string_value(stable_symbol_id, row, "stable_symbol_id")?
                .to_string(),
            file_path: required_string_value(file_path, row, "file_path")?.to_string(),
            byte_range: [
                i64_to_usize(byte_range_start.value(row), "byte_range_start")?,
                i64_to_usize(byte_range_end.value(row), "byte_range_end")?,
            ],
            line_range: [
                i32_to_usize(line_start.value(row), "line_start")?,
                i32_to_usize(line_end.value(row), "line_end")?,
            ],
            entity_name: required_string_value(entity_name, row, "entity_name")?.to_string(),
            qualified_name: required_string_value(qualified_name, row, "qualified_name")?
                .to_string(),
            symbol_kind: required_string_value(symbol_kind, row, "symbol_kind")?.to_string(),
            anchor_hash: required_string_value(anchor_hash, row, "anchor_hash")?.to_string(),
            enclosing_scope: optional_string_value(enclosing_scope, row),
        });
    }
    Ok(symbols)
}

fn file_manifests_from_batches(
    batches: Vec<RecordBatch>,
) -> anyhow::Result<Vec<GraphFileManifestEntry>> {
    let mut manifests = Vec::new();
    for batch in batches {
        manifests.extend(file_manifests_from_batch(&batch)?);
    }
    Ok(manifests)
}

fn file_manifests_from_batch(batch: &RecordBatch) -> anyhow::Result<Vec<GraphFileManifestEntry>> {
    let stable_file_id = string_array_by_name(batch, "stable_file_id")?;
    let path = string_array_by_name(batch, "path")?;
    let content_oid = string_array_by_name(batch, "content_oid")?;
    let node_ids = list_array_by_name(batch, "node_ids")?;

    let mut manifests = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        manifests.push(GraphFileManifestEntry {
            stable_file_id: required_string_value(stable_file_id, row, "stable_file_id")?
                .to_string(),
            path: required_string_value(path, row, "path")?.to_string(),
            content_oid: required_string_value(content_oid, row, "content_oid")?.to_string(),
            node_ids: required_node_id_list_value(node_ids, row, "node_ids")?,
        });
    }
    Ok(manifests)
}

fn resolved_edges_from_batch(batch: &RecordBatch) -> anyhow::Result<Vec<GraphEdgeArtifact>> {
    let source_stable_id = string_array_by_name(batch, "source_stable_id")?;
    let target_stable_id = string_array_by_name(batch, "target_stable_id")?;
    let target_label = string_array_by_name(batch, "target_label")?;
    let relation = string_array_by_name(batch, "relation")?;
    let confidence = string_array_by_name(batch, "confidence")?;
    let confidence_score = f32_array_by_name(batch, "confidence_score")?;
    let edge_kind = string_array_by_name(batch, "edge_kind")?;
    let bind_method = string_array_by_name(batch, "bind_method")?;

    let mut edges = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        edges.push(GraphEdgeArtifact {
            source_stable_symbol_id: required_string_value(
                source_stable_id,
                row,
                "source_stable_id",
            )?
            .to_string(),
            target_stable_symbol_id: Some(
                required_string_value(target_stable_id, row, "target_stable_id")?.to_string(),
            ),
            target_label: optional_string_value(target_label, row),
            relation: relation_from_str(required_string_value(relation, row, "relation")?)?,
            confidence: confidence_from_str(required_string_value(confidence, row, "confidence")?)?,
            confidence_score: confidence_score.value(row),
            change_kind: None,
            edge_kind: optional_string_value(edge_kind, row)
                .map(|value| edge_kind_from_str(&value))
                .transpose()?,
            bind_method: optional_string_value(bind_method, row),
        });
    }
    Ok(edges)
}

fn unresolved_edges_from_batch(batch: &RecordBatch) -> anyhow::Result<Vec<GraphEdgeArtifact>> {
    let source_stable_id = string_array_by_name(batch, "source_stable_id")?;
    let target_label = string_array_by_name(batch, "target_label")?;
    let relation = string_array_by_name(batch, "relation")?;
    let confidence = string_array_by_name(batch, "confidence")?;
    let confidence_score = f32_array_by_name(batch, "confidence_score")?;
    let edge_kind = string_array_by_name(batch, "edge_kind")?;
    let bind_method = string_array_by_name(batch, "bind_method")?;

    let mut edges = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        edges.push(GraphEdgeArtifact {
            source_stable_symbol_id: required_string_value(
                source_stable_id,
                row,
                "source_stable_id",
            )?
            .to_string(),
            target_stable_symbol_id: None,
            target_label: optional_string_value(target_label, row),
            relation: relation_from_str(required_string_value(relation, row, "relation")?)?,
            confidence: confidence_from_str(required_string_value(confidence, row, "confidence")?)?,
            confidence_score: confidence_score.value(row),
            change_kind: None,
            edge_kind: optional_string_value(edge_kind, row)
                .map(|value| edge_kind_from_str(&value))
                .transpose()?,
            bind_method: optional_string_value(bind_method, row),
        });
    }
    Ok(edges)
}

fn string_array_by_name<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a StringArray, ArrowError> {
    let index = batch.schema().index_of(name)?;
    batch
        .column(index)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| ArrowError::CastError(format!("expected string column `{name}`")))
}

fn i32_array_by_name<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a Int32Array, ArrowError> {
    let index = batch.schema().index_of(name)?;
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| ArrowError::CastError(format!("expected int32 column `{name}`")))
}

fn i64_array_by_name<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a Int64Array, ArrowError> {
    let index = batch.schema().index_of(name)?;
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| ArrowError::CastError(format!("expected int64 column `{name}`")))
}

fn f32_array_by_name<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a Float32Array, ArrowError> {
    let index = batch.schema().index_of(name)?;
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Float32Array>()
        .ok_or_else(|| ArrowError::CastError(format!("expected float32 column `{name}`")))
}

fn list_array_by_name<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a ListArray, ArrowError> {
    let index = batch.schema().index_of(name)?;
    batch
        .column(index)
        .as_any()
        .downcast_ref::<ListArray>()
        .ok_or_else(|| ArrowError::CastError(format!("expected list column `{name}`")))
}

fn required_string_value<'a>(
    values: &'a StringArray,
    index: usize,
    name: &str,
) -> Result<&'a str, ArrowError> {
    if values.is_null(index) {
        return Err(ArrowError::ComputeError(format!(
            "missing required string column `{name}`"
        )));
    }
    Ok(values.value(index))
}

fn optional_string_value(values: &StringArray, index: usize) -> Option<String> {
    (!values.is_null(index)).then(|| values.value(index).to_string())
}

fn required_node_id_list_value(
    values: &ListArray,
    index: usize,
    name: &str,
) -> anyhow::Result<Vec<NodeId>> {
    if values.is_null(index) {
        return Ok(Vec::new());
    }
    let item_values = values.value(index);
    let node_ids = item_values
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| anyhow!("expected int64 elements in list column `{name}`"))?;
    (0..node_ids.len())
        .map(|item_index| i64_to_node_id(node_ids.value(item_index), name))
        .collect()
}

fn i32_to_usize(value: i32, name: &str) -> anyhow::Result<usize> {
    usize::try_from(value).map_err(|_| anyhow!("column `{name}` has negative value {value}"))
}

fn i64_to_usize(value: i64, name: &str) -> anyhow::Result<usize> {
    usize::try_from(value).map_err(|_| anyhow!("column `{name}` has negative value {value}"))
}

fn i64_to_node_id(value: i64, name: &str) -> anyhow::Result<NodeId> {
    u64::try_from(value)
        .map(NodeId)
        .map_err(|_| anyhow!("column `{name}` has negative value {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{search_symbols, GraphIndexHeader, GraphSymbolArtifact, SearchFilters, SearchMode};

    fn artifact(symbols: Vec<GraphSymbolArtifact>) -> GraphIndexArtifact {
        GraphIndexArtifact {
            header: GraphIndexHeader {
                graph_index_version: "test".to_string(),
                content_hash_blake3: None,
            },
            manifest_version: "test".to_string(),
            graph_content_hash: "test".to_string(),
            file_manifests: Vec::new(),
            files: Vec::new(),
            file_node_ids: Vec::new(),
            symbols,
            symbol_node_ids: Vec::new(),
            edges: Vec::new(),
            tombstones: Vec::new(),
            diagnostics: Vec::new(),
            commits: Vec::new(),
            symbol_snapshots: Vec::new(),
            temporal_edges: Vec::new(),
        }
    }

    fn symbol(id: &str, entity_name: &str) -> GraphSymbolArtifact {
        GraphSymbolArtifact {
            stable_symbol_id: id.to_string(),
            file_path: "src/lib.rs".to_string(),
            byte_range: [0, 8],
            line_range: [1, 2],
            entity_name: entity_name.to_string(),
            qualified_name: format!("crate::{entity_name}"),
            symbol_kind: "function".to_string(),
            anchor_hash: format!("hash-{id}"),
            enclosing_scope: None,
        }
    }

    fn ids(result: &SearchResult) -> Vec<String> {
        result
            .candidates
            .iter()
            .map(|symbol| symbol.stable_symbol_id.clone())
            .collect()
    }

    #[test]
    fn in_memory_client_search_symbols_delegates_to_search_symbols() {
        let artifact = Arc::new(artifact(vec![
            symbol("s1", "target"),
            symbol("s2", "target_extra"),
            symbol("s3", "other"),
        ]));
        let options = SearchOptions {
            query: "target".to_string(),
            mode: SearchMode::Prefix,
            filters: SearchFilters::default(),
            limit: 20,
        };
        let expected = search_symbols(&artifact, &options);
        let client = InMemoryClient::new(Arc::clone(&artifact));

        let actual = client.search_symbols(&options).expect("search succeeds");

        assert_eq!(ids(&actual), ids(&expected));
        assert_eq!(actual.total_matches, expected.total_matches);
        assert_eq!(actual.truncated, expected.truncated);
    }
}

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context};
use arrow_array::{Array, BooleanArray, Int32Array, RecordBatch, StringArray};
use arrow_schema::ArrowError;
use globset::Glob;
use parquet::arrow::arrow_reader::{
    ArrowPredicateFn, ArrowReaderMetadata, ParquetRecordBatchReaderBuilder, RowFilter,
};
use parquet::arrow::ProjectionMask;

use crate::store::parquet::PARQUET_ROW_GROUP_SIZE;
use crate::temporal::TemporalIndex;
use crate::{
    compare_symbols, find_callee_edges, find_caller_edges, read_artifact_header_parquet,
    resolve_selector, search_symbols, CalleeRecord, CallerRecord, GraphArtifactManifest,
    GraphFileManifestEntry, GraphIndexArtifact, SearchOptions, SearchResult, SearchSymbol,
    SelectorResolution,
};
use crate::{SearchFilters, SearchMode};

pub type CodeSelectorResolution = SelectorResolution;

pub trait GraphQueryClient {
    fn search_symbols(&self, opts: &SearchOptions) -> anyhow::Result<SearchResult>;
    fn find_caller_edges(&self, sid: &str) -> Vec<CallerRecord<'_>>;
    fn find_callee_edges(&self, sid: &str) -> Vec<CalleeRecord<'_>>;
    fn resolve_selector(&self, selector: &str) -> CodeSelectorResolution;
    fn file_manifest_by_path(&self, path: &str) -> Option<&GraphFileManifestEntry>;
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

    fn find_caller_edges(&self, sid: &str) -> Vec<CallerRecord<'_>> {
        find_caller_edges(&self.artifact, sid)
    }

    fn find_callee_edges(&self, sid: &str) -> Vec<CalleeRecord<'_>> {
        find_callee_edges(&self.artifact, sid)
    }

    fn resolve_selector(&self, selector: &str) -> CodeSelectorResolution {
        resolve_selector(&self.artifact, selector)
    }

    fn file_manifest_by_path(&self, path: &str) -> Option<&GraphFileManifestEntry> {
        self.artifact
            .file_manifests
            .iter()
            .find(|entry| entry.path == path)
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

pub struct ParquetClient {
    dir: PathBuf,
    manifest: GraphArtifactManifest,
    nodes_metadata: ArrowReaderMetadata,
    search_projection: ProjectionMask,
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
}

impl GraphQueryClient for ParquetClient {
    fn search_symbols(&self, opts: &SearchOptions) -> anyhow::Result<SearchResult> {
        self.search_symbols_inner(opts)
    }

    fn find_caller_edges(&self, _sid: &str) -> Vec<CallerRecord<'_>> {
        unimplemented!("PR3")
    }

    fn find_callee_edges(&self, _sid: &str) -> Vec<CalleeRecord<'_>> {
        unimplemented!("PR3")
    }

    fn resolve_selector(&self, _selector: &str) -> CodeSelectorResolution {
        unimplemented!("PR4")
    }

    fn file_manifest_by_path(&self, _path: &str) -> Option<&GraphFileManifestEntry> {
        unimplemented!("PR4")
    }

    fn temporal_index(&self) -> Arc<TemporalIndex> {
        unimplemented!("PR5")
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

fn i32_to_usize(value: i32, name: &str) -> anyhow::Result<usize> {
    usize::try_from(value).map_err(|_| anyhow!("column `{name}` has negative value {value}"))
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

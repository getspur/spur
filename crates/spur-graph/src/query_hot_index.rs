use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use arrow_array::RecordBatch;
use globset::Glob;

use crate::query_client::symbol_from_batch_row;
use crate::search::{
    compare_symbols, limited_search_result, matches_filters, matches_query,
    INTERNAL_SEARCH_UNBOUNDED, PUBLIC_SEARCH_LIMIT,
};
use crate::{
    GraphEdgeArtifact, GraphSymbolArtifact, OwnedCalleeRecord, OwnedCallerRecord, RelationKind,
    SearchOptions, SearchResult, SearchSymbol,
};

/// Immutable, cache-resident projection for repeated current-graph queries.
///
/// Parquet remains the source of truth. This projection contains only current
/// symbols; source, edges, and temporal payloads stay on independent hydration
/// paths so edge corruption cannot poison symbol-only search.
pub(crate) struct HotQueryIndex {
    symbol_batch: RecordBatch,
    symbol_by_id: HashMap<String, usize>,
}

/// Immutable adjacency projection sharing symbols with [`HotQueryIndex`].
pub(crate) struct HotAdjacencyIndex {
    symbols: Arc<HotQueryIndex>,
    edges: Vec<GraphEdgeArtifact>,
    resolved_by_source: HashMap<String, Vec<usize>>,
    resolved_by_target: HashMap<String, Vec<usize>>,
    unresolved_by_source: HashMap<String, Vec<usize>>,
    unresolved_by_label: HashMap<String, Vec<usize>>,
}

impl HotQueryIndex {
    pub(crate) fn new(symbol_batch: RecordBatch) -> anyhow::Result<Self> {
        let mut symbol_by_id = HashMap::with_capacity(symbol_batch.num_rows());
        for row in 0..symbol_batch.num_rows() {
            let symbol = symbol_from_batch_row(&symbol_batch, row)?;
            symbol_by_id.insert(symbol.stable_symbol_id, row);
        }
        Ok(Self {
            symbol_batch,
            symbol_by_id,
        })
    }

    pub(crate) fn search_symbols(&self, options: &SearchOptions) -> SearchResult {
        let glob = options
            .filters
            .file_glob
            .as_deref()
            .and_then(|pattern| Glob::new(pattern).ok())
            .map(|glob| glob.compile_matcher());

        let limit = (options.limit != INTERNAL_SEARCH_UNBOUNDED)
            .then(|| options.limit.clamp(1, PUBLIC_SEARCH_LIMIT));
        let mut candidates = Vec::new();
        let mut total_matches = 0usize;

        for row in 0..self.symbol_batch.num_rows() {
            let symbol = SearchSymbol::from(&self.symbol_at(row));
            if !matches_query(&symbol, options)
                || !matches_filters(&symbol, &options.filters, glob.as_ref())
            {
                continue;
            }

            total_matches += 1;
            Self::push_candidate(&mut candidates, symbol, limit, options);
        }

        candidates.sort_by(|left, right| compare_symbols(left, right, options));
        limited_search_result(candidates, total_matches, options.limit)
    }

    pub(crate) fn symbol_by_id(&self, stable_symbol_id: &str) -> Option<GraphSymbolArtifact> {
        self.symbol_by_id
            .get(stable_symbol_id)
            .map(|index| self.symbol_at(*index))
    }

    fn push_candidate(
        candidates: &mut Vec<SearchSymbol>,
        symbol: SearchSymbol,
        limit: Option<usize>,
        options: &SearchOptions,
    ) {
        let Some(limit) = limit else {
            candidates.push(symbol);
            return;
        };
        if candidates.len() < limit {
            candidates.push(symbol);
            return;
        }

        let Some((worst_index, worst)) = candidates
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| compare_symbols(left, right, options))
        else {
            return;
        };
        if compare_symbols(&symbol, worst, options).is_lt() {
            candidates[worst_index] = symbol;
        }
    }

    fn symbol_at(&self, row: usize) -> GraphSymbolArtifact {
        symbol_from_batch_row(&self.symbol_batch, row).unwrap_or_else(|error| {
            panic!("validated HotQueryIndex symbol row {row} failed to materialize: {error:#}")
        })
    }
}

impl HotAdjacencyIndex {
    pub(crate) fn new(symbols: Arc<HotQueryIndex>, edges: Vec<GraphEdgeArtifact>) -> Self {
        let mut resolved_by_source = HashMap::<String, Vec<usize>>::new();
        let mut resolved_by_target = HashMap::<String, Vec<usize>>::new();
        let mut unresolved_by_source = HashMap::<String, Vec<usize>>::new();
        let mut unresolved_by_label = HashMap::<String, Vec<usize>>::new();
        for (index, edge) in edges.iter().enumerate() {
            if let Some(target) = edge.target_stable_symbol_id.as_ref() {
                resolved_by_source
                    .entry(edge.source_stable_symbol_id.clone())
                    .or_default()
                    .push(index);
                resolved_by_target
                    .entry(target.clone())
                    .or_default()
                    .push(index);
            } else {
                unresolved_by_source
                    .entry(edge.source_stable_symbol_id.clone())
                    .or_default()
                    .push(index);
                if let Some(label) = edge.target_label.as_ref() {
                    unresolved_by_label
                        .entry(label.clone())
                        .or_default()
                        .push(index);
                }
            }
        }
        Self {
            symbols,
            edges,
            resolved_by_source,
            resolved_by_target,
            unresolved_by_source,
            unresolved_by_label,
        }
    }

    pub(crate) fn caller_records(
        &self,
        target_symbol_id: &str,
        unresolved_labels: &HashSet<String>,
    ) -> Vec<OwnedCallerRecord> {
        let resolved = self.resolved_by_target.get(target_symbol_id);
        let resolved_count = resolved.map_or(0, Vec::len);
        let resolved = resolved.into_iter().flatten().copied();
        let unresolved = self.edge_indices_for_labels(unresolved_labels);
        let mut records = Vec::with_capacity(resolved_count + unresolved.len());
        for edge_index in resolved {
            let edge = &self.edges[edge_index];
            if !is_caller_relation(edge.relation) {
                continue;
            }
            if let Some(caller) = self.symbol_by_id(&edge.source_stable_symbol_id) {
                records.push(OwnedCallerRecord::Resolved {
                    caller,
                    edge: edge.clone(),
                });
            }
        }
        for edge_index in unresolved {
            let edge = &self.edges[edge_index];
            if !is_caller_relation(edge.relation) {
                continue;
            }
            if let Some(caller) = self.symbol_by_id(&edge.source_stable_symbol_id) {
                records.push(OwnedCallerRecord::Unresolved {
                    caller,
                    target_label: edge.target_label.clone().unwrap_or_default(),
                    edge: edge.clone(),
                });
            }
        }
        records
    }

    pub(crate) fn unresolved_caller_records(
        &self,
        target_labels: &HashSet<String>,
    ) -> Vec<OwnedCallerRecord> {
        self.edge_indices_for_labels(target_labels)
            .into_iter()
            .filter_map(|edge_index| {
                let edge = &self.edges[edge_index];
                if !is_caller_relation(edge.relation) {
                    return None;
                }
                let caller = self.symbol_by_id(&edge.source_stable_symbol_id)?;
                Some(OwnedCallerRecord::Unresolved {
                    caller,
                    target_label: edge.target_label.clone().unwrap_or_default(),
                    edge: edge.clone(),
                })
            })
            .collect()
    }

    pub(crate) fn callee_records(&self, source_symbol_id: &str) -> Vec<OwnedCalleeRecord> {
        let resolved = self
            .resolved_by_source
            .get(source_symbol_id)
            .into_iter()
            .flatten()
            .copied();
        let unresolved = self
            .unresolved_by_source
            .get(source_symbol_id)
            .into_iter()
            .flatten()
            .copied();
        let mut records = Vec::new();
        for edge_index in resolved {
            let edge = &self.edges[edge_index];
            if !is_caller_relation(edge.relation) {
                continue;
            }
            if let Some(symbol) = edge
                .target_stable_symbol_id
                .as_deref()
                .and_then(|target| self.symbol_by_id(target))
            {
                records.push(OwnedCalleeRecord::Resolved {
                    symbol,
                    edge: edge.clone(),
                });
            }
        }
        for edge_index in unresolved {
            let edge = &self.edges[edge_index];
            if !is_caller_relation(edge.relation) {
                continue;
            }
            if let Some(target_label) = edge.target_label.clone() {
                records.push(OwnedCalleeRecord::Unresolved {
                    edge: edge.clone(),
                    target_label,
                });
            }
        }
        records
    }

    fn edge_indices_for_labels(&self, labels: &HashSet<String>) -> Vec<usize> {
        let mut indices = labels
            .iter()
            .filter_map(|label| self.unresolved_by_label.get(label))
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        indices.sort_unstable();
        indices.dedup();
        indices
    }

    fn symbol_by_id(&self, stable_symbol_id: &str) -> Option<GraphSymbolArtifact> {
        self.symbols.symbol_by_id(stable_symbol_id)
    }
}

fn is_caller_relation(relation: RelationKind) -> bool {
    matches!(relation, RelationKind::Calls | RelationKind::References)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Int32Array, Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};

    use super::*;
    use crate::search::PUBLIC_SEARCH_LIMIT;
    use crate::{SearchFilters, SearchMode};

    struct TestSymbol {
        stable_symbol_id: String,
        file_path: String,
        byte_range: [i64; 2],
        line_range: [i32; 2],
        entity_name: String,
        qualified_name: String,
        symbol_kind: String,
        anchor_hash: String,
        enclosing_scope: Option<String>,
    }

    impl TestSymbol {
        fn new(index: usize, entity_name: impl Into<String>) -> Self {
            let entity_name = entity_name.into();
            Self {
                stable_symbol_id: format!("sid-{index:03}"),
                file_path: format!("src/file_{index:03}.rs"),
                byte_range: [index as i64, index as i64 + 10],
                line_range: [index as i32 + 1, index as i32 + 2],
                qualified_name: format!("module::{entity_name}"),
                entity_name,
                symbol_kind: "function".to_owned(),
                anchor_hash: format!("anchor-{index:03}"),
                enclosing_scope: None,
            }
        }

        fn with_scope(mut self, enclosing_scope: impl Into<String>) -> Self {
            self.enclosing_scope = Some(enclosing_scope.into());
            self
        }
    }

    fn symbol_batch(rows: &[TestSymbol]) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("stable_symbol_id", DataType::Utf8, false),
            Field::new("file_path", DataType::Utf8, false),
            Field::new("byte_range_start", DataType::Int64, false),
            Field::new("byte_range_end", DataType::Int64, false),
            Field::new("line_start", DataType::Int32, false),
            Field::new("line_end", DataType::Int32, false),
            Field::new("entity_name", DataType::Utf8, false),
            Field::new("qualified_name", DataType::Utf8, false),
            Field::new("symbol_kind", DataType::Utf8, false),
            Field::new("anchor_hash", DataType::Utf8, false),
            Field::new("enclosing_scope", DataType::Utf8, true),
        ]));
        let stable_symbol_ids = rows
            .iter()
            .map(|row| row.stable_symbol_id.as_str())
            .collect::<Vec<_>>();
        let file_paths = rows
            .iter()
            .map(|row| row.file_path.as_str())
            .collect::<Vec<_>>();
        let byte_range_starts = rows.iter().map(|row| row.byte_range[0]).collect::<Vec<_>>();
        let byte_range_ends = rows.iter().map(|row| row.byte_range[1]).collect::<Vec<_>>();
        let line_starts = rows.iter().map(|row| row.line_range[0]).collect::<Vec<_>>();
        let line_ends = rows.iter().map(|row| row.line_range[1]).collect::<Vec<_>>();
        let entity_names = rows
            .iter()
            .map(|row| row.entity_name.as_str())
            .collect::<Vec<_>>();
        let qualified_names = rows
            .iter()
            .map(|row| row.qualified_name.as_str())
            .collect::<Vec<_>>();
        let symbol_kinds = rows
            .iter()
            .map(|row| row.symbol_kind.as_str())
            .collect::<Vec<_>>();
        let anchor_hashes = rows
            .iter()
            .map(|row| row.anchor_hash.as_str())
            .collect::<Vec<_>>();
        let enclosing_scopes = rows
            .iter()
            .map(|row| row.enclosing_scope.as_deref())
            .collect::<Vec<_>>();

        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(stable_symbol_ids)) as ArrayRef,
                Arc::new(StringArray::from(file_paths)),
                Arc::new(Int64Array::from(byte_range_starts)),
                Arc::new(Int64Array::from(byte_range_ends)),
                Arc::new(Int32Array::from(line_starts)),
                Arc::new(Int32Array::from(line_ends)),
                Arc::new(StringArray::from(entity_names)),
                Arc::new(StringArray::from(qualified_names)),
                Arc::new(StringArray::from(symbol_kinds)),
                Arc::new(StringArray::from(anchor_hashes)),
                Arc::new(StringArray::from(enclosing_scopes)),
            ],
        )
        .expect("valid symbol batch")
    }

    fn hot_query_index_struct_source() -> &'static str {
        let source = include_str!("query_hot_index.rs");
        let start = source
            .find("pub(crate) struct HotQueryIndex")
            .expect("HotQueryIndex struct source");
        let end = source[start..]
            .find("/// Immutable adjacency projection")
            .expect("HotAdjacencyIndex marker");
        &source[start..start + end]
    }

    fn parquet_hot_query_index_source() -> &'static str {
        let source = include_str!("query_client.rs");
        let start = source
            .find("fn hot_query_index(&self)")
            .expect("ParquetClient::hot_query_index source");
        let end = source[start..]
            .find("#[cfg(test)]\n    fn hot_query_index_build_count")
            .expect("hot query index build count marker");
        &source[start..start + end]
    }

    #[test]
    fn hot_query_index_stores_one_record_batch_without_aos_vectors() {
        let source = hot_query_index_struct_source();

        assert!(
            source.contains("RecordBatch"),
            "HotQueryIndex storage must be the current-graph RecordBatch"
        );
        assert!(
            !source.contains("Vec<SearchSymbol>"),
            "HotQueryIndex must not retain SearchSymbol vectors"
        );
        assert!(
            !source.contains("Vec<GraphSymbolArtifact>"),
            "HotQueryIndex must not retain GraphSymbolArtifact vectors"
        );
    }

    #[test]
    fn parquet_client_constructs_hot_query_index_from_record_batch() {
        let source = parquet_hot_query_index_source();

        assert!(
            source.contains("let symbol_batch ="),
            "ParquetClient must read a current-symbol RecordBatch for HotQueryIndex"
        );
        assert!(
            source.contains("HotQueryIndex::new(symbol_batch)"),
            "HotQueryIndex construction must take the RecordBatch directly"
        );
        assert!(
            !source.contains("read_current_query_symbols_parquet"),
            "HotQueryIndex construction must not copy current symbols into Vec<GraphSymbolArtifact>"
        );
    }

    #[test]
    fn hot_query_index_materializes_symbol_lookup_from_record_batch_rows() {
        let rows = vec![
            TestSymbol::new(0, "alpha"),
            TestSymbol::new(1, "beta").with_scope("impl Owner"),
        ];
        let index = HotQueryIndex::new(symbol_batch(&rows)).expect("batch-backed hot index");

        let symbol = index.symbol_by_id("sid-001").expect("indexed symbol");

        assert_eq!(symbol.stable_symbol_id, "sid-001");
        assert_eq!(symbol.entity_name, "beta");
        assert_eq!(symbol.qualified_name, "module::beta");
        assert_eq!(symbol.file_path, "src/file_001.rs");
        assert_eq!(symbol.byte_range, [1, 11]);
        assert_eq!(symbol.line_range, [2, 3]);
        assert_eq!(symbol.symbol_kind, "function");
        assert_eq!(symbol.anchor_hash, "anchor-001");
        assert_eq!(symbol.enclosing_scope.as_deref(), Some("impl Owner"));
    }

    #[test]
    fn hot_query_index_search_materializes_only_public_result_window() {
        let rows = (0..PUBLIC_SEARCH_LIMIT + 5)
            .map(|index| TestSymbol::new(index, format!("target_{index:03}")))
            .collect::<Vec<_>>();
        let index = HotQueryIndex::new(symbol_batch(&rows)).expect("batch-backed hot index");

        let result = index.search_symbols(&SearchOptions {
            query: "target_".to_owned(),
            mode: SearchMode::Prefix,
            filters: SearchFilters::default(),
            limit: PUBLIC_SEARCH_LIMIT + 50,
        });

        assert_eq!(result.total_matches, PUBLIC_SEARCH_LIMIT + 5);
        assert_eq!(result.candidates.len(), PUBLIC_SEARCH_LIMIT);
        assert!(result.truncated);
        assert!(result
            .candidates
            .iter()
            .all(|symbol| symbol.entity_name.starts_with("target_")));
    }
}

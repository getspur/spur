use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::Arc;

use arrow_array::{Array as _, Int32Array, Int64Array, RecordBatch, StringArray};
use globset::{Glob, GlobMatcher};

#[cfg(test)]
use crate::query_client::symbol_from_batch_row;
use crate::query_client::SymbolBatchColumns;
use crate::search::{
    compare_symbols, limited_search_result, INTERNAL_SEARCH_UNBOUNDED, PUBLIC_SEARCH_LIMIT,
};
use crate::{
    GraphEdgeArtifact, GraphSymbolArtifact, OwnedCalleeRecord, OwnedCallerRecord, RelationKind,
    SearchFilters, SearchMode, SearchOptions, SearchResult, SearchSymbol,
};

/// Immutable, cache-resident projection for repeated current-graph queries.
///
/// Parquet remains the source of truth. This projection contains only current
/// symbols; source, edges, and temporal payloads stay on independent hydration
/// paths so edge corruption cannot poison symbol-only search.
pub(crate) struct HotQueryIndex {
    symbol_batch: RecordBatch,
    symbol_rows_by_hash: HashMap<u64, SymbolIdRows>,
    #[cfg(test)]
    search_symbol_materializations: AtomicUsize,
    #[cfg(test)]
    symbol_row_lookups: AtomicUsize,
    #[cfg(test)]
    symbol_column_projections: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct SymbolRowCode(usize);

#[derive(Clone, Copy)]
struct EdgeSymbolRows {
    source: SymbolRowCode,
    target: SymbolRowCode,
}

impl EdgeSymbolRows {
    const MISSING: SymbolRowCode = SymbolRowCode(usize::MAX);

    const fn missing() -> Self {
        Self {
            source: Self::MISSING,
            target: Self::MISSING,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct UnresolvedLabelCode(usize);

/// Immutable adjacency projection sharing symbols with [`HotQueryIndex`].
pub(crate) struct HotAdjacencyIndex {
    symbols: Arc<HotQueryIndex>,
    edges: Vec<GraphEdgeArtifact>,
    edge_symbol_rows: Vec<EdgeSymbolRows>,
    unresolved_label_codes: UnresolvedLabelCodes,
    resolved_by_source: HashMap<SymbolRowCode, Vec<usize>>,
    resolved_by_target: HashMap<SymbolRowCode, Vec<usize>>,
    unresolved_by_source: HashMap<SymbolRowCode, Vec<usize>>,
    unresolved_by_label: HashMap<UnresolvedLabelCode, Vec<usize>>,
}

impl HotQueryIndex {
    pub(crate) fn new(symbol_batch: RecordBatch) -> anyhow::Result<Self> {
        let mut symbol_rows_by_hash =
            HashMap::<u64, SymbolIdRows>::with_capacity(symbol_batch.num_rows());
        let columns = HotSymbolColumns::new(&symbol_batch)?;
        for row in 0..symbol_batch.num_rows() {
            let stable_symbol_id = columns.validate_row(row)?;
            let hash = symbol_id_hash(symbol_rows_by_hash.hasher(), stable_symbol_id);
            symbol_rows_by_hash
                .entry(hash)
                .and_modify(|rows| rows.push(row))
                .or_insert(SymbolIdRows::One(row));
        }
        Ok(Self {
            symbol_batch,
            symbol_rows_by_hash,
            #[cfg(test)]
            search_symbol_materializations: AtomicUsize::new(0),
            #[cfg(test)]
            symbol_row_lookups: AtomicUsize::new(0),
            #[cfg(test)]
            symbol_column_projections: AtomicUsize::new(0),
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
        let search_columns = SearchSymbolColumns::new(&self.symbol_batch);
        let mut matching_rows = Vec::new();

        for row in 0..self.symbol_batch.num_rows() {
            let symbol = search_columns.symbol_at(row);
            if !matches_query_row(&symbol, options)
                || !matches_filters_row(&symbol, &options.filters, glob.as_ref())
            {
                continue;
            }

            matching_rows.push(row);
        }

        let total_matches = matching_rows.len();
        if let Some(limit) = limit {
            if matching_rows.len() > limit {
                matching_rows.select_nth_unstable_by(limit, |left, right| {
                    compare_symbol_rows(
                        &search_columns.symbol_at(*left),
                        &search_columns.symbol_at(*right),
                        options,
                    )
                });
                matching_rows.truncate(limit);
            }
        }

        let symbol_columns = self.symbol_columns();
        let mut candidates = matching_rows
            .into_iter()
            .map(|row| SearchSymbol::from(&self.search_symbol_at(&symbol_columns, row)))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| compare_symbols(left, right, options));
        limited_search_result(candidates, total_matches, options.limit)
    }

    pub(crate) fn symbol_by_id(&self, stable_symbol_id: &str) -> Option<GraphSymbolArtifact> {
        let row = self.symbol_row_by_id(stable_symbol_id)?;
        let columns = self.symbol_columns();
        self.symbol_by_row_code(&columns, row)
    }

    fn symbol_row_by_id(&self, stable_symbol_id: &str) -> Option<SymbolRowCode> {
        #[cfg(test)]
        self.symbol_row_lookups
            .fetch_add(1, AtomicOrdering::Relaxed);

        let stable_symbol_ids = string_array_by_name(&self.symbol_batch, "stable_symbol_id");
        self.symbol_row_by_id_from(stable_symbol_ids, stable_symbol_id)
    }

    fn symbol_row_by_id_from(
        &self,
        stable_symbol_ids: &StringArray,
        stable_symbol_id: &str,
    ) -> Option<SymbolRowCode> {
        let hash = symbol_id_hash(self.symbol_rows_by_hash.hasher(), stable_symbol_id);
        self.symbol_rows_by_hash
            .get(&hash)?
            .matching_row(|row| stable_symbol_ids.value(row) == stable_symbol_id)
            .map(SymbolRowCode)
    }

    fn symbol_by_row_code(
        &self,
        columns: &SymbolBatchColumns<'_>,
        row: SymbolRowCode,
    ) -> Option<GraphSymbolArtifact> {
        (row.0 < self.symbol_batch.num_rows()).then(|| self.symbol_at(columns, row.0))
    }

    fn search_symbol_at(
        &self,
        columns: &SymbolBatchColumns<'_>,
        row: usize,
    ) -> GraphSymbolArtifact {
        #[cfg(test)]
        self.search_symbol_materializations
            .fetch_add(1, AtomicOrdering::Relaxed);

        self.symbol_at(columns, row)
    }

    fn symbol_columns(&self) -> SymbolBatchColumns<'_> {
        #[cfg(test)]
        self.symbol_column_projections
            .fetch_add(1, AtomicOrdering::Relaxed);

        SymbolBatchColumns::new(&self.symbol_batch).unwrap_or_else(|error| {
            panic!("validated HotQueryIndex columns failed to project: {error:#}")
        })
    }

    fn symbol_at(&self, columns: &SymbolBatchColumns<'_>, row: usize) -> GraphSymbolArtifact {
        columns.symbol_at(row).unwrap_or_else(|error| {
            panic!("validated HotQueryIndex symbol row {row} failed to materialize: {error:#}")
        })
    }

    #[cfg(test)]
    fn reset_search_symbol_materializations(&self) {
        self.search_symbol_materializations
            .store(0, AtomicOrdering::Relaxed);
    }

    #[cfg(test)]
    fn search_symbol_materializations(&self) -> usize {
        self.search_symbol_materializations
            .load(AtomicOrdering::Relaxed)
    }

    #[cfg(test)]
    fn reset_symbol_row_lookups(&self) {
        self.symbol_row_lookups.store(0, AtomicOrdering::Relaxed);
    }

    #[cfg(test)]
    fn symbol_row_lookups(&self) -> usize {
        self.symbol_row_lookups.load(AtomicOrdering::Relaxed)
    }

    #[cfg(test)]
    fn reset_symbol_column_projections(&self) {
        self.symbol_column_projections
            .store(0, AtomicOrdering::Relaxed);
    }

    #[cfg(test)]
    fn symbol_column_projections(&self) -> usize {
        self.symbol_column_projections.load(AtomicOrdering::Relaxed)
    }
}

enum SymbolIdRows {
    One(usize),
    Many(Vec<usize>),
}

impl SymbolIdRows {
    fn push(&mut self, row: usize) {
        match self {
            Self::One(existing) => {
                *self = Self::Many(vec![*existing, row]);
            }
            Self::Many(rows) => rows.push(row),
        }
    }

    fn matching_row(&self, mut predicate: impl FnMut(usize) -> bool) -> Option<usize> {
        match self {
            Self::One(row) => predicate(*row).then_some(*row),
            Self::Many(rows) => rows.iter().rev().copied().find(|row| predicate(*row)),
        }
    }
}

#[derive(Default)]
struct UnresolvedLabelCodes {
    labels: Vec<String>,
    codes_by_hash: HashMap<u64, SymbolIdRows>,
}

impl UnresolvedLabelCodes {
    fn code_for(&mut self, label: &str) -> UnresolvedLabelCode {
        if let Some(code) = self.find(label) {
            return code;
        }

        let code = UnresolvedLabelCode(self.labels.len());
        let hash = string_hash(self.codes_by_hash.hasher(), label);
        self.labels.push(label.to_owned());
        self.codes_by_hash
            .entry(hash)
            .and_modify(|rows| rows.push(code.0))
            .or_insert(SymbolIdRows::One(code.0));
        code
    }

    fn find(&self, label: &str) -> Option<UnresolvedLabelCode> {
        let hash = string_hash(self.codes_by_hash.hasher(), label);
        self.codes_by_hash
            .get(&hash)?
            .matching_row(|code| self.labels.get(code).is_some_and(|value| value == label))
            .map(UnresolvedLabelCode)
    }
}

fn symbol_id_hash(hash_builder: &impl BuildHasher, stable_symbol_id: &str) -> u64 {
    string_hash(hash_builder, stable_symbol_id)
}

fn string_hash(hash_builder: &impl BuildHasher, value: &str) -> u64 {
    hash_builder.hash_one(value)
}

struct HotSymbolColumns<'a> {
    stable_symbol_id: &'a StringArray,
    file_path: &'a StringArray,
    byte_range_start: &'a Int64Array,
    byte_range_end: &'a Int64Array,
    line_start: &'a Int32Array,
    line_end: &'a Int32Array,
    entity_name: &'a StringArray,
    qualified_name: &'a StringArray,
    symbol_kind: &'a StringArray,
    anchor_hash: &'a StringArray,
    enclosing_scope: &'a StringArray,
}

impl<'a> HotSymbolColumns<'a> {
    fn new(batch: &'a RecordBatch) -> anyhow::Result<Self> {
        Ok(Self {
            stable_symbol_id: try_string_array_by_name(batch, "stable_symbol_id")?,
            file_path: try_string_array_by_name(batch, "file_path")?,
            byte_range_start: try_i64_array_by_name(batch, "byte_range_start")?,
            byte_range_end: try_i64_array_by_name(batch, "byte_range_end")?,
            line_start: try_i32_array_by_name(batch, "line_start")?,
            line_end: try_i32_array_by_name(batch, "line_end")?,
            entity_name: try_string_array_by_name(batch, "entity_name")?,
            qualified_name: try_string_array_by_name(batch, "qualified_name")?,
            symbol_kind: try_string_array_by_name(batch, "symbol_kind")?,
            anchor_hash: try_string_array_by_name(batch, "anchor_hash")?,
            enclosing_scope: try_string_array_by_name(batch, "enclosing_scope")?,
        })
    }

    fn validate_row(&self, row: usize) -> anyhow::Result<&'a str> {
        let stable_symbol_id =
            try_required_string_value(self.stable_symbol_id, row, "stable_symbol_id")?;
        try_required_string_value(self.file_path, row, "file_path")?;
        try_i64_to_usize(self.byte_range_start.value(row), "byte_range_start")?;
        try_i64_to_usize(self.byte_range_end.value(row), "byte_range_end")?;
        try_i32_to_usize(self.line_start.value(row), "line_start")?;
        try_i32_to_usize(self.line_end.value(row), "line_end")?;
        try_required_string_value(self.entity_name, row, "entity_name")?;
        try_required_string_value(self.qualified_name, row, "qualified_name")?;
        try_required_string_value(self.symbol_kind, row, "symbol_kind")?;
        try_required_string_value(self.anchor_hash, row, "anchor_hash")?;
        if !self.enclosing_scope.is_null(row) {
            self.enclosing_scope.value(row);
        }
        Ok(stable_symbol_id)
    }
}

struct SearchSymbolColumns<'a> {
    stable_symbol_id: &'a StringArray,
    file_path: &'a StringArray,
    line_start: &'a Int32Array,
    line_end: &'a Int32Array,
    entity_name: &'a StringArray,
    qualified_name: &'a StringArray,
    symbol_kind: &'a StringArray,
}

impl<'a> SearchSymbolColumns<'a> {
    fn new(batch: &'a RecordBatch) -> Self {
        Self {
            stable_symbol_id: string_array_by_name(batch, "stable_symbol_id"),
            file_path: string_array_by_name(batch, "file_path"),
            line_start: i32_array_by_name(batch, "line_start"),
            line_end: i32_array_by_name(batch, "line_end"),
            entity_name: string_array_by_name(batch, "entity_name"),
            qualified_name: string_array_by_name(batch, "qualified_name"),
            symbol_kind: string_array_by_name(batch, "symbol_kind"),
        }
    }

    fn symbol_at(&self, row: usize) -> SearchSymbolRow<'a> {
        SearchSymbolRow {
            stable_symbol_id: self.stable_symbol_id.value(row),
            entity_name: self.entity_name.value(row),
            qualified_name: self.qualified_name.value(row),
            file_path: self.file_path.value(row),
            line_range: [
                nonnegative_usize(self.line_start.value(row), "line_start", row),
                nonnegative_usize(self.line_end.value(row), "line_end", row),
            ],
            symbol_kind: self.symbol_kind.value(row),
        }
    }
}

#[derive(Clone, Copy)]
struct SearchSymbolRow<'a> {
    stable_symbol_id: &'a str,
    entity_name: &'a str,
    qualified_name: &'a str,
    file_path: &'a str,
    line_range: [usize; 2],
    symbol_kind: &'a str,
}

fn matches_query_row(symbol: &SearchSymbolRow<'_>, options: &SearchOptions) -> bool {
    let query = options.query.as_str();
    match options.mode {
        SearchMode::Exact => symbol.entity_name == query || symbol.qualified_name == query,
        SearchMode::Prefix => symbol.entity_name.starts_with(query),
        SearchMode::Substring => symbol.entity_name.contains(query),
    }
}

fn matches_filters_row(
    symbol: &SearchSymbolRow<'_>,
    filters: &SearchFilters,
    glob: Option<&GlobMatcher>,
) -> bool {
    if filters
        .symbol_kind
        .as_deref()
        .is_some_and(|symbol_kind| symbol.symbol_kind != symbol_kind)
    {
        return false;
    }

    if filters
        .file
        .as_deref()
        .is_some_and(|file| symbol.file_path != file)
    {
        return false;
    }

    if filters.file_glob.is_some() && !glob.is_some_and(|glob| glob.is_match(symbol.file_path)) {
        return false;
    }

    true
}

fn compare_symbol_rows(
    left: &SearchSymbolRow<'_>,
    right: &SearchSymbolRow<'_>,
    options: &SearchOptions,
) -> Ordering {
    match options.mode {
        SearchMode::Exact => compare_exact_rows(left, right, options.query.as_str()),
        SearchMode::Prefix => compare_prefix_rows(left, right),
        SearchMode::Substring => compare_substring_rows(left, right, options.query.as_str()),
    }
}

fn compare_exact_rows(
    left: &SearchSymbolRow<'_>,
    right: &SearchSymbolRow<'_>,
    query: &str,
) -> Ordering {
    let left_rank = exact_row_rank(left, query);
    let right_rank = exact_row_rank(right, query);
    left_rank
        .cmp(&right_rank)
        .then_with(|| compare_row_location(left, right))
}

fn exact_row_rank(symbol: &SearchSymbolRow<'_>, query: &str) -> u8 {
    u8::from(symbol.entity_name != query)
}

fn compare_prefix_rows(left: &SearchSymbolRow<'_>, right: &SearchSymbolRow<'_>) -> Ordering {
    left.entity_name
        .len()
        .cmp(&right.entity_name.len())
        .then_with(|| compare_row_location(left, right))
}

fn compare_substring_rows(
    left: &SearchSymbolRow<'_>,
    right: &SearchSymbolRow<'_>,
    query: &str,
) -> Ordering {
    let left_position = left
        .entity_name
        .find(query)
        .expect("substring comparator only receives matches");
    let right_position = right
        .entity_name
        .find(query)
        .expect("substring comparator only receives matches");

    left_position
        .cmp(&right_position)
        .then_with(|| left.entity_name.len().cmp(&right.entity_name.len()))
        .then_with(|| compare_row_location(left, right))
}

fn compare_row_location(left: &SearchSymbolRow<'_>, right: &SearchSymbolRow<'_>) -> Ordering {
    left.file_path
        .cmp(right.file_path)
        .then_with(|| left.line_range[0].cmp(&right.line_range[0]))
        .then_with(|| left.line_range[1].cmp(&right.line_range[1]))
        .then_with(|| left.stable_symbol_id.cmp(right.stable_symbol_id))
}

fn string_array_by_name<'a>(batch: &'a RecordBatch, name: &str) -> &'a StringArray {
    let index = batch.schema().index_of(name).unwrap_or_else(|error| {
        panic!("validated HotQueryIndex batch missing string column `{name}`: {error}")
    });
    batch
        .column(index)
        .as_any()
        .downcast_ref::<StringArray>()
        .unwrap_or_else(|| panic!("validated HotQueryIndex column `{name}` is not Utf8"))
}

fn try_string_array_by_name<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> anyhow::Result<&'a StringArray> {
    let index = batch.schema().index_of(name)?;
    batch
        .column(index)
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| anyhow::anyhow!("validated HotQueryIndex column `{name}` is not Utf8"))
}

fn try_i32_array_by_name<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a Int32Array> {
    let index = batch.schema().index_of(name)?;
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Int32Array>()
        .ok_or_else(|| anyhow::anyhow!("validated HotQueryIndex column `{name}` is not Int32"))
}

fn try_i64_array_by_name<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a Int64Array> {
    let index = batch.schema().index_of(name)?;
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Int64Array>()
        .ok_or_else(|| anyhow::anyhow!("validated HotQueryIndex column `{name}` is not Int64"))
}

fn try_required_string_value<'a>(
    array: &'a StringArray,
    row: usize,
    name: &str,
) -> anyhow::Result<&'a str> {
    if array.is_null(row) {
        anyhow::bail!("missing required string column `{name}`");
    }
    Ok(array.value(row))
}

fn try_i32_to_usize(value: i32, name: &str) -> anyhow::Result<usize> {
    usize::try_from(value)
        .map_err(|_| anyhow::anyhow!("column `{name}` has negative value {value}"))
}

fn try_i64_to_usize(value: i64, name: &str) -> anyhow::Result<usize> {
    usize::try_from(value)
        .map_err(|_| anyhow::anyhow!("column `{name}` has negative value {value}"))
}

fn i32_array_by_name<'a>(batch: &'a RecordBatch, name: &str) -> &'a Int32Array {
    let index = batch.schema().index_of(name).unwrap_or_else(|error| {
        panic!("validated HotQueryIndex batch missing int32 column `{name}`: {error}")
    });
    batch
        .column(index)
        .as_any()
        .downcast_ref::<Int32Array>()
        .unwrap_or_else(|| panic!("validated HotQueryIndex column `{name}` is not Int32"))
}

fn nonnegative_usize(value: i32, column: &str, row: usize) -> usize {
    match usize::try_from(value) {
        Ok(value) => value,
        Err(_) => {
            panic!("validated HotQueryIndex row {row} has negative `{column}` value {value}")
        }
    }
}

impl HotAdjacencyIndex {
    pub(crate) fn new(symbols: Arc<HotQueryIndex>, edges: Vec<GraphEdgeArtifact>) -> Self {
        let mut unresolved_label_codes = UnresolvedLabelCodes::default();
        let mut resolved_by_source = HashMap::<SymbolRowCode, Vec<usize>>::new();
        let mut resolved_by_target = HashMap::<SymbolRowCode, Vec<usize>>::new();
        let mut unresolved_by_source = HashMap::<SymbolRowCode, Vec<usize>>::new();
        let mut unresolved_by_label = HashMap::<UnresolvedLabelCode, Vec<usize>>::new();
        let mut edge_symbol_rows = vec![EdgeSymbolRows::missing(); edges.len()];
        let stable_symbol_ids = string_array_by_name(&symbols.symbol_batch, "stable_symbol_id");
        for (index, edge) in edges.iter().enumerate() {
            let Some(source) =
                symbols.symbol_row_by_id_from(stable_symbol_ids, &edge.source_stable_symbol_id)
            else {
                continue;
            };
            edge_symbol_rows[index].source = source;

            if let Some(target_id) = edge.target_stable_symbol_id.as_deref() {
                let Some(target) = symbols.symbol_row_by_id_from(stable_symbol_ids, target_id)
                else {
                    continue;
                };
                edge_symbol_rows[index].target = target;
                resolved_by_source.entry(source).or_default().push(index);
                resolved_by_target.entry(target).or_default().push(index);
            } else {
                unresolved_by_source.entry(source).or_default().push(index);
                if let Some(label) = edge.target_label.as_deref() {
                    let label = unresolved_label_codes.code_for(label);
                    unresolved_by_label.entry(label).or_default().push(index);
                }
            }
        }
        Self {
            symbols,
            edges,
            edge_symbol_rows,
            unresolved_label_codes,
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
        let resolved = self
            .symbols
            .symbol_row_by_id(target_symbol_id)
            .and_then(|target| self.resolved_by_target.get(&target));
        let resolved_count = resolved.map_or(0, Vec::len);
        let resolved = resolved.into_iter().flatten().copied();
        let unresolved = self.edge_indices_for_labels(unresolved_labels);
        let mut records = Vec::with_capacity(resolved_count + unresolved.len());
        let symbol_columns = self.symbols.symbol_columns();
        for edge_index in resolved {
            let edge = &self.edges[edge_index];
            if !is_caller_relation(edge.relation) {
                continue;
            }
            if let Some(caller) = self
                .symbols
                .symbol_by_row_code(&symbol_columns, self.edge_symbol_rows[edge_index].source)
            {
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
            if let Some(caller) = self
                .symbols
                .symbol_by_row_code(&symbol_columns, self.edge_symbol_rows[edge_index].source)
            {
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
        let symbol_columns = self.symbols.symbol_columns();
        self.edge_indices_for_labels(target_labels)
            .into_iter()
            .filter_map(|edge_index| {
                let edge = &self.edges[edge_index];
                if !is_caller_relation(edge.relation) {
                    return None;
                }
                let caller = self.symbols.symbol_by_row_code(
                    &symbol_columns,
                    self.edge_symbol_rows[edge_index].source,
                )?;
                Some(OwnedCallerRecord::Unresolved {
                    caller,
                    target_label: edge.target_label.clone().unwrap_or_default(),
                    edge: edge.clone(),
                })
            })
            .collect()
    }

    pub(crate) fn callee_records(&self, source_symbol_id: &str) -> Vec<OwnedCalleeRecord> {
        let Some(source) = self.symbols.symbol_row_by_id(source_symbol_id) else {
            return Vec::new();
        };
        let resolved = self.resolved_by_source.get(&source);
        let resolved_count = resolved.map_or(0, Vec::len);
        let resolved = resolved.into_iter().flatten().copied();
        let unresolved = self.unresolved_by_source.get(&source);
        let unresolved_count = unresolved.map_or(0, Vec::len);
        let unresolved = unresolved.into_iter().flatten().copied();
        let mut records = Vec::with_capacity(resolved_count + unresolved_count);
        let symbol_columns = self.symbols.symbol_columns();
        for edge_index in resolved {
            let edge = &self.edges[edge_index];
            if !is_caller_relation(edge.relation) {
                continue;
            }
            if let Some(symbol) = self
                .symbols
                .symbol_by_row_code(&symbol_columns, self.edge_symbol_rows[edge_index].target)
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
            .filter_map(|label| self.unresolved_label_codes.find(label))
            .filter_map(|label| self.unresolved_by_label.get(&label))
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        indices.sort_unstable();
        indices.dedup();
        indices
    }
}

fn is_caller_relation(relation: RelationKind) -> bool {
    matches!(relation, RelationKind::Calls | RelationKind::References)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Int32Array, Int64Array, RecordBatch, StringArray};
    use arrow_schema::{DataType, Field, Schema};

    use super::*;
    use crate::search::PUBLIC_SEARCH_LIMIT;
    use crate::{Confidence, SearchFilters, SearchMode};

    fn search_options(query: &str, mode: SearchMode) -> SearchOptions {
        SearchOptions {
            query: query.to_owned(),
            mode,
            filters: SearchFilters::default(),
            limit: PUBLIC_SEARCH_LIMIT,
        }
    }

    fn result_ids(result: &SearchResult) -> Vec<String> {
        result
            .candidates
            .iter()
            .map(|symbol| symbol.stable_symbol_id.clone())
            .collect()
    }

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

        fn with_file_path(mut self, file_path: impl Into<String>) -> Self {
            self.file_path = file_path.into();
            self
        }

        fn with_qualified_name(mut self, qualified_name: impl Into<String>) -> Self {
            self.qualified_name = qualified_name.into();
            self
        }

        fn with_symbol_kind(mut self, symbol_kind: impl Into<String>) -> Self {
            self.symbol_kind = symbol_kind.into();
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

    fn hot_adjacency_index_struct_source() -> &'static str {
        let source = include_str!("query_hot_index.rs");
        let start = source
            .find("pub(crate) struct HotAdjacencyIndex")
            .expect("HotAdjacencyIndex struct source");
        let end = source[start..]
            .find("impl HotQueryIndex")
            .expect("HotQueryIndex impl marker");
        &source[start..start + end]
    }

    fn hot_query_index_search_source() -> &'static str {
        let source = include_str!("query_hot_index.rs");
        let start = source
            .find("pub(crate) fn search_symbols(&self")
            .expect("HotQueryIndex::search_symbols source");
        let end = source[start..]
            .find("pub(crate) fn symbol_by_id")
            .expect("HotQueryIndex::symbol_by_id marker");
        &source[start..start + end]
    }

    fn edge(
        source_stable_symbol_id: &str,
        target_stable_symbol_id: Option<&str>,
        target_label: Option<&str>,
        relation: RelationKind,
    ) -> GraphEdgeArtifact {
        GraphEdgeArtifact {
            source_stable_symbol_id: source_stable_symbol_id.to_owned(),
            target_stable_symbol_id: target_stable_symbol_id.map(str::to_owned),
            target_label: target_label.map(str::to_owned),
            import_path: None,
            relation,
            confidence: Confidence::SyntaxExact,
            confidence_score: 1.0,
            change_kind: None,
            edge_kind: None,
            bind_method: None,
            receiver_text: None,
            scope_text: None,
        }
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
        assert!(
            !source.contains("HashMap<String"),
            "HotQueryIndex must not retain an owned String-key lookup table"
        );
    }

    #[test]
    fn hot_adjacency_index_keys_adjacency_by_compact_codes() {
        let source = hot_adjacency_index_struct_source();

        for field in [
            "resolved_by_source",
            "resolved_by_target",
            "unresolved_by_source",
            "unresolved_by_label",
        ] {
            assert!(
                source.contains(field),
                "HotAdjacencyIndex must retain `{field}` for caller/callee lookups"
            );
        }
        assert!(
            source.contains("HashMap<SymbolRowCode, Vec<usize>>"),
            "resolved and unresolved source/target adjacency must be keyed by symbol row codes"
        );
        assert!(
            source.contains("HashMap<UnresolvedLabelCode, Vec<usize>>"),
            "unresolved-label adjacency must be keyed by compact label codes"
        );
        assert!(
            !source.contains("HashMap<String"),
            "HotAdjacencyIndex must not retain String-keyed adjacency maps"
        );
        assert!(
            !source.contains("RecordBatch"),
            "HotAdjacencyIndex must not add a second long-lived edge RecordBatch"
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
        let batch = symbol_batch(&rows);
        let expected = symbol_from_batch_row(&batch, 1).expect("p0 helper symbol");
        let index = HotQueryIndex::new(batch).expect("batch-backed hot index");

        let symbol = index.symbol_by_id("sid-001").expect("indexed symbol");

        assert_eq!(symbol, expected);
        assert_eq!(symbol.stable_symbol_id, "sid-001");
        assert_eq!(symbol.entity_name, "beta");
        assert_eq!(symbol.qualified_name, "module::beta");
        assert_eq!(symbol.file_path, "src/file_001.rs");
        assert_eq!(symbol.byte_range, [1, 11]);
        assert_eq!(symbol.line_range, [2, 3]);
        assert_eq!(symbol.symbol_kind, "function");
        assert_eq!(symbol.anchor_hash, "anchor-001");
        assert_eq!(symbol.enclosing_scope.as_deref(), Some("impl Owner"));
        assert!(index.symbol_by_id("sid-missing").is_none());
    }

    #[test]
    fn hot_adjacency_index_preserves_resolved_and_unresolved_records() {
        let rows = vec![
            TestSymbol::new(0, "source_fn").with_qualified_name("crate::source_fn"),
            TestSymbol::new(1, "target_fn").with_qualified_name("crate::target_fn"),
            TestSymbol::new(2, "unresolved_caller").with_qualified_name("crate::unresolved_caller"),
        ];
        let symbols = Arc::new(HotQueryIndex::new(symbol_batch(&rows)).expect("symbols index"));
        let source = symbols.symbol_by_id("sid-000").expect("source symbol");
        let target = symbols.symbol_by_id("sid-001").expect("target symbol");
        let unresolved_caller = symbols
            .symbol_by_id("sid-002")
            .expect("unresolved caller symbol");
        let edges = vec![
            edge(
                "sid-000",
                Some("sid-001"),
                Some("target_fn"),
                RelationKind::Calls,
            ),
            edge(
                "sid-002",
                None,
                Some("crate::target_fn"),
                RelationKind::Calls,
            ),
            edge(
                "sid-000",
                None,
                Some("external_target"),
                RelationKind::Calls,
            ),
            edge(
                "sid-002",
                Some("sid-001"),
                Some("target_fn"),
                RelationKind::Contains,
            ),
        ];
        let index = HotAdjacencyIndex::new(Arc::clone(&symbols), edges.clone());
        let target_labels = HashSet::from([
            "target_fn".to_owned(),
            "crate::target_fn".to_owned(),
            "sid-001".to_owned(),
        ]);

        assert_eq!(
            index.caller_records("sid-001", &target_labels),
            vec![
                OwnedCallerRecord::Resolved {
                    caller: source.clone(),
                    edge: edges[0].clone(),
                },
                OwnedCallerRecord::Unresolved {
                    caller: unresolved_caller.clone(),
                    target_label: "crate::target_fn".to_owned(),
                    edge: edges[1].clone(),
                },
            ]
        );
        assert_eq!(
            index.unresolved_caller_records(&target_labels),
            vec![OwnedCallerRecord::Unresolved {
                caller: unresolved_caller,
                target_label: "crate::target_fn".to_owned(),
                edge: edges[1].clone(),
            }]
        );
        assert_eq!(
            index.callee_records("sid-000"),
            vec![
                OwnedCalleeRecord::Resolved {
                    symbol: target,
                    edge: edges[0].clone(),
                },
                OwnedCalleeRecord::Unresolved {
                    edge: edges[2].clone(),
                    target_label: "external_target".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn hot_adjacency_materializes_adjacent_rows_without_per_result_id_lookups() {
        let rows = vec![
            TestSymbol::new(0, "source_fn"),
            TestSymbol::new(1, "target_fn"),
            TestSymbol::new(2, "unresolved_caller"),
        ];
        let symbols = Arc::new(HotQueryIndex::new(symbol_batch(&rows)).expect("symbols index"));
        let edges = vec![
            edge(
                "sid-000",
                Some("sid-001"),
                Some("target_fn"),
                RelationKind::Calls,
            ),
            edge("sid-002", None, Some("target_fn"), RelationKind::Calls),
            edge(
                "sid-000",
                None,
                Some("external_target"),
                RelationKind::Calls,
            ),
        ];
        let index = HotAdjacencyIndex::new(Arc::clone(&symbols), edges);
        let target_labels = HashSet::from(["target_fn".to_owned()]);
        symbols.reset_symbol_row_lookups();
        symbols.reset_symbol_column_projections();

        let callers = index.caller_records("sid-001", &target_labels);
        let callees = index.callee_records("sid-000");

        assert_eq!(callers.len(), 2);
        assert_eq!(callees.len(), 2);
        assert_eq!(
            callees.capacity(),
            callees.len(),
            "callee queries should reserve the known adjacency result count"
        );
        assert_eq!(
            symbols.symbol_row_lookups(),
            2,
            "caller/callee queries should resolve only their input symbol IDs"
        );
        assert_eq!(
            symbols.symbol_column_projections(),
            2,
            "caller/callee queries should project Arrow columns once per query"
        );
    }

    #[test]
    fn hot_query_index_search_preserves_modes_filters_and_ranking() {
        let rows = vec![
            TestSymbol::new(0, "helper")
                .with_qualified_name("target")
                .with_file_path("src/exact_b.rs"),
            TestSymbol::new(1, "target")
                .with_qualified_name("module::target")
                .with_file_path("src/exact_a.rs"),
            TestSymbol::new(2, "targetish"),
            TestSymbol::new(3, "submit_plan"),
            TestSymbol::new(4, "submitter"),
            TestSymbol::new(5, "submit"),
            TestSymbol::new(6, "alpha_def"),
            TestSymbol::new(7, "zdeflong"),
            TestSymbol::new(8, "adef"),
            TestSymbol::new(9, "def"),
            TestSymbol::new(10, "run_query")
                .with_file_path("crates/foo/src/lib.rs")
                .with_symbol_kind("function"),
            TestSymbol::new(11, "run_query")
                .with_file_path("crates/foo/src/nested/mod.rs")
                .with_symbol_kind("mcp_tool"),
            TestSymbol::new(12, "run_query")
                .with_file_path("crates/bar/src/lib.rs")
                .with_symbol_kind("mcp_tool"),
        ];
        let index = HotQueryIndex::new(symbol_batch(&rows)).expect("batch-backed hot index");

        let exact = index.search_symbols(&search_options("target", SearchMode::Exact));
        assert_eq!(result_ids(&exact), vec!["sid-001", "sid-000"]);

        let prefix = index.search_symbols(&search_options("sub", SearchMode::Prefix));
        assert_eq!(result_ids(&prefix), vec!["sid-005", "sid-004", "sid-003"]);

        let substring = index.search_symbols(&search_options("def", SearchMode::Substring));
        assert_eq!(
            result_ids(&substring),
            vec!["sid-009", "sid-008", "sid-007", "sid-006"]
        );

        let mut file_options = search_options("run", SearchMode::Substring);
        file_options.filters.file = Some("crates/bar/src/lib.rs".to_owned());
        let file_filtered = index.search_symbols(&file_options);
        assert_eq!(result_ids(&file_filtered), vec!["sid-012"]);

        let mut kind_glob_options = search_options("run", SearchMode::Substring);
        kind_glob_options.filters.symbol_kind = Some("mcp_tool".to_owned());
        kind_glob_options.filters.file_glob = Some("crates/foo/**/*.rs".to_owned());
        let kind_glob_filtered = index.search_symbols(&kind_glob_options);
        assert_eq!(result_ids(&kind_glob_filtered), vec!["sid-011"]);
    }

    #[test]
    fn hot_query_index_search_partitions_matching_rows_without_window_rescans() {
        let source = hot_query_index_search_source();

        assert!(
            source.contains("select_nth_unstable_by"),
            "bounded Arrow search should partition matching row codes once"
        );
        assert!(
            !source.contains("push_candidate_row"),
            "bounded Arrow search must not rescan the candidate window for every match"
        );
    }

    #[test]
    fn hot_query_index_search_materializes_only_public_result_window() {
        let rows = (0..PUBLIC_SEARCH_LIMIT + 5)
            .map(|index| TestSymbol::new(index, format!("target_{index:03}")))
            .collect::<Vec<_>>();
        let index = HotQueryIndex::new(symbol_batch(&rows)).expect("batch-backed hot index");

        index.reset_search_symbol_materializations();
        let result = index.search_symbols(&SearchOptions {
            query: "target_".to_owned(),
            mode: SearchMode::Prefix,
            filters: SearchFilters::default(),
            limit: PUBLIC_SEARCH_LIMIT + 50,
        });
        let materializations = index.search_symbol_materializations();

        assert_eq!(result.total_matches, PUBLIC_SEARCH_LIMIT + 5);
        assert_eq!(result.candidates.len(), PUBLIC_SEARCH_LIMIT);
        assert!(result.truncated);
        assert_eq!(
            materializations,
            result.candidates.len(),
            "search should materialize only selected public result rows"
        );
        assert!(
            materializations <= PUBLIC_SEARCH_LIMIT,
            "search materialized {materializations} rows, exceeding PUBLIC_SEARCH_LIMIT"
        );
        assert!(result
            .candidates
            .iter()
            .all(|symbol| symbol.entity_name.starts_with("target_")));
    }
}

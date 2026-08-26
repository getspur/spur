use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use globset::Glob;

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
    search_symbols: Vec<SearchSymbol>,
    symbols: Vec<GraphSymbolArtifact>,
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
    pub(crate) fn new(symbols: Vec<GraphSymbolArtifact>) -> Self {
        let search_symbols = symbols.iter().map(SearchSymbol::from).collect();
        let symbol_by_id = symbols
            .iter()
            .enumerate()
            .map(|(index, symbol)| (symbol.stable_symbol_id.clone(), index))
            .collect();
        Self {
            search_symbols,
            symbols,
            symbol_by_id,
        }
    }

    pub(crate) fn search_symbols(&self, options: &SearchOptions) -> SearchResult {
        let glob = options
            .filters
            .file_glob
            .as_deref()
            .and_then(|pattern| Glob::new(pattern).ok())
            .map(|glob| glob.compile_matcher());
        let mut candidates = self
            .search_symbols
            .iter()
            .filter(|symbol| matches_query(symbol, options))
            .filter(|symbol| matches_filters(symbol, &options.filters, glob.as_ref()))
            .collect::<Vec<_>>();

        let total_matches = candidates.len();
        if options.limit != INTERNAL_SEARCH_UNBOUNDED {
            let limit = options.limit.clamp(1, PUBLIC_SEARCH_LIMIT);
            if candidates.len() > limit {
                candidates.select_nth_unstable_by(limit, |left, right| {
                    compare_symbols(left, right, options)
                });
                candidates.truncate(limit);
            }
        }
        candidates.sort_by(|left, right| compare_symbols(left, right, options));
        let candidates = candidates.into_iter().cloned().collect();
        limited_search_result(candidates, total_matches, options.limit)
    }

    pub(crate) fn symbol_by_id(&self, stable_symbol_id: &str) -> Option<&GraphSymbolArtifact> {
        self.symbol_by_id
            .get(stable_symbol_id)
            .map(|index| &self.symbols[*index])
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
                    caller: caller.clone(),
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
                    caller: caller.clone(),
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
                let caller = self.symbol_by_id(&edge.source_stable_symbol_id)?.clone();
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
                    symbol: symbol.clone(),
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

    fn symbol_by_id(&self, stable_symbol_id: &str) -> Option<&GraphSymbolArtifact> {
        self.symbols.symbol_by_id(stable_symbol_id)
    }
}

fn is_caller_relation(relation: RelationKind) -> bool {
    matches!(relation, RelationKind::Calls | RelationKind::References)
}

use std::sync::Arc;

use crate::temporal::TemporalIndex;
use crate::{
    find_callee_edges, find_caller_edges, resolve_selector, search_symbols, CalleeRecord,
    CallerRecord, GraphFileManifestEntry, GraphIndexArtifact, SearchOptions, SearchResult,
    SelectorResolution,
};

pub type CodeSelectorResolution = SelectorResolution;

pub trait GraphQueryClient {
    fn search_symbols(&self, opts: &SearchOptions) -> SearchResult<'_>;
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
    fn search_symbols(&self, opts: &SearchOptions) -> SearchResult<'_> {
        search_symbols(&self.artifact, opts)
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

    fn ids(result: &SearchResult<'_>) -> Vec<String> {
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

        let actual = client.search_symbols(&options);

        assert_eq!(ids(&actual), ids(&expected));
        assert_eq!(actual.total_matches, expected.total_matches);
        assert_eq!(actual.truncated, expected.truncated);
    }
}

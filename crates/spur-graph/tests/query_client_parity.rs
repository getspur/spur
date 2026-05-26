use std::sync::Arc;

use spur_graph::{
    write_artifact_parquet, GraphIndexArtifact, GraphIndexHeader, GraphQueryClient,
    GraphSymbolArtifact, InMemoryClient, NodeId, ParquetClient, SearchFilters, SearchMode,
    SearchOptions, WriteOptions,
};

fn artifact() -> GraphIndexArtifact {
    let symbols = vec![
        symbol("qualified-match", "src/b.rs", [2, 3], "helper", "target"),
        symbol(
            "entity-match",
            "src/a.rs",
            [10, 11],
            "target",
            "module::target",
        ),
        symbol(
            "submit-plan",
            "src/lib.rs",
            [30, 31],
            "submit_plan",
            "submit_plan",
        ),
        symbol(
            "submitter",
            "src/lib.rs",
            [20, 21],
            "submitter",
            "submitter",
        ),
        symbol("submit", "src/lib.rs", [10, 11], "submit", "submit"),
        symbol(
            "alpha-def",
            "src/lib.rs",
            [40, 41],
            "alpha_def",
            "alpha_def",
        ),
        symbol("z-def-long", "src/lib.rs", [30, 31], "zdeflong", "zdeflong"),
        symbol("a-def", "src/lib.rs", [20, 21], "adef", "adef"),
        symbol("def", "src/lib.rs", [10, 11], "def", "def"),
        symbol(
            "foo-lib",
            "crates/foo/src/lib.rs",
            [1, 2],
            "run_query",
            "run_query",
        ),
        symbol(
            "foo-nested",
            "crates/foo/src/nested/mod.rs",
            [1, 2],
            "run_query",
            "run_query",
        ),
        symbol(
            "bar-lib",
            "crates/bar/src/lib.rs",
            [1, 2],
            "run_query",
            "run_query",
        ),
    ];
    let symbol_node_ids = (1..=symbols.len()).map(|id| NodeId(id as u64)).collect();

    GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: "test".to_string(),
            content_hash_blake3: None,
        },
        manifest_version: "test".to_string(),
        graph_content_hash: "query-client-parity".to_string(),
        file_manifests: Vec::new(),
        files: Vec::new(),
        file_node_ids: Vec::new(),
        symbols,
        symbol_node_ids,
        edges: Vec::new(),
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
        commits: Vec::new(),
        symbol_snapshots: Vec::new(),
        temporal_edges: Vec::new(),
    }
}

fn symbol(
    id: &str,
    file_path: &str,
    line_range: [usize; 2],
    entity_name: &str,
    qualified_name: &str,
) -> GraphSymbolArtifact {
    GraphSymbolArtifact {
        stable_symbol_id: id.to_string(),
        file_path: file_path.to_string(),
        byte_range: [0, 8],
        line_range,
        entity_name: entity_name.to_string(),
        qualified_name: qualified_name.to_string(),
        symbol_kind: "function".to_string(),
        anchor_hash: format!("hash-{id}"),
        enclosing_scope: None,
    }
}

fn options(query: &str, mode: SearchMode) -> SearchOptions {
    SearchOptions {
        query: query.to_string(),
        mode,
        filters: SearchFilters::default(),
        limit: 200,
    }
}

#[test]
fn parquet_client_search_symbols_matches_in_memory_client() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = artifact();
    let parquet_dir = write_artifact_parquet(&artifact, tempdir.path(), WriteOptions::default())
        .expect("write parquet artifact");
    let in_memory = InMemoryClient::new(Arc::new(artifact));
    let parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");

    for options in [
        options("target", SearchMode::Exact),
        options("sub", SearchMode::Prefix),
        options("def", SearchMode::Substring),
        SearchOptions {
            query: "run".to_string(),
            mode: SearchMode::Substring,
            filters: SearchFilters {
                file_glob: Some("crates/foo/**/*.rs".to_string()),
                ..SearchFilters::default()
            },
            limit: 200,
        },
    ] {
        let expected = in_memory
            .search_symbols(&options)
            .expect("in-memory search succeeds");
        let actual = parquet
            .search_symbols(&options)
            .expect("parquet search succeeds");

        assert_eq!(actual, expected, "options: {options:?}");
    }
}

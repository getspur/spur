use std::sync::Arc;

use spur_graph::{
    write_artifact_parquet, Confidence, GraphEdgeArtifact, GraphEdgeKind, GraphFileArtifact,
    GraphFileManifestEntry, GraphIndexArtifact, GraphIndexHeader, GraphQueryClient,
    GraphSymbolArtifact, InMemoryClient, NodeId, OwnedCalleeRecord, OwnedCallerRecord,
    ParquetClient, RelationKind, SearchFilters, SearchMode, SearchOptions, WriteOptions,
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
            "aaaaaaaaaaaaaaaa",
            "src/hex.rs",
            [1, 2],
            "hex_target",
            "hex_target",
        ),
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
        symbol("caller", "src/callers.rs", [50, 51], "caller", "caller"),
        symbol(
            "unresolved-caller",
            "src/callers.rs",
            [55, 56],
            "unresolved_caller",
            "unresolved_caller",
        ),
        symbol("target", "src/callees.rs", [60, 61], "target", "target"),
        symbol("root", "src/root.rs", [70, 71], "root", "root"),
        symbol("callee", "src/root.rs", [80, 81], "callee", "callee"),
    ];
    let symbol_node_ids = (1..=symbols.len()).map(|id| NodeId(id as u64)).collect();

    GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: "test".to_string(),
            content_hash_blake3: None,
        },
        manifest_version: "test".to_string(),
        graph_content_hash: "query-client-parity".to_string(),
        file_manifests: file_manifests(),
        files: files(),
        file_node_ids: (101..=107).map(NodeId).collect(),
        symbols,
        symbol_node_ids,
        edges: vec![
            edge("caller", Some("target"), Some("target")),
            edge("unresolved-caller", None, Some("target")),
            edge("root", Some("callee"), Some("callee")),
            edge("root", None, Some("external_call")),
        ],
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
        commits: Vec::new(),
        symbol_snapshots: Vec::new(),
        temporal_edges: Vec::new(),
    }
}

fn files() -> Vec<GraphFileArtifact> {
    [
        "src/a.rs",
        "src/b.rs",
        "src/lib.rs",
        "crates/foo/src/lib.rs",
        "crates/foo/src/nested/mod.rs",
        "crates/bar/src/lib.rs",
        "src/hex.rs",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, path)| GraphFileArtifact {
        stable_file_id: format!("file-{index}"),
        file_path: path.to_string(),
    })
    .collect()
}

fn file_manifests() -> Vec<GraphFileManifestEntry> {
    files()
        .into_iter()
        .enumerate()
        .map(|(index, file)| GraphFileManifestEntry {
            stable_file_id: file.stable_file_id,
            path: file.file_path,
            content_oid: format!("{:040x}", index + 1),
            node_ids: vec![NodeId((index as u64) + 1), NodeId((index as u64) + 50)],
        })
        .collect()
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

fn edge(source: &str, target: Option<&str>, target_label: Option<&str>) -> GraphEdgeArtifact {
    GraphEdgeArtifact {
        source_stable_symbol_id: source.to_string(),
        target_stable_symbol_id: target.map(str::to_string),
        target_label: target_label.map(str::to_string),
        relation: RelationKind::Calls,
        confidence: Confidence::SyntaxExact,
        confidence_score: 1.0,
        change_kind: None,
        edge_kind: Some(GraphEdgeKind::Calls),
        bind_method: None,
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

#[test]
fn parquet_client_find_caller_edges_matches_in_memory_client() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = artifact();
    let parquet_dir = write_artifact_parquet(&artifact, tempdir.path(), WriteOptions::default())
        .expect("write parquet artifact");
    let in_memory = InMemoryClient::new(Arc::new(artifact));
    let parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");

    let expected = caller_records(in_memory.find_caller_edges("target"));
    let actual = caller_records(parquet.find_caller_edges("target"));

    assert_eq!(actual, expected);
}

#[test]
fn parquet_client_find_callee_edges_matches_in_memory_client() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = artifact();
    let parquet_dir = write_artifact_parquet(&artifact, tempdir.path(), WriteOptions::default())
        .expect("write parquet artifact");
    let in_memory = InMemoryClient::new(Arc::new(artifact));
    let parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");

    let expected = callee_records(in_memory.find_callee_edges("root"));
    let actual = callee_records(parquet.find_callee_edges("root"));

    assert_eq!(actual, expected);
}

#[test]
fn parquet_client_resolve_selector_matches_in_memory_client() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = artifact();
    let parquet_dir = write_artifact_parquet(&artifact, tempdir.path(), WriteOptions::default())
        .expect("write parquet artifact");
    let in_memory = InMemoryClient::new(Arc::new(artifact));
    let parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");

    for selector in [
        "graph://symbol/entity-match",
        "aaaaaaaaaaaaaaaa",
        "module::target",
        "src/a.rs::module::target",
        "submit_plan",
        "run_query",
    ] {
        assert_eq!(
            parquet
                .resolve_selector(selector)
                .expect("parquet resolve selector succeeds"),
            in_memory
                .resolve_selector(selector)
                .expect("in-memory resolve selector succeeds"),
            "selector: {selector}"
        );
    }
}

#[test]
fn parquet_client_file_manifest_by_path_matches_in_memory_client() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let artifact = artifact();
    let parquet_dir = write_artifact_parquet(&artifact, tempdir.path(), WriteOptions::default())
        .expect("write parquet artifact");
    let in_memory = InMemoryClient::new(Arc::new(artifact));
    let parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");

    let expected = in_memory
        .file_manifest_by_path("src/a.rs")
        .expect("in-memory manifest query succeeds");
    let actual = parquet
        .file_manifest_by_path("src/a.rs")
        .expect("parquet manifest query succeeds");

    assert_eq!(actual, expected);
    assert_eq!(
        actual.expect("manifest is present").node_ids,
        vec![NodeId(1), NodeId(50)]
    );
    assert_eq!(
        parquet
            .file_manifest_by_path("src/missing.rs")
            .expect("missing manifest query succeeds"),
        None
    );
}

fn caller_records(records: Vec<OwnedCallerRecord>) -> Vec<(String, String, bool, Option<String>)> {
    records
        .into_iter()
        .map(|record| match record {
            OwnedCallerRecord::Resolved { caller, edge } => (
                caller.stable_symbol_id.clone(),
                edge.source_stable_symbol_id.clone(),
                true,
                edge.target_label.clone(),
            ),
            OwnedCallerRecord::Unresolved {
                caller,
                edge,
                target_label,
            } => (
                caller.stable_symbol_id.clone(),
                edge.source_stable_symbol_id.clone(),
                false,
                Some(target_label),
            ),
        })
        .collect()
}

fn callee_records(records: Vec<OwnedCalleeRecord>) -> Vec<(String, String, bool, Option<String>)> {
    records
        .into_iter()
        .map(|record| match record {
            OwnedCalleeRecord::Resolved { symbol, edge } => (
                symbol.stable_symbol_id.clone(),
                edge.source_stable_symbol_id.clone(),
                true,
                edge.target_label.clone(),
            ),
            OwnedCalleeRecord::Unresolved { edge, target_label } => (
                target_label.clone(),
                edge.source_stable_symbol_id.clone(),
                false,
                Some(target_label),
            ),
        })
        .collect()
}

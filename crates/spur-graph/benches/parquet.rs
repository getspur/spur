use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use serde::Deserialize;
use spur_graph::schema::GRAPH_INDEX_VERSION_TEMPORAL;
use spur_graph::store::parquet::read_artifact_parquet_slim;
use spur_graph::{
    artifact_from_facts, build_facts, read_artifact_parquet, write_artifact_parquet, ChangeKind,
    CommitArtifact, Confidence, EdgeEndpoint, GraphEdgeArtifact, GraphEdgeKind, GraphIndexArtifact,
    GraphIndexHeader, GraphQueryClient, GraphSymbolArtifact, InMemoryClient, NodeId, ParquetClient,
    RelationKind, RenamePrev, SearchFilters, SearchMode, SearchOptions, SnapshotKey,
    SymbolSnapshotArtifact, TemporalEdgeArtifact, WriteOptions,
};

#[derive(Debug, Deserialize)]
struct Baselines {
    fixture_path: String,
}

fn bench_write_artifact_parquet(c: &mut Criterion) {
    let fixture = load_fixture();
    let tempdir = tempfile::tempdir().expect("tempdir");

    c.bench_function("write_artifact_parquet", |b| {
        b.iter(|| {
            let dir = write_artifact_parquet(
                black_box(&fixture.artifact),
                tempdir.path(),
                WriteOptions::default(),
                Vec::new(),
            )
            .expect("write parquet artifact");
            black_box(dir);
        });
    });
}

fn bench_read_artifact_parquet(c: &mut Criterion) {
    let fixture = load_fixture();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir = write_artifact_parquet(
        &fixture.artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet artifact");

    c.bench_function("read_artifact_parquet", |b| {
        b.iter(|| {
            let artifact =
                read_artifact_parquet(black_box(&parquet_dir)).expect("read parquet artifact");
            black_box(artifact);
        });
    });
}

fn bench_read_artifact_parquet_slim(c: &mut Criterion) {
    let fixture = load_fixture();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir = write_artifact_parquet(
        &fixture.artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet artifact");

    c.bench_function("read_artifact_parquet_slim", |b| {
        b.iter(|| {
            let artifact = read_artifact_parquet_slim(black_box(&parquet_dir))
                .expect("read parquet artifact (slim)");
            black_box(artifact);
        });
    });
}

fn bench_search_symbols_parquet_vs_inmemory(c: &mut Criterion) {
    let fixture = load_fixture();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir = write_artifact_parquet(
        &fixture.artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet artifact");
    let query = search_benchmark_query(&fixture.artifact);
    let options = SearchOptions {
        query,
        mode: SearchMode::Exact,
        filters: SearchFilters::default(),
        limit: 20,
    };
    let mut group = c.benchmark_group("bench_search_symbols_parquet_vs_inmemory");

    group.bench_function("inmemory_load_then_search", |b| {
        b.iter(|| {
            let artifact = read_artifact_parquet(black_box(parquet_dir.as_path()))
                .expect("read parquet artifact");
            let in_memory = InMemoryClient::new(Arc::new(artifact));
            let result = in_memory
                .search_symbols(black_box(&options))
                .expect("in-memory search symbols");
            black_box(result);
        });
    });
    group.bench_function("parquet_open_then_search", |b| {
        b.iter(|| {
            let parquet =
                ParquetClient::open(black_box(parquet_dir.as_path())).expect("open parquet client");
            let result = parquet
                .search_symbols(black_box(&options))
                .expect("parquet search symbols");
            black_box(result);
        });
    });
    group.finish();
}

fn bench_find_caller_edges_parquet_vs_inmemory(c: &mut Criterion) {
    let artifact = traversal_benchmark_artifact();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir = write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet artifact");
    let target_sid = "target";
    let in_memory = InMemoryClient::new(Arc::new(artifact));
    let parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");
    let mut group = c.benchmark_group("bench_find_caller_edges_parquet_vs_inmemory");

    group.bench_function("inmemory", |b| {
        b.iter(|| {
            let records = in_memory.find_caller_edges(black_box(target_sid));
            black_box(records);
        });
    });
    group.bench_function("parquet", |b| {
        b.iter(|| {
            let records = parquet.find_caller_edges(black_box(target_sid));
            black_box(records);
        });
    });
    group.finish();
}

fn bench_find_callee_edges_parquet_vs_inmemory(c: &mut Criterion) {
    let artifact = traversal_benchmark_artifact();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir = write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet artifact");
    let source_sid = "source";
    let in_memory = InMemoryClient::new(Arc::new(artifact));
    let parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");
    let mut group = c.benchmark_group("bench_find_callee_edges_parquet_vs_inmemory");

    group.bench_function("inmemory", |b| {
        b.iter(|| {
            let records = in_memory.find_callee_edges(black_box(source_sid));
            black_box(records);
        });
    });
    group.bench_function("parquet", |b| {
        b.iter(|| {
            let records = parquet.find_callee_edges(black_box(source_sid));
            black_box(records);
        });
    });
    group.finish();
}

fn bench_resolve_selector_parquet_vs_inmemory(c: &mut Criterion) {
    let artifact = traversal_benchmark_artifact();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir = write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet artifact");
    let selector = "target";
    let in_memory = InMemoryClient::new(Arc::new(artifact));
    let parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");
    let mut group = c.benchmark_group("bench_resolve_selector_parquet_vs_inmemory");

    group.bench_function("inmemory", |b| {
        b.iter(|| {
            let resolution = in_memory
                .resolve_selector(black_box(selector))
                .expect("in-memory resolve selector");
            black_box(resolution);
        });
    });
    group.bench_function("parquet", |b| {
        b.iter(|| {
            let resolution = parquet
                .resolve_selector(black_box(selector))
                .expect("parquet resolve selector");
            black_box(resolution);
        });
    });
    group.finish();
}

fn bench_temporal_index_first_call_parquet_vs_inmemory(c: &mut Criterion) {
    let artifact = temporal_benchmark_artifact();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir = write_artifact_parquet(
        &artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet artifact");
    let persisted_artifact =
        Arc::new(read_artifact_parquet(&parquet_dir).expect("read full parquet artifact"));
    let steady_parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");
    black_box(steady_parquet.temporal_index());
    let mut group = c.benchmark_group("bench_temporal_index_first_call_parquet_vs_inmemory");

    group.bench_function("inmemory_first_call", |b| {
        b.iter(|| {
            let in_memory = InMemoryClient::new(Arc::clone(&persisted_artifact));
            let index = in_memory.temporal_index();
            black_box(index);
        });
    });
    group.bench_function("parquet_first_call", |b| {
        b.iter(|| {
            let parquet =
                ParquetClient::open(black_box(parquet_dir.as_path())).expect("open parquet client");
            let index = parquet.temporal_index();
            black_box(index);
        });
    });
    group.bench_function("parquet_steady_state", |b| {
        b.iter(|| {
            let index = steady_parquet.temporal_index();
            black_box(index);
        });
    });
    group.finish();
}

fn bench_end_to_end_mcp_latency_session(c: &mut Criterion) {
    let fixture = load_fixture();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir = write_artifact_parquet(
        &fixture.artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet artifact");
    let options = SearchOptions {
        query: search_benchmark_query(&fixture.artifact),
        mode: SearchMode::Exact,
        filters: SearchFilters::default(),
        limit: 20,
    };
    let mut group = c.benchmark_group("bench_end_to_end_mcp_latency_session");

    group.bench_function("parquet_open_then_session", |b| {
        b.iter(|| {
            let parquet =
                ParquetClient::open(black_box(parquet_dir.as_path())).expect("open parquet client");
            run_mcp_latency_session(black_box(&parquet), black_box(&options));
        });
    });
    group.finish();
}

fn run_mcp_latency_session(client: &dyn GraphQueryClient, options: &SearchOptions) {
    let search = client
        .search_symbols(options)
        .expect("MCP session code_search");
    let symbol_id = search
        .candidates
        .first()
        .expect("benchmark query returns at least one symbol")
        .stable_symbol_id
        .clone();
    let symbol = client
        .symbol_by_id(&symbol_id)
        .expect("MCP session code_read_symbol lookup")
        .expect("MCP session symbol exists");
    let manifest = client
        .file_manifest_by_path(&symbol.file_path)
        .expect("MCP session file manifest lookup");
    let callers = client.find_caller_edges(&symbol_id);
    black_box((search, symbol, manifest, callers));
}

fn load_fixture() -> Fixture {
    let baselines = baselines();
    let fixture_path = std::env::var_os("SPUR_GRAPH_PERF_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(baselines.fixture_path));
    let repo_root = fixture_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            panic!(
                "fixture path `{}` is expected to live under <repo>/.spur/graph-index.json",
                fixture_path.display()
            )
        });
    let (facts, _counts) = build_facts(&repo_root, None).unwrap_or_else(|err| {
        panic!(
            "failed to build facts for `{}`: {err:#}",
            repo_root.display()
        )
    });
    let artifact = artifact_from_facts(&facts, &repo_root).unwrap_or_else(|err| {
        panic!(
            "failed to build artifact for `{}`: {err:#}",
            repo_root.display()
        )
    });
    Fixture {
        artifact,
        fixture_path,
    }
}

fn traversal_benchmark_artifact() -> GraphIndexArtifact {
    let mut symbols = vec![
        symbol("source", "src/source.rs", "source"),
        symbol("target", "src/target.rs", "target"),
        symbol("callee", "src/callee.rs", "callee"),
        symbol("unresolved-caller", "src/caller.rs", "unresolved_caller"),
    ];
    for index in 0..512 {
        let id = format!("noise-{index:04}");
        symbols.push(symbol(&id, "src/noise.rs", &id));
    }
    let symbol_node_ids = (1..=symbols.len()).map(|id| NodeId(id as u64)).collect();

    let mut edges = vec![
        edge("source", Some("target"), Some("target")),
        edge("unresolved-caller", None, Some("target")),
        edge("source", Some("callee"), Some("callee")),
        edge("source", None, Some("external_call")),
    ];
    for index in 0..512 {
        let source = format!("noise-{index:04}");
        let target = format!("noise-{:04}", (index + 1) % 512);
        edges.push(edge(&source, Some(&target), Some(&target)));
    }

    GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: "bench".to_owned(),
            content_hash_blake3: None,
        },
        manifest_version: "bench".to_owned(),
        graph_content_hash: "traversal-benchmark".to_owned(),
        file_manifests: Vec::new(),
        files: Vec::new(),
        file_node_ids: Vec::new(),
        symbols,
        symbol_node_ids,
        edges,
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
        commits: Vec::new(),
        symbol_snapshots: Vec::new(),
        temporal_edges: Vec::new(),
    }
}

fn temporal_benchmark_artifact() -> GraphIndexArtifact {
    let symbol_count = 512usize;
    let mut artifact = traversal_benchmark_artifact();
    artifact.header.graph_index_version = GRAPH_INDEX_VERSION_TEMPORAL.to_owned();
    artifact.graph_content_hash = "temporal-benchmark".to_owned();
    artifact.commits.clear();
    artifact.symbol_snapshots.clear();
    artifact.temporal_edges.clear();

    for index in 0..symbol_count {
        let old_commit = format!("commit-{:04}", index * 2);
        let new_commit = format!("commit-{:04}", index * 2 + 1);
        artifact.commits.push(CommitArtifact {
            sha: old_commit.clone(),
            parents: if index == 0 {
                Vec::new()
            } else {
                vec![format!("commit-{:04}", index * 2 - 1)]
            },
            author_time: (index * 2) as i64,
            author_name: String::new(),
            author_email: String::new(),
            summary: format!("add temporal {index}"),
        });
        artifact.commits.push(CommitArtifact {
            sha: new_commit.clone(),
            parents: vec![old_commit.clone()],
            author_time: (index * 2 + 1) as i64,
            author_name: String::new(),
            author_email: String::new(),
            summary: format!("rename temporal {index}"),
        });

        let old = snapshot(
            &format!("temporal-old-{index:04}"),
            &old_commit,
            &format!("temporal_old_{index:04}"),
        );
        let new = snapshot(
            &format!("temporal-new-{index:04}"),
            &new_commit,
            &format!("temporal_new_{index:04}"),
        );
        artifact.symbol_snapshots.push(old.clone());
        artifact.symbol_snapshots.push(new.clone());
        artifact.temporal_edges.push(temporal_touch(
            &old_commit,
            old.key.clone(),
            ChangeKind::Added,
        ));
        artifact.temporal_edges.push(temporal_touch(
            &new_commit,
            new.key.clone(),
            ChangeKind::RenamedFrom(RenamePrev::Symbol(old.key.clone())),
        ));
        artifact
            .temporal_edges
            .push(temporal_rename(old.key, new.key));
    }

    artifact
}

fn symbol(id: &str, file_path: &str, entity_name: &str) -> GraphSymbolArtifact {
    GraphSymbolArtifact {
        stable_symbol_id: id.to_owned(),
        file_path: file_path.to_owned(),
        byte_range: [0, 8],
        line_range: [1, 2],
        entity_name: entity_name.to_owned(),
        qualified_name: entity_name.to_owned(),
        symbol_kind: "function".to_owned(),
        anchor_hash: format!("hash-{id}"),
        enclosing_scope: None,
    }
}

fn snapshot(id: &str, commit: &str, entity_name: &str) -> SymbolSnapshotArtifact {
    SymbolSnapshotArtifact {
        key: SnapshotKey {
            stable_symbol_id: id.to_owned(),
            commit: commit.to_owned(),
        },
        file_path: "src/temporal.rs".to_owned().into(),
        entity_name: entity_name.to_owned(),
        symbol_kind: "function".to_owned(),
        enclosing_scope: None,
        byte_range: [0, 8],
        line_range: [1, 2],
        anchor_hash: format!("hash-{id}-{commit}"),
        tokens: vec![entity_name.to_owned()],
    }
}

fn temporal_touch(commit: &str, key: SnapshotKey, change_kind: ChangeKind) -> TemporalEdgeArtifact {
    TemporalEdgeArtifact {
        source: EdgeEndpoint::Commit {
            sha: commit.to_owned(),
        },
        target: EdgeEndpoint::Snapshot { key },
        relation: RelationKind::Touches,
        parent: None,
        change_kind: Some(change_kind),
    }
}

fn temporal_rename(from: SnapshotKey, to: SnapshotKey) -> TemporalEdgeArtifact {
    TemporalEdgeArtifact {
        source: EdgeEndpoint::Snapshot { key: from.clone() },
        target: EdgeEndpoint::Snapshot { key: to },
        relation: RelationKind::Touches,
        parent: None,
        change_kind: Some(ChangeKind::RenamedFrom(RenamePrev::Symbol(from))),
    }
}

fn edge(source: &str, target: Option<&str>, target_label: Option<&str>) -> GraphEdgeArtifact {
    GraphEdgeArtifact {
        source_stable_symbol_id: source.to_owned(),
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

fn search_benchmark_query(artifact: &GraphIndexArtifact) -> String {
    artifact
        .symbols
        .iter()
        .find(|symbol| symbol.entity_name == "handle_code_search")
        .or_else(|| artifact.symbols.first())
        .map(|symbol| symbol.entity_name.clone())
        .expect("benchmark fixture has at least one symbol")
}

fn baselines() -> Baselines {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("benches/baselines.json");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read `{}`: {err}", path.display()));
    serde_json::from_str(&content)
        .unwrap_or_else(|err| panic!("failed to parse `{}`: {err}", path.display()))
}

#[expect(dead_code)]
struct Fixture {
    artifact: GraphIndexArtifact,
    fixture_path: PathBuf,
}

#[expect(dead_code)]
fn median_f64(mut values: Vec<f64>) -> f64 {
    assert!(!values.is_empty(), "median requires at least one sample");
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    values[values.len() / 2]
}

criterion_group!(
    benches,
    bench_write_artifact_parquet,
    bench_read_artifact_parquet,
    bench_read_artifact_parquet_slim,
    bench_search_symbols_parquet_vs_inmemory,
    bench_find_caller_edges_parquet_vs_inmemory,
    bench_find_callee_edges_parquet_vs_inmemory,
    bench_resolve_selector_parquet_vs_inmemory,
    bench_temporal_index_first_call_parquet_vs_inmemory,
    bench_end_to_end_mcp_latency_session
);
criterion_main!(benches);

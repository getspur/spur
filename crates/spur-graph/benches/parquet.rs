use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use serde::Deserialize;
use spur_graph::store::parquet::read_artifact_parquet_slim;
use spur_graph::{
    artifact_from_facts, build_facts, read_artifact_parquet, write_artifact_parquet, Confidence,
    GraphEdgeArtifact, GraphEdgeKind, GraphIndexArtifact, GraphIndexHeader, GraphQueryClient,
    GraphSymbolArtifact, InMemoryClient, NodeId, ParquetClient, RelationKind, SearchFilters,
    SearchMode, SearchOptions, WriteOptions,
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
            )
            .expect("write parquet artifact");
            black_box(dir);
        })
    });
}

fn bench_read_artifact_parquet(c: &mut Criterion) {
    let fixture = load_fixture();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir =
        write_artifact_parquet(&fixture.artifact, tempdir.path(), WriteOptions::default())
            .expect("write parquet artifact");

    c.bench_function("read_artifact_parquet", |b| {
        b.iter(|| {
            let artifact =
                read_artifact_parquet(black_box(&parquet_dir)).expect("read parquet artifact");
            black_box(artifact);
        })
    });
}

fn bench_read_artifact_parquet_slim(c: &mut Criterion) {
    let fixture = load_fixture();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir =
        write_artifact_parquet(&fixture.artifact, tempdir.path(), WriteOptions::default())
            .expect("write parquet artifact");

    c.bench_function("read_artifact_parquet_slim", |b| {
        b.iter(|| {
            let artifact = read_artifact_parquet_slim(black_box(&parquet_dir))
                .expect("read parquet artifact (slim)");
            black_box(artifact);
        })
    });
}

fn bench_search_symbols_parquet_vs_inmemory(c: &mut Criterion) {
    let fixture = load_fixture();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir =
        write_artifact_parquet(&fixture.artifact, tempdir.path(), WriteOptions::default())
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
        })
    });
    group.bench_function("parquet_open_then_search", |b| {
        b.iter(|| {
            let parquet =
                ParquetClient::open(black_box(parquet_dir.as_path())).expect("open parquet client");
            let result = parquet
                .search_symbols(black_box(&options))
                .expect("parquet search symbols");
            black_box(result);
        })
    });
    group.finish();
}

fn bench_find_caller_edges_parquet_vs_inmemory(c: &mut Criterion) {
    let artifact = traversal_benchmark_artifact();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir = write_artifact_parquet(&artifact, tempdir.path(), WriteOptions::default())
        .expect("write parquet artifact");
    let target_sid = "target";
    let in_memory = InMemoryClient::new(Arc::new(artifact));
    let parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");
    let mut group = c.benchmark_group("bench_find_caller_edges_parquet_vs_inmemory");

    group.bench_function("inmemory", |b| {
        b.iter(|| {
            let records = in_memory.find_caller_edges(black_box(target_sid));
            black_box(records);
        })
    });
    group.bench_function("parquet", |b| {
        b.iter(|| {
            let records = parquet.find_caller_edges(black_box(target_sid));
            black_box(records);
        })
    });
    group.finish();
}

fn bench_find_callee_edges_parquet_vs_inmemory(c: &mut Criterion) {
    let artifact = traversal_benchmark_artifact();
    let tempdir = tempfile::tempdir().expect("tempdir");
    let parquet_dir = write_artifact_parquet(&artifact, tempdir.path(), WriteOptions::default())
        .expect("write parquet artifact");
    let source_sid = "source";
    let in_memory = InMemoryClient::new(Arc::new(artifact));
    let parquet = ParquetClient::open(&parquet_dir).expect("open parquet client");
    let mut group = c.benchmark_group("bench_find_callee_edges_parquet_vs_inmemory");

    group.bench_function("inmemory", |b| {
        b.iter(|| {
            let records = in_memory.find_callee_edges(black_box(source_sid));
            black_box(records);
        })
    });
    group.bench_function("parquet", |b| {
        b.iter(|| {
            let records = parquet.find_callee_edges(black_box(source_sid));
            black_box(records);
        })
    });
    group.finish();
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
    let (facts, _counts) = build_facts(&repo_root).unwrap_or_else(|err| {
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
            graph_index_version: "bench".to_string(),
            content_hash_blake3: None,
        },
        manifest_version: "bench".to_string(),
        graph_content_hash: "traversal-benchmark".to_string(),
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

fn symbol(id: &str, file_path: &str, entity_name: &str) -> GraphSymbolArtifact {
    GraphSymbolArtifact {
        stable_symbol_id: id.to_string(),
        file_path: file_path.to_string(),
        byte_range: [0, 8],
        line_range: [1, 2],
        entity_name: entity_name.to_string(),
        qualified_name: entity_name.to_string(),
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

#[allow(dead_code)]
struct Fixture {
    artifact: GraphIndexArtifact,
    fixture_path: PathBuf,
}

#[allow(dead_code)]
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
    bench_find_callee_edges_parquet_vs_inmemory
);
criterion_main!(benches);

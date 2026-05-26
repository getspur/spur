use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use serde::Deserialize;
use spur_graph::store::parquet::read_artifact_parquet_slim;
use spur_graph::{
    artifact_from_facts, build_facts, read_artifact_parquet, write_artifact_parquet,
    GraphIndexArtifact, GraphQueryClient, InMemoryClient, ParquetClient, SearchFilters, SearchMode,
    SearchOptions, WriteOptions,
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
    bench_search_symbols_parquet_vs_inmemory
);
criterion_main!(benches);

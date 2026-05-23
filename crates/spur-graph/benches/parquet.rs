use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use serde::Deserialize;
use spur_graph::{
    artifact_from_facts, build_facts, read_artifact_parquet, write_artifact_parquet,
    GraphIndexArtifact, WriteOptions,
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
    bench_read_artifact_parquet
);
criterion_main!(benches);

use std::cmp::Ordering;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use spur_graph::{
    artifact_from_facts, artifact_from_facts_incremental, build_facts, read_artifact_parquet,
    write_artifact_parquet, GraphEdgeKind, GraphIndexArtifact, WriteOptions,
};
use tempfile::TempDir;

const SAMPLE_COUNT: usize = 10;
const WARMUP_COUNT: usize = 3;
const POC_DUCKDB_COLD_QUERY_MS: u128 = 23;
const DUCKDB_PEAK_RSS_LIMIT_KB: u64 = 500 * 1024;
const HELPER_ENV: &str = "SPUR_GRAPH_PERF_HELPER";
const HELPER_PARQUET_DIR_ENV: &str = "SPUR_GRAPH_PERF_PARQUET_DIR";
const HELPER_SAMPLE_PREFIX: &str = "SPUR_GRAPH_PERF_SAMPLE ";

static PERF_GATE_LOCK: Mutex<()> = Mutex::new(());
static PERF_FIXTURE: OnceLock<PerfFixture> = OnceLock::new();

#[derive(Debug, Deserialize)]
struct Baselines {
    load_artifact_ms_median: f64,
    load_artifact_rss_kb_median: u64,
    write_artifact_ms_median: f64,
    incremental_build_ms_median: Option<f64>,
    fixture_path: String,
}

#[derive(Debug, Clone)]
struct PerfFixture {
    artifact: GraphIndexArtifact,
    repo_root: PathBuf,
}

#[derive(Debug, Clone)]
struct HelperSample {
    elapsed_ms: f64,
    peak_rss_kb: u64,
}

#[test]
#[cfg_attr(not(feature = "perf-gates"), ignore)]
fn gate_3_1_write_artifact_parquet_under_2x_json_baseline() {
    let _guard = perf_gate_guard();
    if skip_unoptimized_profile() {
        return;
    }
    let baselines = baselines();
    let fixture = perf_fixture(&baselines);
    let tempdir = tempfile::tempdir().expect("tempdir");

    for _ in 0..WARMUP_COUNT {
        let dir =
            write_artifact_parquet(&fixture.artifact, tempdir.path(), WriteOptions::default())
                .expect("warm up parquet writer");
        std::hint::black_box(dir);
    }
    let median_ms = median_f64(samples(SAMPLE_COUNT, || {
        let started = Instant::now();
        let dir =
            write_artifact_parquet(&fixture.artifact, tempdir.path(), WriteOptions::default())
                .expect("write parquet artifact");
        std::hint::black_box(dir);
        duration_ms(started.elapsed())
    }));
    let threshold_ms = baselines.write_artifact_ms_median * 2.0;

    assert!(
        median_ms <= threshold_ms,
        "Gate 3.1 FAILED: write_artifact_parquet median {median_ms:.3} ms exceeds 2.0x baseline write_artifact threshold {threshold_ms:.3} ms (baseline {baseline:.3} ms)",
        baseline = baselines.write_artifact_ms_median,
    );
}

#[test]
#[cfg_attr(not(feature = "perf-gates"), ignore)]
fn gate_3_2_read_artifact_parquet_under_half_json_baseline() {
    let _guard = perf_gate_guard();
    if skip_unoptimized_profile() {
        return;
    }
    let baselines = baselines();
    let fixture = perf_fixture(&baselines);
    let parquet_dir = write_fixture_parquet(&fixture.artifact);

    for _ in 0..WARMUP_COUNT {
        let artifact = read_artifact_parquet(&parquet_dir).expect("warm up parquet reader");
        std::hint::black_box(artifact);
    }
    let median_ms = median_f64(samples(SAMPLE_COUNT, || {
        let started = Instant::now();
        let artifact = read_artifact_parquet(&parquet_dir).expect("read parquet artifact");
        std::hint::black_box(artifact);
        duration_ms(started.elapsed())
    }));
    let threshold_ms = baselines.load_artifact_ms_median * 0.5;

    assert!(
        median_ms <= threshold_ms,
        "Gate 3.2 FAILED: read_artifact_parquet median {median_ms:.3} ms exceeds 0.5x baseline load_artifact threshold {threshold_ms:.3} ms (baseline {baseline:.3} ms)",
        baseline = baselines.load_artifact_ms_median,
    );
}

#[test]
#[cfg_attr(not(feature = "perf-gates"), ignore)]
fn gate_3_3_read_artifact_parquet_peak_rss_no_higher_than_json_baseline() {
    let _guard = perf_gate_guard();
    if skip_unoptimized_profile() {
        return;
    }
    let baselines = baselines();
    let fixture = perf_fixture(&baselines);
    let parquet_dir = write_fixture_parquet(&fixture.artifact);

    let median_peak_rss_kb = median_u64(
        helper_samples("read-parquet-rss", &parquet_dir)
            .into_iter()
            .map(|sample| sample.peak_rss_kb)
            .collect(),
    );
    let threshold_kb = baselines.load_artifact_rss_kb_median;

    assert!(
        median_peak_rss_kb <= threshold_kb,
        "Gate 3.3 FAILED: read_artifact_parquet median peak RSS {median_peak_rss_kb} KB exceeds baseline load_artifact peak RSS {threshold_kb} KB",
    );
}

#[test]
#[cfg_attr(not(feature = "perf-gates"), ignore)]
fn gate_3_4_full_incremental_build_under_80_percent_json_baseline() {
    let _guard = perf_gate_guard();
    if skip_unoptimized_profile() {
        return;
    }
    let baselines = baselines();
    let fixture = perf_fixture(&baselines);
    let prev_dir = write_fixture_parquet(&fixture.artifact);
    let output_dir = tempfile::tempdir().expect("tempdir");

    for _ in 0..WARMUP_COUNT {
        std::hint::black_box(sample_parquet_incremental_build_ms(
            &prev_dir,
            output_dir.path(),
            &fixture.repo_root,
        ));
    }
    let median_ms = median_f64(samples(SAMPLE_COUNT, || {
        sample_parquet_incremental_build_ms(&prev_dir, output_dir.path(), &fixture.repo_root)
    }));
    let baseline_ms = baselines
        .incremental_build_ms_median
        .expect("baselines.json should include pre-PR3b incremental_build_ms_median");
    let threshold_ms = baseline_ms * 0.8;

    assert!(
        median_ms <= threshold_ms,
        "Gate 3.4 FAILED: full incremental build median {median_ms:.3} ms exceeds 0.8x baseline incremental threshold {threshold_ms:.3} ms (baseline {baseline_ms:.3} ms)",
    );
}

#[test]
#[cfg_attr(not(feature = "perf-gates"), ignore)]
fn gate_3_5_duckdb_cold_first_query_under_poc_threshold() {
    let _guard = perf_gate_guard();
    if skip_unoptimized_profile() {
        return;
    }
    let baselines = baselines();
    let fixture = perf_fixture(&baselines);
    let parquet_dir = write_fixture_parquet(&fixture.artifact);

    let median_ms = median_f64(
        helper_samples("duckdb-query", &parquet_dir)
            .into_iter()
            .map(|sample| sample.elapsed_ms)
            .collect(),
    );
    let threshold_ms = (POC_DUCKDB_COLD_QUERY_MS as f64) * 1.5;

    assert!(
        median_ms <= threshold_ms,
        "Gate 3.5 FAILED: DuckDB cold first-query median {median_ms:.3} ms exceeds 1.5x POC threshold {threshold_ms:.3} ms (POC median {POC_DUCKDB_COLD_QUERY_MS} ms)",
    );
}

#[test]
#[cfg_attr(not(feature = "perf-gates"), ignore)]
fn gate_3_6_duckdb_peak_rss_under_500_mb() {
    let _guard = perf_gate_guard();
    if skip_unoptimized_profile() {
        return;
    }
    let baselines = baselines();
    let fixture = perf_fixture(&baselines);
    let parquet_dir = write_fixture_parquet(&fixture.artifact);

    let median_peak_rss_kb = median_u64(
        helper_samples("duckdb-query", &parquet_dir)
            .into_iter()
            .map(|sample| sample.peak_rss_kb)
            .collect(),
    );

    assert!(
        median_peak_rss_kb <= DUCKDB_PEAK_RSS_LIMIT_KB,
        "Gate 3.6 FAILED: DuckDB median peak RSS {median_peak_rss_kb} KB exceeds 500 MB threshold {DUCKDB_PEAK_RSS_LIMIT_KB} KB",
    );
}

#[test]
#[cfg_attr(not(feature = "perf-gates"), ignore)]
fn gate_t5_git_path_as_ref_inbound_calls_under_20() {
    let _guard = perf_gate_guard();
    let repo_root = workspace_root();
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
    let target = artifact
        .symbols
        .iter()
        .find(|symbol| symbol.qualified_name == "impl AsRef<[u8]> for GitPath::as_ref")
        .expect("GitPath AsRef::as_ref symbol");
    let inbound_calls = artifact
        .edges
        .iter()
        .filter(|edge| {
            edge.edge_kind == Some(GraphEdgeKind::Calls)
                && edge.target_stable_symbol_id.as_deref() == Some(target.stable_symbol_id.as_str())
        })
        .count();
    let call_edges = artifact
        .edges
        .iter()
        .filter(|edge| {
            edge.edge_kind == Some(GraphEdgeKind::Calls) && edge.target_stable_symbol_id.is_some()
        })
        .count();

    eprintln!(
        "SPUR_GRAPH_PHANTOM_GATE resolved_call_edges={call_edges} git_path_as_ref_inbound={inbound_calls}"
    );
    assert!(
        inbound_calls <= 20,
        "GitPath::as_ref inbound calls {inbound_calls} should stay <= 20"
    );
}

#[test]
#[ignore]
fn perf_helper_sample() {
    let Some(mode) = env::var_os(HELPER_ENV) else {
        return;
    };
    let parquet_dir = env::var_os(HELPER_PARQUET_DIR_ENV)
        .map(PathBuf::from)
        .expect("helper parquet dir env");
    let sample = match mode.to_string_lossy().as_ref() {
        "read-parquet-rss" => helper_read_parquet_rss(&parquet_dir),
        "duckdb-query" => helper_duckdb_query(&parquet_dir),
        other => panic!("unknown perf helper mode `{other}`"),
    };
    println!(
        "{HELPER_SAMPLE_PREFIX}elapsed_ms={:.6} peak_rss_kb={}",
        sample.elapsed_ms, sample.peak_rss_kb
    );
}

fn baselines() -> Baselines {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("benches/baselines.json");
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read `{}`: {err}", path.display()));
    serde_json::from_str(&content)
        .unwrap_or_else(|err| panic!("failed to parse `{}`: {err}", path.display()))
}

fn perf_gate_guard() -> MutexGuard<'static, ()> {
    PERF_GATE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn skip_unoptimized_profile() -> bool {
    if cfg!(debug_assertions) {
        eprintln!("perf-gates assertions require an optimized test profile; skipping debug-profile timing check");
        true
    } else {
        false
    }
}

fn perf_fixture(baselines: &Baselines) -> &'static PerfFixture {
    PERF_FIXTURE.get_or_init(|| {
        let fixture_path = env::var_os("SPUR_GRAPH_PERF_FIXTURE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&baselines.fixture_path));
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
        PerfFixture {
            artifact,
            repo_root,
        }
    })
}

fn write_fixture_parquet(artifact: &GraphIndexArtifact) -> PathBuf {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let dir = write_artifact_parquet(artifact, tempdir.path(), WriteOptions::default())
        .expect("write parquet artifact");
    persist_tempdir_path(tempdir, dir)
}

fn sample_parquet_incremental_build_ms(
    prev_dir: &Path,
    output_dir: &Path,
    repo_root: &Path,
) -> f64 {
    let started = Instant::now();
    let prev = read_artifact_parquet(prev_dir).expect("read previous parquet artifact");
    let (next, mode) =
        artifact_from_facts_incremental(&prev, repo_root).expect("full incremental build");
    std::hint::black_box(mode);
    let written = write_artifact_parquet(&next, output_dir, WriteOptions::default())
        .expect("write incremental parquet artifact");
    std::hint::black_box(written);
    duration_ms(started.elapsed())
}

fn persist_tempdir_path(tempdir: TempDir, dir: PathBuf) -> PathBuf {
    let root = tempdir.keep();
    assert!(
        dir.starts_with(&root),
        "parquet dir `{}` should be inside tempdir `{}`",
        dir.display(),
        root.display()
    );
    dir
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .expect("crate should live under <workspace>/crates/spur-graph")
}

fn helper_samples(mode: &str, parquet_dir: &Path) -> Vec<HelperSample> {
    samples(SAMPLE_COUNT, || run_helper_sample(mode, parquet_dir))
}

fn run_helper_sample(mode: &str, parquet_dir: &Path) -> HelperSample {
    let current_exe = env::current_exe().expect("current test executable");
    let output = Command::new(current_exe)
        .args(["--ignored", "--exact", "perf_helper_sample", "--nocapture"])
        .env(HELPER_ENV, mode)
        .env(HELPER_PARQUET_DIR_ENV, parquet_dir)
        .output()
        .expect("spawn perf helper sample");

    if !output.status.success() {
        panic!(
            "perf helper `{mode}` failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8(output.stdout).expect("helper stdout UTF-8");
    stdout
        .lines()
        .find_map(parse_helper_sample)
        .unwrap_or_else(|| panic!("perf helper `{mode}` did not print a sample line:\n{stdout}"))
}

fn parse_helper_sample(line: &str) -> Option<HelperSample> {
    let rest = line.strip_prefix(HELPER_SAMPLE_PREFIX)?;
    let mut elapsed_ms = None;
    let mut peak_rss_kb = None;
    for field in rest.split_whitespace() {
        let (key, value) = field.split_once('=')?;
        match key {
            "elapsed_ms" => elapsed_ms = Some(value.parse().ok()?),
            "peak_rss_kb" => peak_rss_kb = Some(value.parse().ok()?),
            _ => {}
        }
    }
    Some(HelperSample {
        elapsed_ms: elapsed_ms?,
        peak_rss_kb: peak_rss_kb?,
    })
}

fn helper_read_parquet_rss(parquet_dir: &Path) -> HelperSample {
    let started = Instant::now();
    let artifact = read_artifact_parquet(parquet_dir)
        .unwrap_or_else(|err| panic!("failed to read `{}`: {err:#}", parquet_dir.display()));
    std::hint::black_box(artifact);
    HelperSample {
        elapsed_ms: duration_ms(started.elapsed()),
        peak_rss_kb: peak_rss_kb(libc::RUSAGE_SELF),
    }
}

fn helper_duckdb_query(parquet_dir: &Path) -> HelperSample {
    let sql = duckdb_count_query(parquet_dir);
    let started = Instant::now();
    let output = Command::new(env::var_os("DUCKDB_BIN").unwrap_or_else(|| "duckdb".into()))
        .args(["-c", &sql])
        .output()
        .expect("spawn duckdb");
    let elapsed_ms = duration_ms(started.elapsed());

    if !output.status.success() {
        panic!(
            "duckdb query failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    HelperSample {
        elapsed_ms,
        peak_rss_kb: peak_rss_kb(libc::RUSAGE_CHILDREN),
    }
}

fn duckdb_count_query(parquet_dir: &Path) -> String {
    let table = |name: &str| sql_string_literal(&parquet_dir.join(name));
    format!(
        "SELECT \
         (SELECT COUNT(*) FROM read_parquet({nodes})) + \
         (SELECT COUNT(*) FROM read_parquet({edges})) + \
         (SELECT COUNT(*) FROM read_parquet({edges_unresolved})) + \
         (SELECT COUNT(*) FROM read_parquet({files})) + \
         (SELECT COUNT(*) FROM read_parquet({file_manifests})) + \
         (SELECT COUNT(*) FROM read_parquet({tombstones})) AS rows_scanned;",
        nodes = table("nodes.parquet"),
        edges = table("edges.parquet"),
        edges_unresolved = table("edges_unresolved.parquet"),
        files = table("files.parquet"),
        file_manifests = table("file_manifests.parquet"),
        tombstones = table("tombstones.parquet"),
    )
}

fn sql_string_literal(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

fn samples<T>(count: usize, mut sample: impl FnMut() -> T) -> Vec<T> {
    (0..count).map(|_| sample()).collect()
}

fn median_f64(mut values: Vec<f64>) -> f64 {
    assert!(!values.is_empty(), "median requires at least one sample");
    values.sort_by(|left, right| left.partial_cmp(right).unwrap_or(Ordering::Equal));
    values[values.len() / 2]
}

fn median_u64(mut values: Vec<u64>) -> u64 {
    assert!(!values.is_empty(), "median requires at least one sample");
    values.sort_unstable();
    values[values.len() / 2]
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn peak_rss_kb(who: libc::c_int) -> u64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    let status = unsafe { libc::getrusage(who, usage.as_mut_ptr()) };
    assert_eq!(status, 0, "getrusage failed for who={who}");
    let usage = unsafe { usage.assume_init() };
    let raw = usage.ru_maxrss;
    assert!(raw >= 0, "getrusage returned negative ru_maxrss {raw}");
    normalize_ru_maxrss_to_kb(raw as u64)
}

#[cfg(target_os = "macos")]
fn normalize_ru_maxrss_to_kb(raw: u64) -> u64 {
    raw.div_ceil(1024)
}

#[cfg(not(target_os = "macos"))]
fn normalize_ru_maxrss_to_kb(raw: u64) -> u64 {
    raw
}

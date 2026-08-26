#![allow(unsafe_code)] // libc::getrusage + MaybeUninit::assume_init for perf gate RSS metrics.

use std::cmp::Ordering;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use spur_graph::{
    artifact_from_facts, artifact_from_facts_incremental, build_facts, read_artifact_parquet,
    write_artifact_parquet, GraphEdgeKind, GraphIndexArtifact, RelationKind, WriteOptions,
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
        let dir = write_artifact_parquet(
            &fixture.artifact,
            tempdir.path(),
            WriteOptions::default(),
            Vec::new(),
        )
        .expect("warm up parquet writer");
        std::hint::black_box(dir);
    }
    let median_ms = median_f64(samples(SAMPLE_COUNT, || {
        let started = Instant::now();
        let dir = write_artifact_parquet(
            &fixture.artifact,
            tempdir.path(),
            WriteOptions::default(),
            Vec::new(),
        )
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
#[cfg_attr(not(feature = "perf-gates"), ignore)]
fn gate_import_licensed_edges_are_witness_backed_on_spur_graph() {
    let _guard = perf_gate_guard();
    let repo_root = precision_root();
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

    let report = import_licensed_precision_report(&artifact);
    eprintln!(
        "SPUR_GRAPH_IMPORT_LICENSED_GATE import_licensed_edges={} non_call={} calls_dyn={} missing_target={} non_function_target={} cross_language={} missing_witness={} all_resolved_call_cross_language={} all_resolved_call_cross_language_examples={:?}",
        report.import_licensed_edges,
        report.non_call,
        report.calls_dyn,
        report.missing_target,
        report.non_function_target,
        report.cross_language,
        report.missing_witness,
        report.all_resolved_call_cross_language,
        report.all_resolved_call_cross_language_examples
    );

    assert_eq!(report.non_call, 0, "import_licensed must only stamp calls");
    assert_eq!(
        report.calls_dyn, 0,
        "import_licensed must never stamp CallsDyn edges"
    );
    assert_eq!(
        report.missing_target, 0,
        "import_licensed calls must resolve to a workspace target"
    );
    assert_eq!(
        report.non_function_target, 0,
        "import_licensed targets must be functions"
    );
    assert_eq!(
        report.cross_language, 0,
        "import_licensed calls must stay within one language family"
    );
    assert_eq!(
        report.missing_witness, 0,
        "import_licensed calls must have a same-file workspace import witness"
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
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| {
                panic!(
                    "fixture path `{}` is expected to live under <repo>/.spur/graph/<artifact>",
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
        PerfFixture {
            artifact,
            repo_root,
        }
    })
}

fn write_fixture_parquet(artifact: &GraphIndexArtifact) -> PathBuf {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let dir = write_artifact_parquet(
        artifact,
        tempdir.path(),
        WriteOptions::default(),
        Vec::new(),
    )
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
    let (next, mode, _stats) =
        artifact_from_facts_incremental(&prev, repo_root).expect("full incremental build");
    std::hint::black_box(mode);
    let written = write_artifact_parquet(&next, output_dir, WriteOptions::default(), Vec::new())
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

fn precision_root() -> PathBuf {
    env::var_os("SPUR_GRAPH_PRECISION_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(workspace_root)
}

#[derive(Default)]
struct ImportLicensedPrecisionReport {
    import_licensed_edges: usize,
    non_call: usize,
    calls_dyn: usize,
    missing_target: usize,
    non_function_target: usize,
    cross_language: usize,
    missing_witness: usize,
    all_resolved_call_cross_language: usize,
    all_resolved_call_cross_language_examples: Vec<String>,
}

fn import_licensed_precision_report(
    artifact: &GraphIndexArtifact,
) -> ImportLicensedPrecisionReport {
    let symbols_by_id = artifact
        .symbols
        .iter()
        .map(|symbol| (symbol.stable_symbol_id.as_str(), symbol))
        .collect::<std::collections::HashMap<_, _>>();
    let files_by_id = artifact
        .files
        .iter()
        .map(|file| (file.stable_file_id.as_str(), file.file_path.as_str()))
        .collect::<std::collections::HashMap<_, _>>();
    let mut report = ImportLicensedPrecisionReport::default();

    for edge in artifact
        .edges
        .iter()
        .filter(|edge| edge.relation == RelationKind::Calls)
        .filter(|edge| edge.edge_kind == Some(GraphEdgeKind::Calls))
        .filter(|edge| edge.target_stable_symbol_id.is_some())
    {
        let Some(source_file) = edge_source_file(
            edge.source_stable_symbol_id.as_str(),
            &symbols_by_id,
            &files_by_id,
        ) else {
            continue;
        };
        let Some(target) = edge
            .target_stable_symbol_id
            .as_deref()
            .and_then(|target_id| symbols_by_id.get(target_id))
        else {
            continue;
        };
        if language_family(source_file)
            .zip(language_family(target.file_path.as_str()))
            .is_some_and(|(source, target)| source != target)
        {
            report.all_resolved_call_cross_language += 1;
            if report.all_resolved_call_cross_language_examples.len() < 8 {
                report
                    .all_resolved_call_cross_language_examples
                    .push(format!(
                        "{}:{} -> {}:{} label={:?} bind={:?}",
                        source_file,
                        edge.source_stable_symbol_id,
                        target.file_path,
                        target.qualified_name,
                        edge.target_label,
                        edge.bind_method
                    ));
            }
        }
    }

    for edge in artifact
        .edges
        .iter()
        .filter(|edge| edge.bind_method.as_deref() == Some("import_licensed"))
    {
        report.import_licensed_edges += 1;
        if edge.relation != RelationKind::Calls {
            report.non_call += 1;
        }
        if edge.edge_kind == Some(GraphEdgeKind::CallsDyn) {
            report.calls_dyn += 1;
        }

        let Some(source_file) = edge_source_file(
            edge.source_stable_symbol_id.as_str(),
            &symbols_by_id,
            &files_by_id,
        ) else {
            report.missing_witness += 1;
            continue;
        };
        let Some(target) = edge
            .target_stable_symbol_id
            .as_deref()
            .and_then(|target_id| symbols_by_id.get(target_id))
        else {
            report.missing_target += 1;
            continue;
        };

        if target.symbol_kind != "function" {
            report.non_function_target += 1;
        }
        if language_family(source_file)
            .zip(language_family(target.file_path.as_str()))
            .is_some_and(|(source, target)| source != target)
        {
            report.cross_language += 1;
        }
        if !has_import_license_witness(artifact, &symbols_by_id, &files_by_id, source_file, target)
        {
            report.missing_witness += 1;
        }
    }

    report
}

fn has_import_license_witness(
    artifact: &GraphIndexArtifact,
    symbols_by_id: &std::collections::HashMap<&str, &spur_graph::GraphSymbolArtifact>,
    files_by_id: &std::collections::HashMap<&str, &str>,
    source_file: &str,
    target: &spur_graph::GraphSymbolArtifact,
) -> bool {
    artifact.edges.iter().any(|edge| {
        edge.relation == RelationKind::Imports
            && edge.bind_method.as_deref() != Some("external")
            && edge.target_stable_symbol_id.as_deref() == Some(target.stable_symbol_id.as_str())
            && edge.target_label.as_deref() == Some(target.entity_name.as_str())
            && edge_source_file(
                edge.source_stable_symbol_id.as_str(),
                symbols_by_id,
                files_by_id,
            ) == Some(source_file)
    })
}

fn edge_source_file<'a>(
    source_stable_id: &str,
    symbols_by_id: &std::collections::HashMap<&str, &'a spur_graph::GraphSymbolArtifact>,
    files_by_id: &'a std::collections::HashMap<&str, &'a str>,
) -> Option<&'a str> {
    symbols_by_id
        .get(source_stable_id)
        .map(|symbol| symbol.file_path.as_str())
        .or_else(|| files_by_id.get(source_stable_id).copied())
}

fn language_family(path: &str) -> Option<&'static str> {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase())
        .as_deref()
    {
        Some("rs") => Some("rust"),
        Some("ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs") => Some("javascript"),
        Some("py" | "pyi") => Some("python"),
        Some("cpp" | "cc" | "cxx" | "c" | "h" | "hpp" | "hxx") => Some("cpp"),
        _ => None,
    }
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

    assert!(
        output.status.success(),
        "perf helper `{mode}` failed with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

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

    assert!(
        output.status.success(),
        "duckdb query failed with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

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
    // SAFETY: `usage` points to writable storage for `libc::rusage`, and
    // `getrusage` initializes it when returning 0.
    let status = unsafe { libc::getrusage(who, usage.as_mut_ptr()) };
    assert_eq!(status, 0, "getrusage failed for who={who}");
    // SAFETY: the assertion above guarantees `getrusage` initialized `usage`.
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

#[test]
fn gate_task6_overlay_generation_matrix_requires_structural_release_evidence() {
    let complete_matrix = serde_json::json!({
        "schema_version": 1,
        "protocol_id": "overlay-generation-task6-v1",
        "repetitions": 3,
        "cold_warm_separated": true,
        "timing_gate": "structural_only_no_fixed_millisecond_threshold",
        "release_eligible": true,
        "fsmonitor_auto_safe": true,
        "configuration_default": "Off",
        "configure_semantics_changed": false,
        "cells": [
            task6_release_cell("small_untracked_heavy", "gen_small"),
            task6_release_cell("medium_dirty_rust", "gen_medium"),
            task6_release_cell("large_mostly_clean_polyglot", "gen_large"),
        ],
    });

    validate_task6_overlay_generation_matrix(&complete_matrix)
        .expect("complete parity and structural evidence must pass without a wall-clock threshold");
}

#[test]
fn gate_task6_overlay_generation_matrix_rejects_missing_or_correlated_evidence() {
    let mut incomplete = task6_release_cell("small_untracked_heavy", "gen_small");
    incomplete["warm_generation"]["finalization"]["result_merges"] = serde_json::json!(1);
    incomplete["exact_fallback"]["digest"] = serde_json::json!("different");
    let matrix = serde_json::json!({
        "schema_version": 1,
        "protocol_id": "overlay-generation-task6-v1",
        "repetitions": 3,
        "cells": [incomplete],
    });

    let errors = validate_task6_overlay_generation_matrix(&matrix)
        .expect_err("incomplete, mismatched evidence must fail closed");
    assert!(errors.iter().any(|error| error.contains("three projects")));
    assert!(errors
        .iter()
        .any(|error| error.contains("warm finalization")));
    assert!(errors.iter().any(|error| error.contains("fallback digest")));
}

#[test]
fn gate_task6_fsmonitor_default_remains_off() {
    assert!(
        !spur_graph::GraphMcpDeps::default().overlay_fsmonitor_auto,
        "Task 6 may recommend Auto from evidence but must not alter configuration defaults"
    );
}

#[test]
fn gate_task6_emitted_matrix_when_evidence_path_is_set() {
    let Some(path) = std::env::var_os("SPUR_GRAPH_TASK6_EVIDENCE") else {
        eprintln!("SPUR_GRAPH_TASK6_EVIDENCE is unset; deterministic contract tests remain active");
        return;
    };
    let bytes = fs::read(&path).unwrap_or_else(|error| {
        panic!(
            "read SPUR_GRAPH_TASK6_EVIDENCE `{}`: {error}",
            PathBuf::from(&path).display()
        )
    });
    let matrix = serde_json::from_slice::<serde_json::Value>(&bytes)
        .expect("parse SPUR_GRAPH_TASK6_EVIDENCE JSON");
    validate_task6_overlay_generation_matrix(&matrix).unwrap_or_else(|errors| {
        panic!("Task 6 emitted matrix failed structural release gates: {errors:#?}")
    });
}

fn validate_task6_overlay_generation_matrix(matrix: &serde_json::Value) -> Result<(), Vec<String>> {
    const PROTOCOL: &str = "overlay-generation-task6-v1";
    const LATENCY_CASES: [&str; 7] = [
        "direct_parquet",
        "exact_overlay_oracle",
        "cold_generation_build",
        "warm_generation_reuse",
        "bounded_incremental_update",
        "exact_fallback",
        "full_end_to_end_mcp",
    ];
    const PHASE_CASES: [&str; 9] = [
        "backend_open",
        "backend_full_base_read",
        "freshness_git_validation",
        "generation_lookup_build_cold",
        "query_execution_warm_generation",
        "response_file_metadata_analysis",
        "response_construction_serialization",
        "overlay_finalization_exact_oracle",
        "full_end_to_end_code_request",
    ];

    let mut errors = Vec::new();
    let protocol = matrix["protocol_id"].as_str().unwrap_or_default();
    if matrix["schema_version"].as_u64() != Some(1) {
        errors.push("matrix schema_version must be 1".to_owned());
    }
    if protocol != PROTOCOL {
        errors.push(format!("matrix protocol_id must be {PROTOCOL}"));
    }
    if matrix["cold_warm_separated"].as_bool() != Some(true) {
        errors.push("matrix must separate cold and warm cases".to_owned());
    }
    if matrix["timing_gate"].as_str() != Some("structural_only_no_fixed_millisecond_threshold") {
        errors.push("matrix must use the structural-only timing gate".to_owned());
    }
    if matrix["release_eligible"].as_bool() != Some(true)
        || matrix["fsmonitor_auto_safe"].as_bool() != Some(true)
    {
        errors.push("matrix release and fsmonitor Auto verdicts must pass".to_owned());
    }
    if matrix["configuration_default"].as_str() != Some("Off")
        || matrix["configure_semantics_changed"].as_bool() != Some(false)
    {
        errors.push("matrix must preserve default-Off configuration semantics".to_owned());
    }
    let repetitions = matrix["repetitions"].as_u64().unwrap_or_default();
    if repetitions < 3 {
        errors.push("matrix requires at least three measured repetitions".to_owned());
    }
    let cells = matrix["cells"].as_array().cloned().unwrap_or_default();
    let projects = cells
        .iter()
        .filter_map(|cell| cell["project"].as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if projects.len() < 3 {
        errors.push("matrix must contain three projects with distinct identities".to_owned());
    }

    for cell in &cells {
        let project = cell["project"].as_str().unwrap_or("<missing-project>");
        let prefix = |message: &str| format!("{project}: {message}");
        if cell["protocol_id"].as_str() != Some(protocol) {
            errors.push(prefix("protocol differs from the matrix protocol"));
        }
        if cell["repetitions"].as_u64() != Some(repetitions) {
            errors.push(prefix("repetition count differs from the matrix protocol"));
        }
        if cell["mismatch_count"].as_u64() != Some(0) {
            errors.push(prefix("mismatch_count must be zero"));
        }
        for (label, count) in [
            (
                "direct Parquet",
                cell["direct_parquet"]["query_operation_count"].as_u64(),
            ),
            (
                "exact oracle",
                cell["exact_overlay_oracle"]["query_operation_count"].as_u64(),
            ),
            (
                "exact fallback",
                cell["exact_fallback"]["query_operation_count"].as_u64(),
            ),
        ] {
            if count.unwrap_or_default() < repetitions {
                errors.push(prefix(&format!(
                    "{label} query operation count is incomplete"
                )));
            }
        }

        let oracle_digest = cell["oracle_digest"].as_str().unwrap_or_default();
        if oracle_digest.is_empty() {
            errors.push(prefix("exact oracle digest is missing"));
        }
        for (label, digest) in [
            ("direct Parquet", cell["direct_parquet"]["digest"].as_str()),
            (
                "exact oracle",
                cell["exact_overlay_oracle"]["digest"].as_str(),
            ),
            (
                "cold generation",
                cell["cold_generation"]["digest"].as_str(),
            ),
            (
                "incremental generation",
                cell["incremental_update"]["digest"].as_str(),
            ),
            ("fallback", cell["exact_fallback"]["digest"].as_str()),
            ("full MCP", cell["full_mcp_request"]["digest"].as_str()),
        ] {
            if digest != Some(oracle_digest) {
                errors.push(prefix(&format!(
                    "{label} digest must match the exact oracle"
                )));
            }
        }
        let warm_digests = cell["warm_generation"]["digests"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if warm_digests.len() < repetitions as usize
            || warm_digests
                .iter()
                .any(|digest| digest.as_str() != Some(oracle_digest))
        {
            errors.push(prefix(
                "every warm generation digest must match the exact oracle",
            ));
        }

        let cold_id = cell["cold_generation"]["generation_id"]
            .as_str()
            .unwrap_or_default();
        if !cold_id.starts_with("gen_") {
            errors.push(prefix(
                "cold generation identity must be opaque and present",
            ));
        }
        if cell["cold_generation"]["generation_build_count"].as_u64() != Some(1) {
            errors.push(prefix("cold generation must build exactly once"));
        }
        if cell["cold_generation"]["full_base_load_count"].as_u64() != Some(1) {
            errors.push(prefix(
                "cold generation must load the full base exactly once",
            ));
        }

        let warm_ids = cell["warm_generation"]["generation_ids"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if warm_ids.len() < repetitions as usize
            || warm_ids.iter().any(|id| id.as_str() != Some(cold_id))
        {
            errors.push(prefix(
                "warm no-change requests must reuse the cold generation identity",
            ));
        }
        if cell["warm_generation"]["generation_build_count"].as_u64() != Some(0) {
            errors.push(prefix("warm no-change requests must perform zero builds"));
        }
        if cell["warm_generation"]["full_base_load_count"].as_u64() != Some(0) {
            errors.push(prefix(
                "warm no-change requests must perform zero full-base loads",
            ));
        }
        if cell["warm_generation"]["query_operation_count"]
            .as_u64()
            .unwrap_or_default()
            < repetitions
        {
            errors.push(prefix("warm query operation count is incomplete"));
        }
        let finalization = &cell["warm_generation"]["finalization"];
        for stage in [
            "shadow_filters",
            "result_merges",
            "overlay_sorts",
            "stable_id_deduplications",
        ] {
            if finalization[stage].as_u64() != Some(0) {
                errors.push(prefix(&format!(
                    "warm finalization stage {stage} must be zero"
                )));
            }
        }

        let previous_id = cell["incremental_update"]["previous_generation_id"]
            .as_str()
            .unwrap_or_default();
        let incremental_id = cell["incremental_update"]["generation_id"]
            .as_str()
            .unwrap_or_default();
        if previous_id != cold_id || incremental_id.is_empty() || incremental_id == cold_id {
            errors.push(prefix(
                "bounded update must advance from the measured cold generation identity",
            ));
        }
        if cell["incremental_update"]["full_base_load_count"].as_u64() != Some(0) {
            errors.push(prefix(
                "bounded incremental update must reuse the full base",
            ));
        }
        let changed = string_set(&cell["incremental_update"]["changed_paths"]);
        let rebuilt = string_set(&cell["incremental_update"]["rebuilt_paths"]);
        let closure = string_set(&cell["incremental_update"]["dependency_closure_paths"]);
        if changed.is_empty() || rebuilt.is_empty() || !rebuilt.is_subset(&changed) {
            errors.push(prefix(
                "bounded update rebuilt paths must be non-empty and contained in changed paths",
            ));
        }
        if !rebuilt.is_subset(&closure) {
            errors.push(prefix(
                "rebuilt paths must be contained in the reported dependency closure",
            ));
        }
        if cell["incremental_update"]["changed_segment_count"].as_u64()
            != Some(rebuilt.len() as u64)
            || cell["incremental_update"]["dependency_closure_path_count"].as_u64()
                != Some(closure.len() as u64)
            || cell["incremental_update"]["query_operation_count"]
                .as_u64()
                .unwrap_or_default()
                == 0
        {
            errors.push(prefix(
                "bounded update segment, closure, and query counts must be complete",
            ));
        }

        if cell["exact_fallback"]["route"].as_str() != Some("request_scoped_exact_overlay")
            || cell["exact_fallback"]["reason"]
                .as_str()
                .is_none_or(str::is_empty)
        {
            errors.push(prefix("exact fallback route or reason is missing"));
        }
        if cell["full_mcp_request"]["query_operation_count"]
            .as_u64()
            .unwrap_or_default()
            == 0
        {
            errors.push(prefix("full MCP request must execute a query operation"));
        }
        if cell["full_mcp_request"]["generation_identity_mismatch_count"].as_u64() != Some(0) {
            errors.push(prefix(
                "full MCP request must report zero generation identity mismatches",
            ));
        }
        if cell["full_mcp_request"]["validation_observations_per_request"].as_u64() != Some(2) {
            errors.push(prefix(
                "full MCP request must perform exactly two validation observations",
            ));
        }
        if cell["full_mcp_request"]["response_metadata_scans_per_request"].as_u64() != Some(0) {
            errors.push(prefix(
                "full MCP request must perform zero response metadata scans",
            ));
        }

        for latency_case in LATENCY_CASES {
            let summary = &cell["latency"][latency_case];
            let p50 = summary["p50_ms"].as_f64();
            let p95 = summary["p95_ms"].as_f64();
            if !matches!((p50, p95), (Some(p50), Some(p95)) if p50 >= 0.0 && p95 >= p50) {
                errors.push(prefix(&format!(
                    "{latency_case} must report ordered non-negative p50/p95"
                )));
            }
            if summary["sample_count"].as_u64() != Some(repetitions) {
                errors.push(prefix(&format!(
                    "{latency_case} must contain every measured repetition"
                )));
            }
        }
        for phase_case in PHASE_CASES {
            let summary = &cell["phase_decomposition"][phase_case];
            let p50 = summary["p50_ms"].as_f64();
            let p95 = summary["p95_ms"].as_f64();
            if summary["sample_count"].as_u64() != Some(repetitions)
                || !matches!((p50, p95), (Some(p50), Some(p95)) if p50 >= 0.0 && p95 >= p50)
            {
                errors.push(prefix(&format!(
                    "phase {phase_case} must report every ordered p50/p95 sample"
                )));
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn string_set(value: &serde_json::Value) -> std::collections::BTreeSet<&str> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect()
}

fn task6_release_cell(project: &str, generation_id: &str) -> serde_json::Value {
    serde_json::json!({
        "project": project,
        "protocol_id": "overlay-generation-task6-v1",
        "repetitions": 3,
        "oracle_digest": "digest_equal",
        "mismatch_count": 0,
        "direct_parquet": {
            "query_operation_count": 3,
            "digest": "digest_equal",
        },
        "exact_overlay_oracle": {
            "query_operation_count": 3,
            "digest": "digest_equal",
            "finalization": {
                "shadow_filters": 3,
                "result_merges": 3,
                "overlay_sorts": 3,
                "stable_id_deduplications": 3,
            },
        },
        "cold_generation": {
            "generation_id": generation_id,
            "generation_build_count": 1,
            "full_base_load_count": 1,
            "digest": "digest_equal",
        },
        "warm_generation": {
            "generation_ids": [generation_id, generation_id, generation_id],
            "digests": ["digest_equal", "digest_equal", "digest_equal"],
            "generation_build_count": 0,
            "full_base_load_count": 0,
            "query_operation_count": 3,
            "finalization": {
                "shadow_filters": 0,
                "result_merges": 0,
                "overlay_sorts": 0,
                "stable_id_deduplications": 0,
            },
        },
        "incremental_update": {
            "previous_generation_id": generation_id,
            "generation_id": format!("{generation_id}_next"),
            "changed_paths": ["src/bounded.rs"],
            "rebuilt_paths": ["src/bounded.rs"],
            "dependency_closure_paths": ["src/bounded.rs"],
            "changed_segment_count": 1,
            "dependency_closure_path_count": 1,
            "full_base_load_count": 0,
            "query_operation_count": 1,
            "digest": "digest_equal",
        },
        "exact_fallback": {
            "route": "request_scoped_exact_overlay",
            "reason": "configuration_off_exact_oracle",
            "query_operation_count": 3,
            "digest": "digest_equal",
        },
        "full_mcp_request": {
            "digest": "digest_equal",
            "query_operation_count": 1,
            "generation_identity_mismatch_count": 0,
        },
        "latency": {
            "direct_parquet": {"sample_count": 3, "p50_ms": 1.0, "p95_ms": 2.0},
            "exact_overlay_oracle": {"sample_count": 3, "p50_ms": 2.0, "p95_ms": 3.0},
            "cold_generation_build": {"sample_count": 3, "p50_ms": 3.0, "p95_ms": 4.0},
            "warm_generation_reuse": {"sample_count": 3, "p50_ms": 1.0, "p95_ms": 2.0},
            "bounded_incremental_update": {"sample_count": 3, "p50_ms": 2.0, "p95_ms": 3.0},
            "exact_fallback": {"sample_count": 3, "p50_ms": 2.0, "p95_ms": 3.0},
            "full_end_to_end_mcp": {"sample_count": 3, "p50_ms": 3.0, "p95_ms": 4.0},
        },
        "phase_decomposition": {
            "backend_open": {"sample_count": 3, "p50_ms": 1.0, "p95_ms": 2.0},
            "backend_full_base_read": {"sample_count": 3, "p50_ms": 1.0, "p95_ms": 2.0},
            "freshness_git_validation": {"sample_count": 3, "p50_ms": 1.0, "p95_ms": 2.0},
            "generation_lookup_build_cold": {"sample_count": 3, "p50_ms": 1.0, "p95_ms": 2.0},
            "query_execution_warm_generation": {"sample_count": 3, "p50_ms": 1.0, "p95_ms": 2.0},
            "response_file_metadata_analysis": {"sample_count": 3, "p50_ms": 1.0, "p95_ms": 2.0},
            "response_construction_serialization": {"sample_count": 3, "p50_ms": 1.0, "p95_ms": 2.0},
            "overlay_finalization_exact_oracle": {"sample_count": 3, "p50_ms": 1.0, "p95_ms": 2.0},
            "full_end_to_end_code_request": {"sample_count": 3, "p50_ms": 1.0, "p95_ms": 2.0},
        },
    })
}

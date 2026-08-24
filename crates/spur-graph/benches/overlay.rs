//! OverlayClient vs direct ParquetClient on an explicit repository/artifact pair.
//!
//! Opens `.spur/graph/CURRENT` (override with `SPUR_GRAPH_PERF_FIXTURE`).
//! Does not rebuild facts — this is a query-path comparison, not an index build.
//!
//! Cross-project inputs are `SPUR_GRAPH_PERF_REPO`, `SPUR_GRAPH_PERF_FIXTURE`,
//! `SPUR_GRAPH_PERF_QUERY`, and `SPUR_GRAPH_PERF_CHANGED_FILE`. Optional
//! `SPUR_GRAPH_PERF_LABEL`, `SPUR_GRAPH_PERF_SAMPLE_SIZE`, and
//! `SPUR_GRAPH_PERF_MEASUREMENT_SECONDS` control evidence naming and finite
//! Criterion bounds. Omitting every variable preserves the Spur defaults.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use spur_graph::{
    GraphQueryClient, OverlayClient, ParquetClient, SearchFilters, SearchMode, SearchOptions,
};

const DEFAULT_QUERY: &str = "handle_code_search";
const DEFAULT_OVERLAY_QUERY: &str = "overlay_client_for_backend";
const DEFAULT_CHANGED_FILE: &str = "crates/spur-graph/src/mcp/mod.rs";
const REQUIRED_WARM_SAMPLES: usize = 30;
const MAX_WARM_SAMPLES: usize = 1_000;
const MAX_MEASUREMENT_SECONDS: u64 = 10;

#[derive(Debug, Clone)]
struct ProbeConfig {
    label: String,
    repo: PathBuf,
    parquet_dir: PathBuf,
    query: String,
    overlay_query: String,
    changed_file: PathBuf,
    sample_size: usize,
    measurement_time: Duration,
}

impl ProbeConfig {
    fn load() -> Self {
        let repo = canonical_dir(
            "SPUR_GRAPH_PERF_REPO",
            std::env::var_os("SPUR_GRAPH_PERF_REPO")
                .map(PathBuf::from)
                .unwrap_or_else(default_repo_root),
        );
        require_git_worktree(&repo);

        let parquet_dir = canonical_dir(
            "SPUR_GRAPH_PERF_FIXTURE",
            std::env::var_os("SPUR_GRAPH_PERF_FIXTURE")
                .map(PathBuf::from)
                .unwrap_or_else(|| repo.join(".spur/graph/CURRENT")),
        );
        let query_override = std::env::var("SPUR_GRAPH_PERF_QUERY").ok();
        let query = nonempty_env_or_default(
            "SPUR_GRAPH_PERF_QUERY",
            query_override.as_deref().unwrap_or(DEFAULT_QUERY),
        );
        let overlay_query_override = std::env::var("SPUR_GRAPH_PERF_OVERLAY_QUERY").ok();
        let overlay_query = nonempty_env_or_default(
            "SPUR_GRAPH_PERF_OVERLAY_QUERY",
            overlay_query_override
                .as_deref()
                .or(query_override.as_deref())
                .unwrap_or(DEFAULT_OVERLAY_QUERY),
        );
        let changed_file = relative_fixture_file(
            &repo,
            "SPUR_GRAPH_PERF_CHANGED_FILE",
            std::env::var_os("SPUR_GRAPH_PERF_CHANGED_FILE")
                .unwrap_or_else(|| OsString::from(DEFAULT_CHANGED_FILE)),
        );
        let sample_size = bounded_env_usize(
            "SPUR_GRAPH_PERF_SAMPLE_SIZE",
            REQUIRED_WARM_SAMPLES,
            REQUIRED_WARM_SAMPLES,
            MAX_WARM_SAMPLES,
        );
        let measurement_seconds = bounded_env_u64(
            "SPUR_GRAPH_PERF_MEASUREMENT_SECONDS",
            5,
            1,
            MAX_MEASUREMENT_SECONDS,
        );
        let label = std::env::var("SPUR_GRAPH_PERF_LABEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                repo.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("fixture")
                    .to_owned()
            });

        Self {
            label: sanitize_label(&label),
            repo,
            parquet_dir,
            query,
            overlay_query,
            changed_file,
            sample_size,
            measurement_time: Duration::from_secs(measurement_seconds),
        }
    }

    fn search_options(&self) -> SearchOptions {
        SearchOptions {
            query: self.query.clone(),
            mode: SearchMode::Exact,
            filters: SearchFilters::default(),
            limit: 20,
        }
    }

    fn changed_files(&self) -> [PathBuf; 1] {
        [self.changed_file.clone()]
    }
}

fn bench_overlay_vs_direct_parquet(c: &mut Criterion) {
    let config = ProbeConfig::load();
    let parquet_dir = config.parquet_dir.clone();
    let parquet = ParquetClient::open(&parquet_dir).unwrap_or_else(|err| {
        panic!(
            "invalid SPUR_GRAPH_PERF_FIXTURE `{}`: {err:#}",
            parquet_dir.display()
        )
    });
    let repo = config.repo.clone();
    let changed_files = config.changed_files();
    let overlay_empty =
        OverlayClient::new(&parquet, &repo, &[]).expect("empty overlay over live parquet");
    let overlay_one_file = OverlayClient::new(&parquet, &repo, &changed_files)
        .expect("one-file overlay over live parquet");

    let search_base = config.search_options();
    let search_overlay_hit = SearchOptions {
        query: config.overlay_query.clone(),
        mode: SearchMode::Exact,
        filters: SearchFilters::default(),
        limit: 20,
    };
    let base_search_result = parquet
        .search_symbols(&search_base)
        .unwrap_or_else(|err| panic!("validate SPUR_GRAPH_PERF_QUERY `{}`: {err:#}", config.query));
    let base_candidate = base_search_result.candidates.first().unwrap_or_else(|| {
        panic!(
            "SPUR_GRAPH_PERF_QUERY `{}` returned no symbols in `{}`",
            config.query,
            parquet_dir.display()
        )
    });
    let symbol_id = base_candidate.stable_symbol_id.clone();
    let symbol_file = base_candidate.file_path.clone();

    let mut group = c.benchmark_group("bench_overlay_vs_direct_parquet");
    group.sample_size(config.sample_size);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(8));

    group.bench_function("parquet_cached_search_base", |b| {
        b.iter(|| {
            let result = parquet
                .search_symbols(black_box(&search_base))
                .expect("parquet search");
            black_box(result);
        });
    });
    group.bench_function("overlay_empty_search_base", |b| {
        b.iter(|| {
            let result = overlay_empty
                .search_symbols(black_box(&search_base))
                .expect("empty overlay search");
            black_box(result);
        });
    });
    group.bench_function("overlay_one_file_search_base", |b| {
        b.iter(|| {
            let result = overlay_one_file
                .search_symbols(black_box(&search_base))
                .expect("one-file overlay search");
            black_box(result);
        });
    });
    group.bench_function("parquet_cached_search_overlay_symbol", |b| {
        b.iter(|| {
            let result = parquet
                .search_symbols(black_box(&search_overlay_hit))
                .expect("parquet overlay-symbol search");
            black_box(result);
        });
    });
    group.bench_function("overlay_one_file_search_overlay_symbol", |b| {
        b.iter(|| {
            let result = overlay_one_file
                .search_symbols(black_box(&search_overlay_hit))
                .expect("one-file overlay-symbol search");
            black_box(result);
        });
    });
    group.bench_function("parquet_cached_callers", |b| {
        b.iter(|| {
            let records = parquet.find_caller_edges(black_box(symbol_id.as_str()));
            black_box(records);
        });
    });
    group.bench_function("overlay_empty_callers", |b| {
        b.iter(|| {
            let records = overlay_empty.find_caller_edges(black_box(symbol_id.as_str()));
            black_box(records);
        });
    });
    group.bench_function("overlay_one_file_callers", |b| {
        b.iter(|| {
            let records = overlay_one_file.find_caller_edges(black_box(symbol_id.as_str()));
            black_box(records);
        });
    });
    group.bench_function("parquet_cached_session", |b| {
        b.iter(|| {
            run_mcp_latency_session(black_box(&parquet), black_box(&search_base));
        });
    });
    group.bench_function("overlay_empty_session", |b| {
        b.iter(|| {
            run_mcp_latency_session(black_box(&overlay_empty), black_box(&search_base));
        });
    });
    group.bench_function("overlay_one_file_session", |b| {
        b.iter(|| {
            run_mcp_latency_session(black_box(&overlay_one_file), black_box(&search_base));
        });
    });
    group.bench_function("parquet_cached_callees", |b| {
        b.iter(|| {
            let records = parquet.find_callee_edges(black_box(symbol_id.as_str()));
            black_box(records);
        });
    });
    group.bench_function("overlay_empty_callees", |b| {
        b.iter(|| {
            let records = overlay_empty.find_callee_edges(black_box(symbol_id.as_str()));
            black_box(records);
        });
    });
    group.bench_function("overlay_one_file_callees", |b| {
        b.iter(|| {
            let records = overlay_one_file.find_callee_edges(black_box(symbol_id.as_str()));
            black_box(records);
        });
    });
    group.bench_function("parquet_cached_resolve", |b| {
        b.iter(|| {
            let resolution = parquet
                .resolve_selector(black_box(config.query.as_str()))
                .expect("parquet resolve");
            black_box(resolution);
        });
    });
    group.bench_function("overlay_empty_resolve", |b| {
        b.iter(|| {
            let resolution = overlay_empty
                .resolve_selector(black_box(config.query.as_str()))
                .expect("empty overlay resolve");
            black_box(resolution);
        });
    });
    group.bench_function("parquet_cached_file_symbols_small", |b| {
        b.iter(|| {
            let symbols = parquet
                .symbols_by_file(black_box(symbol_file.as_str()))
                .expect("parquet file symbols");
            black_box(symbols);
        });
    });
    group.bench_function("overlay_empty_file_symbols_small", |b| {
        b.iter(|| {
            let symbols = overlay_empty
                .symbols_by_file(black_box(symbol_file.as_str()))
                .expect("empty overlay file symbols");
            black_box(symbols);
        });
    });
    group.bench_function("parquet_cached_file_symbols_large", |b| {
        b.iter(|| {
            let symbols = parquet
                .symbols_by_file(black_box(path_as_slash(&config.changed_file).as_str()))
                .expect("parquet large file symbols");
            black_box(symbols);
        });
    });
    group.bench_function("overlay_one_file_file_symbols_large", |b| {
        b.iter(|| {
            let symbols = overlay_one_file
                .symbols_by_file(black_box(path_as_slash(&config.changed_file).as_str()))
                .expect("one-file overlay large file symbols");
            black_box(symbols);
        });
    });
    group.finish();
}

fn bench_overlay_construction(c: &mut Criterion) {
    let config = ProbeConfig::load();
    let parquet_dir = config.parquet_dir.clone();
    let repo = config.repo.clone();
    let one_file = config.changed_files();
    let parquet = ParquetClient::open(&parquet_dir).unwrap_or_else(|err| {
        panic!(
            "invalid SPUR_GRAPH_PERF_FIXTURE `{}`: {err:#}",
            parquet_dir.display()
        )
    });
    let (empty_artifact, empty_shadowed) =
        OverlayClient::<&ParquetClient>::extract_delta(&repo, &[])
            .expect("extract empty overlay delta");
    let (one_artifact, one_shadowed) =
        OverlayClient::<&ParquetClient>::extract_delta(&repo, &one_file)
            .expect("extract one-file overlay delta");

    let mut group = c.benchmark_group("bench_overlay_construction");
    group.sample_size(config.sample_size);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(10));

    group.bench_function("parquet_open", |b| {
        b.iter(|| {
            let client = ParquetClient::open(black_box(&parquet_dir)).expect("open parquet");
            black_box(client);
        });
    });
    group.bench_function("extract_delta_empty", |b| {
        b.iter(|| {
            let delta = OverlayClient::<&ParquetClient>::extract_delta(&repo, black_box(&[]))
                .expect("extract empty");
            black_box(delta);
        });
    });
    group.bench_function("extract_delta_one_file", |b| {
        b.iter(|| {
            let delta = OverlayClient::<&ParquetClient>::extract_delta(&repo, black_box(&one_file))
                .expect("extract one file");
            black_box(delta);
        });
    });
    group.bench_function("from_artifacts_empty", |b| {
        b.iter(|| {
            let overlay = OverlayClient::from_artifacts(
                black_box(&parquet),
                black_box(empty_artifact.clone()),
                black_box(empty_shadowed.clone()),
            )
            .expect("wrap empty delta");
            black_box(overlay);
        });
    });
    group.bench_function("from_artifacts_one_file", |b| {
        b.iter(|| {
            let overlay = OverlayClient::from_artifacts(
                black_box(&parquet),
                black_box(one_artifact.clone()),
                black_box(one_shadowed.clone()),
            )
            .expect("wrap one-file delta");
            black_box(overlay);
        });
    });
    group.bench_function("overlay_new_empty", |b| {
        b.iter(|| {
            let overlay = OverlayClient::new(black_box(&parquet), &repo, black_box(&[]))
                .expect("new empty overlay");
            black_box(overlay);
        });
    });
    group.bench_function("overlay_new_one_file", |b| {
        b.iter(|| {
            let overlay = OverlayClient::new(black_box(&parquet), &repo, black_box(&one_file[..]))
                .expect("new one-file overlay");
            black_box(overlay);
        });
    });
    group.finish();
}

fn bench_overlay_stage_probe(c: &mut Criterion) {
    let config = ProbeConfig::load();
    let parquet = ParquetClient::open(&config.parquet_dir).unwrap_or_else(|err| {
        panic!(
            "invalid SPUR_GRAPH_PERF_FIXTURE `{}`: {err:#}",
            config.parquet_dir.display()
        )
    });
    let options = config.search_options();
    let base_search = parquet
        .search_symbols(&options)
        .unwrap_or_else(|err| panic!("validate SPUR_GRAPH_PERF_QUERY `{}`: {err:#}", config.query));
    base_search.candidates.first().unwrap_or_else(|| {
        panic!(
            "SPUR_GRAPH_PERF_QUERY `{}` returned no symbols in `{}`",
            config.query,
            config.parquet_dir.display()
        )
    });
    let changed_files = config.changed_files();
    let (delta_artifact, shadowed) =
        OverlayClient::<&ParquetClient>::extract_delta(&config.repo, &changed_files)
            .expect("validate SPUR_GRAPH_PERF_CHANGED_FILE extraction");
    let overlay = OverlayClient::from_artifacts(&parquet, delta_artifact, shadowed)
        .expect("construct validated overlay fixture");
    let overlay_search = overlay
        .search_symbols(&options)
        .expect("validate query against overlay fixture");
    let symbol_id = overlay_search
        .candidates
        .first()
        .unwrap_or_else(|| {
            panic!(
                "SPUR_GRAPH_PERF_QUERY `{}` was shadowed without an overlay replacement after extracting `{}`",
                config.query, config.changed_file.display()
            )
        })
        .stable_symbol_id
        .clone();
    let digest = total_session_digest(&config, &parquet, &options);
    print_probe_metadata(&config, &parquet, &digest);

    let mut group = c.benchmark_group(format!("overlay_stage_probe_{}", config.label));
    group.sample_size(config.sample_size);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(config.measurement_time);

    group.bench_function("stage_base_parquet_query", |b| {
        b.iter(|| {
            let result = parquet
                .search_symbols(black_box(&options))
                .expect("stage base parquet query");
            black_box(result);
        });
    });
    group.bench_function("stage_git_observation", |b| {
        b.iter(|| black_box(git_observation(black_box(&config.repo))));
    });
    group.bench_function("stage_snapshot_oid_validation", |b| {
        b.iter(|| {
            black_box(snapshot_oid_validation(
                black_box(&parquet),
                black_box(&config.repo),
            ))
        });
    });
    group.bench_function("stage_delta_construction", |b| {
        b.iter(|| {
            let delta = OverlayClient::<&ParquetClient>::extract_delta(
                black_box(&config.repo),
                black_box(&changed_files),
            )
            .expect("stage delta construction");
            black_box(delta);
        });
    });
    group.bench_function("stage_overlay_query", |b| {
        b.iter(|| {
            let result = overlay
                .search_symbols(black_box(&options))
                .expect("stage overlay query");
            black_box(result);
        });
    });
    group.bench_function("stage_response_shaping", |b| {
        b.iter(|| {
            let response = shape_response(black_box(&overlay), black_box(&symbol_id));
            black_box(response);
        });
    });
    group.bench_function("stage_total_session", |b| {
        b.iter(|| {
            let digest =
                total_session_digest(black_box(&config), black_box(&parquet), black_box(&options));
            black_box(digest);
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

fn shape_response(client: &dyn GraphQueryClient, symbol_id: &str) -> String {
    let symbol = client
        .symbol_by_id(symbol_id)
        .expect("response shaping symbol lookup")
        .expect("response shaping symbol exists");
    let manifest = client
        .file_manifest_by_path(&symbol.file_path)
        .expect("response shaping file manifest lookup");
    let callers = client.find_caller_edges(symbol_id);
    format!("{symbol:?}\n{manifest:?}\n{callers:?}")
}

fn total_session_digest(
    config: &ProbeConfig,
    parquet: &ParquetClient,
    options: &SearchOptions,
) -> blake3::Hash {
    let base = parquet
        .search_symbols(options)
        .expect("total session base parquet query");
    let observation = git_observation(&config.repo);
    let validation = snapshot_oid_validation(parquet, &config.repo);
    let changed_files = config.changed_files();
    let (artifact, shadowed) =
        OverlayClient::<&ParquetClient>::extract_delta(&config.repo, &changed_files)
            .expect("total session delta construction");
    let overlay = OverlayClient::from_artifacts(parquet, artifact, shadowed)
        .expect("total session overlay construction");
    let overlay_search = overlay
        .search_symbols(options)
        .expect("total session overlay query");
    let overlay_symbol_id = overlay_search
        .candidates
        .first()
        .expect("validated query remains present in total session")
        .stable_symbol_id
        .clone();
    let response = shape_response(&overlay, &overlay_symbol_id);

    let mut hasher = blake3::Hasher::new();
    hasher.update(format!("{base:?}\n{overlay_search:?}\n{response}").as_bytes());
    hasher.update(&observation);
    hasher.update(validation.as_bytes());
    hasher.finalize()
}

fn git_observation(repo: &Path) -> Vec<u8> {
    checked_git_output(
        repo,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .stdout
}

fn snapshot_oid_validation(parquet: &ParquetClient, repo: &Path) -> blake3::Hash {
    let base_files = parquet
        .file_oids()
        .expect("stage snapshot validation reads artifact file OIDs");
    let tracked_state = checked_git_output(repo, &["ls-files", "-t", "-z"]).stdout;
    let tracked_oids = checked_git_output(repo, &["ls-files", "-s", "-z"]).stdout;
    let mut hasher = blake3::Hasher::new();
    for (path, oid) in base_files {
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(oid.as_bytes());
        hasher.update(&[0]);
    }
    hasher.update(&tracked_state);
    hasher.update(&tracked_oids);
    hasher.finalize()
}

fn checked_git_output(repo: &Path, args: &[&str]) -> Output {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .args(args)
        .output()
        .unwrap_or_else(|err| panic!("run process-scoped git {}: {err}", args.join(" ")));
    if !output.status.success() {
        panic!(
            "process-scoped git {} failed in `{}` (status {}): {}",
            args.join(" "),
            repo.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    output
}

fn print_probe_metadata(config: &ProbeConfig, parquet: &ParquetClient, digest: &blake3::Hash) {
    let git_version = Command::new("git")
        .arg("--version")
        .output()
        .expect("run git --version");
    assert!(git_version.status.success(), "git --version failed");
    let revision =
        String::from_utf8_lossy(&checked_git_output(&config.repo, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_owned();
    let tracked_files =
        nul_record_count(&checked_git_output(&config.repo, &["ls-files", "-z"]).stdout);
    let dirty_records = checked_git_output(
        &config.repo,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    )
    .stdout
    .split(|byte| *byte == b'\n')
    .filter(|line| !line.is_empty())
    .count();
    let indexed_source_files = parquet
        .file_oids()
        .expect("metadata reads artifact file OIDs")
        .len();
    let metadata = serde_json::json!({
        "event": "spur_graph_overlay_probe",
        "label": config.label,
        "repository": config.repo.display().to_string(),
        "artifact": config.parquet_dir.display().to_string(),
        "query": config.query,
        "changed_file": path_as_slash(&config.changed_file),
        "sample_size": config.sample_size,
        "warm_up_seconds": 1,
        "measurement_time_seconds": config.measurement_time.as_secs(),
        "git_version": String::from_utf8_lossy(&git_version.stdout).trim(),
        "git_options": ["GIT_OPTIONAL_LOCKS=0", "-c core.fsmonitor=false", "-c core.untrackedCache=false"],
        "revision": revision,
        "tracked_files": tracked_files,
        "dirty_records": dirty_records,
        "indexed_source_files": indexed_source_files,
        "artifact_graph_content_hash": parquet.manifest().graph_content_hash,
        "artifact_indexed_commit_oid": parquet.manifest().indexed_commit_oid,
        "correctness_digest": digest.to_hex().to_string(),
        "stages": {
            "stage_base_parquet_query": "one Parquet search_symbols call",
            "stage_git_observation": "one exact-path porcelain-v1 status command",
            "stage_snapshot_oid_validation": "artifact file-OID read plus exact-path ls-files -t/-s and fingerprint",
            "stage_delta_construction": "extract_delta for the explicit changed file",
            "stage_overlay_query": "one search_symbols call on a prebuilt overlay",
            "stage_response_shaping": "post-search symbol, file-manifest, and caller assembly",
            "stage_total_session": "all preceding stages once, in order"
        }
    });
    eprintln!("SPUR_GRAPH_PERF_METADATA={metadata}");
}

fn require_git_worktree(repo: &Path) {
    let output = checked_git_output(repo, &["rev-parse", "--is-inside-work-tree"]);
    if output.stdout != b"true\n" {
        panic!(
            "SPUR_GRAPH_PERF_REPO `{}` is not a Git worktree",
            repo.display()
        );
    }
}

fn canonical_dir(name: &str, path: PathBuf) -> PathBuf {
    let canonical = path.canonicalize().unwrap_or_else(|err| {
        panic!("invalid {name} `{}`: {err}", path.display());
    });
    if !canonical.is_dir() {
        panic!("invalid {name} `{}`: expected a directory", path.display());
    }
    canonical
}

fn relative_fixture_file(repo: &Path, name: &str, raw: OsString) -> PathBuf {
    let relative = PathBuf::from(&raw);
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
    {
        panic!(
            "invalid {name} `{}`: expected a non-empty repository-relative path without `..`",
            relative.display()
        );
    }
    let absolute = repo.join(&relative);
    let canonical = absolute.canonicalize().unwrap_or_else(|err| {
        panic!(
            "invalid {name} `{}` under `{}`: {err}",
            relative.display(),
            repo.display()
        );
    });
    if !canonical.starts_with(repo) || !canonical.is_file() {
        panic!(
            "invalid {name} `{}`: expected a regular file contained by `{}`",
            relative.display(),
            repo.display()
        );
    }
    relative
}

fn nonempty_env_or_default(name: &str, value: &str) -> String {
    if value.trim().is_empty() {
        panic!("invalid {name}: value must not be empty");
    }
    value.to_owned()
}

fn bounded_env_usize(name: &str, default: usize, min: usize, max: usize) -> usize {
    let Some(raw) = std::env::var_os(name) else {
        return default;
    };
    let value = raw
        .to_str()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_else(|| {
            panic!(
                "invalid {name} `{}`: expected an integer",
                raw.to_string_lossy()
            )
        });
    if !(min..=max).contains(&value) {
        panic!("invalid {name} `{value}`: expected {min}..={max}");
    }
    value
}

fn bounded_env_u64(name: &str, default: u64, min: u64, max: u64) -> u64 {
    let Some(raw) = std::env::var_os(name) else {
        return default;
    };
    let value = raw
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_else(|| {
            panic!(
                "invalid {name} `{}`: expected an integer",
                raw.to_string_lossy()
            )
        });
    if !(min..=max).contains(&value) {
        panic!("invalid {name} `{value}`: expected {min}..={max}");
    }
    value
}

fn nul_record_count(bytes: &[u8]) -> usize {
    bytes
        .split(|byte| *byte == 0)
        .filter(|row| !row.is_empty())
        .count()
}

fn sanitize_label(label: &str) -> String {
    label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn path_as_slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn default_repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize repo root from spur-graph manifest dir")
}

criterion_group!(
    benches,
    bench_overlay_vs_direct_parquet,
    bench_overlay_construction,
    bench_overlay_stage_probe
);
criterion_main!(benches);

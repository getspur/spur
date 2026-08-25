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

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use spur_graph::{
    overlay_changed_oids, status_observation, with_worktree_root_for_request,
    FsmonitorCapabilities, GraphMcpModule, GraphQueryClient, OverlayClient, ParquetClient,
    SearchFilters, SearchMode, SearchOptions, SearchResult,
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
    overlay_search.candidates.first().unwrap_or_else(|| {
        panic!(
            "SPUR_GRAPH_PERF_QUERY `{}` was shadowed without an overlay replacement after extracting `{}`",
            config.query, config.changed_file.display()
        )
    });
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
    group.bench_function("stage_warm_validation_combined", |b| {
        b.iter(|| {
            black_box(warm_validation_digest(
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
            let response = shape_response(black_box(&overlay_search));
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

fn shape_response(search: &SearchResult) -> String {
    let candidates = search
        .candidates
        .iter()
        .map(|candidate| {
            serde_json::json!({
                "uri": format!("graph://symbol/{}", candidate.stable_symbol_id),
                "id": candidate.stable_symbol_id,
                "entity_name": candidate.entity_name,
                "qualified_name": candidate.qualified_name,
                "file_path": candidate.file_path,
                "line_range": candidate.line_range,
                "symbol_kind": candidate.symbol_kind,
                "enclosing_scope": candidate.enclosing_scope,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "total_matches": search.total_matches,
        "truncated": search.truncated,
        "candidates": candidates,
    })
    .to_string()
}

fn total_session_digest(
    config: &ProbeConfig,
    parquet: &ParquetClient,
    options: &SearchOptions,
) -> blake3::Hash {
    let base = parquet
        .search_symbols(options)
        .expect("total session base parquet query");
    let warm_validation = warm_validation_digest(parquet, &config.repo);
    let changed_files = config.changed_files();
    let (artifact, shadowed) =
        OverlayClient::<&ParquetClient>::extract_delta(&config.repo, &changed_files)
            .expect("total session delta construction");
    let overlay = OverlayClient::from_artifacts(parquet, artifact, shadowed)
        .expect("total session overlay construction");
    let overlay_search = overlay
        .search_symbols(options)
        .expect("total session overlay query");
    overlay_search
        .candidates
        .first()
        .expect("validated query remains present in total session");
    let response = shape_response(&overlay_search);

    let mut hasher = blake3::Hasher::new();
    hasher.update(format!("{base:?}\n{overlay_search:?}\n{response}").as_bytes());
    hasher.update(warm_validation.as_bytes());
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

fn warm_validation_digest(parquet: &ParquetClient, repo: &Path) -> blake3::Hash {
    let observation = git_observation(repo);
    let validation = snapshot_oid_validation(parquet, repo);
    let mut hasher = blake3::Hasher::new();
    hasher.update(&observation);
    hasher.update(validation.as_bytes());
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
            "stage_warm_validation_combined": "Git observation followed by snapshot/OID validation in one directly sampled Criterion iteration",
            "stage_delta_construction": "extract_delta for the explicit changed file",
            "stage_overlay_query": "one search_symbols call on a prebuilt overlay",
            "stage_response_shaping": "pure JSON transformation and serialization of the precomputed overlay search result; zero graph queries",
            "stage_total_session": "one base query, directly combined warm validation, delta extraction and overlay construction, one overlay query, and pure response shaping that reuses that query result"
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

fn assert_release_gate_claim_grounded(claimed_pass: bool, evidence_present: bool, gate: &str) {
    assert!(
        !claimed_pass || evidence_present,
        "release gate `{gate}` cannot pass without direct evidence"
    );
}

fn bench_overlay_release_matrix(_criterion: &mut Criterion) {
    let Some(scenario) = std::env::var("SPUR_GRAPH_RELEASE_SCENARIO").ok() else {
        return;
    };
    let allowed_scenarios = [
        "clean",
        "one_edit",
        "many_edits",
        "untracked_heavy",
        "delete",
        "rename",
        "head_lag",
        "fsmonitor_unsupported",
        "watcher_failure",
        "concurrent_requests",
    ];
    assert!(
        allowed_scenarios.contains(&scenario.as_str()),
        "invalid SPUR_GRAPH_RELEASE_SCENARIO `{scenario}`: expected one of {}",
        allowed_scenarios.join(", ")
    );

    let config = ProbeConfig::load();
    let repeats = bounded_env_usize("SPUR_GRAPH_RELEASE_REPEATS", 3, 3, MAX_WARM_SAMPLES);
    let parquet = ParquetClient::open(&config.parquet_dir).unwrap_or_else(|error| {
        panic!(
            "invalid SPUR_GRAPH_PERF_FIXTURE `{}`: {error:#}",
            config.parquet_dir.display()
        )
    });
    let base_files = parquet
        .file_oids()
        .expect("release matrix reads artifact file OIDs");
    let options = config.search_options();
    let args = serde_json::json!({
        "query": config.query,
        "mode": "exact",
        "limit": 20,
        "response_format": "compact",
    });
    let capabilities = release_observer_capabilities(&scenario);
    let module = GraphMcpModule::default();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("release matrix Tokio runtime");

    // Warm both landed seams before collecting the diagnostic repeat set.
    status_observation(&config.repo, capabilities).expect("warm release observer");
    overlay_changed_oids(&config.repo, base_files.clone())
        .expect("warm production overlay snapshot");

    let mut observer_routes = BTreeSet::new();
    let mut git_observation_ms = Vec::with_capacity(repeats);
    let mut snapshot_validation_ms = Vec::with_capacity(repeats);
    let mut base_parquet_query_ms = Vec::with_capacity(repeats);
    let mut overlay_merge_shaping_ms = Vec::with_capacity(repeats);
    let mut total_ms = Vec::with_capacity(repeats * release_request_concurrency(&scenario));
    let mut actual_digests = Vec::new();
    let mut oracle_digests = Vec::with_capacity(repeats);
    let mut correctness_mismatches = 0usize;
    let mut changed_path_counts = Vec::with_capacity(repeats);

    for _ in 0..repeats {
        let observation_started = Instant::now();
        let observation =
            status_observation(&config.repo, capabilities).expect("release observer sample");
        git_observation_ms.push(elapsed_ms(observation_started));
        observer_routes.insert(format!("{:?}", observation.source));

        let snapshot_started = Instant::now();
        let changed_oids = overlay_changed_oids(&config.repo, base_files.clone())
            .expect("production overlay snapshot sample");
        snapshot_validation_ms.push(elapsed_ms(snapshot_started));
        changed_path_counts.push(changed_oids.len());
        let changed_paths = changed_oids.keys().cloned().collect::<Vec<_>>();

        let base_started = Instant::now();
        let base_search = parquet
            .search_symbols(&options)
            .expect("release base Parquet query sample");
        base_parquet_query_ms.push(elapsed_ms(base_started));

        let merge_started = Instant::now();
        let oracle_search = if changed_paths.is_empty() {
            base_search
        } else {
            OverlayClient::new(&parquet, &config.repo, &changed_paths)
                .expect("construct exact overlay oracle")
                .search_symbols(&options)
                .expect("query exact overlay oracle")
        };
        let oracle_signature = normalized_search_signature(
            &serde_json::from_str::<serde_json::Value>(&shape_response(&oracle_search))
                .expect("parse oracle response shape"),
        );
        overlay_merge_shaping_ms.push(elapsed_ms(merge_started));
        let oracle_digest = response_digest(&oracle_signature);
        oracle_digests.push(oracle_digest.clone());

        for (response, request_ms) in dispatch_release_requests(
            &runtime,
            &module,
            &config.repo,
            &args,
            release_request_concurrency(&scenario),
        ) {
            total_ms.push(request_ms);
            let actual_signature = normalized_search_signature(&response);
            let actual_digest = response_digest(&actual_signature);
            if actual_signature != oracle_signature {
                correctness_mismatches += 1;
            }
            actual_digests.push(actual_digest);
        }
    }

    // This harness dispatches a real request, but the production request path does not
    // expose a base-query counter or route trace. Keep those release gates fail-closed
    // until instrumentation supplies direct evidence for each measured request.
    let base_operation_count = None::<usize>;
    let exactly_one_base_operation = base_operation_count == Some(1);
    assert_release_gate_claim_grounded(
        exactly_one_base_operation,
        base_operation_count.is_some(),
        "exactly_one_base_operation",
    );
    let independent_correctness_mismatches = None::<usize>;
    let zero_independent_correctness_mismatches = independent_correctness_mismatches == Some(0);
    assert_release_gate_claim_grounded(
        zero_independent_correctness_mismatches,
        independent_correctness_mismatches.is_some(),
        "zero_independent_correctness_mismatches",
    );
    let production_request_routes = BTreeSet::<String>::new();
    let optimized_and_fallback_routes_observed = production_request_routes
        .iter()
        .any(|route| route == "FsmonitorNative")
        && production_request_routes
            .iter()
            .any(|route| route.starts_with("ExactFallback"));
    assert_release_gate_claim_grounded(
        optimized_and_fallback_routes_observed,
        !production_request_routes.is_empty(),
        "optimized_and_fallback_request_routes",
    );
    let snapshot_max_ms = max_sample(&snapshot_validation_ms);
    let diagnostic_snapshot_max_below_30_ms = snapshot_max_ms < 30.0;
    let metadata = serde_json::json!({
        "event": "spur_graph_overlay_release_cell",
        "project": config.label,
        "scenario": scenario,
        "repository": config.repo.display().to_string(),
        "artifact": config.parquet_dir.display().to_string(),
        "artifact_graph_content_hash": parquet.manifest().graph_content_hash,
        "artifact_indexed_commit_oid": parquet.manifest().indexed_commit_oid,
        "revision": String::from_utf8_lossy(
            &checked_git_output(&config.repo, &["rev-parse", "HEAD"]).stdout
        ).trim(),
        "query": config.query,
        "repeats": repeats,
        "request_samples": actual_digests.len(),
        "request_concurrency": release_request_concurrency(&scenario),
        "production_release_state_observed_by_harness": false,
        "production_fsmonitor_release_enabled_at_task6_source_review": false,
        "probe_observer_capabilities": {
            "release_enabled": capabilities.release_enabled,
            "built_in_supported": capabilities.built_in_supported,
            "local_filesystem": capabilities.local_filesystem,
            "watcher_healthy": capabilities.watcher_healthy,
        },
        "probe_observer_routes": observer_routes,
        "production_request_routes": production_request_routes,
        "production_request_route_observed": false,
        "expected_production_route_at_task6_from_source_review": "ExactFallback(ReleaseDisabled)",
        "base_operation_count": base_operation_count,
        "base_operation_count_observed": false,
        "base_operation_count_source": "unobserved: request dispatch count is not a base-query count",
        "independent_correctness_mismatches": independent_correctness_mismatches,
        "independent_correctness_oracle": false,
        "correlated_oracle_mismatches": correctness_mismatches,
        "actual_result_digests": actual_digests,
        "correlated_oracle_result_digests": oracle_digests,
        "changed_path_counts": changed_path_counts,
        "sample_protocol": {
            "statistic": "median_and_max",
            "post_sample_count": repeats,
            "pre_sample_count": REQUIRED_WARM_SAMPLES,
            "pre_post_comparable": false,
            "reason": "this harness has five differently defined stages from the eight-stage PRE protocol; compare the explicit sample counts separately",
        },
        "stages": {
            "probe_git_observation": diagnostic_stage_summary(&git_observation_ms),
            "production_snapshot_validation": diagnostic_stage_summary(&snapshot_validation_ms),
            "manual_base_parquet_query": diagnostic_stage_summary(&base_parquet_query_ms),
            "correlated_oracle_overlay_merge_shaping": diagnostic_stage_summary(&overlay_merge_shaping_ms),
            "production_request_total": diagnostic_stage_summary(&total_ms),
        },
        "cell_diagnostics": {
            "repeat_count_at_least_three": repeats >= 3,
            "diagnostic_snapshot_max_below_30_ms": diagnostic_snapshot_max_below_30_ms,
            "correlated_oracle_mismatches_zero": correctness_mismatches == 0,
        },
        "release_gates": {
            "cross_project_snapshot_p95_below_30_ms": {
                "status": "not_proven",
                "reason": "this cell reports median/max diagnostics and cannot establish a cross-project p95 gate; diagnostic_snapshot_max_below_30_ms records any observed threshold violation",
            },
            "exactly_one_base_operation": {
                "status": "not_proven",
                "reason": "the production request path did not expose a base-query counter",
            },
            "zero_independent_correctness_mismatches": {
                "status": "not_proven",
                "reason": "the comparison reused candidate artifact paths and changed-path discovery",
            },
            "optimized_and_fallback_request_routes": {
                "status": "not_proven",
                "reason": "only the separately injected observer probe recorded routes",
            },
            "reproducible_pre_post_and_solve": {
                "status": "not_proven",
                "reason": "a per-cell diagnostic cannot establish identical PRE inputs, cross-project aggregation, or persisted SOLVE evidence",
            },
        },
        "release_eligible": false,
    });
    eprintln!("SPUR_GRAPH_RELEASE_CELL={metadata}");
}

fn release_observer_capabilities(scenario: &str) -> FsmonitorCapabilities {
    match scenario {
        "fsmonitor_unsupported" => FsmonitorCapabilities {
            release_enabled: true,
            built_in_supported: false,
            local_filesystem: true,
            watcher_healthy: false,
        },
        "watcher_failure" => FsmonitorCapabilities {
            release_enabled: true,
            built_in_supported: true,
            local_filesystem: true,
            watcher_healthy: false,
        },
        _ => FsmonitorCapabilities {
            release_enabled: true,
            built_in_supported: true,
            local_filesystem: true,
            watcher_healthy: true,
        },
    }
}

fn release_request_concurrency(scenario: &str) -> usize {
    if scenario == "concurrent_requests" {
        3
    } else {
        1
    }
}

fn dispatch_release_requests(
    runtime: &tokio::runtime::Runtime,
    module: &GraphMcpModule,
    repo: &Path,
    args: &serde_json::Value,
    concurrency: usize,
) -> Vec<(serde_json::Value, f64)> {
    let dispatch = |args: serde_json::Value| {
        let repo = repo.to_path_buf();
        async move {
            let started = Instant::now();
            let response =
                with_worktree_root_for_request(repo, module.dispatch("code_symbol_search", args))
                    .await
                    .unwrap_or_else(|error| {
                        panic!("release code_symbol_search dispatch failed: {error:?}")
                    });
            (response, elapsed_ms(started))
        }
    };

    match concurrency {
        1 => vec![runtime.block_on(dispatch(args.clone()))],
        3 => runtime.block_on(async {
            let (first, second, third) = tokio::join!(
                dispatch(args.clone()),
                dispatch(args.clone()),
                dispatch(args.clone())
            );
            vec![first, second, third]
        }),
        other => panic!("unsupported release request concurrency {other}"),
    }
}

fn normalized_search_signature(value: &serde_json::Value) -> serde_json::Value {
    let candidates = value
        .get("candidates")
        .and_then(serde_json::Value::as_array)
        .unwrap_or_else(|| panic!("release response lacks candidates array: {value}"))
        .iter()
        .map(|candidate| {
            serde_json::json!({
                "id": candidate.get("id"),
                "entity_name": candidate.get("entity_name"),
                "qualified_name": candidate.get("qualified_name"),
                "file_path": candidate.get("file_path"),
                "line_range": candidate.get("line_range"),
                "symbol_kind": candidate.get("symbol_kind"),
                "enclosing_scope": candidate.get("enclosing_scope"),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "total_matches": value.get("total_matches"),
        "truncated": value.get("truncated"),
        "candidates": candidates,
    })
}

fn response_digest(value: &serde_json::Value) -> String {
    blake3::hash(&serde_json::to_vec(value).expect("serialize release response signature"))
        .to_hex()
        .to_string()
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1_000.0
}

fn max_sample(samples: &[f64]) -> f64 {
    samples
        .iter()
        .copied()
        .max_by(f64::total_cmp)
        .expect("release diagnostic has at least one sample")
}

fn diagnostic_stage_summary(samples: &[f64]) -> serde_json::Value {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let median = if sorted.len().is_multiple_of(2) {
        let upper = sorted.len() / 2;
        (sorted[upper - 1] + sorted[upper]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    };
    serde_json::json!({
        "samples_ms": samples,
        "sample_count": samples.len(),
        "median_ms": median,
        "max_ms": max_sample(&sorted),
    })
}

criterion_group!(
    benches,
    bench_overlay_vs_direct_parquet,
    bench_overlay_construction,
    bench_overlay_stage_probe,
    bench_overlay_release_matrix
);
criterion_main!(benches);

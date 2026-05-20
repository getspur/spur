use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use spur_graph::{build_facts_for_paths, compute_graph_content_hash, current_manifest_version};
use tempfile::TempDir;

const DEFAULT_FILE_COUNT: usize = 100_000;
const DEFAULT_CHANGE_SET: usize = 1_000;
const DEFAULT_DIRTY_MODS: usize = 100;
const BENCH_FILE_COUNT_ENV: &str = "SPUR_GRAPH_BENCH_FILES";
const BENCH_CHANGE_SET_ENV: &str = "SPUR_GRAPH_BENCH_CHANGE_SET";
const BENCH_DIRTY_MODS_ENV: &str = "SPUR_GRAPH_BENCH_DIRTY_MODS";
const BENCH_GIT_WALK_1K_COMMITS_ENV: &str = "SPUR_GRAPH_BENCH_GIT_WALK_1K_COMMITS";
const BENCH_GIT_WALK_20K_COMMITS_ENV: &str = "SPUR_GRAPH_BENCH_GIT_WALK_20K_COMMITS";
const QUICK_GIT_WALK_1K_COMMITS: usize = 100;
const QUICK_GIT_WALK_20K_COMMITS: usize = 250;
const SYNTHETIC_SYMBOL_COUNT: usize = 16;
const SYNTHETIC_RENAME_RATE: f32 = 0.04;
#[cfg(test)]
#[allow(dead_code)]
const SYNTHETIC_BUDGET_MERGE_DENSITY: f32 = 0.90;
const SYNTHETIC_START_TIME: i64 = 1_700_000_000;

#[derive(Clone, Copy, Debug, Default)]
struct PhaseTimes {
    total: Duration,
    ls_files: Duration,
    blake3: Duration,
    extraction: Duration,
}

impl PhaseTimes {
    fn checked_avg(self, iters: u64) -> Self {
        if iters == 0 {
            return self;
        }
        Self {
            total: self.total / iters as u32,
            ls_files: self.ls_files / iters as u32,
            blake3: self.blake3 / iters as u32,
            extraction: self.extraction / iters as u32,
        }
    }
}

impl std::ops::AddAssign for PhaseTimes {
    fn add_assign(&mut self, rhs: Self) {
        self.total += rhs.total;
        self.ls_files += rhs.ls_files;
        self.blake3 += rhs.blake3;
        self.extraction += rhs.extraction;
    }
}

#[derive(Clone, Debug)]
struct SummaryRow {
    scenario: &'static str,
    files: usize,
    changed_paths: usize,
    cache_mode: &'static str,
    phases: PhaseTimes,
}

#[derive(Clone, Debug)]
struct ContentEntry {
    path: String,
    content_oid: String,
    extractable: bool,
}

struct SyntheticRepo {
    _temp: TempDir,
    root: PathBuf,
    git_common_dir: PathBuf,
}

impl SyntheticRepo {
    fn new(file_count: usize, label: &str) -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join(label);
        fs::create_dir(&root).expect("create repo dir");
        run_git(&root, &["init"]);
        run_git(
            &root,
            &["config", "user.email", "spur-graph@example.invalid"],
        );
        run_git(&root, &["config", "user.name", "Spur Graph Benchmark"]);
        run_git(&root, &["config", "core.autocrlf", "false"]);
        run_git(&root, &["config", "gc.auto", "0"]);

        write_rust_files(&root, 0, file_count, 0);
        run_git(&root, &["add", "src"]);
        run_git(&root, &["commit", "-m", "baseline"]);

        let git_common_dir = git_stdout(
            &root,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        );
        Self {
            _temp: temp,
            root,
            git_common_dir: PathBuf::from(git_common_dir.trim_end()),
        }
    }

    fn baseline(&self) -> BTreeMap<String, String> {
        let entries = discover_content_entries(&self.root)
            .expect("discover baseline content entries")
            .entries;
        entries
            .into_iter()
            .map(|entry| (entry.path, entry.content_oid))
            .collect()
    }

    fn commit_rewrites(&self, count: usize) {
        write_rust_files(&self.root, 0, count, 1);
        run_git(&self.root, &["add", "src"]);
        run_git(&self.root, &["commit", "-m", "rewrite change set"]);
    }

    fn dirty_rewrites(&self, count: usize) {
        write_rust_files(&self.root, 0, count, 2);
    }

    fn canonical_dir(&self) -> PathBuf {
        self.git_common_dir
            .join("spur-graph")
            .join("artifacts")
            .join(current_manifest_version())
    }

    fn clear_canonical(&self) {
        let dir = self.canonical_dir();
        if dir.exists() {
            fs::remove_dir_all(&dir).expect("clear canonical benchmark dir");
        }
    }

    fn canonical_marker(&self, graph_hash: &str) -> PathBuf {
        self.canonical_dir().join(format!("{graph_hash}.json"))
    }

    fn ensure_marker(&self, graph_hash: &str) {
        let marker = self.canonical_marker(graph_hash);
        if let Some(parent) = marker.parent() {
            fs::create_dir_all(parent).expect("create canonical benchmark dir");
        }
        fs::write(marker, b"benchmark cache marker\n").expect("write canonical marker");
    }

    fn marker_exists(&self, graph_hash: &str) -> bool {
        self.canonical_marker(graph_hash).exists()
    }
}

struct DiscoveryResult {
    entries: Vec<ContentEntry>,
    ls_files: Duration,
}

#[derive(Clone, Debug)]
struct SyntheticSymbol {
    slot: usize,
    name: String,
    generation: usize,
    rename_generation: usize,
}

#[derive(Clone, Debug)]
struct SyntheticRng {
    state: u64,
}

struct FastImportCommit<'a> {
    branch: &'a str,
    mark: usize,
    parent: Option<usize>,
    merges: &'a [usize],
    summary: &'a str,
    lib_rs: &'a str,
    commit_index: usize,
}

impl SyntheticRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        self.state
    }

    fn usize(&mut self, upper: usize) -> usize {
        assert!(upper > 0, "upper bound must be non-zero");
        (self.next_u64() as usize) % upper
    }

    fn chance(&mut self, probability: f32) -> bool {
        let probability = probability.clamp(0.0, 1.0) as f64;
        let sample = ((self.next_u64() >> 11) as f64) / ((1_u64 << 53) as f64);
        sample < probability
    }
}

fn build_synthetic_repo(dir: &Path, n_commits: usize, merge_density: f32) -> Vec<String> {
    assert!(n_commits > 0, "synthetic repo must contain commits");
    fs::create_dir_all(dir).expect("create synthetic repo dir");
    run_git(dir, &["init"]);
    run_git(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    run_git(dir, &["config", "user.email", "spur-graph@example.invalid"]);
    run_git(dir, &["config", "user.name", "Spur Graph Benchmark"]);
    run_git(dir, &["config", "core.autocrlf", "false"]);
    run_git(dir, &["config", "gc.auto", "0"]);

    let mut importer = Command::new("git")
        .args(["fast-import", "--quiet", "--date-format=raw"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn git fast-import");

    {
        let stdin = importer.stdin.take().expect("git fast-import stdin");
        let mut stream = BufWriter::new(stdin);
        let mut rng = SyntheticRng::new(0x005e_ed5e_ed14);
        let mut symbols = initial_synthetic_symbols();
        let mut commit_count = 0usize;
        let mut next_mark = 1usize;

        let baseline = synthetic_rust_source(&symbols);
        write_fast_import_commit(
            &mut stream,
            FastImportCommit {
                branch: "refs/heads/main",
                mark: next_mark,
                parent: None,
                merges: &[],
                summary: "synthetic baseline",
                lib_rs: &baseline,
                commit_index: commit_count,
            },
        );
        let mut main_mark = next_mark;
        next_mark += 1;
        commit_count += 1;

        while commit_count < n_commits {
            let can_merge = commit_count + 2 <= n_commits;
            if can_merge && rng.chance(merge_density) {
                let mut side_symbols = symbols.clone();
                mutate_synthetic_symbol(&mut side_symbols, &mut rng);
                let side_mark = next_mark;
                let side_ref = format!("refs/heads/spur-synthetic-side/{side_mark}");
                let side_source = synthetic_rust_source(&side_symbols);
                let side_summary = format!("synthetic side churn {side_mark}");
                write_fast_import_commit(
                    &mut stream,
                    FastImportCommit {
                        branch: &side_ref,
                        mark: side_mark,
                        parent: Some(main_mark),
                        merges: &[],
                        summary: &side_summary,
                        lib_rs: &side_source,
                        commit_index: commit_count,
                    },
                );
                next_mark += 1;
                commit_count += 1;

                let merge_mark = next_mark;
                let merge_summary = format!("merge synthetic side {side_mark}");
                write_fast_import_commit(
                    &mut stream,
                    FastImportCommit {
                        branch: "refs/heads/main",
                        mark: merge_mark,
                        parent: Some(main_mark),
                        merges: &[side_mark],
                        summary: &merge_summary,
                        lib_rs: &side_source,
                        commit_index: commit_count,
                    },
                );
                next_mark += 1;
                commit_count += 1;
                main_mark = merge_mark;
                symbols = side_symbols;
            } else {
                mutate_synthetic_symbol(&mut symbols, &mut rng);
                let source = synthetic_rust_source(&symbols);
                let summary = format!("synthetic churn {next_mark}");
                write_fast_import_commit(
                    &mut stream,
                    FastImportCommit {
                        branch: "refs/heads/main",
                        mark: next_mark,
                        parent: Some(main_mark),
                        merges: &[],
                        summary: &summary,
                        lib_rs: &source,
                        commit_index: commit_count,
                    },
                );
                main_mark = next_mark;
                next_mark += 1;
                commit_count += 1;
            }
        }

        stream.write_all(b"done\n").expect("finish fast-import");
        stream.flush().expect("flush fast-import stream");
    }

    let output = importer
        .wait_with_output()
        .expect("wait for git fast-import");
    assert!(
        output.status.success(),
        "git fast-import failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    run_git(dir, &["checkout", "-q", "-f", "main"]);
    let shas: Vec<_> = git_stdout(dir, &["rev-list", "--topo-order", "--reverse", "main"])
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    assert_eq!(shas.len(), n_commits, "synthetic commit count mismatch");
    shas
}

fn initial_synthetic_symbols() -> Vec<SyntheticSymbol> {
    (0..SYNTHETIC_SYMBOL_COUNT)
        .map(|slot| SyntheticSymbol {
            slot,
            name: format!("symbol_{slot:03}"),
            generation: 0,
            rename_generation: 0,
        })
        .collect()
}

fn mutate_synthetic_symbol(symbols: &mut [SyntheticSymbol], rng: &mut SyntheticRng) {
    let index = rng.usize(symbols.len());
    let symbol = &mut symbols[index];
    symbol.generation += 1;
    if rng.chance(SYNTHETIC_RENAME_RATE) {
        symbol.rename_generation += 1;
        symbol.name = format!("symbol_{:03}_r{:03}", symbol.slot, symbol.rename_generation);
    }
}

fn synthetic_rust_source(symbols: &[SyntheticSymbol]) -> String {
    let mut source = String::with_capacity(symbols.len() * 240);
    source.push_str("// Synthetic Rust fixture for spur-graph temporal walk benches.\n\n");
    for (index, symbol) in symbols.iter().enumerate() {
        let callee = &symbols[(index + 1) % symbols.len()].name;
        let rotate = (symbol.generation % 31) + 1;
        let salt = symbol.slot.wrapping_mul(97) + symbol.generation.wrapping_mul(31);
        source.push_str(&format!(
            "pub fn {}(input: usize) -> usize {{\n    let base = input.wrapping_add({}usize).rotate_left({});\n    let churn = base ^ {}usize;\n    if input == 0 {{\n        churn\n    }} else {{\n        churn.wrapping_add({}(input - 1))\n    }}\n}}\n\n",
            symbol.name,
            symbol.slot + symbol.generation,
            rotate,
            salt,
            callee
        ));
    }
    source
}

fn write_fast_import_commit<W: Write>(stream: &mut W, commit: FastImportCommit<'_>) {
    let timestamp = SYNTHETIC_START_TIME + commit.commit_index as i64;
    writeln!(stream, "commit {}", commit.branch).expect("write fast-import commit");
    writeln!(stream, "mark :{}", commit.mark).expect("write fast-import mark");
    writeln!(
        stream,
        "author Spur Synthetic <spur-graph@example.invalid> {timestamp} +0000"
    )
    .expect("write fast-import author");
    writeln!(
        stream,
        "committer Spur Synthetic <spur-graph@example.invalid> {timestamp} +0000"
    )
    .expect("write fast-import committer");
    write_fast_import_data(stream, commit.summary.as_bytes());
    if let Some(parent) = commit.parent {
        writeln!(stream, "from :{parent}").expect("write fast-import parent");
    }
    for merge in commit.merges {
        writeln!(stream, "merge :{merge}").expect("write fast-import merge parent");
    }
    writeln!(stream, "M 100644 inline src/lib.rs").expect("write fast-import file command");
    write_fast_import_data(stream, commit.lib_rs.as_bytes());
    writeln!(stream).expect("separate fast-import commit");
}

fn write_fast_import_data<W: Write>(stream: &mut W, bytes: &[u8]) {
    writeln!(stream, "data {}", bytes.len()).expect("write fast-import data header");
    stream.write_all(bytes).expect("write fast-import data");
    stream
        .write_all(b"\n")
        .expect("terminate fast-import data block");
}

fn benchmark_incremental(c: &mut Criterion) {
    if !criterion_filter_allows(&[
        "incremental",
        "clean cold",
        "clean warm",
        "clean change set",
        "dirty unstaged mods",
    ]) {
        return;
    }

    let file_count = env_usize(BENCH_FILE_COUNT_ENV, DEFAULT_FILE_COUNT);
    let change_set = env_usize(BENCH_CHANGE_SET_ENV, DEFAULT_CHANGE_SET).min(file_count);
    let dirty_mods = env_usize(BENCH_DIRTY_MODS_ENV, DEFAULT_DIRTY_MODS).min(file_count);

    eprintln!(
        "spur-graph incremental benchmark fixture: files={file_count}, change_set={change_set}, dirty_mods={dirty_mods}"
    );

    let clean = SyntheticRepo::new(file_count, "clean");
    let clean_hash = graph_hash_for_setup(&clean.root);
    clean.ensure_marker(&clean_hash);

    let changed = SyntheticRepo::new(file_count, "changed");
    let changed_baseline = changed.baseline();
    changed.commit_rewrites(change_set);

    let dirty = SyntheticRepo::new(file_count, "dirty");
    let dirty_baseline = dirty.baseline();
    dirty.dirty_rewrites(dirty_mods);

    let rows: Arc<Mutex<Vec<SummaryRow>>> = Arc::default();
    let mut group = c.benchmark_group("incremental");

    bench_scenario(
        &mut group,
        rows.clone(),
        SummaryRow {
            scenario: "clean cold",
            files: file_count,
            changed_paths: file_count,
            cache_mode: "miss",
            phases: PhaseTimes::default(),
        },
        || clean.clear_canonical(),
        || measure_build(&clean, None, true),
    );

    bench_scenario(
        &mut group,
        rows.clone(),
        SummaryRow {
            scenario: "clean warm",
            files: file_count,
            changed_paths: 0,
            cache_mode: "hit",
            phases: PhaseTimes::default(),
        },
        || clean.ensure_marker(&clean_hash),
        || measure_build(&clean, None, false),
    );

    bench_scenario(
        &mut group,
        rows.clone(),
        SummaryRow {
            scenario: "clean change set",
            files: file_count,
            changed_paths: change_set,
            cache_mode: "miss",
            phases: PhaseTimes::default(),
        },
        || changed.clear_canonical(),
        || measure_build(&changed, Some(&changed_baseline), true),
    );

    bench_scenario(
        &mut group,
        rows.clone(),
        SummaryRow {
            scenario: "dirty unstaged mods",
            files: file_count,
            changed_paths: dirty_mods,
            cache_mode: "miss",
            phases: PhaseTimes::default(),
        },
        || dirty.clear_canonical(),
        || measure_build(&dirty, Some(&dirty_baseline), true),
    );

    group.finish();
    print_summary(&rows.lock().expect("summary rows").clone());
}

fn bench_full_walk_1k(c: &mut Criterion) {
    const BENCH_NAME: &str = "git_walk full 1k linear";
    if !criterion_filter_allows(&[BENCH_NAME]) {
        return;
    }

    let dir = tempfile::TempDir::new().unwrap();
    let commit_count = synthetic_bench_commits(
        BENCH_GIT_WALK_1K_COMMITS_ENV,
        1_000,
        QUICK_GIT_WALK_1K_COMMITS,
    );
    eprintln!("{BENCH_NAME} synthetic commits={commit_count}");
    let shas = build_synthetic_repo(dir.path(), commit_count, 0.0);
    c.bench_function(BENCH_NAME, |b| {
        b.iter(|| {
            let (graph, commits) = spur_graph::git_walk::run_full_walk_into(
                dir.path(),
                &spur_graph::git_walk::GitWalkConfig::default(),
            )
            .unwrap();
            black_box((
                graph.symbol_snapshots.len(),
                graph.temporal_edges.len(),
                commits.commits.len(),
                shas.len(),
            ));
        })
    });
}

fn bench_full_walk_20k_merges(c: &mut Criterion) {
    const BENCH_NAME: &str = "git_walk full 20k merges";
    if !criterion_filter_allows(&[BENCH_NAME]) {
        return;
    }

    let dir = tempfile::TempDir::new().unwrap();
    let commit_count = synthetic_bench_commits(
        BENCH_GIT_WALK_20K_COMMITS_ENV,
        20_000,
        QUICK_GIT_WALK_20K_COMMITS,
    );
    eprintln!("{BENCH_NAME} synthetic commits={commit_count}");
    let shas = build_synthetic_repo(dir.path(), commit_count, 0.30);
    c.bench_function(BENCH_NAME, |b| {
        b.iter(|| {
            let (graph, commits) = spur_graph::git_walk::run_full_walk_into(
                dir.path(),
                &spur_graph::git_walk::GitWalkConfig::default(),
            )
            .unwrap();
            black_box((
                graph.symbol_snapshots.len(),
                graph.temporal_edges.len(),
                commits.commits.len(),
                shas.len(),
            ));
        })
    });
}

fn synthetic_bench_commits(env_name: &str, default: usize, quick_default: usize) -> usize {
    let default = if criterion_quick_mode() {
        quick_default
    } else {
        default
    };
    env_usize(env_name, default)
}

fn criterion_quick_mode() -> bool {
    env::args().any(|arg| arg == "--quick")
}

fn criterion_filter_allows(candidates: &[&str]) -> bool {
    let filters: Vec<_> = env::args()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    filters.is_empty()
        || filters.iter().any(|filter| {
            candidates
                .iter()
                .any(|candidate| candidate.contains(filter))
        })
}

fn bench_scenario<Setup, Measure>(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    rows: Arc<Mutex<Vec<SummaryRow>>>,
    row: SummaryRow,
    mut setup: Setup,
    mut measure: Measure,
) where
    Setup: FnMut(),
    Measure: FnMut() -> anyhow::Result<PhaseTimes>,
{
    group.bench_with_input(BenchmarkId::new(row.scenario, row.files), &row, |b, row| {
        b.iter_custom(|iters| {
            let mut total = PhaseTimes::default();
            for _ in 0..iters {
                setup();
                let phases = measure().expect("measure spur-graph incremental scenario");
                total += phases;
            }
            let averaged = total.checked_avg(iters);
            upsert_summary(
                &rows,
                SummaryRow {
                    phases: averaged,
                    ..row.clone()
                },
            );
            total.total
        });
    });
}

fn measure_build(
    repo: &SyntheticRepo,
    baseline: Option<&BTreeMap<String, String>>,
    populate_cache_on_miss: bool,
) -> anyhow::Result<PhaseTimes> {
    let total_start = Instant::now();
    let discovery = discover_content_entries(&repo.root)?;

    let blake3_start = Instant::now();
    let graph_hash = compute_graph_content_hash(
        discovery
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.content_oid.as_str())),
    );
    let blake3 = blake3_start.elapsed();

    let cache_hit = repo.marker_exists(&graph_hash);
    let mut extraction = Duration::ZERO;
    if !cache_hit {
        let extract_paths = changed_extract_paths(&repo.root, &discovery.entries, baseline);
        let extraction_start = Instant::now();
        let facts = build_facts_for_paths(&repo.root, &extract_paths)?;
        black_box(facts.nodes.len() + facts.edges.len() + facts.spans.len());
        extraction = extraction_start.elapsed();
        if populate_cache_on_miss {
            repo.ensure_marker(&graph_hash);
        }
    }

    Ok(PhaseTimes {
        total: total_start.elapsed(),
        ls_files: discovery.ls_files,
        blake3,
        extraction,
    })
}

fn discover_content_entries(root: &Path) -> anyhow::Result<DiscoveryResult> {
    let dirty_entries = spur_graph::git::status_dirty_paths(root)?;
    let dirty_paths: BTreeMap<String, spur_graph::DirtyEntry> = dirty_entries
        .into_iter()
        .filter(|entry| is_rust_path(&entry.path))
        .map(|entry| (entry.path.clone(), entry))
        .collect();

    let ls_start = Instant::now();
    let tracked = spur_graph::git::ls_files_with_oids(root)?;
    let ls_files = ls_start.elapsed();

    let mut entries = BTreeMap::new();
    for tracked in tracked {
        if !is_rust_path(&tracked.path) {
            continue;
        }
        let content_oid = if tracked.is_gitlink {
            tracked.content_oid
        } else if dirty_paths.contains_key(&tracked.path) {
            match read_worktree_content_oid(root, &tracked.path)? {
                Some(content_oid) => content_oid,
                None => continue,
            }
        } else {
            tracked.content_oid
        };
        entries.insert(
            tracked.path.clone(),
            ContentEntry {
                path: tracked.path,
                content_oid,
                extractable: !tracked.is_gitlink,
            },
        );
    }

    for dirty in dirty_paths.values() {
        if entries.contains_key(&dirty.path) {
            continue;
        }
        let Some(content_oid) = read_worktree_content_oid(root, &dirty.path)? else {
            continue;
        };
        entries.insert(
            dirty.path.clone(),
            ContentEntry {
                path: dirty.path.clone(),
                content_oid,
                extractable: true,
            },
        );
    }

    Ok(DiscoveryResult {
        entries: entries.into_values().collect(),
        ls_files,
    })
}

fn changed_extract_paths(
    root: &Path,
    entries: &[ContentEntry],
    baseline: Option<&BTreeMap<String, String>>,
) -> Vec<PathBuf> {
    entries
        .iter()
        .filter(|entry| entry.extractable)
        .filter(|entry| {
            baseline.is_none_or(|baseline| {
                baseline
                    .get(&entry.path)
                    .is_none_or(|content_oid| content_oid != &entry.content_oid)
            })
        })
        .map(|entry| root.join(&entry.path))
        .collect()
}

fn graph_hash_for_setup(root: &Path) -> String {
    let entries = discover_content_entries(root)
        .expect("discover setup content entries")
        .entries;
    compute_graph_content_hash(
        entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.content_oid.as_str())),
    )
}

fn read_worktree_content_oid(root: &Path, path: &str) -> anyhow::Result<Option<String>> {
    match fs::read(root.join(path)) {
        Ok(bytes) => Ok(Some(spur_graph::git_blob_oid(&bytes))),
        Err(err) if matches!(err.kind(), std::io::ErrorKind::NotFound) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn write_rust_files(root: &Path, start: usize, count: usize, generation: usize) {
    for index in start..start + count {
        let path = rust_file_path(root, index);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture dir");
        }
        fs::write(path, rust_source(index, generation)).expect("write rust fixture");
    }
}

fn rust_file_path(root: &Path, index: usize) -> PathBuf {
    root.join("src")
        .join(format!("mod_{:03}", index / 1_000))
        .join(format!("file_{index:06}.rs"))
}

fn rust_source(index: usize, generation: usize) -> String {
    format!("pub fn f_{index:06}() -> usize {{\n    {index}usize + {generation}usize\n}}\n")
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn is_rust_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("rs"))
}

fn upsert_summary(rows: &Arc<Mutex<Vec<SummaryRow>>>, row: SummaryRow) {
    let mut rows = rows.lock().expect("summary rows");
    if let Some(existing) = rows
        .iter_mut()
        .find(|existing| existing.scenario == row.scenario)
    {
        *existing = row;
    } else {
        rows.push(row);
    }
}

fn print_summary(rows: &[SummaryRow]) {
    eprintln!();
    eprintln!("spur-graph incremental benchmark summary");
    eprintln!("| scenario | files | changed | cache | total | ls-files | blake3 | extraction |");
    eprintln!("|---|---:|---:|---|---:|---:|---:|---:|");
    for row in rows {
        eprintln!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            row.scenario,
            row.files,
            row.changed_paths,
            row.cache_mode,
            fmt_duration(row.phases.total),
            fmt_duration(row.phases.ls_files),
            fmt_duration(row.phases.blake3),
            fmt_duration(row.phases.extraction)
        );
    }
}

fn fmt_duration(duration: Duration) -> String {
    let nanos = duration.as_nanos();
    if nanos >= 1_000_000_000 {
        format!("{:.3}s", duration.as_secs_f64())
    } else if nanos >= 1_000_000 {
        format!("{:.3}ms", nanos as f64 / 1_000_000.0)
    } else if nanos >= 1_000 {
        format!("{:.3}us", nanos as f64 / 1_000.0)
    } else {
        format!("{nanos}ns")
    }
}

fn run_git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?}: {err}"));
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git stdout UTF-8")
}

#[test]
fn snapshot_growth_budget() {
    let d1 = tempfile::TempDir::new().unwrap();
    build_synthetic_repo(d1.path(), 1_000, SYNTHETIC_BUDGET_MERGE_DENSITY);
    let budget_config = spur_graph::git_walk::GitWalkConfig {
        walk_strategy: spur_graph::WalkStrategy::FirstParent,
        ..spur_graph::git_walk::GitWalkConfig::default()
    };
    let (g1, _) = spur_graph::git_walk::run_full_walk_into(d1.path(), &budget_config).unwrap();

    let d2 = tempfile::TempDir::new().unwrap();
    build_synthetic_repo(d2.path(), 10_000, SYNTHETIC_BUDGET_MERGE_DENSITY);
    let (g2, _) = spur_graph::git_walk::run_full_walk_into(d2.path(), &budget_config).unwrap();

    let ratio = g2.symbol_snapshots.len() as f64 / g1.symbol_snapshots.len() as f64;
    eprintln!(
        "snapshot_growth_budget: 1k={} 10k={} ratio={ratio:.3}",
        g1.symbol_snapshots.len(),
        g2.symbol_snapshots.len()
    );
    assert!(
        ratio <= 1.5 * 10.0,
        "snapshot growth {} > 1.5x/10x budget; needs sharded persistence before merge",
        ratio
    );
}

criterion_group!(
    benches,
    benchmark_incremental,
    bench_full_walk_1k,
    bench_full_walk_20k_merges
);
criterion_main!(benches);

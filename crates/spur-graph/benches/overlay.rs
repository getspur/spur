//! OverlayClient vs direct ParquetClient on the live graph artifact.
//!
//! Opens `.spur/graph/CURRENT` (override with `SPUR_GRAPH_PERF_FIXTURE`).
//! Does not rebuild facts — this is a query-path comparison, not an index build.

use std::path::{Path, PathBuf};
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use spur_graph::{
    GraphQueryClient, OverlayClient, ParquetClient, SearchFilters, SearchMode, SearchOptions,
};

fn bench_overlay_vs_direct_parquet(c: &mut Criterion) {
    let parquet_dir = live_parquet_dir();
    let parquet = ParquetClient::open(&parquet_dir)
        .unwrap_or_else(|err| panic!("open live parquet `{}`: {err:#}", parquet_dir.display()));
    let repo = repo_root();
    let overlay_empty =
        OverlayClient::new(&parquet, &repo, &[]).expect("empty overlay over live parquet");
    let overlay_one_file = OverlayClient::new(
        &parquet,
        &repo,
        &[PathBuf::from("crates/spur-graph/src/mcp/mod.rs")],
    )
    .expect("one-file overlay over live parquet");

    let search_base = SearchOptions {
        query: "handle_code_search".to_owned(),
        mode: SearchMode::Exact,
        filters: SearchFilters::default(),
        limit: 20,
    };
    let search_overlay_hit = SearchOptions {
        query: "overlay_client_for_backend".to_owned(),
        mode: SearchMode::Exact,
        filters: SearchFilters::default(),
        limit: 20,
    };
    let symbol_id = parquet
        .search_symbols(&search_base)
        .expect("parquet search for callers target")
        .candidates
        .first()
        .expect("handle_code_search exists in live parquet")
        .stable_symbol_id
        .clone();

    let mut group = c.benchmark_group("bench_overlay_vs_direct_parquet");
    group.sample_size(20);
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
                .resolve_selector(black_box("handle_code_search"))
                .expect("parquet resolve");
            black_box(resolution);
        });
    });
    group.bench_function("overlay_empty_resolve", |b| {
        b.iter(|| {
            let resolution = overlay_empty
                .resolve_selector(black_box("handle_code_search"))
                .expect("empty overlay resolve");
            black_box(resolution);
        });
    });
    group.bench_function("parquet_cached_file_symbols_small", |b| {
        b.iter(|| {
            let symbols = parquet
                .symbols_by_file(black_box("crates/spur-graph/src/lib.rs"))
                .expect("parquet file symbols");
            black_box(symbols);
        });
    });
    group.bench_function("overlay_empty_file_symbols_small", |b| {
        b.iter(|| {
            let symbols = overlay_empty
                .symbols_by_file(black_box("crates/spur-graph/src/lib.rs"))
                .expect("empty overlay file symbols");
            black_box(symbols);
        });
    });
    group.bench_function("parquet_cached_file_symbols_large", |b| {
        b.iter(|| {
            let symbols = parquet
                .symbols_by_file(black_box("crates/spur-graph/src/mcp/mod.rs"))
                .expect("parquet large file symbols");
            black_box(symbols);
        });
    });
    group.bench_function("overlay_one_file_file_symbols_large", |b| {
        b.iter(|| {
            let symbols = overlay_one_file
                .symbols_by_file(black_box("crates/spur-graph/src/mcp/mod.rs"))
                .expect("one-file overlay large file symbols");
            black_box(symbols);
        });
    });
    group.finish();
}

fn bench_overlay_construction(c: &mut Criterion) {
    let parquet_dir = live_parquet_dir();
    let repo = repo_root();
    let one_file = [PathBuf::from("crates/spur-graph/src/mcp/mod.rs")];
    let parquet = ParquetClient::open(&parquet_dir)
        .unwrap_or_else(|err| panic!("open live parquet `{}`: {err:#}", parquet_dir.display()));
    let (empty_artifact, empty_shadowed) =
        OverlayClient::<&ParquetClient>::extract_delta(&repo, &[])
            .expect("extract empty overlay delta");
    let (one_artifact, one_shadowed) =
        OverlayClient::<&ParquetClient>::extract_delta(&repo, &one_file)
            .expect("extract one-file overlay delta");

    let mut group = c.benchmark_group("bench_overlay_construction");
    group.sample_size(10);
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

fn live_parquet_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("SPUR_GRAPH_PERF_FIXTURE") {
        return PathBuf::from(path);
    }
    let current = repo_root().join(".spur/graph/CURRENT");
    if current.exists() {
        return current
            .canonicalize()
            .unwrap_or_else(|err| panic!("canonicalize `{}`: {err}", current.display()));
    }
    panic!("no live parquet at `.spur/graph/CURRENT`; set SPUR_GRAPH_PERF_FIXTURE");
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("canonicalize repo root from spur-graph manifest dir")
}

criterion_group!(
    benches,
    bench_overlay_vs_direct_parquet,
    bench_overlay_construction
);
criterion_main!(benches);

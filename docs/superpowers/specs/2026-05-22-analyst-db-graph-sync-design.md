# Analyst DuckDB ↔ Spur Graph Sync — Design

**Date:** 2026-05-22
**Status:** Draft (brainstormed; pending implementation plan)
**Scope:** `crates/spur-cli` (`graph` and new `analyst` subcommands), `crates/spur-context/poc/duckdb-analyst/setup.sh`

## Problem

`spur-cli graph build` writes parquet artifacts and updates `.spur/graph/CURRENT` + `.spur/graph-index.pointer.json`. A separate `crates/spur-context/poc/duckdb-analyst/setup.sh` reads `CURRENT` and rebuilds `.spur/analyst.duckdb` from those parquets.

The two artifacts drift whenever `graph build` runs without `setup.sh` running after it. Workers querying the analyst DB via MCP, and local dev sessions, see stale schema/data.

## Goal & Invariant

Make `.spur/analyst.duckdb` a deterministic by-product of `spur-cli graph build`. No separate manual step required.

**Invariant:** *If `.spur/graph-index.pointer.json` exists, then `.spur/analyst.duckdb` exists and was built from the parquet artifact that pointer resolves to.*

Exception: explicit opt-out (`--no-analyst` / `SPUR_GRAPH_SKIP_ANALYST=1`) or soft-fail when `duckdb` CLI is unavailable.

## Approach

**Push (eager) only.** `spur-cli graph build` calls a new `analyst::build` step as its final action on the success path. No pull-side hash validation in v1.

## Architecture

### New Rust subcommand: `spur-cli analyst build`

Location: `crates/spur-cli/src/commands/analyst.rs` (new file).

Responsibilities, in order:

1. Resolve artifact directory: `--artifact-dir` flag → `SPUR_GRAPH_ARTIFACT_DIR` env → `.spur/graph/CURRENT` symlink → error.
2. Verify required parquet files exist (same list as current `setup.sh`: `nodes.parquet`, `edges.parquet`, `edges_by_dst.parquet`, `edges_unresolved.parquet`, `files.parquet`, `file_manifests.parquet`, `tombstones.parquet`, `manifest.json`).
3. **Schema-version gate.** Read `manifest.json` and verify `schema_version` matches a constant compiled into the Rust binary (`SUPPORTED_GRAPH_SCHEMA_VERSION`, currently `"spur-graph-schema-v5"`). On mismatch, return a hard error with the expected vs. observed schema version. This prevents silent miscompiles where `init.sql` view definitions still parse but produce wrong results against a newer parquet schema.
4. Check `duckdb` on PATH. If absent, log warning and exit 0 (soft-fail).
5. Acquire non-blocking `flock` on `.spur/analyst.duckdb.lock` using the shared helper (see Locking below). If held, log "another analyst build in progress, skipping" and exit 0.
6. Read `crates/spur-context/poc/duckdb-analyst/init.sql`, substitute `__SPUR_GRAPH_ARTIFACT_DIR__` placeholder, pipe to `duckdb` writing to `.spur/analyst.duckdb.tmp-<pid>`.
7. On `duckdb` success, `rename(2)` tmp file over `.spur/analyst.duckdb`.
8. On `duckdb` failure, remove tmp file, log warning, exit 0 (previous DB stays intact).
9. Release lock. Lock file is **not** deleted (avoids unlink race; relevant given the open flock-leak RCA at `docs/rca/2026-05-18-flock-leak-pid14282/`).

`init.sql` remains the canonical source of view definitions. The Rust subcommand orchestrates path resolution, locking, atomic rename — it does not embed SQL.

### Locking — reuse existing helper

`crates/spur-graph/src/store/cache.rs:91-107` already implements `try_lock_exclusive_with_timeout` using `fs2::FileExt::try_lock_exclusive` + a private `is_lock_contended` predicate that correctly handles `WouldBlock` across platforms. Promote both to `pub(crate)` (or to a small public module like `spur_graph::locking`) and reuse them in `analyst::build`. This keeps all flock discipline consolidated in one place — important context given the open flock-leak RCA at `docs/rca/2026-05-18-flock-leak-pid14282/`.

Do **not** re-implement `WouldBlock` detection in `analyst.rs`. macOS and Linux disagree on the underlying errno; the existing helper is the single source of truth.

### DuckDB extension first-run cost (known operational gotcha)

`init.sql:31-34` runs `INSTALL duckpgq FROM community` and `INSTALL onager FROM community`. On a host with no cached community extensions, **the first run downloads them and requires network access**. Realistic timings: 5-30s on first run, sub-second on subsequent runs. Implications:

- The "Analyst DB ready" timing log will be large on first build per host — that's expected, not a bug.
- A worker in an air-gapped sandbox or with no extension cache hits the soft-degrade path on every build until the cache is populated. The warning should be informative enough that the operator recognizes the network/cache root cause rather than treating it as a flaky test.
- v1 does not attempt to pre-warm or vendor the extensions. If recurring soft-degrade in worker contexts becomes a problem, a follow-up can ship a pre-warmed extension cache or add an `INSTALL`-skip path that checks `duckdb_extensions()` first.

### Wiring into `graph build`

Insertion point is **between** the existing `result?;` (current `commands/graph.rs:200`) and the existing summary `println!` (current `commands/graph.rs:214-223`). This ordering matters:

- After `result?;` — the parquet artifacts and `.spur/graph/CURRENT` pointer have been successfully written; safe to read them.
- Before the summary `println!` — so the graph build summary always prints, and so the analyst step's own output is visually distinct from the graph build summary.

```rust
// commands/graph.rs::build, immediately after `result?;` and before the
// "[spur] Graph index built: …" println!:
if !options.skip_analyst {
    // Hard-fail errors propagate; soft-fail and soft-degrade are converted
    // to Ok(()) inside analyst::build with a warning logged. The summary
    // println! below still runs in all non-hard-fail cases.
    crate::commands::analyst::build_default(&root, options.quiet)?;
}
```

Skip when `options.skip_analyst == true` (set by `--no-analyst`) or when `SPUR_GRAPH_SKIP_ANALYST=1` is in the environment. Honor `options.quiet` for output suppression to match `graph build --quiet` semantics.

### `setup.sh` becomes a shim

`crates/spur-context/poc/duckdb-analyst/setup.sh` is reduced to:

```sh
#!/usr/bin/env bash
set -euo pipefail
exec scripts/spur-cargo run --quiet -p spur-cli -- analyst build "$@"
```

Kept as the documented entry point in the README; underlying logic lives in Rust.

### Dependencies

- File lock: **reuse** `try_lock_exclusive_with_timeout` from `crates/spur-graph/src/store/cache.rs:91-107` (promote to `pub(crate)` / shared module). `fs2 = "0.4"` is already a workspace dep (`crates/spur-graph/Cargo.toml:16`, `crates/spur-pm/Cargo.toml:14`). No new crate.
- DuckDB: continue shelling out to the `duckdb` CLI via `std::process::Command`. **Do not** link `duckdb-rs`; keeps build times and binary size unaffected.

## CLI Surface

```
spur-cli analyst build [--artifact-dir <PATH>] [--db-path <PATH>] [--quiet]
```

- `--artifact-dir`: override pointer resolution. Matches `SPUR_GRAPH_ARTIFACT_DIR`.
- `--db-path`: override output path. Matches `SPUR_ANALYST_DB`.
- `--quiet`: suppress non-error output.

```
spur-cli graph build [...existing flags...] [--no-analyst]
```

- `--no-analyst`: skip the analyst rebuild step. Also honored via `SPUR_GRAPH_SKIP_ANALYST=1`.

### Output contract

Success:

```
[spur] Building code graph index for /Volumes/Projects/spur
... (existing graph build output) ...
[spur] Refreshing analyst DB at .spur/analyst.duckdb
[spur] Analyst DB ready (graph_content_hash=3744e65c…, 1.2s)
```

Soft-fail (no duckdb):

```
[spur] warning: 'duckdb' CLI not found on PATH — skipping analyst DB refresh
[spur] hint: brew install duckdb (or set SPUR_GRAPH_SKIP_ANALYST=1 to silence)
```

Soft-degrade (duckdb failed or lock held): warning printed, graph build exits 0.

## Failure Modes

| Category | Triggers | Behavior |
| --- | --- | --- |
| Soft-fail (env) | `duckdb` not on PATH; `--no-analyst`; `SPUR_GRAPH_SKIP_ANALYST=1` | Warn (or silent for opt-out); exit 0; DB left as-is. |
| Hard-fail (logic) | Required parquet missing; `init.sql` missing; non-recoverable lock error; `manifest.json` `schema_version` does not match `SUPPORTED_GRAPH_SCHEMA_VERSION` | Propagate non-zero exit; graph build fails. Indicates a bug or a stale spur-cli vs. parquet schema. |
| Soft-degrade (transient) | `duckdb` subprocess non-zero; lock already held | Warn; exit 0; previous DB preserved. |

**Atomicity:** `.spur/analyst.duckdb` is always either the previous valid DB or the new valid DB. Achieved by tmp-file + `rename(2)`; the live file is never `rm`-ed.

**Concurrency:** non-blocking `flock` on `.spur/analyst.duckdb.lock`. Second concurrent builder skips with warning. Lock file is never unlinked.

## Out of Scope (v1)

- Pull-side hash validation at MCP query time. (Considered and rejected for v1; revisit if drift is observed despite eager push.)
- Incremental analyst rebuild. Full rebuild is cheap because `init.sql` is mostly `CREATE VIEW` over parquet — no heavy ingest.
- Content-addressed analyst DB (per-hash storage under `.git/spur-graph/...`). Single mutable file at `.spur/analyst.duckdb` is sufficient.
- Linking `duckdb-rs`. Subprocess is sufficient and decouples build complexity.

## Testing

Integration tests added to `crates/spur-cli/tests/` (new file `analyst_build_cli.rs` plus an extension to `graph_build_cli.rs`):

1. **`analyst_build_happy_path`** — `graph build` in tmp worktree → `.spur/analyst.duckdb` exists; `_meta.graph_content_hash` matches `.spur/graph-index.pointer.json`.
2. **`analyst_build_soft_fail_missing_duckdb`** — `PATH` stripped of `duckdb`. Graph build exits 0; warning printed; no DB created.
3. **`analyst_build_skipped_by_flag`** — `--no-analyst` and (separately) `SPUR_GRAPH_SKIP_ANALYST=1`. No DB created, no duckdb subprocess spawned.
4. **`analyst_build_atomic_under_failure`** — pre-populate `.spur/analyst.duckdb` with a known-good DB; run `analyst build` against a tampered `init.sql` that makes `duckdb` exit non-zero. Original DB byte-identical afterwards; no leftover tmp files.
5. **`analyst_build_concurrent_skip`** — spawn two `analyst build` processes; exactly one acquires the lock; the other prints "skipping"; both exit 0; final DB is valid.
6. **`graph_build_triggers_analyst`** — extension to existing `graph_build_cli.rs`: post-build assertion that DB exists and hash matches pointer.

7. **`analyst_build_schema_version_mismatch`** — pre-populate a tmp artifact dir with a `manifest.json` whose `schema_version` is `"spur-graph-schema-vNEXT"` (anything other than the constant). Assert `analyst build` exits non-zero with an error mentioning both expected and observed versions, and that no DB file or tmp file is created. Run `graph build` separately with that same tampered manifest and assert graph build also fails (hard-fail propagates).

Manual smoke (documented in plan, not automated):

- `rm -rf .spur && spur-cli graph build --workspace` → DB present, `examples.sql` queries work.
- Edit a Rust file, re-run `graph build` → `_meta.graph_content_hash` advances.

## Files Touched

**New:**
- `crates/spur-cli/src/commands/analyst.rs`
- `crates/spur-cli/tests/analyst_build_cli.rs`
- `docs/superpowers/specs/2026-05-22-analyst-db-graph-sync-design.md` (this file)

**Modified:**
- `crates/spur-cli/src/commands/mod.rs` — register `analyst` module.
- `crates/spur-cli/src/main.rs` — add `analyst build` subcommand parsing; add `--no-analyst` and corresponding `skip_analyst: bool` field to `GraphCommands::Build` (around line 432); thread it into `GraphBuildOptions` at the dispatch site (line 854).
- `crates/spur-cli/src/commands/graph.rs` — add `skip_analyst: bool` to `GraphBuildOptions`; call `analyst::build_default` between the current `result?;` (line 200) and the current summary `println!` (line 214).
- `crates/spur-graph/src/store/cache.rs` — promote `try_lock_exclusive_with_timeout` and `is_lock_contended` to `pub(crate)` (or move to a small public `locking` module) so `spur-cli` can reuse them.
- `crates/spur-cli/tests/graph_build_cli.rs` — add post-build analyst assertions.
- `crates/spur-context/poc/duckdb-analyst/setup.sh` — reduced to thin shim invoking `spur-cli analyst build`.
- `crates/spur-context/poc/duckdb-analyst/README.md` — point at the new subcommand; note that `graph build` is now the canonical entry point; document the duckdb community-extension first-run download cost.

**Unchanged:**
- `crates/spur-context/poc/duckdb-analyst/init.sql` — remains the source of view definitions.

## Appendix: Temporal collections (Phase 1.5)

Phase 1.5 is an analyst DB surfacing layer over optional temporal and diagnostic parquet collections. Detection gates on file presence in `crates/spur-cli/src/commands/analyst.rs`, not on manifest `row_counts`: the temporal SQL fragment is appended when temporal parquet files are present, and the diagnostics SQL fragment is appended only when `diagnostics.parquet` is present. The optional manifest count keys are metadata, not build gates.

The views are conditional. When temporal parquets are absent, `crates/spur-context/poc/duckdb-analyst/init_temporal.sql` is not concatenated, so the database has no `commits`, `symbol_snapshots`, or `temporal_edges` views. When `diagnostics.parquet` is absent, `crates/spur-context/poc/duckdb-analyst/init_diagnostics.sql` is not concatenated, so the database has no `diagnostics` view.

The base `_meta` table remains available from `crates/spur-context/poc/duckdb-analyst/init.sql`. It exposes `commit_count`, `symbol_snapshot_count`, `temporal_edge_count`, and `diagnostic_count` with `TRY_CAST(json_extract(...)) AS BIGINT` against decoded `manifest.json` text, so missing optional `row_counts` keys read as `NULL` instead of causing struct binder errors against older or stripped manifest schemas.

The real parquet writer entry points for these collections are `write_commits`, `write_symbol_snapshots`, `write_temporal_edges`, and `write_diagnostics` in `crates/spur-graph/src/store/parquet.rs` (verified via `code_search` / `code_symbol_info`). The back-compat invariant is covered by `back_compat_loads_artifact_without_temporal_tables` at `crates/spur-graph/src/store/parquet.rs:2606`: artifacts without `commits.parquet`, `symbol_snapshots.parquet`, `temporal_edges.parquet`, or `diagnostics.parquet`, and with those `row_counts` keys removed, must still load.

Stage 2 is explicitly deferred: modeling temporal relationships as a DuckPGQ temporal graph is outside Phase 1.5. Stage 3 is also explicitly deferred: wiring `run_full_walk_into` from `crates/spur-graph/src/git_walk.rs` into `spur-cli graph build` to produce temporal parquets is separate work.

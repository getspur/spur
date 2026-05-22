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
3. Check `duckdb` on PATH. If absent, log warning and exit 0 (soft-fail).
4. Acquire non-blocking `flock` on `.spur/analyst.duckdb.lock`. If held, log "another analyst build in progress, skipping" and exit 0.
5. Read `crates/spur-context/poc/duckdb-analyst/init.sql`, substitute `__SPUR_GRAPH_ARTIFACT_DIR__` placeholder, pipe to `duckdb` writing to `.spur/analyst.duckdb.tmp-<pid>`.
6. On `duckdb` success, `rename(2)` tmp file over `.spur/analyst.duckdb`.
7. On `duckdb` failure, remove tmp file, log warning, exit 0 (previous DB stays intact).
8. Release lock. Lock file is **not** deleted (avoids unlink race; relevant given the open flock-leak RCA at `docs/rca/2026-05-18-flock-leak-pid14282/`).

`init.sql` remains the canonical source of view definitions. The Rust subcommand orchestrates path resolution, locking, atomic rename — it does not embed SQL.

### Wiring into `graph build`

At the end of `crates/spur-cli/src/commands/graph.rs::build`, after the pointer file is written and on the success path only:

```rust
if !options.skip_analyst {
    if let Err(e) = crate::commands::analyst::build_default(&root) {
        // Hard-fail errors propagate; soft-fail/soft-degrade are already
        // converted to Ok(()) inside analyst::build with a warning logged.
        return Err(e);
    }
}
```

Skip when `options.skip_analyst == true` (set by `--no-analyst`) or when `SPUR_GRAPH_SKIP_ANALYST=1` is in the environment.

### `setup.sh` becomes a shim

`crates/spur-context/poc/duckdb-analyst/setup.sh` is reduced to:

```sh
#!/usr/bin/env bash
set -euo pipefail
exec scripts/spur-cargo run --quiet -p spur-cli -- analyst build "$@"
```

Kept as the documented entry point in the README; underlying logic lives in Rust.

### Dependencies

- File lock: prefer existing locking helper in spur-graph if available; otherwise `fs2::FileExt::try_lock_exclusive` (already in the dep graph) or `nix::fcntl::flock`. No new heavyweight dep.
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
| Hard-fail (logic) | Required parquet missing; `init.sql` missing; non-recoverable lock error | Propagate non-zero exit; graph build fails. Indicates a bug. |
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
- `crates/spur-cli/src/main.rs` — add `analyst build` subcommand parsing; add `--no-analyst` to `graph build`.
- `crates/spur-cli/src/commands/graph.rs` — call `analyst::build_default` at end of `build()` success path.
- `crates/spur-cli/tests/graph_build_cli.rs` — add post-build analyst assertions.
- `crates/spur-context/poc/duckdb-analyst/setup.sh` — reduced to thin shim invoking `spur-cli analyst build`.
- `crates/spur-context/poc/duckdb-analyst/README.md` — point at the new subcommand; note that `graph build` is now the canonical entry point.

**Unchanged:**
- `crates/spur-context/poc/duckdb-analyst/init.sql` — remains the source of view definitions.

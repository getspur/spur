# Analyst DuckDB ↔ Spur Graph Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `.spur/analyst.duckdb` a deterministic by-product of `spur-cli graph build`, eagerly rebuilt with atomic semantics, soft-failing when `duckdb` is unavailable, and hard-failing on parquet schema version drift.

**Architecture:** Add a new `spur-cli analyst build` subcommand (Rust module orchestrating path resolution, schema check, flock, duckdb subprocess, atomic tmp+rename). Wire it into the success path of `spur-cli graph build` between the existing `result?;` and the summary `println!`. `init.sql` remains the single source of view definitions. Reuse the existing `try_lock_exclusive_with_timeout` flock helper from `spur-graph`.

**Tech Stack:** Rust (anyhow, clap, fs2 0.4, std::process::Command, tempfile for tests), DuckDB CLI (external dep, soft-fail if missing).

**Reference spec:** `docs/superpowers/specs/2026-05-22-analyst-db-graph-sync-design.md`

---

## File Structure

**New files:**
- `crates/spur-cli/src/commands/analyst.rs` — orchestration: resolve artifact, schema gate, flock, duckdb subprocess, atomic rename. ~250 LOC.
- `crates/spur-cli/tests/analyst_build_cli.rs` — end-to-end integration tests for the new subcommand.
- `crates/spur-graph/src/locking.rs` — promoted, reusable flock helpers (extracted from `store/cache.rs`).

**Modified files:**
- `crates/spur-graph/src/lib.rs` — declare `pub mod locking;` and re-export `try_lock_exclusive_with_timeout`, `is_lock_contended`.
- `crates/spur-graph/src/store/cache.rs` — replace local copies of helpers with `use crate::locking::*;`.
- `crates/spur-cli/src/commands/mod.rs` — add `pub mod analyst;`.
- `crates/spur-cli/src/main.rs` — extend `Commands` and `GraphCommands::Build`, add dispatch.
- `crates/spur-cli/src/commands/graph.rs` — add `skip_analyst: bool` to `GraphBuildOptions`; call `analyst::build_default` at the pinned insertion point.
- `crates/spur-cli/tests/graph_build_cli.rs` — extend with post-build analyst assertions.
- `crates/spur-context/poc/duckdb-analyst/setup.sh` — reduced to shim.
- `crates/spur-context/poc/duckdb-analyst/README.md` — point at the new canonical entry points.

---

## Task 1: Extract flock helpers into reusable `spur_graph::locking` module

**Files:**
- Create: `crates/spur-graph/src/locking.rs`
- Modify: `crates/spur-graph/src/lib.rs`
- Modify: `crates/spur-graph/src/store/cache.rs` (around lines 91-114)

Pure refactor — no behavior change. Existing tests in `spur-graph` (including the cache locking tests) must continue to pass.

- [ ] **Step 1: Locate the existing helpers and their imports**

Run: `rg -n 'try_lock_exclusive_with_timeout|is_lock_contended|LOCK_RETRY_INTERVAL' crates/spur-graph/src`
Expected: matches in `crates/spur-graph/src/store/cache.rs` only. Note any `use` statements and the `LOCK_RETRY_INTERVAL` constant location.

- [ ] **Step 2: Create the new module**

Write `crates/spur-graph/src/locking.rs`:

```rust
//! Shared file-locking primitives used by spur-graph and downstream crates.
//!
//! Centralizes the cross-platform `fs2::FileExt::try_lock_exclusive` retry
//! discipline so flock semantics stay consistent across the workspace
//! (relevant to the open flock-leak RCA at
//! `docs/rca/2026-05-18-flock-leak-pid14282/`).

use std::fs::File;
use std::io;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use fs2::FileExt;

/// How long to sleep between flock retry attempts. Caller-visible default;
/// individual call sites can pass their own deadline via `timeout`.
pub const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// Try to take an exclusive flock on `file`, retrying until `timeout` elapses.
///
/// Returns `Ok(true)` on success, `Ok(false)` if the deadline expired while the
/// lock was still contended, and `Err(_)` for any other I/O error.
pub fn try_lock_exclusive_with_timeout(file: &File, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(true),
            Err(err) if is_lock_contended(&err) => {
                if Instant::now() >= deadline {
                    return Ok(false);
                }
                thread::sleep(
                    LOCK_RETRY_INTERVAL.min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(err) => return Err(err).context("failed to acquire file lock"),
        }
    }
}

/// True if `err` represents a contended (non-fatal) flock attempt.
///
/// macOS and Linux disagree on the underlying errno for non-blocking flock
/// contention; this predicate normalizes them.
pub fn is_lock_contended(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::AlreadyExists | io::ErrorKind::PermissionDenied
    )
}
```

- [ ] **Step 3: Register the module in `lib.rs`**

Modify `crates/spur-graph/src/lib.rs` — add `pub mod locking;` alongside the other top-level `pub mod` declarations (alphabetical or end of list, matching local style).

- [ ] **Step 4: Replace the local helpers in `store/cache.rs`**

In `crates/spur-graph/src/store/cache.rs`:

1. Delete the local `fn try_lock_exclusive_with_timeout`, `fn is_lock_contended`, and the local `const LOCK_RETRY_INTERVAL` if present.
2. Replace any unused imports (`use std::time::Instant;`, `use fs2::FileExt;`, etc.) if they're no longer referenced after the move — `cargo check` will tell you.
3. Add `use crate::locking::{is_lock_contended, try_lock_exclusive_with_timeout, LOCK_RETRY_INTERVAL};` at the top of `cache.rs`.

Leave all callers (`try_lock_exclusive_with_timeout(...)`, `is_lock_contended(...)`) unchanged — only the import path differs.

- [ ] **Step 5: Verify**

Run: `cargo check -p spur-graph && cargo test -p spur-graph --lib`
Expected: clean compile; all existing spur-graph unit tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-graph/src/locking.rs crates/spur-graph/src/lib.rs crates/spur-graph/src/store/cache.rs
git commit -m "refactor(spur-graph): extract flock helpers into pub mod locking"
```

---

## Task 2: Scaffold the `analyst` command module

**Files:**
- Create: `crates/spur-cli/src/commands/analyst.rs`
- Modify: `crates/spur-cli/src/commands/mod.rs`

Skeleton only — public API surface and constants. Behavior arrives in later tasks.

- [ ] **Step 1: Create the empty module**

Write `crates/spur-cli/src/commands/analyst.rs`:

```rust
//! `spur-cli analyst build` — rebuild `.spur/analyst.duckdb` from the current
//! spur-graph parquet artifact.
//!
//! See: `docs/superpowers/specs/2026-05-22-analyst-db-graph-sync-design.md`.

use std::path::PathBuf;

use anyhow::Result;

/// Compiled-in parquet schema version this analyst build understands.
///
/// Must match `manifest.json::schema_version` in the artifact dir. Hard-fail
/// on mismatch to prevent silent miscompiles where `init.sql` view definitions
/// parse but produce wrong results against a newer parquet schema.
pub const SUPPORTED_GRAPH_SCHEMA_VERSION: &str = "spur-graph-schema-v5";

/// Default relative path to the analyst DuckDB inside a worktree.
pub const DEFAULT_ANALYST_DB_REL: &str = ".spur/analyst.duckdb";

/// Default relative path to the per-worktree lock file.
pub const DEFAULT_ANALYST_LOCK_REL: &str = ".spur/analyst.duckdb.lock";

/// Options accepted by `spur-cli analyst build`.
#[derive(Debug, Clone, Default)]
pub struct AnalystBuildOptions {
    /// Override pointer resolution. Falls back to `SPUR_GRAPH_ARTIFACT_DIR`
    /// env, then `.spur/graph/CURRENT`.
    pub artifact_dir: Option<PathBuf>,
    /// Override output path. Falls back to `SPUR_ANALYST_DB` env, then
    /// `<root>/.spur/analyst.duckdb`.
    pub db_path: Option<PathBuf>,
    /// Suppress non-error output.
    pub quiet: bool,
}

/// Entry point for `spur-cli analyst build`.
///
/// `root` is the worktree root (already canonicalized by the caller).
pub fn build(_root: &std::path::Path, _options: AnalystBuildOptions) -> Result<()> {
    anyhow::bail!("spur-cli analyst build: not yet implemented (see Task 7)")
}

/// Convenience wrapper used by `spur-cli graph build` to invoke the default
/// build with only the quiet flag threaded through.
pub fn build_default(root: &std::path::Path, quiet: bool) -> Result<()> {
    build(
        root,
        AnalystBuildOptions {
            quiet,
            ..Default::default()
        },
    )
}
```

- [ ] **Step 2: Register the module**

Modify `crates/spur-cli/src/commands/mod.rs` — add `pub mod analyst;` (alphabetical with the existing `pub mod` lines):

```rust
pub mod analyst;
pub mod auth;
pub mod config_check;
pub mod config_set;
pub mod flags;
pub mod graph;
pub mod init;
pub mod pm_ingest;
pub mod profile;
pub mod telemetry;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p spur-cli`
Expected: clean compile (the placeholder `bail!` is reachable but never invoked yet).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-cli/src/commands/analyst.rs crates/spur-cli/src/commands/mod.rs
git commit -m "feat(spur-cli): scaffold analyst command module"
```

---

## Task 3: Implement artifact-dir resolution (with unit tests)

**Files:**
- Modify: `crates/spur-cli/src/commands/analyst.rs`

Resolution precedence (matches spec & existing `setup.sh`):
1. `options.artifact_dir` — explicit flag.
2. `SPUR_GRAPH_ARTIFACT_DIR` env.
3. `<root>/.spur/graph/CURRENT` symlink (resolved via `fs::canonicalize`).
4. Error.

- [ ] **Step 1: Write failing tests**

Append to `crates/spur-cli/src/commands/analyst.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn resolve_artifact_dir_prefers_explicit_option() {
        let root = temp_root();
        let target = root.path().join("explicit-artifact");
        fs::create_dir_all(&target).unwrap();

        let opts = AnalystBuildOptions {
            artifact_dir: Some(target.clone()),
            ..Default::default()
        };
        let resolved = resolve_artifact_dir(root.path(), &opts).expect("resolve");
        assert_eq!(resolved, fs::canonicalize(&target).unwrap());
    }

    #[test]
    fn resolve_artifact_dir_falls_back_to_env() {
        let root = temp_root();
        let target = root.path().join("env-artifact");
        fs::create_dir_all(&target).unwrap();
        let prev = std::env::var_os("SPUR_GRAPH_ARTIFACT_DIR");
        std::env::set_var("SPUR_GRAPH_ARTIFACT_DIR", &target);

        let resolved = resolve_artifact_dir(root.path(), &AnalystBuildOptions::default());

        // Restore env before asserting so test isolation is maintained even on
        // assertion failure.
        match prev {
            Some(v) => std::env::set_var("SPUR_GRAPH_ARTIFACT_DIR", v),
            None => std::env::remove_var("SPUR_GRAPH_ARTIFACT_DIR"),
        }

        let resolved = resolved.expect("resolve");
        assert_eq!(resolved, fs::canonicalize(&target).unwrap());
    }

    #[test]
    fn resolve_artifact_dir_uses_current_pointer() {
        let root = temp_root();
        let artifact = root.path().join(".git/spur-graph/artifacts/v/h.parquet");
        fs::create_dir_all(&artifact).unwrap();
        let graph_dir = root.path().join(".spur/graph");
        fs::create_dir_all(&graph_dir).unwrap();
        std::os::unix::fs::symlink(&artifact, graph_dir.join("CURRENT")).unwrap();

        let prev = std::env::var_os("SPUR_GRAPH_ARTIFACT_DIR");
        std::env::remove_var("SPUR_GRAPH_ARTIFACT_DIR");

        let resolved = resolve_artifact_dir(root.path(), &AnalystBuildOptions::default());

        if let Some(v) = prev {
            std::env::set_var("SPUR_GRAPH_ARTIFACT_DIR", v);
        }

        let resolved = resolved.expect("resolve");
        assert_eq!(resolved, fs::canonicalize(&artifact).unwrap());
    }

    #[test]
    fn resolve_artifact_dir_errors_when_nothing_resolves() {
        let root = temp_root();
        let prev = std::env::var_os("SPUR_GRAPH_ARTIFACT_DIR");
        std::env::remove_var("SPUR_GRAPH_ARTIFACT_DIR");

        let err = resolve_artifact_dir(root.path(), &AnalystBuildOptions::default())
            .expect_err("should error");

        if let Some(v) = prev {
            std::env::set_var("SPUR_GRAPH_ARTIFACT_DIR", v);
        }

        let msg = format!("{err:#}");
        assert!(
            msg.contains("CURRENT") || msg.contains("graph build"),
            "unexpected error message: {msg}"
        );
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test -p spur-cli commands::analyst::tests --lib`
Expected: 4 tests fail with "cannot find function `resolve_artifact_dir`".

- [ ] **Step 3: Implement `resolve_artifact_dir`**

Add to `crates/spur-cli/src/commands/analyst.rs` (above the `#[cfg(test)]` block):

```rust
use anyhow::{anyhow, Context};
use std::path::Path;

pub(crate) fn resolve_artifact_dir(
    root: &Path,
    options: &AnalystBuildOptions,
) -> Result<PathBuf> {
    if let Some(explicit) = options.artifact_dir.as_ref() {
        return std::fs::canonicalize(explicit)
            .with_context(|| format!("failed to canonicalize --artifact-dir {explicit:?}"));
    }
    if let Some(env_dir) = std::env::var_os("SPUR_GRAPH_ARTIFACT_DIR") {
        let env_path = PathBuf::from(env_dir);
        return std::fs::canonicalize(&env_path).with_context(|| {
            format!("failed to canonicalize SPUR_GRAPH_ARTIFACT_DIR={env_path:?}")
        });
    }
    let current = root.join(".spur").join("graph").join("CURRENT");
    if current.exists() {
        return std::fs::canonicalize(&current)
            .with_context(|| format!("failed to resolve {} symlink", current.display()));
    }
    Err(anyhow!(
        "spur-graph CURRENT pointer not found at {} — run `spur-cli graph build --workspace` \
         or set SPUR_GRAPH_ARTIFACT_DIR to a parquet artifact directory",
        current.display()
    ))
}
```

- [ ] **Step 4: Run tests to confirm they pass**

Run: `cargo test -p spur-cli commands::analyst::tests --lib`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cli/src/commands/analyst.rs
git commit -m "feat(spur-cli/analyst): resolve artifact dir with explicit/env/CURRENT precedence"
```

---

## Task 4: Schema-version gate via `manifest.json`

**Files:**
- Modify: `crates/spur-cli/src/commands/analyst.rs`

Hard-fail if `manifest.json::schema_version` ≠ `SUPPORTED_GRAPH_SCHEMA_VERSION`. The error message must surface both versions.

- [ ] **Step 1: Write failing tests**

Append to the `mod tests` block in `crates/spur-cli/src/commands/analyst.rs`:

```rust
    #[test]
    fn verify_schema_version_accepts_matching() {
        let dir = temp_root();
        std::fs::write(
            dir.path().join("manifest.json"),
            format!(r#"{{"schema_version":"{SUPPORTED_GRAPH_SCHEMA_VERSION}"}}"#),
        )
        .unwrap();
        verify_schema_version(dir.path()).expect("matching schema version");
    }

    #[test]
    fn verify_schema_version_rejects_mismatch() {
        let dir = temp_root();
        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{"schema_version":"spur-graph-schema-vNEXT"}"#,
        )
        .unwrap();
        let err = verify_schema_version(dir.path()).expect_err("mismatch should error");
        let msg = format!("{err:#}");
        assert!(
            msg.contains(SUPPORTED_GRAPH_SCHEMA_VERSION) && msg.contains("vNEXT"),
            "expected both versions in error, got: {msg}"
        );
    }

    #[test]
    fn verify_schema_version_errors_on_missing_manifest() {
        let dir = temp_root();
        let err = verify_schema_version(dir.path()).expect_err("missing manifest should error");
        assert!(format!("{err:#}").contains("manifest.json"));
    }

    #[test]
    fn verify_schema_version_errors_on_malformed_manifest() {
        let dir = temp_root();
        std::fs::write(dir.path().join("manifest.json"), "not json").unwrap();
        let err = verify_schema_version(dir.path()).expect_err("malformed should error");
        assert!(format!("{err:#}").contains("manifest.json"));
    }
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test -p spur-cli commands::analyst::tests::verify_schema_version --lib`
Expected: 4 tests fail with "cannot find function `verify_schema_version`".

- [ ] **Step 3: Implement `verify_schema_version`**

Add to `crates/spur-cli/src/commands/analyst.rs`:

```rust
pub(crate) fn verify_schema_version(artifact_dir: &Path) -> Result<()> {
    let manifest_path = artifact_dir.join("manifest.json");
    let bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let parsed: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {} as JSON", manifest_path.display()))?;
    let observed = parsed
        .get("schema_version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow!(
                "manifest.json at {} is missing string field `schema_version`",
                manifest_path.display()
            )
        })?;
    if observed != SUPPORTED_GRAPH_SCHEMA_VERSION {
        return Err(anyhow!(
            "analyst build refuses to run: parquet schema_version {observed:?} does not match \
             SUPPORTED_GRAPH_SCHEMA_VERSION {SUPPORTED_GRAPH_SCHEMA_VERSION:?}. Rebuild spur-cli \
             against the new schema or rebuild the graph against the supported schema."
        ));
    }
    Ok(())
}
```

If `serde_json` is not already a dep of `spur-cli`, add it. Check first:

```bash
rg -n '^serde_json\b|"serde_json"' crates/spur-cli/Cargo.toml
```

Expected: it is a dep (spur-cli uses JSON throughout). If absent, add `serde_json = { workspace = true }` to `[dependencies]`.

- [ ] **Step 4: Run tests to confirm they pass**

Run: `cargo test -p spur-cli commands::analyst::tests::verify_schema_version --lib`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cli/src/commands/analyst.rs crates/spur-cli/Cargo.toml
git commit -m "feat(spur-cli/analyst): hard-fail on parquet schema version mismatch"
```

---

## Task 5: Required parquet file presence check + `duckdb` PATH probe

**Files:**
- Modify: `crates/spur-cli/src/commands/analyst.rs`

These are simple, paired checks. Bundling them keeps the module's helper surface narrow.

- [ ] **Step 1: Write failing tests**

Append to the `mod tests` block:

```rust
    const REQUIRED_PARQUETS: &[&str] = &[
        "nodes.parquet",
        "edges.parquet",
        "edges_by_dst.parquet",
        "edges_unresolved.parquet",
        "files.parquet",
        "file_manifests.parquet",
        "tombstones.parquet",
        "manifest.json",
    ];

    fn populate_required(dir: &Path) {
        for name in REQUIRED_PARQUETS {
            std::fs::write(dir.join(name), b"").unwrap();
        }
    }

    #[test]
    fn verify_required_files_ok_when_all_present() {
        let dir = temp_root();
        populate_required(dir.path());
        verify_required_files(dir.path()).expect("all present");
    }

    #[test]
    fn verify_required_files_errors_listing_missing() {
        let dir = temp_root();
        populate_required(dir.path());
        std::fs::remove_file(dir.path().join("edges.parquet")).unwrap();
        std::fs::remove_file(dir.path().join("tombstones.parquet")).unwrap();
        let err = verify_required_files(dir.path()).expect_err("missing should error");
        let msg = format!("{err:#}");
        assert!(msg.contains("edges.parquet"), "missing edges in: {msg}");
        assert!(msg.contains("tombstones.parquet"), "missing tombstones in: {msg}");
    }

    #[test]
    fn duckdb_cli_present_returns_some_when_on_path() {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let dir = temp_root();
        let shim = dir.path().join("duckdb");
        std::fs::write(&shim, "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();

        std::env::set_var("PATH", dir.path());
        let found = duckdb_cli_present();
        std::env::set_var("PATH", path);

        assert!(found, "shim was not found via PATH");
    }

    #[test]
    fn duckdb_cli_present_returns_false_when_absent() {
        let prev = std::env::var_os("PATH").unwrap_or_default();
        let dir = temp_root();
        std::env::set_var("PATH", dir.path());
        let found = duckdb_cli_present();
        std::env::set_var("PATH", prev);

        assert!(!found);
    }
```

- [ ] **Step 2: Run tests to confirm they fail**

Run: `cargo test -p spur-cli commands::analyst::tests --lib`
Expected: 4 new tests fail with "cannot find function".

- [ ] **Step 3: Implement both helpers**

Add to `crates/spur-cli/src/commands/analyst.rs`:

```rust
const REQUIRED_PARQUETS: &[&str] = &[
    "nodes.parquet",
    "edges.parquet",
    "edges_by_dst.parquet",
    "edges_unresolved.parquet",
    "files.parquet",
    "file_manifests.parquet",
    "tombstones.parquet",
    "manifest.json",
];

pub(crate) fn verify_required_files(artifact_dir: &Path) -> Result<()> {
    let missing: Vec<&str> = REQUIRED_PARQUETS
        .iter()
        .copied()
        .filter(|name| !artifact_dir.join(name).is_file())
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(anyhow!(
        "spur-graph artifact at {} is missing required file(s): {}",
        artifact_dir.display(),
        missing.join(", ")
    ))
}

pub(crate) fn duckdb_cli_present() -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    for entry in std::env::split_paths(&paths) {
        let candidate = entry.join("duckdb");
        if candidate.is_file() {
            return true;
        }
    }
    false
}
```

- [ ] **Step 4: Run tests to confirm they pass**

Run: `cargo test -p spur-cli commands::analyst::tests --lib`
Expected: all analyst tests passing.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cli/src/commands/analyst.rs
git commit -m "feat(spur-cli/analyst): parquet file checks and duckdb CLI probe"
```

---

## Task 6: Implement the build flow (flock + tmp+rename + duckdb subprocess)

**Files:**
- Modify: `crates/spur-cli/src/commands/analyst.rs`
- Modify: `crates/spur-cli/Cargo.toml` (if `spur-graph` isn't already a dep)

This task makes `analyst::build` real. The flow:

1. Resolve artifact dir.
2. `verify_schema_version`.
3. `verify_required_files`.
4. If `!duckdb_cli_present()` → log warning, return Ok(()).
5. Open/create the lock file; `try_lock_exclusive_with_timeout(file, Duration::ZERO)`.
   - `Ok(false)` (contended) → log "another analyst build in progress, skipping", return Ok(()).
6. Read `init.sql`, substitute `__SPUR_GRAPH_ARTIFACT_DIR__`.
7. Spawn `duckdb <tmp_db>` with the substituted SQL piped to stdin.
8. On success → `rename` tmp over `<db_path>`. On failure → remove tmp, log warning, return Ok(()).
9. Drop the lock by closing the file. (Lock file is **not** unlinked.)

- [ ] **Step 1: Ensure `spur-graph` is a dep of `spur-cli`**

Run: `rg -n '^spur-graph\b|"spur-graph"' crates/spur-cli/Cargo.toml`
Expected: it is already a dep (spur-cli uses `spur_graph` types in `commands/graph.rs`). If absent, add `spur-graph = { workspace = true }` to `[dependencies]`.

- [ ] **Step 2: Locate `init.sql` from the crate**

The init.sql lives at `crates/spur-context/poc/duckdb-analyst/init.sql`. Embed its path discovery in a small helper. Options:

- **Compile-time embedding** via `include_str!` — simplest, but couples the spur-cli binary version to the SQL file.
- **Runtime resolution** by walking up from the worktree root — fragile if invoked from outside the repo.

Pick **compile-time embed**: this also guarantees workers and CI get exactly the SQL the binary was built with, which matches the schema-version gate's philosophy.

- [ ] **Step 3: Write a happy-path integration test (will fail until Step 5)**

Append to the `mod tests` block:

```rust
    #[test]
    #[cfg(unix)]
    fn build_happy_path_against_real_duckdb_if_present() {
        if !duckdb_cli_present() {
            eprintln!("skipping: duckdb CLI not on PATH");
            return;
        }
        // Real artifacts are required; this test piggybacks on the
        // repo's own .spur/graph/CURRENT if available, otherwise skips.
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let current = repo_root.join(".spur/graph/CURRENT");
        if !current.exists() {
            eprintln!("skipping: no .spur/graph/CURRENT in repo");
            return;
        }
        let tmp_db = repo_root
            .join(".spur")
            .join(format!("analyst.test-{}.duckdb", std::process::id()));
        let _ = std::fs::remove_file(&tmp_db);

        let opts = AnalystBuildOptions {
            db_path: Some(tmp_db.clone()),
            quiet: true,
            ..Default::default()
        };
        build(repo_root, opts).expect("build");

        assert!(tmp_db.is_file(), "db file not created at {}", tmp_db.display());
        let _ = std::fs::remove_file(&tmp_db);
        let _ = std::fs::remove_file(repo_root.join(".spur/analyst.duckdb.lock"));
    }
```

(Yes — this happy-path test depends on the repo state; that's deliberate. Soft-fails / skips are documented.)

- [ ] **Step 4: Run the test to confirm it fails**

Run: `cargo test -p spur-cli build_happy_path_against_real_duckdb_if_present --lib -- --nocapture`
Expected: fails inside `build()` at the `bail!("not yet implemented")` left over from Task 2 (or skips if duckdb is unavailable).

- [ ] **Step 5: Implement `build`**

Replace the stub `pub fn build(...)` in `crates/spur-cli/src/commands/analyst.rs` with the full implementation:

```rust
use std::fs::OpenOptions;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use spur_graph::locking::try_lock_exclusive_with_timeout;

const INIT_SQL: &str = include_str!(
    "../../../spur-context/poc/duckdb-analyst/init.sql"
);
const ARTIFACT_PLACEHOLDER: &str = "__SPUR_GRAPH_ARTIFACT_DIR__";

pub fn build(root: &Path, options: AnalystBuildOptions) -> Result<()> {
    let quiet = options.quiet;
    let artifact_dir = resolve_artifact_dir(root, &options)?;
    verify_schema_version(&artifact_dir)?;
    verify_required_files(&artifact_dir)?;

    if !duckdb_cli_present() {
        if !quiet {
            eprintln!(
                "[spur] warning: 'duckdb' CLI not found on PATH — skipping analyst DB refresh"
            );
            eprintln!(
                "[spur] hint: brew install duckdb (or set SPUR_GRAPH_SKIP_ANALYST=1 to silence)"
            );
        }
        return Ok(());
    }

    let db_path = options
        .db_path
        .clone()
        .or_else(|| std::env::var_os("SPUR_ANALYST_DB").map(PathBuf::from))
        .unwrap_or_else(|| root.join(DEFAULT_ANALYST_DB_REL));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let lock_path = root.join(DEFAULT_ANALYST_LOCK_REL);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to open lock file {}", lock_path.display()))?;
    let acquired = try_lock_exclusive_with_timeout(&lock_file, Duration::ZERO)?;
    if !acquired {
        if !quiet {
            eprintln!("[spur] another analyst build in progress, skipping");
        }
        return Ok(());
    }

    let started = Instant::now();
    if !quiet {
        eprintln!("[spur] Refreshing analyst DB at {}", db_path.display());
    }

    let tmp_db = db_path.with_extension(format!("duckdb.tmp-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp_db);

    let artifact_dir_sql = artifact_dir.display().to_string().replace('\'', "''");
    let sql = INIT_SQL.replace(ARTIFACT_PLACEHOLDER, &artifact_dir_sql);

    let mut child = Command::new("duckdb")
        .arg(&tmp_db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| "failed to spawn duckdb subprocess")?;
    {
        let stdin = child.stdin.as_mut().expect("piped stdin");
        stdin
            .write_all(sql.as_bytes())
            .context("failed to write init.sql to duckdb stdin")?;
    }
    let output = child
        .wait_with_output()
        .context("failed to wait on duckdb subprocess")?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&tmp_db);
        if !quiet {
            eprintln!(
                "[spur] warning: duckdb exited non-zero (status: {}); previous analyst DB preserved",
                output.status
            );
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                eprintln!("[spur] duckdb stderr: {}", stderr.trim());
            }
        }
        return Ok(());
    }

    std::fs::rename(&tmp_db, &db_path).with_context(|| {
        format!(
            "failed to rename {} over {}",
            tmp_db.display(),
            db_path.display()
        )
    })?;

    if !quiet {
        // Surface the schema/content hash from the manifest we already validated.
        let observed_hash = std::fs::read(artifact_dir.join("manifest.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| {
                v.get("graph_content_hash")
                    .and_then(|x| x.as_str().map(str::to_owned))
            })
            .unwrap_or_else(|| "<unknown>".to_string());
        let elapsed = started.elapsed();
        eprintln!(
            "[spur] Analyst DB ready (graph_content_hash={}, {:.1}s)",
            short_hash(&observed_hash),
            elapsed.as_secs_f64()
        );
    }

    // Lock auto-released on file drop.
    Ok(())
}

fn short_hash(hash: &str) -> String {
    if hash.len() > 8 {
        format!("{}…", &hash[..8])
    } else {
        hash.to_string()
    }
}
```

- [ ] **Step 6: Run the happy-path test**

Run: `cargo test -p spur-cli build_happy_path_against_real_duckdb_if_present --lib -- --nocapture`
Expected: PASS if `duckdb` is on PATH and `.spur/graph/CURRENT` exists; otherwise a "skipping" message and exit code 0.

- [ ] **Step 7: Re-run the full analyst test module**

Run: `cargo test -p spur-cli commands::analyst::tests --lib`
Expected: all tests pass.

- [ ] **Step 8: Commit**

```bash
git add crates/spur-cli/src/commands/analyst.rs crates/spur-cli/Cargo.toml
git commit -m "feat(spur-cli/analyst): implement build with flock + atomic tmp+rename"
```

---

## Task 7: Add the CLI subcommand `analyst build`

**Files:**
- Modify: `crates/spur-cli/src/main.rs`

Add a new top-level enum variant `Commands::Analyst { command: AnalystCommands }` with one nested `Build { ... }` variant, mirroring the structure of `Commands::Graph`.

- [ ] **Step 1: Add the subcommand enum near `GraphCommands` (around line 446)**

Add to `crates/spur-cli/src/main.rs`, just before or after the existing `GraphCommands` enum:

```rust
#[derive(Debug, Subcommand)]
enum AnalystCommands {
    /// Rebuild .spur/analyst.duckdb from the current spur-graph parquet artifact.
    Build {
        /// Worktree root. Defaults to the current worktree root.
        #[arg(long)]
        root: Option<PathBuf>,
        /// Override the parquet artifact directory (matches SPUR_GRAPH_ARTIFACT_DIR).
        #[arg(long)]
        artifact_dir: Option<PathBuf>,
        /// Override the analyst DuckDB path (matches SPUR_ANALYST_DB).
        #[arg(long)]
        db_path: Option<PathBuf>,
        /// Suppress non-error output.
        #[arg(long)]
        quiet: bool,
    },
}
```

- [ ] **Step 2: Add the variant to `Commands` (the enum at line 168)**

Locate the `enum Commands { ... }` block and add (alphabetically among the variants, or at a sensible cluster point near `Graph`):

```rust
    /// Manage the .spur/analyst.duckdb (DuckDB-backed query surface over the graph).
    Analyst {
        #[command(subcommand)]
        command: AnalystCommands,
    },
```

- [ ] **Step 3: Add the dispatch arm in `run` (around line 848 — sibling to `Commands::Graph`)**

In the big `match` inside `fn run`, add:

```rust
        Commands::Analyst { command } => match command {
            AnalystCommands::Build {
                root,
                artifact_dir,
                db_path,
                quiet,
            } => {
                let resolved_root = match root {
                    Some(path) => path,
                    None => spur_graph::resolve_worktree_root_from(std::env::current_dir()?),
                };
                let resolved_root = resolved_root.canonicalize().with_context(|| {
                    format!(
                        "failed to canonicalize root `{}`",
                        resolved_root.display()
                    )
                })?;
                commands::analyst::build(
                    &resolved_root,
                    commands::analyst::AnalystBuildOptions {
                        artifact_dir,
                        db_path,
                        quiet,
                    },
                )
            }
        },
```

If `anyhow::Context` isn't already imported in `main.rs`, add `use anyhow::Context;` to the top imports.

- [ ] **Step 4: Compile-check**

Run: `cargo check -p spur-cli`
Expected: clean.

- [ ] **Step 5: Smoke-run the subcommand**

Run (from the repo root with a graph already built):

```bash
cargo run -p spur-cli -- analyst build --quiet
echo "exit: $?"
ls -la .spur/analyst.duckdb
```

Expected: exit 0; `.spur/analyst.duckdb` exists (and was just refreshed).

- [ ] **Step 6: Commit**

```bash
git add crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): wire `analyst build` subcommand"
```

---

## Task 8: Wire analyst rebuild into `graph build`

**Files:**
- Modify: `crates/spur-cli/src/commands/graph.rs`
- Modify: `crates/spur-cli/src/main.rs`

Add `--no-analyst` flag (and `SPUR_GRAPH_SKIP_ANALYST=1` env honoring). Thread through `GraphBuildOptions`. Call `analyst::build_default` between the existing `result?;` (currently line 200) and the summary `println!` (currently line 214).

- [ ] **Step 1: Add the field to `GraphBuildOptions`**

In `crates/spur-cli/src/commands/graph.rs` (around line 17):

```rust
#[derive(Debug, Clone)]
pub struct GraphBuildOptions {
    pub root: Option<PathBuf>,
    pub workspace: bool,
    pub output: Option<PathBuf>,
    pub quiet: bool,
    pub skip_analyst: bool,
}
```

- [ ] **Step 2: Insert the analyst call at the pinned point**

In `crates/spur-cli/src/commands/graph.rs`, between the existing `result?;` (current line 200) and the summary `println!` (current line 214), add:

```rust
    // ---- analyst DB sync (see Task 8 / spec) ----
    let skip_analyst = options.skip_analyst
        || matches!(std::env::var("SPUR_GRAPH_SKIP_ANALYST"), Ok(v) if v == "1");
    if !skip_analyst {
        crate::commands::analyst::build_default(&root, options.quiet)?;
    }
```

Place it after `result?;` returns Ok and before the `let language_summary = ...` line that precedes the summary `println!`.

- [ ] **Step 3: Add the `--no-analyst` flag to `GraphCommands::Build`**

In `crates/spur-cli/src/main.rs` (around line 432) update the `Build` variant:

```rust
enum GraphCommands {
    /// Extract Rust symbols from a worktree and write a graph index artifact.
    Build {
        /// Worktree root to extract. Defaults to the current worktree root.
        #[arg(long, conflicts_with = "workspace")]
        root: Option<PathBuf>,
        /// Build from the resolved worktree root.
        #[arg(long)]
        workspace: bool,
        /// Output artifact directory. Defaults to SPUR_CODE_GRAPH_INDEX or .spur/graph.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Suppress progress output.
        #[arg(long)]
        quiet: bool,
        /// Skip the analyst DuckDB rebuild that normally follows a successful graph build.
        /// Also honored via SPUR_GRAPH_SKIP_ANALYST=1.
        #[arg(long)]
        no_analyst: bool,
    },
}
```

- [ ] **Step 4: Thread the flag through dispatch**

In the `Commands::Graph` arm (around line 848) in `crates/spur-cli/src/main.rs`:

```rust
        Commands::Graph { command } => match command {
            GraphCommands::Build {
                root,
                workspace,
                output,
                quiet,
                no_analyst,
            } => commands::graph::build(commands::graph::GraphBuildOptions {
                root,
                workspace,
                output,
                quiet,
                skip_analyst: no_analyst,
            }),
        },
```

- [ ] **Step 5: Compile-check**

Run: `cargo check -p spur-cli`
Expected: clean.

- [ ] **Step 6: Smoke test — happy path**

```bash
cargo run -p spur-cli -- graph build --workspace
echo "exit: $?"
ls -la .spur/analyst.duckdb
```

Expected: exit 0; `.spur/analyst.duckdb` mtime is fresh.

- [ ] **Step 7: Smoke test — opt-out**

```bash
rm -f .spur/analyst.duckdb
cargo run -p spur-cli -- graph build --workspace --no-analyst
echo "exit: $?"
test -f .spur/analyst.duckdb && echo "BUG: db was created" || echo "ok: no db"
```

Expected: exit 0; "ok: no db".

- [ ] **Step 8: Commit**

```bash
git add crates/spur-cli/src/commands/graph.rs crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): rebuild analyst DB as a post-step of graph build"
```

---

## Task 9: Integration tests for the wired-in flow

**Files:**
- Create: `crates/spur-cli/tests/analyst_build_cli.rs`
- Modify: `crates/spur-cli/tests/graph_build_cli.rs`

End-to-end tests via the actual binary, exercising the soft-fail, opt-out, and schema-version-mismatch paths.

- [ ] **Step 1: Write `analyst_build_cli.rs`**

Create `crates/spur-cli/tests/analyst_build_cli.rs`:

```rust
use std::process::Command;

fn spur_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_spur"))
}

fn fixture_git_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(dir.path())
            .args(args)
            .output()
            .expect("git");
        assert!(out.status.success(), "git {args:?}");
    };
    run(&["init"]);
    run(&["config", "user.email", "spur@example.test"]);
    run(&["config", "user.name", "Spur Test"]);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/lib.rs"),
        "pub fn hello() -> u32 { 42 }\n",
    )
    .unwrap();
    run(&["add", "src/lib.rs"]);
    run(&["commit", "-m", "initial"]);
    dir
}

#[test]
fn analyst_build_skipped_by_flag() {
    let dir = fixture_git_repo();
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--no-analyst", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        !dir.path().join(".spur/analyst.duckdb").exists(),
        "analyst DB should not have been created with --no-analyst"
    );
}

#[test]
fn analyst_build_skipped_by_env() {
    let dir = fixture_git_repo();
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--quiet"])
        .env("SPUR_GRAPH_SKIP_ANALYST", "1")
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(
        !dir.path().join(".spur/analyst.duckdb").exists(),
        "analyst DB should not have been created with SPUR_GRAPH_SKIP_ANALYST=1"
    );
}

#[test]
fn analyst_build_soft_fails_when_duckdb_missing() {
    let dir = fixture_git_repo();
    // Build graph first (with analyst skipped so this test stays isolated).
    let pre = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--no-analyst", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(pre.status.success());

    // Now invoke `analyst build` with a PATH that has no duckdb.
    let empty_path = dir.path().join("empty-path");
    std::fs::create_dir_all(&empty_path).unwrap();
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["analyst", "build"])
        .env("PATH", empty_path)
        .output()
        .expect("spawn");
    assert!(out.status.success(), "soft-fail should exit 0, stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("duckdb"), "expected duckdb-missing warning, got: {stderr}");
    assert!(
        !dir.path().join(".spur/analyst.duckdb").exists(),
        "no DB should exist after soft-fail"
    );
}

#[test]
fn analyst_build_rejects_schema_version_mismatch() {
    let dir = fixture_git_repo();
    // Build graph first to populate parquets.
    let pre = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--no-analyst", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(pre.status.success(), "stderr: {}", String::from_utf8_lossy(&pre.stderr));

    // Tamper with manifest.json under the resolved CURRENT artifact.
    let current = dir.path().join(".spur/graph/CURRENT");
    let resolved = std::fs::canonicalize(&current).expect("CURRENT resolves");
    let manifest_path = resolved.join("manifest.json");
    let original = std::fs::read_to_string(&manifest_path).unwrap();
    let tampered = original.replace("spur-graph-schema-v5", "spur-graph-schema-vNEXT");
    assert_ne!(original, tampered, "fixture invariant: schema_version must have been present");
    std::fs::write(&manifest_path, &tampered).unwrap();

    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["analyst", "build"])
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "schema mismatch should hard-fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("schema") && stderr.contains("vNEXT"),
        "expected schema-mismatch error, got: {stderr}"
    );
}
```

- [ ] **Step 1b: Add the atomicity test**

Append to `crates/spur-cli/tests/analyst_build_cli.rs`:

```rust
#[test]
fn analyst_build_atomic_under_duckdb_failure() {
    let dir = fixture_git_repo();
    // Build the graph (parquets only — skip analyst so we control the next step).
    let pre = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--no-analyst", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(pre.status.success(), "stderr: {}", String::from_utf8_lossy(&pre.stderr));

    // Pre-populate the analyst DB with sentinel bytes that must survive the
    // failed second invocation.
    let db_path = dir.path().join(".spur/analyst.duckdb");
    std::fs::write(&db_path, b"SENTINEL-BYTES-MUST-NOT-CHANGE").unwrap();
    let before = std::fs::read(&db_path).unwrap();

    // Stage a fake `duckdb` that always exits non-zero.
    let fake_bin_dir = dir.path().join("fake-bin");
    std::fs::create_dir_all(&fake_bin_dir).unwrap();
    let fake_duckdb = fake_bin_dir.join("duckdb");
    std::fs::write(&fake_duckdb, "#!/bin/sh\necho 'fake duckdb' >&2\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake_duckdb, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Compose a PATH that has the fake duckdb but nothing else from the host.
    let path_value = fake_bin_dir.display().to_string();

    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["analyst", "build"])
        .env("PATH", path_value)
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "soft-degrade should exit 0, stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = std::fs::read(&db_path).unwrap();
    assert_eq!(
        before, after,
        "previous analyst DB must be byte-identical after a failed duckdb run"
    );

    // No leftover tmp files alongside the DB.
    let tmp_count = std::fs::read_dir(dir.path().join(".spur"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("analyst.duckdb.tmp-")
        })
        .count();
    assert_eq!(tmp_count, 0, "leftover tmp file(s) present after failure");
}

#[test]
fn analyst_build_concurrent_skip() {
    let dir = fixture_git_repo();
    // Build the parquets first (analyst skipped — we'll exercise it concurrently below).
    let pre = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--no-analyst", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(pre.status.success(), "stderr: {}", String::from_utf8_lossy(&pre.stderr));

    let duckdb_found = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("duckdb").is_file()))
        .unwrap_or(false);
    if !duckdb_found {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }

    // Launch two `analyst build` invocations close enough in time that one
    // should observe the other's flock.
    let spawn = || {
        Command::new(spur_binary())
            .current_dir(dir.path())
            .args(["analyst", "build"])
            .stderr(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("spawn")
    };
    let a = spawn();
    let b = spawn();
    let a_out = a.wait_with_output().expect("wait a");
    let b_out = b.wait_with_output().expect("wait b");

    // Both must exit 0 (one builds, the other skips).
    assert!(a_out.status.success(), "a stderr: {}", String::from_utf8_lossy(&a_out.stderr));
    assert!(b_out.status.success(), "b stderr: {}", String::from_utf8_lossy(&b_out.stderr));

    let combined_stderr = format!(
        "{}{}",
        String::from_utf8_lossy(&a_out.stderr),
        String::from_utf8_lossy(&b_out.stderr)
    );
    assert!(
        combined_stderr.contains("another analyst build in progress"),
        "expected at least one process to log the contention skip; got: {combined_stderr}"
    );

    // Final DB must be valid (exists and non-empty).
    let db = dir.path().join(".spur/analyst.duckdb");
    let meta = std::fs::metadata(&db).expect("DB exists after concurrent builds");
    assert!(meta.len() > 0, "DB must be non-empty");
}
```

Note: the concurrent test is inherently timing-sensitive. If the second invocation wins the race and acquires the lock after the first releases it, both will report a successful build and the contention assertion will be flaky. The accepted form here uses `Duration::ZERO` for the timeout (see Task 6, Step 5) which makes the loser fail fast on contention — that's the design choice that makes this test deterministic in practice. If flake is observed, switch the test to assert "at most one full rebuild line in combined stderr" instead of the contention substring.

- [ ] **Step 2: Extend `graph_build_cli.rs` with a post-build analyst assertion**

Append to `crates/spur-cli/tests/graph_build_cli.rs`:

```rust
#[test]
fn graph_build_triggers_analyst_rebuild_when_duckdb_present() {
    // Skip if duckdb isn't on PATH — soft-fail path is covered separately.
    let duckdb_found = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("duckdb").is_file()))
        .unwrap_or(false);
    if !duckdb_found {
        eprintln!("skipping: duckdb CLI not on PATH");
        return;
    }

    let dir = fixture_git_repo();
    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["graph", "build", "--workspace", "--quiet"])
        .env_remove("SPUR_CODE_GRAPH_INDEX")
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let db = dir.path().join(".spur/analyst.duckdb");
    assert!(db.is_file(), "analyst DB should exist at {}", db.display());
}
```

- [ ] **Step 3: Run integration tests**

Run: `cargo test -p spur-cli --test analyst_build_cli --test graph_build_cli`
Expected: all pass (happy-path test skips if `duckdb` is absent).

- [ ] **Step 4: Commit**

```bash
git add crates/spur-cli/tests/analyst_build_cli.rs crates/spur-cli/tests/graph_build_cli.rs
git commit -m "test(spur-cli): integration tests for graph build → analyst rebuild"
```

---

## Task 10: Reduce `setup.sh` to a shim; update README

**Files:**
- Modify: `crates/spur-context/poc/duckdb-analyst/setup.sh`
- Modify: `crates/spur-context/poc/duckdb-analyst/README.md`

- [ ] **Step 1: Replace `setup.sh`**

Overwrite `crates/spur-context/poc/duckdb-analyst/setup.sh` with:

```sh
#!/usr/bin/env bash
# Thin shim — analyst DB build now lives in `spur-cli analyst build`.
# Kept for the documented entry point and CI usage. Forwards all flags.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

exec scripts/spur-cargo run --quiet -p spur-cli -- analyst build "$@"
```

(Existing file is executable; the shim must remain executable. `chmod +x` it if needed.)

- [ ] **Step 2: Update README**

In `crates/spur-context/poc/duckdb-analyst/README.md`, replace the section that documents `setup.sh` invocation with the canonical entry points:

```markdown
## Building the analyst DuckDB

The analyst DB is rebuilt automatically as a post-step of `spur-cli graph build`.
You should rarely need to invoke it manually.

### Canonical entry points

```bash
# Recommended: build the graph; the analyst DB is refreshed automatically.
spur-cli graph build --workspace

# Manual: refresh the analyst DB against the current graph.
spur-cli analyst build

# Legacy entry point — forwards to `spur-cli analyst build`.
./crates/spur-context/poc/duckdb-analyst/setup.sh
```

### Opting out

Pass `--no-analyst` to `spur-cli graph build`, or set
`SPUR_GRAPH_SKIP_ANALYST=1` in the environment. Useful in CI lanes that
build the graph but don't need the analyst surface.

### First-run cost

`init.sql` installs the `duckpgq` and `onager` DuckDB community extensions.
On a host with no cached community extensions, the first run downloads
both (5-30s, network-dependent). Subsequent runs are sub-second.

### Soft-fail behavior

If the `duckdb` CLI is not on PATH, `analyst build` prints a warning and
exits 0; the upstream `graph build` is unaffected. Install with
`brew install duckdb` (or your platform's equivalent) to enable analyst
refresh.
```

- [ ] **Step 3: Smoke-run the shim**

```bash
./crates/spur-context/poc/duckdb-analyst/setup.sh --quiet
echo "exit: $?"
```

Expected: exit 0; `.spur/analyst.duckdb` refreshed.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-context/poc/duckdb-analyst/setup.sh crates/spur-context/poc/duckdb-analyst/README.md
git commit -m "docs(duckdb-analyst): reduce setup.sh to shim; document new entry points"
```

---

## Final verification

- [ ] **Step 1: Full test sweep**

Run: `cargo test -p spur-graph -p spur-cli`
Expected: all green.

- [ ] **Step 2: Real-world smoke from a clean state**

```bash
rm -f .spur/analyst.duckdb .spur/analyst.duckdb.lock
cargo run -p spur-cli -- graph build --workspace
ls -la .spur/analyst.duckdb
duckdb .spur/analyst.duckdb "SELECT graph_content_hash FROM _meta;"
```

Expected: DB exists; `graph_content_hash` value matches `.spur/graph-index.pointer.json`.

- [ ] **Step 3: Confirm invariant holds**

Run:

```bash
jq -r .graph_content_hash .spur/graph-index.pointer.json
duckdb .spur/analyst.duckdb "SELECT graph_content_hash FROM _meta;"
```

Expected: identical hashes. This is the spec's core invariant.

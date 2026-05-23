//! `spur-cli analyst build` - rebuild `.spur/analyst.duckdb` from the current
//! spur-graph parquet artifact.
//!
//! See: `docs/superpowers/specs/2026-05-22-analyst-db-graph-sync-design.md`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use spur_graph::locking::try_lock_exclusive_with_timeout;

const INIT_SQL: &str = include_str!("../../../spur-context/poc/duckdb-analyst/init.sql");
const ARTIFACT_PLACEHOLDER: &str = "__SPUR_GRAPH_ARTIFACT_DIR__";

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
pub fn build(root: &Path, options: AnalystBuildOptions) -> Result<()> {
    let quiet = options.quiet;
    let artifact_dir = resolve_artifact_dir(root, &options)?;
    verify_schema_version(&artifact_dir)?;
    verify_required_files(&artifact_dir)?;

    if !duckdb_cli_present() {
        if !quiet {
            eprintln!(
                "[spur] warning: 'duckdb' CLI not found on PATH - skipping analyst DB refresh"
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

/// Convenience wrapper used by `spur-cli graph build` to invoke the default
/// build with only the quiet flag threaded through.
pub fn build_default(root: &Path, quiet: bool) -> Result<()> {
    build(
        root,
        AnalystBuildOptions {
            quiet,
            ..Default::default()
        },
    )
}

fn short_hash(hash: &str) -> String {
    if hash.len() > 8 {
        format!("{}...", &hash[..8])
    } else {
        hash.to_string()
    }
}

pub(crate) fn resolve_artifact_dir(root: &Path, options: &AnalystBuildOptions) -> Result<PathBuf> {
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
        "spur-graph CURRENT pointer not found at {} - run `spur-cli graph build --workspace` \
         or set SPUR_GRAPH_ARTIFACT_DIR to a parquet artifact directory",
        current.display()
    ))
}

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
        assert!(
            msg.contains("tombstones.parquet"),
            "missing tombstones in: {msg}"
        );
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

        assert!(
            tmp_db.is_file(),
            "db file not created at {}",
            tmp_db.display()
        );
        let _ = std::fs::remove_file(&tmp_db);
    }
}

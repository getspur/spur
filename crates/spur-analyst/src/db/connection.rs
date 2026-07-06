use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(test)]
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use anyhow::{Context as _, Result};

use crate::api::AnalystDuckDbResourceCaps;

use super::sql::sql_string_literal;

pub(crate) const DEFAULT_ANALYST_DUCKDB_MEMORY_LIMIT: &str = "4GB";
pub(crate) const DEFAULT_ANALYST_DUCKDB_THREADS: usize = 4;
pub(crate) const ANALYST_DUCKDB_MEMORY_LIMIT_ENV: &str = "SPUR_ANALYST_DUCKDB_MEMORY_LIMIT";
pub(crate) const ANALYST_DUCKDB_THREADS_ENV: &str = "SPUR_ANALYST_DUCKDB_THREADS";

static ANALYST_TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
static ANALYST_CONNECTION_OPEN_COUNTS: OnceLock<Mutex<HashMap<PathBuf, usize>>> = OnceLock::new();

pub(crate) fn open_analyst_connection_read_only(db_path: &Path) -> Result<duckdb::Connection> {
    open_analyst_connection_read_only_with_caps(db_path, AnalystDuckDbResourceCaps::from_env())
}

#[cfg(test)]
pub(crate) fn reset_analyst_connection_open_count_for_test(db_path: &Path) {
    let counts = ANALYST_CONNECTION_OPEN_COUNTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut counts = counts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    counts.remove(db_path);
}

#[cfg(test)]
pub(crate) fn analyst_connection_open_count_for_test(db_path: &Path) -> usize {
    let Some(counts) = ANALYST_CONNECTION_OPEN_COUNTS.get() else {
        return 0;
    };
    let counts = counts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    counts.get(db_path).copied().unwrap_or(0)
}

#[cfg(test)]
fn record_analyst_connection_open_for_test(db_path: &Path) {
    let counts = ANALYST_CONNECTION_OPEN_COUNTS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut counts = counts
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *counts.entry(db_path.to_path_buf()).or_insert(0) += 1;
}

pub(crate) fn open_analyst_connection_read_only_with_caps(
    db_path: &Path,
    caps: AnalystDuckDbResourceCaps,
) -> Result<duckdb::Connection> {
    let config = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .context("failed to configure read-only duckdb")?;
    let conn = duckdb::Connection::open_with_flags(db_path, config).with_context(|| {
        format!(
            "failed to open analyst DuckDB read-only at {}",
            db_path.display()
        )
    })?;
    let temp_dir = analyst_connection_temp_directory(db_path);
    apply_analyst_duckdb_resource_caps(&conn, &caps, &temp_dir)?;
    #[cfg(test)]
    record_analyst_connection_open_for_test(db_path);
    Ok(conn)
}

fn analyst_connection_temp_directory(db_path: &Path) -> PathBuf {
    // DuckDB spill file names are not safe to share across independent
    // processes; keep every analyst connection on private temp storage.
    let counter = ANALYST_TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    analyst_temp_directory_parent(db_path).join(format!("ro-{}-{counter}", std::process::id()))
}

fn analyst_temp_directory_parent(db_path: &Path) -> PathBuf {
    let mut file_name = db_path
        .file_name()
        .map(|file_name| file_name.to_owned())
        .unwrap_or_else(|| "analyst.duckdb".into());
    file_name.push(".tmp");
    db_path
        .parent()
        .map(|parent| parent.join(&file_name))
        .unwrap_or_else(|| PathBuf::from(file_name))
}

fn apply_analyst_duckdb_resource_caps(
    conn: &duckdb::Connection,
    caps: &AnalystDuckDbResourceCaps,
    temp_dir: &Path,
) -> Result<()> {
    std::fs::create_dir_all(temp_dir).with_context(|| {
        format!(
            "failed to create analyst DuckDB temp directory {}",
            temp_dir.display()
        )
    })?;

    let memory_limit = caps.memory_limit.replace('\'', "''");
    let temp_directory = sql_string_literal(&temp_dir.to_string_lossy());
    // preserve_insertion_order=false shrinks the working set of large
    // aggregations; analyst queries that care about order use explicit
    // ORDER BY clauses.
    conn.execute_batch(&format!(
        "SET temp_directory = {temp_directory};\nSET memory_limit = '{memory_limit}';\nSET threads = {};\nSET preserve_insertion_order = false;",
        caps.threads,
    ))
    .with_context(|| {
        format!(
            "failed to apply analyst DuckDB resource caps (memory_limit={:?}, threads={}, temp_directory={})",
            caps.memory_limit,
            caps.threads,
            temp_dir.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use duckdb::Connection;

    use crate::AnalystDuckDbResourceCaps;

    use super::open_analyst_connection_read_only_with_caps;

    #[test]
    fn read_only_connection_helper_applies_resource_caps() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = temp_dir.path().join("analyst.duckdb");
        drop(Connection::open(&db_path).expect("create fixture db"));

        let conn: Connection = open_analyst_connection_read_only_with_caps(
            &db_path,
            AnalystDuckDbResourceCaps {
                memory_limit: "64MiB".to_owned(),
                threads: 2,
            },
        )
        .expect("open read-only connection with caps");
        let memory_limit: String = conn
            .query_row(
                "SELECT current_setting('memory_limit')::VARCHAR",
                [],
                |row: &duckdb::Row<'_>| row.get::<_, String>(0),
            )
            .expect("read memory_limit setting");
        let threads: i64 = conn
            .query_row(
                "SELECT current_setting('threads')::INTEGER",
                [],
                |row: &duckdb::Row<'_>| row.get::<_, i64>(0),
            )
            .expect("read threads setting");

        assert_eq!(memory_limit, "64.0 MiB");
        assert_eq!(threads, 2);
    }

    #[test]
    fn read_only_connections_use_distinct_temp_directories() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = temp_dir.path().join("analyst.duckdb");
        drop(Connection::open(&db_path).expect("create fixture db"));

        let first = open_analyst_connection_read_only_with_caps(
            &db_path,
            AnalystDuckDbResourceCaps {
                memory_limit: "64MiB".to_owned(),
                threads: 1,
            },
        )
        .expect("open first read-only connection");
        let second = open_analyst_connection_read_only_with_caps(
            &db_path,
            AnalystDuckDbResourceCaps {
                memory_limit: "64MiB".to_owned(),
                threads: 1,
            },
        )
        .expect("open second read-only connection");

        let first_temp: String = first
            .query_row(
                "SELECT current_setting('temp_directory')::VARCHAR",
                [],
                |row: &duckdb::Row<'_>| row.get::<_, String>(0),
            )
            .expect("read first temp_directory setting");
        let second_temp: String = second
            .query_row(
                "SELECT current_setting('temp_directory')::VARCHAR",
                [],
                |row: &duckdb::Row<'_>| row.get::<_, String>(0),
            )
            .expect("read second temp_directory setting");

        assert_ne!(
            first_temp, second_temp,
            "concurrent analyst connections must not share DuckDB spill files"
        );
        assert!(
            first_temp.contains("analyst.duckdb.tmp"),
            "expected db-local temp directory, got {first_temp}"
        );
        assert!(
            second_temp.contains("analyst.duckdb.tmp"),
            "expected db-local temp directory, got {second_temp}"
        );
    }

    fn humanized_memory_setting_bytes(setting: &str) -> f64 {
        let mut parts = setting.split_whitespace();
        let value: f64 = parts
            .next()
            .expect("memory setting value")
            .parse()
            .expect("numeric memory setting value");
        let multiplier: f64 = match parts.next().expect("memory setting unit") {
            "KiB" => 1024.0,
            "MiB" => 1024.0 * 1024.0,
            "GiB" => 1024.0 * 1024.0 * 1024.0,
            "TiB" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
            other => panic!("unexpected memory setting unit: {other}"),
        };
        value * multiplier
    }

    fn open_connection_with_default_caps() -> (tempfile::TempDir, Connection) {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = temp_dir.path().join("analyst.duckdb");
        drop(Connection::open(&db_path).expect("create fixture db"));

        let conn = open_analyst_connection_read_only_with_caps(
            &db_path,
            AnalystDuckDbResourceCaps::default(),
        )
        .expect("open read-only connection with default caps");
        (temp_dir, conn)
    }

    #[test]
    fn default_memory_limit_supports_hybrid_search_workloads() {
        // The old 512MB default starved real-index hybrid context-candidate
        // queries into DuckDB OOM, whose abort path can double-free aggregate
        // state and SIGABRT the whole process (duckdb/duckdb#19391). Keep the
        // default cap at or above 2GB, the verified floor for those queries.
        let (_temp_dir, conn) = open_connection_with_default_caps();
        let memory_limit: String = conn
            .query_row(
                "SELECT current_setting('memory_limit')::VARCHAR",
                [],
                |row: &duckdb::Row<'_>| row.get::<_, String>(0),
            )
            .expect("read memory_limit setting");

        assert!(
            humanized_memory_setting_bytes(&memory_limit) >= 2_000_000_000.0,
            "default analyst memory limit {memory_limit} is below the 2GB floor"
        );
    }

    #[test]
    fn resource_caps_disable_insertion_order_preservation() {
        // Insertion-order preservation inflates the working set of large
        // aggregations; every analyst query that cares about order has an
        // explicit ORDER BY, so trade it away for OOM headroom.
        let (_temp_dir, conn) = open_connection_with_default_caps();
        let preserve_insertion_order: bool = conn
            .query_row(
                "SELECT current_setting('preserve_insertion_order')::BOOLEAN",
                [],
                |row: &duckdb::Row<'_>| row.get::<_, bool>(0),
            )
            .expect("read preserve_insertion_order setting");

        assert!(
            !preserve_insertion_order,
            "analyst connections must disable insertion-order preservation"
        );
    }
}

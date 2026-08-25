use anyhow::{Context as _, Result};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Primary env var for a vendored DuckDB extension tree.
///
/// Layout DuckDB expects under this directory:
/// `{dir}/v<duckdb-version>/<platform>/<name>.duckdb_extension`
pub const ANALYST_EXTENSION_DIR_ENV: &str = "SPUR_DUCKDB_EXTENSION_DIR";
const CONTEXT_EXTENSION_DIR_ENV: &str = "SPUR_CONTEXT_DUCKDB_EXTENSION_DIR";

const ANALYST_EXTENSIONS: &[(&str, bool)] = &[
    ("duckpgq", true),
    ("onager", true),
    ("fts", false),
    ("icu", false),
];

// duckpgq is distributed via DuckDB's community repo (not the core repo) and is
// only published for DuckDB <= 1.4.4. Install it once per process when no local
// extension directory is configured; the LOAD is best-effort in general query
// paths and fallible in graph-path paths.
static DUCKPGQ_INSTALLED: OnceLock<()> = OnceLock::new();

/// Directory of vendored DuckDB extensions, if one is configured or shipped
/// next to the running binary.
pub fn analyst_extension_directory() -> Option<PathBuf> {
    env_extension_dir(ANALYST_EXTENSION_DIR_ENV)
        .or_else(|| env_extension_dir(CONTEXT_EXTENSION_DIR_ENV))
        .or_else(bundled_extension_directory)
}

/// SQL that LOADs analyst extensions without hitting the CDN when a local
/// extension directory is set (`autoinstall_known_extensions = false`).
pub fn analyst_extension_bootstrap_sql() -> String {
    match analyst_extension_directory() {
        Some(dir) => {
            let mut sql = extension_directory_prefix(&dir);
            for (name, _) in ANALYST_EXTENSIONS {
                sql.push_str("LOAD ");
                sql.push_str(name);
                sql.push_str(";\n");
            }
            sql
        }
        None => {
            let mut sql = String::new();
            for (name, community) in ANALYST_EXTENSIONS {
                sql.push_str("INSTALL ");
                sql.push_str(name);
                if *community {
                    sql.push_str(" FROM community");
                }
                sql.push_str(";\n");
            }
            for (name, _) in ANALYST_EXTENSIONS {
                sql.push_str("LOAD ");
                sql.push_str(name);
                sql.push_str(";\n");
            }
            sql
        }
    }
}

/// INSTALL/LOAD SQL for a single extension, preferring a local directory.
pub fn analyst_extension_load_sql(name: &str) -> String {
    debug_assert!(name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'));
    let community = ANALYST_EXTENSIONS
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .is_some_and(|(_, community)| *community);
    match analyst_extension_directory() {
        Some(dir) => {
            let mut sql = extension_directory_prefix(&dir);
            sql.push_str("LOAD ");
            sql.push_str(name);
            sql.push(';');
            sql
        }
        None if community => format!("INSTALL {name} FROM community; LOAD {name};"),
        None => format!("INSTALL {name}; LOAD {name};"),
    }
}

pub(crate) fn load_analyst_icu_extension(conn: &duckdb::Connection) {
    // The analyst scorecard can depend on TIMESTAMPTZ arithmetic whose overloads
    // live in DuckDB's ICU extension. Keep this best-effort and let query
    // preparation surface genuine failures in the existing per-stage shape.
    let _ = conn.execute_batch(&analyst_extension_load_sql("icu"));
}

pub(crate) fn load_analyst_duckpgq_extension(conn: &duckdb::Connection) -> Result<()> {
    DUCKPGQ_INSTALLED.get_or_init(|| {
        if analyst_extension_directory().is_none() {
            let _ = conn.execute_batch("INSTALL duckpgq FROM community;");
        }
    });
    conn.execute_batch(&analyst_extension_load_sql("duckpgq"))
        .context("failed to load DuckPGQ extension")?;
    Ok(())
}

fn env_extension_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

fn bundled_extension_directory() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    [
        exe_dir.join("duckdb-extensions"),
        share_extension_dir(exe_dir),
    ]
    .into_iter()
    .find(|path| path.is_dir())
    .and_then(|path| std::fs::canonicalize(path).ok())
}

fn share_extension_dir(exe_dir: &Path) -> PathBuf {
    exe_dir
        .join("..")
        .join("share")
        .join("spur")
        .join("duckdb-extensions")
}

fn extension_directory_prefix(dir: &Path) -> String {
    format!(
        "SET extension_directory = '{}'; \
         SET autoinstall_known_extensions = false; \
         SET autoload_known_extensions = false;\n",
        escape_sql_literal(&dir.display().to_string())
    )
}

fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

    fn with_extension_dir<R>(dir: Option<&str>, body: impl FnOnce() -> R) -> R {
        let _guard = env_lock();
        let previous_analyst = std::env::var_os(ANALYST_EXTENSION_DIR_ENV);
        let previous_context = std::env::var_os(CONTEXT_EXTENSION_DIR_ENV);
        match dir {
            Some(value) => std::env::set_var(ANALYST_EXTENSION_DIR_ENV, value),
            None => std::env::remove_var(ANALYST_EXTENSION_DIR_ENV),
        }
        std::env::remove_var(CONTEXT_EXTENSION_DIR_ENV);
        let result = body();
        match previous_analyst {
            Some(value) => std::env::set_var(ANALYST_EXTENSION_DIR_ENV, value),
            None => std::env::remove_var(ANALYST_EXTENSION_DIR_ENV),
        }
        match previous_context {
            Some(value) => std::env::set_var(CONTEXT_EXTENSION_DIR_ENV, value),
            None => std::env::remove_var(CONTEXT_EXTENSION_DIR_ENV),
        }
        result
    }

    #[test]
    fn bootstrap_sql_skips_install_when_extension_dir_is_set() {
        let sql = with_extension_dir(Some("/opt/duckdb/extensions/with ' quote"), || {
            analyst_extension_bootstrap_sql()
        });
        assert!(
            !sql.contains("INSTALL "),
            "offline bootstrap must not INSTALL from the CDN: {sql}"
        );
        assert!(sql.contains("SET autoinstall_known_extensions = false"));
        assert!(sql.contains("SET autoload_known_extensions = false"));
        assert!(sql.contains("SET extension_directory = '/opt/duckdb/extensions/with '' quote'"));
        assert!(sql.contains("LOAD duckpgq;"));
        assert!(sql.contains("LOAD onager;"));
        assert!(!sql.contains("LOAD lance;"));
        assert!(sql.contains("LOAD fts;"));
        assert!(sql.contains("LOAD icu;"));
    }

    #[test]
    fn bootstrap_sql_installs_from_community_when_extension_dir_unset() {
        let sql = with_extension_dir(None, analyst_extension_bootstrap_sql);
        assert!(sql.contains("INSTALL duckpgq FROM community;"));
        assert!(sql.contains("INSTALL onager FROM community;"));
        assert!(!sql.contains("INSTALL lance;"));
        assert!(
            !sql.contains("SET autoinstall_known_extensions = false"),
            "home-directory flow still uses INSTALL: {sql}"
        );
    }

    #[test]
    fn load_sql_uses_community_only_for_duckpgq_and_onager() {
        let duckpgq = with_extension_dir(None, || analyst_extension_load_sql("duckpgq"));
        assert_eq!(duckpgq, "INSTALL duckpgq FROM community; LOAD duckpgq;");
    }
}

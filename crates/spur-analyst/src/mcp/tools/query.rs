use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

use serde_json::{json, Value};

use crate::db::{
    connection::open_analyst_connection_read_only,
    extensions::{
        load_analyst_duckpgq_extension, load_analyst_icu_extension, load_analyst_lance_extension,
    },
    freshness::{freshness_gate, FreshnessGate},
    paths::analyst_db_path,
    sql::{query_rows, MAX_QUERY_ROWS},
};
use crate::mcp::McpHandlerError;

type PooledQueryConnection = Arc<Mutex<crate::db::connection::AnalystReadOnlyConnection>>;

static QUERY_CONNECTION_POOL: OnceLock<Mutex<HashMap<PathBuf, PooledQueryConnection>>> =
    OnceLock::new();

pub async fn query(args: &Value) -> Result<Value, McpHandlerError> {
    let request = QueryRequest::parse(args)?;
    reject_write_statement(&request.query)?;
    let db_path = analyst_db_path()?;
    match query_read_only(&db_path, &request.query, request.allow_stale) {
        Ok(result) => Ok(result),
        Err(error) => Ok(query_error(&db_path, &error)),
    }
}

struct QueryRequest {
    query: String,
    allow_stale: bool,
}

impl QueryRequest {
    fn parse(args: &Value) -> Result<Self, McpHandlerError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| {
                McpHandlerError::InvalidParams(
                    "query requires non-empty string field 'query'".into(),
                )
            })?
            .to_owned();
        let allow_stale = args
            .get("allow_stale")
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    McpHandlerError::InvalidParams(
                        "query field 'allow_stale' must be a boolean".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(false);
        Ok(Self { query, allow_stale })
    }
}

fn reject_write_statement(query: &str) -> Result<(), McpHandlerError> {
    let token = first_token(query).to_ascii_uppercase();
    if matches!(
        token.as_str(),
        "INSERT"
            | "UPDATE"
            | "DELETE"
            | "CREATE"
            | "ALTER"
            | "DROP"
            | "ATTACH"
            | "DETACH"
            | "COPY"
            | "PRAGMA"
            | "CALL"
            | "EXPORT"
            | "BEGIN"
            | "COMMIT"
            | "ROLLBACK"
            | "CHECKPOINT"
            | "VACUUM"
            | "REVOKE"
            | "GRANT"
            | "SET"
    ) {
        return Err(McpHandlerError::InvalidParams(format!(
            "query is read-only; statement token {token} is not allowed"
        )));
    }
    Ok(())
}

fn first_token(query: &str) -> &str {
    let trimmed = query.trim_start();
    let end = trimmed
        .find(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .unwrap_or(trimmed.len());
    &trimmed[..end]
}

fn query_read_only(db_path: &Path, sql: &str, allow_stale: bool) -> Result<Value, McpHandlerError> {
    let conn = pooled_query_connection(db_path)?;
    let conn = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let staleness_warning = match freshness_gate(&conn, db_path, allow_stale)? {
        FreshnessGate::Proceed { warning } => warning,
        FreshnessGate::Block(response) => return Ok(response),
    };

    let result = query_rows(&conn, sql, MAX_QUERY_ROWS)?;
    let row_count = result.row_count();

    let mut response = json!({
        "db_path": db_path.display().to_string(),
        "columns": result.columns,
        "rows": result.rows,
        "row_count": row_count,
        "truncated": result.truncated
    });
    if let Some(warning) = staleness_warning {
        response["staleness_warning"] = Value::String(warning);
    }
    Ok(response)
}

fn pooled_query_connection(db_path: &Path) -> Result<PooledQueryConnection, McpHandlerError> {
    let pool = QUERY_CONNECTION_POOL.get_or_init(|| Mutex::new(HashMap::new()));
    let mut pool = pool.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(conn) = pool.get(db_path) {
        return Ok(Arc::clone(conn));
    }

    let conn = open_analyst_connection_read_only(db_path)
        .map_err(|error| McpHandlerError::Internal(format!("{error:#}")))?;
    load_analyst_icu_extension(&conn);
    load_analyst_lance_extension(&conn);
    let _ = load_analyst_duckpgq_extension(&conn);

    let conn = Arc::new(Mutex::new(conn));
    pool.insert(db_path.to_path_buf(), Arc::clone(&conn));
    Ok(conn)
}

fn query_error(db_path: &Path, error: &McpHandlerError) -> Value {
    json!({
        "db_path": db_path.display().to_string(),
        "error": {
            "code": query_error_code(error),
            "message": error.to_string()
        }
    })
}

fn query_error_code(error: &McpHandlerError) -> &'static str {
    match error {
        McpHandlerError::InvalidParams(_) => "invalid_params",
        McpHandlerError::NotFound(_) => "not_found",
        McpHandlerError::Unauthorized(_) => "unauthorized",
        McpHandlerError::UpstreamPm(_) => "upstream_pm",
        McpHandlerError::Internal(_) => "internal",
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use duckdb::Connection;

    use super::*;

    #[test]
    fn query_read_only_reuses_one_connection_for_concurrent_calls_to_same_db_path() {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let db_path = temp_dir.path().join("analyst.duckdb");
        let conn = Connection::open(&db_path).expect("create fixture db");
        conn.execute_batch(
            "
            CREATE TABLE facts (value INTEGER);
            INSERT INTO facts VALUES (42);
            ",
        )
        .expect("seed fixture db");
        drop(conn);
        crate::db::connection::reset_analyst_connection_open_count_for_test(&db_path);

        let db_path = Arc::new(db_path);
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let db_path = Arc::clone(&db_path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let result = query_read_only(&db_path, "SELECT value FROM facts", true)
                        .expect("query read-only");
                    assert_eq!(result["rows"], serde_json::json!([[42]]));
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("query thread");
        }

        assert_eq!(
            crate::db::connection::analyst_connection_open_count_for_test(&db_path),
            1,
            "query handler should pool one read-only connection per analyst DB path"
        );
    }
}

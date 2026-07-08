use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{db::sql::query_rows, mcp::McpHandlerError};

const STALE_ANALYST_DB_MESSAGE: &str =
    "The analyst DB lags the live graph. Run `spur graph build` to refresh, or set allow_stale=true to override.";
const INCOMPLETE_ANALYST_DB_MESSAGE: &str =
    "The analyst DB graph index is incomplete. Run `spur graph build` to finish rebuilding before querying.";

pub(crate) enum FreshnessGate {
    Proceed { warning: Option<String> },
    Block(Value),
}

struct AnalystMeta {
    graph_content_hash: Option<String>,
    complete: Option<bool>,
}

pub(crate) fn freshness_gate(
    conn: &duckdb::Connection,
    db_path: &Path,
    allow_stale: bool,
) -> Result<FreshnessGate, McpHandlerError> {
    if allow_stale {
        return Ok(FreshnessGate::Proceed {
            warning: Some("allow_stale".into()),
        });
    }

    let Some(live_hash) = read_live_graph_hash(db_path)? else {
        return Ok(FreshnessGate::Proceed {
            warning: Some("no_live_pointer".into()),
        });
    };
    let analyst_meta = query_analyst_meta(conn)?;
    let analyst_hash = analyst_meta
        .as_ref()
        .and_then(|meta| meta.graph_content_hash.clone());
    if let Some(meta) = &analyst_meta {
        if meta.complete != Some(true) {
            return Ok(FreshnessGate::Block(json!({
                "error": "analyst_db_incomplete",
                "analyst_hash": analyst_hash,
                "analyst_complete": meta.complete,
                "live_hash": live_hash,
                "message": INCOMPLETE_ANALYST_DB_MESSAGE
            })));
        }
    }
    if analyst_hash.as_deref() == Some(live_hash.as_str()) {
        return Ok(FreshnessGate::Proceed { warning: None });
    }

    Ok(FreshnessGate::Block(json!({
        "error": "analyst_db_stale",
        "analyst_hash": analyst_hash,
        "live_hash": live_hash,
        "message": STALE_ANALYST_DB_MESSAGE
    })))
}

#[cfg(test)]
fn query_analyst_graph_hash(conn: &duckdb::Connection) -> Result<Option<String>, McpHandlerError> {
    Ok(query_analyst_meta(conn)?.and_then(|meta| meta.graph_content_hash))
}

fn query_analyst_meta(conn: &duckdb::Connection) -> Result<Option<AnalystMeta>, McpHandlerError> {
    let result = query_rows(
        conn,
        "SELECT graph_content_hash, complete FROM _meta LIMIT 1",
        1,
    )?;
    let Some(row) = result.rows.first().and_then(Value::as_array) else {
        return Ok(None);
    };
    let graph_content_hash = row.first().and_then(Value::as_str).map(ToOwned::to_owned);
    let complete = row.get(1).and_then(Value::as_bool);
    Ok(Some(AnalystMeta {
        graph_content_hash,
        complete,
    }))
}

fn read_live_graph_hash(db_path: &Path) -> Result<Option<String>, McpHandlerError> {
    let Some(spur_dir) = db_path.parent() else {
        return Ok(None);
    };
    for pointer_path in live_pointer_paths(spur_dir) {
        match fs::read(&pointer_path) {
            Ok(bytes) => {
                let pointer: LiveGraphPointer =
                    serde_json::from_slice(&bytes).map_err(|error| {
                        McpHandlerError::Internal(format!(
                            "invalid live graph pointer `{}`: {error}",
                            pointer_path.display()
                        ))
                    })?;
                return Ok(Some(pointer.graph_content_hash));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(McpHandlerError::Internal(format!(
                    "failed to read live graph pointer `{}`: {error}",
                    pointer_path.display()
                )));
            }
        }
    }
    Ok(None)
}

fn live_pointer_paths(spur_dir: &Path) -> [PathBuf; 2] {
    [
        spur_dir.join("graph").join("pointer.json"),
        spur_dir.join("graph-index.pointer.json"),
    ]
}

#[derive(Deserialize)]
struct LiveGraphPointer {
    graph_content_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn conn_with_meta(hash_expr: &str, complete_expr: &str) -> duckdb::Connection {
        let conn = duckdb::Connection::open_in_memory().expect("open in-memory duckdb");
        conn.execute_batch(&format!(
            "CREATE TABLE _meta AS SELECT {hash_expr} AS graph_content_hash, {complete_expr} AS complete;"
        ))
        .expect("create _meta");
        conn
    }

    fn conn_with_empty_meta() -> duckdb::Connection {
        let conn = duckdb::Connection::open_in_memory().expect("open in-memory duckdb");
        conn.execute_batch("CREATE TABLE _meta (graph_content_hash VARCHAR, complete BOOLEAN);")
            .expect("create empty _meta");
        conn
    }

    fn db_path_with_live_hash(hash: &str) -> (tempfile::TempDir, PathBuf) {
        let temp_dir = tempfile::tempdir().expect("create tempdir");
        let graph_dir = temp_dir.path().join("graph");
        fs::create_dir(&graph_dir).expect("create graph dir");
        fs::write(
            graph_dir.join("pointer.json"),
            json!({ "graph_content_hash": hash }).to_string(),
        )
        .expect("write live graph pointer");
        let db_path = temp_dir.path().join("analyst.duckdb");
        (temp_dir, db_path)
    }

    #[test]
    fn freshness_gate_allows_complete_matching_hash() {
        let conn = conn_with_meta("'live-hash'", "TRUE");
        let (_temp_dir, db_path) = db_path_with_live_hash("live-hash");

        let gate = freshness_gate(&conn, &db_path, false).expect("freshness gate");

        match gate {
            FreshnessGate::Proceed { warning } => assert_eq!(warning, None),
            FreshnessGate::Block(response) => panic!("unexpected block: {response}"),
        }
    }

    #[test]
    fn freshness_gate_blocks_incomplete_false_even_when_hash_matches() {
        let conn = conn_with_meta("'live-hash'", "FALSE");
        let (_temp_dir, db_path) = db_path_with_live_hash("live-hash");

        let gate = freshness_gate(&conn, &db_path, false).expect("freshness gate");

        match gate {
            FreshnessGate::Block(response) => {
                assert_eq!(response["error"], json!("analyst_db_incomplete"));
                assert_eq!(response["analyst_hash"], json!("live-hash"));
                assert_eq!(response["live_hash"], json!("live-hash"));
            }
            FreshnessGate::Proceed { warning } => panic!("unexpected proceed: {warning:?}"),
        }
    }

    #[test]
    fn freshness_gate_blocks_incomplete_null() {
        let conn = conn_with_meta("'live-hash'", "NULL");
        let (_temp_dir, db_path) = db_path_with_live_hash("live-hash");

        let gate = freshness_gate(&conn, &db_path, false).expect("freshness gate");

        match gate {
            FreshnessGate::Block(response) => {
                assert_eq!(response["error"], json!("analyst_db_incomplete"));
                assert_eq!(response["analyst_complete"], Value::Null);
            }
            FreshnessGate::Proceed { warning } => panic!("unexpected proceed: {warning:?}"),
        }
    }

    #[test]
    fn missing_meta_row_returns_none_and_stale_response() {
        let conn = conn_with_empty_meta();
        let (_temp_dir, db_path) = db_path_with_live_hash("live-hash");

        assert_eq!(query_analyst_graph_hash(&conn).expect("query hash"), None);
        let gate = freshness_gate(&conn, &db_path, false).expect("freshness gate");

        match gate {
            FreshnessGate::Block(response) => {
                assert_eq!(response["error"], json!("analyst_db_stale"));
                assert_eq!(response["analyst_hash"], Value::Null);
                assert_eq!(response["live_hash"], json!("live-hash"));
            }
            FreshnessGate::Proceed { warning } => panic!("unexpected proceed: {warning:?}"),
        }
    }

    #[test]
    fn null_or_non_string_graph_hash_returns_none_and_stale_response() {
        for hash_expr in ["NULL", "42"] {
            let conn = conn_with_meta(hash_expr, "TRUE");
            let (_temp_dir, db_path) = db_path_with_live_hash("live-hash");

            assert_eq!(query_analyst_graph_hash(&conn).expect("query hash"), None);
            let gate = freshness_gate(&conn, &db_path, false).expect("freshness gate");

            match gate {
                FreshnessGate::Block(response) => {
                    assert_eq!(response["error"], json!("analyst_db_stale"));
                    assert_eq!(response["analyst_hash"], Value::Null);
                    assert_eq!(response["live_hash"], json!("live-hash"));
                }
                FreshnessGate::Proceed { warning } => {
                    panic!("unexpected proceed for {hash_expr}: {warning:?}")
                }
            }
        }
    }
}

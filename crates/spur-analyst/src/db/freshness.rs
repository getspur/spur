use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{db::sql::query_rows, mcp::McpHandlerError};

const STALE_ANALYST_DB_MESSAGE: &str =
    "The analyst DB lags the live graph. Run `spur graph build` to refresh, or set allow_stale=true to override.";

pub(crate) enum FreshnessGate {
    Proceed { warning: Option<String> },
    Block(Value),
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
    let analyst_hash = query_analyst_graph_hash(conn)?;
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

pub(crate) fn query_analyst_graph_hash(
    conn: &duckdb::Connection,
) -> Result<Option<String>, McpHandlerError> {
    let result = query_rows(conn, "SELECT graph_content_hash FROM _meta LIMIT 1", 1)?;
    let Some(row) = result.rows.first().and_then(Value::as_array) else {
        return Ok(None);
    };
    row.first()
        .and_then(Value::as_str)
        .map(|value| Some(value.to_owned()))
        .ok_or_else(|| {
            McpHandlerError::Internal(
                "failed to read analyst graph_content_hash: expected string".into(),
            )
        })
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

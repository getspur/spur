//! `code_semantic_search` — BM25 retrieval over the analyst DuckDB's doc + code
//! corpora (`search()` / `search_docs()` / `search_code()` macros).
//!
//! Complements `code_symbol_search` (spur-mcp): that tool matches symbol *names*
//! lexically; this one ranks *content* (section bodies + symbol token text) by
//! relevance, and — for code hits — fuses the scorecard signal (centrality /
//! churn / posture). Reads `.spur/analyst.duckdb` read-only via the bundled
//! libduckdb; only the `fts` core extension is needed (it autoloads), so the
//! community extensions used to BUILD the DB are irrelevant to the read path.

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

const METHOD: &str = "code_semantic_search";
const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;

#[derive(Debug, Deserialize)]
struct Params {
    query: String,
    /// "all" (docs + code), "docs", or "code". Defaults to "all".
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// Optional explicit path to analyst.duckdb; otherwise discovered from cwd.
    #[serde(default)]
    db_path: Option<String>,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Semantic (BM25) search over the analyst index: ranks documentation/skill/plan section bodies AND code (symbol token text) by relevance to a natural-language query. Code hits carry their scorecard signal (pagerank/churn/posture). Use this for concept/content questions ('how does oauth refresh work', 'where is the review gate documented'); use code_symbol_search for exact symbol-name lookup and code_callers/code_callees for the call graph.",
        rmcp_object(json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": { "type": "string", "minLength": 1 },
                "scope": { "type": "string", "enum": ["all", "docs", "code"], "default": "all" },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT },
                "db_path": { "type": "string", "description": "Optional path to analyst.duckdb; discovered from cwd upward by default." }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(
    _deps: &crate::mcp::ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: Params = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{METHOD} requires {{ query, scope?, limit?, db_path? }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.query.trim().is_empty() {
        return Err(McpError::invalid_params(
            format!("{METHOD} query must be non-empty"),
            None,
        ));
    }
    let scope = params.scope.as_deref().unwrap_or("all");
    if !matches!(scope, "all" | "docs" | "code") {
        return Err(McpError::invalid_params(
            format!("{METHOD} scope must be one of all|docs|code"),
            Some(json!({ "scope": scope })),
        ));
    }
    let limit = params.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);

    run(&params.query, scope, limit, params.db_path.as_deref())
}

#[cfg(feature = "datasource-introspect")]
fn run(
    query: &str,
    scope: &str,
    limit: usize,
    db_path: Option<&str>,
) -> Result<CallToolResult, McpError> {
    let (path, results, graph_hash) = search_rows(query, scope, limit, db_path)?;
    Ok(CallToolResult::structured(json!({
        "scope": scope,
        "query": query,
        "count": results.len(),
        "results": results,
        "graph_content_hash": graph_hash,
        "db_path": path,
    })))
}

/// Core query: open analyst.duckdb read-only and run the chosen `search*` macro.
/// Returns (resolved_db_path, rows, graph_content_hash).
#[cfg(feature = "datasource-introspect")]
fn search_rows(
    query: &str,
    scope: &str,
    limit: usize,
    db_path: Option<&str>,
) -> Result<(String, Vec<Value>, Option<String>), McpError> {
    let path = resolve_db_path(db_path)?;

    // Read-only so the tool coexists with the read-only spur-analyst MCP and never
    // takes a write lock on the live DB.
    let config = duckdb::Config::default()
        .access_mode(duckdb::AccessMode::ReadOnly)
        .map_err(|e| internal("failed to configure read-only duckdb", &e))?;
    let conn = duckdb::Connection::open_with_flags(&path, config).map_err(|e| {
        McpError::internal_error(
            format!("{METHOD} could not open analyst.duckdb (version skew between writer and bundled reader can cause this)"),
            Some(json!({ "db_path": path.display().to_string(), "error": e.to_string() })),
        )
    })?;

    // The macro arg flows into FTS match_bm25, which wants a constant — inline the
    // query as an escaped string literal rather than a bind parameter.
    let q = query.replace('\'', "''");
    let sql = match scope {
        "docs" => format!(
            "SELECT 'doc' AS kind, section AS title, file_path AS file, \
             round(bm25, 3) AS score, CAST(NULL AS VARCHAR) AS signal \
             FROM search_docs('{q}') LIMIT {limit}"
        ),
        "code" => format!(
            "SELECT 'code' AS kind, symbol AS title, file_path AS file, \
             round(bm25, 3) AS score, posture AS signal \
             FROM search_code('{q}') LIMIT {limit}"
        ),
        _ => format!(
            "SELECT kind, title, file, round(score, 3) AS score, signal \
             FROM search('{q}') LIMIT {limit}"
        ),
    };

    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| internal("failed to prepare search query", &e))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(json!({
                "kind": row.get::<_, String>(0)?,
                "title": row.get::<_, String>(1)?,
                "file": row.get::<_, String>(2)?,
                "score": row.get::<_, f64>(3)?,
                "signal": row.get::<_, Option<String>>(4)?,
            }))
        })
        .map_err(|e| internal("failed to run search query", &e))?;
    let results: Vec<Value> = rows
        .collect::<Result<_, _>>()
        .map_err(|e| internal("failed to read search rows", &e))?;

    // Surface the indexed graph hash so callers can detect staleness vs code_* tools.
    let graph_hash: Option<String> = conn
        .query_row("SELECT graph_content_hash FROM _meta", [], |r| r.get(0))
        .ok();

    Ok((path.display().to_string(), results, graph_hash))
}

#[cfg(not(feature = "datasource-introspect"))]
fn run(
    _query: &str,
    _scope: &str,
    _limit: usize,
    _db_path: Option<&str>,
) -> Result<CallToolResult, McpError> {
    Err(McpError::internal_error(
        format!(
            "{METHOD} requires the `datasource-introspect` feature (bundled duckdb) to be enabled"
        ),
        Some(json!({ "code": "duckdb_unavailable" })),
    ))
}

#[cfg(feature = "datasource-introspect")]
fn resolve_db_path(explicit: Option<&str>) -> Result<std::path::PathBuf, McpError> {
    use std::path::PathBuf;
    if let Some(raw) = explicit {
        let pb = PathBuf::from(raw);
        if pb.is_file() {
            return Ok(pb);
        }
        return Err(McpError::invalid_params(
            format!("{METHOD} db_path does not exist: {raw}"),
            Some(json!({ "db_path": raw })),
        ));
    }
    let mut dir = std::env::current_dir().map_err(|e| {
        McpError::internal_error(
            format!("{METHOD} could not read current directory"),
            Some(json!({ "error": e.to_string() })),
        )
    })?;
    loop {
        let candidate = dir.join(".spur").join("analyst.duckdb");
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    Err(McpError::internal_error(
        format!("{METHOD} found no .spur/analyst.duckdb from the working directory upward; run `spur-cli graph build`"),
        Some(json!({ "code": "analyst_db_not_found" })),
    ))
}

#[cfg(feature = "datasource-introspect")]
fn internal(message: &str, error: &impl std::fmt::Display) -> McpError {
    McpError::internal_error(
        format!("{METHOD} {message}"),
        Some(json!({ "error": error.to_string() })),
    )
}

#[cfg(all(test, feature = "datasource-introspect"))]
mod tests {
    use super::*;

    // Builds a tiny analyst-shaped DB with the BUNDLED duckdb (same lib the tool
    // reads with), creates the FTS index + search macro, and asserts the tool's
    // query path returns ranked rows. Proves fts + create_fts_index + the search
    // macro all work with the linked libduckdb — independent of the CLI version.
    #[test]
    fn search_docs_over_bundled_fixture_returns_ranked_rows() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("analyst.duckdb");
        {
            let conn = duckdb::Connection::open(&db)?;
            conn.execute_batch("INSTALL fts; LOAD fts;")?;
            // Separate statements: create_fts_index runs its own internal
            // transaction and must see `sections` already committed, so the
            // table population can't share a batch with the index build.
            conn.execute_batch(
                r#"
                CREATE TABLE sections(stable_symbol_id VARCHAR, section VARCHAR,
                                      file_path VARCHAR, body_text VARCHAR);
                INSERT INTO sections VALUES
                  ('s1','OAuth refresh','docs/oauth.md','how the oauth token refresh grant works'),
                  ('s2','Unrelated','docs/misc.md','notebook cells and kernels');
                CREATE TABLE _meta(graph_content_hash VARCHAR);
                INSERT INTO _meta VALUES ('deadbeef');
                "#,
            )?;
            conn.execute_batch(
                "PRAGMA create_fts_index('sections','stable_symbol_id','body_text', overwrite=1);",
            )?;
            conn.execute_batch(
                r#"
                CREATE OR REPLACE MACRO search_docs(q) AS TABLE
                  SELECT s.section AS section, s.file_path AS file_path,
                         fts_main_sections.match_bm25(s.stable_symbol_id, q) AS bm25
                  FROM sections s
                  WHERE fts_main_sections.match_bm25(s.stable_symbol_id, q) IS NOT NULL
                  ORDER BY bm25 DESC;
                "#,
            )?;
            // Flush WAL → main file so the read-only reopen (which cannot replay
            // a WAL) sees the tables. The real analyst.duckdb is checkpointed by
            // the CLI on exit, so this only matters in-test.
            conn.execute_batch("CHECKPOINT;")?;
        }

        let (resolved, results, graph_hash) =
            search_rows("oauth token refresh", "docs", 10, db.to_str())
                .expect("semantic search over fixture");
        assert_eq!(resolved, db.display().to_string());
        assert_eq!(graph_hash.as_deref(), Some("deadbeef"));
        assert!(!results.is_empty(), "expected at least one ranked hit");
        assert_eq!(results[0]["title"], "OAuth refresh");
        // run() wraps the same rows into a CallToolResult without error.
        run("oauth token refresh", "docs", 10, db.to_str()).expect("run wraps result");
        Ok(())
    }
}

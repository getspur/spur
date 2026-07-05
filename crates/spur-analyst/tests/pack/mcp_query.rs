use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use spur_analyst::mcp::{AnalystMcpModule, McpHandlerError};

struct QueryFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    db_path: PathBuf,
}

impl QueryFixture {
    fn new(setup_sql: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("repo");
        fs::create_dir_all(root.join(".spur")).expect("create .spur");
        let db_path = root.join(".spur").join("analyst.duckdb");
        let conn = duckdb::Connection::open(&db_path).expect("open fixture db");
        conn.execute_batch(setup_sql).expect("seed fixture db");
        drop(conn);
        Self {
            _dir: dir,
            root,
            db_path,
        }
    }

    fn write_live_pointer(&self, graph_content_hash: &str) {
        let pointer_dir = self.root.join(".spur").join("graph");
        fs::create_dir_all(&pointer_dir).expect("create graph pointer dir");
        fs::write(
            pointer_dir.join("pointer.json"),
            json!({
                "graph_content_hash": graph_content_hash
            })
            .to_string(),
        )
        .expect("write live graph pointer");
    }
}

#[tokio::test]
async fn query_tool_is_advertised_with_motherduck_compatible_name() {
    let module = AnalystMcpModule::new();

    let query_tool = module
        .tools()
        .into_iter()
        .find(|tool| tool.name == "query")
        .expect("query tool definition");

    assert_eq!(query_tool.input_schema["required"], json!(["query"]));
}

#[tokio::test]
async fn query_select_returns_motherduck_compatible_shape() {
    let fixture = QueryFixture::new("");

    let result = query_fixture(&fixture, "SELECT 1 AS a").await;

    assert_eq!(result["columns"], json!(["a"]));
    assert_eq!(result["rows"], json!([[1]]));
    assert_eq!(result["row_count"], json!(1));
    assert_eq!(result["truncated"], json!(false));
    assert_eq!(
        result["db_path"],
        json!(fixture.db_path.display().to_string())
    );
}

#[tokio::test]
async fn query_rejects_write_statement_with_clear_invalid_params() {
    let fixture = QueryFixture::new("");

    let error = query_fixture_err(&fixture, "CREATE TABLE foo (x INT)").await;

    match error {
        McpHandlerError::InvalidParams(message) => {
            assert!(message.contains("read-only"));
            assert!(message.contains("CREATE"));
        }
        other => panic!("expected InvalidParams, got {other:?}"),
    }
}

#[tokio::test]
async fn query_caps_rows_at_1000_without_injecting_limit() {
    let fixture = QueryFixture::new(
        r#"
        CREATE TABLE many AS
        SELECT range AS value FROM range(1005);
        "#,
    );

    let result = query_fixture(&fixture, "SELECT value FROM many ORDER BY value").await;
    let rows = result["rows"].as_array().expect("rows array");

    assert_eq!(result["columns"], json!(["value"]));
    assert_eq!(rows.len(), 1000);
    assert_eq!(rows.first(), Some(&json!([0])));
    assert_eq!(rows.last(), Some(&json!([999])));
    assert_eq!(result["row_count"], json!(1000));
    assert_eq!(result["truncated"], json!(true));
}

#[tokio::test]
async fn query_show_tables_returns_expected_columns() {
    let fixture = QueryFixture::new(
        r#"
        CREATE TABLE expected_table (id INTEGER);
        "#,
    );

    let result = query_fixture(&fixture, "SHOW TABLES").await;

    assert_eq!(result["columns"], json!(["name"]));
    assert_eq!(result["rows"], json!([["expected_table"]]));
    assert_eq!(result["row_count"], json!(1));
    assert_eq!(result["truncated"], json!(false));
}

#[tokio::test]
async fn query_blocks_stale_analyst_db_unless_allow_stale_is_explicit() {
    let fixture = QueryFixture::new(
        r#"
        CREATE TABLE _meta (graph_content_hash VARCHAR);
        INSERT INTO _meta VALUES ('old');
        CREATE TABLE facts (value INTEGER);
        INSERT INTO facts VALUES (42);
        "#,
    );
    fixture.write_live_pointer("new");

    let stale = query_fixture(&fixture, "SELECT value FROM facts").await;

    assert_eq!(stale["error"], json!("analyst_db_stale"));
    assert_eq!(stale["analyst_hash"], json!("old"));
    assert_eq!(stale["live_hash"], json!("new"));
    assert!(stale["message"]
        .as_str()
        .expect("stale error message")
        .contains("allow_stale=true"));
    assert!(
        stale.get("rows").is_none(),
        "stale query should not execute"
    );

    let allowed = query_fixture_with_args(
        fixture.root.as_path(),
        json!({
            "query": "SELECT value FROM facts",
            "allow_stale": true
        }),
    )
    .await
    .expect("allow stale query dispatch");

    assert_eq!(allowed["columns"], json!(["value"]));
    assert_eq!(allowed["rows"], json!([[42]]));
    assert_eq!(allowed["row_count"], json!(1));
    assert_eq!(allowed["staleness_warning"], json!("allow_stale"));
}

async fn query_fixture(fixture: &QueryFixture, sql: &str) -> Value {
    query_fixture_result(fixture.root.as_path(), sql)
        .await
        .expect("query dispatch")
}

async fn query_fixture_err(fixture: &QueryFixture, sql: &str) -> McpHandlerError {
    query_fixture_result(fixture.root.as_path(), sql)
        .await
        .expect_err("query dispatch error")
}

async fn query_fixture_result(root: &Path, sql: &str) -> Result<Value, McpHandlerError> {
    query_fixture_with_args(root, json!({ "query": sql })).await
}

async fn query_fixture_with_args(root: &Path, args: Value) -> Result<Value, McpHandlerError> {
    let module = AnalystMcpModule::new();
    spur_graph::mcp::with_worktree_root_for_request(root.to_path_buf(), async move {
        module.dispatch("query", args).await
    })
    .await
}

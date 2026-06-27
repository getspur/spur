use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use duckdb::Connection;
use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_core::server::{community_feature_gate, DetachedContinuationCtx};
use spur_core::McpCallbackServer;

static CWD_LOCK: Mutex<()> = Mutex::new(());

struct CwdGuard {
    original: PathBuf,
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

fn enter_dir(path: &Path) -> CwdGuard {
    let original = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(path).expect("set current dir");
    CwdGuard { original }
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

fn test_server(repo_root: &Path) -> McpCallbackServer {
    let session_id = BrainSessionId::new(SessionId("brain-analyst-dispatch".into()));
    let (mut server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        None,
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        community_feature_gate(),
    );
    server.set_repo_root(repo_root.to_path_buf());
    server
}

fn write_analyst_db(repo_root: &Path) {
    std::fs::create_dir_all(repo_root.join(".spur")).expect("create .spur");
    let db_path = repo_root.join(".spur/analyst.duckdb");
    let conn = Connection::open(&db_path).expect("open analyst db");
    conn.execute_batch(
        r#"
        CREATE TABLE dispatch_probe (answer INTEGER);
        INSERT INTO dispatch_probe VALUES (42);
        "#,
    )
    .expect("seed analyst db");
}

fn tool_body(response: Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("successful tool response with text content: {response}"));
    serde_json::from_str(text).expect("tool text is JSON")
}

#[tokio::test]
async fn callback_server_dispatches_query_against_repo_root() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let repo = tempdir.path().join("repo");
    let other = tempdir.path().join("other");
    std::fs::create_dir_all(repo.join(".git")).expect("create repo git marker");
    std::fs::create_dir_all(other.join(".git")).expect("create other git marker");
    write_analyst_db(&repo);

    let server = test_server(&repo);
    let _cwd = enter_dir(&other);
    let response = server
        .__test_call_tool(
            "query",
            json!({ "query": "SELECT answer FROM dispatch_probe" }),
        )
        .await;

    assert!(
        response.get("error").is_none(),
        "query should be dispatched by spur-core, not the placeholder catalog: {response}"
    );
    let body = tool_body(response);
    assert_eq!(body["rows"], json!([[42]]));
}

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard};

use duckdb::Connection;
use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_core::server::{community_feature_gate, DetachedContinuationCtx};
use spur_core::McpCallbackServer;
use spur_graph::{
    artifact_from_facts, build_facts, write_artifact_parquet, write_current_pointer, WriteOptions,
};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct CatalogEnvGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Option<OsString>,
}

impl CatalogEnvGuard {
    fn install(path: &Path) -> Self {
        let lock = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::var_os("SPUR_PROJECT_CATALOG");
        std::env::set_var("SPUR_PROJECT_CATALOG", path);
        Self {
            _lock: lock,
            previous,
        }
    }
}

impl Drop for CatalogEnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            std::env::set_var("SPUR_PROJECT_CATALOG", previous);
        } else {
            std::env::remove_var("SPUR_PROJECT_CATALOG");
        }
    }
}

struct ProjectFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl ProjectFixture {
    fn new(name: &str, answer: i64, graph: bool, analyst: bool) -> Self {
        let dir = tempfile::tempdir().expect("project tempdir");
        let root = dir.path().join(name);
        std::fs::create_dir_all(root.join("src")).expect("create source directory");
        std::fs::write(
            root.join("src/lib.rs"),
            format!("pub fn {name}_symbol() -> i64 {{ {answer} }}\n"),
        )
        .expect("write source");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.email", "test@example.com"]);
        git(&root, &["config", "user.name", "SPUR Test"]);
        git(&root, &["add", "src/lib.rs"]);
        git(&root, &["commit", "-q", "-m", "fixture"]);

        if graph {
            let (facts, _) = build_facts(&root, None).expect("build graph facts");
            let artifact = artifact_from_facts(&facts, &root).expect("build graph artifact");
            let artifact_dir = write_artifact_parquet(
                &artifact,
                &root.join(".spur/graph"),
                WriteOptions::default(),
                Vec::new(),
            )
            .expect("write graph artifact");
            write_current_pointer(&root, &artifact_dir).expect("write graph pointer");
        }
        if analyst {
            std::fs::create_dir_all(root.join(".spur")).expect("create analyst directory");
            let conn =
                Connection::open(root.join(".spur/analyst.duckdb")).expect("open analyst fixture");
            conn.execute_batch(&format!(
                "CREATE TABLE nodes (file_path VARCHAR, entity_name VARCHAR); \
                 CREATE TABLE identity (name VARCHAR, answer BIGINT); \
                 INSERT INTO identity VALUES ('{name}', {answer});"
            ))
            .expect("seed analyst fixture");
        }

        Self { _dir: dir, root }
    }
}

fn git(root: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(root)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

fn server(repo_root: &Path) -> McpCallbackServer {
    let session_id = BrainSessionId::new(SessionId("brain-local-project-routing".into()));
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

fn tool_body(response: &Value) -> Value {
    assert!(
        response.get("error").is_none(),
        "expected successful tool response: {response:#?}"
    );
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool response must contain JSON text: {response:#?}"));
    serde_json::from_str(text).expect("parse tool JSON")
}

#[tokio::test]
async fn brain_manages_and_routes_ready_local_projects_without_changing_default_scope() {
    let catalog_dir = tempfile::tempdir().expect("catalog tempdir");
    let catalog_path = catalog_dir.path().join("projects.toml");
    let _env = CatalogEnvGuard::install(&catalog_path);
    let current = ProjectFixture::new("current", 11, true, true);
    let external = ProjectFixture::new("external", 22, true, true);
    let server = server(&current.root);

    let add_response = server
        .__test_call_tool(
            "local_project_add",
            json!({"name": "external", "path": external.root.join("src")}),
        )
        .await;
    let add = tool_body(&add_response);
    assert_eq!(add["changed"], true);
    assert_eq!(add["catalog_generation"], 1);
    assert_eq!(add["project"]["status"], "ready");
    assert_eq!(
        add["project"]["root"],
        json!(external
            .root
            .canonicalize()
            .expect("canonical external root"))
    );

    let list_response = server
        .__test_call_tool("local_project_list", json!({}))
        .await;
    let list = tool_body(&list_response);
    assert_eq!(list["catalog_generation"], 1);
    assert_eq!(list["projects"][0]["name"], "external");

    for tool_name in ["code_symbol_search", "code_search"] {
        let graph_response = server
            .__test_call_tool(
                tool_name,
                json!({"query": "external_symbol", "mode": "exact", "project": "external"}),
            )
            .await;
        let graph = tool_body(&graph_response);
        assert_eq!(graph["candidates"][0]["entity_name"], "external_symbol");
        assert_eq!(graph["project"]["name"], "external");
        assert_eq!(graph["project"]["catalog_generation"], 1);
    }

    let analyst_response = server
        .__test_call_tool(
            "query",
            json!({
                "query": "SELECT name, answer FROM identity",
                "allow_stale": true,
                "project": "external"
            }),
        )
        .await;
    let analyst = tool_body(&analyst_response);
    assert_eq!(analyst["rows"], json!([["external", 22]]));
    assert_eq!(analyst["project"]["name"], "external");
    assert_eq!(analyst["project"]["catalog_generation"], 1);

    let current_response = server
        .__test_call_tool(
            "query",
            json!({"query": "SELECT name, answer FROM identity", "allow_stale": true}),
        )
        .await;
    let current_body = tool_body(&current_response);
    assert_eq!(current_body["rows"], json!([["current", 11]]));
    assert!(current_body.get("project").is_none());

    let remove_response = server
        .__test_call_tool("local_project_remove", json!({"name": "external"}))
        .await;
    let remove = tool_body(&remove_response);
    assert_eq!(remove["removed"], true);
    assert_eq!(remove["catalog_generation"], 2);
}

#[tokio::test]
async fn brain_registration_rejects_non_git_or_incompletely_indexed_roots() {
    let catalog_dir = tempfile::tempdir().expect("catalog tempdir");
    let _env = CatalogEnvGuard::install(&catalog_dir.path().join("projects.toml"));
    let current = ProjectFixture::new("current", 11, true, true);
    let graph_only = ProjectFixture::new("graph_only", 1, true, false);
    let analyst_only = ProjectFixture::new("analyst_only", 2, false, true);
    let arbitrary_duckdb = ProjectFixture::new("arbitrary_duckdb", 3, true, false);
    let conn = Connection::open(arbitrary_duckdb.root.join(".spur/analyst.duckdb"))
        .expect("open arbitrary DuckDB fixture");
    conn.execute_batch("CREATE TABLE identity (value VARCHAR);")
        .expect("seed arbitrary DuckDB fixture");
    drop(conn);
    let non_git = tempfile::tempdir().expect("non-git tempdir");
    let server = server(&current.root);

    for (name, path, expected) in [
        ("graph-only", graph_only.root.as_path(), "analyst"),
        ("analyst-only", analyst_only.root.as_path(), "graph"),
        ("arbitrary-duckdb", arbitrary_duckdb.root.as_path(), "nodes"),
        ("non-git", non_git.path(), "Git"),
    ] {
        let response = server
            .__test_call_tool("local_project_add", json!({"name": name, "path": path}))
            .await;
        assert_eq!(response["error"]["code"], -32602, "{response:#?}");
        assert!(
            response["error"]["message"]
                .as_str()
                .is_some_and(|message| message.contains(expected)),
            "unexpected error response: {response:#?}"
        );
    }
    assert!(
        !catalog_dir.path().join("projects.toml").exists(),
        "failed registration must not create the catalog"
    );
}

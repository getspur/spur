use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_graph::{artifact_from_facts, build_facts, write_artifact, GraphIndexArtifact};
use spur_mcp::server::{community_feature_gate, DetachedContinuationCtx};
use spur_mcp::McpCallbackServer;
use tempfile::TempDir;

const ROOT_SYMBOL: &str = "orchestrate_order";

struct CwdGuard {
    original: std::path::PathBuf,
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

fn test_server() -> McpCallbackServer {
    let session_id = BrainSessionId::new(SessionId("brain-code-graph-e2e".into()));
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        None,
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        community_feature_gate(),
    );
    server
}

fn fixture_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/code_graph_sample")
}

fn copy_fixture_crate(worktree: &Path) {
    let fixture = fixture_root();
    std::fs::create_dir_all(worktree.join("src")).expect("create fixture src dir");
    std::fs::copy(fixture.join("Cargo.toml"), worktree.join("Cargo.toml"))
        .expect("copy fixture manifest");
    std::fs::copy(fixture.join("src/lib.rs"), worktree.join("src/lib.rs"))
        .expect("copy fixture source");
    std::fs::create_dir_all(worktree.join(".git")).expect("create git marker");
}

fn build_graph_artifact(worktree: &Path) -> GraphIndexArtifact {
    let (facts, _file_counts) = build_facts(worktree).expect("build graph facts");
    let artifact = artifact_from_facts(&facts, worktree).expect("build graph artifact");
    write_artifact(&artifact, &worktree.join(".spur/graph-index.json")).expect("write artifact");
    artifact
}

fn symbol_id(artifact: &GraphIndexArtifact, entity_name: &str) -> String {
    artifact
        .symbols
        .iter()
        .find(|symbol| symbol.entity_name == entity_name)
        .unwrap_or_else(|| panic!("symbol `{entity_name}` exists in artifact"))
        .stable_symbol_id
        .clone()
}

async fn call_tool(server: &McpCallbackServer, tool: &str, arguments: Value) -> Value {
    server.__test_call_tool(tool, arguments).await
}

fn tool_body(response: Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("successful tool response with text content: {response}"));
    serde_json::from_str(text).expect("tool text is JSON")
}

fn entity_names(rows: &[Value]) -> BTreeSet<String> {
    rows.iter()
        .map(|row| {
            row["entity_name"]
                .as_str()
                .expect("row has entity_name")
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn code_graph_tools_traverse_artifact_built_from_real_rust_fixture() {
    let worktree = TempDir::new().expect("temp worktree");
    copy_fixture_crate(worktree.path());
    let artifact = build_graph_artifact(worktree.path());
    let root_id = symbol_id(&artifact, ROOT_SYMBOL);
    let root_uri = format!("graph://symbol/{root_id}");
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let callers = tool_body(
        call_tool(
            &server,
            "code_callers",
            json!({ "symbol": root_uri.clone() }),
        )
        .await,
    );
    assert_eq!(
        entity_names(callers["callers"].as_array().expect("callers")),
        BTreeSet::from(["launch_order".to_string()])
    );
    assert_eq!(callers["graph_content_hash"], artifact.graph_content_hash);
    assert_eq!(
        callers["graph_index_version"],
        artifact.header.graph_index_version
    );

    let callees = tool_body(
        call_tool(
            &server,
            "code_callees",
            json!({ "symbol": root_id.clone() }),
        )
        .await,
    );
    assert_eq!(
        entity_names(callees["callees"].as_array().expect("callees")),
        BTreeSet::from(["charge_order".to_string(), "parse_order".to_string()])
    );

    let selector_callees =
        tool_body(call_tool(&server, "code_callees", json!({ "selector": ROOT_SYMBOL })).await);
    assert_eq!(
        entity_names(
            selector_callees["callees"]
                .as_array()
                .expect("selector callees")
        ),
        BTreeSet::from(["charge_order".to_string(), "parse_order".to_string()])
    );
    assert_eq!(
        selector_callees["graph_content_hash"],
        artifact.graph_content_hash
    );
    assert_eq!(
        selector_callees["graph_index_version"],
        artifact.header.graph_index_version
    );

    let radius_one = tool_body(
        call_tool(
            &server,
            "code_subgraph",
            json!({ "symbol": root_uri, "radius": 1, "edge_kinds": ["calls"] }),
        )
        .await,
    );
    assert_eq!(
        entity_names(radius_one["nodes"].as_array().expect("radius one nodes")),
        BTreeSet::from([
            "charge_order".to_string(),
            "launch_order".to_string(),
            "orchestrate_order".to_string(),
            "parse_order".to_string(),
        ])
    );
    assert_eq!(
        radius_one["edges"]
            .as_array()
            .expect("radius one edges")
            .len(),
        3
    );

    let radius_zero = tool_body(
        call_tool(
            &server,
            "code_subgraph",
            json!({ "symbol": root_id, "radius": 0, "edge_kinds": ["calls"] }),
        )
        .await,
    );
    assert_eq!(
        entity_names(radius_zero["nodes"].as_array().expect("radius zero nodes")),
        BTreeSet::from(["orchestrate_order".to_string()])
    );
    assert!(radius_zero["edges"]
        .as_array()
        .expect("radius zero edges")
        .is_empty());

    let unknown = call_tool(
        &server,
        "code_callers",
        json!({ "symbol": "graph://symbol/not-in-artifact" }),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32004);
    assert!(unknown["error"]["message"]
        .as_str()
        .expect("unknown error message")
        .contains("symbol not-in-artifact not found in graph artifact"));
}

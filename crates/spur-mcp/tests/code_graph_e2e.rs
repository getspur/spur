use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_graph::{
    artifact_from_facts, build_facts, build_facts_for_paths, write_artifact, GraphIndexArtifact,
    GraphSymbolArtifact,
};
use spur_mcp::server::{community_feature_gate, DetachedContinuationCtx};
use spur_mcp::McpCallbackServer;
use tempfile::TempDir;

const ROOT_SYMBOL: &str = "orchestrate_order";
static CWD_LOCK: Mutex<()> = Mutex::new(());

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

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
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

fn build_real_tools_graph_artifact(worktree: &Path) -> GraphIndexArtifact {
    let root = workspace_root();
    let files = [
        PathBuf::from("crates/spur-mcp/src/tools.rs"),
        PathBuf::from("crates/spur-mcp/tests/rework_reuse_prior_worktree_e2e.rs"),
    ];
    let facts = build_facts_for_paths(&root, &files).expect("build graph facts for real tools");
    let artifact = artifact_from_facts(&facts, &root).expect("build graph artifact");
    write_artifact(&artifact, &worktree.join(".spur/graph-index.json")).expect("write artifact");
    artifact
}

fn symbol_id(artifact: &GraphIndexArtifact, entity_name: &str) -> String {
    symbol_by_entity(artifact, entity_name)
        .stable_symbol_id
        .clone()
}

fn symbol_by_file_entity_kind<'a>(
    artifact: &'a GraphIndexArtifact,
    file_path: &str,
    entity_name: &str,
    symbol_kind: &str,
) -> &'a GraphSymbolArtifact {
    artifact
        .symbols
        .iter()
        .find(|symbol| {
            symbol.file_path == file_path
                && symbol.entity_name == entity_name
                && symbol.symbol_kind == symbol_kind
        })
        .unwrap_or_else(|| {
            panic!("symbol `{entity_name}` kind `{symbol_kind}` exists in `{file_path}`")
        })
}

fn symbol_by_entity<'a>(
    artifact: &'a GraphIndexArtifact,
    entity_name: &str,
) -> &'a GraphSymbolArtifact {
    artifact
        .symbols
        .iter()
        .find(|symbol| symbol.entity_name == entity_name)
        .unwrap_or_else(|| panic!("symbol `{entity_name}` exists in artifact"))
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

fn qualified_names(rows: &[Value]) -> BTreeSet<String> {
    rows.iter()
        .map(|row| {
            row["qualified_name"]
                .as_str()
                .expect("row has qualified_name")
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn code_graph_tools_traverse_artifact_built_from_real_rust_fixture() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
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
    assert_eq!(callers["callers"][0]["edge_kind"], "calls");
    assert_eq!(callers["include_unresolved"], false);
    assert_eq!(callers["counts_by_kind"]["calls"], 1);
    assert_eq!(callers["counts_by_kind"]["unresolved"], 0);
    assert_eq!(callers["unresolved_sample"], json!([]));
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
    assert!(callees["callees"]
        .as_array()
        .expect("callees")
        .iter()
        .all(|row| row["edge_kind"] == "calls"));
    assert_eq!(callees["include_unresolved"], false);
    assert_eq!(callees["counts_by_kind"]["calls"], 2);
    assert_eq!(callees["counts_by_kind"]["unresolved"], 0);

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
    assert!(radius_one["edges"]
        .as_array()
        .expect("radius one edges")
        .iter()
        .all(|edge| edge["edge_kind"] == "calls"));
    assert_eq!(radius_one["include_unresolved"], false);

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
    assert_eq!(
        unknown["error"]["data"]["graph_content_hash"],
        artifact.graph_content_hash
    );
    assert_eq!(
        unknown["error"]["data"]["graph_index_version"],
        artifact.header.graph_index_version
    );
}

#[tokio::test]
async fn code_graph_tools_accept_real_sixteen_hex_legacy_symbol_id() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    copy_fixture_crate(worktree.path());
    let artifact = build_graph_artifact(worktree.path());
    let root_id = symbol_id(&artifact, ROOT_SYMBOL);
    assert_eq!(root_id.len(), 16);
    assert!(root_id
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')));
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let callers = tool_body(
        call_tool(
            &server,
            "code_callers",
            json!({ "symbol": root_id.clone() }),
        )
        .await,
    );
    assert_eq!(
        entity_names(callers["callers"].as_array().expect("callers")),
        BTreeSet::from(["launch_order".to_string()])
    );

    let callees = tool_body(call_tool(&server, "code_callees", json!({ "symbol": root_id })).await);
    assert_eq!(
        entity_names(callees["callees"].as_array().expect("callees")),
        BTreeSet::from(["charge_order".to_string(), "parse_order".to_string()])
    );
    assert_eq!(callees["graph_content_hash"], artifact.graph_content_hash);
    assert_eq!(
        callees["graph_index_version"],
        artifact.header.graph_index_version
    );
}

#[tokio::test]
async fn code_resolve_returns_candidate_rows_without_traversal_payloads() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    copy_fixture_crate(worktree.path());
    let artifact = build_graph_artifact(worktree.path());
    let root = symbol_by_entity(&artifact, ROOT_SYMBOL);
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let body =
        tool_body(call_tool(&server, "code_resolve", json!({ "selector": ROOT_SYMBOL })).await);
    let candidates = body["candidates"].as_array().expect("candidates");

    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0]["selector"],
        format!("{}::{}", root.file_path, root.qualified_name)
    );
    assert_eq!(
        candidates[0]["uri"],
        format!("graph://symbol/{}", root.stable_symbol_id)
    );
    assert_eq!(candidates[0]["id"], root.stable_symbol_id);
    assert_eq!(candidates[0]["qualified_name"], root.qualified_name);
    assert_eq!(candidates[0]["file_path"], root.file_path);
    assert_eq!(candidates[0]["line_range"], json!(root.line_range));
    assert_eq!(candidates[0]["symbol_kind"], root.symbol_kind);
    assert!(body.get("callers").is_none());
    assert!(body.get("callees").is_none());
    assert!(body.get("nodes").is_none());
    assert_eq!(body["graph_content_hash"], artifact.graph_content_hash);
    assert_eq!(
        body["graph_index_version"],
        artifact.header.graph_index_version
    );
}

#[tokio::test]
async fn code_resolve_prefers_real_submit_plan_mcp_tool_registration() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    let artifact = build_real_tools_graph_artifact(worktree.path());
    let mcp_tool = symbol_by_file_entity_kind(
        &artifact,
        "crates/spur-mcp/src/tools.rs",
        "submit_plan",
        "mcp_tool",
    );
    let helper = symbol_by_file_entity_kind(
        &artifact,
        "crates/spur-mcp/tests/rework_reuse_prior_worktree_e2e.rs",
        "submit_plan",
        "function",
    );
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let body = tool_body(
        call_tool(
            &server,
            "code_resolve",
            json!({ "selector": "submit_plan" }),
        )
        .await,
    );
    let candidates = body["candidates"].as_array().expect("candidates");

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0]["id"], mcp_tool.stable_symbol_id);
    assert_eq!(candidates[0]["symbol_kind"], "mcp_tool");
    assert_eq!(candidates[0]["qualified_name"], "submit_plan");
    assert_eq!(candidates[0]["file_path"], "crates/spur-mcp/src/tools.rs");
    assert_ne!(candidates[0]["id"], helper.stable_symbol_id);
}

#[tokio::test]
async fn code_file_symbols_returns_candidate_rows_for_worktree_relative_file() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    copy_fixture_crate(worktree.path());
    let artifact = build_graph_artifact(worktree.path());
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let body = tool_body(
        call_tool(
            &server,
            "code_file_symbols",
            json!({ "file": "src/lib.rs" }),
        )
        .await,
    );
    let symbols = body["symbols"].as_array().expect("symbols");

    assert_eq!(
        qualified_names(symbols),
        BTreeSet::from([
            "audit_order".to_string(),
            "charge_order".to_string(),
            "launch_order".to_string(),
            "orchestrate_order".to_string(),
            "parse_order".to_string(),
        ])
    );
    assert!(symbols.iter().all(|row| row["file_path"] == "src/lib.rs"));
    assert_eq!(body["graph_content_hash"], artifact.graph_content_hash);
    assert_eq!(
        body["graph_index_version"],
        artifact.header.graph_index_version
    );

    let invalid = call_tool(
        &server,
        "code_file_symbols",
        json!({ "file": "../src/lib.rs" }),
    )
    .await;
    assert_eq!(invalid["error"]["code"], -32602);

    let current_dir = call_tool(
        &server,
        "code_file_symbols",
        json!({ "file": "./src/lib.rs" }),
    )
    .await;
    assert_eq!(current_dir["error"]["code"], -32602);

    let embedded_current_dir = call_tool(
        &server,
        "code_file_symbols",
        json!({ "file": "src/./lib.rs" }),
    )
    .await;
    assert_eq!(embedded_current_dir["error"]["code"], -32602);
}

#[tokio::test]
async fn code_file_symbols_uses_symbol_uri_selector_for_legacy_empty_qualified_name() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    std::fs::create_dir_all(worktree.path().join(".spur")).expect("create .spur");
    std::fs::write(
        worktree.path().join(".spur/graph-index.json"),
        serde_json::to_string_pretty(&json!({
            "header": {
                "graph_index_version": "v4"
            },
            "manifest_version": "v4",
            "graph_content_hash": "legacy-empty-qualified-name",
            "files": [
                { "stable_file_id": "file-src-lib", "file_path": "src/lib.rs" }
            ],
            "symbols": [
                {
                    "stable_symbol_id": "legacy-empty-qualified-name-id",
                    "file_path": "src/lib.rs",
                    "byte_range": [0, 8],
                    "line_range": [1, 3],
                    "entity_name": "legacy_symbol",
                    "symbol_kind": "function",
                    "anchor_hash": "hash-legacy-empty-qualified-name-id",
                    "enclosing_scope": null
                }
            ]
        }))
        .expect("encode legacy artifact"),
    )
    .expect("write legacy artifact");
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let body = tool_body(
        call_tool(
            &server,
            "code_file_symbols",
            json!({ "file": "src/lib.rs" }),
        )
        .await,
    );
    let symbols = body["symbols"].as_array().expect("symbols");

    assert_eq!(symbols.len(), 1);
    assert_eq!(
        symbols[0]["selector"],
        "graph://symbol/legacy-empty-qualified-name-id"
    );
    assert_ne!(symbols[0]["selector"], "src/lib.rs::");
    assert_eq!(symbols[0]["qualified_name"], "");
}

#[tokio::test]
async fn code_symbol_info_returns_single_symbol_metadata() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    copy_fixture_crate(worktree.path());
    let artifact = build_graph_artifact(worktree.path());
    let root = symbol_by_entity(&artifact, ROOT_SYMBOL);
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let body = tool_body(
        call_tool(
            &server,
            "code_symbol_info",
            json!({ "selector": ROOT_SYMBOL }),
        )
        .await,
    );
    let symbol = &body["symbol"];

    assert_eq!(symbol["qualified_name"], root.qualified_name);
    assert_eq!(symbol["file_path"], root.file_path);
    assert_eq!(symbol["line_range"], json!(root.line_range));
    assert_eq!(symbol["symbol_kind"], root.symbol_kind);
    assert_eq!(symbol["enclosing_scope"], Value::Null);
    assert_eq!(
        symbol["uri"],
        format!("graph://symbol/{}", root.stable_symbol_id)
    );
    assert_eq!(symbol["id"], root.stable_symbol_id);
    assert!(body.get("callers").is_none());
    assert!(body.get("callees").is_none());
    assert!(body.get("nodes").is_none());
    assert_eq!(body["graph_content_hash"], artifact.graph_content_hash);
    assert_eq!(
        body["graph_index_version"],
        artifact.header.graph_index_version
    );
}

#[tokio::test]
async fn code_search_recovers_macro_bodied_callees_for_tools_list() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    let artifact = build_real_tools_graph_artifact(worktree.path());
    let tools_list = symbol_by_file_entity_kind(
        &artifact,
        "crates/spur-mcp/src/tools.rs",
        "tools_list",
        "function",
    );
    let tools_list_uri = format!("graph://symbol/{}", tools_list.stable_symbol_id);
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let callees = tool_body(
        call_tool(
            &server,
            "code_callees",
            json!({ "selector": tools_list_uri }),
        )
        .await,
    );
    let callee_names = entity_names(callees["callees"].as_array().expect("callees"));
    assert!(callee_names.contains("submit_plan_def"));
    assert!(callee_names.contains("code_search_def"));

    let search = tool_body(
        call_tool(
            &server,
            "code_search",
            json!({
                "query": "_def",
                "mode": "substring",
                "file": "crates/spur-mcp/src/tools.rs",
                "symbol_kind": "function",
                "limit": 100
            }),
        )
        .await,
    );
    let candidates = search["candidates"].as_array().expect("candidates");
    let names = entity_names(candidates);

    assert!(names.contains("delegate_to_worker_def"));
    assert!(names.contains("get_issue_def"));
    assert!(names.contains("submit_plan_def"));
    assert!(
        search["total_matches"].as_u64().expect("total_matches") >= 30,
        "expected at least 30 *_def functions, got {}",
        search["total_matches"]
    );
    assert!(candidates.iter().all(|candidate| candidate["entity_name"]
        .as_str()
        .expect("entity_name")
        .ends_with("_def")));

    let submit_tools = tool_body(
        call_tool(
            &server,
            "code_search",
            json!({
                "query": "submit",
                "symbol_kind": "mcp_tool",
                "limit": 20
            }),
        )
        .await,
    );
    let submit_tool_candidates = submit_tools["candidates"].as_array().expect("candidates");
    assert!(submit_tool_candidates.iter().any(|candidate| {
        candidate["entity_name"] == "submit_plan"
            && candidate["symbol_kind"] == "mcp_tool"
            && candidate["file_path"] == "crates/spur-mcp/src/tools.rs"
    }));
}

#[tokio::test]
async fn code_search_echoes_requested_limit_when_clamped() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    copy_fixture_crate(worktree.path());
    build_graph_artifact(worktree.path());
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let body = tool_body(
        call_tool(
            &server,
            "code_search",
            json!({
                "query": "order",
                "mode": "substring",
                "limit": 500
            }),
        )
        .await,
    );

    assert_eq!(body["limit"], 200);
    assert_eq!(body["requested_limit"], 500);
}

#[tokio::test]
async fn code_search_candidate_rows_include_enclosing_scope() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    let artifact = build_real_tools_graph_artifact(worktree.path());
    let mcp_tool = symbol_by_file_entity_kind(
        &artifact,
        "crates/spur-mcp/src/tools.rs",
        "submit_plan",
        "mcp_tool",
    );
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let body = tool_body(
        call_tool(
            &server,
            "code_search",
            json!({
                "query": "submit_plan",
                "symbol_kind": "mcp_tool",
                "limit": 20
            }),
        )
        .await,
    );
    let candidates = body["candidates"].as_array().expect("candidates");
    let candidate = candidates
        .iter()
        .find(|candidate| candidate["id"] == mcp_tool.stable_symbol_id)
        .expect("submit_plan mcp_tool candidate");

    assert_eq!(candidate["enclosing_scope"], "submit_plan_def");
}

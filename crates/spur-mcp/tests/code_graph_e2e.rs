use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_graph::{
    artifact_from_facts, build_facts, write_artifact, ChangeKind, CommitArtifact,
    CommitIndexArtifact, Confidence, EdgeEndpoint, GraphEdgeArtifact, GraphIndexArtifact,
    GraphIndexHeader, GraphSymbolArtifact, RelationKind, RenamePrev, SnapshotKey,
    SymbolSnapshotArtifact, TemporalEdgeArtifact, WalkStrategy, GRAPH_INDEX_VERSION_TEMPORAL,
};
use spur_mcp::server::{community_feature_gate, DetachedContinuationCtx};
use spur_mcp::McpCallbackServer;
use tempfile::TempDir;

const ROOT_SYMBOL: &str = "orchestrate_order";
const OLD_SHA: &str = "1111111111111111111111111111111111111111";
const NEW_SHA: &str = "2222222222222222222222222222222222222222";
const OLD_ROOT_ID: &str = "symbol-foo";
const NEW_ROOT_ID: &str = "symbol-bar";
const OLD_CALLER_ID: &str = "caller-foo";
const OLD_CALLEE_ID: &str = "callee-foo";
const NEW_CALLER_ID: &str = "caller-bar";
const NEW_CALLEE_ID: &str = "callee-bar";

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

fn write_temporal_fixture_artifact(worktree: &Path) {
    std::fs::create_dir_all(worktree.join(".git")).expect("create git marker");
    let old_root = snapshot(OLD_ROOT_ID, OLD_SHA, "foo");
    let new_root = snapshot(NEW_ROOT_ID, NEW_SHA, "bar");
    let graph = GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: GRAPH_INDEX_VERSION_TEMPORAL.to_string(),
            content_hash_blake3: None,
        },
        manifest_version: "temporal-fixture".to_string(),
        graph_content_hash: "temporal-fixture".to_string(),
        file_manifests: Vec::new(),
        files: Vec::new(),
        symbols: vec![
            graph_symbol(OLD_CALLER_ID, "launch_foo"),
            graph_symbol(OLD_ROOT_ID, "foo"),
            graph_symbol(OLD_CALLEE_ID, "helper_foo"),
            graph_symbol(NEW_CALLER_ID, "launch_bar"),
            graph_symbol(NEW_ROOT_ID, "bar"),
            graph_symbol(NEW_CALLEE_ID, "helper_bar"),
        ],
        edges: vec![
            graph_edge(OLD_CALLER_ID, OLD_ROOT_ID),
            graph_edge(OLD_ROOT_ID, OLD_CALLEE_ID),
            graph_edge(NEW_CALLER_ID, NEW_ROOT_ID),
            graph_edge(NEW_ROOT_ID, NEW_CALLEE_ID),
        ],
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
        commits: vec![
            CommitArtifact {
                sha: OLD_SHA.to_string(),
                parents: Vec::new(),
                author_time: 1,
                summary: "add foo".to_string(),
            },
            CommitArtifact {
                sha: NEW_SHA.to_string(),
                parents: vec![OLD_SHA.to_string()],
                author_time: 2,
                summary: "rename foo to bar".to_string(),
            },
        ],
        symbol_snapshots: vec![old_root.clone(), new_root.clone()],
        temporal_edges: vec![
            temporal_touch(OLD_SHA, old_root.key.clone(), ChangeKind::Added),
            temporal_touch(
                NEW_SHA,
                new_root.key.clone(),
                ChangeKind::RenamedFrom(RenamePrev::Symbol(old_root.key)),
            ),
        ],
    };
    write_artifact(&graph, &worktree.join(".spur/graph-index.json"))
        .expect("write temporal graph artifact");

    let commits = CommitIndexArtifact {
        schema_version: GRAPH_INDEX_VERSION_TEMPORAL
            .parse()
            .expect("temporal graph index version is numeric"),
        commits: graph.commits.clone(),
        refs: [("HEAD".to_string(), NEW_SHA.to_string())].into(),
        indexed_at: "2026-05-20T12:00:00Z".to_string(),
        walk_strategy: WalkStrategy::Reachable,
    };
    spur_graph::store::commit_index::save_artifact(worktree, ".spur/commit-index.json", &commits)
        .expect("write commit index artifact");
    spur_graph::store::commit_index::save_pointer(
        worktree,
        &spur_graph::store::commit_index::CommitIndexPointer {
            schema_version: GRAPH_INDEX_VERSION_TEMPORAL
                .parse()
                .expect("temporal graph index version is numeric"),
            artifact_relative_path: ".spur/commit-index.json".to_string(),
            indexed_at: commits.indexed_at.clone(),
            refs: commits.refs.clone(),
        },
    )
    .expect("write commit index pointer");
}

fn write_graph_without_commit_index(worktree: &Path) {
    std::fs::create_dir_all(worktree.join(".git")).expect("create git marker");
    let graph = GraphIndexArtifact {
        header: GraphIndexHeader {
            graph_index_version: GRAPH_INDEX_VERSION_TEMPORAL.to_string(),
            content_hash_blake3: None,
        },
        manifest_version: "missing-commit-index-fixture".to_string(),
        graph_content_hash: "missing-commit-index-fixture".to_string(),
        file_manifests: Vec::new(),
        files: Vec::new(),
        symbols: vec![graph_symbol(NEW_ROOT_ID, "bar")],
        edges: Vec::new(),
        tombstones: Vec::new(),
        diagnostics: Vec::new(),
        commits: Vec::new(),
        symbol_snapshots: Vec::new(),
        temporal_edges: Vec::new(),
    };
    write_artifact(&graph, &worktree.join(".spur/graph-index.json")).expect("write graph artifact");
}

fn graph_symbol(id: &str, entity_name: &str) -> GraphSymbolArtifact {
    GraphSymbolArtifact {
        stable_symbol_id: id.to_string(),
        file_path: format!("src/{entity_name}.rs"),
        byte_range: [0, 10],
        line_range: [1, 3],
        entity_name: entity_name.to_string(),
        symbol_kind: "function".to_string(),
        anchor_hash: format!("anchor-{id}"),
        enclosing_scope: None,
    }
}

fn graph_edge(source: &str, target: &str) -> GraphEdgeArtifact {
    GraphEdgeArtifact {
        source_stable_symbol_id: source.to_string(),
        target_stable_symbol_id: Some(target.to_string()),
        target_label: None,
        relation: RelationKind::Calls,
        confidence: Confidence::SyntaxExact,
        confidence_score: 1.0,
        change_kind: None,
    }
}

fn snapshot(id: &str, commit: &str, entity_name: &str) -> SymbolSnapshotArtifact {
    SymbolSnapshotArtifact {
        key: SnapshotKey {
            stable_symbol_id: id.to_string(),
            commit: commit.to_string(),
        },
        file_path: format!("src/{entity_name}.rs").into(),
        entity_name: entity_name.to_string(),
        symbol_kind: "function".to_string(),
        enclosing_scope: None,
        byte_range: [0, 10],
        line_range: [1, 3],
        anchor_hash: "shared-anchor".to_string(),
        tokens: vec![entity_name.to_string(), "body".to_string()],
    }
}

fn temporal_touch(commit: &str, key: SnapshotKey, change_kind: ChangeKind) -> TemporalEdgeArtifact {
    TemporalEdgeArtifact {
        source: EdgeEndpoint::Commit {
            sha: commit.to_string(),
        },
        target: EdgeEndpoint::Snapshot { key },
        relation: RelationKind::Touches,
        change_kind: Some(change_kind),
    }
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

#[tokio::test]
async fn code_graph_tools_resolve_requested_symbol_as_of_historical_commit() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    write_temporal_fixture_artifact(worktree.path());
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let current_root_uri = format!("graph://symbol/{NEW_ROOT_ID}");
    let historical_subgraph = tool_body(
        call_tool(
            &server,
            "code_subgraph",
            json!({
                "symbol": current_root_uri.clone(),
                "as_of": OLD_SHA,
                "radius": 1,
                "edge_kinds": ["calls"]
            }),
        )
        .await,
    );
    let historical_names = entity_names(
        historical_subgraph["nodes"]
            .as_array()
            .expect("historical nodes"),
    );
    assert_eq!(
        historical_names,
        BTreeSet::from([
            "foo".to_string(),
            "helper_foo".to_string(),
            "launch_foo".to_string(),
        ])
    );
    assert!(!historical_names.contains("bar"));

    let callers = tool_body(
        call_tool(
            &server,
            "code_callers",
            json!({ "symbol": current_root_uri.clone(), "as_of": OLD_SHA }),
        )
        .await,
    );
    assert_eq!(
        entity_names(callers["callers"].as_array().expect("callers")),
        BTreeSet::from(["launch_foo".to_string()])
    );

    let callees = tool_body(
        call_tool(
            &server,
            "code_callees",
            json!({ "symbol": current_root_uri.clone(), "as_of": OLD_SHA }),
        )
        .await,
    );
    assert_eq!(
        entity_names(callees["callees"].as_array().expect("callees")),
        BTreeSet::from(["helper_foo".to_string()])
    );

    let current_subgraph = tool_body(
        call_tool(
            &server,
            "code_subgraph",
            json!({ "symbol": current_root_uri, "radius": 1, "edge_kinds": ["calls"] }),
        )
        .await,
    );
    let current_names = entity_names(current_subgraph["nodes"].as_array().expect("current nodes"));
    assert_eq!(
        current_names,
        BTreeSet::from([
            "bar".to_string(),
            "helper_bar".to_string(),
            "launch_bar".to_string(),
        ])
    );
}

#[tokio::test]
async fn code_symbol_history_returns_rename_chain() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    write_temporal_fixture_artifact(worktree.path());
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let body = tool_body(
        call_tool(
            &server,
            "code_symbol_history",
            json!({ "symbol": format!("graph://symbol/{NEW_ROOT_ID}") }),
        )
        .await,
    );

    assert_eq!(body["symbol"], format!("graph://symbol/{NEW_ROOT_ID}"));
    let events = body["events"].as_array().expect("history events");
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["commit"], OLD_SHA);
    assert_eq!(events[0]["change_kind"], "added");
    assert_eq!(events[0]["snapshot"]["stable_symbol_id"], OLD_ROOT_ID);
    assert_eq!(events[1]["commit"], NEW_SHA);
    assert_eq!(
        events[1]["change_kind"]["renamed_from"]["symbol"]["stable_symbol_id"],
        OLD_ROOT_ID
    );
    assert_eq!(events[1]["snapshot"]["stable_symbol_id"], NEW_ROOT_ID);
}

#[tokio::test]
async fn code_symbol_history_reports_missing_commit_index_cleanly() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let worktree = TempDir::new().expect("temp worktree");
    write_graph_without_commit_index(worktree.path());
    let _cwd = enter_dir(worktree.path());
    let server = test_server();

    let response = call_tool(
        &server,
        "code_symbol_history",
        json!({ "symbol": NEW_ROOT_ID }),
    )
    .await;

    assert_eq!(response["error"]["code"], -32603);
    assert!(response["error"]["message"]
        .as_str()
        .expect("error message")
        .contains("commit index not found"));
}

use std::io::ErrorKind;
use std::path::Path;

use serde_json::{json, Value};
use spur_graph::{
    bounded_subgraph, find_callees, find_callers, find_symbol, load_artifact,
    resolve_worktree_root_from, GraphEdgeArtifact, GraphIndexArtifact, GraphSymbolArtifact,
    RelationKind, CODE_SYMBOL_URI_PREFIX,
};

use super::McpCallbackServer;
use super::*;

const MAX_MCP_CODE_SUBGRAPH_RADIUS: u8 = 3;
const GRAPH_ARTIFACT_RELATIVE_PATH: &str = ".spur/graph-index.json";

impl McpCallbackServer {
    pub(crate) async fn handle_code_callers(&self, id: Value, args: Value) -> JsonRpcResponse {
        let symbol_id = match normalize_symbol_arg(&args) {
            Ok(symbol_id) => symbol_id,
            Err(message) => return JsonRpcResponse::invalid_params(id, message),
        };
        let artifact = match load_graph_artifact_for_request(&id) {
            Ok(artifact) => artifact,
            Err(response) => return response,
        };
        if find_symbol(&artifact, &symbol_id).is_none() {
            return symbol_not_found(id, &symbol_id);
        }

        let callers = find_callers(&artifact, &symbol_id)
            .into_iter()
            .map(symbol_row)
            .collect::<Vec<_>>();
        json_success(id, json!({ "callers": callers }))
    }

    pub(crate) async fn handle_code_callees(&self, id: Value, args: Value) -> JsonRpcResponse {
        let symbol_id = match normalize_symbol_arg(&args) {
            Ok(symbol_id) => symbol_id,
            Err(message) => return JsonRpcResponse::invalid_params(id, message),
        };
        let artifact = match load_graph_artifact_for_request(&id) {
            Ok(artifact) => artifact,
            Err(response) => return response,
        };
        if find_symbol(&artifact, &symbol_id).is_none() {
            return symbol_not_found(id, &symbol_id);
        }

        let callees = find_callees(&artifact, &symbol_id)
            .into_iter()
            .map(symbol_row)
            .collect::<Vec<_>>();
        json_success(id, json!({ "callees": callees }))
    }

    pub(crate) async fn handle_code_subgraph(&self, id: Value, args: Value) -> JsonRpcResponse {
        let symbol_id = match normalize_symbol_arg(&args) {
            Ok(symbol_id) => symbol_id,
            Err(message) => return JsonRpcResponse::invalid_params(id, message),
        };
        let artifact = match load_graph_artifact_for_request(&id) {
            Ok(artifact) => artifact,
            Err(response) => return response,
        };
        if find_symbol(&artifact, &symbol_id).is_none() {
            return symbol_not_found(id, &symbol_id);
        }

        let requested_radius = args
            .get("radius")
            .and_then(|value| value.as_u64())
            .unwrap_or(1);
        let radius = requested_radius.min(u64::from(MAX_MCP_CODE_SUBGRAPH_RADIUS)) as u8;
        let warning = (requested_radius > u64::from(MAX_MCP_CODE_SUBGRAPH_RADIUS)).then(|| {
            format!(
                "radius {requested_radius} exceeds max {MAX_MCP_CODE_SUBGRAPH_RADIUS}; clamped to {MAX_MCP_CODE_SUBGRAPH_RADIUS}"
            )
        });
        let format = args
            .get("format")
            .and_then(|value| value.as_str())
            .unwrap_or("json");
        let edge_kinds = match parse_edge_kinds(&args) {
            Ok(edge_kinds) => edge_kinds,
            Err(message) => return JsonRpcResponse::invalid_params(id, message),
        };
        let edge_filter = edge_kinds.as_deref();
        let view = bounded_subgraph(&artifact, &symbol_id, radius, edge_filter);

        match format {
            "json" => {
                let mut metadata = json!({ "radius": radius });
                if let Some(warning) = warning {
                    metadata["warning"] = Value::String(warning);
                }
                json_success(
                    id,
                    json!({
                        "nodes": view.nodes.into_iter().map(symbol_row).collect::<Vec<_>>(),
                        "edges": view.edges.into_iter().map(edge_row).collect::<Vec<_>>(),
                        "metadata": metadata,
                    }),
                )
            }
            "mermaid" => json_success(
                id,
                json!({ "mermaid": mermaid_subgraph(&view.nodes, &view.edges) }),
            ),
            other => JsonRpcResponse::invalid_params(
                id,
                format!("invalid format `{other}`; expected `json` or `mermaid`"),
            ),
        }
    }
}

fn normalize_symbol_arg(args: &Value) -> Result<String, String> {
    let symbol = args
        .get("symbol")
        .and_then(|value| value.as_str())
        .ok_or_else(|| "Missing required field 'symbol'".to_string())?;
    if symbol.is_empty() {
        return Err("field 'symbol' must not be empty".to_string());
    }
    if let Some(symbol_id) = symbol.strip_prefix(CODE_SYMBOL_URI_PREFIX) {
        if symbol_id.is_empty() {
            return Err("field 'symbol' must include a symbol id".to_string());
        }
        return Ok(symbol_id.to_string());
    }
    if symbol.contains("://") {
        return Err(format!(
            "invalid symbol URI prefix; expected `{CODE_SYMBOL_URI_PREFIX}` or a bare symbol id"
        ));
    }
    Ok(symbol.to_string())
}

#[allow(clippy::result_large_err)]
fn load_graph_artifact_for_request(id: &Value) -> Result<GraphIndexArtifact, JsonRpcResponse> {
    let current_dir = std::env::current_dir().map_err(|error| {
        JsonRpcResponse::internal_error(
            id.clone(),
            format!("failed to read current directory: {error}"),
        )
    })?;
    let worktree = resolve_worktree_root_from(current_dir);
    let artifact_path = worktree.join(GRAPH_ARTIFACT_RELATIVE_PATH);

    match load_artifact(&artifact_path) {
        Ok(artifact) => Ok(artifact),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == ErrorKind::NotFound) =>
        {
            Err(graph_artifact_missing(id.clone(), &worktree))
        }
        Err(_) if !artifact_path.exists() => Err(graph_artifact_missing(id.clone(), &worktree)),
        Err(error) => Err(JsonRpcResponse::internal_error(
            id.clone(),
            format!(
                "failed to load graph artifact `{}`: {error}",
                artifact_path.display()
            ),
        )),
    }
}

fn graph_artifact_missing(id: Value, worktree: &Path) -> JsonRpcResponse {
    JsonRpcResponse::internal_error(
        id,
        format!(
            "graph artifact not found; run `spur graph build` in {}",
            worktree.display()
        ),
    )
}

fn symbol_not_found(id: Value, symbol_id: &str) -> JsonRpcResponse {
    JsonRpcResponse::error(
        id,
        -32004,
        format!("symbol {symbol_id} not found in graph artifact"),
    )
}

fn parse_edge_kinds(args: &Value) -> Result<Option<Vec<RelationKind>>, String> {
    let Some(value) = args.get("edge_kinds") else {
        return Ok(None);
    };
    let kinds = value
        .as_array()
        .ok_or_else(|| "field 'edge_kinds' must be an array of strings".to_string())?;
    kinds
        .iter()
        .map(|kind| {
            let kind = kind
                .as_str()
                .ok_or_else(|| "field 'edge_kinds' must be an array of strings".to_string())?;
            serde_json::from_value::<RelationKind>(Value::String(kind.to_string()))
                .map_err(|_| format!("invalid edge kind `{kind}`"))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn symbol_row(symbol: &GraphSymbolArtifact) -> Value {
    json!({
        "uri": symbol_uri(&symbol.stable_symbol_id),
        "entity_name": symbol.entity_name,
        "enclosing_scope": symbol.enclosing_scope,
        "file_path": symbol.file_path,
        "line_range": symbol.line_range,
        "symbol_kind": symbol.symbol_kind,
    })
}

fn edge_row(edge: &GraphEdgeArtifact) -> Value {
    json!({
        "source_uri": symbol_uri(&edge.source_stable_symbol_id),
        "target_uri": edge.target_stable_symbol_id.as_ref().map(|id| symbol_uri(id)),
        "target_label": edge.target_label,
        "relation": edge.relation,
        "confidence": edge.confidence,
        "confidence_score": edge.confidence_score,
    })
}

fn symbol_uri(symbol_id: &str) -> String {
    format!("{CODE_SYMBOL_URI_PREFIX}{symbol_id}")
}

fn json_success(id: Value, body: Value) -> JsonRpcResponse {
    let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
    JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
}

fn mermaid_subgraph(nodes: &[&GraphSymbolArtifact], edges: &[&GraphEdgeArtifact]) -> String {
    let mut lines = vec!["graph TD".to_string()];
    for symbol in nodes {
        lines.push(format!(
            "    {}[\"{}\"]",
            mermaid_id(&symbol.stable_symbol_id),
            escape_mermaid_label(&symbol.entity_name)
        ));
    }
    for edge in edges {
        let Some(target_id) = edge.target_stable_symbol_id.as_deref() else {
            continue;
        };
        lines.push(format!(
            "    {} --> {}",
            mermaid_id(&edge.source_stable_symbol_id),
            mermaid_id(target_id)
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

fn mermaid_id(symbol_id: &str) -> String {
    symbol_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn escape_mermaid_label(label: &str) -> String {
    label.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use serde_json::{json, Value};
    use spur_acp::{BrainSessionId, SessionId};
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;

    static CWD_LOCK: Mutex<()> = Mutex::new(());

    struct CwdGuard {
        original: std::path::PathBuf,
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    fn enter_dir(path: &std::path::Path) -> CwdGuard {
        let original = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(path).expect("set current dir");
        CwdGuard { original }
    }

    fn no_op_ctx() -> DetachedContinuationCtx {
        DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    fn test_server() -> McpCallbackServer {
        let session_id = BrainSessionId::new(SessionId("brain-test".into()));
        let (server, _channel) = McpCallbackServer::new(
            Some(&session_id),
            None,
            None,
            no_op_ctx(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            community_feature_gate(),
        );
        server
    }

    fn write_fixture_artifact(dir: &TempDir) {
        std::fs::create_dir_all(dir.path().join(".spur")).expect("create .spur");
        std::fs::write(
            dir.path().join(".spur/graph-index.json"),
            serde_json::to_string_pretty(&json!({
                "header": {
                    "graph_index_version": "test"
                },
                "manifest_version": "test",
                "graph_content_hash": "test",
                "files": [],
                "symbols": [
                    symbol("caller", "src/caller.rs", [3, 5], "call_root"),
                    symbol("root", "src/root.rs", [10, 12], "root"),
                    symbol("callee", "src/callee.rs", [20, 22], "callee")
                ],
                "edges": [
                    edge("caller", "root"),
                    edge("root", "callee")
                ],
                "tombstones": []
            }))
            .expect("encode artifact"),
        )
        .expect("write artifact");
    }

    fn symbol(id: &str, file_path: &str, line_range: [usize; 2], entity_name: &str) -> Value {
        json!({
            "stable_symbol_id": id,
            "file_path": file_path,
            "byte_range": [0, 8],
            "line_range": line_range,
            "entity_name": entity_name,
            "symbol_kind": "function",
            "anchor_hash": format!("hash-{id}"),
            "enclosing_scope": null
        })
    }

    fn edge(source: &str, target: &str) -> Value {
        json!({
            "source_stable_symbol_id": source,
            "target_stable_symbol_id": target,
            "target_label": null,
            "relation": "calls",
            "confidence": "syntax_exact",
            "confidence_score": 1.0
        })
    }

    fn response_json(response: JsonRpcResponse) -> Value {
        let text = response.result.expect("success result")["content"][0]["text"]
            .as_str()
            .expect("content text")
            .to_string();
        serde_json::from_str(&text).expect("JSON content")
    }

    #[tokio::test]
    async fn code_callers_returns_lightweight_symbol_rows() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callers(Value::from(1), json!({ "symbol": "graph://symbol/root" }))
            .await;
        let body = response_json(response);

        assert_eq!(body["callers"].as_array().expect("callers").len(), 1);
        assert_eq!(body["callers"][0]["uri"], "graph://symbol/caller");
        assert_eq!(body["callers"][0]["entity_name"], "call_root");
        assert_eq!(body["callers"][0]["file_path"], "src/caller.rs");
        assert_eq!(body["callers"][0]["line_range"], json!([3, 5]));
        assert_eq!(body["callers"][0]["symbol_kind"], "function");
    }

    #[tokio::test]
    async fn code_callees_accepts_bare_symbol_id() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callees(Value::from(1), json!({ "symbol": "root" }))
            .await;
        let body = response_json(response);

        assert_eq!(body["callees"].as_array().expect("callees").len(), 1);
        assert_eq!(body["callees"][0]["uri"], "graph://symbol/callee");
        assert_eq!(body["callees"][0]["entity_name"], "callee");
    }

    #[tokio::test]
    async fn code_subgraph_returns_json_and_clamps_radius_metadata() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(
                Value::from(1),
                json!({ "symbol": "root", "radius": 9, "edge_kinds": ["calls"] }),
            )
            .await;
        let body = response_json(response);

        assert_eq!(body["nodes"].as_array().expect("nodes").len(), 3);
        assert_eq!(body["edges"].as_array().expect("edges").len(), 2);
        assert_eq!(body["metadata"]["radius"], 3);
        assert_eq!(
            body["metadata"]["warning"],
            "radius 9 exceeds max 3; clamped to 3"
        );
    }

    #[tokio::test]
    async fn code_subgraph_returns_mermaid_output() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(
                Value::from(1),
                json!({ "symbol": "root", "radius": 1, "format": "mermaid" }),
            )
            .await;
        let body = response_json(response);
        let mermaid = body["mermaid"].as_str().expect("mermaid");

        assert!(mermaid.starts_with("graph TD\n"));
        assert!(mermaid.contains("root[\"root\"]"));
        assert!(mermaid.contains("root --> callee"));
    }

    #[tokio::test]
    async fn missing_artifact_returns_clear_json_rpc_error() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".git")).expect("create .git marker");
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callers(Value::from(1), json!({ "symbol": "root" }))
            .await;

        let error = response.error.expect("error");
        assert!(error
            .message
            .contains("graph artifact not found; run `spur graph build`"));
        assert!(error
            .message
            .contains(dir.path().to_string_lossy().as_ref()));
    }
}

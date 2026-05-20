use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};
use spur_graph::{
    bounded_subgraph, find_callees, find_callers, load_artifact, resolve_selector,
    resolve_worktree_root_from, CandidateRow, GraphEdgeArtifact, GraphIndexArtifact,
    GraphSymbolArtifact, RelationKind, SelectorResolution, CODE_SYMBOL_URI_PREFIX,
};

use crate::handlers::McpHandlerError;

use super::McpCallbackServer;
use super::*;

const MAX_MCP_CODE_SUBGRAPH_RADIUS: u8 = 3;
const GRAPH_ARTIFACT_RELATIVE_PATH: &str = ".spur/graph-index.json";

impl McpCallbackServer {
    pub(crate) async fn handle_code_resolve(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_resolve_response(&args))
    }

    pub(crate) async fn handle_code_file_symbols(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_file_symbols_response(&args))
    }

    pub(crate) async fn handle_code_symbol_info(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_symbol_info_response(&args))
    }

    pub(crate) async fn handle_code_callers(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_callers_response(&args))
    }

    pub(crate) async fn handle_code_callees(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_callees_response(&args))
    }

    pub(crate) async fn handle_code_subgraph(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_subgraph_response(&args))
    }
}

pub(crate) fn code_resolve(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    code_resolve_with_artifact(args, &artifact)
}

fn code_resolve_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_resolve_with_artifact(args, artifact))
}

fn code_resolve_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let selector = selector_arg(args)?;
    let candidates = resolve_candidate_rows(artifact, selector)?
        .into_iter()
        .map(candidate_row)
        .collect::<Vec<_>>();

    Ok(with_graph_metadata(
        artifact,
        json!({ "candidates": candidates }),
    ))
}

pub(crate) fn code_file_symbols(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    code_file_symbols_with_artifact(args, &artifact)
}

fn code_file_symbols_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_file_symbols_with_artifact(args, artifact))
}

fn code_file_symbols_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let file = file_arg(args)?;
    let file = validate_file_path_arg(file)?;
    if !artifact.files.iter().any(|entry| entry.file_path == file) {
        return Err(McpHandlerError::NotFound(format!(
            "file `{file}` not found in graph artifact"
        )));
    }

    let symbols = candidate_rows_for_symbols(
        artifact
            .symbols
            .iter()
            .filter(|symbol| symbol.file_path == file),
    )
    .into_iter()
    .map(candidate_row)
    .collect::<Vec<_>>();

    Ok(with_graph_metadata(artifact, json!({ "symbols": symbols })))
}

pub(crate) fn code_symbol_info(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    code_symbol_info_with_artifact(args, &artifact)
}

fn code_symbol_info_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_symbol_info_with_artifact(args, artifact))
}

fn code_symbol_info_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let symbol_id = match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => symbol_id,
        CodeSelectorResolution::Ambiguous(candidates) => {
            return Ok(ambiguous_response(artifact, candidates));
        }
    };
    let symbol = symbol_by_id(artifact, &symbol_id)?;

    Ok(with_graph_metadata(
        artifact,
        json!({ "symbol": symbol_info_row(symbol) }),
    ))
}

pub(crate) fn code_callers(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    code_callers_with_artifact(args, &artifact)
}

fn code_callers_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_callers_with_artifact(args, artifact))
}

fn code_callers_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let symbol_id = match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => symbol_id,
        CodeSelectorResolution::Ambiguous(candidates) => {
            return Ok(ambiguous_response(artifact, candidates));
        }
    };

    let callers = find_callers(artifact, &symbol_id)
        .into_iter()
        .map(symbol_row)
        .collect::<Vec<_>>();
    Ok(with_graph_metadata(artifact, json!({ "callers": callers })))
}

pub(crate) fn code_callees(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    code_callees_with_artifact(args, &artifact)
}

fn code_callees_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_callees_with_artifact(args, artifact))
}

fn code_callees_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let symbol_id = match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => symbol_id,
        CodeSelectorResolution::Ambiguous(candidates) => {
            return Ok(ambiguous_response(artifact, candidates));
        }
    };

    let callees = find_callees(artifact, &symbol_id)
        .into_iter()
        .map(symbol_row)
        .collect::<Vec<_>>();
    Ok(with_graph_metadata(artifact, json!({ "callees": callees })))
}

pub(crate) fn code_subgraph(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    code_subgraph_with_artifact(args, &artifact)
}

fn code_subgraph_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_subgraph_with_artifact(args, artifact))
}

fn code_subgraph_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let symbol_id = match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => symbol_id,
        CodeSelectorResolution::Ambiguous(candidates) => {
            return Ok(ambiguous_response(artifact, candidates));
        }
    };

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
    let edge_kinds = parse_edge_kinds(args)?;
    let edge_filter = edge_kinds.as_deref();
    let view = bounded_subgraph(artifact, &symbol_id, radius, edge_filter);

    match format {
        "json" => {
            let mut metadata = json!({ "radius": radius });
            if let Some(warning) = warning {
                metadata["warning"] = Value::String(warning);
            }
            Ok(with_graph_metadata(
                artifact,
                json!({
                    "nodes": view.nodes.into_iter().map(symbol_row).collect::<Vec<_>>(),
                    "edges": view.edges.into_iter().map(edge_row).collect::<Vec<_>>(),
                    "metadata": metadata,
                }),
            ))
        }
        "mermaid" => Ok(with_graph_metadata(
            artifact,
            json!({ "mermaid": mermaid_subgraph(&view.nodes, &view.edges) }),
        )),
        other => Err(McpHandlerError::InvalidParams(format!(
            "invalid format `{other}`; expected `json` or `mermaid`"
        ))),
    }
}

type CodeGraphResult = Result<Value, CodeGraphError>;

#[derive(Debug)]
struct CodeGraphError {
    error: McpHandlerError,
    metadata: Option<GraphResponseMetadata>,
}

impl CodeGraphError {
    fn without_metadata(error: McpHandlerError) -> Self {
        Self {
            error,
            metadata: None,
        }
    }

    fn with_artifact(error: McpHandlerError, artifact: &GraphIndexArtifact) -> Self {
        Self {
            error,
            metadata: Some(GraphResponseMetadata::from_artifact(artifact)),
        }
    }
}

#[derive(Debug)]
struct GraphResponseMetadata {
    graph_content_hash: String,
    graph_index_version: String,
}

impl GraphResponseMetadata {
    fn from_artifact(artifact: &GraphIndexArtifact) -> Self {
        Self {
            graph_content_hash: artifact.graph_content_hash.clone(),
            graph_index_version: artifact.header.graph_index_version.clone(),
        }
    }

    fn into_value(self) -> Value {
        json!({
            "graph_content_hash": self.graph_content_hash,
            "graph_index_version": self.graph_index_version,
        })
    }
}

fn with_loaded_graph_artifact(
    handler: impl FnOnce(&GraphIndexArtifact) -> Result<Value, McpHandlerError>,
) -> CodeGraphResult {
    let artifact = load_graph_artifact_for_request().map_err(CodeGraphError::without_metadata)?;
    handler(&artifact).map_err(|error| CodeGraphError::with_artifact(error, &artifact))
}

fn code_graph_response(id: Value, result: CodeGraphResult) -> JsonRpcResponse {
    match result {
        Ok(body) => json_success(id, body),
        Err(error) => code_graph_error_response(id, error),
    }
}

fn code_graph_error_response(id: Value, error: CodeGraphError) -> JsonRpcResponse {
    let CodeGraphError { error, metadata } = error;
    let mut response = match error {
        McpHandlerError::InvalidParams(message) => JsonRpcResponse::invalid_params(id, message),
        McpHandlerError::NotFound(message) => JsonRpcResponse::error(id, -32004, message),
        McpHandlerError::Unauthorized(message) => JsonRpcResponse::error(id, -32001, message),
        McpHandlerError::UpstreamPm(message) | McpHandlerError::Internal(message) => {
            JsonRpcResponse::internal_error(id, message)
        }
    };
    if let (Some(error), Some(metadata)) = (response.error.as_mut(), metadata) {
        error.data = Some(metadata.into_value());
    }
    response
}

#[allow(clippy::result_large_err)]
fn load_graph_artifact_for_request() -> Result<GraphIndexArtifact, McpHandlerError> {
    let current_dir = std::env::current_dir().map_err(|error| {
        McpHandlerError::Internal(format!("failed to read current directory: {error}"))
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
            Err(graph_artifact_missing(&worktree))
        }
        Err(_) if !artifact_path.exists() => Err(graph_artifact_missing(&worktree)),
        Err(error) => Err(McpHandlerError::Internal(format!(
            "failed to load graph artifact `{}`: {error}",
            artifact_path.display()
        ))),
    }
}

fn graph_artifact_missing(worktree: &Path) -> McpHandlerError {
    McpHandlerError::Internal(format!(
        "graph artifact not found; run `spur graph build` in {}",
        worktree.display()
    ))
}

enum CodeSelectorResolution {
    Resolved(String),
    Ambiguous(Vec<CandidateRow>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum OnAmbiguousMode {
    Candidates,
    Error,
}

fn resolve_code_selector(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<CodeSelectorResolution, McpHandlerError> {
    let selector = selected_code_selector(args)?;
    let on_ambiguous = on_ambiguous_mode(args)?;

    match resolve_selector(artifact, selector) {
        SelectorResolution::Resolved(resolved) => {
            Ok(CodeSelectorResolution::Resolved(resolved.stable_symbol_id))
        }
        SelectorResolution::Ambiguous { candidates: _ }
            if on_ambiguous == OnAmbiguousMode::Error =>
        {
            Err(McpHandlerError::InvalidParams(format!(
                "selector `{selector}` is ambiguous; choose one candidate selector or uri"
            )))
        }
        SelectorResolution::Ambiguous { candidates } => {
            Ok(CodeSelectorResolution::Ambiguous(candidates))
        }
        SelectorResolution::NotFound => Err(McpHandlerError::NotFound(format!(
            "symbol {} not found in graph artifact",
            missing_symbol_label(selector)
        ))),
    }
}

fn selected_code_selector(args: &Value) -> Result<&str, McpHandlerError> {
    let selector = string_arg(args, "selector")?;
    let symbol = string_arg(args, "symbol")?;

    match (selector, symbol) {
        (Some(selector), Some(_)) => {
            tracing::warn!(
                "code graph request included deprecated `symbol` with `selector`; using `selector`"
            );
            Ok(selector)
        }
        (Some(selector), None) => Ok(selector),
        (None, Some(symbol)) => {
            tracing::warn!("code graph request used deprecated `symbol`; use `selector`");
            Ok(symbol)
        }
        (None, None) => Err(McpHandlerError::InvalidParams(
            "Missing required field 'selector' (or deprecated 'symbol')".into(),
        )),
    }
}

fn selector_arg(args: &Value) -> Result<&str, McpHandlerError> {
    string_arg(args, "selector")?
        .ok_or_else(|| McpHandlerError::InvalidParams("Missing required field 'selector'".into()))
}

fn file_arg(args: &Value) -> Result<&str, McpHandlerError> {
    string_arg(args, "file")?
        .ok_or_else(|| McpHandlerError::InvalidParams("Missing required field 'file'".into()))
}

fn validate_file_path_arg(file: &str) -> Result<String, McpHandlerError> {
    let path = Path::new(file);
    if path.is_absolute() {
        return Err(McpHandlerError::InvalidParams(
            "field 'file' must be a worktree-relative path".into(),
        ));
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {
                return Err(McpHandlerError::InvalidParams(
                    "field 'file' must not contain '.' path components".into(),
                ));
            }
            Component::ParentDir => {
                return Err(McpHandlerError::InvalidParams(
                    "field 'file' must not contain '..' path components".into(),
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(McpHandlerError::InvalidParams(
                    "field 'file' must be a worktree-relative path".into(),
                ));
            }
        }
    }

    let normalized = normalized.to_string_lossy().into_owned();
    if normalized != file {
        return Err(McpHandlerError::InvalidParams(
            "field 'file' must be a normalized worktree-relative path without '.' or '..' components"
                .into(),
        ));
    }

    Ok(normalized)
}

fn string_arg<'a>(args: &'a Value, field: &str) -> Result<Option<&'a str>, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(None);
    };
    let value = value.as_str().ok_or_else(|| {
        McpHandlerError::InvalidParams(format!("field '{field}' must be a string"))
    })?;
    if value.trim().is_empty() {
        return Err(McpHandlerError::InvalidParams(format!(
            "field '{field}' must not be empty"
        )));
    }
    Ok(Some(value))
}

fn on_ambiguous_mode(args: &Value) -> Result<OnAmbiguousMode, McpHandlerError> {
    let Some(value) = args.get("on_ambiguous") else {
        return Ok(OnAmbiguousMode::Candidates);
    };
    match value.as_str() {
        Some("candidates") => Ok(OnAmbiguousMode::Candidates),
        Some("error") => Ok(OnAmbiguousMode::Error),
        Some(other) => Err(McpHandlerError::InvalidParams(format!(
            "invalid on_ambiguous `{other}`; expected `candidates` or `error`"
        ))),
        None => Err(McpHandlerError::InvalidParams(
            "field 'on_ambiguous' must be a string".into(),
        )),
    }
}

fn missing_symbol_label(selector: &str) -> &str {
    selector
        .strip_prefix(CODE_SYMBOL_URI_PREFIX)
        .unwrap_or(selector)
}

fn parse_edge_kinds(args: &Value) -> Result<Option<Vec<RelationKind>>, McpHandlerError> {
    let Some(value) = args.get("edge_kinds") else {
        return Ok(None);
    };
    let kinds = value.as_array().ok_or_else(|| {
        McpHandlerError::InvalidParams("field 'edge_kinds' must be an array of strings".to_string())
    })?;
    kinds
        .iter()
        .map(|kind| {
            let kind = kind.as_str().ok_or_else(|| {
                McpHandlerError::InvalidParams(
                    "field 'edge_kinds' must be an array of strings".to_string(),
                )
            })?;
            serde_json::from_value::<RelationKind>(Value::String(kind.to_string()))
                .map_err(|_| McpHandlerError::InvalidParams(format!("invalid edge kind `{kind}`")))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn resolve_candidate_rows(
    artifact: &GraphIndexArtifact,
    selector: &str,
) -> Result<Vec<CandidateRow>, McpHandlerError> {
    match resolve_selector(artifact, selector) {
        SelectorResolution::Resolved(resolved) => {
            let symbol = symbol_by_id(artifact, &resolved.stable_symbol_id)?;
            Ok(vec![candidate_row_for_symbol(symbol)])
        }
        SelectorResolution::Ambiguous { candidates } => Ok(candidates),
        SelectorResolution::NotFound => Err(McpHandlerError::NotFound(format!(
            "symbol {} not found in graph artifact",
            missing_symbol_label(selector)
        ))),
    }
}

fn symbol_by_id<'a>(
    artifact: &'a GraphIndexArtifact,
    symbol_id: &str,
) -> Result<&'a GraphSymbolArtifact, McpHandlerError> {
    artifact
        .symbols
        .iter()
        .find(|symbol| symbol.stable_symbol_id == symbol_id)
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "resolved symbol id `{symbol_id}` missing from graph artifact"
            ))
        })
}

fn candidate_rows_for_symbols<'a>(
    symbols: impl IntoIterator<Item = &'a GraphSymbolArtifact>,
) -> Vec<CandidateRow> {
    let mut rows = symbols
        .into_iter()
        .map(candidate_row_for_symbol)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line_range[0].cmp(&right.line_range[0]))
            .then_with(|| left.line_range[1].cmp(&right.line_range[1]))
            .then_with(|| left.qualified_name.cmp(&right.qualified_name))
            .then_with(|| left.id.cmp(&right.id))
    });
    rows
}

fn candidate_row_for_symbol(symbol: &GraphSymbolArtifact) -> CandidateRow {
    CandidateRow {
        selector: format!("{}::{}", symbol.file_path, symbol.qualified_name),
        uri: symbol_uri(&symbol.stable_symbol_id),
        id: symbol.stable_symbol_id.clone(),
        qualified_name: symbol.qualified_name.clone(),
        file_path: symbol.file_path.clone(),
        line_range: symbol.line_range,
        symbol_kind: symbol.symbol_kind.clone(),
    }
}

fn ambiguous_response(artifact: &GraphIndexArtifact, candidates: Vec<CandidateRow>) -> Value {
    with_graph_metadata(
        artifact,
        json!({
            "ambiguous": true,
            "candidates": candidates.into_iter().map(candidate_row).collect::<Vec<_>>(),
        }),
    )
}

fn with_graph_metadata(artifact: &GraphIndexArtifact, mut body: Value) -> Value {
    if let Value::Object(map) = &mut body {
        map.insert(
            "graph_content_hash".to_string(),
            Value::String(artifact.graph_content_hash.clone()),
        );
        map.insert(
            "graph_index_version".to_string(),
            Value::String(artifact.header.graph_index_version.clone()),
        );
    }
    body
}

fn candidate_row(candidate: CandidateRow) -> Value {
    json!({
        "selector": candidate.selector,
        "uri": candidate.uri,
        "id": candidate.id,
        "qualified_name": candidate.qualified_name,
        "file_path": candidate.file_path,
        "line_range": candidate.line_range,
        "symbol_kind": candidate.symbol_kind,
    })
}

fn symbol_info_row(symbol: &GraphSymbolArtifact) -> Value {
    json!({
        "qualified_name": symbol.qualified_name,
        "file_path": symbol.file_path,
        "line_range": symbol.line_range,
        "symbol_kind": symbol.symbol_kind,
        "enclosing_scope": symbol.enclosing_scope,
        "uri": symbol_uri(&symbol.stable_symbol_id),
        "id": symbol.stable_symbol_id,
    })
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
                "files": [
                    { "stable_file_id": "file-src-caller", "file_path": "src/caller.rs" },
                    { "stable_file_id": "file-src-root", "file_path": "src/root.rs" },
                    { "stable_file_id": "file-src-callee", "file_path": "src/callee.rs" },
                    { "stable_file_id": "file-crates-foo", "file_path": "crates/foo" },
                    { "stable_file_id": "file-crates-other", "file_path": "crates/other" }
                ],
                "symbols": [
                    symbol("caller", "src/caller.rs", [3, 5], "call_root", "call_root"),
                    symbol("root", "src/root.rs", [10, 12], "root", "root"),
                    symbol("callee", "src/callee.rs", [20, 22], "callee", "callee"),
                    symbol("cache-caller", "crates/foo", [24, 26], "call_cache", "call_cache"),
                    symbol("cache-run", "crates/foo", [30, 32], "run", "Cache::run"),
                    symbol("cache-callee", "crates/foo", [34, 36], "finish_cache", "finish_cache"),
                    symbol("other-run", "crates/other", [40, 42], "run", "Other::run")
                ],
                "edges": [
                    edge("caller", "root"),
                    edge("root", "callee"),
                    edge("cache-caller", "cache-run"),
                    edge("cache-run", "cache-callee")
                ],
                "tombstones": []
            }))
            .expect("encode artifact"),
        )
        .expect("write artifact");
    }

    fn symbol(
        id: &str,
        file_path: &str,
        line_range: [usize; 2],
        entity_name: &str,
        qualified_name: &str,
    ) -> Value {
        json!({
            "stable_symbol_id": id,
            "file_path": file_path,
            "byte_range": [0, 8],
            "line_range": line_range,
            "entity_name": entity_name,
            "qualified_name": qualified_name,
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
        assert_eq!(body["graph_content_hash"], "test");
        assert_eq!(body["graph_index_version"], "test");
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
    async fn code_graph_handlers_resolve_selector_for_callers_and_callees() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let callers = response_json(
            server
                .handle_code_callers(
                    Value::from(1),
                    json!({ "selector": "crates/foo::Cache::run" }),
                )
                .await,
        );
        assert_eq!(callers["callers"].as_array().expect("callers").len(), 1);
        assert_eq!(callers["callers"][0]["uri"], "graph://symbol/cache-caller");
        assert_eq!(callers["graph_content_hash"], "test");
        assert_eq!(callers["graph_index_version"], "test");

        let callees = response_json(
            server
                .handle_code_callees(
                    Value::from(1),
                    json!({ "selector": "crates/foo::Cache::run" }),
                )
                .await,
        );
        assert_eq!(callees["callees"].as_array().expect("callees").len(), 1);
        assert_eq!(callees["callees"][0]["uri"], "graph://symbol/cache-callee");
        assert_eq!(callees["graph_content_hash"], "test");
        assert_eq!(callees["graph_index_version"], "test");
    }

    #[tokio::test]
    async fn selector_takes_precedence_over_legacy_symbol_when_both_are_present() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let body = response_json(
            server
                .handle_code_callees(
                    Value::from(1),
                    json!({ "selector": "crates/foo::Cache::run", "symbol": "root" }),
                )
                .await,
        );

        assert_eq!(body["callees"].as_array().expect("callees").len(), 1);
        assert_eq!(body["callees"][0]["uri"], "graph://symbol/cache-callee");
    }

    #[tokio::test]
    async fn ambiguous_selector_defaults_to_successful_candidates_response() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callers(Value::from(1), json!({ "selector": "run" }))
            .await;
        assert!(
            response.error.is_none(),
            "ambiguous default must be success"
        );
        let body = response_json(response);

        assert_eq!(body["ambiguous"], true);
        assert_eq!(body["candidates"].as_array().expect("candidates").len(), 2);
        assert_eq!(body["candidates"][0]["selector"], "crates/foo::Cache::run");
        assert_eq!(
            body["candidates"][1]["selector"],
            "crates/other::Other::run"
        );
        assert_eq!(body["graph_content_hash"], "test");
        assert_eq!(body["graph_index_version"], "test");
    }

    #[tokio::test]
    async fn ambiguous_selector_can_be_returned_as_json_rpc_error() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callers(
                Value::from(1),
                json!({ "selector": "run", "on_ambiguous": "error" }),
            )
            .await;

        let error = response.error.expect("ambiguous error");
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("selector `run` is ambiguous"));
        assert_eq!(
            error.data.as_ref().expect("graph metadata")["graph_content_hash"],
            "test"
        );
        assert_eq!(
            error.data.as_ref().expect("graph metadata")["graph_index_version"],
            "test"
        );
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
        assert_eq!(body["graph_content_hash"], "test");
        assert_eq!(body["graph_index_version"], "test");
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
        assert!(
            error.data.is_none(),
            "artifact-load failures must not echo graph metadata"
        );
    }
}

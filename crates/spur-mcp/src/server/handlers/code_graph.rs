use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Component, Path};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{json, Value};
use spur_graph::{
    bounded_subgraph_with_budget, edge_kind, find_callee_edges, find_caller_edges, load_artifact,
    resolve_selector, resolve_worktree_root_from, search_symbols, CalleeRecord, CallerRecord,
    CandidateRow, GraphEdgeArtifact, GraphEdgeKind, GraphIndexArtifact, GraphIndexPointer,
    GraphSymbolArtifact, SearchFilters, SearchMode, SearchOptions, SelectorResolution,
    SubgraphBudget, CODE_SYMBOL_URI_PREFIX,
};

use crate::handlers::McpHandlerError;

use super::McpCallbackServer;
use super::*;

const MAX_MCP_CODE_SUBGRAPH_RADIUS: u8 = 3;
const DEFAULT_MCP_CODE_SUBGRAPH_MAX_NODES: usize = 40;
const MIN_MCP_CODE_SUBGRAPH_MAX_NODES: usize = 1;
const MAX_MCP_CODE_SUBGRAPH_MAX_NODES: usize = 400;
const DEFAULT_MCP_CODE_SUBGRAPH_MAX_EDGES: usize = 120;
const MIN_MCP_CODE_SUBGRAPH_MAX_EDGES: usize = 1;
const MAX_MCP_CODE_SUBGRAPH_MAX_EDGES: usize = 1200;
const GRAPH_ARTIFACT_RELATIVE_PATH: &str = ".spur/graph-index.json";
const GRAPH_POINTER_RELATIVE_PATH: &str = ".spur/graph-index.pointer.json";
const GRAPH_GIT_METADATA_TIMEOUT: Duration = Duration::from_millis(200);

impl McpCallbackServer {
    pub(crate) async fn handle_code_resolve(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_resolve_response(&args).await).await
    }

    pub(crate) async fn handle_code_search(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_search_response(&args).await).await
    }

    pub(crate) async fn handle_code_file_symbols(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_file_symbols_response(&args).await).await
    }

    pub(crate) async fn handle_code_symbol_info(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_symbol_info_response(&args).await).await
    }

    pub(crate) async fn handle_code_callers(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_callers_response(&args).await).await
    }

    pub(crate) async fn handle_code_callees(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_callees_response(&args).await).await
    }

    pub(crate) async fn handle_code_subgraph(&self, id: Value, args: Value) -> JsonRpcResponse {
        code_graph_response(id, code_subgraph_response(&args).await).await
    }
}

pub(crate) async fn code_resolve(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body = code_resolve_with_artifact(args, &artifact)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

pub(crate) async fn code_search(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body = code_search_with_artifact(args, &artifact)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_search_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_search_with_artifact(args, artifact)).await
}

fn code_search_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let request = code_search_options(args)?;
    let options = request.options;
    let result = search_symbols(artifact, &options);
    let candidates = result
        .candidates
        .into_iter()
        .map(candidate_row_for_symbol)
        .map(candidate_row)
        .collect::<Vec<_>>();

    let mut body = json!({
        "query": options.query,
        "mode": search_mode_str(options.mode),
        "symbol_kind": options.filters.symbol_kind,
        "file": options.filters.file,
        "file_glob": options.filters.file_glob,
        "limit": options.limit,
        "total_matches": result.total_matches,
        "truncated": result.truncated,
        "candidates": candidates,
    });
    if let Some(requested_limit) = request.requested_limit {
        body["requested_limit"] = requested_limit;
    }
    Ok(body)
}

async fn code_resolve_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_resolve_with_artifact(args, artifact)).await
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

    Ok(json!({ "candidates": candidates }))
}

pub(crate) async fn code_file_symbols(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body = code_file_symbols_with_artifact(args, &artifact)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_file_symbols_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_file_symbols_with_artifact(args, artifact)).await
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

    Ok(json!({ "symbols": symbols }))
}

pub(crate) async fn code_symbol_info(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body = code_symbol_info_with_artifact(args, &artifact)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_symbol_info_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_symbol_info_with_artifact(args, artifact)).await
}

fn code_symbol_info_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let symbol_id = match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => symbol_id,
        CodeSelectorResolution::Ambiguous(candidates) => {
            return Ok(ambiguous_response(candidates));
        }
    };
    let symbol = symbol_by_id(artifact, &symbol_id)?;

    Ok(json!({ "symbol": symbol_info_row(symbol) }))
}

pub(crate) async fn code_callers(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body = code_callers_with_artifact(args, &artifact)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_callers_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_callers_with_artifact(args, artifact)).await
}

fn code_callers_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let request = code_traversal_request(args)?;
    let symbol_id = match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => symbol_id,
        CodeSelectorResolution::Ambiguous(candidates) => {
            return Ok(ambiguous_response(candidates));
        }
    };

    let records = find_caller_edges(artifact, &symbol_id);
    let summary = caller_summary(&records);
    let callers = records
        .into_iter()
        .filter(|record| request.include_unresolved || record.is_resolved())
        .map(caller_row)
        .collect::<Vec<_>>();
    Ok(json!({
        "callers": callers,
        "include_unresolved": request.include_unresolved,
        "counts_by_kind": summary.counts_by_kind,
        "unresolved_sample": summary.unresolved_sample,
    }))
}

pub(crate) async fn code_callees(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body = code_callees_with_artifact(args, &artifact)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_callees_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_callees_with_artifact(args, artifact)).await
}

fn code_callees_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let request = code_traversal_request(args)?;
    let symbol_id = match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => symbol_id,
        CodeSelectorResolution::Ambiguous(candidates) => {
            return Ok(ambiguous_response(candidates));
        }
    };

    let records = find_callee_edges(artifact, &symbol_id);
    let summary = callee_summary(&records);
    let callees = records
        .into_iter()
        .filter(|record| request.include_unresolved || record.is_resolved())
        .map(callee_row)
        .collect::<Vec<_>>();
    Ok(json!({
        "callees": callees,
        "include_unresolved": request.include_unresolved,
        "counts_by_kind": summary.counts_by_kind,
        "unresolved_sample": summary.unresolved_sample,
    }))
}

pub(crate) async fn code_subgraph(args: &Value) -> Result<Value, McpHandlerError> {
    let artifact = load_graph_artifact_for_request()?;
    let body = code_subgraph_with_artifact(args, &artifact)?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_subgraph_response(args: &Value) -> CodeGraphResult {
    with_loaded_graph_artifact(|artifact| code_subgraph_with_artifact(args, artifact)).await
}

fn code_subgraph_with_artifact(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<Value, McpHandlerError> {
    let request = code_traversal_request(args)?;
    let root_ids = match code_subgraph_root_ids(args, artifact)? {
        CodeSubgraphRoots::RootIds(root_ids) => root_ids,
        CodeSubgraphRoots::Ambiguous(candidates) => {
            return Ok(ambiguous_response(candidates));
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
    let budget = code_subgraph_budget(args)?;
    let root_refs = root_ids.iter().map(String::as_str).collect::<Vec<_>>();
    let view = bounded_subgraph_with_budget(
        artifact,
        &root_refs,
        radius,
        edge_filter,
        request.include_unresolved,
        budget.budget,
    );

    match format {
        "json" => {
            let mut metadata = code_subgraph_metadata(radius, view.truncated, &budget);
            if let Some(warning) = warning {
                metadata["warning"] = Value::String(warning);
            }
            Ok(json!({
                "nodes": view.nodes.into_iter().map(symbol_row).collect::<Vec<_>>(),
                "edges": view.edges.into_iter().map(edge_row).collect::<Vec<_>>(),
                "truncated_frontier": view.truncated_frontier,
                "include_unresolved": request.include_unresolved,
                "metadata": metadata,
            }))
        }
        "mermaid" => {
            let mut metadata = code_subgraph_metadata(radius, view.truncated, &budget);
            if let Some(warning) = warning {
                metadata["warning"] = Value::String(warning);
            }
            let mermaid = mermaid_subgraph(&view.nodes, &view.edges);
            Ok(json!({
                "mermaid": mermaid,
                "truncated_frontier": view.truncated_frontier,
                "include_unresolved": request.include_unresolved,
                "metadata": metadata,
            }))
        }
        other => Err(McpHandlerError::InvalidParams(format!(
            "invalid format `{other}`; expected `json` or `mermaid`"
        ))),
    }
}

type CodeGraphResult = Result<Value, CodeGraphError>;

#[derive(Debug)]
struct CodeGraphError {
    error: McpHandlerError,
    metadata: Option<GraphMetadataSource>,
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
            metadata: Some(GraphMetadataSource::from_artifact(artifact)),
        }
    }
}

#[derive(Debug, Clone)]
struct GraphMetadataSource {
    graph_content_hash: String,
    graph_index_version: String,
    manifest_version: String,
}

impl GraphMetadataSource {
    fn from_artifact(artifact: &GraphIndexArtifact) -> Self {
        Self {
            graph_content_hash: artifact.graph_content_hash.clone(),
            graph_index_version: artifact.header.graph_index_version.clone(),
            manifest_version: artifact.manifest_version.clone(),
        }
    }
}

#[derive(Debug)]
struct GraphResponseMetadata {
    source: GraphMetadataSource,
    graph_built_at: Option<String>,
    indexed_head_oid: Option<String>,
    worktree_head_oid: Option<String>,
    worktree_dirty: Option<bool>,
}

impl GraphResponseMetadata {
    async fn from_artifact(artifact: &GraphIndexArtifact) -> Self {
        Self::from_source(GraphMetadataSource::from_artifact(artifact)).await
    }

    async fn from_source(source: GraphMetadataSource) -> Self {
        let worktree = current_worktree_root();
        let pointer = worktree
            .as_deref()
            .and_then(|worktree| matching_graph_pointer(worktree, &source));
        let graph_built_at = pointer.as_ref().and_then(graph_built_at_from_pointer);
        let indexed_head_oid = pointer
            .as_ref()
            .and_then(|pointer| non_empty_string(pointer.indexed_commit_oid.clone()));
        let git = match worktree.as_deref() {
            Some(worktree) => worktree_git_metadata(worktree).await,
            None => None,
        };
        let worktree_head_oid = git.as_ref().map(|git| git.head_oid.clone());
        let worktree_dirty = git.as_ref().and_then(|git| {
            compute_worktree_dirty(
                indexed_head_oid.as_deref(),
                &git.head_oid,
                git.has_uncommitted_changes,
            )
        });

        Self {
            source,
            graph_built_at,
            indexed_head_oid,
            worktree_head_oid,
            worktree_dirty,
        }
    }

    fn into_value(self) -> Value {
        json!({
            "graph_content_hash": self.source.graph_content_hash,
            "graph_index_version": self.source.graph_index_version,
            "graph_built_at": self.graph_built_at,
            "indexed_head_oid": self.indexed_head_oid,
            "worktree_head_oid": self.worktree_head_oid,
            "worktree_dirty": self.worktree_dirty,
        })
    }

    fn insert_into(self, body: &mut Value) {
        if let Value::Object(map) = body {
            let Value::Object(metadata) = self.into_value() else {
                return;
            };
            map.extend(metadata);
        }
    }
}

async fn with_loaded_graph_artifact(
    handler: impl FnOnce(&GraphIndexArtifact) -> Result<Value, McpHandlerError>,
) -> CodeGraphResult {
    let artifact = load_graph_artifact_for_request().map_err(CodeGraphError::without_metadata)?;
    let body =
        handler(&artifact).map_err(|error| CodeGraphError::with_artifact(error, &artifact))?;
    Ok(with_graph_metadata(&artifact, body).await)
}

async fn code_graph_response(id: Value, result: CodeGraphResult) -> JsonRpcResponse {
    match result {
        Ok(body) => json_success(id, body),
        Err(error) => code_graph_error_response(id, error).await,
    }
}

async fn code_graph_error_response(id: Value, error: CodeGraphError) -> JsonRpcResponse {
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
        error.data = Some(
            GraphResponseMetadata::from_source(metadata)
                .await
                .into_value(),
        );
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

#[derive(Debug)]
struct CodeSearchRequest {
    options: SearchOptions,
    requested_limit: Option<Value>,
}

#[derive(Debug)]
struct CodeTraversalRequest {
    include_unresolved: bool,
}

#[derive(Debug)]
enum CodeSubgraphRoots {
    RootIds(Vec<String>),
    Ambiguous(Vec<CandidateRow>),
}

#[derive(Debug)]
struct CodeSubgraphBudgetRequest {
    budget: SubgraphBudget,
    requested_max_nodes: Option<Value>,
    requested_max_edges: Option<Value>,
}

#[derive(Debug)]
struct ClampedUsizeArg {
    value: usize,
    requested_value: Option<Value>,
}

#[derive(Debug)]
struct LimitArg {
    limit: usize,
    requested_limit: Option<Value>,
}

fn code_search_options(args: &Value) -> Result<CodeSearchRequest, McpHandlerError> {
    let query = query_arg(args)?;
    let mode = search_mode_arg(args)?;
    let symbol_kind = string_arg(args, "symbol_kind")?.map(str::to_string);
    let file = string_arg(args, "file")?
        .map(validate_file_path_arg)
        .transpose()?;
    let file_glob = string_arg(args, "file_glob")?
        .map(validate_file_glob_arg)
        .transpose()?;
    if file.is_some() && file_glob.is_some() {
        return Err(McpHandlerError::InvalidParams(
            "fields 'file' and 'file_glob' are mutually exclusive".into(),
        ));
    }
    let limit = limit_arg(args)?;

    Ok(CodeSearchRequest {
        options: SearchOptions {
            query,
            mode,
            filters: SearchFilters {
                symbol_kind,
                file,
                file_glob,
            },
            limit: limit.limit,
        },
        requested_limit: limit.requested_limit,
    })
}

fn code_traversal_request(args: &Value) -> Result<CodeTraversalRequest, McpHandlerError> {
    Ok(CodeTraversalRequest {
        include_unresolved: bool_arg(args, "include_unresolved")?.unwrap_or(false),
    })
}

fn code_subgraph_root_ids(
    args: &Value,
    artifact: &GraphIndexArtifact,
) -> Result<CodeSubgraphRoots, McpHandlerError> {
    if let Some(start_nodes) = start_nodes_arg(args)? {
        if string_arg(args, "selector")?.is_some() || string_arg(args, "symbol")?.is_some() {
            return Err(McpHandlerError::InvalidParams(
                "field 'start_nodes' is mutually exclusive with 'selector' and 'symbol'".into(),
            ));
        }
        for node_id in &start_nodes {
            ensure_symbol_id_exists(artifact, node_id)?;
        }
        return Ok(CodeSubgraphRoots::RootIds(start_nodes));
    }

    match resolve_code_selector(args, artifact)? {
        CodeSelectorResolution::Resolved(symbol_id) => {
            Ok(CodeSubgraphRoots::RootIds(vec![symbol_id]))
        }
        CodeSelectorResolution::Ambiguous(candidates) => {
            Ok(CodeSubgraphRoots::Ambiguous(candidates))
        }
    }
}

fn start_nodes_arg(args: &Value) -> Result<Option<Vec<String>>, McpHandlerError> {
    let Some(value) = args.get("start_nodes") else {
        return Ok(None);
    };
    let nodes = value.as_array().ok_or_else(|| {
        McpHandlerError::InvalidParams("field 'start_nodes' must be an array of strings".into())
    })?;
    if nodes.is_empty() {
        return Err(McpHandlerError::InvalidParams(
            "field 'start_nodes' must contain at least one node id".into(),
        ));
    }

    let mut seen = HashSet::new();
    let mut start_nodes = Vec::new();
    for node in nodes {
        let node = node.as_str().ok_or_else(|| {
            McpHandlerError::InvalidParams("field 'start_nodes' must be an array of strings".into())
        })?;
        if node.trim().is_empty() {
            return Err(McpHandlerError::InvalidParams(
                "field 'start_nodes' must not contain empty node ids".into(),
            ));
        }
        let node_id = missing_symbol_label(node);
        if node_id.trim().is_empty() {
            return Err(McpHandlerError::InvalidParams(
                "field 'start_nodes' must not contain empty node ids".into(),
            ));
        }
        let node_id = node_id.to_string();
        if seen.insert(node_id.clone()) {
            start_nodes.push(node_id);
        }
    }

    Ok(Some(start_nodes))
}

fn ensure_symbol_id_exists(
    artifact: &GraphIndexArtifact,
    symbol_id: &str,
) -> Result<(), McpHandlerError> {
    if artifact
        .symbols
        .iter()
        .any(|symbol| symbol.stable_symbol_id == symbol_id)
    {
        Ok(())
    } else {
        Err(McpHandlerError::NotFound(format!(
            "symbol {} not found in graph artifact",
            missing_symbol_label(symbol_id)
        )))
    }
}

fn code_subgraph_budget(args: &Value) -> Result<CodeSubgraphBudgetRequest, McpHandlerError> {
    let max_nodes = clamped_usize_arg(
        args,
        "max_nodes",
        DEFAULT_MCP_CODE_SUBGRAPH_MAX_NODES,
        MIN_MCP_CODE_SUBGRAPH_MAX_NODES,
        MAX_MCP_CODE_SUBGRAPH_MAX_NODES,
    )?;
    let max_edges = clamped_usize_arg(
        args,
        "max_edges",
        DEFAULT_MCP_CODE_SUBGRAPH_MAX_EDGES,
        MIN_MCP_CODE_SUBGRAPH_MAX_EDGES,
        MAX_MCP_CODE_SUBGRAPH_MAX_EDGES,
    )?;

    Ok(CodeSubgraphBudgetRequest {
        budget: SubgraphBudget {
            max_nodes: max_nodes.value,
            max_edges: max_edges.value,
        },
        requested_max_nodes: max_nodes.requested_value,
        requested_max_edges: max_edges.requested_value,
    })
}

fn clamped_usize_arg(
    args: &Value,
    field: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<ClampedUsizeArg, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(ClampedUsizeArg {
            value: default,
            requested_value: None,
        });
    };
    if let Some(limit) = value.as_i64() {
        let clamped = limit.clamp(min as i64, max as i64);
        return Ok(ClampedUsizeArg {
            value: clamped as usize,
            requested_value: (limit != clamped).then(|| json!(limit)),
        });
    }
    if let Some(limit) = value.as_u64() {
        let clamped = limit.clamp(min as u64, max as u64);
        return Ok(ClampedUsizeArg {
            value: clamped as usize,
            requested_value: (limit != clamped).then(|| json!(limit)),
        });
    }

    Err(McpHandlerError::InvalidParams(format!(
        "field '{field}' must be an integer"
    )))
}

fn code_subgraph_metadata(
    radius: u8,
    truncated: bool,
    budget: &CodeSubgraphBudgetRequest,
) -> Value {
    let mut metadata = json!({
        "radius": radius,
        "max_nodes": budget.budget.max_nodes,
        "max_edges": budget.budget.max_edges,
        "truncated": truncated,
    });
    if let Some(requested_max_nodes) = &budget.requested_max_nodes {
        metadata["requested_max_nodes"] = requested_max_nodes.clone();
    }
    if let Some(requested_max_edges) = &budget.requested_max_edges {
        metadata["requested_max_edges"] = requested_max_edges.clone();
    }
    metadata
}

fn query_arg(args: &Value) -> Result<String, McpHandlerError> {
    let value = args
        .get("query")
        .ok_or_else(|| McpHandlerError::InvalidParams("Missing required field 'query'".into()))?;
    let query = value
        .as_str()
        .ok_or_else(|| McpHandlerError::InvalidParams("field 'query' must be a string".into()))?
        .trim();
    if query.is_empty() {
        return Err(McpHandlerError::InvalidParams(
            "field 'query' must not be empty".into(),
        ));
    }
    Ok(query.to_string())
}

fn search_mode_arg(args: &Value) -> Result<SearchMode, McpHandlerError> {
    let Some(value) = args.get("mode") else {
        return Ok(SearchMode::Substring);
    };
    match value.as_str() {
        Some("exact") => Ok(SearchMode::Exact),
        Some("prefix") => Ok(SearchMode::Prefix),
        Some("substring") => Ok(SearchMode::Substring),
        Some(other) => Err(McpHandlerError::InvalidParams(format!(
            "invalid mode `{other}`; expected `exact`, `prefix`, or `substring`"
        ))),
        None => Err(McpHandlerError::InvalidParams(
            "field 'mode' must be a string".into(),
        )),
    }
}

fn limit_arg(args: &Value) -> Result<LimitArg, McpHandlerError> {
    let Some(value) = args.get("limit") else {
        return Ok(LimitArg {
            limit: 20,
            requested_limit: None,
        });
    };
    if let Some(limit) = value.as_i64() {
        let clamped = limit.clamp(1, 200);
        return Ok(LimitArg {
            limit: clamped as usize,
            requested_limit: (limit != clamped).then(|| json!(limit)),
        });
    }
    if let Some(limit) = value.as_u64() {
        let clamped = limit.clamp(1, 200);
        return Ok(LimitArg {
            limit: clamped as usize,
            requested_limit: (limit != clamped).then(|| json!(limit)),
        });
    }
    Err(McpHandlerError::InvalidParams(
        "field 'limit' must be an integer".into(),
    ))
}

fn search_mode_str(mode: SearchMode) -> &'static str {
    match mode {
        SearchMode::Exact => "exact",
        SearchMode::Prefix => "prefix",
        SearchMode::Substring => "substring",
    }
}

fn validate_file_path_arg(file: &str) -> Result<String, McpHandlerError> {
    validate_worktree_relative_path_arg("file", file)
}

fn validate_file_glob_arg(file_glob: &str) -> Result<String, McpHandlerError> {
    validate_worktree_relative_path_arg("file_glob", file_glob)
}

fn validate_worktree_relative_path_arg(
    field: &str,
    value: &str,
) -> Result<String, McpHandlerError> {
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(McpHandlerError::InvalidParams(format!(
            "field '{field}' must be a worktree-relative path"
        )));
    }

    let mut normalized = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Err(McpHandlerError::InvalidParams(format!(
                        "field '{field}' must be a UTF-8 path"
                    )));
                };
                normalized.push(part);
            }
            Component::CurDir => {
                return Err(McpHandlerError::InvalidParams(format!(
                    "field '{field}' must not contain '.' path components"
                )));
            }
            Component::ParentDir => {
                return Err(McpHandlerError::InvalidParams(format!(
                    "field '{field}' must not contain '..' path components"
                )));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(McpHandlerError::InvalidParams(format!(
                    "field '{field}' must be a worktree-relative path"
                )));
            }
        }
    }

    let normalized = normalized.join("/");
    if normalized != value {
        return Err(McpHandlerError::InvalidParams(format!(
            "field '{field}' must be a normalized worktree-relative path without '.' or '..' components"
        )));
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

fn bool_arg(args: &Value, field: &str) -> Result<Option<bool>, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| McpHandlerError::InvalidParams(format!("field '{field}' must be a boolean")))
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

fn parse_edge_kinds(args: &Value) -> Result<Option<Vec<GraphEdgeKind>>, McpHandlerError> {
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
            serde_json::from_value::<GraphEdgeKind>(Value::String(kind.to_string())).map_err(
                |_| {
                    McpHandlerError::InvalidParams(format!(
                        "invalid edge kind `{kind}`; expected one of calls, calls_dyn, references_hof, references_other"
                    ))
                },
            )
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
    let uri = format!("{CODE_SYMBOL_URI_PREFIX}{}", symbol.stable_symbol_id);
    let selector = if symbol.qualified_name.is_empty() {
        uri.clone()
    } else {
        format!("{}::{}", symbol.file_path, symbol.qualified_name)
    };

    CandidateRow {
        selector,
        uri,
        id: symbol.stable_symbol_id.clone(),
        entity_name: symbol.entity_name.clone(),
        qualified_name: symbol.qualified_name.clone(),
        file_path: symbol.file_path.clone(),
        line_range: symbol.line_range,
        symbol_kind: symbol.symbol_kind.clone(),
        enclosing_scope: symbol.enclosing_scope.clone(),
    }
}

fn ambiguous_response(candidates: Vec<CandidateRow>) -> Value {
    json!({
        "ambiguous": true,
        "candidates": candidates.into_iter().map(candidate_row).collect::<Vec<_>>(),
    })
}

async fn with_graph_metadata(artifact: &GraphIndexArtifact, mut body: Value) -> Value {
    GraphResponseMetadata::from_artifact(artifact)
        .await
        .insert_into(&mut body);
    body
}

fn current_worktree_root() -> Option<std::path::PathBuf> {
    std::env::current_dir().ok().map(resolve_worktree_root_from)
}

fn matching_graph_pointer(
    worktree: &Path,
    source: &GraphMetadataSource,
) -> Option<GraphIndexPointer> {
    let pointer_path = worktree.join(GRAPH_POINTER_RELATIVE_PATH);
    let bytes = std::fs::read(pointer_path).ok()?;
    let pointer: GraphIndexPointer = serde_json::from_slice(&bytes).ok()?;
    if pointer.graph_content_hash == source.graph_content_hash
        && pointer.manifest_version == source.manifest_version
    {
        Some(pointer)
    } else {
        None
    }
}

fn graph_built_at_from_pointer(pointer: &GraphIndexPointer) -> Option<String> {
    let modified = std::fs::metadata(&pointer.canonical_artifact_path)
        .ok()?
        .modified()
        .ok()?;
    let built_at = DateTime::<Utc>::from(modified);
    Some(built_at.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn non_empty_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| (!value.trim().is_empty()).then_some(value))
}

#[derive(Debug)]
struct WorktreeGitMetadata {
    head_oid: String,
    has_uncommitted_changes: bool,
}

async fn worktree_git_metadata(worktree: &Path) -> Option<WorktreeGitMetadata> {
    tokio::time::timeout(GRAPH_GIT_METADATA_TIMEOUT, async {
        let head_oid = run_git_stdout(worktree, &["rev-parse", "HEAD"]).await?;
        let status = run_git_stdout(worktree, &["status", "--porcelain"]).await?;
        Some(WorktreeGitMetadata {
            head_oid,
            has_uncommitted_changes: !status.is_empty(),
        })
    })
    .await
    .ok()
    .flatten()
}

async fn run_git_stdout(worktree: &Path, args: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new("git");
    command.args(args).current_dir(worktree).kill_on_drop(true);
    let output = command.output().await.ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8(output.stdout).ok()?.trim().to_string())
}

fn compute_worktree_dirty(
    indexed_head_oid: Option<&str>,
    worktree_head_oid: &str,
    has_uncommitted_changes: bool,
) -> Option<bool> {
    if has_uncommitted_changes {
        Some(true)
    } else {
        indexed_head_oid.map(|indexed_head_oid| indexed_head_oid != worktree_head_oid)
    }
}

fn candidate_row(candidate: CandidateRow) -> Value {
    json!({
        "selector": candidate.selector,
        "uri": candidate.uri,
        "id": candidate.id,
        "entity_name": candidate.entity_name,
        "qualified_name": candidate.qualified_name,
        "file_path": candidate.file_path,
        "line_range": candidate.line_range,
        "symbol_kind": candidate.symbol_kind,
        "enclosing_scope": candidate.enclosing_scope,
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

#[derive(Debug)]
struct TraversalSummary {
    counts_by_kind: Value,
    unresolved_sample: Vec<String>,
}

fn caller_summary(records: &[CallerRecord<'_>]) -> TraversalSummary {
    let unresolved = records.iter().filter_map(|record| match record {
        CallerRecord::Unresolved { target_label, .. } => Some(target_label.as_str()),
        CallerRecord::Resolved { .. } => None,
    });
    traversal_summary(records.iter().map(CallerRecord::edge), unresolved)
}

fn callee_summary(records: &[CalleeRecord<'_>]) -> TraversalSummary {
    let unresolved = records.iter().filter_map(|record| match record {
        CalleeRecord::Unresolved { target_label, .. } => Some(target_label.as_str()),
        CalleeRecord::Resolved { .. } => None,
    });
    traversal_summary(records.iter().map(CalleeRecord::edge), unresolved)
}

fn traversal_summary<'a>(
    edges: impl IntoIterator<Item = &'a GraphEdgeArtifact>,
    unresolved_labels: impl IntoIterator<Item = &'a str>,
) -> TraversalSummary {
    let mut calls = 0usize;
    let mut calls_dyn = 0usize;
    let mut references_hof = 0usize;
    let mut references_other = 0usize;
    let mut unresolved = 0usize;

    for edge in edges {
        match edge_kind(edge) {
            GraphEdgeKind::Calls => calls += 1,
            GraphEdgeKind::CallsDyn => calls_dyn += 1,
            GraphEdgeKind::ReferencesHof => references_hof += 1,
            GraphEdgeKind::ReferencesOther => references_other += 1,
        }
        if edge.target_stable_symbol_id.is_none() {
            unresolved += 1;
        }
    }

    TraversalSummary {
        counts_by_kind: json!({
            "calls": calls,
            "calls_dyn": calls_dyn,
            "references_hof": references_hof,
            "references_other": references_other,
            "unresolved": unresolved,
        }),
        unresolved_sample: unresolved_sample(unresolved_labels),
    }
}

fn unresolved_sample<'a>(labels: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut sample = Vec::new();
    let mut bytes = 0usize;

    for label in labels {
        if sample.len() >= 5 || !seen.insert(label) {
            continue;
        }
        let next_bytes = bytes + label.len();
        if next_bytes > 120 {
            break;
        }
        bytes = next_bytes;
        sample.push(label.to_string());
    }

    sample
}

fn caller_row(caller: CallerRecord<'_>) -> Value {
    match caller {
        CallerRecord::Resolved { caller, edge } => {
            let mut row = symbol_row(caller);
            add_edge_metadata(&mut row, edge, true, None);
            row
        }
        CallerRecord::Unresolved {
            caller,
            edge,
            target_label,
        } => {
            let mut row = symbol_row(caller);
            add_edge_metadata(&mut row, edge, false, Some(target_label));
            row
        }
    }
}

fn callee_row(callee: CalleeRecord<'_>) -> Value {
    match callee {
        CalleeRecord::Resolved { symbol, edge } => {
            let mut row = symbol_row(symbol);
            add_edge_metadata(&mut row, edge, true, None);
            row
        }
        CalleeRecord::Unresolved { edge, target_label } => {
            let entity_name = target_label.clone();
            let mut row = json!({
                "resolved": false,
                "entity_name": entity_name,
                "target_label": target_label,
            });
            add_edge_metadata(&mut row, edge, false, None);
            row
        }
    }
}

fn add_edge_metadata(
    row: &mut Value,
    edge: &GraphEdgeArtifact,
    resolved: bool,
    unresolved_target_label: Option<String>,
) {
    let Some(map) = row.as_object_mut() else {
        return;
    };
    map.insert("resolved".to_string(), Value::Bool(resolved));
    let kind = edge_kind(edge);
    map.insert(
        "edge_kind".to_string(),
        Value::String(edge_kind_str(kind).to_string()),
    );
    if let Some(target_label) = unresolved_target_label {
        map.insert("target_label".to_string(), Value::String(target_label));
    }
    if kind == GraphEdgeKind::CallsDyn {
        map.insert("confidence".to_string(), json!(edge.confidence));
    }
}

fn edge_row(edge: &GraphEdgeArtifact) -> Value {
    json!({
        "source_uri": symbol_uri(&edge.source_stable_symbol_id),
        "target_uri": edge.target_stable_symbol_id.as_ref().map(|id| symbol_uri(id)),
        "target_label": edge.target_label,
        "resolved": edge.target_stable_symbol_id.is_some(),
        "relation": edge.relation,
        "edge_kind": edge_kind_str(edge_kind(edge)),
        "confidence": edge.confidence,
        "confidence_score": edge.confidence_score,
    })
}

fn edge_kind_str(edge_kind: GraphEdgeKind) -> &'static str {
    match edge_kind {
        GraphEdgeKind::Calls => "calls",
        GraphEdgeKind::CallsDyn => "calls_dyn",
        GraphEdgeKind::ReferencesHof => "references_hof",
        GraphEdgeKind::ReferencesOther => "references_other",
    }
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
                    symbol("unresolved-caller", "src/caller.rs", [6, 8], "call_root_unresolved", "call_root_unresolved"),
                    symbol("root", "src/root.rs", [10, 12], "root", "root"),
                    symbol("callee", "src/callee.rs", [20, 22], "callee", "callee"),
                    symbol("dyn-callee", "src/callee.rs", [23, 25], "dyn_callee", "dyn_callee"),
                    symbol("hof-callee", "src/callee.rs", [26, 28], "hof_callee", "hof_callee"),
                    symbol("cache-caller", "crates/foo", [24, 26], "call_cache", "call_cache"),
                    symbol("cache-run", "crates/foo", [30, 32], "run", "Cache::run"),
                    symbol("cache-callee", "crates/foo", [34, 36], "finish_cache", "finish_cache"),
                    symbol("mixed-root", "src/root.rs", [50, 52], "mixed_root", "mixed_root"),
                    symbol("mixed-callee", "src/callee.rs", [60, 62], "mixed_callee", "mixed_callee"),
                    symbol("other-run", "crates/other", [40, 42], "run", "Other::run"),
                    symbol("search-submit", "src/search.rs", [70, 72], "submit", "submit"),
                    symbol("search-submit-plan", "src/search.rs", [80, 82], "submit_plan", "submit_plan"),
                    symbol_kind("search-submit-tool", "src/search.rs", [84, 84], "submit_plan", "submit_plan", "mcp_tool")
                ],
                "edges": [
                    edge("caller", "root"),
                    unresolved_edge("unresolved-caller", "root"),
                    edge("root", "callee"),
                    edge_with_kind("root", "dyn-callee", "calls", "calls_dyn"),
                    edge_with_kind("root", "hof-callee", "references", "references_hof"),
                    edge("cache-caller", "cache-run"),
                    edge("cache-run", "cache-callee"),
                    edge("mixed-root", "mixed-callee"),
                    unresolved_edge("mixed-root", "into")
                ],
                "tombstones": []
            }))
            .expect("encode artifact"),
        )
        .expect("write artifact");
    }

    fn write_wide_subgraph_artifact(dir: &TempDir, child_count: usize, edge_count: usize) {
        let child_ids = (0..child_count)
            .map(|index| format!("wide-child-{index:03}"))
            .collect::<Vec<_>>();
        let mut symbols = vec![symbol(
            "wide-root",
            "src/wide.rs",
            [1, 10],
            "wide_root",
            "wide_root",
        )];
        symbols.extend(child_ids.iter().enumerate().map(|(index, id)| {
            symbol(
                id,
                "src/wide.rs",
                [20 + index, 20 + index],
                &format!("wide_child_{index:03}"),
                &format!("wide_child_{index:03}"),
            )
        }));
        let edges = (0..edge_count)
            .map(|index| edge("wide-root", &child_ids[index % child_ids.len()]))
            .collect::<Vec<_>>();

        std::fs::create_dir_all(dir.path().join(".spur")).expect("create .spur");
        std::fs::write(
            dir.path().join(".spur/graph-index.json"),
            serde_json::to_string_pretty(&json!({
                "header": {
                    "graph_index_version": "test"
                },
                "manifest_version": "test",
                "graph_content_hash": "wide-test",
                "files": [
                    { "stable_file_id": "file-src-wide", "file_path": "src/wide.rs" }
                ],
                "symbols": symbols,
                "edges": edges,
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

    fn symbol_kind(
        id: &str,
        file_path: &str,
        line_range: [usize; 2],
        entity_name: &str,
        qualified_name: &str,
        symbol_kind: &str,
    ) -> Value {
        let mut symbol = symbol(id, file_path, line_range, entity_name, qualified_name);
        symbol["symbol_kind"] = Value::String(symbol_kind.to_string());
        symbol
    }

    fn edge(source: &str, target: &str) -> Value {
        edge_with_kind(source, target, "calls", "calls")
    }

    fn edge_with_kind(source: &str, target: &str, relation: &str, edge_kind: &str) -> Value {
        json!({
            "source_stable_symbol_id": source,
            "target_stable_symbol_id": target,
            "target_label": null,
            "relation": relation,
            "confidence": "syntax_exact",
            "confidence_score": 1.0,
            "edge_kind": edge_kind
        })
    }

    fn unresolved_edge(source: &str, target_label: &str) -> Value {
        json!({
            "source_stable_symbol_id": source,
            "target_stable_symbol_id": null,
            "target_label": target_label,
            "relation": "calls",
            "confidence": "syntax_exact",
            "confidence_score": 1.0,
            "edge_kind": "calls"
        })
    }

    fn response_json(response: JsonRpcResponse) -> Value {
        let text = response.result.expect("success result")["content"][0]["text"]
            .as_str()
            .expect("content text")
            .to_string();
        serde_json::from_str(&text).expect("JSON content")
    }

    fn assert_unavailable_freshness_metadata(body: &Value) {
        assert_eq!(body.get("graph_built_at"), Some(&Value::Null));
        assert_eq!(body.get("indexed_head_oid"), Some(&Value::Null));
        assert_eq!(body.get("worktree_head_oid"), Some(&Value::Null));
        assert_eq!(body.get("worktree_dirty"), Some(&Value::Null));
    }

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap_or_else(|error| panic!("run git {}: {error}", args.join(" ")));
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("git stdout utf8")
            .trim()
            .to_string()
    }

    fn init_clean_git_fixture(dir: &std::path::Path) -> String {
        git(dir, &["init", "-q"]);
        git(dir, &["config", "user.email", "test@spur"]);
        git(dir, &["config", "user.name", "Spur Test"]);
        std::fs::create_dir_all(dir.join("src")).expect("create src");
        std::fs::write(dir.join("src/root.rs"), "fn root() {}\n").expect("write source");
        std::fs::write(dir.join(".git/info/exclude"), ".spur/\n").expect("ignore graph sidecar");
        git(dir, &["add", "src/root.rs"]);
        git(dir, &["commit", "-q", "-m", "initial"]);
        git(dir, &["rev-parse", "HEAD"])
    }

    fn write_fixture_pointer(dir: &TempDir, indexed_head_oid: &str) {
        let pointer_path = dir.path().join(".spur/graph-index.pointer.json");
        std::fs::create_dir_all(pointer_path.parent().expect("pointer parent"))
            .expect("create pointer parent");
        std::fs::write(
            pointer_path,
            serde_json::to_string_pretty(&json!({
                "schema": "spur-graph-pointer-v1",
                "graph_content_hash": "test",
                "manifest_version": "test",
                "source_kind": "git",
                "indexed_commit_oid": indexed_head_oid,
                "canonical_artifact_path": dir.path().join(".spur/graph-index.json")
            }))
            .expect("encode pointer"),
        )
        .expect("write pointer");
    }

    #[test]
    fn validate_file_path_arg_requires_slash_normalized_relative_paths() {
        assert_eq!(
            super::validate_file_path_arg("src/lib.rs").expect("valid relative file path"),
            "src/lib.rs"
        );

        for file in ["./src/lib.rs", "src/./lib.rs", "../src/lib.rs", "/abs/path"] {
            let error = match super::validate_file_path_arg(file) {
                Ok(_) => panic!("`{file}` must be rejected"),
                Err(error) => error,
            };
            assert_eq!(error.json_rpc_code(), -32602);
        }
    }

    #[tokio::test]
    async fn file_and_file_glob_mutually_exclusive_in_handler() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_search(
                Value::from(1),
                json!({
                    "query": "submit",
                    "file": "src/search.rs",
                    "file_glob": "src/*.rs"
                }),
            )
            .await;

        let error = response.error.expect("mutually exclusive error");
        assert_eq!(error.code, -32602);
        assert!(error
            .message
            .contains("fields 'file' and 'file_glob' are mutually exclusive"));
        assert_eq!(
            error.data.as_ref().expect("graph metadata")["graph_content_hash"],
            "test"
        );
        assert_unavailable_freshness_metadata(error.data.as_ref().expect("graph metadata"));
    }

    #[tokio::test]
    async fn empty_query_rejected_by_handler() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_search(Value::from(1), json!({ "query": " \n\t " }))
            .await;

        let error = response.error.expect("empty query error");
        assert_eq!(error.code, -32602);
        assert!(error.message.contains("field 'query' must not be empty"));
        assert_unavailable_freshness_metadata(error.data.as_ref().expect("graph metadata"));
    }

    #[tokio::test]
    async fn absolute_or_dotdot_file_rejected_by_handler() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        for args in [
            json!({ "query": "submit", "file": "/abs/path.rs" }),
            json!({ "query": "submit", "file": "../src/search.rs" }),
            json!({ "query": "submit", "file_glob": "/abs/*.rs" }),
            json!({ "query": "submit", "file_glob": "../src/*.rs" }),
        ] {
            let response = server.handle_code_search(Value::from(1), args).await;
            let error = response.error.expect("invalid path error");
            assert_eq!(error.code, -32602);
            assert_unavailable_freshness_metadata(error.data.as_ref().expect("graph metadata"));
        }
    }

    #[tokio::test]
    async fn code_search_returns_ranked_candidates_with_freshness_metadata() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_search(
                Value::from(1),
                json!({
                    "query": "sub",
                    "mode": "prefix",
                    "symbol_kind": "function",
                    "file": "src/search.rs",
                    "limit": 10
                }),
            )
            .await;
        let body = response_json(response);
        let candidates = body["candidates"].as_array().expect("candidates");

        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0]["selector"], "src/search.rs::submit");
        assert_eq!(candidates[0]["uri"], "graph://symbol/search-submit");
        assert_eq!(candidates[0]["id"], "search-submit");
        assert_eq!(candidates[0]["entity_name"], "submit");
        assert_eq!(candidates[0]["qualified_name"], "submit");
        assert_eq!(candidates[0]["file_path"], "src/search.rs");
        assert_eq!(candidates[0]["line_range"], json!([70, 72]));
        assert_eq!(candidates[0]["symbol_kind"], "function");
        assert_eq!(candidates[1]["entity_name"], "submit_plan");
        assert_eq!(body["graph_content_hash"], "test");
        assert_eq!(body["graph_index_version"], "test");
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_search_echoes_inputs_and_total_matches() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_search(
                Value::from(1),
                json!({
                    "query": "submit",
                    "mode": "substring",
                    "symbol_kind": "function",
                    "file_glob": "src/*.rs",
                    "limit": 1
                }),
            )
            .await;
        let body = response_json(response);

        assert_eq!(body["query"], "submit");
        assert_eq!(body["mode"], "substring");
        assert_eq!(body["symbol_kind"], "function");
        assert_eq!(body["file"], Value::Null);
        assert_eq!(body["file_glob"], "src/*.rs");
        assert_eq!(body["limit"], 1);
        assert_eq!(body["total_matches"], 2);
        assert_eq!(body["truncated"], true);
        assert_eq!(body["candidates"].as_array().expect("candidates").len(), 1);
        assert_unavailable_freshness_metadata(&body);
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

        let callers = body["callers"].as_array().expect("callers");
        assert_eq!(callers.len(), 1);
        assert_eq!(callers[0]["uri"], "graph://symbol/caller");
        assert_eq!(body["callers"][0]["resolved"], true);
        assert_eq!(body["callers"][0]["edge_kind"], "calls");
        assert_eq!(body["callers"][0]["entity_name"], "call_root");
        assert_eq!(body["callers"][0]["file_path"], "src/caller.rs");
        assert_eq!(body["callers"][0]["line_range"], json!([3, 5]));
        assert_eq!(body["callers"][0]["symbol_kind"], "function");
        assert_eq!(body["include_unresolved"], false);
        assert_eq!(
            body["counts_by_kind"],
            json!({
                "calls": 2,
                "calls_dyn": 0,
                "references_hof": 0,
                "references_other": 0,
                "unresolved": 1
            })
        );
        assert_eq!(body["unresolved_sample"], json!(["root"]));
        assert_eq!(body["graph_content_hash"], "test");
        assert_eq!(body["graph_index_version"], "test");
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_callers_can_include_unresolved_rows_when_requested() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callers(
                Value::from(1),
                json!({
                    "symbol": "graph://symbol/root",
                    "include_unresolved": true
                }),
            )
            .await;
        let body = response_json(response);
        let callers = body["callers"].as_array().expect("callers");

        assert_eq!(callers.len(), 2);
        assert_eq!(callers[0]["resolved"], true);
        assert_eq!(callers[1]["resolved"], false);
        assert_eq!(callers[1]["uri"], "graph://symbol/unresolved-caller");
        assert_eq!(callers[1]["target_label"], "root");
        assert_eq!(body["include_unresolved"], true);
        assert_eq!(body["unresolved_sample"], json!(["root"]));
    }

    #[tokio::test]
    async fn code_callers_counts_legacy_references_edges_as_references_other() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
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
                    { "stable_file_id": "file-src-lib", "file_path": "src/lib.rs" }
                ],
                "symbols": [
                    symbol("caller", "src/lib.rs", [1, 1], "caller", "caller"),
                    symbol("root", "src/lib.rs", [3, 3], "root", "root")
                ],
                "edges": [
                    {
                        "source_stable_symbol_id": "caller",
                        "target_stable_symbol_id": "root",
                        "target_label": "root",
                        "relation": "references",
                        "confidence": "syntax_exact",
                        "confidence_score": 1.0
                    }
                ],
                "tombstones": []
            }))
            .expect("encode artifact"),
        )
        .expect("write artifact");
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let body = response_json(
            server
                .handle_code_callers(Value::from(1), json!({ "symbol": "root" }))
                .await,
        );

        assert_eq!(body["callers"][0]["edge_kind"], "references_other");
        assert_eq!(
            body["counts_by_kind"],
            json!({
                "calls": 0,
                "calls_dyn": 0,
                "references_hof": 0,
                "references_other": 1,
                "unresolved": 0
            })
        );
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

        assert_eq!(body["callees"].as_array().expect("callees").len(), 3);
        assert_eq!(body["callees"][0]["resolved"], true);
        assert_eq!(body["callees"][0]["edge_kind"], "calls");
        assert_eq!(body["callees"][0]["uri"], "graph://symbol/callee");
        assert_eq!(body["callees"][0]["entity_name"], "callee");
        assert_eq!(body["include_unresolved"], false);
        assert_eq!(
            body["counts_by_kind"],
            json!({
                "calls": 1,
                "calls_dyn": 1,
                "references_hof": 1,
                "references_other": 0,
                "unresolved": 0
            })
        );
        assert_eq!(body["unresolved_sample"], json!([]));
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_callees_filters_unresolved_by_default_and_reports_counts() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callees(
                Value::from(1),
                json!({ "symbol": "graph://symbol/mixed-root" }),
            )
            .await;
        let body = response_json(response);
        let callees = body["callees"].as_array().expect("callees");

        assert_eq!(callees.len(), 1);
        assert_eq!(callees[0]["resolved"], true);
        assert_eq!(callees[0]["edge_kind"], "calls");
        assert_eq!(callees[0]["uri"], "graph://symbol/mixed-callee");
        assert_eq!(callees[0]["entity_name"], "mixed_callee");
        assert_eq!(callees[0]["file_path"], "src/callee.rs");
        assert_eq!(callees[0]["line_range"], json!([60, 62]));
        assert_eq!(callees[0]["symbol_kind"], "function");
        assert_eq!(body["include_unresolved"], false);
        assert_eq!(
            body["counts_by_kind"],
            json!({
                "calls": 2,
                "calls_dyn": 0,
                "references_hof": 0,
                "references_other": 0,
                "unresolved": 1
            })
        );
        assert_eq!(body["unresolved_sample"], json!(["into"]));

        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_callees_can_include_unresolved_rows_when_requested() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_callees(
                Value::from(1),
                json!({
                    "symbol": "graph://symbol/mixed-root",
                    "include_unresolved": true
                }),
            )
            .await;
        let body = response_json(response);
        let callees = body["callees"].as_array().expect("callees");

        assert_eq!(callees.len(), 2);
        assert_eq!(callees[1]["resolved"], false);
        assert_eq!(callees[1]["edge_kind"], "calls");
        assert_eq!(callees[1]["entity_name"], "into");
        assert_eq!(callees[1]["target_label"], "into");
        assert!(callees[1].get("uri").is_none());
        assert!(callees[1].get("file_path").is_none());
        assert_eq!(body["include_unresolved"], true);
        assert_eq!(body["unresolved_sample"], json!(["into"]));
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
        assert_unavailable_freshness_metadata(&callers);

        let callees = response_json(
            server
                .handle_code_callees(
                    Value::from(1),
                    json!({ "selector": "crates/foo::Cache::run" }),
                )
                .await,
        );
        assert_eq!(callees["callees"].as_array().expect("callees").len(), 1);
        assert_eq!(callees["callees"][0]["resolved"], true);
        assert_eq!(callees["callees"][0]["edge_kind"], "calls");
        assert_eq!(callees["callees"][0]["uri"], "graph://symbol/cache-callee");
        assert_eq!(callees["graph_content_hash"], "test");
        assert_eq!(callees["graph_index_version"], "test");
        assert_unavailable_freshness_metadata(&callees);
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
        assert_unavailable_freshness_metadata(&body);
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
        assert_unavailable_freshness_metadata(&body);
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
        assert_unavailable_freshness_metadata(error.data.as_ref().expect("graph metadata"));
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
        let edges = body["edges"].as_array().expect("edges");
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|edge| edge["edge_kind"] == "calls"));
        assert_eq!(body["include_unresolved"], false);
        assert_eq!(body["metadata"]["radius"], 3);
        assert_eq!(
            body["metadata"]["warning"],
            "radius 9 exceeds max 3; clamped to 3"
        );
        assert_eq!(body["metadata"]["max_nodes"], 40);
        assert_eq!(body["metadata"]["max_edges"], 120);
        assert_eq!(body["metadata"]["truncated"], false);
        assert_eq!(body["truncated_frontier"], json!([]));
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn code_subgraph_enforces_default_node_budget() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_wide_subgraph_artifact(&dir, 45, 45);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(
                Value::from(1),
                json!({ "symbol": "graph://symbol/wide-root", "radius": 1 }),
            )
            .await;
        let body = response_json(response);

        assert_eq!(body["metadata"]["max_nodes"], 40);
        assert_eq!(body["metadata"]["max_edges"], 120);
        assert_eq!(body["metadata"]["truncated"], true);
        assert_eq!(body["nodes"].as_array().expect("nodes").len(), 40);
        assert_eq!(body["edges"].as_array().expect("edges").len(), 39);
        assert_eq!(
            body["truncated_frontier"],
            json!([
                "wide-child-039",
                "wide-child-040",
                "wide-child-041",
                "wide-child-042",
                "wide-child-043",
                "wide-child-044"
            ])
        );
    }

    #[tokio::test]
    async fn code_subgraph_enforces_default_edge_budget() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_wide_subgraph_artifact(&dir, 130, 130);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(
                Value::from(1),
                json!({
                    "symbol": "graph://symbol/wide-root",
                    "radius": 1,
                    "max_nodes": 400
                }),
            )
            .await;
        let body = response_json(response);

        assert_eq!(body["metadata"]["max_nodes"], 400);
        assert_eq!(body["metadata"]["max_edges"], 120);
        assert_eq!(body["metadata"]["truncated"], true);
        assert_eq!(body["nodes"].as_array().expect("nodes").len(), 121);
        assert_eq!(body["edges"].as_array().expect("edges").len(), 120);
        assert_eq!(
            body["truncated_frontier"],
            json!([
                "wide-child-120",
                "wide-child-121",
                "wide-child-122",
                "wide-child-123",
                "wide-child-124",
                "wide-child-125",
                "wide-child-126",
                "wide-child-127",
                "wide-child-128",
                "wide-child-129"
            ])
        );
    }

    #[tokio::test]
    async fn code_subgraph_clamps_and_echoes_requested_budgets() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(
                Value::from(1),
                json!({
                    "symbol": "root",
                    "radius": 1,
                    "max_nodes": 999,
                    "max_edges": 9999
                }),
            )
            .await;
        let body = response_json(response);

        assert_eq!(body["metadata"]["max_nodes"], 400);
        assert_eq!(body["metadata"]["max_edges"], 1200);
        assert_eq!(body["metadata"]["requested_max_nodes"], 999);
        assert_eq!(body["metadata"]["requested_max_edges"], 9999);
        assert_eq!(body["metadata"]["truncated"], false);
        assert_eq!(body["truncated_frontier"], json!([]));
    }

    #[tokio::test]
    async fn code_subgraph_returns_empty_frontier_when_untruncated() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let response = server
            .handle_code_subgraph(Value::from(1), json!({ "symbol": "root", "radius": 1 }))
            .await;
        let body = response_json(response);

        assert_eq!(body["metadata"]["truncated"], false);
        assert_eq!(body["truncated_frontier"], json!([]));
    }

    #[tokio::test]
    async fn code_subgraph_filters_unresolved_by_default_and_can_include_them() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let default_body = response_json(
            server
                .handle_code_subgraph(
                    Value::from(1),
                    json!({
                        "symbol": "graph://symbol/mixed-root",
                        "radius": 1,
                        "edge_kinds": ["calls"]
                    }),
                )
                .await,
        );
        let default_edges = default_body["edges"].as_array().expect("default edges");
        assert_eq!(default_edges.len(), 1);
        assert!(default_edges
            .iter()
            .all(|edge| edge["target_uri"].as_str().is_some()));
        assert_eq!(default_body["include_unresolved"], false);

        let included_body = response_json(
            server
                .handle_code_subgraph(
                    Value::from(1),
                    json!({
                        "symbol": "graph://symbol/mixed-root",
                        "radius": 1,
                        "edge_kinds": ["calls"],
                        "include_unresolved": true
                    }),
                )
                .await,
        );
        let included_edges = included_body["edges"].as_array().expect("included edges");
        assert_eq!(included_edges.len(), 2);
        assert!(included_edges
            .iter()
            .any(|edge| edge["target_label"] == "into" && edge["target_uri"].is_null()));
        assert_eq!(included_body["include_unresolved"], true);
    }

    #[tokio::test]
    async fn code_subgraph_can_include_incoming_unresolved_caller_edges() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let body = response_json(
            server
                .handle_code_subgraph(
                    Value::from(1),
                    json!({
                        "symbol": "graph://symbol/root",
                        "radius": 1,
                        "edge_kinds": ["calls"],
                        "include_unresolved": true
                    }),
                )
                .await,
        );
        let edges = body["edges"].as_array().expect("edges");

        assert!(edges.iter().any(|edge| {
            edge["source_uri"] == "graph://symbol/unresolved-caller"
                && edge["target_uri"].is_null()
                && edge["target_label"] == "root"
                && edge["resolved"] == false
        }));
    }

    #[tokio::test]
    async fn code_subgraph_edge_kinds_accept_public_edge_kind_values() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        write_fixture_artifact(&dir);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let dyn_body = response_json(
            server
                .handle_code_subgraph(
                    Value::from(1),
                    json!({ "symbol": "root", "radius": 1, "edge_kinds": ["calls_dyn"] }),
                )
                .await,
        );
        let dyn_edges = dyn_body["edges"].as_array().expect("dyn edges");
        assert_eq!(dyn_edges.len(), 1);
        assert_eq!(dyn_edges[0]["edge_kind"], "calls_dyn");

        let hof_body = response_json(
            server
                .handle_code_subgraph(
                    Value::from(1),
                    json!({ "symbol": "root", "radius": 1, "edge_kinds": ["references_hof"] }),
                )
                .await,
        );
        let hof_edges = hof_body["edges"].as_array().expect("hof edges");
        assert_eq!(hof_edges.len(), 1);
        assert_eq!(hof_edges[0]["edge_kind"], "references_hof");
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
        assert_unavailable_freshness_metadata(&body);
    }

    #[tokio::test]
    async fn graph_metadata_reports_pointer_head_and_dirty_state() {
        let _lock = CWD_LOCK.lock().expect("cwd lock");
        let dir = TempDir::new().expect("tempdir");
        let indexed_head_oid = init_clean_git_fixture(dir.path());
        write_fixture_artifact(&dir);
        write_fixture_pointer(&dir, &indexed_head_oid);
        let _cwd = enter_dir(dir.path());
        let server = test_server();

        let clean = response_json(
            server
                .handle_code_callers(Value::from(1), json!({ "symbol": "graph://symbol/root" }))
                .await,
        );

        assert!(
            chrono::DateTime::parse_from_rfc3339(
                clean["graph_built_at"].as_str().expect("graph_built_at")
            )
            .is_ok(),
            "graph_built_at must be RFC3339"
        );
        assert_eq!(clean["indexed_head_oid"], indexed_head_oid);
        assert_eq!(clean["worktree_head_oid"], indexed_head_oid);
        assert_eq!(clean["worktree_dirty"], false);

        std::fs::write(
            dir.path().join("src/root.rs"),
            "fn root() { let _dirty = true; }\n",
        )
        .expect("dirty source");

        let dirty = response_json(
            server
                .handle_code_callers(Value::from(1), json!({ "symbol": "graph://symbol/root" }))
                .await,
        );

        assert_eq!(dirty["indexed_head_oid"], indexed_head_oid);
        assert_eq!(dirty["worktree_head_oid"], indexed_head_oid);
        assert_eq!(dirty["worktree_dirty"], true);
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

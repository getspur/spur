use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Value};

use crate::handlers::McpHandlerError;
use crate::server::types::JsonRpcResponse;

use super::McpCallbackServer;

pub(crate) use spur_graph::mcp::{
    scoped_worktree_root, shared_rebuild_coordinator, with_worktree_root_for_request,
};

#[cfg(any(test, feature = "test-support"))]
pub(crate) use spur_graph::mcp::{
    set_graph_rebuild_delay_for_test, set_graph_rebuild_latency_budget_for_test,
};

impl McpCallbackServer {
    pub(crate) async fn handle_code_resolve(&self, id: Value, args: Value) -> JsonRpcResponse {
        self.handle_graph_tool(id, "code_resolve", args).await
    }

    pub(crate) async fn handle_code_search(&self, id: Value, args: Value) -> JsonRpcResponse {
        self.handle_graph_tool(id, "code_symbol_search", args).await
    }

    pub(crate) async fn handle_code_file_symbols(&self, id: Value, args: Value) -> JsonRpcResponse {
        self.handle_graph_tool(id, "code_file_symbols", args).await
    }

    pub(crate) async fn handle_code_symbol_info(&self, id: Value, args: Value) -> JsonRpcResponse {
        self.handle_graph_tool(id, "code_symbol_info", args).await
    }

    pub(crate) async fn handle_code_read_symbol(&self, id: Value, args: Value) -> JsonRpcResponse {
        self.handle_graph_tool(id, "code_read_symbol", args).await
    }

    pub(crate) async fn handle_code_callers(&self, id: Value, args: Value) -> JsonRpcResponse {
        self.handle_graph_tool(id, "code_callers", args).await
    }

    pub(crate) async fn handle_code_callees(&self, id: Value, args: Value) -> JsonRpcResponse {
        self.handle_graph_tool(id, "code_callees", args).await
    }

    pub(crate) async fn handle_code_subgraph(&self, id: Value, args: Value) -> JsonRpcResponse {
        self.handle_graph_tool(id, "code_subgraph", args).await
    }

    pub(crate) async fn handle_code_symbol_history(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        self.handle_graph_tool(id, "code_symbol_history", args)
            .await
    }

    async fn handle_graph_tool(&self, id: Value, name: &str, args: Value) -> JsonRpcResponse {
        let module = spur_graph::mcp::GraphMcpModule::new(self.graph_mcp_deps.clone());
        code_graph_response(id, module.call(name, args).await).await
    }
}

pub(crate) async fn code_resolve(args: &Value) -> Result<Value, McpHandlerError> {
    spur_graph::mcp::code_resolve(args)
        .await
        .map_err(graph_handler_error)
}

pub(crate) async fn code_search(args: &Value) -> Result<Value, McpHandlerError> {
    spur_graph::mcp::code_search(args)
        .await
        .map_err(graph_handler_error)
}

pub(crate) async fn code_file_symbols(args: &Value) -> Result<Value, McpHandlerError> {
    spur_graph::mcp::code_file_symbols(args)
        .await
        .map_err(graph_handler_error)
}

pub(crate) async fn code_symbol_info(args: &Value) -> Result<Value, McpHandlerError> {
    spur_graph::mcp::code_symbol_info(args)
        .await
        .map_err(graph_handler_error)
}

pub(crate) async fn code_read_symbol(args: &Value) -> Result<Value, McpHandlerError> {
    spur_graph::mcp::code_read_symbol(args)
        .await
        .map_err(graph_handler_error)
}

pub(crate) async fn code_callers(args: &Value) -> Result<Value, McpHandlerError> {
    spur_graph::mcp::code_callers(args)
        .await
        .map_err(graph_handler_error)
}

pub(crate) async fn code_callees(args: &Value) -> Result<Value, McpHandlerError> {
    spur_graph::mcp::code_callees(args)
        .await
        .map_err(graph_handler_error)
}

pub(crate) async fn code_subgraph(args: &Value) -> Result<Value, McpHandlerError> {
    spur_graph::mcp::code_subgraph(args)
        .await
        .map_err(graph_handler_error)
}

pub(crate) async fn code_symbol_history(args: &Value) -> Result<Value, McpHandlerError> {
    spur_graph::mcp::code_symbol_history(args)
        .await
        .map_err(graph_handler_error)
}

pub(crate) async fn overlaid_graph_artifact_from_base_seed_for_worktree(
    worktree: PathBuf,
    rebuild_coordinator: Arc<spur_graph::mcp::RebuildCoordinator>,
) -> Result<Arc<spur_graph::GraphIndexArtifact>, McpHandlerError> {
    spur_graph::mcp::overlaid_graph_artifact_from_base_seed_for_worktree(
        worktree,
        rebuild_coordinator,
    )
    .await
    .map_err(graph_handler_error)
}

async fn code_graph_response(
    id: Value,
    result: spur_graph::mcp::CodeGraphResult,
) -> JsonRpcResponse {
    match result {
        Ok(body) => json_success(id, body),
        Err(error) => {
            let response = error.into_error_response().await;
            match response.data {
                Some(data) => {
                    JsonRpcResponse::error_with_data(id, response.code, response.message, data)
                }
                None => JsonRpcResponse::error(id, response.code, response.message),
            }
        }
    }
}

fn graph_handler_error(error: spur_graph::mcp::McpHandlerError) -> McpHandlerError {
    match error {
        spur_graph::mcp::McpHandlerError::InvalidParams(message) => {
            McpHandlerError::InvalidParams(message)
        }
        spur_graph::mcp::McpHandlerError::NotFound(message) => McpHandlerError::NotFound(message),
        spur_graph::mcp::McpHandlerError::Unauthorized(message) => {
            McpHandlerError::Unauthorized(message)
        }
        spur_graph::mcp::McpHandlerError::UpstreamPm(message) => {
            McpHandlerError::UpstreamPm(message)
        }
        spur_graph::mcp::McpHandlerError::Internal(message) => McpHandlerError::Internal(message),
    }
}

fn json_success(id: Value, body: Value) -> JsonRpcResponse {
    let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
    JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
}

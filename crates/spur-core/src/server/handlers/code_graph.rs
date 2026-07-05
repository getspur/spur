use serde_json::{json, Value};

use crate::server::types::JsonRpcResponse;

use super::McpCallbackServer;

pub(crate) use spur_graph::mcp::with_worktree_root_for_request;

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
        code_graph_response(id, module.dispatch(name, args).await).await
    }
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

fn json_success(id: Value, body: Value) -> JsonRpcResponse {
    let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
    JsonRpcResponse::success(id, json!({ "content": [{ "type": "text", "text": text }] }))
}

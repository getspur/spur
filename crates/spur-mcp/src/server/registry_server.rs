//! Generic rmcp `ServerHandler` backed by a [`ToolRegistry`].
//!
//! This is the composition primitive for standalone MCP servers: register any
//! set of [`crate::ToolModule`]s into a [`crate::ToolRegistry`], wrap it in a
//! [`RegistryServerHandler`], and serve it over stdio (via
//! [`super::serve_stdio_server`]) or streamable HTTP. The handler delegates
//! `tools/list` and `tools/call` to the registry, so domain crates only need to
//! `impl ToolModule` — no per-server dispatch glue.
//!
//! Authority posture: standalone servers are read-only query surfaces. The
//! handler advertises worker-level read authority, which matches the read-only
//! graph/analyst modules. Brain-only tools (delegation, plan, review) are simply
//! not registered into a standalone registry.

use rmcp::{
    model::{
        object as rmcp_object, CallToolRequestParams, CallToolResult, Implementation,
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    ErrorData as McpError, RoleServer, ServerHandler,
};
use serde_json::Value;

use crate::registry::{ServerKind, ToolAuthority, ToolCallContext, ToolRegistry};
use crate::response::JsonRpcResponse;
use crate::ToolDefinition;

/// A read-only rmcp `ServerHandler` that dispatches `tools/list` and
/// `tools/call` to a [`ToolRegistry`].
///
/// Construct with [`RegistryServerHandler::new`], then serve via
/// [`crate::server::serve_stdio_server`].
pub struct RegistryServerHandler {
    registry: std::sync::Arc<ToolRegistry>,
    server_kind: ServerKind,
    authority: ToolAuthority,
    name: String,
    instructions: String,
}

impl RegistryServerHandler {
    /// Wrap a registry for serving.
    ///
    /// `name` and `instructions` populate the MCP server info advertised to
    /// clients in the `initialize` response.
    pub fn new(
        registry: ToolRegistry,
        name: impl Into<String>,
        instructions: impl Into<String>,
    ) -> Self {
        Self {
            registry: std::sync::Arc::new(registry),
            server_kind: ServerKind::Worker,
            authority: ToolAuthority::Worker,
            name: name.into(),
            instructions: instructions.into(),
        }
    }

    /// Returns the registry name advertised to clients.
    pub fn name(&self) -> &str {
        &self.name
    }
}

fn registry_tool_to_rmcp(definition: ToolDefinition) -> Tool {
    Tool::new(
        definition.name,
        definition.description,
        rmcp_object(definition.input_schema),
    )
}

/// Convert a registry-produced JSON-RPC envelope into an rmcp `CallToolResult`.
///
/// A successful envelope's `result` must deserialize into a `CallToolResult`
/// (i.e. the `{ "content": [...] }` shape that modules produce via
/// [`crate::ToolResponse::json_text`]). Error envelopes become MCP errors.
fn json_rpc_to_call_tool_result(
    response: JsonRpcResponse,
    tool_name: &str,
) -> Result<CallToolResult, McpError> {
    match (response.result, response.error) {
        (Some(result), None) => serde_json::from_value(result).map_err(|error| {
            McpError::internal_error(
                format!("failed to serialize tool result for {tool_name}: {error}"),
                None,
            )
        }),
        (None, Some(error)) => Err(error.into_mcp_error()),
        (Some(_), Some(_)) | (None, None) => Err(McpError::internal_error(
            format!("tool handler returned an invalid response envelope for {tool_name}"),
            None,
        )),
    }
}

impl ServerHandler for RegistryServerHandler {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(self.instructions.clone());
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        let mut implementation = Implementation::default();
        implementation.name = self.name.clone();
        implementation.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = implementation;
        info
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.registry
            .tool_definition_for_call(name)
            .map(registry_tool_to_rmcp)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(
            self.registry
                .list_tools()
                .into_iter()
                .map(registry_tool_to_rmcp)
                .collect(),
        ))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let tool_name = request.name.to_string();
        let arguments = request
            .arguments
            .map(Value::Object)
            .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
        let ctx = ToolCallContext::new(self.server_kind, self.authority, None, None);
        let response = self
            .registry
            .call_json_tool(ctx, &tool_name, arguments)
            .await;
        json_rpc_to_call_tool_result(response, &tool_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{ToolCallContext, ToolModule, ToolRegistry, ToolResponse};
    use crate::{ServerKind, ToolAuthority};
    use async_trait::async_trait;
    use rmcp::model::ErrorCode;
    use serde_json::json;

    /// A toy module that echoes its `value` argument as a text content block,
    /// mirroring how the real graph/analyst modules wrap results via
    /// `ToolResponse::json_text`.
    struct EchoModule {
        tools: Vec<ToolDefinition>,
    }

    #[async_trait]
    impl ToolModule for EchoModule {
        fn tools(&self) -> Vec<ToolDefinition> {
            self.tools.clone()
        }

        async fn call(
            &self,
            ctx: ToolCallContext<'_>,
            name: &str,
            args: Value,
        ) -> Result<ToolResponse, McpError> {
            if name != "echo" {
                return Err(McpError::new(
                    ErrorCode(-32602),
                    format!("unknown tool: {name}"),
                    None,
                ));
            }
            let value = args.get("value").cloned().unwrap_or(json!(null));
            Ok(ToolResponse::json_text(ctx.request_id_value(), value))
        }
    }

    fn echo_registry() -> ToolRegistry {
        let module = EchoModule {
            tools: vec![ToolDefinition {
                name: "echo".into(),
                description: "echo the value argument".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "value": {} }
                }),
            }],
        };
        ToolRegistry::builder()
            .with(module)
            .expect("register echo module")
            .build()
    }

    fn worker_ctx() -> ToolCallContext<'static> {
        ToolCallContext::new(ServerKind::Worker, ToolAuthority::Worker, None, None)
    }

    #[test]
    fn get_info_advertises_name_and_instructions() {
        let handler = RegistryServerHandler::new(echo_registry(), "standalone", "use me");
        let info = handler.get_info();
        assert_eq!(info.server_info.name, "standalone");
        assert_eq!(info.instructions.as_deref(), Some("use me"));
    }

    #[test]
    fn get_tool_returns_registered_definition_by_name() {
        let handler = RegistryServerHandler::new(echo_registry(), "s", "i");
        assert!(handler.get_tool("echo").is_some());
        assert!(handler.get_tool("missing").is_none());
    }

    #[tokio::test]
    async fn call_path_routes_to_module_and_wraps_result_as_text_content() {
        // Mirror exactly what the handler's `call_tool` does, minus the rmcp
        // request-context glue (which needs a live Peer unavailable in unit
        // tests): registry -> call_json_tool -> convert to CallToolResult.
        let handler = RegistryServerHandler::new(echo_registry(), "s", "i");
        let mut args = serde_json::Map::new();
        args.insert("value".into(), json!({"answer": 42}));
        let response = handler
            .registry
            .call_json_tool(worker_ctx(), "echo", Value::Object(args))
            .await;
        let result = json_rpc_to_call_tool_result(response, "echo").expect("call_tool result");

        assert_eq!(result.content.len(), 1);
        let serialized = serde_json::to_string(&result).expect("serialize CallToolResult");
        assert!(
            serialized.contains("answer") && serialized.contains("42"),
            "content should carry the echoed value: {serialized}"
        );
    }

    #[tokio::test]
    async fn call_path_reports_unknown_tool_as_mcp_error() {
        let handler = RegistryServerHandler::new(echo_registry(), "s", "i");
        let response = handler
            .registry
            .call_json_tool(worker_ctx(), "nope", Value::Object(serde_json::Map::new()))
            .await;
        let err =
            json_rpc_to_call_tool_result(response, "nope").expect_err("unknown tool must error");
        assert!(
            err.message.contains("Unknown tool"),
            "expected unknown-tool error, got: {}",
            err.message
        );
    }
}

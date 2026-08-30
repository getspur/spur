//! Subprocess MCP server used by the probe integration tests.

use async_trait::async_trait;
use rmcp::{
    model::{
        Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo,
    },
    service::RequestContext,
    ErrorData as McpError, RoleServer, ServerHandler,
};
use serde_json::{json, Value};
use spur_mcp::{
    serve_stdio_server, RegistryServerHandler, ToolCallContext, ToolDefinition, ToolModule,
    ToolRegistry, ToolResponse,
};

pub const FIXTURE_MARKER: &str = "SPUR_MCP_PROBE_FIXTURE_CHILD";

struct FixtureTools;

#[async_trait]
impl ToolModule for FixtureTools {
    fn tools(&self) -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "echo".into(),
                description: "echo a string".into(),
                input_schema: echo_schema(),
            },
            ToolDefinition {
                name: "add".into(),
                description: "add two numbers".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "a": { "type": "number" },
                        "b": { "type": "number" }
                    },
                    "required": ["a", "b"]
                }),
            },
        ]
    }

    async fn call(
        &self,
        _ctx: ToolCallContext<'_>,
        _name: &str,
        _args: Value,
    ) -> Result<ToolResponse, McpError> {
        panic!("the probe must not invoke fixture tools")
    }
}

pub fn echo_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "value": { "type": "string" } },
        "required": ["value"]
    })
}

struct SleepingToolsServer;

impl ServerHandler for SleepingToolsServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        let mut implementation = Implementation::default();
        implementation.name = "spur-mcp-probe-sleep-fixture".into();
        implementation.version = env!("CARGO_PKG_VERSION").into();
        info.server_info = implementation;
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        std::future::pending().await
    }
}

#[test]
fn probe_fixture_server() {
    let Some(mode) = std::env::var_os(FIXTURE_MARKER) else {
        return;
    };

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build fixture runtime")
        .block_on(async move {
            if mode == "sleep" {
                serve_stdio_server(SleepingToolsServer)
                    .await
                    .expect("serve sleeping probe fixture");
            } else {
                let registry = ToolRegistry::builder()
                    .with(FixtureTools)
                    .expect("register fixture tools")
                    .build();
                let server = RegistryServerHandler::new(
                    registry,
                    "spur-mcp-probe-fixture",
                    "Fixture server for MCP probe tests",
                );
                serve_stdio_server(server)
                    .await
                    .expect("serve probe fixture");
            }
        });
}

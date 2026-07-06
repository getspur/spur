use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// Metadata for a single MCP tool, returned by `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Error)]
pub enum McpHandlerError {
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("upstream PM failure: {0}")]
    UpstreamPm(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl McpHandlerError {
    pub fn json_rpc_code(&self) -> i32 {
        match self {
            Self::InvalidParams(_) => -32602,
            Self::NotFound(_) => -32004,
            Self::Unauthorized(_) => -32001,
            Self::UpstreamPm(_) | Self::Internal(_) => -32603,
        }
    }

    pub fn to_jsonrpc_response(&self, id: Value) -> Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": self.json_rpc_code(),
                "message": self.to_string(),
            }
        })
    }
}

impl From<McpHandlerError> for rmcp::ErrorData {
    fn from(value: McpHandlerError) -> Self {
        rmcp::ErrorData::new(
            rmcp::model::ErrorCode(value.json_rpc_code()),
            value.to_string(),
            None,
        )
    }
}

/// Returns the infrastructure-owned `spur-mcp` brain tool definitions.
///
/// Domain crates compose their own modules into per-server registries.
pub fn tools_list() -> Vec<ToolDefinition> {
    crate::registry::default_tool_registry()
        .expect("default MCP tool registry must be valid")
        .list_tools()
}

/// Returns the infrastructure-owned `spur-mcp` worker tool definitions.
///
/// Domain crates compose worker-readable modules and policy externally.
pub fn worker_tools_list() -> Vec<ToolDefinition> {
    crate::registry::default_worker_tool_registry()
        .expect("default worker MCP tool registry must be valid")
        .list_tools()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{ToolCallContext, ToolModule, ToolRegistry, ToolResponse};
    use async_trait::async_trait;
    use rmcp::model::ErrorData as McpError;
    use serde_json::json;

    #[test]
    fn tools_list_contains_no_domain_tools() {
        let actual: Vec<String> = tools_list().iter().map(|t| t.name.clone()).collect();
        assert!(
            actual.is_empty(),
            "spur-mcp default catalog must be empty: {actual:?}"
        );
    }

    #[test]
    fn worker_tools_list_contains_no_domain_tools() {
        let actual: Vec<String> = worker_tools_list().iter().map(|t| t.name.clone()).collect();
        assert!(
            actual.is_empty(),
            "spur-mcp default worker catalog must be empty: {actual:?}",
        );
    }

    struct StaticToolModule {
        name: &'static str,
    }

    #[async_trait]
    impl ToolModule for StaticToolModule {
        fn tools(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: self.name.to_string(),
                description: "test tool".to_string(),
                input_schema: json!({ "type": "object" }),
            }]
        }

        async fn call(
            &self,
            _ctx: ToolCallContext<'_>,
            _name: &str,
            _args: Value,
        ) -> Result<ToolResponse, McpError> {
            unreachable!("registry duplicate test never invokes tools")
        }
    }

    #[test]
    fn tool_registry_rejects_duplicate_tool_names() {
        let mut registry = ToolRegistry::new();
        registry
            .register(StaticToolModule { name: "duplicate" })
            .expect("first registration succeeds");

        let err = registry
            .register(StaticToolModule { name: "duplicate" })
            .expect_err("duplicate tool names must be rejected");

        assert!(
            err.to_string().contains("duplicate"),
            "unexpected duplicate error: {err}"
        );
    }
}

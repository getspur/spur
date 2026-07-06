#![expect(
    clippy::allow_attributes,
    reason = "legacy MCP infrastructure modules still contain local allow markers"
)]
#![expect(
    clippy::map_err_ignore,
    reason = "legacy MCP handlers intentionally replace low-level errors with JSON-RPC domain messages"
)]
#![expect(
    clippy::str_to_string,
    reason = "legacy MCP code predates the current to_owned style lint"
)]
#![expect(
    clippy::unused_trait_names,
    reason = "legacy MCP modules import extension traits by name for readability"
)]

pub mod events;
pub mod git;
pub mod registry;
pub mod response;
pub mod server;
pub mod token;
pub mod tool_schemas;
pub mod tools;

pub use events::McpEventSink;
pub use registry::{
    ServerKind, ToolAuthority, ToolCallContext, ToolModule, ToolRegistry, ToolRegistryBuilder,
    ToolRegistryError, ToolResponse,
};
pub use response::{JsonRpcError, JsonRpcResponse};
// Re-export the rmcp error primitives so domain crates can `impl ToolModule`
// without taking a direct `rmcp` dependency. `McpError` is the `Err` type of
// `ToolModule::call`; `ErrorCode` lets domain modules map their local error
// kinds onto JSON-RPC codes.
pub use rmcp::model::{ErrorCode, ErrorData as McpError};
pub use server::{serve_stdio_server, RegistryServerHandler};
pub use tools::{tools_list, McpHandlerError, ToolDefinition};

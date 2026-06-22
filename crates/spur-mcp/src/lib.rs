#![expect(
    clippy::allow_attributes,
    reason = "legacy MCP plan and code-graph modules still contain many local allow markers"
)]
#![expect(
    clippy::doc_markdown,
    reason = "legacy MCP docs contain many tool and field identifiers that need a dedicated markdown pass"
)]
#![expect(
    clippy::derive_partial_eq_without_eq,
    reason = "legacy tool schema structs derive PartialEq for tests and can be tightened later"
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
#![expect(
    clippy::use_self,
    reason = "legacy MCP plan/code-graph code often spells type names explicitly for clarity"
)]

pub mod events;
pub mod feature;
pub mod git;
pub mod registry;
pub mod response;
pub mod server;
pub mod token;
pub mod tool_schemas;
pub mod tools;

pub use events::McpEventSink;
pub use registry::{
    AnalystMcpDeps, CoreMcpDeps, GraphMcpDeps, PmMcpDeps, ServerKind, ToolAuthority,
    ToolCallContext, ToolModule, ToolRegistry, ToolRegistryBuilder, ToolRegistryError,
    ToolResponse, WorkerMcpDeps,
};
pub use response::{JsonRpcError, JsonRpcResponse};
pub use tools::{tools_list, DelegationChannel, DelegationRequest, ToolDefinition};

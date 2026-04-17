//! Event sink trait used by the MCP callback server to emit plan-review
//! lifecycle events. A trait is used instead of a direct `FunnelHandle`
//! reference because `spur-core` depends on `spur-mcp` — adding a reverse
//! dependency would create a circular dependency.
//!
//! `spur-core` implements `McpEventSink for FunnelHandle` and injects it at
//! `McpCallbackServer` construction.

use spur_acp::SpurEventBody;

/// Emit plan-review lifecycle events to the process-wide event funnel.
pub trait McpEventSink: Send + Sync {
    fn emit(&self, event: SpurEventBody);
}

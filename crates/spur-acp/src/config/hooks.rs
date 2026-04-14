//! Hook-ID enums for `[agents.entries.commands]`. Strongly typed on purpose
//! — serde rejects unknown variants at parse time, so a typo in
//! `.spur/config.toml` fails loudly with a clear error instead of silently
//! falling back.
//!
//! Each enum names a built-in hook registered in `spur-tui` (see
//! `crates/spur-tui/src/agents/`). The enum lives here because spur-acp
//! owns the config schema and must validate it at load time; hook *behavior*
//! lives in spur-tui where it has AgentConnection / SessionDetailView in
//! scope.

use serde::{Deserialize, Serialize};

/// How a selected slash-command is delivered to the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchKind {
    /// Send `ContentBlock::Text("/name args")` on the agent's prompt stream.
    #[default]
    PromptText,
    /// Call a vendor extension RPC with a typed args payload.
    VendorExec,
}

/// How `/cmd rest-of-line` turns into the RPC args payload for vendor_exec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgsTemplateKind {
    /// `"/cmd rest…"` → `{ "args": { "raw": "rest…" } }`. Today's kiro behavior.
    #[default]
    RawRest,
}

/// How to decode a vendor-ext notification payload into items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestParserKind {
    /// Look up `params[path]`, expect an array, decode each element via `item_schema`.
    JsonPathList,
}

/// Schema describing each element of a decoded ingest list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemSchemaKind {
    /// `serde_json::from_value::<Vec<agent_client_protocol::AvailableCommand>>`.
    AcpAvailableCommand,
}

/// How to render a vendor-ext response in the trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseRenderKind {
    /// Append a system-note trace entry with the raw params.
    SystemNote,
}

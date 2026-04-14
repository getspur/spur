//! Sub-table shapes for `[[agents.entries]]` blocks added in Spec 1.
//!
//!   [agents.entries.commands]       → CommandsConfig
//!   [agents.entries.display]        → DisplayConfig
//!   [agents.entries.permissions]    → PermissionsConfig
//!
//! All are optional via `#[serde(default)]` on AgentConfig; omitting them
//! preserves today's hardcoded behavior (prompt_text dispatch, no vendor ext,
//! no bypass).

use serde::{Deserialize, Serialize};

use super::hooks::{
    ArgsTemplateKind, DispatchKind, IngestParserKind, ItemSchemaKind, ResponseRenderKind,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DisplayConfig {
    /// Short alias used as `/handle:cmd` on collision. Defaults to lowercase
    /// of `AgentConfig::name` when absent (resolved by
    /// `AgentConfig::effective_handle`, not by deserialize).
    #[serde(default)]
    pub handle: Option<String>,
    /// Reserved for future UX. Unused in Spec 1.
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CommandsConfig {
    #[serde(default)]
    pub dispatch: DispatchKind,

    /// Required when `dispatch = "vendor_exec"`. Full wire method, e.g.
    /// `"_kiro.dev/commands/execute"`. Validator rejects absent.
    #[serde(default)]
    pub exec_method: Option<String>,

    /// Required when `dispatch = "vendor_exec"`. How to shape args.
    #[serde(default)]
    pub args_template: ArgsTemplateKind,

    /// One entry per vendor-ext notification that advertises commands.
    #[serde(default)]
    pub ingest: Vec<IngestBinding>,

    /// One entry per vendor-ext method whose response is rendered in the trace.
    #[serde(default)]
    pub response: Vec<ResponseBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestBinding {
    /// Wire method, e.g. `"_kiro.dev/commands/available"`.
    pub method: String,
    pub parser: IngestParserKind,
    /// Dotted JSON path (no array indexing) to the list inside `params`.
    pub path: String,
    pub item_schema: ItemSchemaKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseBinding {
    pub method: String,
    pub render: ResponseRenderKind,
}

/// Permission-bypass levers. Replaces the three flat `skip_permissions*`
/// fields on AgentConfig. Old configs keep working via `AgentConfig::
/// effective_permissions`, which falls back to flat fields when this
/// nested block is absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionsConfig {
    #[serde(default)]
    pub skip: bool,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub session_mode: Option<String>,
}

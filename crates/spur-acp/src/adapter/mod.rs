pub mod arg_picker_hint;
pub mod claude;
pub mod codex;
pub mod config_options;
pub mod gemini;
pub mod generic;
pub mod kimi;
pub mod kiro;
pub mod mcp;

use crate::types::AgentKind;
use agent_client_protocol::schema::{SessionNotification, ToolCall, ToolKind};
use serde_json::Value;

/// Mirrors ACP `ToolKind` 1:1 with TUI-specific refinements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolFamily {
    // 1:1 mirror of ACP ToolKind variants:
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    // TUI-specific refinements produced by per-kind `refine(title, base)`:
    /// e.g. Claude TodoWrite, Codex plan_update
    Plan,
    /// title starts with "mcp__" (MCP tool passthrough)
    Mcp,
    /// maps from ACP `Other` with no per-kind refinement
    Unknown,
}

impl From<ToolKind> for ToolFamily {
    fn from(k: ToolKind) -> Self {
        match k {
            ToolKind::Read => ToolFamily::Read,
            ToolKind::Edit => ToolFamily::Edit,
            ToolKind::Delete => ToolFamily::Delete,
            ToolKind::Move => ToolFamily::Move,
            ToolKind::Search => ToolFamily::Search,
            ToolKind::Execute => ToolFamily::Execute,
            ToolKind::Think => ToolFamily::Think,
            ToolKind::Fetch => ToolFamily::Fetch,
            ToolKind::SwitchMode => ToolFamily::SwitchMode,
            // ToolKind is #[non_exhaustive]; wildcard covers Other + any future variants
            _ => ToolFamily::Unknown,
        }
    }
}

/// Display-friendly representation of a tool's input parameters.
#[derive(Debug, Clone)]
pub enum ToolInputDisplay {
    Path(String),
    Diff {
        path: String,
        diff: String,
    },
    Command {
        cmd: String,
        cwd: Option<String>,
    },
    Query(String),
    /// Pretty-printed JSON, truncated to 8 lines.
    Json(String),
    Text(String),
    /// Nothing meaningful to show — callers fall back to `TraceEntry.text`.
    Empty,
}

/// Structured representation of a tool's output for rendering.
#[derive(Debug, Clone)]
pub enum ObservePayload {
    CommandOutput {
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },
    FileRead {
        path: Option<String>,
        content: String,
        truncated: bool,
    },
    EditResult {
        path: Option<String>,
        replacements: Option<usize>,
        diff: Option<String>,
    },
    Json {
        pretty: String,
    },
    Text {
        body: String,
    },
    Error {
        message: String,
    },
}

/// Short badge shown in the TUI for a session mode.
#[derive(Debug, Clone)]
pub struct ModeBadge {
    pub short: &'static str, // "PLAN", "AUTO", "RO"
    pub color: BadgeColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeColor {
    Amber,
    Green,
    Red,
    Neutral,
}

/// Classify a tool call.
///
/// Takes `&ToolCall` because ACP tool identity is `(title, kind)` — there is
/// no `name` field on the ACP `ToolCall` struct. Pipeline: `ToolKind →
/// ToolFamily` (via `From`), then per-kind `refine(title, base)` which may
/// upgrade `Unknown` → `Plan`/`Mcp`. Never panics.
pub fn classify_tool(tc: &ToolCall, kind: AgentKind) -> ToolFamily {
    classify_tool_parts(&tc.title, tc.kind, kind)
}

pub fn classify_tool_parts(title: &str, tool_kind: ToolKind, kind: AgentKind) -> ToolFamily {
    let base = ToolFamily::from(tool_kind);
    match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => claude::refine(title, base),
        AgentKind::CodexAcp => codex::refine(title, base),
        AgentKind::Kimi => kimi::refine(title, base),
        AgentKind::Gemini => gemini::refine(title, base),
        AgentKind::Kiro => kiro::refine(title, base),
        AgentKind::OpenCode | AgentKind::Generic => generic::refine(title, base),
    }
}

/// Convert a ToolCall's `raw_input` JSON into a display-friendly form.
/// Per-kind first; generic fallback.
pub fn format_input(raw_input: &Value, kind: AgentKind) -> ToolInputDisplay {
    let per_kind = match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => {
            claude::try_format_input(raw_input)
        }
        AgentKind::CodexAcp => codex::try_format_input(raw_input),
        AgentKind::Kimi => kimi::try_format_input(raw_input),
        AgentKind::Gemini => gemini::try_format_input(raw_input),
        AgentKind::Kiro => kiro::try_format_input(raw_input),
        AgentKind::OpenCode | AgentKind::Generic => None,
    };
    per_kind.unwrap_or_else(|| generic::format_input(raw_input))
}

/// Pipeline: MCP-envelope unwrap (shared) → per-kind extraction → generic fallback.
pub fn extract_observe(raw_output: &Value, kind: AgentKind) -> ObservePayload {
    let unwrapped = mcp::unwrap_envelope(raw_output);
    let per_kind = match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => {
            claude::try_extract_observe(&unwrapped)
        }
        AgentKind::CodexAcp => codex::try_extract_observe(&unwrapped),
        AgentKind::Kimi => kimi::try_extract_observe(&unwrapped),
        AgentKind::Gemini => gemini::try_extract_observe(&unwrapped),
        AgentKind::Kiro => kiro::try_extract_observe(&unwrapped),
        AgentKind::OpenCode | AgentKind::Generic => None,
    };
    per_kind.unwrap_or_else(|| generic::extract_observe(&unwrapped))
}

/// Translate `CurrentModeUpdate::current_mode_id` into a short badge.
/// `None` = the kind has no known modes (callers hide the badge).
pub fn mode_badge(mode_id: &str, kind: AgentKind) -> Option<ModeBadge> {
    match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => claude::mode_badge(mode_id),
        AgentKind::CodexAcp => codex::mode_badge(mode_id),
        AgentKind::Kimi => None,
        AgentKind::Gemini => None,
        AgentKind::Kiro => kiro::mode_badge(mode_id),
        AgentKind::OpenCode | AgentKind::Generic => None,
    }
}

/// Normalized view of vendor-specific `_meta` extensions on a ToolCall.
///
/// Fields are added ONLY when a concept is genuinely cross-vendor and NOT
/// already expressed by an ACP spec field. Adding a field is a design
/// change — see `docs/spur/acp-meta-conventions.md`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SpurToolMeta {
    /// Vendor-specific tool identity (e.g. "Bash", "Edit", "/spec-init").
    /// Prefer this over `tc.title` for identity-sensitive rendering.
    pub tool_name: Option<String>,

    /// ID of the parent ToolCall when this call was spawned by a
    /// subagent / Task mechanism. Used for render indentation.
    pub parent_tool_use_id: Option<String>,
}

/// Extract a `SpurToolMeta` from a `ToolCall` using the vendor's
/// `_meta.<vendor>.*` convention. Returns default for unknown/absent meta.
pub fn extract_tool_meta(tc: &ToolCall, kind: AgentKind) -> SpurToolMeta {
    match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => claude::extract_tool_meta(tc),
        AgentKind::CodexAcp => codex::extract_tool_meta(tc),
        AgentKind::Kimi => kimi::extract_tool_meta(tc),
        AgentKind::Gemini => gemini::extract_tool_meta(tc),
        AgentKind::Kiro => kiro::extract_tool_meta(tc),
        AgentKind::OpenCode | AgentKind::Generic => SpurToolMeta::default(),
    }
}

#[derive(Debug)]
pub enum SessionEventStandardizer {
    Kimi(kimi::SessionStandardizer),
    Gemini(gemini::SessionStandardizer),
    Passthrough,
}

impl SessionEventStandardizer {
    pub fn for_agent(kind: AgentKind) -> Self {
        match kind {
            AgentKind::Kimi => Self::Kimi(kimi::SessionStandardizer::default()),
            AgentKind::Gemini => Self::Gemini(gemini::SessionStandardizer),
            _ => Self::Passthrough,
        }
    }

    pub fn standardize(&mut self, notification: SessionNotification) -> SessionNotification {
        match self {
            Self::Kimi(standardizer) => standardizer.standardize(notification),
            Self::Gemini(standardizer) => standardizer.standardize(notification),
            Self::Passthrough => notification,
        }
    }
}

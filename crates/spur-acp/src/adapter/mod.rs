pub mod arg_picker_hint;
pub mod claude;
pub mod codex;
pub mod config_options;
pub mod diff;
pub mod gemini;
pub mod generic;
pub mod grok_session_display;
pub mod kimi;
pub mod kiro;
pub mod kiro_session_display;
pub mod mcp;

pub use diff::unified_edit_diff;

use crate::types::AgentKind;
use agent_client_protocol::schema::v1::{SessionNotification, ToolCall, ToolKind};
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
    /// Codex ACP native child-session lifecycle normalized as a tool call.
    Subagent,
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
    /// Ordered file changes supplied as typed ACP `ToolCallContent::Diff`
    /// values. This remains distinct from adapter-derived [`Self::Diff`] so
    /// clients can preserve every wire item and its presentation metadata.
    FileChanges(Vec<FileChangeDisplay>),
    Command {
        cmd: String,
        cwd: Option<String>,
    },
    Query(String),
    /// Pretty-printed JSON, capped to [`JSON_INPUT_PREVIEW_LINES`] content lines.
    ///
    /// When `omitted_lines > 0`, more source lines existed and the renderer must
    /// show a truncation affordance (the adapter does **not** embed a `"…"`
    /// sentinel in `body` — that dual-capped path dropped the marker in the TUI).
    Json {
        body: String,
        /// Count of pretty-printed lines omitted after the cap; `0` = complete.
        omitted_lines: usize,
    },
    Text(String),
    /// Nothing meaningful to show — callers fall back to `TraceEntry.text`.
    Empty,
}

/// Presentation semantics for one typed ACP file change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeKind {
    Added,
    Updated,
    Deleted,
}

impl FileChangeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Updated => "updated",
            Self::Deleted => "deleted",
        }
    }
}

/// One normalized file change, retained in ACP wire order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChangeDisplay {
    pub path: String,
    pub kind: FileChangeKind,
    /// Bounded unified-diff preview retained for this file.
    pub diff: String,
    /// Diff lines omitted from `diff` by the aggregate tool-call budget.
    pub omitted_lines: usize,
}

/// Max content lines kept for [`ToolInputDisplay::Json`] after pretty-print.
/// Shared contract with the TUI preview (see `trace_format::input_display_lines`).
pub const JSON_INPUT_PREVIEW_LINES: usize = 8;

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
    let base = ToolFamily::from(tc.kind);
    if kind == AgentKind::CodexAcp {
        codex::refine_tool_call(tc, base)
    } else {
        classify_tool_parts(&tc.title, tc.kind, kind)
    }
}

pub fn classify_tool_parts(title: &str, tool_kind: ToolKind, kind: AgentKind) -> ToolFamily {
    let base = ToolFamily::from(tool_kind);
    match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => claude::refine(title, base),
        AgentKind::CodexAcp => codex::refine(title, base),
        AgentKind::Kimi => kimi::refine(title, base),
        AgentKind::Gemini => gemini::refine(title, base),
        AgentKind::Kiro => kiro::refine(title, base),
        AgentKind::OpenCode | AgentKind::Grok | AgentKind::Generic => generic::refine(title, base),
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
        AgentKind::OpenCode | AgentKind::Grok | AgentKind::Generic => None,
    };
    per_kind.unwrap_or_else(|| generic::format_input(raw_input))
}

/// Pipeline: MCP-envelope unwrap (shared) → per-kind extraction → generic fallback.
pub fn extract_observe(raw_output: &Value, kind: AgentKind) -> ObservePayload {
    if let Some(body) = mcp::single_text(raw_output) {
        return ObservePayload::Text {
            body: body.to_owned(),
        };
    }

    let unwrapped = mcp::unwrap_envelope(raw_output);
    let per_kind = match kind {
        AgentKind::ClaudeCodeAcp | AgentKind::ClaudeStreamJson => {
            claude::try_extract_observe(&unwrapped)
        }
        AgentKind::CodexAcp => codex::try_extract_observe(&unwrapped),
        AgentKind::Kimi => kimi::try_extract_observe(&unwrapped),
        AgentKind::Gemini => gemini::try_extract_observe(&unwrapped),
        AgentKind::Kiro => kiro::try_extract_observe(&unwrapped),
        AgentKind::OpenCode | AgentKind::Grok | AgentKind::Generic => None,
    };
    per_kind.unwrap_or_else(|| generic::extract_observe(&unwrapped))
}

#[cfg(test)]
mod extract_observe_tests {
    use super::{extract_observe, ObservePayload};
    use crate::types::AgentKind;
    use serde_json::json;

    #[test]
    fn single_text_envelopes_match_direct_text_for_every_agent_kind() {
        let kinds = [
            AgentKind::ClaudeStreamJson,
            AgentKind::ClaudeCodeAcp,
            AgentKind::CodexAcp,
            AgentKind::Kiro,
            AgentKind::Kimi,
            AgentKind::Gemini,
            AgentKind::OpenCode,
            AgentKind::Grok,
            AgentKind::Generic,
        ];
        let values = [
            json!("hello world"),
            json!({"items": [{"Text": "hello world"}]}),
            json!({"content": [{"type": "text", "text": "hello world"}]}),
        ];

        for kind in kinds {
            for value in &values {
                let ObservePayload::Text { body } = extract_observe(value, kind) else {
                    panic!("expected Text for {kind:?} from {value}");
                };
                assert_eq!(body, "hello world", "agent kind: {kind:?}");
            }
        }
    }
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
        AgentKind::OpenCode | AgentKind::Grok | AgentKind::Generic => None,
    }
}

/// Translate a mode with its full ACP presentation record when available.
pub fn mode_badge_with_metadata(
    mode_id: &str,
    kind: AgentKind,
    mode: Option<&agent_client_protocol::schema::v1::SessionMode>,
) -> Option<ModeBadge> {
    if kind == AgentKind::CodexAcp {
        if let Some(mode) = mode {
            return codex::mode_badge_with_metadata(mode);
        }
    }
    mode_badge(mode_id, kind)
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
        AgentKind::OpenCode | AgentKind::Grok | AgentKind::Generic => SpurToolMeta::default(),
    }
}

#[derive(Debug)]
pub enum SessionEventStandardizer {
    Kimi(kimi::SessionStandardizer),
    Gemini(gemini::SessionStandardizer),
    Passthrough,
}

impl SessionEventStandardizer {
    /// Selects the standardizer for empirically discovered wire-protocol quirks.
    ///
    /// AgentKind dispatch stays exhaustive when the value is known up front, but
    /// this wildcard is intentional: Kimi and Gemini are listed because their
    /// session notifications needed reverse-engineered standardization, and a
    /// new agent's needs are knowable only after observing its wire traffic.
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

#[cfg(test)]
mod unified_edit_diff_tests {
    use super::unified_edit_diff;

    #[test]
    fn single_line_replace_omits_unchanged_regions_outside_the_hunk() {
        let old = "one\ntwo\nthree\nfour\nfive\nsix\nseven\n";
        let new = "one\ntwo\nthree\nFOUR\nfive\nsix\nseven\n";

        let diff = unified_edit_diff("src/lib.rs", old, new, 1);

        assert_eq!(
            diff,
            "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -3,3 +3,3 @@\n three\n-four\n+FOUR\n five\n"
        );
        assert!(!diff.contains(" one\n"));
        assert!(!diff.contains(" seven\n"));
    }

    #[test]
    fn insert_only_emits_an_addition_hunk() {
        let diff = unified_edit_diff("notes.txt", "alpha\ngamma\n", "alpha\nbeta\ngamma\n", 1);

        assert_eq!(
            diff,
            "--- a/notes.txt\n+++ b/notes.txt\n@@ -1,2 +1,3 @@\n alpha\n+beta\n gamma\n"
        );
    }

    #[test]
    fn delete_only_emits_a_deletion_hunk() {
        let diff = unified_edit_diff("notes.txt", "alpha\nbeta\ngamma\n", "alpha\ngamma\n", 1);

        assert_eq!(
            diff,
            "--- a/notes.txt\n+++ b/notes.txt\n@@ -1,3 +1,2 @@\n alpha\n-beta\n gamma\n"
        );
    }

    #[test]
    fn identical_text_has_no_diff() {
        assert_eq!(
            unified_edit_diff("same.txt", "unchanged\n", "unchanged\n", 3),
            ""
        );
    }

    #[test]
    fn empty_old_text_emits_a_file_creation_hunk() {
        let diff = unified_edit_diff("new.txt", "", "alpha\nbeta\n", 3);

        assert_eq!(
            diff,
            "--- a/new.txt\n+++ b/new.txt\n@@ -0,0 +1,2 @@\n+alpha\n+beta\n"
        );
    }

    #[test]
    fn empty_new_text_emits_a_file_deletion_hunk() {
        let diff = unified_edit_diff("old.txt", "alpha\nbeta\n", "", 3);

        assert_eq!(
            diff,
            "--- a/old.txt\n+++ b/old.txt\n@@ -1,2 +0,0 @@\n-alpha\n-beta\n"
        );
    }
}

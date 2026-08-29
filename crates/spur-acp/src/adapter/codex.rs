use agent_client_protocol::schema::v1::{SessionMode, ToolCall};
use serde_json::Value;

use super::{BadgeColor, ModeBadge, ObservePayload, ToolFamily, ToolInputDisplay};

/// Refine the base `ToolFamily` for Codex-family agents.
///
/// Rules:
/// 1. Legacy `mcp__*` and Codex ACP v1.7 `mcp.*.*` titles → `Mcp`.
/// 2. If base is `Unknown` and title is `plan_update` → `Plan`.
/// 3. Otherwise return `base` unchanged.
pub fn refine(title: &str, base: ToolFamily) -> ToolFamily {
    if title.starts_with("mcp__") || is_v1_7_mcp_title(title) {
        return ToolFamily::Mcp;
    }
    if matches!(base, ToolFamily::Unknown) && title == "plan_update" {
        return ToolFamily::Plan;
    }
    base
}

/// Refine a complete Codex tool call, including v1.7 presentation metadata.
pub fn refine_tool_call(tc: &ToolCall, base: ToolFamily) -> ToolFamily {
    if matches!(base, ToolFamily::Unknown)
        && tc.tool_call_id.0.as_ref().starts_with("subagent:")
        && tc.title.starts_with("Subagent: ")
    {
        return ToolFamily::Subagent;
    }
    let is_mcp = tc
        .meta
        .as_ref()
        .and_then(|meta| meta.get("is_mcp_tool_call"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if is_mcp {
        ToolFamily::Mcp
    } else {
        refine(&tc.title, base)
    }
}

fn is_v1_7_mcp_title(title: &str) -> bool {
    let mut parts = title.split('.');
    matches!(parts.next(), Some("mcp"))
        && parts.next().is_some_and(|part| !part.is_empty())
        && parts.next().is_some_and(|part| !part.is_empty())
}

/// Try to parse Codex's tool input shapes.
///
/// Recognised shapes:
/// - `{cmd, cwd?}` → `Command`
/// - `{patch}`     → `Diff { path: "<patch>", diff: patch }`
pub fn try_format_input(raw: &Value) -> Option<ToolInputDisplay> {
    let obj = raw.as_object()?;

    // Command shape: {cmd, cwd?}
    if let Some(cmd) = obj.get("cmd").and_then(|v| v.as_str()) {
        let cwd = obj.get("cwd").and_then(|v| v.as_str()).map(str::to_owned);
        return Some(ToolInputDisplay::Command {
            cmd: cmd.to_owned(),
            cwd,
        });
    }

    // Current Codex ACP harmony/unified-exec shape:
    // {command: ["/bin/zsh", "-lc", "<script>"], cwd?, ...}
    if let Some(command) = obj.get("command") {
        let cmd = command
            .as_str()
            .map(str::to_owned)
            .or_else(|| command_array_to_display(command));
        if let Some(cmd) = cmd {
            let cwd = obj.get("cwd").and_then(|v| v.as_str()).map(str::to_owned);
            return Some(ToolInputDisplay::Command { cmd, cwd });
        }
    }

    // Patch shape: {patch}
    if let Some(patch) = obj.get("patch").and_then(|v| v.as_str()) {
        return Some(ToolInputDisplay::Diff {
            path: "<patch>".to_owned(),
            diff: patch.to_owned(),
        });
    }

    None
}

fn command_array_to_display(command: &Value) -> Option<String> {
    let args = command.as_array()?;
    let strings: Vec<&str> = args.iter().filter_map(Value::as_str).collect();
    if strings.is_empty() {
        return None;
    }
    if strings.len() >= 3 && matches!(strings[1], "-lc" | "-c") {
        return Some(strings[2].to_owned());
    }
    Some(strings.join(" "))
}

/// Try to parse Codex's tool output into an `ObservePayload`.
///
/// Recognised shape: any object containing at least one of `exit_code`,
/// `stdout`, `stderr`, or Codex ACP's `formatted_output`. Missing fields
/// default to empty / None.
pub fn try_extract_observe(raw: &Value) -> Option<ObservePayload> {
    let obj = raw.as_object()?;

    let exit_code = obj
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let stdout = obj
        .get("stdout")
        .and_then(|v| v.as_str())
        .filter(|value| !value.is_empty())
        .or_else(|| obj.get("formatted_output").and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_owned();
    let stderr = obj
        .get("stderr")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    // Semantic guard: require at least one field to carry meaningful content.
    // An object like `{"exit_code": null, "stdout": "", "stderr": ""}` has all
    // three keys present but conveys nothing — let the generic fallback render
    // it rather than claim it as structured command output.
    if exit_code.is_none() && stdout.is_empty() && stderr.is_empty() {
        return None;
    }

    Some(ObservePayload::CommandOutput {
        exit_code,
        stdout,
        stderr,
    })
}

/// Map Codex mode IDs to short badges.
///
/// | mode_id               | badge | color   |
/// |-----------------------|-------|---------|
/// | `"agent-full-access"` | FULL  | Green   |
/// | `"agent"`             | AGENT | Amber   |
/// | `"read-only"`         | RO    | Neutral |
/// | anything else         | —     | (none)  |
pub fn mode_badge(mode_id: &str) -> Option<ModeBadge> {
    match mode_id {
        "agent-full-access" => Some(ModeBadge {
            short: "FULL",
            color: BadgeColor::Green,
        }),
        "agent" => Some(ModeBadge {
            short: "AGENT",
            color: BadgeColor::Amber,
        }),
        "read-only" => Some(ModeBadge {
            short: "RO",
            color: BadgeColor::Neutral,
        }),
        _ => None,
    }
}

/// Map Codex ACP v1.7 semantic mode metadata to a badge. Unknown or absent
/// metadata falls back to the legacy stable-ID mapping.
pub fn mode_badge_with_metadata(mode: &SessionMode) -> Option<ModeBadge> {
    let semantic_kind = mode
        .meta
        .as_ref()
        .and_then(|meta| meta.get("kind"))
        .and_then(Value::as_str);
    match semantic_kind {
        Some("standard") => Some(ModeBadge {
            short: "ASK",
            color: BadgeColor::Neutral,
        }),
        Some("auto_review") => Some(ModeBadge {
            short: "AUTO",
            color: BadgeColor::Amber,
        }),
        Some("full_access") => Some(ModeBadge {
            short: "FULL",
            color: BadgeColor::Red,
        }),
        _ => mode_badge(mode.id.0.as_ref()),
    }
}

/// Codex `_meta` extractor stub.
///
/// Live ACP re-probe (2026-07-13, codex-acp 1.1.2) and in-tree tool-call
/// fixtures (`tool_call_exec`, apply_patch) carry no `_meta.codex` plane on
/// tool_call updates. Only Claude emits a real vendor tool meta today
/// (`_meta.claudeCode`). Leave this as a no-op until a billed tool-turn
/// capture proves a codex-specific meta shape — do not invent keys.
///
/// See docs/spur/acp-meta-conventions.md and
/// `.spur/logs/reprobe-20260713/schema-inventory-20260713T023432.md`.
pub fn extract_tool_meta(_tc: &ToolCall) -> super::SpurToolMeta {
    super::SpurToolMeta::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mode(id: &str, name: &str, kind: &str) -> SessionMode {
        let mut meta = serde_json::Map::new();
        meta.insert("kind".to_string(), Value::String(kind.to_string()));
        SessionMode::new(id.to_owned(), name.to_owned()).meta(meta)
    }

    #[test]
    fn v1_7_mode_kind_drives_semantic_badges() {
        let cases = [
            (mode("read-only", "Ask for approval", "standard"), "ASK"),
            (mode("agent", "Agent", "auto_review"), "AUTO"),
            (
                mode("agent-full-access", "Agent (full access)", "full_access"),
                "FULL",
            ),
        ];

        for (mode, expected) in cases {
            assert_eq!(
                mode_badge_with_metadata(&mode).map(|badge| badge.short),
                Some(expected),
                "mode={mode:?}"
            );
        }
    }

    #[test]
    fn v1_7_native_subagent_has_a_semantic_tool_family() {
        let call = ToolCall::new("subagent:child", "Subagent: researcher")
            .kind(agent_client_protocol::schema::v1::ToolKind::Other);

        assert_eq!(
            refine_tool_call(&call, ToolFamily::Unknown),
            ToolFamily::Subagent
        );
    }

    #[test]
    fn subagent_identity_does_not_override_a_typed_tool_family() {
        let call = ToolCall::new("subagent:child", "Subagent: researcher")
            .kind(agent_client_protocol::schema::v1::ToolKind::Execute);

        assert_eq!(
            refine_tool_call(&call, ToolFamily::Execute),
            ToolFamily::Execute
        );
    }
}

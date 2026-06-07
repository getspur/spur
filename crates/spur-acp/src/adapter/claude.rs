use std::fmt::Write as _;

use agent_client_protocol::schema::ToolCall;
use serde_json::Value;

use super::{BadgeColor, ModeBadge, ObservePayload, ToolFamily, ToolInputDisplay};

/// Refine the base `ToolFamily` for Claude-family agents.
///
/// Rules (in priority order):
/// 1. `mcp__*` titles → `Mcp` (always, regardless of base).
/// 2. If base is `Unknown`:
///    - `TodoWrite` → `Plan`
///    - anything else stays `Unknown` (e.g. `Task` — subagent dispatch; a
///      distinct family would be misleading at this scope).
/// 3. Otherwise return `base` unchanged (do not downgrade protocol-given kinds).
pub fn refine(title: &str, base: ToolFamily) -> ToolFamily {
    if title.starts_with("mcp__") {
        return ToolFamily::Mcp;
    }
    if matches!(base, ToolFamily::Unknown) && title == "TodoWrite" {
        return ToolFamily::Plan;
    }
    base
}

/// Synthesise a minimal unified diff from an old/new string pair.
///
/// Produces an `--- a/<path>` / `+++ b/<path>` header followed by one hunk
/// that removes every line of `old` and adds every line of `new`.  This is a
/// "delete-all + add-all" diff — not line-level LCS, but sufficient for TUI
/// preview purposes without pulling in an external diff library.
fn make_unified(path: &str, old: &str, new: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "--- a/{path}");
    let _ = writeln!(out, "+++ b/{path}");

    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let hunk_header = format!(
        "@@ -{},{} +{},{} @@\n",
        i32::from(!old_lines.is_empty()),
        old_lines.len(),
        i32::from(!new_lines.is_empty()),
        new_lines.len()
    );
    out.push_str(&hunk_header);
    for l in &old_lines {
        out.push('-');
        out.push_str(l);
        out.push('\n');
    }
    for l in &new_lines {
        out.push('+');
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Try to parse Claude's tool input shapes into a `ToolInputDisplay`.
///
/// Recognised shapes:
/// - `{file_path, old_string, new_string}` → `Diff` (synthesised unified diff)
/// - `{command, …}`                         → `Command`
/// - `{file_path}`                          → `Path`
/// - `{pattern, …}`                         → `Query`
pub fn try_format_input(raw: &Value) -> Option<ToolInputDisplay> {
    let obj = raw.as_object()?;

    // Edit shape: {file_path, old_string, new_string}
    if let (Some(fp), Some(old), Some(new)) = (
        obj.get("file_path").and_then(|v| v.as_str()),
        obj.get("old_string").and_then(|v| v.as_str()),
        obj.get("new_string").and_then(|v| v.as_str()),
    ) {
        let diff = make_unified(fp, old, new);
        return Some(ToolInputDisplay::Diff {
            path: fp.to_owned(),
            diff,
        });
    }

    // Command shape: {command, cwd?}
    if let Some(cmd) = obj.get("command").and_then(|v| v.as_str()) {
        let cwd = obj.get("cwd").and_then(|v| v.as_str()).map(str::to_owned);
        return Some(ToolInputDisplay::Command {
            cmd: cmd.to_owned(),
            cwd,
        });
    }

    // Path shape: {file_path}
    if let Some(fp) = obj.get("file_path").and_then(|v| v.as_str()) {
        return Some(ToolInputDisplay::Path(fp.to_owned()));
    }

    // Query/search shape: {pattern, …}
    if let Some(q) = obj.get("pattern").and_then(|v| v.as_str()) {
        return Some(ToolInputDisplay::Query(q.to_owned()));
    }

    None
}

/// Try to parse Claude's tool output (Bash-ish) into an `ObservePayload`.
///
/// Recognised shape: `{stdout, stderr, exit_code}` — the Bash tool result.
/// Everything else returns `None` so the generic fallback handles it.
pub fn try_extract_observe(raw: &Value) -> Option<ObservePayload> {
    let obj = raw.as_object()?;

    // Must have at least one of the Bash output fields.
    let has_stdout = obj.contains_key("stdout");
    let has_stderr = obj.contains_key("stderr");
    let has_exit = obj.contains_key("exit_code");

    if !has_stdout && !has_stderr && !has_exit {
        return None;
    }

    let exit_code = obj
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let stdout = obj
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    let stderr = obj
        .get("stderr")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();

    Some(ObservePayload::CommandOutput {
        exit_code,
        stdout,
        stderr,
    })
}

/// Map Claude mode IDs to short badges.
///
/// | mode_id            | badge  | color   |
/// |--------------------|--------|---------|
/// | `"plan"`           | PLAN   | Amber   |
/// | `"acceptEdits"`    | AUTO   | Green   |
/// | `"bypassPermissions"` | BYPASS | Red  |
/// | anything else      | —      | (none)  |
pub fn mode_badge(mode_id: &str) -> Option<ModeBadge> {
    match mode_id {
        "plan" => Some(ModeBadge {
            short: "PLAN",
            color: BadgeColor::Amber,
        }),
        "acceptEdits" => Some(ModeBadge {
            short: "AUTO",
            color: BadgeColor::Green,
        }),
        "bypassPermissions" => Some(ModeBadge {
            short: "BYPASS",
            color: BadgeColor::Red,
        }),
        _ => None,
    }
}

/// Read `_meta.claudeCode.{toolName, parentToolUseId}` from a `ToolCall`.
/// Absent keys produce `None`; non-string values are treated as absent.
pub fn extract_tool_meta(tc: &ToolCall) -> super::SpurToolMeta {
    let cc = tc.meta.as_ref().and_then(|m| m.get("claudeCode"));
    super::SpurToolMeta {
        tool_name: cc
            .and_then(|v| v.get("toolName"))
            .and_then(|v| v.as_str())
            .map(String::from),
        parent_tool_use_id: cc
            .and_then(|v| v.get("parentToolUseId"))
            .and_then(|v| v.as_str())
            .map(String::from),
    }
}

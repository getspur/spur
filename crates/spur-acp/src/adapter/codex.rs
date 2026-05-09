use agent_client_protocol::schema::ToolCall;
use serde_json::Value;

use super::{BadgeColor, ModeBadge, ObservePayload, ToolFamily, ToolInputDisplay};

/// Refine the base `ToolFamily` for Codex-family agents.
///
/// Rules:
/// 1. `mcp__*` titles → `Mcp` (always).
/// 2. If base is `Unknown` and title is `plan_update` → `Plan`.
/// 3. Otherwise return `base` unchanged.
pub fn refine(title: &str, base: ToolFamily) -> ToolFamily {
    if title.starts_with("mcp__") {
        return ToolFamily::Mcp;
    }
    if matches!(base, ToolFamily::Unknown) && title == "plan_update" {
        return ToolFamily::Plan;
    }
    base
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
        let cwd = obj
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        return Some(ToolInputDisplay::Command {
            cmd: cmd.to_string(),
            cwd,
        });
    }

    // Current Codex ACP harmony/unified-exec shape:
    // {command: ["/bin/zsh", "-lc", "<script>"], cwd?, ...}
    if let Some(command) = obj.get("command") {
        let cmd = command
            .as_str()
            .map(str::to_string)
            .or_else(|| command_array_to_display(command));
        if let Some(cmd) = cmd {
            let cwd = obj
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            return Some(ToolInputDisplay::Command { cmd, cwd });
        }
    }

    // Patch shape: {patch}
    if let Some(patch) = obj.get("patch").and_then(|v| v.as_str()) {
        return Some(ToolInputDisplay::Diff {
            path: "<patch>".to_string(),
            diff: patch.to_string(),
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
        return Some(strings[2].to_string());
    }
    Some(strings.join(" "))
}

/// Try to parse Codex's tool output into an `ObservePayload`.
///
/// Recognised shape: any object containing at least one of
/// `exit_code`, `stdout`, `stderr`.  Missing fields default to empty / None.
pub fn try_extract_observe(raw: &Value) -> Option<ObservePayload> {
    let obj = raw.as_object()?;

    let exit_code = obj
        .get("exit_code")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
    let stdout = obj
        .get("stdout")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let stderr = obj
        .get("stderr")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

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
/// | mode_id        | badge  | color   |
/// |----------------|--------|---------|
/// | `"full-auto"`  | AUTO   | Green   |
/// | `"read-only"`  | RO     | Neutral |
/// | `"on-failure"` | ONFAIL | Amber   |
/// | `"on-request"` | ASK    | Amber   |
/// | anything else  | —      | (none)  |
pub fn mode_badge(mode_id: &str) -> Option<ModeBadge> {
    match mode_id {
        "full-auto" => Some(ModeBadge {
            short: "AUTO",
            color: BadgeColor::Green,
        }),
        "read-only" => Some(ModeBadge {
            short: "RO",
            color: BadgeColor::Neutral,
        }),
        "on-failure" => Some(ModeBadge {
            short: "ONFAIL",
            color: BadgeColor::Amber,
        }),
        "on-request" => Some(ModeBadge {
            short: "ASK",
            color: BadgeColor::Amber,
        }),
        _ => None,
    }
}

/// Codex `_meta` extractor stub.
/// TODO(vendor-onboarding): replace with real extractor when codex emits
/// recognizable `_meta.codex.*` fields. See
/// docs/spur/acp-meta-conventions.md.
pub fn extract_tool_meta(_tc: &ToolCall) -> super::SpurToolMeta {
    super::SpurToolMeta::default()
}

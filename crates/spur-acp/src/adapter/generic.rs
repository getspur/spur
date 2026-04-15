use serde_json::Value;

use super::{ObservePayload, ToolFamily, ToolInputDisplay};

/// Generic `refine` — catches common `Other`-kinded tools the protocol
/// didn't classify, via case-insensitive substring match on `title`.
pub fn refine(title: &str, base: ToolFamily) -> ToolFamily {
    let low = title.to_ascii_lowercase();
    if low.starts_with("mcp__") {
        return ToolFamily::Mcp;
    }
    if matches!(base, ToolFamily::Unknown) && (low.contains("todo") || low.contains("plan")) {
        return ToolFamily::Plan;
    }
    base
}

/// Generic input formatter. Invoked only when the per-kind module returned None.
pub fn format_input(raw: &Value) -> ToolInputDisplay {
    match raw {
        Value::Null => ToolInputDisplay::Empty,
        Value::Object(obj) if obj.is_empty() => ToolInputDisplay::Empty,
        Value::Object(obj) => {
            // Check for path-like fields
            for key in &["path", "file_path", "target", "filename"] {
                if let Some(p) = obj.get(*key).and_then(|v| v.as_str()) {
                    return ToolInputDisplay::Path(p.to_string());
                }
            }
            // Check for command-like fields
            for key in &["command", "cmd"] {
                if let Some(cmd) = obj.get(*key).and_then(|v| v.as_str()) {
                    let cwd = obj
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    return ToolInputDisplay::Command {
                        cmd: cmd.to_string(),
                        cwd,
                    };
                }
            }
            // Check for query/search fields
            for key in &["pattern", "query"] {
                if let Some(q) = obj.get(*key).and_then(|v| v.as_str()) {
                    return ToolInputDisplay::Query(q.to_string());
                }
            }
            // Fall back to pretty JSON, truncated to 8 lines
            let pretty = serde_json::to_string_pretty(raw).unwrap_or_default();
            let truncated = truncate_lines(&pretty, 8);
            ToolInputDisplay::Json(truncated)
        }
        Value::String(s) => ToolInputDisplay::Text(s.clone()),
        _ => {
            let pretty = serde_json::to_string_pretty(raw).unwrap_or_default();
            let truncated = truncate_lines(&pretty, 8);
            ToolInputDisplay::Json(truncated)
        }
    }
}

/// Generic observe extractor. Receives the MCP-unwrapped Value.
pub fn extract_observe(raw: &Value) -> ObservePayload {
    match raw {
        Value::Null => ObservePayload::Text {
            body: String::new(),
        },
        Value::String(s) => ObservePayload::Text { body: s.clone() },
        Value::Object(obj) => {
            // Check for error indicator
            let is_error = obj
                .get("error")
                .map(|v| v.as_bool().unwrap_or(false) || v.is_string())
                .unwrap_or(false);
            if is_error {
                let message = obj
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                return ObservePayload::Error { message };
            }

            // Check for command output: exit_code / exitCode / status + optional stdout/stderr
            let exit_code = obj
                .get("exit_code")
                .or_else(|| obj.get("exitCode"))
                .and_then(|v| v.as_i64())
                .map(|v| v as i32);
            let has_status = obj.get("status").and_then(|v| v.as_i64()).is_some();
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

            if exit_code.is_some() || has_status || !stdout.is_empty() || !stderr.is_empty() {
                // Prefer exit_code; fall back to status field
                let resolved_exit = exit_code
                    .or_else(|| obj.get("status").and_then(|v| v.as_i64()).map(|v| v as i32));
                return ObservePayload::CommandOutput {
                    exit_code: resolved_exit,
                    stdout,
                    stderr,
                };
            }

            // Check for file read: content field
            if let Some(content) = obj.get("content").and_then(|v| v.as_str()) {
                let path = obj
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let truncated = obj
                    .get("__truncated__")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                return ObservePayload::FileRead {
                    path,
                    content: content.to_string(),
                    truncated,
                };
            }

            // Check for edit result: diff / replacements / replaced fields
            let has_diff = obj.contains_key("diff");
            let has_replacements = obj.contains_key("replacements") || obj.contains_key("replaced");
            if has_diff || has_replacements {
                let path = obj
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let replacements = obj
                    .get("replacements")
                    .or_else(|| obj.get("replaced"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as usize);
                let diff = obj
                    .get("diff")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                return ObservePayload::EditResult {
                    path,
                    replacements,
                    diff,
                };
            }

            // Fall back to pretty JSON
            let pretty = serde_json::to_string_pretty(raw).unwrap_or_default();
            ObservePayload::Json { pretty }
        }
        _ => {
            let pretty = serde_json::to_string_pretty(raw).unwrap_or_default();
            ObservePayload::Json { pretty }
        }
    }
}

fn truncate_lines(s: &str, max_lines: usize) -> String {
    let mut lines = s.lines();
    let collected: Vec<&str> = lines.by_ref().take(max_lines).collect();
    if lines.next().is_some() {
        format!("{}\n…", collected.join("\n"))
    } else {
        collected.join("\n")
    }
}

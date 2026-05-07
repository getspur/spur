//! Formatting helpers for ReAct trace entries.
//!
//! Pure functions that convert trace data into styled `ratatui::text::Line`
//! values. Extracted from `react_trace.rs` for testability and to reduce
//! the size of the god-component.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use spur_acp::{
    adapter::{ObservePayload, ToolFamily, ToolInputDisplay},
    LifecycleState,
};

use crate::theme::{resolve_token, ColorDepth, Theme};

fn token(theme: &Theme, name: &str) -> Color {
    resolve_token(theme, name, ColorDepth::Truecolor)
}

// ─── Glyph / label helpers ──────────────────────────────────────────

/// Convert external tool/file/stdout text into printable terminal text before
/// it reaches ratatui's backend.
///
/// Raw C0 controls break ratatui's cursor model: a tab jumps to a terminal tab
/// stop, carriage return moves to column 0, and ESC can begin an ANSI sequence.
/// In a contiguous diff stream that leaves stale cells behind, which matches
/// the ghost characters visible after numbered file-output rows.
pub(crate) fn terminal_safe_text(input: &str) -> String {
    if !input
        .as_bytes()
        .iter()
        .any(|b| is_terminal_control_byte(*b))
    {
        return input.to_string();
    }

    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '\t' => out.push_str("    "),
            '\x1b' => out.push_str("^["),
            '\x00'..='\x08' | '\x0b'..='\x0c' | '\x0e'..='\x1f' | '\x7f' | '\r' | '\n' => {
                out.push('^');
                out.push(control_name(ch));
            }
            _ => out.push(ch),
        }
    }
    out
}

fn is_terminal_control_byte(byte: u8) -> bool {
    matches!(byte, b'\t' | b'\r' | b'\n' | 0x00..=0x08 | 0x0b..=0x1f | 0x7f)
}

fn control_name(ch: char) -> char {
    match ch {
        '\x00'..='\x1a' => ((ch as u8) + b'@') as char,
        '\x1b' => '[',
        '\x1c' => '\\',
        '\x1d' => ']',
        '\x1e' => '^',
        '\x1f' => '_',
        '\x7f' => '?',
        _ => '?',
    }
}

/// Map `ToolFamily` to a display glyph + color.
pub(crate) fn family_glyph(theme: &Theme, f: ToolFamily) -> (&'static str, Color) {
    match f {
        ToolFamily::Read => ("⚙ reads", token(theme, "tool.family.read")),
        ToolFamily::Edit => ("✎ edits", token(theme, "tool.family.edit")),
        ToolFamily::Delete => ("✗ deletes", token(theme, "tool.family.delete")),
        ToolFamily::Move => ("→ moves", token(theme, "tool.family.move")),
        ToolFamily::Search => ("🔎 search", token(theme, "tool.family.search")),
        ToolFamily::Execute => ("$ runs", token(theme, "tool.family.bash")),
        ToolFamily::Think => ("◈ thinks", token(theme, "tool.family.thinking")),
        ToolFamily::Fetch => ("↯ fetch", token(theme, "tool.family.fetch")),
        ToolFamily::SwitchMode => ("⇄ mode", token(theme, "tool.family.switch_mode")),
        ToolFamily::Plan => ("▸ plan", token(theme, "tool.family.task")),
        ToolFamily::Mcp => ("⧉ mcp", token(theme, "tool.family.mcp")),
        ToolFamily::Unknown => ("🔧 ACT", token(theme, "tool.family.unknown")),
    }
}

/// Map `ObservePayload` to an outcome glyph + color.
pub(crate) fn outcome_glyph(theme: &Theme, p: &ObservePayload) -> (&'static str, Color) {
    match p {
        ObservePayload::CommandOutput {
            exit_code: Some(0), ..
        } => ("✓", token(theme, "react_trace.outcome.success.fg")),
        ObservePayload::CommandOutput {
            exit_code: Some(_), ..
        } => ("✗", token(theme, "react_trace.outcome.error.fg")),
        ObservePayload::CommandOutput {
            exit_code: None, ..
        } => ("?", token(theme, "react_trace.outcome.pending.fg")),
        ObservePayload::Error { .. } => ("✗", token(theme, "react_trace.outcome.error.fg")),
        _ => ("✓", token(theme, "react_trace.outcome.success.fg")),
    }
}

/// Verb used in the observe header (past tense).
pub(crate) fn observe_verb(p: &ObservePayload) -> &'static str {
    match p {
        ObservePayload::CommandOutput { .. } => "ran",
        ObservePayload::FileRead { .. } => "read",
        ObservePayload::EditResult { .. } => "edited",
        ObservePayload::Json { .. } | ObservePayload::Text { .. } => "done",
        ObservePayload::Error { .. } => "erred",
    }
}

/// Compact single-line identifier for a tool invocation.
pub(crate) fn input_summary(input: &ToolInputDisplay, tool: &str) -> String {
    match input {
        ToolInputDisplay::Path(p) => p.clone(),
        ToolInputDisplay::Diff { path, .. } => path.clone(),
        ToolInputDisplay::Command { cmd, .. } => cmd.clone(),
        ToolInputDisplay::Query(q) => format!("\"{}\"", q),
        ToolInputDisplay::Text(t) => t
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .to_string(),
        ToolInputDisplay::Json(_) | ToolInputDisplay::Empty => tool.to_string(),
    }
}

/// Compact outcome for collapsed grouped rendering:
/// (outcome glyph, glyph color, compact stats string).
pub(crate) fn observe_compact(
    theme: &Theme,
    payload: &ObservePayload,
) -> (&'static str, Color, String) {
    let success = token(theme, "react_trace.outcome.success.fg");
    let error = token(theme, "react_trace.outcome.error.fg");
    let pending = token(theme, "react_trace.outcome.pending.fg");
    match payload {
        ObservePayload::CommandOutput {
            exit_code,
            stdout,
            stderr,
        } => {
            let total = stdout.lines().count() + stderr.lines().count();
            match exit_code {
                Some(0) => ("✓", success, format!("{} lines", total)),
                Some(c) => ("✗", error, format!("exit {} · {} lines", c, total)),
                None => ("?", pending, format!("{} lines", total)),
            }
        }
        ObservePayload::FileRead {
            content, truncated, ..
        } => {
            let n = content.lines().count();
            let suffix = if *truncated { " (truncated)" } else { "" };
            ("✓", success, format!("{} lines{}", n, suffix))
        }
        ObservePayload::EditResult {
            replacements, diff, ..
        } => {
            if let Some(n) = replacements {
                (
                    "✓",
                    success,
                    format!("{} replacement{}", n, if *n == 1 { "" } else { "s" }),
                )
            } else if let Some(d) = diff {
                let plus = d.lines().filter(|l| l.starts_with('+')).count();
                let minus = d.lines().filter(|l| l.starts_with('-')).count();
                ("✓", success, format!("+{}/-{}", plus, minus))
            } else {
                ("✓", success, String::new())
            }
        }
        ObservePayload::Json { pretty } => {
            let n = pretty.lines().count();
            ("✓", success, format!("{} lines", n))
        }
        ObservePayload::Text { body } => {
            let n = body.lines().count();
            ("✓", success, format!("{} lines", n))
        }
        ObservePayload::Error { message } => {
            let truncated = if message.chars().count() > 60 {
                let mut end = 60;
                while !message.is_char_boundary(end) && end > 0 {
                    end -= 1;
                }
                format!("{}…", &message[..end])
            } else {
                message.clone()
            };
            ("✗", error, truncated)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_external_text_does_not_emit_terminal_controls() {
        assert_eq!(
            terminal_safe_text("2728\tself.process_action();"),
            "2728    self.process_action();"
        );
        assert_eq!(terminal_safe_text("red\x1b[31m"), "red^[[31m");
        assert_eq!(terminal_safe_text("left\rright"), "left^Mright");
    }
}

// ─── Line builders ──────────────────────────────────────────────────

/// Build display lines for a `ToolInputDisplay` value (3-space indented).
pub(crate) fn input_display_lines(theme: &Theme, input: &ToolInputDisplay) -> Vec<Line<'static>> {
    let subtle = token(theme, "react_trace.diff.context.fg");
    let diff_add = token(theme, "diff.add.fg");
    let diff_del = token(theme, "diff.del.fg");
    let command = token(theme, "react_trace.command.fg");
    let body = token(theme, "react_trace.message.body.fg");
    let mut lines = Vec::new();
    match input {
        ToolInputDisplay::Path(p) => {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(terminal_safe_text(p), Style::default().fg(subtle)),
            ]));
        }
        ToolInputDisplay::Diff { path, diff } => {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    terminal_safe_text(path),
                    Style::default().fg(subtle).add_modifier(Modifier::BOLD),
                ),
            ]));
            let mut count = 0usize;
            let mut total = 0usize;
            for dl in diff.lines() {
                total += 1;
                let _ = dl;
            }
            for dl in diff.lines() {
                if count >= 6 {
                    let remaining = total.saturating_sub(6);
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(
                            format!("[… {} more]", remaining),
                            Style::default().fg(subtle),
                        ),
                    ]));
                    break;
                }
                let color = if dl.starts_with('+') {
                    diff_add
                } else if dl.starts_with('-') {
                    diff_del
                } else {
                    subtle
                };
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(dl), Style::default().fg(color)),
                ]));
                count += 1;
            }
        }
        ToolInputDisplay::Command { cmd, cwd } => {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    format!("$ {}", terminal_safe_text(cmd)),
                    Style::default().fg(command),
                ),
            ]));
            if let Some(cwd) = cwd {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("(cwd: {})", terminal_safe_text(cwd)),
                        Style::default().fg(subtle),
                    ),
                ]));
            }
        }
        ToolInputDisplay::Query(q) => {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    terminal_safe_text(q),
                    Style::default().fg(body).add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        ToolInputDisplay::Json(p) => {
            for jl in p.lines().take(8) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(jl), Style::default().fg(subtle)),
                ]));
            }
        }
        ToolInputDisplay::Text(t) => {
            for tl in t.lines() {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(tl), Style::default().fg(body)),
                ]));
            }
        }
        ToolInputDisplay::Empty => {}
    }
    lines
}

/// Build display lines for an `ObservePayload`.
/// When `collapsed` is true, output is truncated to a short preview.
pub(crate) fn observe_payload_lines(
    theme: &Theme,
    payload: &ObservePayload,
    collapsed: bool,
) -> Vec<Line<'static>> {
    let subtle = token(theme, "react_trace.diff.context.fg");
    let body = token(theme, "react_trace.message.body.fg");
    let command = token(theme, "react_trace.command.fg");
    let read_color = token(theme, "tool.family.read");
    let edit_color = token(theme, "tool.family.edit");
    let diff_add = token(theme, "diff.add.fg");
    let diff_del = token(theme, "diff.del.fg");
    let error = token(theme, "react_trace.outcome.error.fg");
    let mut lines = Vec::new();
    match payload {
        ObservePayload::CommandOutput {
            exit_code,
            stdout,
            stderr,
        } => {
            let exit_str = match exit_code {
                Some(c) => format!("$ exit {}", c),
                None => "$ exit -".to_string(),
            };
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(exit_str, Style::default().fg(command)),
            ]));
            let stdout_limit = if collapsed { 8 } else { usize::MAX };
            let stderr_limit = if collapsed { 4 } else { usize::MAX };
            let stdout_total = stdout.lines().count();
            for sl in stdout.lines().take(stdout_limit) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(sl), Style::default().fg(body)),
                ]));
            }
            if collapsed && stdout_total > stdout_limit {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!(
                            "[… {} more lines · Ctrl+O expand]",
                            stdout_total - stdout_limit
                        ),
                        Style::default().fg(subtle),
                    ),
                ]));
            }
            let stderr_total = stderr.lines().count();
            for el in stderr.lines().take(stderr_limit) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(el), Style::default().fg(error)),
                ]));
            }
            if collapsed && stderr_total > stderr_limit {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("[… {} more lines]", stderr_total - stderr_limit),
                        Style::default().fg(subtle),
                    ),
                ]));
            }
        }
        ObservePayload::FileRead {
            path,
            content,
            truncated,
        } => {
            let line_count = content.lines().count();
            let path_str = path.as_deref().unwrap_or("<unknown>");
            let header = format!(
                "{} · {} lines{}",
                path_str,
                line_count,
                if *truncated { " (truncated)" } else { "" }
            );
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(header, Style::default().fg(read_color)),
            ]));
            let limit = if collapsed { 8 } else { usize::MAX };
            for cl in content.lines().take(limit) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(cl), Style::default().fg(body)),
                ]));
            }
            if collapsed && line_count > limit {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("[… {} more lines · Ctrl+O expand]", line_count - limit),
                        Style::default().fg(subtle),
                    ),
                ]));
            }
        }
        ObservePayload::EditResult {
            path,
            replacements,
            diff,
        } => {
            if let Some(n) = replacements {
                let msg = format!("{} replacement{}", n, if *n == 1 { "" } else { "s" });
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(msg, Style::default().fg(edit_color)),
                ]));
            } else if let Some(d) = diff {
                let limit = if collapsed { 6 } else { usize::MAX };
                let total = d.lines().count();
                for dl in d.lines().take(limit) {
                    let color = if dl.starts_with('+') {
                        diff_add
                    } else if dl.starts_with('-') {
                        diff_del
                    } else {
                        subtle
                    };
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(terminal_safe_text(dl), Style::default().fg(color)),
                    ]));
                }
                if collapsed && total > limit {
                    lines.push(Line::from(vec![
                        Span::raw("   "),
                        Span::styled(
                            format!("[… {} more lines]", total - limit),
                            Style::default().fg(subtle),
                        ),
                    ]));
                }
            } else if let Some(p) = path {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(p), Style::default().fg(subtle)),
                ]));
            }
        }
        ObservePayload::Json { pretty } => {
            let limit = if collapsed { 8 } else { usize::MAX };
            let total = pretty.lines().count();
            for jl in pretty.lines().take(limit) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(jl), Style::default().fg(subtle)),
                ]));
            }
            if collapsed && total > limit {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("[… {} more lines · Ctrl+O expand]", total - limit),
                        Style::default().fg(subtle),
                    ),
                ]));
            }
        }
        ObservePayload::Text { body: text_body } => {
            let limit = if collapsed { 8 } else { usize::MAX };
            let total = text_body.lines().count();
            for tl in text_body.lines().take(limit) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(tl), Style::default().fg(body)),
                ]));
            }
            if collapsed && total > limit {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("[… {} more lines · Ctrl+O expand]", total - limit),
                        Style::default().fg(subtle),
                    ),
                ]));
            }
        }
        ObservePayload::Error { message } => {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(terminal_safe_text(message), Style::default().fg(error)),
            ]));
        }
    }
    lines
}

/// Derive a live status label from the lineage for a Delegate trace entry.
pub(crate) fn derive_delegate_status(
    executor_id: Option<&str>,
    lineage: Option<&spur_core::lineage::projection::ExecutorLineage>,
) -> Option<&'static str> {
    let eid = executor_id?;
    let lin = lineage?;
    let node = lin.node(&spur_core::ExecutorId(eid.to_string()))?;
    Some(match node.phase {
        LifecycleState::Spawning => "spawning",
        LifecycleState::Running | LifecycleState::Resuming => "running",
        LifecycleState::AwaitingReview => "awaiting review",
        LifecycleState::Succeeded => "done",
        LifecycleState::Failed => "failed",
        LifecycleState::Cancelled => "cancelled",
    })
}

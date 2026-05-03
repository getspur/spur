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
pub(crate) fn family_glyph(f: ToolFamily) -> (&'static str, Color) {
    match f {
        ToolFamily::Read => ("reads", Color::Cyan),
        ToolFamily::Edit => ("edits", Color::Yellow),
        ToolFamily::Delete => ("deletes", Color::Red),
        ToolFamily::Move => ("moves", Color::Yellow),
        ToolFamily::Search => ("search", Color::Blue),
        ToolFamily::Execute => ("$ runs", Color::Magenta),
        ToolFamily::Think => ("thinks", Color::DarkGray),
        ToolFamily::Fetch => ("fetch", Color::Blue),
        ToolFamily::SwitchMode => ("mode", Color::Cyan),
        ToolFamily::Plan => ("plan", Color::Cyan),
        ToolFamily::Mcp => ("mcp", Color::DarkGray),
        ToolFamily::Unknown => ("ACT", Color::Yellow),
    }
}

/// Map `ObservePayload` to an outcome glyph + color.
///
/// Labels MUST be pure ASCII to avoid terminal font-fallback cursor desync
/// in ratatui's contiguous diff output. Do not introduce emoji, EAW=A, or
/// EAW=N glyphs here.
pub(crate) fn outcome_glyph(p: &ObservePayload) -> (&'static str, Color) {
    match p {
        ObservePayload::CommandOutput {
            exit_code: Some(0), ..
        } => ("ok", Color::Green),
        ObservePayload::CommandOutput {
            exit_code: Some(_), ..
        } => ("err", Color::Red),
        ObservePayload::CommandOutput {
            exit_code: None, ..
        } => ("?", Color::Yellow),
        ObservePayload::Error { .. } => ("err", Color::Red),
        _ => ("ok", Color::Green),
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
pub(crate) fn observe_compact(payload: &ObservePayload) -> (&'static str, Color, String) {
    match payload {
        ObservePayload::CommandOutput {
            exit_code,
            stdout,
            stderr,
        } => {
            let total = stdout.lines().count() + stderr.lines().count();
            match exit_code {
                Some(0) => ("ok", Color::Green, format!("{} lines", total)),
                Some(c) => ("err", Color::Red, format!("exit {} - {} lines", c, total)),
                None => ("?", Color::Yellow, format!("{} lines", total)),
            }
        }
        ObservePayload::FileRead {
            content, truncated, ..
        } => {
            let n = content.lines().count();
            let suffix = if *truncated { " (truncated)" } else { "" };
            ("ok", Color::Green, format!("{} lines{}", n, suffix))
        }
        ObservePayload::EditResult {
            replacements, diff, ..
        } => {
            if let Some(n) = replacements {
                (
                    "ok",
                    Color::Green,
                    format!("{} replacement{}", n, if *n == 1 { "" } else { "s" }),
                )
            } else if let Some(d) = diff {
                let plus = d.lines().filter(|l| l.starts_with('+')).count();
                let minus = d.lines().filter(|l| l.starts_with('-')).count();
                ("ok", Color::Green, format!("+{}/-{}", plus, minus))
            } else {
                ("ok", Color::Green, String::new())
            }
        }
        ObservePayload::Json { pretty } => {
            let n = pretty.lines().count();
            ("ok", Color::Green, format!("{} lines", n))
        }
        ObservePayload::Text { body } => {
            let n = body.lines().count();
            ("ok", Color::Green, format!("{} lines", n))
        }
        ObservePayload::Error { message } => {
            let truncated = if message.chars().count() > 60 {
                let mut end = 60;
                while !message.is_char_boundary(end) && end > 0 {
                    end -= 1;
                }
                format!("{}...", &message[..end])
            } else {
                message.clone()
            };
            ("err", Color::Red, truncated)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_ascii(label: &str) {
        assert!(
            label.is_ascii(),
            "ReAct-owned chrome must be ASCII to avoid terminal cursor-width desync: {label:?}"
        );
    }

    #[test]
    fn internal_trace_chrome_labels_are_ascii() {
        for family in [
            ToolFamily::Read,
            ToolFamily::Edit,
            ToolFamily::Delete,
            ToolFamily::Move,
            ToolFamily::Search,
            ToolFamily::Execute,
            ToolFamily::Think,
            ToolFamily::Fetch,
            ToolFamily::SwitchMode,
            ToolFamily::Plan,
            ToolFamily::Mcp,
            ToolFamily::Unknown,
        ] {
            assert_ascii(family_glyph(family).0);
        }

        let ok_payload = ObservePayload::Text {
            body: String::new(),
        };
        assert_ascii(outcome_glyph(&ok_payload).0);
        assert_ascii(observe_compact(&ok_payload).0);

        let err_payload = ObservePayload::Error {
            message: "boom".to_string(),
        };
        assert_ascii(outcome_glyph(&err_payload).0);
        assert_ascii(observe_compact(&err_payload).0);
    }

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
pub(crate) fn input_display_lines(input: &ToolInputDisplay) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    match input {
        ToolInputDisplay::Path(p) => {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(terminal_safe_text(p), Style::default().fg(Color::DarkGray)),
            ]));
        }
        ToolInputDisplay::Diff { path, diff } => {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    terminal_safe_text(path),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
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
                            format!("[... {} more]", remaining),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                    break;
                }
                let color = if dl.starts_with('+') {
                    Color::Green
                } else if dl.starts_with('-') {
                    Color::Red
                } else {
                    Color::DarkGray
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
                    Style::default().fg(Color::Magenta),
                ),
            ]));
            if let Some(cwd) = cwd {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("(cwd: {})", terminal_safe_text(cwd)),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
        ToolInputDisplay::Query(q) => {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(
                    terminal_safe_text(q),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        ToolInputDisplay::Json(p) => {
            for jl in p.lines().take(8) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(jl), Style::default().fg(Color::DarkGray)),
                ]));
            }
        }
        ToolInputDisplay::Text(t) => {
            for tl in t.lines() {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(tl), Style::default().fg(Color::White)),
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
    payload: &ObservePayload,
    collapsed: bool,
) -> Vec<Line<'static>> {
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
                Span::styled(exit_str, Style::default().fg(Color::Magenta)),
            ]));
            let stdout_limit = if collapsed { 8 } else { usize::MAX };
            let stderr_limit = if collapsed { 4 } else { usize::MAX };
            let stdout_total = stdout.lines().count();
            for sl in stdout.lines().take(stdout_limit) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(sl), Style::default().fg(Color::White)),
                ]));
            }
            if collapsed && stdout_total > stdout_limit {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!(
                            "[... {} more lines - Ctrl+O expand]",
                            stdout_total - stdout_limit
                        ),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
            let stderr_total = stderr.lines().count();
            for el in stderr.lines().take(stderr_limit) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(el), Style::default().fg(Color::Red)),
                ]));
            }
            if collapsed && stderr_total > stderr_limit {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("[... {} more lines]", stderr_total - stderr_limit),
                        Style::default().fg(Color::DarkGray),
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
                "{} - {} lines{}",
                path_str,
                line_count,
                if *truncated { " (truncated)" } else { "" }
            );
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(header, Style::default().fg(Color::Cyan)),
            ]));
            let limit = if collapsed { 8 } else { usize::MAX };
            for cl in content.lines().take(limit) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(cl), Style::default().fg(Color::White)),
                ]));
            }
            if collapsed && line_count > limit {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("[... {} more lines - Ctrl+O expand]", line_count - limit),
                        Style::default().fg(Color::DarkGray),
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
                    Span::styled(msg, Style::default().fg(Color::Yellow)),
                ]));
            } else if let Some(d) = diff {
                let limit = if collapsed { 6 } else { usize::MAX };
                let total = d.lines().count();
                for dl in d.lines().take(limit) {
                    let color = if dl.starts_with('+') {
                        Color::Green
                    } else if dl.starts_with('-') {
                        Color::Red
                    } else {
                        Color::DarkGray
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
                            format!("[... {} more lines]", total - limit),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]));
                }
            } else if let Some(p) = path {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(p), Style::default().fg(Color::DarkGray)),
                ]));
            }
        }
        ObservePayload::Json { pretty } => {
            let limit = if collapsed { 8 } else { usize::MAX };
            let total = pretty.lines().count();
            for jl in pretty.lines().take(limit) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(jl), Style::default().fg(Color::DarkGray)),
                ]));
            }
            if collapsed && total > limit {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("[... {} more lines - Ctrl+O expand]", total - limit),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
        ObservePayload::Text { body } => {
            let limit = if collapsed { 8 } else { usize::MAX };
            let total = body.lines().count();
            for tl in body.lines().take(limit) {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(terminal_safe_text(tl), Style::default().fg(Color::White)),
                ]));
            }
            if collapsed && total > limit {
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(
                        format!("[... {} more lines - Ctrl+O expand]", total - limit),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]));
            }
        }
        ObservePayload::Error { message } => {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(terminal_safe_text(message), Style::default().fg(Color::Red)),
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

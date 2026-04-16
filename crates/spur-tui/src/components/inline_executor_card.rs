//! Live executor card rendered inline in the brain conversation at
//! each delegate_to_worker call site. Pure render against
//! `ExecutorLineage`; no internal state. Reactivity comes from the
//! projection.
//!
//! Implements UX refinements R1 (focus indicator), R3 (stale colors +
//! spinner), R5 (update-flash, see app.rs trigger), R7 (attention-
//! state taller cards), R14 (per-state density).

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use spur_acp::domain::events::LifecycleState;
use spur_core::{ExecutorId, ExecutorLineage, ExecutorNode};

const TASK_TRUNCATE: usize = 60;

pub fn render_card(
    lineage: &ExecutorLineage,
    executor_id: &ExecutorId,
    focused: bool,
) -> Vec<Line<'static>> {
    let node = match lineage.node(executor_id) {
        Some(n) => n,
        None => return placeholder_card(executor_id, focused),
    };

    let phase = node.phase;
    let task = truncate(&node.task_spec, TASK_TRUNCATE);
    let agent = node.agent.clone();
    let id = executor_id.0.clone();

    let header = header_line(phase, &id, &agent, &task);

    let mut lines = vec![header];

    match phase {
        LifecycleState::Spawning | LifecycleState::Running | LifecycleState::Resuming => {
            lines.push(running_status_line(node));
            lines.push(running_diff_line(node));
        }
        LifecycleState::AwaitingReview => {
            lines.push(attention_header());
            lines.push(awaiting_status_line(node));
            lines.push(awaiting_summary_line(node));
            lines.push(awaiting_cta_line());
        }
        LifecycleState::Failed => {
            lines.push(attention_header_failed());
            lines.push(failed_status_line(node));
            lines.push(failed_cta_line());
        }
        LifecycleState::Succeeded => {
            lines.push(done_status_line(node));
        }
        LifecycleState::Cancelled => {
            lines.push(cancelled_status_line(node));
        }
    }

    if focused {
        lines.push(focus_hint_line(phase));
    }

    lines
}

fn placeholder_card(executor_id: &ExecutorId, focused: bool) -> Vec<Line<'static>> {
    let mut out = vec![Line::from(vec![
        Span::styled(
            format!("○ exec/{} ", short_id(&executor_id.0)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            "(spawning…)",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ),
    ])];
    if focused {
        out.push(Line::from(Span::styled(
            "  [ executor not yet spawned ]",
            Style::default().fg(Color::DarkGray),
        )));
    }
    out
}

fn header_line(phase: LifecycleState, id: &str, agent: &str, task: &str) -> Line<'static> {
    let (glyph, color) = phase_glyph(phase);
    Line::from(vec![
        Span::styled(
            format!("{glyph} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("exec/{} · ", short_id(id)),
            Style::default().fg(Color::White),
        ),
        Span::styled(format!("{agent} · "), Style::default().fg(Color::Cyan)),
        Span::styled(format!("\"{task}\""), Style::default().fg(Color::Gray)),
    ])
}

fn running_status_line(node: &ExecutorNode) -> Line<'static> {
    let elapsed = format_elapsed(node.elapsed_secs());
    let tool_count = node.tool_call_count;
    let last_tool = node.latest_tool_call.as_deref().unwrap_or("(none)");
    let stale_secs = node.seconds_since_last_event();
    let stale_color = stale_color_for(stale_secs);
    let spinner = if stale_secs.unwrap_or(u64::MAX) < 10 {
        spinner_glyph()
    } else {
        ' '
    };

    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("Running · {elapsed} · {tool_count} calls · last: {last_tool}"),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            format!(
                " · {} ago {spinner}",
                format_elapsed(stale_secs.unwrap_or(0))
            ),
            Style::default().fg(stale_color),
        ),
    ])
}

fn running_diff_line(node: &ExecutorNode) -> Line<'static> {
    let files = node.files_touched_count;
    let (ins, del) = node.diff_totals();
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("files: {files} · diff: +{ins}/-{del}"),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

fn attention_header() -> Line<'static> {
    Line::from(Span::styled(
        "┌─ ⚠ ATTENTION ──────────────────────────────────────────────────────",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))
}

fn attention_header_failed() -> Line<'static> {
    Line::from(Span::styled(
        "┌─ ✗ FAILED ─────────────────────────────────────────────────────────",
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    ))
}

fn awaiting_status_line(node: &ExecutorNode) -> Line<'static> {
    let elapsed = format_elapsed(node.elapsed_secs());
    let (ins, del) = node.diff_totals();
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!(
                "AwaitingReview · {elapsed} · diff: {} files, +{ins}/-{del}",
                node.files_touched_count
            ),
            Style::default().fg(Color::Yellow),
        ),
    ])
}

fn awaiting_summary_line(node: &ExecutorNode) -> Line<'static> {
    let summary = node
        .pending_review
        .as_ref()
        .map(|r| r.payload.summary.clone())
        .unwrap_or_default();
    Line::from(vec![
        Span::raw("  Worker summary: "),
        Span::styled(
            format!("\"{}\"", truncate(&summary, 70)),
            Style::default().fg(Color::Gray),
        ),
    ])
}

fn awaiting_cta_line() -> Line<'static> {
    Line::from(Span::styled(
        "  ⏳ Awaiting review — this delegation is blocking the brain",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    ))
}

fn failed_status_line(node: &ExecutorNode) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!(
                "Failed · {}",
                node.last_error.as_deref().unwrap_or("(no error message)")
            ),
            Style::default().fg(Color::Red),
        ),
    ])
}

fn failed_cta_line() -> Line<'static> {
    Line::from(Span::styled(
        "  ✗ Worker failed — brain will handle the error",
        Style::default().fg(Color::Red),
    ))
}

fn done_status_line(node: &ExecutorNode) -> Line<'static> {
    let elapsed = format_elapsed(node.elapsed_secs());
    let (ins, del) = node.diff_totals();
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!(
                "Done · {elapsed} · diff: {} files, +{ins}/-{del}",
                node.files_touched_count
            ),
            Style::default().fg(Color::Cyan),
        ),
    ])
}

fn cancelled_status_line(node: &ExecutorNode) -> Line<'static> {
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("Cancelled · {}", format_elapsed(node.elapsed_secs())),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

fn focus_hint_line(phase: LifecycleState) -> Line<'static> {
    let hint = match phase {
        LifecycleState::AwaitingReview => {
            "[ press 'r' to review · Enter / > to enter executor view ]"
        }
        LifecycleState::Failed | LifecycleState::Cancelled => "[ Enter / > to inspect events ]",
        _ => "[ Enter / > to open executor view · Tab for next ]",
    };
    Line::from(Span::styled(
        format!("  {hint}"),
        Style::default().fg(Color::Cyan),
    ))
}

pub(crate) fn phase_glyph(phase: LifecycleState) -> (char, Color) {
    match phase {
        LifecycleState::Spawning => ('○', Color::DarkGray),
        LifecycleState::Running | LifecycleState::Resuming => ('▶', Color::Green),
        LifecycleState::AwaitingReview => ('⚠', Color::Yellow),
        LifecycleState::Succeeded => ('✓', Color::Cyan),
        LifecycleState::Failed => ('✗', Color::Red),
        LifecycleState::Cancelled => ('💀', Color::DarkGray),
    }
}

fn stale_color_for(secs_since_last: Option<u64>) -> Color {
    match secs_since_last {
        Some(s) if s > 300 => Color::Red,
        Some(s) if s > 30 => Color::Yellow,
        _ => Color::DarkGray,
    }
}

fn spinner_glyph() -> char {
    let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let idx = (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() / 80)
        .unwrap_or(0)
        % frames.len() as u128) as usize;
    frames[idx]
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

pub(crate) fn short_id(id: &str) -> String {
    id.chars().take(4).collect()
}

pub(crate) fn format_elapsed(secs: u64) -> String {
    let m = secs / 60;
    let s = secs % 60;
    if m > 0 {
        format!("{m}m{s:02}s")
    } else {
        format!("{s}s")
    }
}

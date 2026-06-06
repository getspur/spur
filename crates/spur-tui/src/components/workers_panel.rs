//! Collapsible workers-status panel rendered between the ReactTrace and
//! InputBar in SessionDetailView. Shows one condensed line per active
//! worker delegation. Pure render against `ExecutorLineage`.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use spur_acp::domain::events::LifecycleState;
use spur_core::{ExecutorId, ExecutorLineage, ExecutorNode};

use super::focused_border_style;
use super::inline_executor_card::{format_elapsed, phase_glyph, short_id};

const MAX_VISIBLE: usize = 5;

/// Phases considered "active" (shown in the panel).
fn is_active(phase: LifecycleState) -> bool {
    matches!(
        phase,
        LifecycleState::Spawning
            | LifecycleState::Running
            | LifecycleState::Resuming
            | LifecycleState::AwaitingReview
    )
}

/// Compute the height the panel needs. Returns 0 when there are no active
/// workers (panel hidden), 1 when collapsed, or 2 + min(active, MAX_VISIBLE)
/// when expanded (border top/bottom + rows).
pub fn compute_height(lineage: &ExecutorLineage, executor_ids: &[String], collapsed: bool) -> u16 {
    let active = count_active(lineage, executor_ids);
    if active == 0 {
        return 0;
    }
    if collapsed {
        return 1;
    }
    // borders (2) + visible rows
    2 + active.min(MAX_VISIBLE) as u16
}

/// Render the workers panel into `area`.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    lineage: &ExecutorLineage,
    executor_ids: &[String],
    collapsed: bool,
) {
    render_focused(frame, area, lineage, executor_ids, collapsed, false);
}

pub fn render_focused(
    frame: &mut Frame,
    area: Rect,
    lineage: &ExecutorLineage,
    executor_ids: &[String],
    collapsed: bool,
    focused: bool,
) {
    let nodes: Vec<(&str, &ExecutorNode)> = executor_ids
        .iter()
        .filter_map(|id| {
            let node = lineage.node(&ExecutorId(id.clone()))?;
            is_active(node.phase).then_some((id.as_str(), node))
        })
        .collect();

    if nodes.is_empty() {
        return;
    }

    if collapsed {
        frame.render_widget(Paragraph::new(collapsed_line(&nodes, focused)), area);
        return;
    }

    let title = format!(" Workers ({}) ", nodes.len());
    let block = Block::default()
        .title(title)
        .title_bottom(" Alt+D collapse ")
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(focused_border_style(focused));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let visible = if nodes.len() > MAX_VISIBLE {
        &nodes[..MAX_VISIBLE - 1]
    } else {
        &nodes[..]
    };
    let mut lines: Vec<Line<'static>> = visible.iter().map(|(id, n)| worker_row(id, n)).collect();

    let overflow = nodes.len().saturating_sub(MAX_VISIBLE);
    if overflow > 0 {
        lines.push(Line::from(Span::styled(
            format!("  +{} more…", nodes.len() - (MAX_VISIBLE - 1)),
            Style::default().fg(Color::DarkGray),
        )));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

// ── helpers ─────────────────────────────────────────────────────────

fn count_active(lineage: &ExecutorLineage, executor_ids: &[String]) -> usize {
    executor_ids
        .iter()
        .filter(|id| {
            lineage
                .node(&ExecutorId(id.to_string()))
                .is_some_and(|n| is_active(n.phase))
        })
        .count()
}

fn worker_row(id: &str, node: &ExecutorNode) -> Line<'static> {
    let (glyph, color) = phase_glyph(node.phase);
    let elapsed = format_elapsed(node.elapsed_secs());
    let cost: f64 = node.attempts.iter().map(|a| a.cost_usd).sum();
    let (ins, del) = node.diff_totals();

    let mut spans = vec![
        Span::styled(
            format!(" {glyph} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{}/", short_id(id)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:<14}", node.agent),
            Style::default().fg(Color::Cyan),
        ),
        Span::styled(
            format!("{:<14}", phase_label(node.phase)),
            Style::default().fg(color),
        ),
        Span::styled(format!("{:<8}", elapsed), Style::default().fg(Color::White)),
    ];

    if cost > 0.0 {
        spans.push(Span::styled(
            format!("${:.2}  ", cost),
            Style::default().fg(Color::Yellow),
        ));
    }

    if ins > 0 || del > 0 {
        spans.push(Span::styled(
            format!("+{}/-{}", ins, del),
            Style::default().fg(Color::DarkGray),
        ));
    }

    if let Some(progress) = progress_summary(node) {
        spans.push(Span::styled(
            format!("  {}", progress),
            Style::default().fg(Color::LightGreen),
        ));
    }

    Line::from(spans)
}

fn progress_summary(node: &ExecutorNode) -> Option<String> {
    let message = node.latest_progress_message.as_deref()?;
    match node.latest_progress_percent {
        Some(percent) => Some(format!("{}% {}", percent, message)),
        None => Some(message.to_string()),
    }
}

fn phase_label(phase: LifecycleState) -> &'static str {
    match phase {
        LifecycleState::Spawning => "Spawning",
        LifecycleState::Running | LifecycleState::Resuming => "Running",
        LifecycleState::AwaitingReview => "Review",
        LifecycleState::Succeeded => "Done",
        LifecycleState::Failed => "Failed",
        LifecycleState::Cancelled => "Cancelled",
    }
}

fn collapsed_line(nodes: &[(&str, &ExecutorNode)], focused: bool) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        format!(" ▸ Workers ({}): ", nodes.len()),
        focused_border_style(focused).add_modifier(Modifier::BOLD),
    )];

    for (i, (_id, node)) in nodes.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(", ", Style::default().fg(Color::DarkGray)));
        }
        let (glyph, color) = phase_glyph(node.phase);
        spans.push(Span::styled(
            format!("{} {glyph}", node.agent),
            Style::default().fg(color),
        ));
    }

    spans.push(Span::styled(
        " — Alt+D",
        Style::default().fg(Color::DarkGray),
    ));

    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};
    use std::path::PathBuf;

    fn buffer_text(buf: &ratatui::buffer::Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn worker_row_renders_latest_progress_percent_and_message() {
        let mut lineage = ExecutorLineage::new();
        lineage.apply(&SpurEvent::now(SpurEventBody::BrainSpawned {
            agent: "brain".into(),
            session: SessionId("brain-1".into()),
        }));
        lineage.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
            agent: "codex".into(),
            session: SessionId("worker-1".into()),
            worktree: PathBuf::from("/tmp/wt"),
        }));
        lineage.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
            from: SessionId("brain-1".into()),
            to_agent: "codex".into(),
            task: "task".into(),
            request_id: "deleg-1".into(),
            delegation_plan: None,
            issue_id: None,
        }));
        lineage.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
            from: SessionId("brain-1".into()),
            request_id: "deleg-1".into(),
            executor_id: "worker-1".into(),
        }));
        lineage.apply(&SpurEvent::now(SpurEventBody::WorkerReportProgress {
            delegation_id: "deleg-1".into(),
            message: "Running targeted verification".into(),
            percent: Some(67.5),
        }));

        let mut terminal = Terminal::new(TestBackend::new(96, 4)).unwrap();
        terminal
            .draw(|frame| {
                render_focused(
                    frame,
                    Rect::new(0, 0, 96, 4),
                    &lineage,
                    &["worker-1".to_string()],
                    false,
                    false,
                );
            })
            .unwrap();

        let rendered = buffer_text(terminal.backend().buffer());
        assert!(
            rendered.contains("67.5%"),
            "progress percent missing:\n{rendered}"
        );
        assert!(
            rendered.contains("Running targeted verification"),
            "progress message missing:\n{rendered}"
        );
    }
}

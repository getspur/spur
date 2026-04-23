use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use std::time::SystemTime;

use spur_core::{ExecutorLineage, ExecutorNode, LifecycleState, TrackedPlan, TrackedTask};

pub fn render_stage_board(
    frame: &mut Frame,
    area: Rect,
    plan: &TrackedPlan,
    selected_task_id: &str,
    selected_stage_idx: usize,
    lineage: &ExecutorLineage,
) {
    let stage_count = max_stage(plan) + 1;
    let constraints = vec![Constraint::Ratio(1, stage_count as u32); stage_count];
    let areas = Layout::horizontal(constraints).split(area);

    for (stage_idx, stage_area) in areas.iter().enumerate() {
        let tasks = tasks_in_stage(plan, stage_idx);
        let mut lines = Vec::new();

        if tasks.is_empty() {
            lines.push(Line::from("(empty)"));
        } else {
            for task in tasks {
                let selected = task.task_id == selected_task_id;
                let title_line = vec![
                    Span::styled(
                        if selected { "▶ " } else { "  " },
                        if selected {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                        },
                    ),
                    Span::styled(status_badge(&task.status), status_style(&task.status)),
                    Span::raw(" "),
                    Span::raw(task.task_name.clone()),
                ];
                lines.push(Line::from(title_line));

                let meta = task_meta_chips(task, lineage);
                if !meta.is_empty() {
                    let mut meta_line = vec![Span::raw("    ")];
                    for (i, span) in meta.into_iter().enumerate() {
                        if i > 0 {
                            meta_line.push(Span::raw("  "));
                        }
                        meta_line.push(span);
                    }
                    lines.push(Line::from(meta_line));
                }
                lines.push(Line::raw(""));
            }
        }

        let is_active_stage = stage_idx == selected_stage_idx;
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!("Stage {}", stage_idx))
            .border_style(if is_active_stage {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            })
            .title_style(if is_active_stage {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            });
        frame.render_widget(Paragraph::new(lines).block(block), *stage_area);
    }
}

pub fn render_stacked_stage_groups(
    frame: &mut Frame,
    area: Rect,
    plan: &TrackedPlan,
    selected_task_id: &str,
    lineage: &ExecutorLineage,
) {
    let mut lines = Vec::new();
    for stage_idx in 0..=max_stage(plan) {
        lines.push(Line::from(Span::styled(
            format!("Stage {}", stage_idx),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for task in tasks_in_stage(plan, stage_idx) {
            let selected = task.task_id == selected_task_id;
            let mut text = format!(
                "{}{} {}",
                if selected { "▶ " } else { "  " },
                status_badge(&task.status),
                task.task_name
            );
            let meta = task_meta_chips(task, lineage);
            if !meta.is_empty() {
                text.push_str("  ");
                for (i, span) in meta.into_iter().enumerate() {
                    if i > 0 {
                        text.push_str("  ");
                    }
                    text.push_str(&span.content);
                }
            }
            lines.push(Line::from(text));
        }
        lines.push(Line::raw(""));
    }

    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Plan board")),
        area,
    );
}

pub(crate) fn stage_grouped_tasks(plan: &TrackedPlan) -> Vec<&TrackedTask> {
    let mut ordered = Vec::new();
    for stage_idx in 0..=max_stage(plan) {
        ordered.extend(tasks_in_stage(plan, stage_idx));
    }
    ordered
}

pub fn preferred_live_node<'a>(
    lineage: &'a ExecutorLineage,
    issue_id: &str,
) -> Option<&'a ExecutorNode> {
    lineage
        .nodes_for_issue(issue_id)
        .into_iter()
        .max_by_key(|node| {
            (
                is_live_phase(node.phase),
                node.last_event_at
                    .or_else(|| node.current_attempt().map(|attempt| attempt.started_at))
                    .unwrap_or(SystemTime::UNIX_EPOCH),
                node.id.0.clone(),
            )
        })
}

fn max_stage(plan: &TrackedPlan) -> usize {
    plan.tasks
        .iter()
        .map(|task| task.stage_idx)
        .max()
        .unwrap_or(0)
}

fn tasks_in_stage(plan: &TrackedPlan, stage_idx: usize) -> Vec<&TrackedTask> {
    plan.tasks
        .iter()
        .filter(|task| task.stage_idx == stage_idx)
        .collect()
}

fn status_badge(status: &str) -> &str {
    match status {
        "pending" => "[QUE]",
        "ready" => "[RDY]",
        "dispatched" => "[RUN]",
        "awaiting_review" => "[REV]",
        "approved" => "[PAS]",
        "rejected" => "[REJ]",
        "failed" => "[ERR]",
        "cancelled" => "[SKP]",
        "superseded" => "[SUP]",
        _ => "[???]",
    }
}

fn status_style(status: &str) -> Style {
    match status {
        "pending" | "ready" => Style::default().fg(Color::DarkGray),
        "dispatched" => Style::default()
            .fg(Color::Blue)
            .add_modifier(Modifier::BOLD),
        "awaiting_review" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        "approved" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "rejected" | "failed" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "cancelled" => Style::default().fg(Color::Magenta),
        "superseded" => Style::default().fg(Color::Yellow),
        _ => Style::default(),
    }
}

fn task_meta_chips<'a>(task: &TrackedTask, lineage: &ExecutorLineage) -> Vec<Span<'a>> {
    let mut chips = Vec::new();

    // Live worker link (agent + phase)
    if let Some(issue_id) = task.issue_id.as_deref() {
        if let Some(node) = preferred_live_node(lineage, issue_id) {
            chips.push(Span::styled(
                format!("{}:{}", node.agent, phase_label(node.phase)),
                Style::default().fg(Color::Cyan),
            ));
        }
    }

    // Blocked indicator
    if !task.blocked_by.is_empty() {
        let label = if task.blocked_by.len() == 1 {
            format!("blocked:{}", task.blocked_by[0])
        } else {
            format!("blocked:{} deps", task.blocked_by.len())
        };
        chips.push(Span::styled(label, Style::default().fg(Color::Red)));
    }

    // Dependency hint
    if !task.depends_on.is_empty() {
        let label = if task.depends_on.len() == 1 {
            format!("↑{}", task.depends_on[0])
        } else {
            format!("↑{} deps", task.depends_on.len())
        };
        chips.push(Span::styled(label, Style::default().fg(Color::DarkGray)));
    }

    // Attempt / retry indicator
    if task.attempt > 1 {
        chips.push(Span::styled(
            format!("retry {}/{}", task.attempt, task.max_attempts),
            Style::default().fg(Color::Magenta),
        ));
    }

    chips
}

fn is_live_phase(phase: LifecycleState) -> bool {
    !matches!(
        phase,
        LifecycleState::Succeeded | LifecycleState::Failed | LifecycleState::Cancelled
    )
}

fn phase_label(phase: LifecycleState) -> &'static str {
    match phase {
        LifecycleState::Spawning => "spawn",
        LifecycleState::Running => "run",
        LifecycleState::AwaitingReview => "review",
        LifecycleState::Resuming => "resume",
        LifecycleState::Succeeded => "done",
        LifecycleState::Failed => "fail",
        LifecycleState::Cancelled => "stop",
    }
}

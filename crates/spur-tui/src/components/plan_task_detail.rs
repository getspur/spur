use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use spur_core::{ExecutorNode, TrackedTask};

pub fn render_task_detail(
    frame: &mut Frame,
    area: Rect,
    task: &TrackedTask,
    live_node: Option<&ExecutorNode>,
) {
    let mut lines = vec![
        section_header("Identity"),
        kv("task", &task.task_id),
        kv("name", &task.task_name),
        kv("status", &task.status),
    ];
    if let Some(issue_id) = task.issue_id.as_deref() {
        lines.push(kv("issue", issue_id));
    }

    // ── Execution ─────────────────────────────────────────────────────────
    lines.push(section_header("Execution"));
    lines.push(kv("agent", &task.agent));
    lines.push(kv(
        "attempt",
        &format!("{}/{}", task.attempt, task.max_attempts),
    ));
    if let Some(node) = live_node {
        lines.push(kv(
            "live",
            &format!("{} {:?}", node.agent, node.phase).to_lowercase(),
        ));
    }
    if let Some(branch) = task.worker_branch.as_deref() {
        lines.push(kv("branch", branch));
    }
    if let Some(delegation_id) = task.delegation_id.as_deref() {
        lines.push(kv("delegation", delegation_id));
    }

    // ── Dependencies ──────────────────────────────────────────────────────
    if !task.depends_on.is_empty() || !task.blocked_by.is_empty() || !task.unblocks.is_empty() {
        lines.push(section_header("Dependencies"));
        if !task.depends_on.is_empty() {
            lines.push(kv("depends_on", &task.depends_on.join(", ")));
        }
        if !task.blocked_by.is_empty() {
            lines.push(Line::from(vec![
                Span::styled(
                    "blocked_by: ",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::styled(task.blocked_by.join(", "), Style::default().fg(Color::Red)),
            ]));
        }
        if !task.unblocks.is_empty() {
            lines.push(kv("unblocks", &task.unblocks.join(", ")));
        }
    }

    // ── Output ────────────────────────────────────────────────────────────
    if task.summary.is_some()
        || task.feedback.is_some()
        || task.error.is_some()
        || task.diff_summary.is_some()
        || task.mutation_id.is_some()
        || !task.superseded_by.is_empty()
        || !task.next_action.is_empty()
    {
        lines.push(section_header("Output"));
        if let Some(summary) = task.summary.as_deref() {
            lines.push(kv("summary", summary));
        }
        if let Some(feedback) = task.feedback.as_deref() {
            lines.push(kv("feedback", feedback));
        }
        if let Some(error) = task.error.as_deref() {
            lines.push(kv("error", error));
        }
        if let Some(diff_summary) = task.diff_summary.as_ref() {
            lines.push(kv(
                "diff",
                &format!(
                    "{} files +{}/-{}",
                    diff_summary.files_changed, diff_summary.insertions, diff_summary.deletions
                ),
            ));
        }
        if let Some(mutation_id) = task.mutation_id.as_deref() {
            lines.push(kv("mutation", mutation_id));
        }
        if !task.superseded_by.is_empty() {
            lines.push(kv("superseded_by", &task.superseded_by.join(", ")));
        }
        if !task.next_action.is_empty() {
            lines.push(kv("next", &task.next_action));
        }
    }

    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Task detail")),
        area,
    );
}

fn section_header(title: &str) -> Line<'static> {
    Line::from(vec![
        Span::raw(""),
        Span::styled(
            format!("━━ {title} "),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("━".repeat(20), Style::default().fg(Color::DarkGray)),
    ])
}

fn kv(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(value.to_string()),
    ])
}

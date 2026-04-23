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
        kv("task", &task.task_id),
        kv("name", &task.task_name),
        kv("status", &task.status),
        kv("agent", &task.agent),
        kv(
            "attempt",
            &format!("{}/{}", task.attempt, task.max_attempts),
        ),
    ];

    if let Some(issue_id) = task.issue_id.as_deref() {
        lines.push(kv("issue", issue_id));
    }
    if !task.depends_on.is_empty() {
        lines.push(kv("depends_on", &task.depends_on.join(", ")));
    }
    if !task.unblocks.is_empty() {
        lines.push(kv("unblocks", &task.unblocks.join(", ")));
    }
    if !task.blocked_by.is_empty() {
        lines.push(kv("blocked_by", &task.blocked_by.join(", ")));
    }
    if let Some(summary) = task.summary.as_deref() {
        lines.push(kv("summary", summary));
    }
    if let Some(feedback) = task.feedback.as_deref() {
        lines.push(kv("feedback", feedback));
    }
    if let Some(error) = task.error.as_deref() {
        lines.push(kv("error", error));
    }
    if let Some(branch) = task.worker_branch.as_deref() {
        lines.push(kv("branch", branch));
    }
    if let Some(delegation_id) = task.delegation_id.as_deref() {
        lines.push(kv("delegation", delegation_id));
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
    if let Some(node) = live_node {
        lines.push(kv(
            "live",
            &format!("{} {:?}", node.agent, node.phase).to_lowercase(),
        ));
    }

    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Task detail")),
        area,
    );
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

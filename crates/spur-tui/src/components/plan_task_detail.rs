use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use spur_core::{ExecutorNode, TrackedTask};
use spur_pm::Issue;

pub fn render_task_detail(
    frame: &mut Frame,
    area: Rect,
    task: &TrackedTask,
    live_node: Option<&ExecutorNode>,
    issue_detail: Option<&Issue>,
    issue_detail_status: Option<&str>,
    scroll_offset: usize,
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

    // ── Issue detail (optional) ────────────────────────────────────────────
    if let Some(status) = issue_detail_status {
        lines.push(section_header("Issue"));
        lines.push(kv("status", status));
    }
    if let Some(issue) = issue_detail {
        lines.push(section_header("Issue"));
        lines.push(kv("id", &issue.id));
        lines.push(kv("title", &issue.title));
        lines.push(kv(
            "source",
            match issue.source {
                spur_pm::PmSource::GitHub => "github",
                spur_pm::PmSource::Linear => "linear",
                spur_pm::PmSource::Plane => "plane",
                _ => "beads",
            },
        ));
        lines.push(kv("status", &issue.status));
        lines.push(kv(
            "priority",
            &issue
                .priority
                .map(|priority| format!("P{priority}"))
                .unwrap_or_else(|| "--".to_string()),
        ));
        lines.push(kv("type", issue.issue_type.as_deref().unwrap_or("--")));
        lines.push(kv(
            "assignee",
            issue.assignee.as_deref().unwrap_or("unassigned"),
        ));
        lines.push(kv(
            "due",
            &issue
                .due_at
                .map(|due_at| due_at.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "--".to_string()),
        ));
        lines.push(kv(
            "created",
            &issue
                .created_at
                .map(|created_at| created_at.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "--".to_string()),
        ));
        lines.push(kv(
            "updated",
            &issue
                .updated_at
                .map(|updated_at| updated_at.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "--".to_string()),
        ));
        if !issue.labels.is_empty() {
            lines.push(kv("labels", &issue.labels.join(", ")));
        }
        if !issue.blocked_by.is_empty() {
            lines.push(kv("blocked_by", &issue.blocked_by.join(", ")));
        }
        if !issue.url.is_empty() {
            lines.push(kv("url", &issue.url));
        }

        lines.push(section_header("Description"));
        if issue.body.is_empty() {
            lines.push(kv("body", "(empty)"));
        } else {
            for line in issue.body.lines() {
                lines.push(Line::from(vec![Span::raw(line.to_string())]));
            }
        }
    }

    let visible_lines = area.height.saturating_sub(2) as usize;
    let total_lines = lines.len();
    let mut scroll_offset = match u16::try_from(scroll_offset) {
        Ok(offset) => offset,
        Err(_) => u16::MAX,
    };

    if visible_lines > 0 && total_lines > visible_lines {
        let max_scroll = total_lines - visible_lines;
        scroll_offset = scroll_offset.min(max_scroll as u16);
    } else {
        scroll_offset = 0;
    }

    let title = if visible_lines == 0 || total_lines <= visible_lines {
        "Task detail".to_string()
    } else {
        let start_line = (scroll_offset as usize).saturating_add(1);
        let end_line = ((scroll_offset as usize).saturating_add(visible_lines)).min(total_lines);
        let max_scroll = total_lines - visible_lines;
        let percent = if max_scroll == 0 {
            100
        } else {
            ((scroll_offset as usize).saturating_mul(100)) / max_scroll
        };
        format!("Task detail ({start_line}-{end_line}/{total_lines}) {percent}%")
    };

    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll_offset, 0))
            .block(Block::default().borders(Borders::ALL).title(title)),
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

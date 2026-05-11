use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};
use spur_core::{ExecutorNode, TrackedPlan, TrackedTask};
use spur_pm::Issue;

use crate::theme::{resolve_token, ColorDepth, Theme};

pub fn render_task_detail(
    frame: &mut Frame,
    area: Rect,
    plan: &TrackedPlan,
    task: &TrackedTask,
    live_node: Option<&ExecutorNode>,
    issue_detail: Option<&Issue>,
    issue_detail_status: Option<&str>,
    scroll_offset: usize,
    theme: &Theme,
) {
    let content_width = area.width.saturating_sub(2) as usize;
    let mut lines = vec![hero_line(task, live_node, theme)];
    if task_is_blocked(task) {
        lines.push(blocked_banner(task, plan, content_width, theme));
    }
    lines.extend(dependency_strip(task, plan, content_width, theme));
    lines.push(execution_strip(task, theme));
    lines.push(kv(theme, "task", &task.task_id));
    if let Some(issue_id) = task.issue_id.as_deref() {
        lines.push(kv(theme, "issue", issue_id));
    }
    lines.push(kv(theme, "status", &task.status));

    if task.summary.is_some()
        || task.feedback.is_some()
        || task.error.is_some()
        || task.diff_summary.is_some()
        || task.mutation_id.is_some()
        || !task.superseded_by.is_empty()
        || !task.next_action.is_empty()
    {
        lines.push(section_header("v Output", theme));
        if let Some(summary) = task.summary.as_deref() {
            lines.push(kv(theme, "summary", summary));
        }
        if let Some(feedback) = task.feedback.as_deref() {
            lines.push(kv(theme, "feedback", feedback));
        }
        if let Some(error) = task.error.as_deref() {
            lines.push(kv(theme, "error", error));
        }
        if let Some(diff_summary) = task.diff_summary.as_ref() {
            lines.push(kv(
                theme,
                "diff",
                &format!(
                    "{} files +{}/-{}",
                    diff_summary.files_changed, diff_summary.insertions, diff_summary.deletions
                ),
            ));
        }
        if let Some(mutation_id) = task.mutation_id.as_deref() {
            lines.push(kv(theme, "mutation", mutation_id));
        }
        if !task.superseded_by.is_empty() {
            lines.push(kv(theme, "superseded_by", &task.superseded_by.join(", ")));
        }
        if !task.next_action.is_empty() {
            lines.push(kv(theme, "next", &task.next_action));
        }
    }

    if let Some(status) = issue_detail_status {
        lines.push(section_header("v Issue", theme));
        lines.push(kv(theme, "status", status));
    }
    if let Some(issue) = issue_detail {
        lines.push(section_header("v Issue", theme));
        lines.push(kv(theme, "id", &issue.id));
        lines.push(kv(theme, "title", &issue.title));
        lines.push(kv(
            theme,
            "source",
            match issue.source {
                spur_pm::PmSource::GitHub => "github",
                spur_pm::PmSource::Linear => "linear",
                spur_pm::PmSource::Plane => "plane",
                _ => "beads",
            },
        ));
        lines.push(kv(theme, "status", &issue.status));
        lines.push(kv(
            theme,
            "priority",
            &issue
                .priority
                .map(|priority| format!("P{priority}"))
                .unwrap_or_else(|| "--".to_string()),
        ));
        lines.push(kv(
            theme,
            "type",
            issue.issue_type.as_deref().unwrap_or("--"),
        ));
        lines.push(kv(
            theme,
            "assignee",
            issue.assignee.as_deref().unwrap_or("unassigned"),
        ));
        lines.push(kv(
            theme,
            "due",
            &issue
                .due_at
                .map(|due_at| due_at.format("%Y-%m-%d").to_string())
                .unwrap_or_else(|| "--".to_string()),
        ));
        lines.push(kv(
            theme,
            "created",
            &issue.created_at.format("%Y-%m-%d").to_string(),
        ));
        lines.push(kv(
            theme,
            "updated",
            &issue.updated_at.format("%Y-%m-%d").to_string(),
        ));
        if !issue.labels.is_empty() {
            lines.push(kv(theme, "labels", &issue.labels.join(", ")));
        }
        if !issue.blocked_by.is_empty() {
            lines.push(kv(theme, "blocked_by", &issue.blocked_by.join(", ")));
        }
        if !issue.url.is_empty() {
            lines.push(kv(theme, "url", &issue.url));
        }

        lines.push(section_header("Description", theme));
        if issue.body.is_empty() {
            lines.push(kv(theme, "body", "(empty)"));
        } else {
            for line in issue.body.lines() {
                lines.push(Line::from(vec![Span::raw(line.to_string())]));
            }
        }
    }

    let visible_lines = area.height.saturating_sub(2) as usize;
    let total_lines = lines.len();
    let mut scroll_offset = u16::try_from(scroll_offset).unwrap_or(u16::MAX);

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

fn token(theme: &Theme, name: &str) -> ratatui::style::Color {
    resolve_token(theme, name, ColorDepth::Truecolor)
}

fn hero_line(task: &TrackedTask, live_node: Option<&ExecutorNode>, theme: &Theme) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            task.task_name.clone(),
            Style::default()
                .fg(token(theme, "plan_inspector.detail.hero.fg"))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" · "),
        Span::styled(
            task.agent.clone(),
            Style::default().fg(token(theme, "plan_inspector.detail.hero.agent.fg")),
        ),
    ];
    if let Some(node) = live_node {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            format!("live {} {:?}", node.agent, node.phase).to_lowercase(),
            Style::default().fg(token(theme, "plan_inspector.chip.live.fg")),
        ));
    }
    Line::from(spans)
}

fn task_is_blocked(task: &TrackedTask) -> bool {
    task.status == "blocked" || !task.blocked_by.is_empty()
}

fn blocked_banner(
    task: &TrackedTask,
    plan: &TrackedPlan,
    content_width: usize,
    theme: &Theme,
) -> Line<'static> {
    let refs = if task.blocked_by.is_empty() {
        task.status.clone()
    } else {
        task.blocked_by
            .iter()
            .map(|id| task_ref(id, "↑", plan))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut text = format!(" BLOCKED {refs}");
    if content_width > text.len() {
        text.push_str(&" ".repeat(content_width - text.len()));
    }
    Line::from(vec![Span::styled(
        text,
        Style::default()
            .fg(token(theme, "plan_inspector.detail.blocked_banner.fg"))
            .bg(token(theme, "plan_inspector.detail.blocked_banner.bg"))
            .add_modifier(Modifier::BOLD),
    )])
}

fn dependency_strip(
    task: &TrackedTask,
    plan: &TrackedPlan,
    content_width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if task.depends_on.is_empty() && task.unblocks.is_empty() && task.blocked_by.is_empty() {
        return Vec::new();
    }

    let parents = dependency_column("Parents", "↑", &task.depends_on, plan);
    let children = dependency_column("Children", "→", &task.unblocks, plan);
    let blocked_by = dependency_column("Blocked by", "↑", &task.blocked_by, plan);

    if content_width < 40 {
        return vec![
            dep_line("Parents", &parents, false, theme),
            dep_line("Children", &children, false, theme),
            dep_line("Blocked by", &blocked_by, true, theme),
        ];
    }

    let col_width = (content_width / 3).max(1);
    vec![Line::from(vec![
        Span::styled(
            pad_column(&format!("Parents {parents}"), col_width),
            Style::default().fg(token(theme, "plan_inspector.detail.edge.structural.fg")),
        ),
        Span::styled(
            pad_column(&format!("Children {children}"), col_width),
            Style::default().fg(token(theme, "plan_inspector.detail.edge.highlight.fg")),
        ),
        Span::styled(
            truncate_to_width(&format!("Blocked by {blocked_by}"), col_width),
            Style::default().fg(token(theme, "plan_inspector.detail.edge.blocked.fg")),
        ),
    ])]
}

fn dependency_column(_label: &str, arrow: &str, ids: &[String], plan: &TrackedPlan) -> String {
    if ids.is_empty() {
        return "--".to_string();
    }
    ids.iter()
        .map(|id| task_ref(id, arrow, plan))
        .collect::<Vec<_>>()
        .join(", ")
}

fn task_ref(id: &str, arrow: &str, plan: &TrackedPlan) -> String {
    match plan.task(id) {
        Some(task) => format!("{arrow}{id} ({})", task.status),
        None => format!("{arrow}{id}"),
    }
}

fn dep_line(label: &'static str, value: &str, blocked: bool, theme: &Theme) -> Line<'static> {
    let token_name = if blocked {
        "plan_inspector.detail.edge.blocked.fg"
    } else {
        "plan_inspector.detail.edge.structural.fg"
    };
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(token(theme, "plan_inspector.detail.label.fg"))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            value.to_string(),
            Style::default().fg(token(theme, token_name)),
        ),
    ])
}

fn execution_strip(task: &TrackedTask, theme: &Theme) -> Line<'static> {
    let mut fields = vec![
        format!("status {}", task.status),
        format!("attempt {}/{}", task.attempt, task.max_attempts),
    ];
    if let Some(branch) = task.worker_branch.as_deref() {
        fields.push(format!("branch {branch}"));
    }
    if let Some(delegation_id) = task.delegation_id.as_deref() {
        fields.push(format!("delegation {delegation_id}"));
    }
    Line::from(vec![Span::styled(
        fields.join(" · "),
        Style::default().fg(token(theme, "plan_inspector.detail.section.fg")),
    )])
}

fn pad_column(value: &str, width: usize) -> String {
    let truncated = truncate_to_width(value, width);
    let len = truncated.chars().count();
    if len >= width {
        truncated
    } else {
        format!("{truncated}{}", " ".repeat(width - len))
    }
}

fn truncate_to_width(value: &str, width: usize) -> String {
    value.chars().take(width).collect()
}

fn section_header(title: &str, theme: &Theme) -> Line<'static> {
    Line::from(vec![
        Span::raw(""),
        Span::styled(
            format!("-- {title} "),
            Style::default()
                .fg(token(theme, "plan_inspector.detail.section.fg"))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "-".repeat(20),
            Style::default().fg(token(theme, "plan_inspector.detail.section.fg")),
        ),
    ])
}

fn kv(theme: &Theme, label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label}: "),
            Style::default()
                .fg(token(theme, "plan_inspector.detail.label.fg"))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(value.to_string()),
    ])
}

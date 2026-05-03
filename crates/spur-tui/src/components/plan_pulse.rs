use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use spur_core::{TrackedPlan, TrackedTask};

const POTENTIAL_CLOBBER_LABEL: &str = "signal:potential-clobber";
const CLOBBER_BADGE_LABEL: &str = "⚠ CLOBBER";
const CLOBBER_BADGE_TEXT: &str = " ⚠ CLOBBER ";

pub fn pulse_text(plan: &TrackedPlan) -> String {
    let mut text = base_pulse_text(plan);
    if plan_has_potential_clobber_signal(plan) {
        text.push_str(" | ");
        text.push_str(CLOBBER_BADGE_LABEL);
    }
    text
}

pub fn render(frame: &mut Frame, area: Rect, plan: &TrackedPlan) {
    let line = pulse_line(plan);
    frame.render_widget(Paragraph::new(line).right_aligned(), area);
}

fn base_pulse_text(plan: &TrackedPlan) -> String {
    format!(
        "plan {} {} | {} | rv:{} fl:{} | next:{} | Alt+P",
        plan.plan_id,
        short_status(&plan.status),
        compact_progress(plan),
        plan.counts.awaiting_review,
        plan.counts.failed + plan.counts.rejected + plan.counts.cancelled,
        short_next_action(&plan.next_action),
    )
}

fn pulse_line(plan: &TrackedPlan) -> Line<'static> {
    let pulse_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(base_pulse_text(plan), pulse_style)];
    if plan_has_potential_clobber_signal(plan) {
        spans.push(Span::styled(" |", pulse_style));
        spans.push(Span::styled(
            CLOBBER_BADGE_TEXT,
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ));
    }
    Line::from(spans)
}

fn short_status(status: &str) -> &str {
    match status {
        "running" => "run",
        "approved" => "done",
        "rejected" => "rej",
        "failed" => "fail",
        "cancelled" => "skip",
        other => other,
    }
}

fn compact_progress(plan: &TrackedPlan) -> String {
    let total = plan.counts.pending
        + plan.counts.ready
        + plan.counts.dispatched
        + plan.counts.awaiting_review
        + plan.counts.approved
        + plan.counts.rejected
        + plan.counts.failed
        + plan.counts.cancelled;
    let completed =
        plan.counts.approved + plan.counts.rejected + plan.counts.failed + plan.counts.cancelled;
    format!("{completed}/{total}")
}

fn short_next_action(next_action: &str) -> &'static str {
    if next_action.is_empty() || next_action.contains("Workers still running") {
        "wait"
    } else if next_action.contains("get_task_diff") || next_action.contains("review_task") {
        "review"
    } else if next_action.contains("merge_plan") {
        "merge"
    } else if next_action.contains("create_pr") {
        "pr"
    } else if next_action.contains("failed") {
        "inspect"
    } else if next_action.contains("rejected") {
        "revise"
    } else {
        "wait"
    }
}

fn plan_has_potential_clobber_signal(plan: &TrackedPlan) -> bool {
    plan.tasks.iter().any(task_has_potential_clobber_signal)
}

fn task_has_potential_clobber_signal(task: &TrackedTask) -> bool {
    // Plan snapshots do not currently model issue labels directly; surface
    // the signal when the label text is carried in existing task metadata.
    [
        task.summary.as_deref(),
        task.feedback.as_deref(),
        task.error.as_deref(),
        Some(task.next_action.as_str()),
    ]
    .into_iter()
    .flatten()
    .any(contains_potential_clobber_label)
}

fn contains_potential_clobber_label(value: &str) -> bool {
    value.split_whitespace().any(|token| {
        token.trim_matches(|ch: char| matches!(ch, ',' | ';' | '.' | '[' | ']' | '(' | ')' | '"'))
            == POTENTIAL_CLOBBER_LABEL
    })
}

#[cfg(test)]
mod tests {
    use ratatui::{backend::TestBackend, style::Color, style::Style, Terminal};
    use spur_acp::{PlanSnapshotCounts, SessionId};
    use spur_core::{TrackedPlan, TrackedTask};

    use super::{pulse_text, render};

    fn sample_plan(next_action: &str) -> TrackedPlan {
        TrackedPlan {
            session_id: SessionId("brain-1".into()),
            plan_id: "p-123".into(),
            epic_id: None,
            status: "running".into(),
            progress: "1/3 reviewed, 1 running, 1 pending".into(),
            next_action: next_action.into(),
            ready_to_merge: false,
            owner_brain_session_id: None,
            counts: PlanSnapshotCounts {
                pending: 1,
                dispatched: 1,
                awaiting_review: 1,
                ..Default::default()
            },
            tasks: Vec::new(),
            updated_at: std::time::SystemTime::UNIX_EPOCH,
        }
    }

    fn sample_task_with_potential_clobber_label() -> TrackedTask {
        TrackedTask {
            task_id: "T2".into(),
            task_name: "overlay projection".into(),
            agent: "codex".into(),
            issue_id: Some("bd-1dwm.2".into()),
            status: "approved".into(),
            attempt: 1,
            max_attempts: 3,
            depends_on: Vec::new(),
            blocked_by: Vec::new(),
            unblocks: Vec::new(),
            summary: Some("labels: signal:potential-clobber".into()),
            feedback: None,
            error: None,
            worker_branch: None,
            delegation_id: None,
            diff_summary: None,
            mutation_id: None,
            superseded_by: Vec::new(),
            next_action: "wait".into(),
            stage_idx: 0,
        }
    }

    fn rendered_buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut rendered = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                rendered.push_str(buf[(x, y)].symbol());
            }
            rendered.push('\n');
        }
        rendered
    }

    fn style_for_cell_run(terminal: &Terminal<TestBackend>, needle: &str) -> Option<Style> {
        let buf = terminal.backend().buffer();
        for y in 0..buf.area.height {
            let mut row = String::new();
            let mut cell_x_by_byte = Vec::new();
            for x in 0..buf.area.width {
                cell_x_by_byte.push((row.len(), x));
                row.push_str(buf[(x, y)].symbol());
            }
            if let Some(start) = row.find(needle) {
                let x = cell_x_by_byte
                    .iter()
                    .find_map(|(byte_idx, x)| (*byte_idx == start).then_some(*x))?;
                return Some(buf[(x, y)].style());
            }
        }
        None
    }

    #[test]
    fn pulse_text_compacts_production_next_action_copy() {
        let plan = sample_plan(
            "Use get_task_diff to review each awaiting task, then review_task to approve or reject.",
        );
        assert_eq!(
            pulse_text(&plan),
            "plan p-123 run | 0/3 | rv:1 fl:0 | next:review | Alt+P"
        );
    }

    #[test]
    fn pulse_text_compacts_progress_from_counts() {
        let plan = sample_plan("Workers still running. Poll get_plan_status to monitor.");
        assert_eq!(
            pulse_text(&plan),
            "plan p-123 run | 0/3 | rv:1 fl:0 | next:wait | Alt+P"
        );
    }

    #[test]
    fn plan_pulse_renders_potential_clobber_badge() {
        let mut plan = sample_plan("Workers still running. Poll get_plan_status to monitor.");
        plan.tasks.push(sample_task_with_potential_clobber_label());

        let text = pulse_text(&plan);
        assert!(text.contains("⚠ CLOBBER"), "rendered pulse text: {text}");

        let backend = TestBackend::new(96, 1);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), &plan))
            .expect("render plan pulse");

        let rendered = rendered_buffer_text(&terminal);
        assert!(rendered.contains("⚠ CLOBBER"), "rendered: {rendered}");
        let style = style_for_cell_run(&terminal, "CLOBBER").expect("badge style");
        assert_eq!(style.fg, Some(Color::Black));
        assert_eq!(style.bg, Some(Color::Yellow));
    }
}

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use spur_core::TrackedPlan;

pub fn pulse_text(plan: &TrackedPlan) -> String {
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

pub fn render(frame: &mut Frame, area: Rect, plan: &TrackedPlan) {
    let line = Line::from(Span::styled(
        pulse_text(plan),
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(line).right_aligned(), area);
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

#[cfg(test)]
mod tests {
    use spur_acp::{PlanSnapshotCounts, SessionId};
    use spur_core::TrackedPlan;

    use super::pulse_text;

    fn sample_plan(next_action: &str) -> TrackedPlan {
        TrackedPlan {
            session_id: SessionId("brain-1".into()),
            plan_id: "p-123".into(),
            status: "running".into(),
            progress: "1/3 reviewed, 1 running, 1 pending".into(),
            next_action: next_action.into(),
            ready_to_merge: false,
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
}

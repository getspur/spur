use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use spur_acp::PlanLoopOriginEvent;
use spur_core::{TrackedPlan, TrackedTask};
use unicode_width::UnicodeWidthStr;

use crate::views::plan_browser::truncate;

const POTENTIAL_CLOBBER_LABEL: &str = "signal:potential-clobber";
const CLOBBER_BADGE_LABEL: &str = "⚠ CLOBBER";
const CLOBBER_BADGE_TEXT: &str = " ⚠ CLOBBER ";
const PULSE_TEXT_WIDTH: usize = 48;
const EPIC_ID_MAX_CHARS: usize = 12;
const PLAN_ID_MAX_CHARS: usize = 16;
const ALT_P_HINT: &str = " | Alt+P";

pub fn pulse_text(plan: &TrackedPlan, loop_origin: Option<&PlanLoopOriginEvent>) -> String {
    let mut text = base_pulse_text(plan, loop_origin);
    if plan_has_potential_clobber_signal(plan) {
        text.push_str(" | ");
        text.push_str(CLOBBER_BADGE_LABEL);
    }
    text
}

pub fn render(
    frame: &mut Frame,
    area: Rect,
    plan: &TrackedPlan,
    loop_origin: Option<&PlanLoopOriginEvent>,
) {
    let line = pulse_line(plan, loop_origin);
    frame.render_widget(Paragraph::new(line).right_aligned(), area);
}

fn base_pulse_text(plan: &TrackedPlan, loop_origin: Option<&PlanLoopOriginEvent>) -> String {
    let status = short_status(&plan.status);
    let progress = compact_progress(plan);
    let failed = plan.counts.failed + plan.counts.rejected + plan.counts.cancelled;
    let next_action = short_next_action(&plan.next_action);
    let suffix = pulse_suffix(plan, loop_origin, status, &progress, failed, next_action);
    let id = compact_plan_id(plan, status, &progress, failed, next_action, &suffix);

    format!(
        "{} {}|{}|rv:{} fl:{} nxt:{}{}",
        id, status, progress, plan.counts.awaiting_review, failed, next_action, suffix,
    )
}

fn pulse_line(plan: &TrackedPlan, loop_origin: Option<&PlanLoopOriginEvent>) -> Line<'static> {
    let pulse_style = Style::default()
        .fg(Color::Green)
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![Span::styled(
        base_pulse_text(plan, loop_origin),
        pulse_style,
    )];
    if plan_has_potential_clobber_signal(plan) {
        spans.push(Span::styled(" |", pulse_style));
        spans.push(Span::styled(
            CLOBBER_BADGE_TEXT,
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ));
    }
    Line::from(spans)
}

fn compact_plan_id(
    plan: &TrackedPlan,
    status: &str,
    progress: &str,
    failed: u32,
    next_action: &str,
    suffix: &str,
) -> String {
    let (id, preferred_chars) = match plan.epic_id.as_deref().filter(|id| !id.is_empty()) {
        Some(epic_id) => (epic_id, EPIC_ID_MAX_CHARS),
        None => (plan.plan_id.as_str(), PLAN_ID_MAX_CHARS),
    };
    let fixed_text = format!(
        " {}|{}|rv:{} fl:{} nxt:{}{}",
        status, progress, plan.counts.awaiting_review, failed, next_action, suffix,
    );
    let available_chars =
        PULSE_TEXT_WIDTH.saturating_sub(UnicodeWidthStr::width(fixed_text.as_str()));
    truncate(id, preferred_chars.min(available_chars))
}

fn pulse_suffix(
    plan: &TrackedPlan,
    loop_origin: Option<&PlanLoopOriginEvent>,
    status: &str,
    progress: &str,
    failed: u32,
    next_action: &str,
) -> String {
    let Some(origin) = loop_origin else {
        return ALT_P_HINT.to_string();
    };

    let badge = format!(" {}", loop_origin_badge(origin));
    let badge_with_hint = format!("{badge}{ALT_P_HINT}");
    if base_text_width_with_untruncated_id(
        plan,
        status,
        progress,
        failed,
        next_action,
        &badge_with_hint,
    ) <= PULSE_TEXT_WIDTH
    {
        badge_with_hint
    } else {
        badge
    }
}

fn base_text_width_with_untruncated_id(
    plan: &TrackedPlan,
    status: &str,
    progress: &str,
    failed: u32,
    next_action: &str,
    suffix: &str,
) -> usize {
    let id = match plan.epic_id.as_deref().filter(|id| !id.is_empty()) {
        Some(epic_id) => truncate(epic_id, EPIC_ID_MAX_CHARS),
        None => truncate(&plan.plan_id, PLAN_ID_MAX_CHARS),
    };
    UnicodeWidthStr::width(
        format!(
            "{} {}|{}|rv:{} fl:{} nxt:{}{}",
            id, status, progress, plan.counts.awaiting_review, failed, next_action, suffix,
        )
        .as_str(),
    )
}

fn loop_origin_badge(origin: &PlanLoopOriginEvent) -> String {
    format!("⟳ gen {}", origin.generation)
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
    use spur_acp::{PlanLoopOriginEvent, PlanSnapshotCounts, SessionId};
    use spur_core::{TrackedPlan, TrackedTask};
    use unicode_width::UnicodeWidthStr;

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

    fn sample_loop_origin() -> PlanLoopOriginEvent {
        PlanLoopOriginEvent {
            loop_id: "loop-1".into(),
            generation: 4,
        }
    }

    fn sample_task_with_potential_clobber_label() -> TrackedTask {
        TrackedTask {
            task_id: "T2".into(),
            task_name: "overlay projection".into(),
            agent: "codex".into(),
            issue_id: Some("bd-1dwm.2".into()),
            issue_title: None,
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
            pulse_text(&plan, None),
            "p-123 run|0/3|rv:1 fl:0 nxt:review | Alt+P"
        );
    }

    #[test]
    fn pulse_text_compacts_progress_from_counts() {
        let plan = sample_plan("Workers still running. Poll get_plan_status to monitor.");
        assert_eq!(
            pulse_text(&plan, None),
            "p-123 run|0/3|rv:1 fl:0 nxt:wait | Alt+P"
        );
    }

    #[test]
    fn pulse_text_uses_epic_id_when_present() {
        let mut plan = sample_plan("Workers still running. Poll get_plan_status to monitor.");
        plan.plan_id = "0ce4d22e-a783-48b7-acda-4f1fee79672b".into();
        plan.epic_id = Some("bd-1dwm".into());

        assert_eq!(
            pulse_text(&plan, None),
            "bd-1dwm run|0/3|rv:1 fl:0 nxt:wait | Alt+P"
        );
    }

    #[test]
    fn pulse_text_falls_back_to_truncated_plan_id_when_epic_id_is_absent() {
        let mut plan = sample_plan("Workers still running. Poll get_plan_status to monitor.");
        plan.plan_id = "0ce4d22e-a783-48b7-acda-4f1fee79672b".into();

        assert_eq!(
            pulse_text(&plan, None),
            "0ce4d22e-a... run|0/3|rv:1 fl:0 nxt:wait | Alt+P"
        );
    }

    #[test]
    fn pulse_text_appends_loop_origin_badge_ahead_of_alt_p_hint() {
        let mut plan = sample_plan("Workers still running. Poll get_plan_status to monitor.");
        plan.epic_id = Some("bd-1dwm".into());
        let origin = sample_loop_origin();

        assert_eq!(
            pulse_text(&plan, Some(&origin)),
            "bd-1dwm run|0/3|rv:1 fl:0 nxt:wait ⟳ gen 4"
        );
    }

    #[test]
    fn pulse_text_stays_within_session_detail_header_budget_with_loop_origin() {
        let mut plan = sample_plan(
            "Use get_task_diff to review each awaiting task, then review_task to approve or reject.",
        );
        plan.plan_id = "0ce4d22e-a783-48b7-acda-4f1fee79672b".into();
        plan.epic_id = Some("bd-1dwm".into());
        plan.counts = PlanSnapshotCounts {
            pending: 30,
            ready: 20,
            dispatched: 20,
            awaiting_review: 12,
            approved: 47,
            rejected: 3,
            failed: 4,
            cancelled: 1,
            ..Default::default()
        };
        let origin = sample_loop_origin();

        let text = pulse_text(&plan, Some(&origin));

        assert_eq!(text, "bd-1dwm run|55/137|rv:12 fl:8 nxt:review ⟳ gen 4");
        assert!(
            UnicodeWidthStr::width(text.as_str()) <= 48,
            "pulse text exceeded 48 display columns: {text}"
        );
        assert!(text.contains("⟳ gen 4"), "pulse text: {text}");
        assert!(!text.contains("Alt+P"), "pulse text: {text}");
    }

    #[test]
    fn plan_pulse_renders_potential_clobber_badge() {
        let mut plan = sample_plan("Workers still running. Poll get_plan_status to monitor.");
        plan.tasks.push(sample_task_with_potential_clobber_label());

        let text = pulse_text(&plan, None);
        assert!(text.contains("⚠ CLOBBER"), "rendered pulse text: {text}");

        let backend = TestBackend::new(96, 1);
        let mut terminal = Terminal::new(backend).expect("test backend");
        terminal
            .draw(|frame| render(frame, frame.area(), &plan, None))
            .expect("render plan pulse");

        let rendered = rendered_buffer_text(&terminal);
        assert!(rendered.contains("⚠ CLOBBER"), "rendered: {rendered}");
        let style = style_for_cell_run(&terminal, "CLOBBER").expect("badge style");
        assert_eq!(style.fg, Some(Color::Black));
        assert_eq!(style.bg, Some(Color::Yellow));
    }
}

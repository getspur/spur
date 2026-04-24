use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use spur_core::{ExecutorNode, ReviewDecision, ReviewKind};

/// Render a pending review as a block of styled lines.
pub fn render_review(node: &ExecutorNode) -> Vec<Line<'static>> {
    let req = match &node.pending_review {
        Some(r) => r,
        None => {
            return vec![Line::from(Span::styled(
                "(no pending review)",
                Style::default().fg(Color::DarkGray),
            ))]
        }
    };
    let mut out = Vec::new();
    out.push(Line::from(Span::styled(
        format!("── Review requested: {} ──", kind_label(&req.kind)),
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    out.push(Line::from(""));
    out.push(Line::from(format!("Summary: {}", req.payload.summary)));
    if let Some(d) = &req.payload.diff_summary {
        out.push(Line::from(format!(
            "Diff: {} files, +{} -{}",
            d.files_changed, d.insertions, d.deletions
        )));
    }
    if let Some(pr) = &req.payload.pr_url {
        out.push(Line::from(format!("PR: {}", pr)));
    }
    if let Some(err) = &req.payload.error {
        out.push(Line::from(Span::styled(
            format!("Error: {}", err),
            Style::default().fg(Color::Red),
        )));
    }
    out.push(Line::from(""));
    out.push(Line::from(Span::styled(
        "[A] approve  [D] deny  [M] modify+approve  [R] retry",
        Style::default().fg(Color::Cyan),
    )));
    out
}

fn kind_label(k: &ReviewKind) -> &'static str {
    match k {
        ReviewKind::Completion => "completion",
        ReviewKind::Failure => "failure",
        ReviewKind::Conflict => "conflict",
        ReviewKind::Checkpoint => "checkpoint",
    }
}

/// Pure function mapping a single key + optional free-text prompt answer to a
/// `ReviewDecision`. Returns `None` for keys that are not review actions.
pub fn decision_for_key(key: char, prompt_answer: Option<String>) -> Option<ReviewDecision> {
    match key {
        'A' => Some(ReviewDecision::Approve),
        'D' => Some(ReviewDecision::Reject {
            reason: prompt_answer.unwrap_or_else(|| "(no reason given)".into()),
        }),
        'M' => Some(ReviewDecision::Modify {
            note: prompt_answer.unwrap_or_else(|| "(no note)".into()),
        }),
        'R' => Some(ReviewDecision::Retry {
            new_constraints: prompt_answer.unwrap_or_else(|| "(no constraints)".into()),
        }),
        _ => None,
    }
}

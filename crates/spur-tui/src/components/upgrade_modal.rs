//! Plan C Tier 2 — TUI capability-tease modal. Renders a centered
//! overlay when a feature gate denies a TUI-side action, converting
//! denial-without-recovery into structured upgrade pressure (the
//! same conversion mechanism Tier 1 wired into CLI stderr).
//!
//! Pattern: matches `CollisionModal` — data-bearing render fn
//! (`render(frame, area, &UpgradeModalState)`) + Yellow-bordered
//! `Block` + `Clear` + styled `Paragraph`.

use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use spur_license::{FeatureGateError, Plan};

/// Data carried by `App::upgrade_modal` while the modal is visible.
#[derive(Debug, Clone)]
pub struct UpgradeModalState {
    pub err: FeatureGateError,
    pub required_tier: Option<Plan>,
}

const MODAL_WIDTH: u16 = 70;
const MODAL_HEIGHT: u16 = 16;

pub fn render(frame: &mut Frame, area: Rect, state: &UpgradeModalState) {
    let popup = centered_rect(area, MODAL_WIDTH, MODAL_HEIGHT);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(Span::styled(
            " Feature unavailable ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ))
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));

    let lines = modal_lines(state);
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, popup);
}

fn modal_lines(state: &UpgradeModalState) -> Vec<Line<'static>> {
    // `FeatureGateError` is `#[non_exhaustive]` to leave room for future
    // denial-shape variants. The current MVP renders the Denied case;
    // any future variant falls back to a minimal "feature unavailable"
    // line so the modal still renders something useful before we add
    // a dedicated branch.
    let (key_text, tier_label): (String, &'static str) = match &state.err {
        FeatureGateError::Denied { key, tier } => (key.as_str().to_string(), tier.label()),
        _ => ("(unknown)".to_string(), "Unknown"),
    };
    let mut out: Vec<Line<'static>> = Vec::with_capacity(14);

    // Spacer
    out.push(Line::from(""));

    // Feature: <key>  (key name BOLD-WHITE)
    out.push(Line::from(vec![
        Span::raw("  Feature: "),
        Span::styled(
            key_text,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Current tier: <tier>  (DarkGray for community-tier flavor)
    out.push(Line::from(vec![
        Span::raw("  Current tier: "),
        Span::styled(tier_label.to_string(), Style::default().fg(Color::DarkGray)),
    ]));

    // Required tier: <plan>  (Cyan-BOLD if Some, omit row if None).
    // `Plan` has no `Display` impl today; `{plan:?}` produces
    // PascalCase ("Pro", "Community", …) which is the desired
    // display form. Adding a Display impl is a clean follow-up but
    // not required for this MVP.
    if let Some(req) = state.required_tier {
        out.push(Line::from(vec![
            Span::raw("  Required tier: "),
            Span::styled(
                format!("{req:?}"),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
    }

    // Spacer + "To unlock this feature:"
    out.push(Line::from(""));
    out.push(Line::from("  To unlock this feature:"));

    // Recovery affordances (GREEN-BOLD for the runnable commands)
    out.push(Line::from(vec![
        Span::raw("    \u{2022} View tier comparison:  "),
        Span::styled(
            "spur auth status",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    out.push(Line::from(vec![
        Span::raw("    \u{2022} Activate a license:    "),
        Span::styled(
            "spur auth login --key <KEY>",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Spacer + footnote (DarkGray)
    out.push(Line::from(""));
    out.push(Line::from(Span::styled(
        "  Already have a license? Run `spur auth logout` then re-login",
        Style::default().fg(Color::DarkGray),
    )));
    out.push(Line::from(Span::styled(
        "  to refresh.",
        Style::default().fg(Color::DarkGray),
    )));

    // Spacer + action keys (GREEN-BOLD bracketed)
    out.push(Line::from(""));
    out.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            "[Esc/q]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" Dismiss   "),
        Span::styled(
            "[s]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" Status   "),
        Span::styled(
            "[l]",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" Login"),
    ]));

    out
}

fn centered_rect(outer: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(outer.width);
    let h = height.min(outer.height);
    let x = outer.x + outer.width.saturating_sub(w) / 2;
    let y = outer.y + outer.height.saturating_sub(h) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_license::{FeatureKey, Tier};

    fn fixture_state(required: Option<Plan>) -> UpgradeModalState {
        UpgradeModalState {
            err: FeatureGateError::Denied {
                key: FeatureKey::CLI_CORE_EXEC,
                tier: Tier::Community,
            },
            required_tier: required,
        }
    }

    fn flatten(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
            .collect::<String>()
    }

    #[test]
    fn modal_lines_includes_key_name() {
        let lines = modal_lines(&fixture_state(Some(Plan::Pro)));
        let flat = flatten(&lines);
        assert!(
            flat.contains("cli_core_exec"),
            "lines must name the denied key: {flat}"
        );
    }

    #[test]
    fn modal_lines_includes_recovery_commands() {
        let lines = modal_lines(&fixture_state(Some(Plan::Pro)));
        let flat = flatten(&lines);
        assert!(flat.contains("spur auth status"));
        assert!(flat.contains("spur auth login --key"));
        assert!(flat.contains("spur auth logout"));
    }

    #[test]
    fn modal_lines_includes_required_tier_when_some() {
        let lines = modal_lines(&fixture_state(Some(Plan::Pro)));
        let flat = flatten(&lines);
        assert!(flat.contains("Required tier"));
        assert!(flat.contains("Pro"));
    }

    #[test]
    fn modal_lines_omits_required_tier_when_none() {
        let lines = modal_lines(&fixture_state(None));
        let flat = flatten(&lines);
        assert!(
            !flat.contains("Required tier"),
            "required-tier row must be omitted when None: {flat}"
        );
    }

    #[test]
    fn modal_lines_includes_action_keys() {
        let lines = modal_lines(&fixture_state(Some(Plan::Pro)));
        let flat = flatten(&lines);
        assert!(flat.contains("[Esc/q]"));
        assert!(flat.contains("[s]"));
        assert!(flat.contains("[l]"));
    }

    #[test]
    fn modal_lines_includes_current_tier_label() {
        let lines = modal_lines(&fixture_state(Some(Plan::Pro)));
        let flat = flatten(&lines);
        assert!(flat.contains("Current tier"));
        assert!(
            flat.contains("Community"),
            "current tier label must be PascalCase via Tier::label(): {flat}"
        );
    }

    #[test]
    fn centered_rect_clamps_to_outer_when_smaller() {
        let outer = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 10,
        };
        let r = centered_rect(outer, 70, 16);
        assert_eq!(r.width, 40);
        assert_eq!(r.height, 10);
        assert_eq!(r.x, 0);
        assert_eq!(r.y, 0);
    }

    #[test]
    fn centered_rect_centers_inside_larger_outer() {
        let outer = Rect {
            x: 0,
            y: 0,
            width: 100,
            height: 30,
        };
        let r = centered_rect(outer, 70, 16);
        assert_eq!(r.width, 70);
        assert_eq!(r.height, 16);
        assert_eq!(r.x, 15);
        assert_eq!(r.y, 7);
    }
}

//! Pure diff rendering: colorizes unified-diff text for display in
//! the DetailPane Artifacts tab.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

/// Convert raw unified-diff text into styled `Line`s.
///
/// Coloring rules:
/// - `+` lines → green
/// - `-` lines → red
/// - `@@` hunks → cyan
/// - `diff --git` / `---` / `+++` headers → white bold
/// - context lines → default gray
pub fn render_diff_lines(text: &str) -> Vec<Line<'static>> {
    text.lines()
        .map(|line| {
            let owned = line.to_string();
            if owned.starts_with('+') && !owned.starts_with("+++") {
                Line::from(Span::styled(owned, Style::default().fg(Color::Green)))
            } else if owned.starts_with('-') && !owned.starts_with("---") {
                Line::from(Span::styled(owned, Style::default().fg(Color::Red)))
            } else if owned.starts_with("@@") {
                Line::from(Span::styled(owned, Style::default().fg(Color::Cyan)))
            } else if owned.starts_with("diff ")
                || owned.starts_with("---")
                || owned.starts_with("+++")
            {
                Line::from(Span::styled(
                    owned,
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(owned, Style::default().fg(Color::DarkGray)))
            }
        })
        .collect()
}

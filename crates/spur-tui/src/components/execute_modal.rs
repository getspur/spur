use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

const MODAL_WIDTH: u16 = 60;
const CONFIRM_HEIGHT: u16 = 9;
const ALREADY_EXECUTING_HEIGHT: u16 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecuteModal {
    pub epic_id: String, // Kept as epic_id for now to match struct definition, but conceptually it's an item_id
    pub epic_title: String,
    pub variant: ExecuteModalVariant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecuteModalVariant {
    Confirm,
    AlreadyExecuting { plan_id: String },
}

impl ExecuteModal {
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let height = match self.variant {
            ExecuteModalVariant::Confirm => CONFIRM_HEIGHT,
            ExecuteModalVariant::AlreadyExecuting { .. } => ALREADY_EXECUTING_HEIGHT,
        };
        let popup = centered_rect(area, MODAL_WIDTH, height);

        frame.render_widget(Clear, popup);

        let (title, border_color) = match self.variant {
            ExecuteModalVariant::Confirm => (" Execute Item ", Color::Green),
            ExecuteModalVariant::AlreadyExecuting { .. } => (" Already Executing ", Color::Yellow),
        };
        let block = Block::default()
            .title(Span::styled(
                title,
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Left)
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .border_style(Style::default().fg(border_color));

        frame.render_widget(Paragraph::new(self.lines(popup.width)).block(block), popup);
    }

    fn lines(&self, popup_width: u16) -> Vec<Line<'static>> {
        match &self.variant {
            ExecuteModalVariant::Confirm => vec![
                Line::from(Span::styled(
                    format!("  {} — {}", self.epic_id, self.epic_title),
                    Style::default().fg(Color::White),
                )),
                Line::from(""),
                Line::from("  This sends a prompt asking the brain to analyze"),
                Line::from("  this item and determine how to execute it."),
                Line::from(""),
                Line::from("  Use e to review the prompt before sending."),
                action_line3(
                    "[Enter]",
                    "Confirm",
                    "[e]",
                    "Edit in input bar",
                    "[Esc]",
                    "Cancel",
                    popup_width,
                    Color::Green,
                ),
            ],
            ExecuteModalVariant::AlreadyExecuting { plan_id } => vec![
                Line::from(format!(
                    "  Work item {} is already executing.",
                    self.epic_id
                )),
                Line::from(format!("  Plan-id: {plan_id}")),
                Line::from(""),
                action_line(
                    "[s]",
                    "View session",
                    "[Esc]",
                    "cancel",
                    popup_width,
                    Color::Cyan,
                ),
            ],
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn action_line3(
    left_key: &'static str,
    left_label: &'static str,
    middle_key: &'static str,
    middle_label: &'static str,
    right_key: &'static str,
    right_label: &'static str,
    popup_width: u16,
    primary_color: Color,
) -> Line<'static> {
    let left_width = 1 + left_key.len() + 1 + left_label.len();
    let middle_width = middle_key.len() + 1 + middle_label.len();
    let right_width = right_key.len() + 1 + right_label.len();
    let content_width = popup_width.saturating_sub(2) as usize;
    let total_width = left_width + middle_width + right_width;
    let gap = content_width.saturating_sub(total_width).max(2) / 2;

    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            left_key,
            Style::default()
                .fg(primary_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(
            " {left_label}{}",
            " ".repeat(gap.saturating_sub(1))
        )),
        Span::styled(
            middle_key,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {middle_label}{}", " ".repeat(gap))),
        Span::styled(
            right_key,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {right_label}")),
    ])
}

fn action_line(
    left_key: &'static str,
    left_label: &'static str,
    right_key: &'static str,
    right_label: &'static str,
    popup_width: u16,
    primary_color: Color,
) -> Line<'static> {
    let left_width = 1 + left_key.len() + 1 + left_label.len();
    let right_width = right_key.len() + 1 + right_label.len();
    let content_width = popup_width.saturating_sub(2) as usize;
    let gap = content_width
        .saturating_sub(left_width + right_width)
        .max(1);

    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            left_key,
            Style::default()
                .fg(primary_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {left_label}{}", " ".repeat(gap))),
        Span::styled(
            right_key,
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" {right_label}")),
    ])
}

fn centered_rect(outer: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(outer.width);
    let height = height.min(outer.height);

    Rect {
        x: outer.x + outer.width.saturating_sub(width) / 2,
        y: outer.y + outer.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, Panel};

/// Render the entire TUI layout into the given frame.
pub fn draw(f: &mut Frame, app: &App) {
    let size = f.area();

    // Outer vertical split: main area + bottom status bar
    let outer_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(1)])
        .split(size);

    // Main area: left panel (30%) + right panel (70%)
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(outer_chunks[0]);

    // Left column: agents list + cost summary
    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(main_chunks[0]);

    draw_agents_panel(f, app, left_chunks[0]);
    draw_cost_panel(f, app, left_chunks[1]);
    draw_log_panel(f, app, main_chunks[1]);
    draw_status_bar(f, app, outer_chunks[1]);
}

fn draw_agents_panel(f: &mut Frame, app: &App, area: Rect) {
    let highlight = matches!(app.selected_panel, Panel::Agents);
    let border_style = if highlight {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" Agents ")
        .borders(Borders::ALL)
        .border_style(border_style);

    let items: Vec<ListItem> = app
        .agents
        .iter()
        .map(|agent| {
            let indicator = match agent.status.as_str() {
                "idle" => "○",
                _ => "●",
            };

            let status_color = match agent.status.as_str() {
                "spawned" | "working" => Color::Green,
                "done" => Color::Blue,
                "error" => Color::Red,
                "rate-limited" => Color::Yellow,
                _ => Color::DarkGray,
            };

            let role_label = agent.role.to_uppercase();
            let line = Line::from(vec![
                Span::styled(
                    format!(" {} ", indicator),
                    Style::default().fg(status_color),
                ),
                Span::styled(
                    format!("{:<10}", agent.name),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    role_label,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::DIM),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_cost_panel(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .title(" Cost ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let mut lines: Vec<Line> = app
        .cost_by_agent
        .iter()
        .map(|(agent, cost)| {
            Line::from(vec![
                Span::styled(
                    format!(" {:<10}", agent),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!("${:.2}", cost),
                    Style::default().fg(Color::Yellow),
                ),
            ])
        })
        .collect();

    lines.push(Line::from(vec![
        Span::styled(
            format!(" {:<10}", "total"),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("${:.2}", app.total_cost()),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn draw_log_panel(f: &mut Frame, app: &App, area: Rect) {
    let highlight = matches!(app.selected_panel, Panel::Log);
    let border_style = if highlight {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let block = Block::default()
        .title(" Session Log ")
        .borders(Borders::ALL)
        .border_style(border_style);

    let lines: Vec<Line> = app
        .event_log
        .iter()
        .map(|entry| {
            Line::from(vec![
                Span::styled(
                    format!(" {} ", entry.timestamp),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{} ", entry.prefix),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(&entry.message, Style::default().fg(Color::White)),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((app.log_scroll as u16, 0));

    f.render_widget(paragraph, area);
}

fn draw_status_bar(f: &mut Frame, _app: &App, area: Rect) {
    let line = Line::from(vec![
        Span::styled(
            " [q]uit  [Tab]switch  [↑↓]scroll ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::DIM),
        ),
        Span::styled(
            "                                        ",
            Style::default(),
        ),
        Span::styled(
            "SPUR v0.1 ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    let bar = Paragraph::new(line);
    f.render_widget(bar, area);
}

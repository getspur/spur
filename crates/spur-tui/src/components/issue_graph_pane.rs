use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph, Wrap},
    Frame,
};
use spur_acp::{GraphEdgeEvent, GraphNodeEvent};

pub struct IssueGraphPane {
    scroll_offset: u16,
}

impl IssueGraphPane {
    pub fn new() -> Self {
        Self { scroll_offset: 0 }
    }

    pub fn reset(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn scroll_up_by(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn scroll_down_by(&mut self, lines: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines).min(500);
    }

    pub fn render(
        &self,
        requested_id: &str,
        nodes: &[GraphNodeEvent],
        edges: &[GraphEdgeEvent],
        frame: &mut Frame,
        area: Rect,
    ) {
        let block = Block::bordered()
            .title(format!(" Issue Graph: {} ", requested_id))
            .border_style(Style::default().fg(Color::Magenta));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

        let summary = Line::from(vec![
            Span::styled("nodes:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" {}  ", nodes.len())),
            Span::styled("edges:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" {}", edges.len())),
        ]);
        frame.render_widget(Paragraph::new(summary), chunks[0]);

        let separator = Paragraph::new("-".repeat(inner.width as usize))
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(separator, chunks[1]);

        let mut lines: Vec<Line<'static>> = Vec::new();
        if nodes.is_empty() {
            lines.push(Line::from(Span::styled(
                "No graph nodes returned",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "Nodes",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            for node in nodes {
                let title = node.title.as_deref().unwrap_or("(untitled)");
                let status = node.status.as_deref().unwrap_or("--");
                let priority = node
                    .priority
                    .map(|p| format!("P{p}"))
                    .unwrap_or_else(|| "--".to_string());
                lines.push(Line::from(format!(
                    "{}  {}  {}  {}",
                    node.id, priority, status, title
                )));
            }
        }

        if !edges.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Edges",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            for edge in edges {
                let edge_type = edge.edge_type.as_deref().unwrap_or("depends");
                lines.push(Line::from(format!(
                    "{} -> {}  {}",
                    edge.from, edge.to, edge_type
                )));
            }
        }

        let body = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll_offset, 0));
        frame.render_widget(body, chunks[2]);

        let footer = Paragraph::new("[v] text  [PgUp/PgDn] scroll  [Esc] close")
            .style(Style::default().fg(Color::DarkGray));
        frame.render_widget(footer, chunks[3]);
    }

    pub fn render_loading(id: &str, frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .title(format!(" Issue Graph: {} ", id))
            .border_style(Style::default().fg(Color::Magenta));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let msg = Paragraph::new(format!("Loading graph for {id}"))
            .alignment(Alignment::Center)
            .style(Style::default().fg(Color::DarkGray));
        let vert = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .split(inner);
        frame.render_widget(msg, vert[1]);
    }

    pub fn render_error(id: &str, error: &str, frame: &mut Frame, area: Rect) {
        let block = Block::bordered()
            .title(format!(" Issue Graph: {} ", id))
            .border_style(Style::default().fg(Color::Red));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let msg = Paragraph::new(format!("Graph error: {error}"))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(Color::Red));
        let vert = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(3),
            Constraint::Fill(1),
        ])
        .split(inner);
        frame.render_widget(msg, vert[1]);
    }
}

impl Default for IssueGraphPane {
    fn default() -> Self {
        Self::new()
    }
}

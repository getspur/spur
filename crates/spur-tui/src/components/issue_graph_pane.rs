use std::collections::{HashMap, HashSet};

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Paragraph},
    Frame,
};
use spur_acp::{GraphEdgeEvent, GraphNodeEvent};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const LEGEND: &str = "Legend: ○ open  ● in_progress  ! blocked  ✓ closed";

pub struct IssueGraphPane {
    scroll: u16,
    last_total_lines: u16,
    last_visible_height: u16,
}

impl IssueGraphPane {
    pub fn new() -> Self {
        Self {
            scroll: 0,
            last_total_lines: 0,
            last_visible_height: 0,
        }
    }

    pub fn reset(&mut self) {
        self.scroll = 0;
    }

    pub fn render(
        &mut self,
        requested_id: &str,
        nodes: &[GraphNodeEvent],
        edges: &[GraphEdgeEvent],
        frame: &mut Frame,
        area: Rect,
    ) {
        let graph_lines = build_graph_lines(nodes, edges, requested_id);
        let block = graph_block();
        let inner = block.inner(area);
        self.remember_content_height(graph_lines.len(), inner.height.saturating_sub(1));
        frame.render_widget(block, area);
        render_title_bar(requested_id, Some(nodes.len()), frame, area);

        if inner.height == 0 {
            return;
        }

        if inner.height == 1 {
            frame.render_widget(Paragraph::new(LEGEND), inner);
            return;
        }

        let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);

        let viewport = viewport_lines(&graph_lines, self.scroll, chunks[0].height);
        frame.render_widget(Paragraph::new(viewport), chunks[0]);
        frame.render_widget(Paragraph::new(LEGEND), chunks[1]);
    }

    pub fn render_loading(requested_id: &str, frame: &mut Frame, area: Rect) {
        render_centered_state(
            requested_id,
            format!("Loading graph for {requested_id}"),
            Color::DarkGray,
            frame,
            area,
        );
    }

    pub fn render_error(requested_id: &str, message: &str, frame: &mut Frame, area: Rect) {
        render_centered_state(
            requested_id,
            format!("Graph error: {message}"),
            Color::Red,
            frame,
            area,
        );
    }

    pub fn scroll_up_by(&mut self, lines: u16) {
        self.scroll = self.scroll.saturating_sub(lines);
    }

    pub fn scroll_down_by(&mut self, lines: u16) {
        let max_offset = self
            .last_total_lines
            .saturating_sub(self.last_visible_height);
        self.scroll = self.scroll.saturating_add(lines).min(max_offset);
    }

    fn remember_content_height(&mut self, total_lines: usize, visible_height: u16) {
        self.last_total_lines = total_lines.min(u16::MAX as usize) as u16;
        self.last_visible_height = visible_height;
        let max_offset = self
            .last_total_lines
            .saturating_sub(self.last_visible_height);
        self.scroll = self.scroll.min(max_offset);
    }
}

impl Default for IssueGraphPane {
    fn default() -> Self {
        Self::new()
    }
}

fn graph_block() -> Block<'static> {
    Block::bordered().border_style(Style::default().fg(Color::Magenta))
}

fn render_title_bar(root_id: &str, node_count: Option<usize>, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    frame.render_widget(
        Paragraph::new(title_bar(root_id, node_count, area.width)),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

fn render_centered_state(
    root_id: &str,
    message: String,
    color: Color,
    frame: &mut Frame,
    area: Rect,
) {
    let block = graph_block();
    let inner = block.inner(area);
    frame.render_widget(block, area);
    render_title_bar(root_id, None, frame, area);

    if inner.height == 0 {
        return;
    }

    let chunks = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .split(inner);
    frame.render_widget(
        Paragraph::new(message)
            .alignment(Alignment::Center)
            .style(Style::default().fg(color)),
        chunks[1],
    );
}

fn title_bar(root_id: &str, node_count: Option<usize>, width: u16) -> String {
    let width = width as usize;
    if width == 0 {
        return String::new();
    }

    let prefix = match node_count {
        Some(count) => format!("┌ Issue Graph: {root_id}  {count} nodes "),
        None => format!("┌ Issue Graph: {root_id} "),
    };
    let max_width = width.saturating_sub(1);
    let prefix_width = UnicodeWidthStr::width(prefix.as_str());
    if prefix_width <= max_width {
        return format!("{prefix}{}┐", "─".repeat(max_width - prefix_width));
    }

    let mut truncated = truncate_to_width(&prefix, max_width);
    let padded_width = UnicodeWidthStr::width(truncated.as_str());
    if padded_width < max_width {
        truncated.push_str(&"─".repeat(max_width - padded_width));
    }
    truncated.push('┐');
    truncated
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        width += ch_width;
        out.push(ch);
    }
    out
}

fn build_graph_lines(
    nodes: &[GraphNodeEvent],
    edges: &[GraphEdgeEvent],
    root_id: &str,
) -> Vec<Line<'static>> {
    let node_by_id: HashMap<&str, &GraphNodeEvent> =
        nodes.iter().map(|node| (node.id.as_str(), node)).collect();
    let mut children_by_id: HashMap<&str, Vec<&str>> = HashMap::new();
    for edge in edges {
        if edge.edge_type.as_deref() == Some("blocks") || edge.edge_type.is_none() {
            children_by_id
                .entry(edge.from.as_str())
                .or_default()
                .push(edge.to.as_str());
        }
    }

    let mut lines = Vec::new();
    let mut path = HashSet::new();
    push_dfs_lines(
        root_id,
        0,
        &node_by_id,
        &children_by_id,
        &mut path,
        &mut lines,
    );
    lines
}

fn push_dfs_lines<'a>(
    id: &'a str,
    depth: usize,
    node_by_id: &HashMap<&'a str, &'a GraphNodeEvent>,
    children_by_id: &HashMap<&'a str, Vec<&'a str>>,
    path: &mut HashSet<&'a str>,
    lines: &mut Vec<Line<'static>>,
) {
    if path.contains(id) {
        lines.push(Line::from(format_node_line(id, depth, node_by_id, true)));
        return;
    }

    path.insert(id);
    lines.push(Line::from(format_node_line(id, depth, node_by_id, false)));

    if let Some(children) = children_by_id.get(id) {
        for child in children {
            push_dfs_lines(child, depth + 1, node_by_id, children_by_id, path, lines);
        }
    }

    path.remove(id);
}

fn format_node_line(
    id: &str,
    depth: usize,
    node_by_id: &HashMap<&str, &GraphNodeEvent>,
    cycle: bool,
) -> String {
    let node = node_by_id.get(id).copied();
    let status = node
        .and_then(|node| node.status.as_deref())
        .unwrap_or("open");
    let title = node
        .and_then(|node| node.title.as_deref())
        .filter(|title| !title.is_empty())
        .unwrap_or(id);
    let mut line = format!(
        "{}{} {} ({})",
        "  ".repeat(depth),
        status_icon(status),
        title,
        id
    );
    if cycle {
        line.push_str(" ↻ cycle");
    }
    line
}

fn status_icon(status: &str) -> &'static str {
    match status {
        "open" => "○",
        "in_progress" => "●",
        "blocked" => "!",
        "closed" => "✓",
        _ => "○",
    }
}

fn viewport_lines(lines: &[Line<'static>], scroll: u16, height: u16) -> Vec<Line<'static>> {
    let height = height as usize;
    if height == 0 {
        return Vec::new();
    }

    let max_start = lines.len().saturating_sub(height);
    let start = (scroll as usize).min(max_start);
    let has_more = start + height < lines.len();
    let tree_rows = if has_more {
        height.saturating_sub(1)
    } else {
        height
    };

    let mut viewport: Vec<Line<'static>> =
        lines.iter().skip(start).take(tree_rows).cloned().collect();

    if has_more {
        let more = lines.len().saturating_sub(start + tree_rows);
        viewport.push(Line::from(format!("↓ {more} more dependencies (PageDown)")));
    }

    while viewport.len() < height {
        viewport.push(Line::from(""));
    }

    viewport
}

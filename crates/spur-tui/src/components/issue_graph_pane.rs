use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    hash::{Hash, Hasher},
};

use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use spur_acp::{GraphEdgeEvent, GraphNodeEvent};

use crate::components::issue_utils::status_icon;

const LEGEND: &str = "Legend: ○ open  ● in_progress  ! blocked  ✓ closed";

pub struct IssueGraphPane {
    scroll: u16,
    last_total_lines: u16,
    last_visible_height: u16,
    cached_lines: Vec<Line<'static>>,
    cache_key: Option<GraphRenderCacheKey>,
}

#[derive(Debug, PartialEq, Eq)]
struct GraphRenderCacheKey {
    requested_id: String,
    node_count: usize,
    edge_count: usize,
    content_fingerprint: u64,
}

impl GraphRenderCacheKey {
    fn new(requested_id: &str, nodes: &[GraphNodeEvent], edges: &[GraphEdgeEvent]) -> Self {
        Self {
            requested_id: requested_id.to_string(),
            node_count: nodes.len(),
            edge_count: edges.len(),
            content_fingerprint: graph_content_fingerprint(nodes, edges),
        }
    }
}

impl IssueGraphPane {
    pub fn new() -> Self {
        Self {
            scroll: 0,
            last_total_lines: 0,
            last_visible_height: 0,
            cached_lines: Vec::new(),
            cache_key: None,
        }
    }

    pub fn reset(&mut self) {
        self.scroll = 0;
        self.cached_lines = Vec::new();
        self.cache_key = None;
    }

    pub fn render(
        &mut self,
        requested_id: &str,
        nodes: &[GraphNodeEvent],
        edges: &[GraphEdgeEvent],
        frame: &mut Frame,
        area: Rect,
    ) {
        let cache_key = GraphRenderCacheKey::new(requested_id, nodes, edges);
        if self.cache_key.as_ref() != Some(&cache_key) {
            self.cached_lines = build_graph_lines(nodes, edges, requested_id);
            self.cache_key = Some(cache_key);
        }

        let block = graph_block(format!(
            " Issue Graph: {} ({} nodes) ",
            requested_id,
            nodes.len()
        ));
        let inner = block.inner(area);
        self.remember_content_height(self.cached_lines.len(), inner.height.saturating_sub(1));
        frame.render_widget(block, area);

        if inner.height == 0 {
            return;
        }

        if inner.height == 1 {
            frame.render_widget(Paragraph::new(LEGEND), inner);
            return;
        }

        let chunks = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(inner);

        let viewport = viewport_lines(&self.cached_lines, self.scroll, chunks[0].height);
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

fn graph_block(title: String) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta))
}

fn render_centered_state(
    root_id: &str,
    message: String,
    color: Color,
    frame: &mut Frame,
    area: Rect,
) {
    let block = graph_block(format!(" Issue Graph: {root_id} "));
    let inner = block.inner(area);
    frame.render_widget(block, area);

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

fn graph_content_fingerprint(nodes: &[GraphNodeEvent], edges: &[GraphEdgeEvent]) -> u64 {
    let mut hasher = DefaultHasher::new();

    for node in nodes {
        0_u8.hash(&mut hasher);
        node.id.hash(&mut hasher);
        node.status.hash(&mut hasher);
        node.title.hash(&mut hasher);
    }

    for edge in edges {
        1_u8.hash(&mut hasher);
        edge.from.hash(&mut hasher);
        edge.to.hash(&mut hasher);
        edge.edge_type.hash(&mut hasher);
    }

    hasher.finish()
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
    push_dfs_lines(root_id, &node_by_id, &children_by_id, &mut path, &mut lines);
    lines
}

fn push_dfs_lines<'a>(
    root_id: &'a str,
    node_by_id: &HashMap<&'a str, &'a GraphNodeEvent>,
    children_by_id: &HashMap<&'a str, Vec<&'a str>>,
    path: &mut HashSet<&'a str>,
    lines: &mut Vec<Line<'static>>,
) {
    enum Frame<'a> {
        Enter(&'a str, usize),
        Exit(&'a str),
    }

    let mut stack = vec![Frame::Enter(root_id, 0)];

    while let Some(frame) = stack.pop() {
        match frame {
            Frame::Enter(id, depth) => {
                if path.contains(id) {
                    lines.push(Line::from(format_node_line(id, depth, node_by_id, true)));
                    continue;
                }

                path.insert(id);
                lines.push(Line::from(format_node_line(id, depth, node_by_id, false)));
                stack.push(Frame::Exit(id));

                if let Some(children) = children_by_id.get(id) {
                    for child in children.iter().rev() {
                        stack.push(Frame::Enter(child, depth + 1));
                    }
                }
            }
            Frame::Exit(id) => {
                path.remove(id);
            }
        }
    }
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

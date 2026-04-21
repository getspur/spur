use std::collections::HashSet;
use std::time::SystemTime;

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use spur_core::{ExecutorId, ExecutorLineage, ExecutorNode, LifecycleState, Role};

use super::focused_border_style;
use crate::components::spinner;

pub struct AgentsTree {
    focused: bool,
    tick_counter: u8,
    /// Ids whose subtree is collapsed (children hidden).
    pub(crate) collapsed: HashSet<ExecutorId>,
    /// Currently selected id (if any).
    selected: Option<ExecutorId>,
}

impl Default for AgentsTree {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentsTree {
    pub fn new() -> Self {
        Self {
            focused: false,
            tick_counter: 0,
            collapsed: HashSet::new(),
            selected: None,
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn tick(&mut self) {
        self.tick_counter = self.tick_counter.wrapping_add(1);
    }

    pub fn selected(&self) -> Option<&ExecutorId> {
        self.selected.as_ref()
    }

    pub fn set_selected(&mut self, id: Option<ExecutorId>) {
        self.selected = id;
    }

    pub fn toggle_collapsed(&mut self, id: &ExecutorId) {
        if !self.collapsed.remove(id) {
            self.collapsed.insert(id.clone());
        }
    }

    pub fn select_next(&mut self, lineage: &ExecutorLineage) -> Option<ExecutorId> {
        let order = self.visible_order(lineage);
        if order.is_empty() {
            return None;
        }
        let idx = self
            .selected
            .as_ref()
            .and_then(|s| order.iter().position(|i| i == s))
            .map(|i| (i + 1).min(order.len() - 1))
            .unwrap_or(0);
        self.selected = order.get(idx).cloned();
        self.selected.clone()
    }

    pub fn select_prev(&mut self, lineage: &ExecutorLineage) -> Option<ExecutorId> {
        let order = self.visible_order(lineage);
        if order.is_empty() {
            return None;
        }
        let idx = self
            .selected
            .as_ref()
            .and_then(|s| order.iter().position(|i| i == s))
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0);
        self.selected = order.get(idx).cloned();
        self.selected.clone()
    }

    fn visible_order(&self, lineage: &ExecutorLineage) -> Vec<ExecutorId> {
        let mut out = Vec::new();
        for rid in lineage.root_ids() {
            self.walk(lineage, rid, &mut out);
        }
        out
    }

    fn walk(&self, l: &ExecutorLineage, id: &ExecutorId, out: &mut Vec<ExecutorId>) {
        if let Some(n) = l.node(id) {
            out.push(id.clone());
            if !self.collapsed.contains(id) {
                for c in &n.child_ids {
                    self.walk(l, c, out);
                }
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, lineage: &ExecutorLineage) {
        let block = Block::default()
            .title(" Lineage ")
            .borders(Borders::ALL)
            .border_style(focused_border_style(self.focused));

        let mut lines: Vec<Line> = Vec::new();
        for rid in lineage.root_ids() {
            self.render_subtree(lineage, rid, 0, &mut lines);
        }

        let paragraph = Paragraph::new(lines).block(block);
        frame.render_widget(paragraph, area);
    }

    fn render_subtree<'a>(
        &self,
        l: &'a ExecutorLineage,
        id: &ExecutorId,
        depth: usize,
        out: &mut Vec<Line<'a>>,
    ) {
        let node = match l.node(id) {
            Some(n) => n,
            None => return,
        };
        let is_selected = self.selected.as_ref() == Some(id);
        out.push(self.build_line(node, depth, is_selected));
        if self.collapsed.contains(id) {
            return;
        }
        for c in &node.child_ids {
            self.render_subtree(l, c, depth + 1, out);
        }
    }

    fn build_line<'a>(&self, node: &'a ExecutorNode, depth: usize, selected: bool) -> Line<'a> {
        let indent = "  ".repeat(depth);
        let connector = if depth == 0 { "" } else { "└─ " };

        let spinner = match node.phase {
            LifecycleState::Running | LifecycleState::Spawning => {
                spinner::frame(spinner::BRAILLE, self.tick_counter as u32)
            }
            LifecycleState::AwaitingReview => "⚠",
            LifecycleState::Succeeded => "●",
            LifecycleState::Failed => "✗",
            LifecycleState::Cancelled => "○",
            LifecycleState::Resuming => "↻",
        };

        let status_color = match node.phase {
            LifecycleState::Running | LifecycleState::Spawning | LifecycleState::Resuming => {
                Color::Green
            }
            LifecycleState::AwaitingReview => Color::Yellow,
            LifecycleState::Succeeded => Color::Blue,
            LifecycleState::Failed => Color::Red,
            LifecycleState::Cancelled => Color::DarkGray,
        };

        let elapsed_str = node
            .current_attempt()
            .map(|a| {
                let now = SystemTime::now();
                let secs = now
                    .duration_since(a.started_at)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                format!("{}m {:02}s", secs / 60, secs % 60)
            })
            .unwrap_or_default();

        let cost = node.current_attempt().map(|a| a.cost_usd).unwrap_or(0.0);
        let cost_str = if cost > 0.0 {
            format!("${:.2}", cost)
        } else {
            String::new()
        };

        let role_label = match node.role {
            Role::Brain => "BRAIN",
            Role::Executor => "EXEC",
            Role::SubExecutor => "SUB",
        };

        let review_badge = if node.pending_review.is_some() {
            " ⚠review"
        } else {
            ""
        };

        let base = Style::default();
        let row = if selected {
            base.bg(Color::DarkGray)
        } else {
            base
        };

        let mut spans: Vec<Span> = Vec::new();
        spans.push(Span::styled(
            format!("{}{}", indent, connector),
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::styled(
            format!("{} ", spinner),
            Style::default().fg(status_color),
        ));
        spans.push(Span::styled(
            format!("{:<12} ", node.agent),
            row.fg(Color::White),
        ));
        spans.push(Span::styled(
            format!("{:<5} ", role_label),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
        ));
        spans.push(Span::styled(
            format!("{:<14} ", format!("{:?}", node.phase)),
            Style::default().fg(status_color),
        ));
        if !elapsed_str.is_empty() {
            spans.push(Span::styled(
                format!("{} ", elapsed_str),
                Style::default().fg(Color::DarkGray),
            ));
        }
        if !cost_str.is_empty() {
            spans.push(Span::styled(cost_str, Style::default().fg(Color::Yellow)));
        }
        if !review_badge.is_empty() {
            spans.push(Span::styled(
                review_badge.to_string(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        Line::from(spans)
    }
}

/// Testing helper: render the lineage to plain strings.
pub fn render_lineage_to_strings(
    lineage: &ExecutorLineage,
    selected: Option<ExecutorId>,
) -> Vec<String> {
    let mut tree = AgentsTree::new();
    tree.set_selected(selected);
    let mut out = Vec::new();
    for rid in lineage.root_ids() {
        collect_lines(&tree, lineage, rid, 0, &mut out);
    }
    out
}

fn collect_lines(
    tree: &AgentsTree,
    l: &ExecutorLineage,
    id: &ExecutorId,
    depth: usize,
    out: &mut Vec<String>,
) {
    if let Some(node) = l.node(id) {
        let indent = "  ".repeat(depth);
        let connector = if depth == 0 { "" } else { "└─ " };
        out.push(format!(
            "{}{}{} {} [{:?}]",
            indent,
            connector,
            node.agent,
            match node.role {
                Role::Brain => "BRAIN",
                Role::Executor => "EXEC",
                Role::SubExecutor => "SUB",
            },
            node.phase
        ));
        if !tree.collapsed.contains(id) {
            for c in &node.child_ids {
                collect_lines(tree, l, c, depth + 1, out);
            }
        }
    }
}

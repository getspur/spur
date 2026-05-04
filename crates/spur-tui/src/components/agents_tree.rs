use std::collections::HashSet;

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
    /// Vertical scroll offset in lines.
    scroll_offset: usize,
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
            scroll_offset: 0,
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

    pub fn select_first(&mut self, lineage: &ExecutorLineage) {
        let order = self.visible_order(lineage);
        self.selected = order.first().cloned();
    }

    pub fn select_last(&mut self, lineage: &ExecutorLineage) {
        let order = self.visible_order(lineage);
        self.selected = order.last().cloned();
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

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
    }

    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = usize::MAX;
    }

    /// Pre-order traversal of visible nodes in display order
    /// (newest root first; within each node, newest child first).
    /// Used by selection navigation (`select_next`/`select_prev`) and to
    /// keep the selected row scrolled into view in `render`.
    ///
    /// NOTE: `render` and `render_lineage_to_strings` currently duplicate
    /// this reversed traversal because they need richer per-node context
    /// (ratatui `Line<'a>` and connector glyphs vs. flat `Vec<ExecutorId>`).
    /// All three walkers MUST stay in sync on iteration order. A future
    /// refactor could collapse them by emitting `(id, depth, is_last,
    /// ancestor_states)` tuples from a single shared traversal.
    fn visible_order(&self, lineage: &ExecutorLineage) -> Vec<ExecutorId> {
        let mut out = Vec::new();
        for rid in lineage.root_ids().iter().rev() {
            self.walk(lineage, rid, &mut out);
        }
        out
    }

    fn walk(&self, l: &ExecutorLineage, id: &ExecutorId, out: &mut Vec<ExecutorId>) {
        if let Some(n) = l.node(id) {
            out.push(id.clone());
            if !self.collapsed.contains(id) {
                for c in n.child_ids.iter().rev() {
                    self.walk(l, c, out);
                }
            }
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, lineage: &ExecutorLineage) {
        let block = Block::default()
            .title(" Lineage ")
            .borders(Borders::ALL)
            .border_style(focused_border_style(self.focused));

        let mut lines: Vec<Line> = Vec::new();
        // Display order: newest root first; within each subtree, newest child first.
        let roots: Vec<&ExecutorId> = lineage.root_ids().iter().rev().collect();
        for (i, rid) in roots.iter().enumerate() {
            let is_last = i == roots.len().saturating_sub(1);
            self.render_subtree(lineage, rid, 0, is_last, &[], &mut lines);
        }

        let inner_h = area.height.saturating_sub(2) as usize;
        let total = lines.len();
        let max_offset = total.saturating_sub(inner_h);

        // Keep selected item visible
        if let Some(ref sel) = self.selected {
            let order = self.visible_order(lineage);
            if let Some(idx) = order.iter().position(|id| id == sel) {
                if idx < self.scroll_offset {
                    self.scroll_offset = idx;
                } else if inner_h > 0 && idx >= self.scroll_offset + inner_h {
                    self.scroll_offset = idx.saturating_sub(inner_h - 1);
                }
            }
        }
        self.scroll_offset = self.scroll_offset.min(max_offset);

        let paragraph = Paragraph::new(lines)
            .scroll((self.scroll_offset as u16, 0))
            .block(block);
        frame.render_widget(paragraph, area);
    }

    fn render_subtree<'a>(
        &self,
        l: &'a ExecutorLineage,
        id: &ExecutorId,
        depth: usize,
        is_last: bool,
        ancestor_states: &[bool],
        out: &mut Vec<Line<'a>>,
    ) {
        let node = match l.node(id) {
            Some(n) => n,
            None => return,
        };
        let is_selected = self.selected.as_ref() == Some(id);
        out.push(self.build_line(node, depth, is_last, ancestor_states, is_selected));
        if self.collapsed.contains(id) {
            return;
        }
        // Children walked in REVERSE so the newest child renders first.
        let children: Vec<&ExecutorId> = node.child_ids.iter().rev().collect();
        let child_count = children.len();
        for (i, c) in children.iter().enumerate() {
            let child_is_last = i == child_count.saturating_sub(1);
            let mut next_ancestors = ancestor_states.to_vec();
            next_ancestors.push(is_last);
            self.render_subtree(l, c, depth + 1, child_is_last, &next_ancestors, out);
        }
    }

    fn build_line<'a>(
        &self,
        node: &'a ExecutorNode,
        depth: usize,
        is_last: bool,
        ancestor_states: &[bool],
        selected: bool,
    ) -> Line<'a> {
        let mut indent = String::new();
        for &ancestor_was_last in ancestor_states {
            if ancestor_was_last {
                indent.push_str("   ");
            } else {
                indent.push_str("│  ");
            }
        }
        let connector = if depth == 0 {
            ""
        } else if is_last {
            "└─ "
        } else {
            "├─ "
        };

        let has_children = !node.child_ids.is_empty();
        let collapse_glyph = if has_children {
            if self.collapsed.contains(&node.id) {
                "▶ "
            } else {
                "▼ "
            }
        } else {
            "  "
        };

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

        let elapsed_str = if node.attempts.is_empty() {
            String::new()
        } else {
            let secs = node.elapsed_secs();
            format!("{}m {:02}s", secs / 60, secs % 60)
        };

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
            format!("{}{}{}", indent, connector, collapse_glyph),
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

/// Testing helper: render the lineage to plain strings, in display order
/// (newest root first; within each node, newest child first).
pub fn render_lineage_to_strings(
    lineage: &ExecutorLineage,
    selected: Option<ExecutorId>,
) -> Vec<String> {
    let mut tree = AgentsTree::new();
    tree.set_selected(selected);
    let mut out = Vec::new();
    let roots: Vec<&ExecutorId> = lineage.root_ids().iter().rev().collect();
    for (i, rid) in roots.iter().enumerate() {
        let is_last = i == roots.len().saturating_sub(1);
        collect_lines(&tree, lineage, rid, 0, is_last, &[], &mut out);
    }
    out
}

fn collect_lines(
    tree: &AgentsTree,
    l: &ExecutorLineage,
    id: &ExecutorId,
    depth: usize,
    is_last: bool,
    ancestor_states: &[bool],
    out: &mut Vec<String>,
) {
    if let Some(node) = l.node(id) {
        let mut indent = String::new();
        for &ancestor_was_last in ancestor_states {
            if ancestor_was_last {
                indent.push_str("   ");
            } else {
                indent.push_str("│  ");
            }
        }
        let connector = if depth == 0 {
            ""
        } else if is_last {
            "└─ "
        } else {
            "├─ "
        };
        let has_children = !node.child_ids.is_empty();
        let collapse_glyph = if has_children {
            if tree.collapsed.contains(&node.id) {
                "▶ "
            } else {
                "▼ "
            }
        } else {
            "  "
        };
        out.push(format!(
            "{}{}{}{} {} [{:?}]",
            indent,
            connector,
            collapse_glyph,
            node.agent,
            match node.role {
                Role::Brain => "BRAIN",
                Role::Executor => "EXEC",
                Role::SubExecutor => "SUB",
            },
            node.phase
        ));
        if !tree.collapsed.contains(id) {
            // Children walked in REVERSE so the newest renders first.
            let children: Vec<&ExecutorId> = node.child_ids.iter().rev().collect();
            let child_count = children.len();
            for (i, c) in children.iter().enumerate() {
                let child_is_last = i == child_count.saturating_sub(1);
                let mut next_ancestors = ancestor_states.to_vec();
                next_ancestors.push(is_last);
                collect_lines(tree, l, c, depth + 1, child_is_last, &next_ancestors, out);
            }
        }
    }
}

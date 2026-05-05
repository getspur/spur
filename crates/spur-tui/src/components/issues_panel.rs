use std::collections::{HashMap, HashSet};

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style, Stylize},
    widgets::{Block, Cell, Row, Table, TableState},
    Frame,
};

use spur_acp::{GraphEdgeEvent, GraphNodeEvent};
use spur_pm::IssueSummary;

use crate::components::issue_utils::{
    descendant_depth, find_plan_id_label, has_label, has_plan_task_label, insert_parent_id,
    status_icon,
};

pub struct IssueLineageContext<'a> {
    pub root_id: &'a str,
    pub nodes: &'a [GraphNodeEvent],
    pub edges: &'a [GraphEdgeEvent],
}

pub enum IssueLineageView<'a> {
    Loaded(IssueLineageContext<'a>),
    Cached {
        root_id: String,
        nodes: &'a [GraphNodeEvent],
        edges: &'a [GraphEdgeEvent],
    },
    Loading {
        root_id: String,
        nodes: &'a [GraphNodeEvent],
        edges: &'a [GraphEdgeEvent],
    },
}

pub struct IssuesPanel {
    // Selection state invariant:
    // - table_state stores the source index into the `issues` slice.
    // - lineage_table_state stores the visual index for lineage rendering only.
    // - selected_id always reads table_state; never treat lineage_table_state as
    //   a source index.
    table_state: TableState,
    lineage_table_state: TableState,
    focused: bool,
    display_order: Vec<usize>,
}

impl IssuesPanel {
    const WRAPAROUND_LIMIT: usize = 20;

    pub fn new() -> Self {
        Self {
            table_state: TableState::default(),
            lineage_table_state: TableState::default(),
            focused: false,
            display_order: Vec::new(),
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn select_next(&mut self, count: usize, issue_count: usize) {
        if issue_count == 0 {
            return;
        }
        let current = self
            .table_state
            .selected()
            .unwrap_or(0)
            .min(issue_count.saturating_sub(1));
        let display_order = self.valid_display_order(issue_count);
        let current_position = display_order
            .map(|display_order| {
                display_order
                    .iter()
                    .position(|idx| *idx == current)
                    .unwrap_or(0)
            })
            .unwrap_or(current);
        let next_position = if issue_count <= Self::WRAPAROUND_LIMIT {
            (current_position + (count % issue_count)) % issue_count
        } else {
            current_position.saturating_add(count).min(issue_count - 1)
        };
        let next = display_order
            .map(|display_order| display_order[next_position])
            .unwrap_or(next_position);
        self.table_state.select(Some(next));
    }

    pub fn select_prev(&mut self, count: usize, issue_count: usize) {
        if issue_count == 0 {
            return;
        }
        let current = self
            .table_state
            .selected()
            .unwrap_or(0)
            .min(issue_count.saturating_sub(1));
        let display_order = self.valid_display_order(issue_count);
        let current_position = display_order
            .map(|display_order| {
                display_order
                    .iter()
                    .position(|idx| *idx == current)
                    .unwrap_or(0)
            })
            .unwrap_or(current);
        let prev_position = if issue_count <= Self::WRAPAROUND_LIMIT {
            (current_position + issue_count - (count % issue_count)) % issue_count
        } else {
            current_position.saturating_sub(count)
        };
        let prev = display_order
            .map(|display_order| display_order[prev_position])
            .unwrap_or(prev_position);
        self.table_state.select(Some(prev));
    }

    pub fn select_first(&mut self, issue_count: usize) {
        let first = self
            .valid_display_order(issue_count)
            .and_then(|display_order| display_order.first().copied())
            .unwrap_or(0);
        self.table_state.select(Some(first));
    }

    pub fn select_last(&mut self, issue_count: usize) {
        if issue_count > 0 {
            let last = self
                .valid_display_order(issue_count)
                .and_then(|display_order| display_order.last().copied())
                .unwrap_or(issue_count - 1);
            self.table_state.select(Some(last));
        }
    }

    pub fn selected_id<'a>(&self, issues: &'a [IssueSummary]) -> Option<&'a str> {
        let idx = self.table_state.selected()?;
        issues.get(idx).map(|i| i.id.as_str())
    }

    fn valid_display_order(&self, issue_count: usize) -> Option<&[usize]> {
        (self.display_order.len() == issue_count
            && self.display_order.iter().all(|idx| *idx < issue_count))
        .then_some(self.display_order.as_slice())
    }

    /// Inc 3 (bd-d587.3): select the row whose id matches `id`. Returns `true`
    /// if the id was found and selection was moved; `false` otherwise (caller
    /// can stash the id for a later retry once `tracked_issues` is updated).
    pub fn select_by_id(&mut self, id: &str, issues: &[IssueSummary]) -> bool {
        if let Some(idx) = issues.iter().position(|i| i.id == id) {
            self.table_state.select(Some(idx));
            true
        } else {
            false
        }
    }

    pub fn render(&mut self, issues: &[IssueSummary], frame: &mut Frame, area: Rect) {
        self.render_with_lineage(issues, None, frame, area);
    }

    pub fn render_with_lineage(
        &mut self,
        issues: &[IssueSummary],
        lineage: Option<IssueLineageView<'_>>,
        frame: &mut Frame,
        area: Rect,
    ) {
        if issues.is_empty() {
            return;
        }

        if let Some(lineage) = lineage {
            match lineage {
                IssueLineageView::Loaded(lineage) if lineage.nodes.len() > 1 => {
                    let meta = LineageMeta::new(lineage);
                    let readiness = meta.readiness();
                    self.render_lineage(issues, meta, readiness, frame, area);
                    return;
                }
                IssueLineageView::Cached {
                    root_id,
                    nodes,
                    edges,
                } => {
                    let meta = LineageMeta::new(IssueLineageContext {
                        root_id: root_id.as_str(),
                        nodes,
                        edges,
                    });
                    let readiness = meta.readiness();
                    self.render_lineage(issues, meta, readiness, frame, area);
                    return;
                }
                IssueLineageView::Loading {
                    root_id,
                    nodes,
                    edges,
                } => {
                    let meta = LineageMeta::new(IssueLineageContext {
                        root_id: root_id.as_str(),
                        nodes,
                        edges,
                    });
                    self.render_lineage(issues, meta, "loading work tree".into(), frame, area);
                    return;
                }
                IssueLineageView::Loaded(_) => {}
            }
        }

        let header = Row::new(["ID", "P", "Type", "Status", "Assignee", "Title"])
            .style(Style::default().bold());
        self.display_order = (0..issues.len()).collect();

        let rows: Vec<Row> = issues
            .iter()
            .map(|issue| {
                let priority_cell = match issue.priority {
                    Some(0) => Cell::from("P0").fg(Color::Red),
                    Some(1) => Cell::from("P1").fg(Color::Yellow),
                    Some(2) => Cell::from("P2").fg(Color::White),
                    Some(3) => Cell::from("P3").fg(Color::DarkGray),
                    Some(4) => Cell::from("P4").fg(Color::DarkGray),
                    _ => Cell::from("--").fg(Color::DarkGray),
                };

                let status_cell = match issue.status.as_str() {
                    "open" => Cell::from("open").fg(Color::Green),
                    "in_progress" => Cell::from("wip").fg(Color::Cyan),
                    "blocked" => Cell::from("blk").fg(Color::Red),
                    "closed" => Cell::from("done").fg(Color::DarkGray),
                    other => Cell::from(other.to_string()).fg(Color::White),
                };

                Row::new([
                    Cell::from(issue.id.as_str()),
                    priority_cell,
                    Cell::from(issue.issue_type.as_deref().unwrap_or("--")),
                    status_cell,
                    Cell::from(issue.assignee.as_deref().unwrap_or("--")),
                    Cell::from(issue.title.as_str()),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(8),
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Length(4),
            Constraint::Length(10),
            Constraint::Min(20),
        ];

        let selected_idx = self.table_state.selected().unwrap_or(0);
        let total = issues.len();
        let (border_style, title) = if self.focused {
            (
                Style::default().fg(Color::Cyan),
                format!(
                    " Issues {}/{} — [j/k] select · [Enter] detail · [W]ork ",
                    selected_idx + 1,
                    total
                ),
            )
        } else {
            (
                Style::default(),
                format!(" Issues {}/{} ", selected_idx + 1, total),
            )
        };

        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::bordered().title(title).border_style(border_style))
            .row_highlight_style(
                Style::default()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(table, area, &mut self.table_state);
    }

    fn render_lineage(
        &mut self,
        issues: &[IssueSummary],
        meta: LineageMeta<'_>,
        readiness: String,
        frame: &mut Frame,
        area: Rect,
    ) {
        let selected_idx = self.table_state.selected().unwrap_or(0);
        let total = issues.len();
        let (border_style, title) = if self.focused {
            (
                Style::default().fg(Color::Cyan),
                format!(
                    " Work Item Lineage {}/{} · {} — [j/k] select · [Enter] detail · [e] execute ",
                    selected_idx + 1,
                    total,
                    readiness
                ),
            )
        } else {
            (
                Style::default(),
                format!(
                    " Work Item Lineage {}/{} · {} ",
                    selected_idx + 1,
                    total,
                    readiness
                ),
            )
        };

        let header = Row::new(["Lineage", "P", "Type", "Status", "Assignee", "Title"])
            .style(Style::default().bold());
        let ordered_issues = meta.ordered_issues(issues);
        self.display_order = ordered_issues.iter().map(|(idx, _)| *idx).collect();
        let display_selected_idx = ordered_issues
            .iter()
            .position(|(issue_idx, _)| *issue_idx == selected_idx)
            .unwrap_or_else(|| selected_idx.min(ordered_issues.len().saturating_sub(1)));

        let rows: Vec<Row> = ordered_issues
            .iter()
            .map(|(_, issue)| {
                let priority_cell = match issue.priority {
                    Some(0) => Cell::from("P0").fg(Color::Red),
                    Some(1) => Cell::from("P1").fg(Color::Yellow),
                    Some(2) => Cell::from("P2").fg(Color::White),
                    Some(3) => Cell::from("P3").fg(Color::DarkGray),
                    Some(4) => Cell::from("P4").fg(Color::DarkGray),
                    _ => Cell::from("--").fg(Color::DarkGray),
                };

                let status_cell = match issue.status.as_str() {
                    "open" => Cell::from("open").fg(Color::Green),
                    "in_progress" => Cell::from("wip").fg(Color::Cyan),
                    "blocked" => Cell::from("blk").fg(Color::Red),
                    "closed" => Cell::from("done").fg(Color::DarkGray),
                    other => Cell::from(other.to_string()).fg(Color::White),
                };

                Row::new([
                    Cell::from(meta.issue_label(issue)),
                    priority_cell,
                    Cell::from(issue.issue_type.as_deref().unwrap_or("--")),
                    status_cell,
                    Cell::from(issue.assignee.as_deref().unwrap_or("--")),
                    Cell::from(issue.title.as_str()),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(18),
            Constraint::Length(2),
            Constraint::Length(7),
            Constraint::Length(4),
            Constraint::Length(10),
            Constraint::Min(20),
        ];
        let table = Table::new(rows, widths)
            .header(header)
            .block(Block::bordered().title(title).border_style(border_style))
            .row_highlight_style(
                Style::default()
                    .fg(Color::White)
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );

        self.lineage_table_state.select(Some(display_selected_idx));
        frame.render_stateful_widget(table, area, &mut self.lineage_table_state);
    }

    pub fn computed_height(issue_count: usize, available_height: u16) -> u16 {
        let max_panel = (available_height / 4).max(3);
        issue_count.saturating_add(3).min(usize::from(max_panel)) as u16
    }
}

impl Default for IssuesPanel {
    fn default() -> Self {
        Self::new()
    }
}

struct LineageMeta<'a> {
    root_id: &'a str,
    blockers_by_id: HashMap<&'a str, Vec<&'a str>>,
    blocks_by_id: HashMap<&'a str, Vec<&'a str>>,
    children_by_parent_id: HashMap<&'a str, Vec<&'a str>>,
    node_by_id: HashMap<&'a str, &'a GraphNodeEvent>,
    open_blockers: usize,
}

impl<'a> LineageMeta<'a> {
    fn new(lineage: IssueLineageContext<'a>) -> Self {
        let node_by_id: HashMap<&str, &GraphNodeEvent> = lineage
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let mut blockers_by_id: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut blocks_by_id: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut parent_by_child_id: HashMap<&str, &str> = HashMap::new();
        let mut children_by_parent_id: HashMap<&str, Vec<&str>> = HashMap::new();
        for edge in lineage.edges {
            match edge.edge_type.as_deref() {
                Some("blocks") | None => {
                    blocks_by_id
                        .entry(edge.from.as_str())
                        .or_default()
                        .push(edge.to.as_str());
                    blockers_by_id
                        .entry(edge.to.as_str())
                        .or_default()
                        .push(edge.from.as_str());
                }
                Some("parent-child") => {
                    insert_parent_id(
                        &mut parent_by_child_id,
                        edge.from.as_str(),
                        edge.to.as_str(),
                    );
                    children_by_parent_id
                        .entry(edge.to.as_str())
                        .or_default()
                        .push(edge.from.as_str());
                }
                Some("related") if is_structural_related_edge(edge, &node_by_id) => {
                    insert_parent_id(
                        &mut parent_by_child_id,
                        edge.from.as_str(),
                        edge.to.as_str(),
                    );
                    children_by_parent_id
                        .entry(edge.to.as_str())
                        .or_default()
                        .push(edge.from.as_str());
                }
                Some(_) => {}
            }
        }
        let root_id = resolve_lineage_root(lineage.root_id, &parent_by_child_id, &node_by_id);
        let open_blockers = blockers_by_id
            .get(root_id)
            .into_iter()
            .flatten()
            .filter(|id| !is_closed(node_by_id.get(**id).copied()))
            .count();

        Self {
            root_id,
            blockers_by_id,
            blocks_by_id,
            children_by_parent_id,
            node_by_id,
            open_blockers,
        }
    }

    fn readiness(&self) -> String {
        if self.open_blockers > 0 {
            format!("blocked by open upstream ({})", self.open_blockers)
        } else {
            "ready".into()
        }
    }

    fn issue_label(&self, issue: &IssueSummary) -> String {
        let icon = status_icon(issue.status.as_str());
        if issue.id == self.root_id {
            let icon = if self.open_blockers > 0 { "!" } else { icon };
            return format!("> {icon} {}", issue.id);
        }

        if let Some(depth) = descendant_depth(self.root_id, issue.id.as_str()) {
            return format!("{} {icon} {}", lineage_prefix(depth), issue.id);
        }

        if self
            .children_by_parent_id
            .get(self.root_id)
            .is_some_and(|ids| ids.iter().any(|id| *id == issue.id))
        {
            return format!("├─ {icon} {}", issue.id);
        }

        if self
            .blockers_by_id
            .get(self.root_id)
            .is_some_and(|ids| ids.iter().any(|id| *id == issue.id))
        {
            return format!("├─ {icon} {}", issue.id);
        }

        if self
            .blocks_by_id
            .get(self.root_id)
            .is_some_and(|ids| ids.iter().any(|id| *id == issue.id))
        {
            return format!("└─ {icon} {}", issue.id);
        }

        if self.node_by_id.contains_key(issue.id.as_str()) {
            return format!("· {icon} {}", issue.id);
        }

        format!("  {icon} {}", issue.id)
    }

    fn ordered_issues<'b>(&self, issues: &'b [IssueSummary]) -> Vec<(usize, &'b IssueSummary)> {
        let mut ordered: Vec<(usize, &IssueSummary)> = issues.iter().enumerate().collect();
        ordered.sort_by(|(left_idx, left), (right_idx, right)| {
            self.issue_display_rank(left)
                .cmp(&self.issue_display_rank(right))
                .then_with(|| left_idx.cmp(right_idx))
        });
        ordered
    }

    fn issue_display_rank(&self, issue: &IssueSummary) -> (usize, usize) {
        if issue.id == self.root_id {
            return (0, 0);
        }

        if let Some(depth) = self.family_depth(issue.id.as_str()) {
            return (1, depth);
        }

        if self
            .children_by_parent_id
            .get(self.root_id)
            .is_some_and(|ids| ids.iter().any(|id| *id == issue.id))
        {
            return (1, 1);
        }

        if self
            .blockers_by_id
            .get(self.root_id)
            .is_some_and(|ids| ids.iter().any(|id| *id == issue.id))
        {
            return (2, 0);
        }

        if self
            .blocks_by_id
            .get(self.root_id)
            .is_some_and(|ids| ids.iter().any(|id| *id == issue.id))
        {
            return (3, 0);
        }

        if self.node_by_id.contains_key(issue.id.as_str()) {
            return (4, 0);
        }

        (5, 0)
    }

    fn family_depth(&self, issue_id: &str) -> Option<usize> {
        if let Some(depth) = descendant_depth(self.root_id, issue_id) {
            return Some(depth);
        }

        let family_root = issue_family_root(self.root_id);
        if family_root == self.root_id {
            return None;
        }
        descendant_depth(family_root, issue_id)
    }
}

fn resolve_lineage_root<'a>(
    requested_root_id: &'a str,
    parent_by_child_id: &HashMap<&'a str, &'a str>,
    node_by_id: &HashMap<&'a str, &'a GraphNodeEvent>,
) -> &'a str {
    shortest_prefix_ancestor(requested_root_id, node_by_id.values().copied())
        .unwrap_or_else(|| walk_parent_chain(requested_root_id, parent_by_child_id))
}

fn walk_parent_chain<'a>(
    start_id: &'a str,
    parent_by_child_id: &HashMap<&'a str, &'a str>,
) -> &'a str {
    let mut current_id = start_id;
    let mut seen = HashSet::new();
    while let Some(parent_id) = parent_by_child_id.get(current_id).copied() {
        if !seen.insert(current_id) || seen.contains(parent_id) {
            break;
        }
        current_id = parent_id;
    }
    current_id
}

fn shortest_prefix_ancestor<'a>(
    id: &str,
    candidates: impl IntoIterator<Item = &'a GraphNodeEvent>,
) -> Option<&'a str> {
    candidates
        .into_iter()
        .filter(|candidate| {
            is_plan_root_node(candidate) && descendant_depth(candidate.id.as_str(), id).is_some()
        })
        .min_by_key(|candidate| candidate.id.len())
        .map(|candidate| candidate.id.as_str())
}

fn is_structural_related_edge(
    edge: &GraphEdgeEvent,
    node_by_id: &HashMap<&str, &GraphNodeEvent>,
) -> bool {
    descendant_depth(edge.to.as_str(), edge.from.as_str()).is_some()
        || is_plan_membership_edge(edge, node_by_id)
}

fn is_plan_membership_edge(
    edge: &GraphEdgeEvent,
    node_by_id: &HashMap<&str, &GraphNodeEvent>,
) -> bool {
    let Some(child) = node_by_id.get(edge.from.as_str()) else {
        return false;
    };
    let Some(parent) = node_by_id.get(edge.to.as_str()) else {
        return false;
    };
    let Some(child_plan_id) = find_plan_id_label(&child.labels) else {
        return false;
    };
    find_plan_id_label(&parent.labels) == Some(child_plan_id)
        && has_plan_task_label(&child.labels)
        && is_plan_root_node(parent)
}

fn is_plan_root_node(node: &GraphNodeEvent) -> bool {
    has_label(&node.labels, "spur:plan-complete")
        || (find_plan_id_label(&node.labels).is_some() && !has_plan_task_label(&node.labels))
}

fn issue_family_root(id: &str) -> &str {
    id.rsplit_once('.').map(|(root, _)| root).unwrap_or(id)
}

fn lineage_prefix(depth: usize) -> String {
    if depth <= 1 {
        return "├─".into();
    }
    format!("{}├─", "│ ".repeat(depth.saturating_sub(1)))
}

fn is_closed(node: Option<&GraphNodeEvent>) -> bool {
    node.and_then(|node| node.status.as_deref()) == Some("closed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, style::Color, Terminal};
    use spur_pm::PmSource;

    fn issue(id: &str) -> IssueSummary {
        IssueSummary {
            id: id.into(),
            source: PmSource::Beads,
            title: id.into(),
            status: "open".into(),
            labels: Vec::new(),
            url: format!("beads://{id}"),
            priority: None,
            issue_type: None,
            assignee: None,
        }
    }

    fn rendered_row_with_cell_positions(
        terminal: &Terminal<TestBackend>,
        needle: &str,
    ) -> Option<(String, Vec<u16>, u16)> {
        let buf = terminal.backend().buffer();
        for y in 0..buf.area.height {
            let mut row = String::new();
            let mut cell_x_by_byte = Vec::new();
            for x in 0..buf.area.width {
                cell_x_by_byte.push((row.len(), x));
                row.push_str(buf[(x, y)].symbol());
            }
            if row.contains(needle) {
                let cell_positions = needle
                    .char_indices()
                    .filter_map(|(offset, _)| {
                        let byte_idx = row.find(needle)? + offset;
                        cell_x_by_byte
                            .iter()
                            .find_map(|(candidate, x)| (*candidate == byte_idx).then_some(*x))
                    })
                    .collect();
                return Some((row, cell_positions, y));
            }
        }
        None
    }

    #[test]
    fn computed_height_saturates_issue_count_before_panel_cap() {
        assert_eq!(IssuesPanel::computed_height(65_533, u16::MAX), u16::MAX / 4);
    }

    #[test]
    fn select_first_uses_first_visual_row_when_display_order_is_valid() {
        let issues = vec![issue("issue-A"), issue("issue-B"), issue("issue-C")];
        let mut panel = IssuesPanel::new();
        panel.display_order = vec![2, 0, 1];

        panel.select_first(issues.len());

        assert_eq!(panel.selected_id(&issues), Some("issue-C"));
    }

    #[test]
    fn select_first_with_stale_display_order_falls_back_to_zero() {
        let issues = vec![issue("issue-A"), issue("issue-B")];
        let mut panel = IssuesPanel::new();
        panel.display_order = vec![2, 0, 1];

        panel.select_first(issues.len());

        assert_eq!(panel.selected_id(&issues), Some("issue-A"));
    }

    #[test]
    fn select_last_uses_last_visual_row_when_display_order_is_valid() {
        let issues = vec![issue("issue-A"), issue("issue-B"), issue("issue-C")];
        let mut panel = IssuesPanel::new();
        panel.display_order = vec![2, 0, 1];

        panel.select_last(issues.len());

        assert_eq!(panel.selected_id(&issues), Some("issue-B"));
    }

    #[test]
    fn stale_display_order_with_out_of_bounds_index_is_ignored() {
        let issues = vec![issue("issue-A"), issue("issue-B"), issue("issue-C")];
        let mut panel = IssuesPanel::new();
        panel.display_order = vec![0, 3, 1];
        panel.table_state.select(Some(0));

        panel.select_next(1, issues.len());

        assert_eq!(panel.selected_id(&issues), Some("issue-B"));
    }

    #[test]
    fn selected_closed_issue_status_uses_contrasting_foreground() {
        let mut closed = issue("issue-A");
        closed.status = "closed".into();
        closed.priority = Some(3);
        let issues = vec![closed];
        let mut panel = IssuesPanel::new();
        panel.select_first(issues.len());
        let backend = TestBackend::new(80, 6);
        let mut terminal = Terminal::new(backend).expect("test backend");

        terminal
            .draw(|frame| panel.render(&issues, frame, frame.area()))
            .expect("render issues panel");

        let (row, status_cells, y) =
            rendered_row_with_cell_positions(&terminal, "done").expect("done status row");
        assert!(row.contains("done"), "rendered row: {row}");
        assert_eq!(status_cells.len(), "done".len(), "rendered row: {row}");
        let buf = terminal.backend().buffer();
        for x in status_cells {
            assert_eq!(buf[(x, y)].fg, Color::White, "rendered row: {row}");
        }
    }

    #[test]
    fn issue_selection_clamps_large_lists_and_wraps_small_lists() {
        let large_issues: Vec<_> = (0..21).map(|idx| issue(&format!("large-{idx}"))).collect();
        let small_issues: Vec<_> = (0..5).map(|idx| issue(&format!("small-{idx}"))).collect();
        let mut panel = IssuesPanel::new();

        panel.table_state.select(Some(20));
        panel.select_next(1, large_issues.len());
        assert_eq!(panel.selected_id(&large_issues), Some("large-20"));

        panel.table_state.select(Some(0));
        panel.select_prev(1, large_issues.len());
        assert_eq!(panel.selected_id(&large_issues), Some("large-0"));

        panel.table_state.select(Some(4));
        panel.select_next(1, small_issues.len());
        assert_eq!(panel.selected_id(&small_issues), Some("small-0"));

        panel.table_state.select(Some(0));
        panel.select_prev(1, small_issues.len());
        assert_eq!(panel.selected_id(&small_issues), Some("small-4"));
    }
}

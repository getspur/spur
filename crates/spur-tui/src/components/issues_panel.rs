use std::collections::HashMap;

use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style, Stylize},
    widgets::{Block, Cell, Row, Table, TableState},
    Frame,
};

use spur_acp::{GraphEdgeEvent, GraphNodeEvent};
use spur_pm::IssueSummary;

pub struct IssueLineageContext<'a> {
    pub root_id: &'a str,
    pub nodes: &'a [GraphNodeEvent],
    pub edges: &'a [GraphEdgeEvent],
}

pub enum IssueLineageView<'a> {
    Loaded(IssueLineageContext<'a>),
    Loading { root_id: &'a str },
}

pub struct IssuesPanel {
    table_state: TableState,
    focused: bool,
}

impl IssuesPanel {
    pub fn new() -> Self {
        Self {
            table_state: TableState::default(),
            focused: false,
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
        let current = self.table_state.selected().unwrap_or(0);
        let next = (current + count) % issue_count;
        self.table_state.select(Some(next));
    }

    pub fn select_prev(&mut self, count: usize, issue_count: usize) {
        if issue_count == 0 {
            return;
        }
        let current = self.table_state.selected().unwrap_or(0);
        let prev = (current + issue_count - (count % issue_count)) % issue_count;
        self.table_state.select(Some(prev));
    }

    pub fn select_first(&mut self) {
        self.table_state.select(Some(0));
    }

    pub fn select_last(&mut self, issue_count: usize) {
        if issue_count > 0 {
            self.table_state.select(Some(issue_count - 1));
        }
    }

    pub fn selected_id<'a>(&self, issues: &'a [IssueSummary]) -> Option<&'a str> {
        let idx = self.table_state.selected()?;
        issues.get(idx).map(|i| i.id.as_str())
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
                IssueLineageView::Loading { root_id } => {
                    let meta = LineageMeta::loading(root_id);
                    self.render_lineage(issues, meta, "loading work tree".into(), frame, area);
                    return;
                }
                IssueLineageView::Loaded(_) => {}
            }
        }

        let header = Row::new(["ID", "P", "Type", "Status", "Assignee", "Title"])
            .style(Style::default().bold());

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
                    " Work Item Lineage {}/{} · {} — [j/k] select · [Enter] detail · [E] execute ",
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
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(table, area, &mut self.table_state);
    }

    pub fn computed_height(issue_count: usize, available_height: u16) -> u16 {
        let max_panel = (available_height / 4).max(3);
        (issue_count as u16 + 3).min(max_panel)
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
        for edge in lineage.edges {
            if edge.edge_type.as_deref() == Some("blocks") || edge.edge_type.is_none() {
                blocks_by_id
                    .entry(edge.from.as_str())
                    .or_default()
                    .push(edge.to.as_str());
                blockers_by_id
                    .entry(edge.to.as_str())
                    .or_default()
                    .push(edge.from.as_str());
            }
        }
        let open_blockers = blockers_by_id
            .get(lineage.root_id)
            .into_iter()
            .flatten()
            .filter(|id| !is_closed(node_by_id.get(**id).copied()))
            .count();

        Self {
            root_id: lineage.root_id,
            blockers_by_id,
            blocks_by_id,
            node_by_id,
            open_blockers,
        }
    }

    fn loading(root_id: &'a str) -> Self {
        Self {
            root_id,
            blockers_by_id: HashMap::new(),
            blocks_by_id: HashMap::new(),
            node_by_id: HashMap::new(),
            open_blockers: 0,
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
}

fn status_icon(status: &str) -> &'static str {
    match status {
        "closed" => "✅",
        "in_progress" => "●",
        "blocked" => "!",
        _ => "○",
    }
}

fn is_closed(node: Option<&GraphNodeEvent>) -> bool {
    node.and_then(|node| node.status.as_deref()) == Some("closed")
}

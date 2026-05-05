use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Cell, Row, Table, TableState},
    Frame,
};

use spur_pm::IssueSummary;

/// Find the longest tracked-issue id that is a dot-prefix ancestor of `id`.
/// Used to surface "↳ <parent>" annotations in the flat issues panel without
/// reintroducing the lineage rendering surface (see `bd-2u0n.13`). Pure id
/// string analysis — no graph_cache dependency, no render-time mutation.
fn parent_id_for_prefix_child<'a>(id: &str, issues: &'a [IssueSummary]) -> Option<&'a str> {
    issues
        .iter()
        .filter(|other| other.id.as_str() != id)
        .filter_map(|other| {
            id.strip_prefix(other.id.as_str())
                .filter(|suffix| suffix.starts_with('.'))
                .map(|_| other.id.as_str())
        })
        .max_by_key(|parent_id| parent_id.len())
}

pub struct IssuesPanel {
    // Selection state invariant:
    // - `table_state` stores the source index into the `issues` slice.
    // - `selected_id` always reads `table_state`; never treat any other field
    //   as a source index.
    table_state: TableState,
    focused: bool,
    display_order: Vec<usize>,
}

impl IssuesPanel {
    const WRAPAROUND_LIMIT: usize = 20;

    pub fn new() -> Self {
        Self {
            table_state: TableState::default(),
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
        if issues.is_empty() {
            return;
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

                let title_cell = match parent_id_for_prefix_child(&issue.id, issues) {
                    Some(parent_id) => Cell::from(Line::from(vec![
                        Span::raw(issue.title.as_str()),
                        Span::styled(
                            format!("  ↳ {parent_id}"),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ])),
                    None => Cell::from(issue.title.as_str()),
                };

                Row::new([
                    Cell::from(issue.id.as_str()),
                    priority_cell,
                    Cell::from(issue.issue_type.as_deref().unwrap_or("--")),
                    status_cell,
                    Cell::from(issue.assignee.as_deref().unwrap_or("--")),
                    title_cell,
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
                    " Issues {}/{} — [j/k] select · [Enter] detail · [v] graph · [W]ork · [e] exec · [?] help ",
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
    fn focused_title_advertises_graph_toggle_and_help() {
        let issues = vec![issue("issue-A"), issue("issue-B")];
        let mut panel = IssuesPanel::new();
        panel.set_focused(true);
        let mut terminal = Terminal::new(TestBackend::new(160, 8)).unwrap();

        terminal
            .draw(|frame| panel.render(&issues, frame, frame.area()))
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut rendered = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                rendered.push_str(buf[(x, y)].symbol());
            }
        }

        assert!(
            rendered.contains("[v]"),
            "title should advertise graph toggle:\n{rendered}"
        );
        assert!(
            rendered.contains("[?]"),
            "title should advertise help:\n{rendered}"
        );
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
    fn parent_id_for_prefix_child_returns_longest_ancestor() {
        let issues = vec![
            issue("bd-root"),
            issue("bd-root.1"),
            issue("bd-root.1.2"),
            issue("bd-other"),
            issue("bd-1"),
            issue("bd-12"),
        ];

        assert_eq!(
            parent_id_for_prefix_child("bd-root.1.2", &issues),
            Some("bd-root.1"),
            "longest tracked prefix wins (bd-root.1 > bd-root)"
        );
        assert_eq!(
            parent_id_for_prefix_child("bd-root.1", &issues),
            Some("bd-root")
        );
        assert_eq!(
            parent_id_for_prefix_child("bd-root", &issues),
            None,
            "root has no prefix ancestor"
        );
        assert_eq!(
            parent_id_for_prefix_child("bd-other", &issues),
            None,
            "unrelated id has no prefix ancestor"
        );
        assert_eq!(
            parent_id_for_prefix_child("bd-12", &issues),
            None,
            "bd-1 must not match bd-12 (suffix must start with '.')"
        );
        assert_eq!(
            parent_id_for_prefix_child("bd-root.unknown", &issues),
            Some("bd-root"),
            "unseen child still annotates against tracked ancestor"
        );
    }

    #[test]
    fn render_appends_parent_annotation_to_title_for_prefix_child() {
        let issues = vec![issue("bd-root"), issue("bd-root.1")];
        let mut panel = IssuesPanel::new();
        let mut terminal = Terminal::new(TestBackend::new(120, 6)).unwrap();

        terminal
            .draw(|frame| panel.render(&issues, frame, frame.area()))
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut child_row = String::new();
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
            if row.contains("bd-root.1") && row.contains("↳ bd-root") {
                child_row = row;
                break;
            }
        }

        assert!(
            !child_row.is_empty(),
            "child row should annotate '↳ bd-root' next to its title"
        );
    }

    #[test]
    fn render_does_not_annotate_when_no_ancestor_is_tracked() {
        let issues = vec![issue("bd-orphan")];
        let mut panel = IssuesPanel::new();
        let mut terminal = Terminal::new(TestBackend::new(120, 6)).unwrap();

        terminal
            .draw(|frame| panel.render(&issues, frame, frame.area()))
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut whole = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                whole.push_str(buf[(x, y)].symbol());
            }
            whole.push('\n');
        }

        assert!(
            !whole.contains('↳'),
            "panel must not show a parent annotation when no ancestor is tracked:\n{whole}"
        );
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

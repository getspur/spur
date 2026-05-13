use ratatui::{
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, Cell, Row, Table, TableState},
    Frame,
};

use spur_pm::IssueSummary;

use crate::mentions::issue_search::push_issue_summary_search_text;

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
    filter_query: String,
    matcher: nucleo_matcher::Matcher,
    match_scratch: Vec<char>,
    search_scratch: String,
}

impl IssuesPanel {
    const WRAPAROUND_LIMIT: usize = 20;

    pub fn new() -> Self {
        Self {
            table_state: TableState::default(),
            focused: false,
            display_order: Vec::new(),
            filter_query: String::new(),
            matcher: nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT),
            match_scratch: Vec::new(),
            search_scratch: String::new(),
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
            self.table_state.select(None);
            return;
        }
        let current = self
            .table_state
            .selected()
            .unwrap_or(0)
            .min(issue_count.saturating_sub(1));
        let display_order = self.navigation_display_order(issue_count);
        let visible_count = display_order
            .map(|display_order| display_order.len())
            .unwrap_or(issue_count);
        if visible_count == 0 {
            self.table_state.select(None);
            return;
        }
        let current_position = display_order
            .map(|display_order| {
                display_order
                    .iter()
                    .position(|idx| *idx == current)
                    .unwrap_or(0)
            })
            .unwrap_or(current.min(visible_count - 1));
        let next_position = if visible_count <= Self::WRAPAROUND_LIMIT {
            (current_position + (count % visible_count)) % visible_count
        } else {
            current_position
                .saturating_add(count)
                .min(visible_count - 1)
        };
        let next = display_order
            .map(|display_order| display_order[next_position])
            .unwrap_or(next_position);
        self.table_state.select(Some(next));
    }

    pub fn select_prev(&mut self, count: usize, issue_count: usize) {
        if issue_count == 0 {
            self.table_state.select(None);
            return;
        }
        let current = self
            .table_state
            .selected()
            .unwrap_or(0)
            .min(issue_count.saturating_sub(1));
        let display_order = self.navigation_display_order(issue_count);
        let visible_count = display_order
            .map(|display_order| display_order.len())
            .unwrap_or(issue_count);
        if visible_count == 0 {
            self.table_state.select(None);
            return;
        }
        let current_position = display_order
            .map(|display_order| {
                display_order
                    .iter()
                    .position(|idx| *idx == current)
                    .unwrap_or(0)
            })
            .unwrap_or(current.min(visible_count - 1));
        let prev_position = if visible_count <= Self::WRAPAROUND_LIMIT {
            (current_position + visible_count - (count % visible_count)) % visible_count
        } else {
            current_position.saturating_sub(count)
        };
        let prev = display_order
            .map(|display_order| display_order[prev_position])
            .unwrap_or(prev_position);
        self.table_state.select(Some(prev));
    }

    pub fn select_first(&mut self, issue_count: usize) {
        if issue_count == 0 {
            self.table_state.select(None);
            return;
        }
        match self.navigation_display_order(issue_count) {
            Some(display_order) => self.table_state.select(display_order.first().copied()),
            None => self.table_state.select(Some(0)),
        }
    }

    pub fn select_last(&mut self, issue_count: usize) {
        if issue_count == 0 {
            self.table_state.select(None);
            return;
        }
        match self.navigation_display_order(issue_count) {
            Some(display_order) => self.table_state.select(display_order.last().copied()),
            None => self.table_state.select(Some(issue_count - 1)),
        }
    }

    pub fn selected_id<'a>(&self, issues: &'a [IssueSummary]) -> Option<&'a str> {
        let idx = self.table_state.selected()?;
        issues.get(idx).map(|i| i.id.as_str())
    }

    fn valid_display_order(&self, issue_count: usize) -> Option<&[usize]> {
        (self.display_order.len() <= issue_count
            && self.display_order.iter().all(|idx| *idx < issue_count))
        .then_some(self.display_order.as_slice())
    }

    fn navigation_display_order(&self, issue_count: usize) -> Option<&[usize]> {
        let display_order = self.valid_display_order(issue_count)?;
        if display_order.is_empty() && self.filter_query.is_empty() && issue_count > 0 {
            None
        } else {
            Some(display_order)
        }
    }

    fn render_display_order(&self, issue_count: usize) -> Vec<usize> {
        match self.valid_display_order(issue_count) {
            Some(display_order) if !display_order.is_empty() || !self.filter_query.is_empty() => {
                display_order.to_vec()
            }
            _ => (0..issue_count).collect(),
        }
    }

    pub fn set_issues(&mut self, issues: &[IssueSummary]) {
        self.recompute_display_order(issues);
    }

    pub fn set_filter(&mut self, query: &str, issues: &[IssueSummary]) {
        self.filter_query.clear();
        self.filter_query.push_str(query);
        self.recompute_display_order(issues);
    }

    pub fn filter_query(&self) -> &str {
        &self.filter_query
    }

    pub fn clear_filter(&mut self, issues: &[IssueSummary]) {
        self.filter_query.clear();
        self.recompute_display_order(issues);
    }

    // Invariant: this is the only production mutator for display_order; it emits unique source indices from one issues enumeration.
    fn recompute_display_order(&mut self, issues: &[IssueSummary]) {
        use nucleo_matcher::{
            pattern::{CaseMatching, Normalization, Pattern},
            Utf32Str,
        };

        let selected = self.table_state.selected();
        self.display_order.clear();

        if self.filter_query.is_empty() {
            self.display_order.extend(0..issues.len());
        } else {
            let pattern = Pattern::parse(
                &self.filter_query,
                CaseMatching::Ignore,
                Normalization::Smart,
            );
            let mut ranked = Vec::with_capacity(issues.len());

            for (idx, issue) in issues.iter().enumerate() {
                self.search_scratch.clear();
                push_issue_summary_search_text(&mut self.search_scratch, issue);

                self.match_scratch.clear();
                let haystack = Utf32Str::new(&self.search_scratch, &mut self.match_scratch);
                if let Some(score) = pattern.score(haystack, &mut self.matcher) {
                    ranked.push((score, idx));
                }
            }

            ranked.sort_by(|(score_a, idx_a), (score_b, idx_b)| {
                score_b.cmp(score_a).then_with(|| idx_a.cmp(idx_b))
            });
            self.display_order
                .extend(ranked.into_iter().map(|(_, idx)| idx));
        }

        let next_selection = selected
            .filter(|idx| self.display_order.contains(idx))
            .or_else(|| self.display_order.first().copied());
        self.table_state.select(next_selection);
    }

    /// Inc 3 (bd-d587.3): select the row whose id matches `id`. Returns `true`
    /// if the id was found and selection was moved; `false` otherwise (caller
    /// can stash the id for a later retry once `tracked_issues` is updated).
    pub fn select_by_id(&mut self, id: &str, issues: &[IssueSummary]) -> bool {
        if let Some(idx) = issues.iter().position(|i| i.id == id) {
            if !self.filter_query.is_empty()
                && !self
                    .valid_display_order(issues.len())
                    .is_some_and(|display_order| display_order.contains(&idx))
            {
                return false;
            }
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
        let display_order = self.render_display_order(issues.len());
        let rows: Vec<Row> = if !self.filter_query.is_empty() && display_order.is_empty() {
            vec![Row::new([
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from(""),
                Cell::from("No matches"),
            ])
            .style(Style::default().fg(Color::DarkGray))]
        } else {
            display_order
                .iter()
                .map(|idx| {
                    let issue = &issues[*idx];
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
                .collect()
        };

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
        let visible = display_order.len();
        let (border_style, title) = if self.focused {
            let count = if self.filter_query.is_empty() {
                format!("{}/{}", selected_idx + 1, total)
            } else {
                format!("{}/{} (filter: {})", visible, total, self.filter_query)
            };
            let title = format!(
                " Issues {count} — [j/k] select · [Enter] detail · [v] graph · [W]ork · [e] exec · [?] help ",
            );
            (Style::default().fg(Color::Cyan), title)
        } else if self.filter_query.is_empty() {
            (
                Style::default(),
                format!(" Issues {}/{} ", selected_idx + 1, total),
            )
        } else {
            (
                Style::default(),
                format!(
                    " Issues {}/{} (filter: {}) ",
                    visible, total, self.filter_query
                ),
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

        let mut render_state = TableState::default();
        let selected_visible = self
            .table_state
            .selected()
            .and_then(|source_idx| display_order.iter().position(|idx| *idx == source_idx));
        render_state.select(selected_visible);

        frame.render_stateful_widget(table, area, &mut render_state);
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
            description: None,
        }
    }

    fn issue_with(
        id: &str,
        title: &str,
        status: &str,
        labels: &[&str],
        issue_type: Option<&str>,
        assignee: Option<&str>,
    ) -> IssueSummary {
        let mut issue = issue(id);
        issue.title = title.into();
        issue.status = status.into();
        issue.labels = labels.iter().map(|label| (*label).into()).collect();
        issue.issue_type = issue_type.map(String::from);
        issue.assignee = assignee.map(String::from);
        issue
    }

    fn rendered_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut rendered = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                rendered.push_str(buf[(x, y)].symbol());
            }
            rendered.push('\n');
        }
        rendered
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
    fn filter_query_returns_current_query() {
        let issues = vec![issue("bd-auth")];
        let mut panel = IssuesPanel::new();

        panel.set_filter("auth", &issues);

        assert_eq!(panel.filter_query(), "auth");
    }

    #[test]
    fn set_filter_ranks_id_title_labels_assignee_type_status() {
        let issues = vec![
            issue_with("needle-id", "plain", "open", &[], None, None),
            issue_with("bd-title", "needle title", "open", &[], None, None),
            issue_with("bd-label", "plain", "open", &["needle"], None, None),
            issue_with(
                "bd-assignee",
                "plain",
                "open",
                &[],
                None,
                Some("needle-owner"),
            ),
            issue_with("bd-type", "plain", "open", &[], Some("needle-task"), None),
            issue_with("bd-status", "plain", "needle-status", &[], None, None),
            issue_with(
                "bd-miss",
                "plain",
                "open",
                &["other"],
                Some("task"),
                Some("owner"),
            ),
        ];
        let mut panel = IssuesPanel::new();

        panel.set_filter("needle", &issues);

        let matched_ids = panel
            .display_order
            .iter()
            .map(|idx| issues[*idx].id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(matched_ids.len(), 6);
        assert_eq!(matched_ids.first(), Some(&"needle-id"));
        assert!(matched_ids.contains(&"bd-title"));
        assert!(matched_ids.contains(&"bd-label"));
        assert!(matched_ids.contains(&"bd-assignee"));
        assert!(matched_ids.contains(&"bd-type"));
        assert!(matched_ids.contains(&"bd-status"));
        assert!(!matched_ids.contains(&"bd-miss"));
    }

    #[test]
    fn set_filter_prefers_direct_id_match_over_title_subsequence_match() {
        let issues = vec![
            issue_with("bd-d587.3", "plain", "open", &[], None, None),
            issue_with(
                "bd-other",
                "dump 5 logs 87 times 3 quickly",
                "open",
                &[],
                None,
                None,
            ),
        ];
        let mut panel = IssuesPanel::new();

        panel.set_filter("d587", &issues);

        let matched_ids = panel
            .display_order
            .iter()
            .map(|idx| issues[*idx].id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(matched_ids, vec!["bd-d587.3", "bd-other"]);
    }

    #[test]
    fn set_filter_preserves_selection_when_row_remains_visible() {
        let issues = vec![
            issue_with("bd-auth-1", "Auth work", "open", &[], None, None),
            issue_with("bd-billing", "Billing work", "open", &[], None, None),
            issue_with("bd-auth-2", "Second auth task", "open", &[], None, None),
        ];
        let mut panel = IssuesPanel::new();
        panel.table_state.select(Some(2));

        panel.set_filter("auth", &issues);

        assert_eq!(panel.selected_id(&issues), Some("bd-auth-2"));
    }

    #[test]
    fn set_filter_jumps_to_first_match_when_selection_filtered_out() {
        let issues = vec![
            issue_with("bd-auth-1", "Auth work", "open", &[], None, None),
            issue_with("bd-billing", "Billing work", "open", &[], None, None),
            issue_with("bd-auth-2", "Second auth task", "open", &[], None, None),
        ];
        let mut panel = IssuesPanel::new();
        panel.table_state.select(Some(1));

        panel.set_filter("auth", &issues);

        assert_eq!(
            panel.table_state.selected(),
            panel.display_order.first().copied()
        );
    }

    #[test]
    fn set_filter_empty_query_restores_full_order_and_keeps_selection() {
        let issues = vec![
            issue_with("bd-auth-1", "Auth work", "open", &[], None, None),
            issue_with("bd-billing", "Billing work", "open", &[], None, None),
            issue_with("bd-auth-2", "Second auth task", "open", &[], None, None),
        ];
        let mut panel = IssuesPanel::new();
        panel.table_state.select(Some(2));
        panel.set_filter("auth", &issues);

        panel.set_filter("", &issues);

        assert_eq!(panel.display_order, vec![0, 1, 2]);
        assert_eq!(panel.selected_id(&issues), Some("bd-auth-2"));
    }

    #[test]
    fn set_filter_no_matches_renders_no_matches_placeholder() {
        let issues = vec![issue("bd-1"), issue("bd-2")];
        let mut panel = IssuesPanel::new();
        panel.set_filter("does-not-match", &issues);
        let mut terminal = Terminal::new(TestBackend::new(100, 6)).unwrap();

        terminal
            .draw(|frame| panel.render(&issues, frame, frame.area()))
            .unwrap();

        let rendered = rendered_text(&terminal);
        assert!(rendered.contains("No matches"), "rendered:\n{rendered}");
        assert!(
            rendered.contains("(filter: does-not-match)"),
            "title should render filter without debug quotes:\n{rendered}"
        );
        assert!(
            !rendered.contains("(filter: \"does-not-match\")"),
            "title should not render filter with debug quotes:\n{rendered}"
        );
        assert!(!rendered.contains("bd-1"), "rendered:\n{rendered}");
        let (row, cells, y) =
            rendered_row_with_cell_positions(&terminal, "No matches").expect("placeholder row");
        let buf = terminal.backend().buffer();
        for x in cells {
            assert_eq!(buf[(x, y)].fg, Color::DarkGray, "rendered row: {row}");
        }
    }

    #[test]
    fn select_next_under_filter_wraps_within_visible_count() {
        let mut issues: Vec<_> = (0..25).map(|idx| issue(&format!("bd-{idx}"))).collect();
        issues[3].labels.push("keep".into());
        issues[24].labels.push("keep".into());
        let mut panel = IssuesPanel::new();
        panel.set_filter("keep", &issues);
        let first_visible = panel.display_order.first().copied().expect("first match");
        let last_visible = panel.display_order.last().copied().expect("last match");
        panel.table_state.select(Some(last_visible));

        panel.select_next(1, issues.len());

        assert_eq!(panel.table_state.selected(), Some(first_visible));
    }

    #[test]
    fn select_prev_under_filter_wraps_within_visible_count() {
        let mut issues: Vec<_> = (0..25).map(|idx| issue(&format!("bd-{idx}"))).collect();
        issues[3].labels.push("keep".into());
        issues[24].labels.push("keep".into());
        let mut panel = IssuesPanel::new();
        panel.set_filter("keep", &issues);
        let first_visible = panel.display_order.first().copied().expect("first match");
        let last_visible = panel.display_order.last().copied().expect("last match");
        panel.table_state.select(Some(first_visible));

        panel.select_prev(1, issues.len());

        assert_eq!(panel.table_state.selected(), Some(last_visible));
    }

    #[test]
    fn select_first_under_filter_picks_first_visible_match() {
        let mut issues: Vec<_> = (0..10).map(|idx| issue(&format!("bd-{idx}"))).collect();
        issues[3].labels.push("keep".into());
        issues[8].labels.push("keep".into());
        let mut panel = IssuesPanel::new();
        panel.set_filter("keep", &issues);
        panel.table_state.select(Some(8));

        panel.select_first(issues.len());

        assert_eq!(panel.selected_id(&issues), Some("bd-3"));
    }

    #[test]
    fn select_last_under_filter_picks_last_visible_match() {
        let mut issues: Vec<_> = (0..10).map(|idx| issue(&format!("bd-{idx}"))).collect();
        issues[3].labels.push("keep".into());
        issues[8].labels.push("keep".into());
        let mut panel = IssuesPanel::new();
        panel.set_filter("keep", &issues);
        panel.table_state.select(Some(3));

        panel.select_last(issues.len());

        assert_eq!(panel.selected_id(&issues), Some("bd-8"));
    }

    #[test]
    fn set_issues_preserves_active_filter_against_new_issue_list() {
        let mut issues: Vec<_> = (0..5).map(|idx| issue(&format!("old-{idx}"))).collect();
        issues[1].labels.push("keep".into());
        issues[3].labels.push("keep".into());
        let mut panel = IssuesPanel::new();
        panel.set_filter("keep", &issues);
        assert_eq!(panel.display_order, vec![1, 3]);

        let mut new_issues: Vec<_> = (0..6).map(|idx| issue(&format!("new-{idx}"))).collect();
        new_issues[2].labels.push("keep".into());
        new_issues[5].labels.push("keep".into());

        panel.set_issues(&new_issues);

        assert_eq!(panel.display_order, vec![2, 5]);
        assert_eq!(panel.selected_id(&new_issues), Some("new-2"));
        assert!(panel
            .table_state
            .selected()
            .is_some_and(|selected| panel.display_order.contains(&selected)));
    }

    #[test]
    fn select_by_id_with_filtered_out_target_returns_false_and_keeps_selection() {
        let mut issues = vec![issue("bd-hidden"), issue("bd-visible")];
        issues[1].labels.push("keep".into());
        let mut panel = IssuesPanel::new();
        panel.set_filter("keep", &issues);
        let selected_before = panel.selected_id(&issues);

        let selected = panel.select_by_id("bd-hidden", &issues);

        assert!(!selected);
        assert_eq!(panel.selected_id(&issues), selected_before);
    }

    #[test]
    fn render_translates_source_index_to_visible_row_for_highlight() {
        let issues = vec![
            issue_with("bd-src", "a u t h x detail task", "open", &[], None, None),
            issue_with("authx", "plain", "open", &[], None, None),
        ];
        let mut panel = IssuesPanel::new();
        panel.set_filter("authx", &issues);
        assert_eq!(panel.display_order, vec![1, 0]);
        panel.table_state.select(Some(0));
        let mut terminal = Terminal::new(TestBackend::new(100, 6)).unwrap();

        terminal
            .draw(|frame| panel.render(&issues, frame, frame.area()))
            .unwrap();

        let (selected_row, selected_cells, selected_y) =
            rendered_row_with_cell_positions(&terminal, "bd-src").expect("selected row");
        let (first_row, first_cells, first_y) =
            rendered_row_with_cell_positions(&terminal, "authx").expect("first row");
        let buf = terminal.backend().buffer();
        for x in selected_cells {
            assert_eq!(
                buf[(x, selected_y)].bg,
                Color::DarkGray,
                "rendered row: {selected_row}"
            );
        }
        for x in first_cells {
            assert_ne!(
                buf[(x, first_y)].bg,
                Color::DarkGray,
                "rendered row: {first_row}"
            );
        }
    }

    #[test]
    fn valid_display_order_accepts_filtered_subset() {
        let mut panel = IssuesPanel::new();
        panel.display_order = vec![2, 0];
        assert_eq!(panel.valid_display_order(4), Some([2, 0].as_slice()));

        panel.display_order = vec![2, 2];
        assert_eq!(panel.valid_display_order(4), Some([2, 2].as_slice()));

        panel.display_order = vec![2, 4];
        assert!(panel.valid_display_order(4).is_none());
    }

    #[test]
    fn focused_title_advertises_graph_toggle_and_help() {
        let issues = vec![issue("issue-A"), issue("issue-B")];
        let mut panel = IssuesPanel::new();
        panel.set_focused(true);
        panel.set_filter("issue", &issues);
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
        assert!(
            rendered.contains("(filter: issue)"),
            "title should render filter without debug quotes:\n{rendered}"
        );
        assert!(
            !rendered.contains("(filter: \"issue\")"),
            "title should not render filter with debug quotes:\n{rendered}"
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

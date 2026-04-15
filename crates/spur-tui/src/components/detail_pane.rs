use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Tabs},
    Frame,
};

use spur_core::{Artifact, ExecutorNode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Stream,
    Artifacts,
    Attempts,
    Task,
    Review,
}

impl DetailTab {
    pub fn all() -> &'static [DetailTab] {
        &[
            DetailTab::Stream,
            DetailTab::Artifacts,
            DetailTab::Attempts,
            DetailTab::Task,
            DetailTab::Review,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            DetailTab::Stream => "stream",
            DetailTab::Artifacts => "artifacts",
            DetailTab::Attempts => "attempts",
            DetailTab::Task => "task",
            DetailTab::Review => "review",
        }
    }
}

pub struct DetailPane {
    pub current_tab: DetailTab,
}

impl DetailPane {
    pub fn new() -> Self {
        Self {
            current_tab: DetailTab::Stream,
        }
    }

    pub fn cycle_tab(&mut self, forward: bool) {
        let all = DetailTab::all();
        let idx = all.iter().position(|t| *t == self.current_tab).unwrap_or(0);
        let next = if forward {
            (idx + 1) % all.len()
        } else {
            (idx + all.len() - 1) % all.len()
        };
        self.current_tab = all[next];
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, node: &ExecutorNode) {
        let block = Block::default()
            .title(format!(" {} ", node.agent))
            .borders(Borders::ALL);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);

        let titles: Vec<Line> = DetailTab::all()
            .iter()
            .map(|t| {
                let style = if *t == self.current_tab {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                Line::from(Span::styled(t.label(), style))
            })
            .collect();
        let tabs = Tabs::new(titles)
            .select(
                DetailTab::all()
                    .iter()
                    .position(|t| *t == self.current_tab)
                    .unwrap_or(0),
            )
            .divider("│");
        frame.render_widget(tabs, chunks[0]);

        let body_lines = match self.current_tab {
            DetailTab::Stream => self.render_stream(node),
            DetailTab::Artifacts => self.render_artifacts(node),
            DetailTab::Attempts => self.render_attempts(node),
            DetailTab::Task => self.render_task(node),
            DetailTab::Review => self.render_review(node),
        };
        let p = Paragraph::new(body_lines).wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(p, chunks[1]);
    }

    fn render_stream<'a>(&self, _node: &'a ExecutorNode) -> Vec<Line<'a>> {
        vec![Line::from(Span::styled(
            "(live stream — rebinding to focused-node view is a follow-up)",
            Style::default().fg(Color::DarkGray),
        ))]
    }

    fn render_artifacts<'a>(&self, node: &'a ExecutorNode) -> Vec<Line<'a>> {
        let mut out = Vec::new();
        for attempt in &node.attempts {
            for a in &attempt.artifacts {
                out.push(match a {
                    Artifact::Diff { summary, .. } => Line::from(format!(
                        "diff: {} files, +{} -{}",
                        summary.files_changed, summary.insertions, summary.deletions
                    )),
                    Artifact::PrUrl(u) => Line::from(format!("pr: {}", u)),
                    Artifact::FileList(f) => Line::from(format!("files: {}", f.len())),
                    Artifact::Text(t) => Line::from(t.clone()),
                });
            }
        }
        if out.is_empty() {
            out.push(Line::from(Span::styled(
                "(no artifacts yet)",
                Style::default().fg(Color::DarkGray),
            )));
        }
        out
    }

    fn render_attempts<'a>(&self, node: &'a ExecutorNode) -> Vec<Line<'a>> {
        node.attempts
            .iter()
            .enumerate()
            .map(|(i, a)| {
                Line::from(format!(
                    "#{}: {:?}  cost=${:.2}  session={}",
                    i + 1,
                    a.status,
                    a.cost_usd,
                    a.session_id.0
                ))
            })
            .collect()
    }

    fn render_task<'a>(&self, node: &'a ExecutorNode) -> Vec<Line<'a>> {
        if node.task_spec.is_empty() {
            vec![Line::from(Span::styled(
                "(no task spec captured)",
                Style::default().fg(Color::DarkGray),
            ))]
        } else {
            node.task_spec
                .lines()
                .map(|l| Line::from(l.to_string()))
                .collect()
        }
    }

    fn render_review(&self, node: &ExecutorNode) -> Vec<Line<'static>> {
        super::review_card::render_review(node)
    }
}

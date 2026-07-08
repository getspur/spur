use chrono::{DateTime, Utc};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use spur_core::explore::{
    apply,
    catalog::ItemKind,
    materialize::read_recent_materializations,
    pool::{self, ManifestItem, StatusReport},
};

use crate::action::{Action, ViewId};

use super::{sha7, ExploreBrowserView, ExploreStage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManageLens {
    Pool,
    LastMaterialization,
}

impl ExploreBrowserView {
    pub(super) fn handle_manage_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
                self.move_manage_selection(1);
                None
            }
            KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
                self.move_manage_selection(-1);
                None
            }
            KeyCode::Char('l') if key.modifiers.is_empty() => {
                self.manage_lens = match self.manage_lens {
                    ManageLens::Pool => ManageLens::LastMaterialization,
                    ManageLens::LastMaterialization => ManageLens::Pool,
                };
                self.manage_selected = 0;
                self.clamp_manage_selection();
                None
            }
            KeyCode::Char('m') if key.modifiers.is_empty() => {
                self.stage = ExploreStage::Browse;
                self.manage_selected = 0;
                None
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                self.reload();
                None
            }
            KeyCode::Char('x') if key.modifiers.is_empty() => {
                self.remove_selected_pool_item();
                None
            }
            KeyCode::Esc if key.modifiers.is_empty() => Some(Action::NavigateTo(ViewId::Dashboard)),
            _ => None,
        }
    }

    pub(super) fn render_manage(&mut self, frame: &mut Frame, area: Rect) {
        self.clamp_manage_selection();
        let lines = match self.manage_lens {
            ManageLens::Pool => self.pool_lines(),
            ManageLens::LastMaterialization => self.last_materialization_lines(),
        };
        frame.render_widget(
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .title(manage_title(self.manage_lens))
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    pub(super) fn clamp_manage_selection(&mut self) {
        let count = self.manage_row_count();
        if count == 0 {
            self.manage_selected = 0;
        } else {
            self.manage_selected = self.manage_selected.min(count - 1);
        }
    }

    fn move_manage_selection(&mut self, delta: isize) {
        let len = self.manage_row_count();
        if len == 0 {
            self.manage_selected = 0;
            return;
        }
        let current = self.manage_selected.min(len.saturating_sub(1));
        self.manage_selected = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current
                .saturating_add(delta as usize)
                .min(len.saturating_sub(1))
        };
    }

    fn manage_row_count(&self) -> usize {
        match self.manage_lens {
            ManageLens::Pool => {
                let report = pool::status(&self.repo_root, &self.manifest);
                self.manifest.items.len() + report.missing.len() + report.sha_mismatch.len()
            }
            ManageLens::LastMaterialization => {
                read_recent_materializations(&self.repo_root, 20).len()
            }
        }
    }

    fn remove_selected_pool_item(&mut self) {
        if self.manage_lens != ManageLens::Pool || self.manage_selected >= self.manifest.items.len()
        {
            return;
        }
        let name = self.manifest.items[self.manage_selected].name.clone();
        match apply::remove(&self.repo_root, &mut self.manifest, &name) {
            Ok(()) => {
                self.reload();
            }
            Err(error) => {
                self.load_error = Some(format!("remove failed: {error:#}"));
            }
        }
    }

    fn pool_lines(&self) -> Vec<Line<'static>> {
        let report = pool::status(&self.repo_root, &self.manifest);
        let mut lines = vec![
            lens_tabs(self.manage_lens),
            Line::from("name  kind  sha  verdict  license"),
            Line::from(""),
        ];

        if self.manifest.items.is_empty() {
            lines.push(Line::from(Span::styled(
                "pool is empty",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (index, item) in self.manifest.items.iter().enumerate() {
                lines.push(pool_item_line(item, index == self.manage_selected));
            }
        }

        push_status_lines(
            &mut lines,
            &report,
            self.manifest.items.len(),
            self.manage_selected,
        );
        lines
    }

    fn last_materialization_lines(&self) -> Vec<Line<'static>> {
        let records = read_recent_materializations(&self.repo_root, 20);
        let mut lines = vec![
            lens_tabs(self.manage_lens),
            Line::from("newest first · last 20 dispatches"),
            Line::from(""),
        ];

        if records.is_empty() {
            lines.push(Line::from(Span::styled(
                "no materializations recorded",
                Style::default().fg(Color::DarkGray),
            )));
            return lines;
        }

        for (index, record) in records.iter().enumerate() {
            let selected = index == self.manage_selected;
            let prefix = if selected { "> " } else { "  " };
            let item_count = record.items.len();
            let item_label = if item_count == 1 { "skill" } else { "skills" };
            let items = if record.items.is_empty() {
                "none".to_string()
            } else {
                record.items.join(", ")
            };
            lines.push(Line::from(vec![
                Span::styled(prefix, Style::default().fg(Color::Cyan)),
                Span::styled(
                    format_epoch_hhmm(record.recorded_at_epoch),
                    row_style(selected),
                ),
                Span::raw(" · "),
                Span::styled(record.agent.clone(), row_style(selected)),
                Span::raw(" · "),
                Span::styled(record.delegation_id.clone(), row_style(selected)),
                Span::raw(" · "),
                Span::styled(
                    format!("{item_count} {item_label}: {items}"),
                    row_style(selected),
                ),
            ]));
        }

        lines
    }
}

fn manage_title(lens: ManageLens) -> &'static str {
    match lens {
        ManageLens::Pool => "Manage · Pool",
        ManageLens::LastMaterialization => "Manage · Last materialization",
    }
}

fn lens_tabs(active: ManageLens) -> Line<'static> {
    Line::from(vec![
        lens_span("Pool", active == ManageLens::Pool),
        Span::raw("  "),
        lens_span(
            "Last materialization",
            active == ManageLens::LastMaterialization,
        ),
    ])
}

fn lens_span(label: &'static str, active: bool) -> Span<'static> {
    if active {
        Span::styled(
            format!("[{label}]"),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(format!(" {label} "), Style::default().fg(Color::DarkGray))
    }
}

fn pool_item_line(item: &ManifestItem, selected: bool) -> Line<'static> {
    let prefix = if selected { "> " } else { "  " };
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(Color::Cyan)),
        Span::styled(item.name.clone(), row_style(selected)),
        Span::raw("  "),
        Span::styled(kind_label(item.kind), row_style(selected)),
        Span::raw("  "),
        Span::styled(sha7(&item.pinned_commit).to_string(), row_style(selected)),
        Span::raw("  "),
        Span::styled(item.gate.verdict.clone(), verdict_style(&item.gate.verdict)),
        Span::raw("  "),
        Span::styled(
            item.license
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            row_style(selected),
        ),
    ])
}

fn push_status_lines(
    lines: &mut Vec<Line<'static>>,
    report: &StatusReport,
    row_offset: usize,
    selected: usize,
) {
    if report.missing.is_empty() && report.sha_mismatch.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "status: all pool bodies present",
            Style::default().fg(Color::Green),
        )));
        return;
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "status findings",
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    )));
    let mut row = row_offset;
    for name in &report.missing {
        lines.push(status_line("missing body", name, row == selected));
        row += 1;
    }
    for name in &report.sha_mismatch {
        lines.push(status_line("sha mismatch", name, row == selected));
        row += 1;
    }
}

fn status_line(kind: &'static str, name: &str, selected: bool) -> Line<'static> {
    let prefix = if selected { "> " } else { "  " };
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(Color::Cyan)),
        Span::styled("⚠ ", Style::default().fg(Color::Yellow)),
        Span::styled(kind, Style::default().fg(Color::Yellow)),
        Span::raw("  "),
        Span::styled(name.to_string(), row_style(selected)),
    ])
}

fn row_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    }
}

fn verdict_style(verdict: &str) -> Style {
    match verdict {
        "clean" => Style::default().fg(Color::Green),
        "overridden" | "replaced-bundled" => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::Red),
    }
}

fn kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Skill => "skill",
        ItemKind::Agent => "agent",
    }
}

fn format_epoch_hhmm(epoch: u64) -> String {
    let Some(timestamp) = DateTime::<Utc>::from_timestamp(epoch as i64, 0) else {
        return epoch.to_string();
    };
    timestamp.format("%H:%M").to_string()
}

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use spur_acp::SpurEvent;
use spur_core::explore::{
    catalog::{Catalog, CatalogEntry, ItemKind},
    pool::{pool_dir, Manifest},
};

use crate::action::{Action, ViewId};

use super::{View, ViewContext};

mod manage;
pub use manage::ManageLens;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExploreTab {
    Skills,
    Agents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExploreStage {
    Browse,
    Gate,
    Manage,
}

pub struct ExploreBrowserView {
    pub(crate) repo_root: PathBuf,
    pub(crate) tab: ExploreTab,
    pub(crate) stage: ExploreStage,
    pub(crate) manage_lens: ManageLens,
    pub(crate) manage_selected: usize,
    pub(crate) catalog: Catalog,
    pub(crate) manifest: Manifest,
    pub(crate) selected: usize,
    pub(crate) starred: BTreeSet<String>,
    pub(crate) load_error: Option<String>,
}

impl ExploreBrowserView {
    pub fn new(repo_root: PathBuf) -> Self {
        let (catalog, manifest, load_error) = load_state(&repo_root);
        Self {
            repo_root,
            tab: ExploreTab::Skills,
            stage: ExploreStage::Browse,
            manage_lens: ManageLens::Pool,
            manage_selected: 0,
            catalog,
            manifest,
            selected: 0,
            starred: BTreeSet::new(),
            load_error,
        }
    }

    pub fn visible_entries(&self) -> Vec<&CatalogEntry> {
        let kind = match self.tab {
            ExploreTab::Skills => ItemKind::Skill,
            ExploreTab::Agents => ItemKind::Agent,
        };
        self.catalog
            .entries
            .iter()
            .filter(|entry| entry.kind == kind)
            .collect()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        let key = super::normalize_macos_option(key);
        if self.stage == ExploreStage::Manage {
            return self.handle_manage_key(key);
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
                self.move_selection(1);
                None
            }
            KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
                self.move_selection(-1);
                None
            }
            KeyCode::Tab if key.modifiers.is_empty() => {
                self.toggle_tab();
                None
            }
            KeyCode::Char(' ') if key.modifiers.is_empty() => {
                self.toggle_starred();
                None
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                self.reload();
                None
            }
            KeyCode::Char('m') if key.modifiers.is_empty() => {
                self.stage = ExploreStage::Manage;
                self.manage_lens = ManageLens::Pool;
                self.manage_selected = 0;
                self.clamp_manage_selection();
                None
            }
            KeyCode::Esc if key.modifiers.is_empty() => Some(Action::NavigateTo(ViewId::Dashboard)),
            _ => None,
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &ViewContext) {
        <Self as View>::render(self, frame, area, ctx);
    }

    fn reload(&mut self) {
        let (catalog, manifest, load_error) = load_state(&self.repo_root);
        self.catalog = catalog;
        self.manifest = manifest;
        self.load_error = load_error;
        self.clamp_selection();
        self.clamp_manage_selection();
    }

    fn toggle_tab(&mut self) {
        self.tab = match self.tab {
            ExploreTab::Skills => ExploreTab::Agents,
            ExploreTab::Agents => ExploreTab::Skills,
        };
        self.selected = 0;
        self.clamp_selection();
    }

    fn toggle_starred(&mut self) {
        let Some(name) = self.selected_entry().map(|entry| entry.name.clone()) else {
            return;
        };
        if !self.starred.remove(&name) {
            self.starred.insert(name);
        }
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.visible_entries().len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        let current = self.selected.min(len.saturating_sub(1));
        self.selected = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current
                .saturating_add(delta as usize)
                .min(len.saturating_sub(1))
        };
    }

    fn clamp_selection(&mut self) {
        let len = self.visible_entries().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(len - 1);
        }
    }

    fn selected_entry(&self) -> Option<&CatalogEntry> {
        self.visible_entries().get(self.selected).copied()
    }

    fn is_in_pool(&self, entry: &CatalogEntry) -> bool {
        self.manifest
            .items
            .iter()
            .any(|item| item.name == entry.name && item.kind == entry.kind)
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let title = Line::from(vec![
            Span::styled(
                "Explore",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            tab_span("Skills", self.tab == ExploreTab::Skills),
            Span::raw(" "),
            tab_span("Agents", self.tab == ExploreTab::Agents),
            Span::raw("  "),
            Span::styled(
                stage_label(self.stage),
                Style::default().fg(Color::DarkGray),
            ),
        ]);

        let mut lines = vec![title, Line::from(sync_banner(&self.catalog))];
        if let Some(error) = self.load_error.as_deref() {
            lines.push(Line::from(Span::styled(
                format!("load warning: {error}"),
                Style::default().fg(Color::Yellow),
            )));
        }

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
    }

    fn render_sources(&self, frame: &mut Frame, area: Rect) {
        let mut source_counts = BTreeMap::<String, usize>::new();
        for entry in &self.catalog.entries {
            *source_counts.entry(entry.source.clone()).or_default() += 1;
        }

        let mut lines = vec![
            Line::from(format!("catalog: {} items", self.catalog.entries.len())),
            Line::from(format!("pool: {} items", self.manifest.items.len())),
            Line::from(""),
        ];
        if source_counts.is_empty() {
            lines.push(Line::from(Span::styled(
                "no sources synced",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (source, count) in source_counts {
                lines.push(Line::from(vec![
                    Span::styled(source, Style::default().fg(Color::White)),
                    Span::raw(format!("  {count}")),
                ]));
            }
        }

        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().title("Sources").borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn render_catalog(&self, frame: &mut Frame, area: Rect) {
        let entries = self.visible_entries();
        let mut lines = Vec::new();
        if entries.is_empty() {
            lines.push(Line::from(Span::styled(
                empty_catalog_message(self.tab),
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (index, entry) in entries.iter().enumerate() {
                let selected = index == self.selected;
                let mut spans = vec![
                    Span::styled(
                        if selected { "> " } else { "  " },
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::styled(
                        entry.name.as_str(),
                        if selected {
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::White)
                        },
                    ),
                ];
                if self.is_in_pool(entry) {
                    spans.push(Span::styled("  in pool", Style::default().fg(Color::Green)));
                }
                if self.starred.contains(entry.name.as_str()) {
                    spans.push(Span::styled("  ★", Style::default().fg(Color::Yellow)));
                }
                lines.push(Line::from(spans));
                lines.push(Line::from(Span::styled(
                    format!("    {}", entry.description),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        frame.render_widget(
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .title(catalog_title(self.tab))
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn render_preview(&self, frame: &mut Frame, area: Rect) {
        let mut lines = Vec::new();
        if let Some(entry) = self.selected_entry() {
            lines.push(Line::from(Span::styled(
                entry.name.as_str(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(entry.description.as_str()));
            lines.push(Line::from(""));
            lines.push(Line::from(format!("source: {}", entry.source)));
            lines.push(Line::from(format!("pin: {}", sha7(&entry.pinned_commit))));
            lines.push(Line::from(format!(
                "license: {}",
                entry.license.as_deref().unwrap_or("unknown")
            )));
            lines.push(Line::from(format!("path: {}", entry.rel_path)));
            lines.push(Line::from(""));
            lines.extend(self.preview_body_lines(entry));
        } else {
            lines.push(Line::from(Span::styled(
                "select an item to preview",
                Style::default().fg(Color::DarkGray),
            )));
        }

        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().title("Preview").borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Paragraph::new(match self.stage {
                ExploreStage::Manage => "j/k move  l lens  x remove  m browse  r reload  Esc back",
                _ => "j/k move  Tab tabs  space select  r reload  Esc back",
            })
            .style(Style::default().fg(Color::DarkGray)),
            area,
        );
    }

    fn preview_body_lines(&self, entry: &CatalogEntry) -> Vec<Line<'static>> {
        let Some(path) = pooled_body_path(&self.repo_root, entry) else {
            return vec![Line::from("sync to fetch bodies — spur explore sync")];
        };
        let Ok(raw) = std::fs::read_to_string(path) else {
            return vec![Line::from("sync to fetch bodies — spur explore sync")];
        };
        let mut lines: Vec<Line<'static>> = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && *line != "---")
            .take(8)
            .map(|line| Line::from(line.to_string()))
            .collect();
        if lines.is_empty() {
            lines.push(Line::from("vendored body is empty"));
        }
        lines
    }
}

impl View for ExploreBrowserView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &ViewContext) -> Option<Action> {
        self.handle_key(key)
    }

    fn handle_spur_event(&mut self, _event: &SpurEvent, _ctx: &ViewContext) {}

    fn render(&mut self, frame: &mut Frame, area: Rect, _ctx: &ViewContext) {
        self.clamp_selection();
        let chunks = Layout::vertical([
            Constraint::Length(header_height(self.load_error.is_some())),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(area);
        self.render_header(frame, chunks[0]);
        if self.stage == ExploreStage::Manage {
            self.render_manage(frame, chunks[1]);
            self.render_footer(frame, chunks[2]);
            return;
        }
        let panes = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(24),
                Constraint::Percentage(38),
                Constraint::Min(30),
            ])
            .split(chunks[1]);

        self.render_sources(frame, panes[0]);
        self.render_catalog(frame, panes[1]);
        self.render_preview(frame, panes[2]);
        self.render_footer(frame, chunks[2]);
    }

    fn tick(&mut self) {}
}

fn load_state(repo_root: &Path) -> (Catalog, Manifest, Option<String>) {
    let mut errors = Vec::new();
    let catalog = match Catalog::load(repo_root) {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::warn!(%error, "explore catalog load failed");
            errors.push(format!("catalog: {error:#}"));
            Catalog::default()
        }
    };
    let manifest = match Manifest::load(repo_root) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(%error, "explore manifest load failed");
            errors.push(format!("manifest: {error:#}"));
            Manifest::default()
        }
    };
    let load_error = (!errors.is_empty()).then(|| errors.join("; "));
    (catalog, manifest, load_error)
}

fn tab_span(label: &'static str, active: bool) -> Span<'static> {
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

fn stage_label(stage: ExploreStage) -> &'static str {
    match stage {
        ExploreStage::Browse => "Browse",
        ExploreStage::Gate => "Gate",
        ExploreStage::Manage => "Manage",
    }
}

fn header_height(has_error: bool) -> u16 {
    if has_error {
        3
    } else {
        2
    }
}

fn catalog_title(tab: ExploreTab) -> &'static str {
    match tab {
        ExploreTab::Skills => "Catalog · Skills",
        ExploreTab::Agents => "Catalog · Agents",
    }
}

fn empty_catalog_message(tab: ExploreTab) -> &'static str {
    match tab {
        ExploreTab::Skills => "no skills in catalog",
        ExploreTab::Agents => "no agents in catalog",
    }
}

fn sync_banner(catalog: &Catalog) -> String {
    match catalog.synced_at_epoch {
        None => "never synced".to_string(),
        Some(epoch) => format!("synced {} ago", elapsed_label(epoch)),
    }
}

fn elapsed_label(epoch: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(epoch);
    let elapsed = now.saturating_sub(epoch);
    match elapsed {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{}m", elapsed / 60),
        3_600..=86_399 => format!("{}h", elapsed / 3_600),
        _ => format!("{}d", elapsed / 86_400),
    }
}

fn sha7(value: &str) -> &str {
    value.get(..7).unwrap_or(value)
}

fn pooled_body_path(repo_root: &Path, entry: &CatalogEntry) -> Option<PathBuf> {
    let dir = pool_dir(repo_root, &entry.source, &entry.name, &entry.pinned_commit);
    match entry.kind {
        ItemKind::Skill => Some(dir.join("SKILL.md")),
        ItemKind::Agent => Path::new(&entry.rel_path)
            .file_name()
            .map(|file_name| dir.join(file_name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use spur_core::explore::pool::{item_from_entry, GateRecord};

    fn key(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
    }

    fn sample_entry() -> CatalogEntry {
        CatalogEntry {
            kind: ItemKind::Skill,
            name: "review-helper".into(),
            source: "getspur/ecosystem".into(),
            rel_path: "skills/review-helper".into(),
            pinned_commit: "0123456789abcdef".into(),
            description: "Tightens code review with focused risk checks.".into(),
            license: Some("MIT".into()),
            content_sha256: "0".repeat(64),
        }
    }

    fn repo_with_pool_item() -> tempfile::TempDir {
        let repo = tempfile::tempdir().expect("temp repo");
        let entry = sample_entry();
        Catalog {
            synced_at_epoch: None,
            entries: vec![entry.clone()],
        }
        .save(repo.path())
        .expect("save catalog");
        Manifest {
            sources: Vec::new(),
            items: vec![item_from_entry(
                &entry,
                GateRecord {
                    verdict: "clean".into(),
                    justification: None,
                    decided_at_epoch: Some(1_700_000_000),
                },
            )],
        }
        .save(repo.path())
        .expect("save manifest");
        repo
    }

    #[test]
    fn m_from_browse_enters_manage_and_x_removes_selected_pool_item() {
        let repo = repo_with_pool_item();
        let mut view = ExploreBrowserView::new(repo.path().to_path_buf());

        view.handle_key(key('m'));

        assert_eq!(view.stage, ExploreStage::Manage);
        view.handle_key(key('x'));
        let manifest = Manifest::load(repo.path()).expect("reload manifest");
        assert!(
            manifest.items.is_empty(),
            "x should remove the selected pool item from disk"
        );
    }
}

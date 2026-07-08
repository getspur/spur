use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use spur_core::explore::apply::{Resolution, Selection};
use spur_core::explore::catalog::CatalogEntry;
use spur_core::explore::gate::{self, Verdict};
use spur_core::explore::pool::pool_dir;

pub(crate) struct GateState {
    pub(crate) cards: Vec<GateCard>,
    pub(crate) selected: usize,
    override_input: Option<String>,
}

pub(crate) struct GateCard {
    pub(crate) entry: CatalogEntry,
    verdict: GateVerdict,
    pub(crate) resolution: Option<Resolution>,
}

#[derive(Clone)]
enum GateVerdict {
    Ready(Verdict),
    Unresolved(String),
}

pub(crate) enum GateAction {
    None,
    Apply,
    Back,
    Error(String),
}

impl GateState {
    pub(crate) fn new(cards: Vec<GateCard>) -> Self {
        Self {
            cards,
            selected: 0,
            override_input: None,
        }
    }

    pub(crate) fn from_starred(
        repo_root: &Path,
        entries: &[CatalogEntry],
        starred: &BTreeSet<String>,
        bundled_ids: &[String],
    ) -> Self {
        let cards = entries
            .iter()
            .filter(|entry| starred.contains(entry.name.as_str()))
            .cloned()
            .map(|entry| GateCard::new(repo_root, entry, bundled_ids))
            .collect();
        Self::new(cards)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    pub(crate) fn resolved_selections(&self) -> Vec<Selection> {
        self.cards
            .iter()
            .filter(|card| card.is_evaluable())
            .filter_map(|card| {
                card.resolution.clone().map(|resolution| Selection {
                    entry: card.entry.clone(),
                    resolution,
                })
            })
            .collect()
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> GateAction {
        if self.override_input.is_some() {
            return self.handle_override_input(key);
        }

        match key.code {
            KeyCode::Char('A') => GateAction::Apply,
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::SHIFT) => GateAction::Apply,
            KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
                self.move_selection(1);
                GateAction::None
            }
            KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
                self.move_selection(-1);
                GateAction::None
            }
            KeyCode::Char('a') if key.modifiers.is_empty() => self.accept_selected(),
            KeyCode::Char('o') if key.modifiers.is_empty() => self.begin_override(),
            KeyCode::Char('b') if key.modifiers.is_empty() => self.replace_selected(),
            KeyCode::Char('s') if key.modifiers.is_empty() => self.skip_selected(),
            KeyCode::Esc if key.modifiers.is_empty() => GateAction::Back,
            _ => GateAction::None,
        }
    }

    pub(crate) fn render(&self, frame: &mut Frame, area: Rect) {
        let mut lines = Vec::new();
        if self.cards.is_empty() {
            lines.push(Line::from(Span::styled(
                "no gate cards",
                Style::default().fg(Color::DarkGray),
            )));
        } else {
            for (index, card) in self.cards.iter().enumerate() {
                if index > 0 {
                    lines.push(Line::from(""));
                }
                lines.extend(card.render_lines(index == self.selected));
            }
        }

        if let Some(input) = self.override_input.as_deref() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled(
                    "override justification: ",
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(input.to_string()),
            ]));
        }

        frame.render_widget(
            Paragraph::new(lines)
                .block(Block::default().title("Gate Cards").borders(Borders::ALL))
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn handle_override_input(&mut self, key: KeyEvent) -> GateAction {
        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => {
                let input = self.override_input.take().unwrap_or_default();
                let justification = input.trim().to_string();
                if justification.is_empty() {
                    self.override_input = Some(input);
                    return GateAction::Error("override justification is required".to_string());
                }
                if let Some(card) = self.selected_card_mut() {
                    card.resolution = Some(Resolution::Override { justification });
                }
                GateAction::None
            }
            KeyCode::Esc if key.modifiers.is_empty() => {
                self.override_input = None;
                GateAction::None
            }
            KeyCode::Backspace if key.modifiers.is_empty() => {
                if let Some(input) = self.override_input.as_mut() {
                    input.pop();
                }
                GateAction::None
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(input) = self.override_input.as_mut() {
                    input.push(ch);
                }
                GateAction::None
            }
            _ => GateAction::None,
        }
    }

    fn accept_selected(&mut self) -> GateAction {
        let Some(card) = self.selected_card_mut() else {
            return GateAction::None;
        };
        match card.verdict {
            GateVerdict::Ready(Verdict::Clean) => {
                card.resolution = Some(Resolution::Accept);
                GateAction::None
            }
            GateVerdict::Ready(_) => {
                GateAction::Error("accept is available only for clean cards".to_string())
            }
            GateVerdict::Unresolved(_) => {
                GateAction::Error("unresolved gate card cannot be accepted".to_string())
            }
        }
    }

    fn begin_override(&mut self) -> GateAction {
        let Some(card) = self.selected_card() else {
            return GateAction::None;
        };
        match card.verdict {
            GateVerdict::Ready(Verdict::Flagged { .. }) => {
                self.override_input = Some(String::new());
                GateAction::None
            }
            GateVerdict::Ready(_) => {
                GateAction::Error("override is available only for flagged cards".to_string())
            }
            GateVerdict::Unresolved(_) => {
                GateAction::Error("unresolved gate card cannot be overridden".to_string())
            }
        }
    }

    fn replace_selected(&mut self) -> GateAction {
        let Some(card) = self.selected_card_mut() else {
            return GateAction::None;
        };
        match card.verdict {
            GateVerdict::Ready(Verdict::Conflict { .. }) => {
                card.resolution = Some(Resolution::ReplaceBundled);
                GateAction::None
            }
            GateVerdict::Ready(_) => {
                GateAction::Error("replace bundled is available only for conflicts".to_string())
            }
            GateVerdict::Unresolved(_) => {
                GateAction::Error("unresolved gate card cannot replace bundled".to_string())
            }
        }
    }

    fn skip_selected(&mut self) -> GateAction {
        if let Some(card) = self.selected_card_mut() {
            card.resolution = Some(Resolution::Skip);
        }
        GateAction::None
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.cards.len();
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

    fn selected_card(&self) -> Option<&GateCard> {
        self.cards.get(self.selected)
    }

    fn selected_card_mut(&mut self) -> Option<&mut GateCard> {
        self.cards.get_mut(self.selected)
    }
}

impl Default for GateState {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl GateCard {
    fn new(repo_root: &Path, entry: CatalogEntry, bundled_ids: &[String]) -> Self {
        let source_path = gate_path(repo_root, &entry);
        let verdict = if source_path.exists() {
            GateVerdict::Ready(gate::evaluate(&entry.name, &source_path, bundled_ids))
        } else {
            GateVerdict::Unresolved(format!(
                "missing cache checkout {}; run `spur explore sync`",
                source_path.display()
            ))
        };
        Self {
            entry,
            verdict,
            resolution: None,
        }
    }

    fn render_lines(&self, selected: bool) -> Vec<Line<'static>> {
        let marker = if selected { "> " } else { "  " };
        let name_style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };
        let mut lines = vec![
            Line::from(vec![
                Span::styled(marker, Style::default().fg(Color::Cyan)),
                Span::styled(self.entry.name.clone(), name_style),
            ]),
            Line::from(format!(
                "  pin {}  license {}",
                sha7(&self.entry.pinned_commit),
                license_label(self.entry.license.as_deref())
            )),
            Line::from(format!("  verdict {}", verdict_label(&self.verdict))),
            Line::from(format!(
                "  resolution {}",
                resolution_label(self.resolution.as_ref())
            )),
        ];
        match &self.verdict {
            GateVerdict::Ready(Verdict::Flagged { reasons }) => {
                for reason in reasons.iter().take(2) {
                    lines.push(Line::from(format!("  reason {}", reason)));
                }
            }
            GateVerdict::Unresolved(reason) => {
                lines.push(Line::from(format!("  {}", reason)));
            }
            GateVerdict::Ready(Verdict::Clean | Verdict::Conflict { .. }) => {}
        }
        lines.push(Line::from(format!("  path {}", self.entry.rel_path)));
        lines
    }

    fn is_evaluable(&self) -> bool {
        matches!(self.verdict, GateVerdict::Ready(_))
    }
}

fn gate_path(repo_root: &Path, entry: &CatalogEntry) -> PathBuf {
    let pooled = pool_dir(repo_root, &entry.source, &entry.name, &entry.pinned_commit);
    if pooled.exists() {
        pooled
    } else {
        spur_core::explore::sync::cache_dir(repo_root, &entry.source).join(&entry.rel_path)
    }
}

fn license_label(license: Option<&str>) -> String {
    license
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "unknown ⚠".to_string())
}

fn verdict_label(verdict: &GateVerdict) -> String {
    match verdict {
        GateVerdict::Ready(Verdict::Clean) => "Clean".to_string(),
        GateVerdict::Ready(Verdict::Flagged { .. }) => "Flagged".to_string(),
        GateVerdict::Ready(Verdict::Conflict { bundled_id }) => {
            format!("Conflict with {bundled_id}")
        }
        GateVerdict::Unresolved(_) => "Unresolved".to_string(),
    }
}

fn resolution_label(resolution: Option<&Resolution>) -> String {
    match resolution {
        Some(Resolution::Accept) => "Accept".to_string(),
        Some(Resolution::Override { justification }) => {
            format!("Override ({justification})")
        }
        Some(Resolution::ReplaceBundled) => "ReplaceBundled".to_string(),
        Some(Resolution::Skip) => "Skip".to_string(),
        None => "unresolved".to_string(),
    }
}

fn sha7(value: &str) -> &str {
    value.get(..7).unwrap_or(value)
}

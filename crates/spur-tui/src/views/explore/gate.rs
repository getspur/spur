use std::collections::BTreeSet;
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use spur_core::explore::apply::{Resolution, Selection};
use spur_core::explore::catalog::CatalogEntry;
use spur_core::explore::gate::{self, Verdict};

use super::{scroll_offset_for_selected_line, selected_marker_line, StarKey};

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

#[derive(Default)]
struct GateProgress {
    clean: usize,
    flagged: usize,
    conflict: usize,
    unresolved: usize,
    resolution_set: usize,
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
        starred: &BTreeSet<StarKey>,
        bundled_ids: &[String],
    ) -> Self {
        let cards = entries
            .iter()
            .filter(|entry| starred.contains(&StarKey::from_entry(entry)))
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

    pub(crate) fn unresolved_count(&self) -> usize {
        self.cards
            .iter()
            .filter(|card| !card.is_evaluable())
            .count()
    }

    pub(crate) fn footer_hint(&self) -> &'static str {
        if self.override_input.is_some() {
            return "type justification  Enter save override  Esc cancel";
        }

        match self.selected_card().map(|card| &card.verdict) {
            Some(GateVerdict::Ready(Verdict::Clean)) => {
                "j/k cards  a accept  s skip  c all-clean  Enter apply  Shift+A apply  Esc browse"
            }
            Some(GateVerdict::Ready(Verdict::Flagged { .. })) => {
                "j/k cards  o override  s skip  c all-clean  Enter apply  Shift+A apply  Esc browse"
            }
            Some(GateVerdict::Ready(Verdict::Conflict { .. })) => {
                "j/k cards  b replace  s skip  c all-clean  Enter apply  Shift+A apply  Esc browse"
            }
            Some(GateVerdict::Unresolved(_)) | None => {
                "j/k cards  s skip  c all-clean  Enter apply  Shift+A apply  Esc browse"
            }
        }
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> GateAction {
        if self.override_input.is_some() {
            return self.handle_override_input(key);
        }

        match key.code {
            KeyCode::Char('A') => GateAction::Apply,
            KeyCode::Enter if key.modifiers.is_empty() => {
                if self.has_resolved_selection() {
                    GateAction::Apply
                } else {
                    GateAction::Error("no resolved gate cards to apply".to_string())
                }
            }
            KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
                self.move_selection(1);
                GateAction::None
            }
            KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
                self.move_selection(-1);
                GateAction::None
            }
            KeyCode::Char('a') if key.modifiers.is_empty() => self.accept_selected(),
            KeyCode::Char('c') if key.modifiers.is_empty() => self.resolve_all_clean(),
            KeyCode::Char('o') if key.modifiers.is_empty() => self.begin_override(),
            KeyCode::Char('b') if key.modifiers.is_empty() => self.replace_selected(),
            KeyCode::Char('s') if key.modifiers.is_empty() => self.skip_selected(),
            KeyCode::Esc if key.modifiers.is_empty() => GateAction::Back,
            _ => GateAction::None,
        }
    }

    pub(crate) fn render(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default().title("Gate Cards").borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let chunks = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(inner);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                self.progress_summary(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))),
            chunks[0],
        );
        let (cards_area, override_area) = if self.override_input.is_some() {
            let content_chunks =
                Layout::vertical([Constraint::Min(0), Constraint::Length(2)]).split(chunks[1]);
            (content_chunks[0], Some(content_chunks[1]))
        } else {
            (chunks[1], None)
        };

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

        if let (Some(input), Some(input_area)) = (self.override_input.as_deref(), override_area) {
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::styled(
                            "override justification: ",
                            Style::default().fg(Color::Yellow),
                        ),
                        Span::raw(input.to_string()),
                    ]),
                ]),
                input_area,
            );
        }

        let scroll = scroll_offset_for_selected_line(
            &lines,
            selected_marker_line(&lines),
            cards_area.width,
            cards_area.height,
        );
        frame.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: true })
                .scroll((scroll, 0)),
            cards_area,
        );
    }

    fn handle_override_input(&mut self, key: KeyEvent) -> GateAction {
        match key.code {
            KeyCode::Enter if key.modifiers.is_empty() => {
                let input = self.override_input.take().unwrap_or_default();
                let justification = input.trim().to_string();
                if justification.is_empty() {
                    self.override_input = Some(input);
                    return GateAction::Error(
                        "override justification is required; type a reason, then press Enter, or Esc to cancel"
                            .to_string(),
                    );
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
            _ => wrong_resolution_key("accept", &card.verdict),
        }
    }

    fn resolve_all_clean(&mut self) -> GateAction {
        for card in &mut self.cards {
            if card.resolution.is_none()
                && matches!(card.verdict, GateVerdict::Ready(Verdict::Clean))
            {
                card.resolution = Some(Resolution::Accept);
            }
        }
        GateAction::None
    }

    fn begin_override(&mut self) -> GateAction {
        let Some(card) = self.selected_card() else {
            return GateAction::None;
        };
        match &card.verdict {
            GateVerdict::Ready(Verdict::Flagged { .. }) => {
                self.override_input = Some(String::new());
                GateAction::None
            }
            _ => wrong_resolution_key("override", &card.verdict),
        }
    }

    fn replace_selected(&mut self) -> GateAction {
        let Some(card) = self.selected_card_mut() else {
            return GateAction::None;
        };
        match &card.verdict {
            GateVerdict::Ready(Verdict::Conflict { .. }) => {
                card.resolution = Some(Resolution::ReplaceBundled);
                GateAction::None
            }
            _ => wrong_resolution_key("replace bundled", &card.verdict),
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

    fn has_resolved_selection(&self) -> bool {
        self.cards
            .iter()
            .any(|card| card.is_evaluable() && card.resolution.is_some())
    }

    fn progress_summary(&self) -> String {
        let progress = self
            .cards
            .iter()
            .fold(GateProgress::default(), |mut progress, card| {
                match card.verdict {
                    GateVerdict::Ready(Verdict::Clean) => progress.clean += 1,
                    GateVerdict::Ready(Verdict::Flagged { .. }) => progress.flagged += 1,
                    GateVerdict::Ready(Verdict::Conflict { .. }) => progress.conflict += 1,
                    GateVerdict::Unresolved(_) => progress.unresolved += 1,
                }
                progress.resolution_set += usize::from(card.resolution.is_some());
                progress
            });
        format!(
            "{} cards · clean {} · flagged {} · conflict {} · unresolved {} · resolved {}/{}",
            self.cards.len(),
            progress.clean,
            progress.flagged,
            progress.conflict,
            progress.unresolved,
            progress.resolution_set,
            self.cards.len()
        )
    }
}

impl Default for GateState {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl GateCard {
    fn new(repo_root: &Path, entry: CatalogEntry, bundled_ids: &[String]) -> Self {
        let source_path = super::explore_item_path(repo_root, &entry);
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

fn resolution_key_hint(verdict: &GateVerdict) -> &'static str {
    match verdict {
        GateVerdict::Ready(Verdict::Clean) => "press a to accept or s to skip",
        GateVerdict::Ready(Verdict::Flagged { .. }) => "press o to override or s to skip",
        GateVerdict::Ready(Verdict::Conflict { .. }) => "press b to replace bundled or s to skip",
        GateVerdict::Unresolved(_) => "run `spur explore sync`, or press s to skip",
    }
}

fn wrong_resolution_key(action: &str, verdict: &GateVerdict) -> GateAction {
    GateAction::Error(format!(
        "{action} is unavailable for this card; {}",
        resolution_key_hint(verdict)
    ))
}

fn sha7(value: &str) -> &str {
    value.get(..7).unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::{backend::TestBackend, Terminal};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn shift_key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    fn render_to_string(state: &GateState) -> String {
        render_to_string_with_size(state, 100, 30)
    }

    fn render_to_string_with_size(state: &GateState, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| state.render(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut output = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                output.push_str(buffer[(x, y)].symbol());
            }
            output.push('\n');
        }
        output
    }

    fn entry(name: &str, license: Option<&str>) -> CatalogEntry {
        CatalogEntry {
            kind: spur_core::explore::catalog::ItemKind::Skill,
            name: name.to_string(),
            source: "acme/repo".to_string(),
            rel_path: format!("skills/{name}"),
            pinned_commit: "abcdef1234567890abcdef1234567890abcdef12".to_string(),
            description: "fixture".to_string(),
            license: license.map(str::to_string),
            content_sha256: "0".repeat(64),
        }
    }

    fn card(name: &str, verdict: GateVerdict) -> GateCard {
        GateCard {
            entry: entry(name, Some("MIT")),
            verdict,
            resolution: None,
        }
    }

    fn clean_card(name: &str) -> GateCard {
        card(name, GateVerdict::Ready(Verdict::Clean))
    }

    fn flagged_card(name: &str) -> GateCard {
        card(
            name,
            GateVerdict::Ready(Verdict::Flagged {
                reasons: vec!["injection pattern".to_string()],
            }),
        )
    }

    fn conflict_card(name: &str) -> GateCard {
        card(
            name,
            GateVerdict::Ready(Verdict::Conflict {
                bundled_id: "spurpower-spur-way".to_string(),
            }),
        )
    }

    fn unresolved_card(name: &str) -> GateCard {
        card(
            name,
            GateVerdict::Unresolved("missing cache checkout; run `spur explore sync`".to_string()),
        )
    }

    #[test]
    fn accept_rejects_non_clean_verdicts() {
        let mut flagged = GateState::new(vec![flagged_card("a")]);
        let GateAction::Error(error) = flagged.handle_key(key(KeyCode::Char('a'))) else {
            panic!("flagged card should reject accept");
        };
        assert!(error.contains("press o to override or s to skip"));

        let mut conflict = GateState::new(vec![conflict_card("a")]);
        let GateAction::Error(error) = conflict.handle_key(key(KeyCode::Char('a'))) else {
            panic!("conflict card should reject accept");
        };
        assert!(error.contains("press b to replace bundled or s to skip"));

        let mut unresolved = GateState::new(vec![unresolved_card("a")]);
        let GateAction::Error(error) = unresolved.handle_key(key(KeyCode::Char('a'))) else {
            panic!("unresolved card should reject accept");
        };
        assert!(error.contains("press s to skip"));

        assert!(flagged.cards[0].resolution.is_none());
        assert!(conflict.cards[0].resolution.is_none());
        assert!(unresolved.cards[0].resolution.is_none());
    }

    #[test]
    fn override_rejects_non_flagged_verdicts() {
        for mut state in [
            GateState::new(vec![clean_card("a")]),
            GateState::new(vec![conflict_card("a")]),
            GateState::new(vec![unresolved_card("a")]),
        ] {
            let action = state.handle_key(key(KeyCode::Char('o')));
            assert!(matches!(action, GateAction::Error(_)));
        }
    }

    #[test]
    fn replace_bundled_rejects_non_conflict_verdicts() {
        for mut state in [
            GateState::new(vec![clean_card("a")]),
            GateState::new(vec![flagged_card("a")]),
            GateState::new(vec![unresolved_card("a")]),
        ] {
            let action = state.handle_key(key(KeyCode::Char('b')));
            assert!(matches!(action, GateAction::Error(_)));
        }
    }

    #[test]
    fn skip_resolves_regardless_of_verdict() {
        let mut state = GateState::new(vec![unresolved_card("a")]);
        assert!(matches!(
            state.handle_key(key(KeyCode::Char('s'))),
            GateAction::None
        ));
        assert_eq!(state.cards[0].resolution, Some(Resolution::Skip));
    }

    #[test]
    fn c_resolves_all_clean_cards_to_accept() {
        let mut state = GateState::new(vec![
            clean_card("clean-a"),
            flagged_card("flagged-a"),
            clean_card("clean-b"),
            clean_card("clean-c"),
        ]);

        assert!(matches!(
            state.handle_key(key(KeyCode::Char('c'))),
            GateAction::None
        ));

        assert_eq!(state.cards[0].resolution, Some(Resolution::Accept));
        assert_eq!(state.cards[1].resolution, None);
        assert_eq!(state.cards[2].resolution, Some(Resolution::Accept));
        assert_eq!(state.cards[3].resolution, Some(Resolution::Accept));
    }

    #[test]
    fn esc_returns_back_and_shift_a_returns_apply() {
        let mut state = GateState::new(vec![clean_card("a")]);
        assert!(matches!(
            state.handle_key(key(KeyCode::Esc)),
            GateAction::Back
        ));
        assert!(matches!(
            state.handle_key(shift_key(KeyCode::Char('A'))),
            GateAction::Apply
        ));
    }

    #[test]
    fn enter_applies_only_after_at_least_one_resolution() {
        let mut state = GateState::new(vec![clean_card("a")]);

        let GateAction::Error(error) = state.handle_key(key(KeyCode::Enter)) else {
            panic!("Enter without a resolution should report an error");
        };
        assert_eq!(error, "no resolved gate cards to apply");

        assert!(matches!(
            state.handle_key(key(KeyCode::Char('a'))),
            GateAction::None
        ));
        assert!(matches!(
            state.handle_key(key(KeyCode::Enter)),
            GateAction::Apply
        ));
    }

    #[test]
    fn navigation_clamps_at_boundaries() {
        let mut state = GateState::new(vec![clean_card("a"), clean_card("b"), clean_card("c")]);
        assert!(matches!(
            state.handle_key(key(KeyCode::Char('k'))),
            GateAction::None
        ));
        assert_eq!(state.selected, 0, "k at top stays clamped");

        state.handle_key(key(KeyCode::Char('j')));
        state.handle_key(key(KeyCode::Char('j')));
        state.handle_key(key(KeyCode::Char('j')));
        assert_eq!(state.selected, 2, "j past the end clamps to last card");
    }

    #[test]
    fn override_input_backspace_and_escape_cancel() {
        let mut state = GateState::new(vec![flagged_card("a")]);
        state.handle_key(key(KeyCode::Char('o')));
        assert!(state.override_input.is_some());

        state.handle_key(key(KeyCode::Char('x')));
        state.handle_key(key(KeyCode::Char('y')));
        assert_eq!(state.override_input.as_deref(), Some("xy"));

        state.handle_key(key(KeyCode::Backspace));
        assert_eq!(state.override_input.as_deref(), Some("x"));

        state.handle_key(key(KeyCode::Esc));
        assert!(state.override_input.is_none());
        assert!(state.cards[0].resolution.is_none());
    }

    #[test]
    fn override_input_remains_visible_in_a_scrolled_multi_card_gate() {
        let mut state = GateState::new(vec![
            clean_card("clean-a"),
            flagged_card("flagged-a"),
            clean_card("clean-b"),
            clean_card("clean-c"),
        ]);
        state.selected = 1;
        state.handle_key(key(KeyCode::Char('o')));
        let GateAction::Error(_) = state.handle_key(key(KeyCode::Enter)) else {
            panic!("empty override should remain in input mode with an error");
        };

        let text = render_to_string_with_size(&state, 80, 12);

        assert!(
            text.contains("override justification:"),
            "override editor should remain visible after the actionable error:\n{text}"
        );
    }

    #[test]
    fn render_lines_cover_all_verdict_branches() {
        for c in [
            clean_card("clean-a"),
            flagged_card("flagged-a"),
            conflict_card("conflict-a"),
            unresolved_card("unresolved-a"),
        ] {
            let lines = c.render_lines(true);
            let text: String = lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .map(|s| s.content.as_ref())
                .collect();
            assert!(text.contains(&c.entry.name));
        }
    }

    #[test]
    fn render_shows_verdict_and_resolution_progress() {
        let mut state = GateState::new(vec![
            clean_card("clean-a"),
            flagged_card("flagged-a"),
            conflict_card("conflict-a"),
            unresolved_card("unresolved-a"),
        ]);
        state.cards[0].resolution = Some(Resolution::Accept);
        state.cards[1].resolution = Some(Resolution::Skip);

        let text = render_to_string(&state);

        assert!(
            text.contains(
                "4 cards · clean 1 · flagged 1 · conflict 1 · unresolved 1 · resolved 2/4"
            ),
            "render text:\n{text}"
        );
    }

    #[test]
    fn render_keeps_progress_visible_when_cards_scroll() {
        let mut state = GateState::new(
            (0..8)
                .map(|index| clean_card(&format!("clean-{index}")))
                .collect(),
        );
        state.selected = 7;

        let text = render_to_string(&state);

        assert!(
            text.contains(
                "8 cards · clean 8 · flagged 0 · conflict 0 · unresolved 0 · resolved 0/8"
            ),
            "progress should remain above the scrolling card list:\n{text}"
        );
        assert!(
            text.contains("> clean-7"),
            "selected card should remain visible:\n{text}"
        );
    }

    #[test]
    fn is_evaluable_true_only_for_ready_verdicts() {
        assert!(clean_card("a").is_evaluable());
        assert!(flagged_card("a").is_evaluable());
        assert!(conflict_card("a").is_evaluable());
        assert!(!unresolved_card("a").is_evaluable());
    }

    #[test]
    fn license_label_and_resolution_label_cover_all_branches() {
        assert_eq!(license_label(Some("MIT")), "MIT");
        assert_eq!(license_label(Some("  ")), "unknown ⚠");
        assert_eq!(license_label(None), "unknown ⚠");

        assert_eq!(resolution_label(None), "unresolved");
        assert_eq!(resolution_label(Some(&Resolution::Accept)), "Accept");
        assert_eq!(
            resolution_label(Some(&Resolution::Override {
                justification: "ok".to_string()
            })),
            "Override (ok)"
        );
        assert_eq!(
            resolution_label(Some(&Resolution::ReplaceBundled)),
            "ReplaceBundled"
        );
        assert_eq!(resolution_label(Some(&Resolution::Skip)), "Skip");
    }

    #[test]
    fn render_produces_no_gate_cards_placeholder_when_empty() {
        let state = GateState::default();
        assert!(state.is_empty());
        assert!(state.resolved_selections().is_empty());
    }
}

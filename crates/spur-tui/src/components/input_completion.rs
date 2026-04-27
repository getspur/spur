use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use crossterm::event::KeyEvent;
use ratatui::{layout::Rect, Frame};

use crate::commands::CommandRegistry;
use crate::components::completion_trigger::{
    IntentEvent, TriggerDetector, TriggerKind, TriggerTransition,
};
use crate::components::input_bar::InputBar;
use crate::components::picker_shell::{PickerAction, PickerShell};
use crate::components::query_source::{
    MentionQuerySource, QueryMode, RetrievalAccept, SlashQuerySource, SlashRow,
};
use crate::mentions::{CompletionScope, MentionRegistry};

pub struct InputCompletionPort {
    trigger_detector: TriggerDetector,
    picker_shell: Option<PickerShell>,
}

pub struct CompletionEnv<'a> {
    pub command_registry: &'a CommandRegistry,
    pub mention_registry: &'a Rc<RefCell<MentionRegistry>>,
    pub cwd: &'a Path,
    pub scope: CompletionScope<'a>,
}

impl InputCompletionPort {
    pub fn new() -> Self {
        Self {
            trigger_detector: TriggerDetector::new(),
            picker_shell: None,
        }
    }

    /// Feed a classified IntentEvent into the TriggerDetector and apply the
    /// resulting transition to the active PickerShell.
    pub fn dispatch(
        &mut self,
        event: IntentEvent,
        input_bar: &mut InputBar,
        env: &CompletionEnv<'_>,
    ) {
        // Fast path: Idle state + non-opening event -> no text fetch, no alloc.
        if self.trigger_detector.is_idle()
            && !matches!(
                event,
                IntentEvent::TypedChar('@') | IntentEvent::TypedChar('/')
            )
        {
            return;
        }

        // History-mode shell owns the picker; detector is inert.
        if let Some(shell) = self.picker_shell.as_ref() {
            if shell.query_mode() == QueryMode::OwnedByShell {
                self.trigger_detector.reset();
                return;
            }
        }

        let text = input_bar.text();
        let cursor = input_bar.cursor();
        let ranges = input_bar.protected_ranges().to_vec();

        let transition = self.trigger_detector.step(event, &text, cursor, &ranges);

        match transition {
            TriggerTransition::None => {}
            TriggerTransition::Update { query } => {
                if let Some(shell) = self.picker_shell.as_mut() {
                    shell.set_query_from_input_bar(&query);
                }
            }
            TriggerTransition::Open { trigger } => {
                let shell = match trigger.kind {
                    TriggerKind::Slash => {
                        let entries = env.command_registry.list();
                        let rows: Vec<SlashRow> = entries
                            .iter()
                            .map(|e| SlashRow {
                                canonical: env.command_registry.canonical_typed_form(e),
                                description: e.description.clone(),
                                tag: match &e.source {
                                    crate::commands::CommandSource::Spur => "⟨spur⟩".into(),
                                    crate::commands::CommandSource::Agent { handle }
                                    | crate::commands::CommandSource::Advertised { handle } => {
                                        format!("⟨{}⟩", handle)
                                    }
                                },
                            })
                            .collect();
                        let src = SlashQuerySource::new(rows, trigger.prefix_start);
                        PickerShell::open_with_query(Box::new(src), &trigger.query)
                    }
                    TriggerKind::Mention => {
                        let src = MentionQuerySource::new(
                            Rc::clone(env.mention_registry),
                            env.scope,
                            env.cwd.to_path_buf(),
                            trigger.prefix_start,
                        );
                        PickerShell::open_with_query(Box::new(src), &trigger.query)
                    }
                };
                self.picker_shell = Some(shell);
            }
            TriggerTransition::Close => {
                self.picker_shell = None;
            }
        }
    }

    pub fn handle_picker_key(
        &mut self,
        key: KeyEvent,
        input_bar: &mut InputBar,
    ) -> Option<RetrievalAccept> {
        let act = self.picker_shell.as_mut()?.handle_key(key);
        match act {
            PickerAction::None => None,
            PickerAction::Cancel => {
                self.picker_shell = None;
                self.trigger_detector.reset();
                None
            }
            PickerAction::Accept(accept) => {
                let out = accept.clone();
                self.apply_accept(accept, input_bar);
                self.picker_shell = None;
                self.trigger_detector.reset();
                Some(out)
            }
        }
    }

    pub fn render(&self, frame: &mut Frame, input_rect: Rect, area: Rect) {
        if let Some(shell) = self.picker_shell.as_ref() {
            shell.render(frame, input_rect, area);
        }
    }

    pub fn is_active(&self) -> bool {
        self.picker_shell.is_some()
    }

    pub fn reset(&mut self) {
        self.trigger_detector.reset();
        self.picker_shell = None;
    }

    pub fn is_trigger_driven(&self) -> bool {
        matches!(
            self.picker_shell.as_ref().map(PickerShell::query_mode),
            Some(QueryMode::ReadFromInputBar)
        )
    }

    pub fn open_history(&mut self, history: Vec<crate::input_history::InputHistoryEntry>) {
        use crate::components::query_source::HistoryQuerySource;
        self.picker_shell = Some(PickerShell::open(Box::new(HistoryQuerySource::new(
            history,
        ))));
        self.trigger_detector.reset();
    }

    #[cfg(test)]
    pub fn row_primaries_for_test(&self) -> Vec<String> {
        self.picker_shell
            .as_ref()
            .map(PickerShell::row_primaries)
            .unwrap_or_default()
    }

    fn apply_accept(&mut self, accept: RetrievalAccept, input_bar: &mut InputBar) {
        match accept {
            RetrievalAccept::ReplaceState(snap) => {
                let len = snap.text.len();
                input_bar.set_state(snap, len);
            }
            RetrievalAccept::InsertAtom {
                text,
                uri,
                name,
                replace_from,
            } => {
                if let Some(prefix_start) = replace_from {
                    replace_trigger_token(input_bar, prefix_start, "");
                }
                input_bar.insert_atom(text, uri, name);
            }
            RetrievalAccept::ReplaceTriggerToken {
                prefix_start,
                replacement,
            } => {
                replace_trigger_token(input_bar, prefix_start, &replacement);
            }
        }
    }
}

impl Default for InputCompletionPort {
    fn default() -> Self {
        Self::new()
    }
}

/// Replace the range [prefix_start..cursor] in the InputBar with `replacement`.
/// Leaves the cursor at `prefix_start + replacement.len()`.
fn replace_trigger_token(input_bar: &mut InputBar, prefix_start: usize, replacement: &str) {
    let current = input_bar.text().to_string();
    let cursor = input_bar.cursor();
    let mut new_text = String::with_capacity(current.len());
    new_text.push_str(&current[..prefix_start]);
    new_text.push_str(replacement);
    new_text.push_str(&current[cursor..]);
    let new_cursor = prefix_start + replacement.len();
    input_bar.set_text(new_text, new_cursor);
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::commands::CommandRegistry;
    use crate::components::completion_trigger::IntentEvent;
    use crate::components::input_bar::InputBar;
    use crate::components::query_source::RetrievalAccept;
    use crate::mentions::CompletionScope;
    use crate::mentions::MentionRegistry;

    use super::{CompletionEnv, InputCompletionPort};

    fn env<'a>(
        command_registry: &'a CommandRegistry,
        mention_registry: &'a Rc<RefCell<MentionRegistry>>,
        cwd: &'a std::path::Path,
    ) -> CompletionEnv<'a> {
        CompletionEnv {
            command_registry,
            mention_registry,
            cwd,
            scope: CompletionScope::PreSession,
        }
    }

    #[test]
    fn dispatch_opens_and_accepts_at_mentions_pre_session() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        let command_registry = CommandRegistry::new();
        let mention_registry = Rc::new(RefCell::new(MentionRegistry::new()));
        let mut input_bar = InputBar::new();
        let mut completion = InputCompletionPort::new();

        input_bar.set_text("@".to_string(), 1);
        completion.dispatch(
            IntentEvent::TypedChar('@'),
            &mut input_bar,
            &env(&command_registry, &mention_registry, tmp.path()),
        );

        assert!(completion.is_active());
        assert!(
            completion
                .row_primaries_for_test()
                .iter()
                .any(|row| row.contains("@Cargo.toml")),
            "expected Cargo.toml row"
        );

        let accepted = completion.handle_picker_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut input_bar,
        );

        assert!(matches!(accepted, Some(RetrievalAccept::InsertAtom { .. })));
        assert_eq!(input_bar.text(), "@Cargo.toml");
        assert_eq!(input_bar.protected_ranges().len(), 1);
        assert!(!completion.is_active());
    }

    #[test]
    fn dispatch_opens_and_accepts_slash_commands_pre_session() {
        let command_registry = CommandRegistry::new();
        let mention_registry = Rc::new(RefCell::new(MentionRegistry::new()));
        let mut input_bar = InputBar::new();
        let mut completion = InputCompletionPort::new();

        input_bar.set_text("/".to_string(), 1);
        completion.dispatch(
            IntentEvent::TypedChar('/'),
            &mut input_bar,
            &env(
                &command_registry,
                &mention_registry,
                std::path::Path::new("."),
            ),
        );

        assert!(completion.is_active());
        let rows = completion.row_primaries_for_test();
        assert!(rows.iter().any(|row| row == "/help"), "{rows:?}");
        assert!(rows.iter().any(|row| row == "/quit"), "{rows:?}");

        let accepted = completion.handle_picker_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut input_bar,
        );

        assert!(matches!(
            accepted,
            Some(RetrievalAccept::ReplaceTriggerToken { .. })
        ));
        assert!(input_bar.text().starts_with('/'));
        assert!(input_bar.text().ends_with(' '));
        assert!(!completion.is_active());
    }
}

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use crossterm::event::KeyEvent;
use ratatui::{layout::Rect, Frame};

use crate::commands::CommandRegistry;
use crate::components::completion_trigger::{
    IntentEvent, TriggerDetector, TriggerKind, TriggerTransition,
};
use crate::components::input_bar::{InputBar, ProtectedRange};
use crate::components::picker_shell::{PickerAction, PickerShell};
use crate::components::query_source::{
    MentionQuerySource, QueryMode, RetrievalAccept, SlashQuerySource, SlashRow,
};
use crate::input_history::InputStateSnapshot;
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
    /// Cached agent-advertised session config options. Used by SlashArg
    /// dispatch to instantiate `ConfigOptionQuerySource`. Empty pre-session.
    /// Populated by callers from `Orchestrator::session_config_options(brain)`.
    pub session_config_options: &'a [spur_acp::SessionConfigOption],
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
        // Fast path: Idle state + non-mutating event -> no text fetch, no
        // alloc. We must keep the detector in the loop for any event that can
        // open a SlashArg from a fresh paste/SetText/typed char (incl. typed
        // chars other than '@'/'/', e.g. continuing to type "/model gpt"
        // after a paste of "/model "). Pure cursor motion / lifecycle events
        // never open a picker — short-circuit those. Per codex review #2.
        if self.trigger_detector.is_idle()
            && !matches!(
                event,
                IntentEvent::TypedChar(_)
                    | IntentEvent::Pasted
                    | IntentEvent::SetText
                    | IntentEvent::DeletedChar
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

        let transition = self
            .trigger_detector
            .step(event, &text, cursor, &ranges, |name| {
                env.command_registry.arg_picker_spec(name).is_some()
            });

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
                    TriggerKind::SlashArg { command_name } => {
                        // Resolve the command's arg-picker spec.
                        //   typed_hint == Some(ConfigOption) → v1 select picker
                        //   typed_hint == None             → v2 PR-3 free-text picker
                        //   future: GitRef, FilePath, Choice, etc. (PR-4+)
                        let Some(spec) = env.command_registry.arg_picker_spec(&command_name) else {
                            return;
                        };
                        match spec.typed_hint.clone() {
                            Some(
                                spur_acp::adapter::arg_picker_hint::ArgPickerHint::ConfigOption {
                                    config_id,
                                },
                            ) => {
                                let choices = env
                                    .session_config_options
                                    .iter()
                                    .find(|o| o.id.0.as_ref() == config_id.as_str())
                                    .map(spur_acp::extract_choices)
                                    .unwrap_or_default();
                                let src = crate::components::config_option_query_source::ConfigOptionQuerySource::new(
                                    command_name.clone(),
                                    config_id.clone(),
                                    choices,
                                );
                                PickerShell::open_with_query(Box::new(src), &trigger.query)
                            }
                            None => {
                                // PR-3: agent advertised Unstructured input
                                // (e.g. codex's /review, /review-branch). Free-
                                // text picker reads the arg from the InputBar.
                                // Anchor is re-resolved by apply_accept via
                                // trigger_detector.current_prefix_start().
                                let src = crate::components::command_input_query_source::CommandInputQuerySource::new(
                                    command_name.clone(),
                                    spec.free_text_hint.clone(),
                                );
                                PickerShell::open_with_query(Box::new(src), &trigger.query)
                            }
                        }
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

    pub fn render(
        &mut self,
        frame: &mut Frame,
        input_rect: Rect,
        area: Rect,
        theme: &crate::theme::Theme,
    ) {
        if let Some(shell) = self.picker_shell.as_mut() {
            shell.render(frame, input_rect, area, theme);
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

    pub fn open_theme_picker(&mut self, active_theme_name: &str) {
        let source = crate::components::theme_query_source::ThemeQuerySource::new(
            crate::theme::list_available_themes(),
            active_theme_name,
        );
        self.picker_shell = Some(PickerShell::open(Box::new(source)));
        self.trigger_detector.reset();
    }

    #[cfg(test)]
    pub fn row_primaries_for_test(&self) -> Vec<String> {
        self.picker_shell
            .as_ref()
            .map(PickerShell::row_primaries)
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn picker_title_for_test(&self) -> Option<String> {
        self.picker_shell.as_ref().map(|s| s.title().to_string())
    }

    #[cfg(test)]
    pub fn picker_row_count_for_test(&self) -> Option<usize> {
        self.picker_shell.as_ref().map(PickerShell::row_count)
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
                    // Re-anchor against the live detector state — same
                    // contract as ReplaceTriggerToken below. Without this,
                    // a stale `replace_from` after the user types more
                    // characters past picker-open can carve out a span
                    // that starts too early.
                    let anchor = self
                        .trigger_detector
                        .current_prefix_start()
                        .unwrap_or(prefix_start);
                    replace_trigger_token(input_bar, anchor, "");
                }
                input_bar.insert_atom(text, uri, name);
            }
            RetrievalAccept::ReplaceTriggerToken {
                prefix_start,
                replacement,
            } => {
                // Re-anchor against the live trigger state. SlashArg's
                // ConfigOptionQuerySource hardcodes prefix_start=0 as a
                // sentinel; the real anchor is the byte just past `/<cmd> `,
                // tracked by the detector. For Slash/Mention the detector's
                // prefix_start equals the trigger char's byte offset, which
                // matches what the existing query sources pass through, so
                // this preserves legacy behavior. Falls back to the picker-
                // provided value only if the detector somehow Idle'd before
                // accept landed.
                let anchor = self
                    .trigger_detector
                    .current_prefix_start()
                    .unwrap_or(prefix_start);
                replace_trigger_token(input_bar, anchor, &replacement);
            }
            RetrievalAccept::SubmitText { .. } => {
                input_bar.clear();
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
///
/// Preserves existing protected atoms outside the carved-out span so prior
/// `@mentions` keep their range metadata when a sibling trigger is accepted.
/// `set_text` cannot be used here: it routes through `InputStateSnapshot::from_text`
/// which seeds `protected_ranges = []` and drops every prior atom.
fn replace_trigger_token(input_bar: &mut InputBar, prefix_start: usize, replacement: &str) {
    let current = input_bar.text();
    let cursor = input_bar.cursor();

    // Detector contract: prefix_start ≤ cursor and both sit on UTF-8 char
    // boundaries. Violation means the detector or accept dispatcher is racy;
    // surface it in dev/test instead of returning corrupt offsets.
    debug_assert!(
        prefix_start <= cursor,
        "replace_trigger_token: prefix_start ({prefix_start}) > cursor ({cursor})"
    );
    debug_assert!(
        current.is_char_boundary(prefix_start) && current.is_char_boundary(cursor),
        "replace_trigger_token: byte offsets off a UTF-8 char boundary"
    );
    // Atoms can't overlap the trigger span — the detector never opens a
    // picker inside a protected range. Catch future regressions loudly.
    debug_assert!(
        input_bar
            .protected_ranges()
            .iter()
            .all(|r| r.end <= prefix_start || r.start >= cursor),
        "replace_trigger_token: a protected range overlaps [{prefix_start}..{cursor}]"
    );

    let removed_len = cursor.saturating_sub(prefix_start);
    let inserted_len = replacement.len();
    let delta = inserted_len as isize - removed_len as isize;

    let mut new_text = String::with_capacity(current.len() + inserted_len);
    new_text.push_str(&current[..prefix_start]);
    new_text.push_str(replacement);
    new_text.push_str(&current[cursor..]);

    // Filter is defense-in-depth for the debug_assert above; in release builds
    // we'd rather drop a misaligned range than panic on a stray edge case.
    let ranges: Vec<ProtectedRange> = input_bar
        .protected_ranges()
        .iter()
        .filter(|r| r.end <= prefix_start || r.start >= cursor)
        .cloned()
        .filter_map(|mut r| {
            if r.start >= cursor {
                // Surviving ranges are at or beyond `cursor`, so adding
                // `delta = inserted_len - removed_len` lands in
                // [prefix_start + inserted_len, _]. Use checked arithmetic
                // to fail loudly on any future invariant break instead of
                // silently wrapping into a huge usize address in release.
                let new_start = r.start.checked_add_signed(delta)?;
                let new_end = r.end.checked_add_signed(delta)?;
                debug_assert!(
                    new_start >= prefix_start + inserted_len,
                    "shifted range start escaped the post-replacement region"
                );
                r.start = new_start;
                r.end = new_end;
            }
            Some(r)
        })
        .collect();

    let new_cursor = prefix_start + inserted_len;
    input_bar.set_state(InputStateSnapshot::new(new_text, ranges), new_cursor);
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
            session_config_options: &[],
        }
    }

    fn env_with_options<'a>(
        command_registry: &'a CommandRegistry,
        mention_registry: &'a Rc<RefCell<MentionRegistry>>,
        cwd: &'a std::path::Path,
        opts: &'a [spur_acp::SessionConfigOption],
    ) -> CompletionEnv<'a> {
        CompletionEnv {
            command_registry,
            mention_registry,
            cwd,
            scope: CompletionScope::PreSession,
            session_config_options: opts,
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
    fn slash_arg_open_with_typed_hint_none_instantiates_command_input_query_source() {
        // Wave B.7 regression: when an agent advertises an Unstructured input
        // (e.g. codex's /review-branch), parse() yields ArgPickerSpec with
        // typed_hint=None. The SlashArg arm must dispatch on that to a
        // CommandInputQuerySource (free-text picker), not silently drop.
        use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};
        use spur_acp::adapter::arg_picker_hint::ArgPickerSpec;

        let mut command_registry = CommandRegistry::new();
        let entry = CommandEntry {
            name: "review-branch".into(),
            description: "Review branch".into(),
            hint: Some("branch name".into()),
            source: CommandSource::Agent {
                handle: "codex".into(),
            },
            dispatch: Dispatch::PromptText {
                normalized: "/review-branch".into(),
            },
            arg_picker_spec: Some(ArgPickerSpec {
                free_text_hint: "branch name".into(),
                typed_hint: None,
            }),
        };
        command_registry.set_agent_commands("codex", vec![entry]);

        let mention_registry = Rc::new(RefCell::new(MentionRegistry::new()));
        let mut input_bar = InputBar::new();
        let mut completion = InputCompletionPort::new();

        // Paste "/review-branch " (cursor at end). Trigger detector enters
        // SlashArg{ command_name: "review-branch" } and the dispatch arm
        // must instantiate the free-text picker.
        input_bar.set_text("/review-branch ".to_string(), 15);
        completion.dispatch(
            IntentEvent::Pasted,
            &mut input_bar,
            &env_with_options(
                &command_registry,
                &mention_registry,
                std::path::Path::new("."),
                &[],
            ),
        );

        assert!(
            completion.is_active(),
            "SlashArg dispatch with typed_hint=None must open the free-text picker"
        );
        assert_eq!(
            completion.picker_title_for_test().as_deref(),
            Some("branch name"),
            "CommandInputQuerySource title must surface the advertised hint"
        );
    }

    #[test]
    fn slash_arg_open_instantiates_config_option_query_source() {
        use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};
        use spur_acp::adapter::arg_picker_hint::{ArgPickerHint, ArgPickerSpec};
        use spur_acp::{SessionConfigId, SessionConfigOption, SessionConfigSelectOption};

        // Registry: /model with arg_picker_spec → typed ConfigOption("model").
        let mut command_registry = CommandRegistry::new();
        let entry = CommandEntry {
            name: "model".into(),
            description: "Switch model".into(),
            hint: None,
            source: CommandSource::Advertised {
                handle: "codex".into(),
            },
            dispatch: Dispatch::SetSessionConfigOption {
                config_id: "model".into(),
            },
            arg_picker_spec: Some(ArgPickerSpec {
                free_text_hint: String::new(),
                typed_hint: Some(ArgPickerHint::ConfigOption {
                    config_id: "model".into(),
                }),
            }),
        };
        command_registry.set_advertised_commands("codex", vec![entry]);

        // Cached session_config_options: a Select with 3 choices.
        let opt = SessionConfigOption::select(
            SessionConfigId::new("model".to_string()),
            "Model".to_string(),
            "gpt-5-codex".to_string(),
            vec![
                SessionConfigSelectOption::new(
                    "gpt-5-codex".to_string(),
                    "GPT-5 Codex".to_string(),
                ),
                SessionConfigSelectOption::new("gpt-5".to_string(), "GPT-5".to_string()),
                SessionConfigSelectOption::new("o4-mini".to_string(), "o4-mini".to_string()),
            ],
        );
        let opts = vec![opt];

        let mention_registry = Rc::new(RefCell::new(MentionRegistry::new()));
        let mut input_bar = InputBar::new();
        let mut completion = InputCompletionPort::new();

        // Paste "/model " (cursor at end). Should open SlashArg picker.
        input_bar.set_text("/model ".to_string(), 7);
        completion.dispatch(
            IntentEvent::Pasted,
            &mut input_bar,
            &env_with_options(
                &command_registry,
                &mention_registry,
                std::path::Path::new("."),
                &opts,
            ),
        );

        assert!(
            completion.is_active(),
            "SlashArg dispatch must instantiate a picker"
        );
        assert_eq!(
            completion.picker_title_for_test().as_deref(),
            Some("Model"),
            "ConfigOptionQuerySource should advertise title 'Model' for /model"
        );
        assert_eq!(
            completion.picker_row_count_for_test(),
            Some(3),
            "all 3 advertised choices should be visible with empty query"
        );
        let rows = completion.row_primaries_for_test();
        assert!(rows.iter().any(|r| r == "GPT-5 Codex"), "{rows:?}");
        assert!(rows.iter().any(|r| r == "GPT-5"), "{rows:?}");
        assert!(rows.iter().any(|r| r == "o4-mini"), "{rows:?}");
    }

    #[test]
    fn slash_arg_accept_re_anchors_and_replaces_only_arg_region() {
        use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use spur_acp::adapter::arg_picker_hint::{ArgPickerHint, ArgPickerSpec};
        use spur_acp::{SessionConfigId, SessionConfigOption, SessionConfigSelectOption};

        let mut command_registry = CommandRegistry::new();
        command_registry.set_advertised_commands(
            "codex",
            vec![CommandEntry {
                name: "model".into(),
                description: "Switch model".into(),
                hint: None,
                source: CommandSource::Advertised {
                    handle: "codex".into(),
                },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: "model".into(),
                },
                arg_picker_spec: Some(ArgPickerSpec {
                    free_text_hint: String::new(),
                    typed_hint: Some(ArgPickerHint::ConfigOption {
                        config_id: "model".into(),
                    }),
                }),
            }],
        );
        let opts = vec![SessionConfigOption::select(
            SessionConfigId::new("model".to_string()),
            "Model".to_string(),
            "gpt-5-codex".to_string(),
            vec![SessionConfigSelectOption::new(
                "gpt-5-codex".to_string(),
                "GPT-5 Codex".to_string(),
            )],
        )];

        let mention_registry = Rc::new(RefCell::new(MentionRegistry::new()));
        let mut input_bar = InputBar::new();
        let mut completion = InputCompletionPort::new();

        input_bar.set_text("/model ".to_string(), 7);
        completion.dispatch(
            IntentEvent::Pasted,
            &mut input_bar,
            &env_with_options(
                &command_registry,
                &mention_registry,
                std::path::Path::new("."),
                &opts,
            ),
        );
        assert!(completion.is_active());

        // Accept the first row. Re-anchor MUST keep "/model " in the buffer
        // and only replace the arg region with the chosen value.
        let accepted = completion.handle_picker_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut input_bar,
        );
        assert!(matches!(
            accepted,
            Some(RetrievalAccept::ReplaceTriggerToken { .. })
        ));
        assert_eq!(
            input_bar.text(),
            "/model gpt-5-codex",
            "the `/model ` prefix must survive; only the arg region is replaced"
        );
    }

    /// Wave B gemini #1 + #3 regression: when the free-text picker accepts on
    /// `/review-branch main`, the buffer must remain `/review-branch main`
    /// (NOT `/review-branch /review-branch main`). This was the critical
    /// duplication bug found by gemini — the original `accept` returned
    /// `/<cmd> <query>` but `apply_accept` re-anchors to the arg-region byte
    /// offset, so the picker must return only `<query>` as the replacement.
    #[test]
    fn slash_arg_free_text_accept_does_not_duplicate_command_prefix() {
        use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use spur_acp::adapter::arg_picker_hint::ArgPickerSpec;

        let mut command_registry = CommandRegistry::new();
        command_registry.set_agent_commands(
            "codex",
            vec![CommandEntry {
                name: "review-branch".into(),
                description: "Review against branch".into(),
                hint: Some("branch name".into()),
                source: CommandSource::Agent {
                    handle: "codex".into(),
                },
                dispatch: Dispatch::PromptText {
                    normalized: "/review-branch".into(),
                },
                arg_picker_spec: Some(ArgPickerSpec {
                    free_text_hint: "branch name".into(),
                    typed_hint: None,
                }),
            }],
        );

        let mention_registry = Rc::new(RefCell::new(MentionRegistry::new()));
        let mut input_bar = InputBar::new();
        let mut completion = InputCompletionPort::new();

        // User types up to /review-branch then hits space — picker opens.
        input_bar.set_text("/review-branch ".to_string(), 15);
        completion.dispatch(
            IntentEvent::Pasted,
            &mut input_bar,
            &env_with_options(
                &command_registry,
                &mention_registry,
                std::path::Path::new("."),
                &[],
            ),
        );
        assert!(
            completion.is_active(),
            "free-text picker must open on `/review-branch `"
        );

        // User then types "main" — buffer becomes "/review-branch main".
        // The picker reads the InputBar via ReadFromInputBar mode, so each
        // typed char dispatches to the port which forwards the updated query
        // to the picker's refresh().
        for ch in "main".chars() {
            input_bar.set_text(
                format!("{}{}", input_bar.text(), ch),
                input_bar.cursor() + ch.len_utf8(),
            );
            completion.dispatch(
                IntentEvent::TypedChar(ch),
                &mut input_bar,
                &env_with_options(
                    &command_registry,
                    &mention_registry,
                    std::path::Path::new("."),
                    &[],
                ),
            );
        }
        assert_eq!(input_bar.text(), "/review-branch main");

        // Accept (Tab). Anchor MUST be at byte 15 (start of arg region); the
        // replacement (= "main") collapses cleanly so the buffer is unchanged.
        let accepted = completion.handle_picker_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut input_bar,
        );
        assert!(matches!(
            accepted,
            Some(RetrievalAccept::ReplaceTriggerToken { .. })
        ));
        assert_eq!(
            input_bar.text(),
            "/review-branch main",
            "free-text picker accept must NOT duplicate the `/review-branch` prefix",
        );
    }

    #[test]
    fn replace_trigger_token_preserves_prior_protected_atoms() {
        // Regression: a second @mention used to wipe the first atom's protected
        // range because `replace_trigger_token` routed through `set_text`,
        // which always seeds an empty `protected_ranges`. Both atoms must
        // survive the carve-out of the new trigger token.
        use super::replace_trigger_token;

        let mut input_bar = InputBar::new();

        // First mention is already accepted as a protected atom: `@foo`.
        input_bar.insert_atom("@foo", "file:///foo".to_string(), "foo".to_string());
        // Type ` @b` in plain text (single-line `insert_paste` is the public
        // way to splice unprotected text without going through key dispatch).
        input_bar.insert_paste(" @b");
        assert_eq!(input_bar.text(), "@foo @b");
        assert_eq!(input_bar.cursor(), 7);
        assert_eq!(input_bar.protected_ranges().len(), 1);

        // Picker accept of the second mention's first step: replace `@b`
        // (prefix_start=5, cursor=7) with the empty string so `insert_atom`
        // can place the chosen atom. The first atom's range MUST survive.
        replace_trigger_token(&mut input_bar, 5, "");

        assert_eq!(input_bar.text(), "@foo ");
        assert_eq!(
            input_bar.protected_ranges().len(),
            1,
            "first @foo atom must remain protected after sibling trigger replace"
        );
        let r = &input_bar.protected_ranges()[0];
        assert_eq!((r.start, r.end), (0, 4));
        assert_eq!(r.uri, "file:///foo");
    }

    #[test]
    fn replace_trigger_token_shifts_ranges_after_replacement() {
        // When the trigger sits between two existing atoms, the trailing
        // atom's offsets must shift by `replacement.len() - removed_len`.
        use super::replace_trigger_token;

        let mut input_bar = InputBar::new();
        input_bar.insert_atom("@foo", "file:///foo".to_string(), "foo".to_string());
        input_bar.insert_paste(" /m ");
        input_bar.insert_atom("@bar", "file:///bar".to_string(), "bar".to_string());
        // Layout: `@foo /m @bar`
        //          0123 4 5678 9012
        assert_eq!(input_bar.text(), "@foo /m @bar");
        assert_eq!(input_bar.protected_ranges().len(), 2);

        // Replace `/m` (prefix_start=5, cursor=7) with `/model`.
        // We must rewind the cursor so it sits at the end of `/m` (=7) before
        // calling — that's the contract `apply_accept` honors.
        input_bar.set_text_cursor_for_test(7);
        replace_trigger_token(&mut input_bar, 5, "/model");

        assert_eq!(input_bar.text(), "@foo /model @bar");
        let ranges = input_bar.protected_ranges();
        assert_eq!(ranges.len(), 2, "both atoms must survive");
        assert_eq!((ranges[0].start, ranges[0].end), (0, 4));
        assert_eq!(
            (ranges[1].start, ranges[1].end),
            (12, 16),
            "trailing atom shifts by +4 (delta = 6 - 2)"
        );
    }

    #[test]
    fn replace_trigger_token_handles_multibyte_neighbors() {
        // Byte offsets — not char positions — drive the shift arithmetic.
        // A range sitting after the trigger across multibyte text must end up
        // at the right *byte* coordinates after substitution.
        use super::replace_trigger_token;

        let mut input_bar = InputBar::new();
        input_bar.insert_atom("@foo", "file:///foo".to_string(), "foo".to_string());
        // `你好` = 6 bytes (3 + 3). Buffer becomes `@foo 你好 /m `.
        input_bar.insert_paste(" 你好 /m ");
        input_bar.insert_atom("@bar", "file:///bar".to_string(), "bar".to_string());
        let original = input_bar.text();
        assert_eq!(original, "@foo 你好 /m @bar");

        // Locate `/m` by byte offset; cursor at end of `/m`.
        let prefix_start = original.find("/m").expect("prefix");
        let cursor = prefix_start + "/m".len();
        let bar_byte = original.find("@bar").expect("bar atom");

        input_bar.set_text_cursor_for_test(cursor);
        replace_trigger_token(&mut input_bar, prefix_start, "/model");

        assert_eq!(input_bar.text(), "@foo 你好 /model @bar");
        let ranges = input_bar.protected_ranges();
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].start, ranges[0].end), (0, 4));
        // delta = 6 (`/model`) − 2 (`/m`) = +4 bytes.
        assert_eq!(
            (ranges[1].start, ranges[1].end),
            (bar_byte + 4, bar_byte + 4 + "@bar".len()),
        );
    }

    #[test]
    fn replace_trigger_token_zero_width_carve_shifts_trailing_atom() {
        // `prefix_start == cursor` (e.g. the user types `@`, picker opens
        // immediately, accept fires before any query characters land).
        // No bytes are removed; trailing atoms shift by exactly the
        // replacement length.
        use super::replace_trigger_token;

        let mut input_bar = InputBar::new();
        input_bar.insert_atom("@foo", "file:///foo".to_string(), "foo".to_string());
        input_bar.insert_paste(" ");
        input_bar.insert_atom("@bar", "file:///bar".to_string(), "bar".to_string());
        // Insert at the boundary between ` ` and `@bar` (byte 5).
        input_bar.set_text_cursor_for_test(5);
        replace_trigger_token(&mut input_bar, 5, "INS");

        assert_eq!(input_bar.text(), "@foo INS@bar");
        let ranges = input_bar.protected_ranges();
        assert_eq!(ranges.len(), 2);
        assert_eq!((ranges[0].start, ranges[0].end), (0, 4));
        assert_eq!((ranges[1].start, ranges[1].end), (8, 12));
    }

    #[test]
    fn two_worker_mentions_both_survive_into_worker_hint() {
        // End-to-end regression for the reported bug: typing two `@worker:*`
        // atoms in the composer must surface both names in the
        // `[UI hint] User-suggested workers ...` line. Before the fix,
        // accepting the second mention dropped the first range, so the hint
        // only listed the most recent worker.
        use std::collections::HashSet;

        use spur_acp::{ContentBlock, TextContent};

        use crate::mentions::hint::prepend_worker_hint;

        use super::replace_trigger_token;

        let mut input_bar = InputBar::new();
        // First accepted worker atom — already in the buffer.
        input_bar.insert_atom(
            "@worker:codex",
            "worker://codex".to_string(),
            "codex".to_string(),
        );
        // User types ` @ki` (plain text).
        input_bar.insert_paste(" @ki");
        assert_eq!(input_bar.text(), "@worker:codex @ki");

        // Picker accept-step 1: carve out the `@ki` typed trigger.
        replace_trigger_token(&mut input_bar, 14, "");
        // Picker accept-step 2: insert the second worker atom.
        input_bar.insert_atom(
            "@worker:kimi",
            "worker://kimi".to_string(),
            "kimi".to_string(),
        );

        assert_eq!(input_bar.text(), "@worker:codex @worker:kimi");
        assert_eq!(
            input_bar.protected_ranges().len(),
            2,
            "both worker atoms must remain protected"
        );

        // Downstream: the worker-hint builder reads protected_ranges. Both
        // worker names must reach the hint.
        let known: HashSet<String> = ["codex", "kimi", "gemini"]
            .into_iter()
            .map(String::from)
            .collect();
        let mut blocks: Vec<ContentBlock> = vec![ContentBlock::Text(TextContent::new("user text"))];
        let prepended = prepend_worker_hint(&mut blocks, input_bar.protected_ranges(), &known);
        assert!(prepended);
        let hint = match &blocks[0] {
            ContentBlock::Text(t) => t.text.clone(),
            _ => panic!("first block should be the hint"),
        };
        assert!(hint.contains("codex"), "hint missing codex: {hint}");
        assert!(hint.contains("kimi"), "hint missing kimi: {hint}");
    }

    #[test]
    fn apply_accept_twice_via_handle_picker_key_keeps_both_atoms() {
        // Defends against the highest-confidence Gate 3 finding: the regression
        // path is `handle_picker_key` → `apply_accept` → `replace_trigger_token`
        // run twice in sequence. If anyone reverts `apply_accept` (or
        // `replace_trigger_token`) to a `set_text` route, this test fails.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("alpha.txt"), "a").unwrap();
        let command_registry = CommandRegistry::new();
        let mention_registry = Rc::new(RefCell::new(MentionRegistry::new()));
        let mut input_bar = InputBar::new();
        let mut completion = InputCompletionPort::new();

        // Round 1: type `@`, picker opens at offset 0, Tab accepts.
        input_bar.set_text("@".to_string(), 1);
        completion.dispatch(
            IntentEvent::TypedChar('@'),
            &mut input_bar,
            &env(&command_registry, &mention_registry, tmp.path()),
        );
        assert!(completion.is_active());
        let first = completion.handle_picker_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut input_bar,
        );
        assert!(matches!(first, Some(RetrievalAccept::InsertAtom { .. })));
        assert_eq!(input_bar.protected_ranges().len(), 1);

        // Splice ` @` as plain text (single-line `insert_paste` does not
        // create a range and does not wipe existing ranges), then dispatch
        // a synthetic TypedChar('@') so the detector sees the new `@` as
        // freshly typed at the cursor's previous byte.
        input_bar.insert_paste(" @");
        completion.dispatch(
            IntentEvent::TypedChar('@'),
            &mut input_bar,
            &env(&command_registry, &mention_registry, tmp.path()),
        );
        assert!(completion.is_active(), "second mention picker should open");

        let second = completion.handle_picker_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut input_bar,
        );
        assert!(matches!(second, Some(RetrievalAccept::InsertAtom { .. })));
        assert_eq!(
            input_bar.protected_ranges().len(),
            2,
            "two completion accepts in sequence must leave two protected atoms; \
             a regression to `set_text` in apply_accept would drop the first"
        );
    }

    #[test]
    fn slash_arg_accept_preserves_trailing_atom() {
        // Gate 3 #2: prior tests cover a slash-arg accept with a clean buffer
        // and a free-function `replace_trigger_token` call with flanking
        // atoms. Neither covers the *combined* path: a real slash-arg accept
        // via `handle_picker_key` while a protected atom sits past the arg
        // region. (Atoms before a slash are impossible by design — slash
        // commands fire only at byte offset 0.)
        use crate::commands::entry::{CommandEntry, CommandSource, Dispatch};
        use crate::components::input_bar::{ProtectedRange, RangeKind};
        use crate::input_history::InputStateSnapshot;
        use spur_acp::adapter::arg_picker_hint::{ArgPickerHint, ArgPickerSpec};
        use spur_acp::{SessionConfigId, SessionConfigOption, SessionConfigSelectOption};

        let mut command_registry = CommandRegistry::new();
        command_registry.set_advertised_commands(
            "codex",
            vec![CommandEntry {
                name: "model".into(),
                description: "Switch model".into(),
                hint: None,
                source: CommandSource::Advertised {
                    handle: "codex".into(),
                },
                dispatch: Dispatch::SetSessionConfigOption {
                    config_id: "model".into(),
                },
                arg_picker_spec: Some(ArgPickerSpec {
                    free_text_hint: String::new(),
                    typed_hint: Some(ArgPickerHint::ConfigOption {
                        config_id: "model".into(),
                    }),
                }),
            }],
        );
        let opts = vec![SessionConfigOption::select(
            SessionConfigId::new("model".to_string()),
            "Model".to_string(),
            "gpt-5".to_string(),
            vec![SessionConfigSelectOption::new(
                "gpt-5".to_string(),
                "GPT-5".to_string(),
            )],
        )];
        let mention_registry = Rc::new(RefCell::new(MentionRegistry::new()));
        let mut input_bar = InputBar::new();
        let mut completion = InputCompletionPort::new();

        // Buffer: `/model  @bar` (extra space so the trailing atom doesn't
        // butt up against the inserted arg). Cursor at byte 7 — right after
        // the first space — so the SlashArg picker opens with empty arg.
        let snapshot = InputStateSnapshot::new(
            "/model  @bar".to_string(),
            vec![ProtectedRange {
                start: 8,
                end: 12,
                kind: RangeKind::Atom,
                uri: "file:///bar".into(),
                name: "bar".into(),
            }],
        );
        input_bar.set_state(snapshot, 7);

        completion.dispatch(
            IntentEvent::Pasted,
            &mut input_bar,
            &env_with_options(
                &command_registry,
                &mention_registry,
                std::path::Path::new("."),
                &opts,
            ),
        );
        assert!(completion.is_active(), "slash-arg picker must open");

        let accepted = completion.handle_picker_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut input_bar,
        );
        assert!(matches!(
            accepted,
            Some(RetrievalAccept::ReplaceTriggerToken { .. })
        ));

        assert_eq!(input_bar.text(), "/model gpt-5 @bar");
        let ranges = input_bar.protected_ranges();
        assert_eq!(ranges.len(), 1, "trailing atom must survive the accept");
        // delta = 5 (`gpt-5`) − 0 (empty arg before) = +5.
        assert_eq!((ranges[0].start, ranges[0].end), (13, 17));
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

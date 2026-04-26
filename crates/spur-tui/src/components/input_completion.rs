#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::commands::CommandRegistry;
    use crate::components::completion_trigger::IntentEvent;
    use crate::components::input_bar::InputBar;
    use crate::components::query_source::RetrievalAccept;
    use crate::mentions::registry::CompletionScope;
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
            &env(&command_registry, &mention_registry, std::path::Path::new(".")),
        );

        assert!(completion.is_active());

        let accepted = completion.handle_picker_key(
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            &mut input_bar,
        );

        assert!(matches!(
            accepted,
            Some(RetrievalAccept::ReplaceTriggerToken { .. })
        ));
        assert!(matches!(
            input_bar.text().as_str(),
            "/help " | "/quit " | "/clear " | "/review "
        ));
        assert!(!completion.is_active());
    }
}

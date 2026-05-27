use super::*;

impl App {
    pub(super) fn process_scroll(&mut self, action: Action) -> Option<Action> {
        match action {
            // Scroll actions are already handled inside the views' handle_key methods.
            Action::ScrollUp
            | Action::ScrollDown
            | Action::ScrollToTop
            | Action::ScrollToBottom
            | Action::CycleFocus
            | Action::Tick => None,
            _ => None,
        }
    }
}

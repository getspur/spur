use spur_acp::SessionId;

/// Actions that flow between components and the app controller.
#[derive(Debug, Clone)]
pub enum Action {
    Quit,
    NavigateTo(ViewId),
    NavigateBack,
    SendMessage {
        session: SessionId,
        text: String,
        interrupt: bool,
    },
    ToggleVerbose,
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    CycleFocus,
    ShowHelp,
    HideHelp,
    Tick,
}

/// Identifies which view is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewId {
    Dashboard,
    SessionDetail(SessionId),
}

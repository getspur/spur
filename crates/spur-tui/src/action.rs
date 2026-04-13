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
    PermissionGrant(PermissionChoice),
    RequestSessions,
    ResumeSession { session_id: String },
    /// Cycle the active Claude session between `default` and `plan` mode.
    /// Dispatched by `Alt-m` in `SessionDetailView`.
    TogglePlanMode,
    /// The orchestrator reported that the current session requires
    /// authentication. Surfaced as a dismissable banner by
    /// `SessionDetailView`. The string is the user-facing message.
    AuthRequired(String),
}

/// Which permission option the user selected.
#[derive(Debug, Clone)]
pub enum PermissionChoice {
    /// [y] — select the first (allow) option
    Allow,
    /// [a] — select the always-allow option
    AlwaysAllow,
    /// [n] — deny (drop the reply channel)
    Deny,
}

/// Identifies which view is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewId {
    Dashboard,
    SessionDetail(SessionId),
    SessionPicker,
}

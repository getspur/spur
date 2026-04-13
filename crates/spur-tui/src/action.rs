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
    /// Push a trace entry showing the current session cost.
    ShowSessionCost,
    Tick,
    PermissionGrant(PermissionChoice),
    RequestSessions,
    ResumeSession { session_id: String },
    /// Cycle the active Claude session between `default` and `plan` mode.
    /// Dispatched by `Alt-m` in `SessionDetailView`.
    TogglePlanMode,
    /// Move tree selection down one row.
    SelectNext,
    /// Move tree selection up one row.
    SelectPrev,
    /// Focus the currently-selected executor node (right pane → detail mode).
    FocusNode,
    /// Unfocus (right pane → chronological log).
    UnfocusNode,
    /// Jump to the next executor with a pending review.
    JumpToReview,
    /// Toggle collapse on the selected subtree.
    ToggleCollapse,
    /// Submit a review decision for the given executor.
    SubmitReview {
        executor_id: String,
        decision: spur_core::ReviewDecision,
    },
    /// Request the app to render a mermaid diagram on a blocking worker.
    /// Emitted by `SessionDetailView::tick` when a new fence closes.
    #[cfg(feature = "markdown")]
    MermaidRenderRequest {
        session: SessionId,
        ref_id: crate::components::mermaid::MermaidId,
        code: String,
    },
    /// Completion of a previously-dispatched render request.
    #[cfg(feature = "markdown")]
    MermaidRenderCompleted {
        session: SessionId,
        ref_id: crate::components::mermaid::MermaidId,
        result: Result<std::sync::Arc<image::DynamicImage>, String>,
    },
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
    #[cfg(feature = "markdown")]
    MermaidOverlay(SessionId),
}

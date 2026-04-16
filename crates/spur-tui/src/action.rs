use spur_acp::SessionId;

/// Actions that flow between components and the app controller.
#[derive(Debug, Clone)]
pub enum Action {
    Quit,
    NavigateTo(ViewId),
    NavigateBack,
    SendMessage {
        session: SessionId,
        blocks: Vec<spur_acp::ContentBlock>,
        interrupt: bool,
    },
    /// Spawn a new session and send these blocks as the first prompt atomically.
    /// Emitted by Dashboard's InputBar when no brain is attached, and by the
    /// picker's NewSessionRequested path once wired (blocks empty = no first prompt).
    NewSessionWithMessage {
        blocks: Vec<spur_acp::ContentBlock>,
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
    ResumeSession {
        session_id: String,
    },
    /// Toggle the pinned flag for a session in the picker metadata store.
    ToggleSessionPin {
        session_id: String,
    },
    /// Toggle the archived flag for a session in the picker metadata store.
    ToggleSessionArchive {
        session_id: String,
    },
    /// Toggle the picker's view-level show-archived flag.
    ToggleShowArchived,
    /// Commit an inline rename from the picker to metadata `title_override`.
    RenameSession {
        session_id: String,
        new_title: String,
    },
    /// Persist a session's unsent InputBar text to metadata.
    SaveDraft {
        session_id: String,
        draft: String,
    },
    /// User requested spawning a new session from the picker.
    NewSessionRequested,
    /// Re-issue `ListSessions` to refresh the picker's agent-side state.
    RefreshSessions,
    /// Cycle the active Claude session between `default` and `plan` mode.
    /// Dispatched by `Alt-m` in `SessionDetailView`.
    TogglePlanMode,
    /// Invoke an agent vendor-extension RPC.
    VendorExec {
        session: SessionId,
        /// Full wire method (e.g. `"_kiro.dev/commands/execute"`).
        method: String,
        params: serde_json::Value,
    },
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
        attempt_n: u32,
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
    /// Halt an in-flight agent stream via ACP cancel. Emitted by
    /// `SessionDetailView` when the user presses `Esc` and a stream is live.
    /// The orchestrator matches the corresponding `UserInput::CancelStream`
    /// inside its streaming `select!` loop and calls `AgentConnection::cancel`.
    CancelStream {
        session: SessionId,
    },
    /// Navigate to Dashboard with Agents panel focused and the
    /// highest-priority executor pre-selected (AwaitingReview > Running
    /// > most recent worker). Emitted by Alt+w in SessionDetailView.
    InspectWorkers,
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

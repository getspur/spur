use spur_acp::SessionId;

/// Issue-related actions dispatched from IssuesPanel or slash commands.
#[derive(Debug, Clone)]
pub enum IssueAction {
    ViewDetail {
        id: String,
    },
    UpdateStatus {
        id: String,
        status: String,
        via_legacy_key: bool,
    },
    WorkOn {
        id: String,
    },
    Execute {
        id: String,
    },
    ExecuteEdit {
        id: String,
    },
}

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
    /// Retire the active brain and reset view state. The user's next
    /// prompt will lazy-spawn a fresh brain with a new `spur_session_id`.
    /// A spur-local meta-command (client-owned): it does NOT forward
    /// `/clear` text to the agent.
    ClearSession,
    ToggleVerbose,
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    CycleFocus,
    ShowHelp,
    HideHelp,
    /// Triple-Esc panic reset back to the Dashboard root.
    PanicReset,
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
        via_legacy_key: bool,
    },
    /// Toggle the picker's view-level show-archived flag.
    ToggleShowArchived,
    /// Commit an inline rename from the picker to metadata `title_override`.
    RenameSession {
        session_id: String,
        new_title: String,
        /// Title in place before this rename. Used by tombstone undo to construct inverse.
        original_title: String,
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
    /// Copy a session id to the system clipboard via OSC 52.
    /// Emitted by the picker's `y` keybind.
    CopySessionId(String),
    /// Cycle the active Claude session between `default` and `plan` mode.
    /// Dispatched by `Alt-m` in `SessionDetailView`.
    TogglePlanMode,
    /// Toggle input bar between Emacs and Vim editing modes.
    /// Dispatched by `Alt-v` or `/vim` slash command.
    ToggleVimMode,
    /// Invoke an agent vendor-extension RPC.
    VendorExec {
        session: SessionId,
        /// Full wire method (e.g. `"_kiro.dev/commands/execute"`).
        method: String,
        params: serde_json::Value,
    },
    /// Apply an ACP `session/set_config_option` for v1 codex /model and
    /// /effort slash pickers. The orchestrator looks up the active brain
    /// session itself; no `session` field needed.
    SetSessionConfigOption {
        config_id: String,
        value: String,
    },
    /// Dedicated `session/set_model` dispatch (M9 F-C). Emitted by
    /// the submit-router consumer when caps advertise
    /// `supports_set_model()` so the orchestrator can route through
    /// `AgentConnection::set_session_model` instead of the legacy
    /// `set_session_config_option` fallback. The variant carries
    /// `session_id` for forward-compat parity with other session-scoped
    /// actions even though the orchestrator currently looks up the
    /// active brain itself.
    SetSessionModel {
        session_id: SessionId,
        value: String,
    },
    /// Move tree selection down by N rows.
    SelectNextBy(usize),
    /// Move tree selection up by N rows.
    SelectPrevBy(usize),
    /// Focus the currently-selected executor node (right pane → detail mode).
    FocusNode,
    /// Unfocus (right pane → chronological log).
    UnfocusNode,
    /// Jump to the next executor with a pending review.
    JumpToReview,
    /// Jump to the previous executor with a pending review.
    JumpToPreviousReview,
    /// Toggle collapse on the selected subtree.
    ToggleCollapse,
    /// Submit a review decision for the given executor.
    SubmitReview {
        executor_id: String,
        attempt_n: u32,
        decision: spur_core::ReviewDecision,
    },
    /// Bare ACP-dispatch path for SubmitReview. Constructed ONLY by the
    /// SubmitReview install arm or by the tick-expiry/displacement-flush path.
    /// The process_action arm performs the actual orchestrator send WITHOUT
    /// installing a tombstone. Tombstone's pending field stores this variant;
    /// tick-expiry dispatches it. Never emitted by views.
    SubmitReviewDispatch {
        executor_id: String,
        attempt_n: u32,
        decision: spur_core::ReviewDecision,
    },
    /// Request the app to render a mermaid diagram on a blocking worker.
    /// Emitted when a new fence closes in `SessionDetailView`.
    #[cfg(feature = "markdown")]
    MermaidRenderRequest {
        session: SessionId,
        ref_id: crate::components::mermaid::MermaidId,
        code: String,
        /// Target raster width in pixels. Currently the renderer always uses
        /// `mermaid::DEFAULT_WIDTH`; the field is plumbed through so a future
        /// resize-aware re-raster path can vary the bucket without churning
        /// the action shape again.
        target_width: u32,
    },
    /// Completion of a previously-dispatched render request. `target_width`
    /// echoes the request bucket; consumers may ignore it today.
    #[cfg(feature = "markdown")]
    MermaidRenderCompleted {
        session: SessionId,
        ref_id: crate::components::mermaid::MermaidId,
        target_width: u32,
        result: Result<std::sync::Arc<image::DynamicImage>, String>,
    },
    /// Halt an in-flight agent stream via ACP cancel. Emitted by
    /// `SessionDetailView` when the user presses `Esc` and a stream is live.
    /// The orchestrator matches the corresponding `UserInput::CancelStream`
    /// inside its streaming `select!` loop and calls `AgentConnection::cancel`.
    CancelStream {
        session: SessionId,
    },
    /// Refresh the tracked issues list from the PM backend.
    RefreshIssues,
    /// Refresh persisted plan summaries from the PM backend.
    RefreshPlans,
    /// Claim a persisted plan for the current brain session without starting execution.
    ClaimPlan {
        plan_id: String,
    },
    /// Resume a persisted plan.
    ResumePlan {
        plan_id: String,
    },
    /// Open a persisted implementation-plan snapshot by plan id.
    InspectPlan {
        session_id: SessionId,
        plan_id: String,
    },
    /// Navigate to Sprints and focus a persisted implementation plan.
    OpenPlanInBrowser {
        plan_id: String,
    },
    /// Fetch the dependency subgraph for an issue from the PM backend.
    GetIssueGraph {
        id: String,
    },
    /// Navigate to Backlog and open issue detail for the given issue id.
    OpenIssueInBacklog {
        id: String,
    },
    /// Show a short status-bar hint owned by the App transient hint channel.
    FlashHint {
        message: String,
    },
    /// Replace the active view's InputBar contents and focus the composer.
    PrefillInput {
        text: String,
    },
    /// An issue-related action from the IssuesPanel or slash commands.
    Issue(IssueAction),
    /// Navigate to Dashboard with Agents panel focused and the
    /// highest-priority executor pre-selected (AwaitingReview > Running
    /// > most recent worker). Emitted by Alt+g in SessionDetailView.
    InspectWorkers,
    /// Plan C Tier 2 — show the capability-tease modal in response to
    /// a TUI-side feature-gate denial. The orchestrator-resolved
    /// `required_tier` (if any) is surfaced as the upgrade target;
    /// `None` means the policy does not yet grant the key on any tier.
    ShowUpgradeModal {
        err: spur_license::FeatureGateError,
        required_tier: Option<spur_license::Plan>,
    },
    OpenInsights,
    /// Spur-local `/theme` slash command. `arg` is the raw rest after the
    /// `/theme` token: `""` opens the interactive theme picker, `"reload"`
    /// re-resolves the active theme name from disk, and any other value is
    /// treated as a theme name to switch to via `load_runtime_theme`.
    ThemeCommand {
        arg: String,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ViewId {
    Dashboard,
    IssueBrowser,
    PlanBrowser,
    SessionDetail(SessionId),
    SessionPicker,
    PlanInspector(SessionId),
    #[cfg(feature = "markdown")]
    MermaidOverlay(SessionId),
    Insights,
}

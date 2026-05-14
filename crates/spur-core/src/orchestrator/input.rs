use agent_client_protocol::schema::ContentBlock;
use spur_acp::SessionId;

/// A user input message from the TUI.
#[derive(Debug)]
#[non_exhaustive]
#[allow(clippy::large_enum_variant)]
pub enum InteractiveInput {
    /// Initialize and warm the brain transport without creating an ACP
    /// session yet. Used by dashboard startup to reduce first-prompt latency.
    WarmConnect,
    Message {
        blocks: Vec<ContentBlock>,
        interrupt: bool,
    },
    /// Spawn a fresh brain session and send these blocks as the first prompt
    /// atomically. If a brain is already attached, it is shut down first.
    /// Empty `blocks` means spawn-only with no first prompt.
    NewSessionWithMessage {
        blocks: Vec<ContentBlock>,
        interrupt: bool,
    },
    ListSessions,
    ResumeSession {
        session_id: String,
    },
    /// Request `set_session_mode` on the active brain session. No-op if
    /// there is no active brain session.
    SetSessionMode {
        mode_id: String,
    },
    /// Request `set_session_config_option` on the active brain session for
    /// the v1 codex `/model` and `/effort` slash pickers. No-op if there is
    /// no active brain session. On success, refreshes the orchestrator's
    /// cached `config_options` from the response.
    SetSessionConfigOption {
        config_id: String,
        value: String,
    },
    /// Dedicated `session/set_model` dispatch (M9 F-C). Fired when the
    /// caps-aware submit-router routes `/model <value>` for an agent that
    /// advertises `supports_set_model()` (e.g. claude-code-acp). The
    /// orchestrator delegates to `AgentConnection::set_session_model`,
    /// which carries its own state-gated fallback to
    /// `session/set_config_option` for agents that lack the dedicated
    /// method. No-op when there is no active brain session.
    SetSessionModel {
        value: String,
    },
    /// Invoke an agent vendor-extension RPC on the active brain session.
    /// No-op if there is no active brain session. The method name and params
    /// are chosen by the TUI's config-driven dispatch path — the
    /// orchestrator is agnostic to specific extensions. `sessionId` is
    /// injected into `params` here (the TUI doesn't know ACP session IDs).
    VendorExec {
        session: SessionId,
        method: String,
        params: serde_json::Value,
    },
    /// Submit a human review decision. Routed to the ReviewSink by the
    /// dispatcher task, not handled inline in `run_interactive`.
    SubmitReview {
        executor_id: String,
        attempt_n: u32,
        decision: spur_acp::ReviewDecision,
    },
    /// Halt the currently streaming prompt (if any) via `AgentConnection::cancel`.
    /// When received inside the streaming `select!`, calls `cancel()` and arms
    /// the 5s force-timeout. When received outside the streaming loop (no
    /// active turn), dropped with a debug log (the view guards against emitting
    /// this unless a stream is in-flight, but a TurnComplete-vs-Esc race can
    /// still produce a stray one).
    CancelStream {
        session: SessionId,
    },
    /// Refresh the issue list and re-emit IssuesLoaded.
    RefreshIssues,
    /// Refresh persisted plan summaries and emit PlansLoaded.
    RefreshPlans,
    /// Claim a persisted plan for this brain without starting execution.
    ClaimPlan {
        plan_id: String,
    },
    /// Force claim a persisted plan from another brain.
    ForceReclaimPlan {
        plan_id: String,
    },
    /// Resume a persisted plan.
    ResumePlan {
        plan_id: String,
    },
    /// Load/project a persisted implementation plan and emit PlanSnapshotUpdated.
    /// This is read-only and must not claim plan ownership.
    InspectPlan {
        plan_id: String,
    },
    /// Fetch full issue detail and emit IssueDetailFetched.
    GetIssueDetail {
        id: String,
    },
    /// Fetch an issue dependency subgraph and emit IssueSubgraphLoaded.
    GetIssueGraph {
        id: String,
    },
    /// Update an issue and emit IssueUpdated.
    UpdateIssue {
        id: String,
        update: spur_pm::IssueUpdate,
    },
    /// Add a comment to an issue and re-emit IssueDetailFetched.
    AddIssueComment {
        issue_id: String,
        body: String,
    },
    /// Detached delegation completion returned to the orchestrator for
    /// scheduled brain re-entry. Never constructed by the TUI. See
    /// `docs/superpowers/specs/2026-04-19-brain-async-continuation-design.md`.
    SystemContinuation {
        session: SessionId,
        continuation: spur_acp::domain::BrainContinuation,
    },
}

/// Strip a leading `!` from the first text block in `blocks`, if any.
///
/// The TUI forwards interrupt commands (`!stop`) as a text block with a
/// leading bang. We strip it once here before forwarding to the agent so
/// the agent sees clean prompt text.
pub(super) fn strip_bang_prefix(mut blocks: Vec<ContentBlock>) -> Vec<ContentBlock> {
    if let Some(ContentBlock::Text(tc)) = blocks.first_mut() {
        if tc.text.starts_with('!') {
            tc.text = tc.text.strip_prefix('!').unwrap_or(&tc.text).to_string();
        }
    }
    blocks
}

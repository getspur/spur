use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;

use crate::domain::continuation::{DeferReason, DropReason};
use crate::domain::delegation::{DelegationId, DelegationStatus};
use crate::types::{CancelMode, SessionId};
use agent_client_protocol::schema::{SessionConfigOption, SessionInfo, SessionNotification};

mod option_arc_spur_agent_caps_serde {
    use std::sync::Arc;

    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(
        caps: &Option<Arc<crate::SpurAgentCaps>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        caps.as_deref().serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<Option<Arc<crate::SpurAgentCaps>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<crate::SpurAgentCaps>::deserialize(deserializer).map(|caps| caps.map(Arc::new))
    }
}

/// Review kind for `ExecutorReviewRequested`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReviewKind {
    Completion,
    Failure,
    Conflict,
    Checkpoint,
}

/// Whether a file was read or written.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum FileTouchKind {
    Read,
    Write,
}

/// Payload carried with a review request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPayload {
    pub summary: String,
    pub diff_summary: Option<DiffSummary>,
    pub pr_url: Option<String>,
    pub error: Option<String>,
    /// Structured delegation reasoning the brain emitted for this call.
    /// See design spec section C.5.
    #[serde(default)]
    pub delegation_plan: Option<crate::domain::DelegationPlan>,
    /// `Some(false)` when `delegation_plan.chosen` doesn't match the
    /// dispatched agent (after `normalize_agent_name`). Never blocks
    /// dispatch; exposed for reviewer visibility.
    #[serde(default)]
    pub chosen_matches_dispatched: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_influence: Option<PeerInfluenceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PeerInfluenceSummary {
    pub inbound_consumed: u32,
    pub inbound_ignored: u32,
    pub outbound_emitted: u32,
    pub undelivered: u32,
    pub from_unreviewed_source: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiffSummary {
    pub files_changed: usize,
    pub insertions: usize,
    pub deletions: usize,
    pub files: Vec<PathBuf>,
}

/// Artifact kinds emitted by an executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Artifact {
    Diff {
        summary: DiffSummary,
        /// Raw unified-diff text retained for pager display.
        /// `None` means the emitter didn't have the text available
        /// (e.g., replay of a pre-Task-14 event, or synthetic artifact).
        text: Option<String>,
    },
    PrUrl(String),
    FileList(Vec<PathBuf>),
    Text(String),
}

/// User's decision on a review request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ReviewDecision {
    Approve,
    Reject { reason: String },
    Modify { note: String },
    Retry { new_constraints: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LifecycleState {
    Spawning,
    Running,
    AwaitingReview,
    Resuming,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Brain,
    Executor,
    SubExecutor,
}

/// Envelope wrapping every domain event with an occurrence timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpurEvent {
    pub occurred_at: SystemTime,
    /// Monotonic sequence number assigned by the orchestrator's emit
    /// funnel (S2). Direct constructors set this to 0; the funnel
    /// overwrites. Subscribers can detect gaps and order chronologically.
    ///
    /// `#[serde(default)]` so pre-S2 event logs (no `seq` field) deserialize
    /// with `seq = 0` — the same sentinel the funnel uses for un-stamped
    /// events. Keeps Phase S3 JSONL replay backward-compatible.
    #[serde(default)]
    pub seq: u64,
    pub body: SpurEventBody,
}

impl SpurEvent {
    /// Convenience constructor. Use at emission sites. Do NOT call inside
    /// `apply` / projection code — timestamps there must come from the
    /// arriving event.
    ///
    /// Note: `seq` defaults to 0; the orchestrator's emit funnel (S2)
    /// overwrites with a real monotonic value before broadcast.
    pub fn now(body: SpurEventBody) -> Self {
        Self {
            occurred_at: SystemTime::now(),
            seq: 0,
            body,
        }
    }
}

/// Result of attempting `session/load` on a brain connection. Returned
/// from `load_brain_session` so the caller can distinguish "state
/// actually came back" from "we silently created a fresh session."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoadOutcome {
    /// `session/load` returned the prior session state.
    Restored,
    /// `session/load` failed (unsupported, or errored) and we started a
    /// new session. `reason` is the underlying error.
    FellBackToNew { reason: String },
}

/// Issue summary carried in SpurEvents for TUI display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueSummaryEvent {
    pub id: String,
    pub source: String,
    pub title: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Persisted plan summary carried in `PlansLoaded` for Sprints browsing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSummaryEvent {
    pub plan_id: String,
    pub epic_id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_body_preview: Option<String>,
    pub owner_state: PlanOwnerStateEvent,
    pub lifecycle: PlanLifecycleEvent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<PlanSummaryCountsEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Non-fatal issues discovered while loading persisted plan summaries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanLoadWarningEvent {
    pub plan_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_epic_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stale_epic_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_owner_state: Option<PlanOwnerStateEvent>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSummaryCountsEvent {
    pub total: u32,
    pub pending: u32,
    pub ready: u32,
    pub running: u32,
    pub awaiting_review: u32,
    pub approved: u32,
    pub rejected: u32,
    pub failed: u32,
    #[serde(default)]
    pub cancelled: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlanOwnerStateEvent {
    Mine,
    Unowned,
    Other { owner: String },
    Ambiguous { owners: Vec<String> },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum PlanLifecycleEvent {
    Pending,
    Running,
    AwaitingReview,
    Complete,
    Failed,
    Unknown,
}

/// Full issue detail carried in the `IssueDetailFetched` event.
/// Mirrors `spur_pm::Issue` without taking a direct dependency on spur-pm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IssueDetailEvent {
    pub id: String,
    pub source: String,
    pub title: String,
    pub body: String,
    pub status: String,
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_type: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Issue graph node carried in `IssueSubgraphLoaded`.
/// Mirrors `spur_pm::graph::GraphNode` without taking a direct dependency on spur-pm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphNodeEvent {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagerank: Option<f64>,
}

/// Issue graph edge carried in `IssueSubgraphLoaded`.
/// Mirrors `spur_pm::graph::GraphEdge` without taking a direct dependency on spur-pm.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphEdgeEvent {
    pub from: String,
    pub to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge_type: Option<String>,
}

/// Canonical durable plan state rendered by the plan inspector UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSnapshot {
    pub plan_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epic_id: Option<String>,
    pub status: String,
    pub progress: String,
    pub next_action: String,
    pub ready_to_merge: bool,
    pub counts: PlanSnapshotCounts,
    pub tasks: Vec<PlanSnapshotTask>,
    /// Brain session id observed in the projected `PlanState` at snapshot time.
    /// Mirrors the `spur:plan-owner:*` label semantics on the epic. Reads from
    /// `PlanState.brain_session_id`; for projector-rebuilt plans this is the
    /// original submitter and may lag the live label-derived owner if a
    /// transfer happened between projector ticks. Tightening to label-derived
    /// truth requires threading `epic.labels` into the snapshot builder
    /// (tracked as a follow-up). `None` for plans pre-feature or unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_brain_session_id: Option<String>,
    /// Most recently emitted ownership token from the
    /// `PlanOwnershipAcquired` / `PlanOwnershipTransferred` audit sentinel for
    /// this plan. Currently a `None` placeholder — derivation requires
    /// scanning the epic audit history, which is out of scope for the snapshot
    /// builder. TODO: thread the latest token through the projector and
    /// in-memory ownership acquisition path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_token: Option<String>,
    /// Wall-clock timestamp of the latest ownership acquisition or transfer,
    /// derived from the audit sentinel comment timestamp or the brain's
    /// monotonic-mapped wall time at acquisition. Currently a `None`
    /// placeholder — see `owner_token` for derivation notes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_acquired_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PlanSnapshotCounts {
    pub pending: u32,
    pub ready: u32,
    pub dispatched: u32,
    pub awaiting_review: u32,
    pub approved: u32,
    pub rejected: u32,
    pub failed: u32,
    pub cancelled: u32,
    /// bd-2m2u Phase 2d — count of tasks currently in `EscalatedToBrain`
    /// awaiting a brain `submit_plan_mutation` decision.
    #[serde(default)]
    pub escalated: u32,
    /// bd-2m2u Phase 2d — running count of attempts auto-retried with the
    /// amended-prompt recovery path. Derived from `AttemptRecord` history;
    /// observability surface only.
    #[serde(default)]
    pub auto_retried: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSnapshotTask {
    pub task_id: String,
    pub task_name: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_id: Option<String>,
    pub status: String,
    pub attempt: u32,
    pub max_attempts: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unblocks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_summary: Option<DiffSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mutation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub superseded_by: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub next_action: String,
}

/// Snapshot of licensing state mirrored into the ACP event bus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseStateEvent {
    pub status: LicenseStatusEvent,
    pub subject_kind: LicenseSubjectKind,
    pub plan: LicensePlan,
    pub features: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub binding_mode: LicenseBindingMode,
    pub offline_ok: bool,
    pub status_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicenseStatusEvent {
    Inactive,
    Active,
    Degraded,
    Invalid,
    ConfigError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicenseSubjectKind {
    User,
    Organization,
    Ci,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicenseBindingMode {
    NodeLocked,
    FloatingCi,
    Organization,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LicensePlan {
    Community,
    StarterLtd,
    BuilderLtd,
    FounderLtd,
    Pro,
    Team,
    Enterprise,
    Unknown,
}

/// Why a brain session was retired. Companion to [`SpurEventBody::BrainRetired`].
/// See docs/superpowers/specs/2026-04-19-clear-command-session-reset-design.md.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub enum BrainRetireReason {
    /// User invoked `/clear`. Spur-local meta-command.
    UserClear,
    /// Session swap via `ResumeSession` (user selected a different session).
    ResumeSwitch,
    /// Orchestrator shutting down.
    Shutdown,
}

/// The discriminated payload of a [`SpurEvent`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SpurEventBody {
    /// The orchestrator has started a connect-only brain prewarm.
    ///
    /// No ACP session exists yet; this only covers `initialize()` and the
    /// transport/process setup needed to make the first prompt faster.
    BrainConnectStarted {
        brain: String,
    },
    /// Connect-only brain prewarm completed successfully.
    ///
    /// No ACP session exists yet. The next prompt may reuse the warmed
    /// connection and call `new_session()` lazily.
    BrainConnected {
        brain: String,
    },
    /// Connect-only brain prewarm failed before any ACP session was created.
    BrainConnectFailed {
        brain: String,
        reason: String,
    },
    BrainSpawned {
        agent: String,
        session: SessionId,
    },
    /// Emitted AFTER a brain session is established (fresh or resumed) and
    /// the agent-authoritative ACP session id is known. The TUI persists
    /// the (spur_id → acp_id, brain) mapping so the next `spur watch` run
    /// can resume by the real ACP id.
    ///
    /// - `session`: the SPUR session id (matches the earlier `BrainSpawned`).
    /// - `acp_session_id`: the id the agent assigned (stable across runs
    ///   where the agent supports `session/load`).
    /// - `brain`: the brain agent name that owns this ACP id.
    /// - `resumed`: `true` iff `session/load` succeeded. `false` when the
    ///   path fell back to `new_session` or spawned fresh.
    AgentSessionReady {
        session: SessionId,
        acp_session_id: String,
        brain: String,
        resumed: bool,
        /// How `session/cancel` is implemented for this session's transport.
        /// The TUI uses this to render transport-aware cancel feedback.
        cancel_mode: CancelMode,
        /// True when this session was attached without an enforceable
        /// lockfile (NFS/sshfs/SMB). Multi-instance protection is OFF.
        fs_unsafe: bool,
        /// Caps snapshot at session-create. None for resumed-pre-M9 sessions
        /// (M8 §F-3 permissive fallback).
        #[serde(default, with = "option_arc_spur_agent_caps_serde")]
        caps: Option<Arc<crate::SpurAgentCaps>>,
    },
    SessionAttachRejected {
        acp_session_id: String,
        holder: crate::session_lock::HolderInfo,
        fs_unsafe: bool,
    },
    WorkerPeerMessageAccepted {
        brain_session_id: String,
        message_id: crate::domain::peer_message::PeerMessageId,
        source_delegation_id: crate::domain::delegation::DelegationId,
        target_delegation_id: crate::domain::delegation::DelegationId,
        kind: crate::domain::peer_message::MessageKind,
        sequence: u64,
    },
    WorkerPeerMessageRejected {
        brain_session_id: String,
        message_id: crate::domain::peer_message::PeerMessageId,
        source_delegation_id: crate::domain::delegation::DelegationId,
        target_delegation_id: crate::domain::delegation::DelegationId,
        reason: String,
    },
    WorkerPeerMessageQueued {
        brain_session_id: String,
        message_id: crate::domain::peer_message::PeerMessageId,
        target_delegation_id: crate::domain::delegation::DelegationId,
    },
    WorkerPeerMessageDelivered {
        brain_session_id: String,
        message_id: crate::domain::peer_message::PeerMessageId,
        target_delegation_id: crate::domain::delegation::DelegationId,
        target_prompt_id: String,
        injected_chars: u32,
    },
    WorkerPeerMessageConsumed {
        brain_session_id: String,
        message_id: crate::domain::peer_message::PeerMessageId,
        target_delegation_id: crate::domain::delegation::DelegationId,
    },
    WorkerPeerMessageIgnored {
        brain_session_id: String,
        message_id: crate::domain::peer_message::PeerMessageId,
        target_delegation_id: crate::domain::delegation::DelegationId,
        reason: String,
    },
    /// Diagnostic-only. Emitted at drain entry to anchor latency / saturation
    /// dashboards. Does NOT mutate lineage state. Pairs with the eventual
    /// drain exit event (`WorkerPeerMessageDrainCappedOut` or
    /// `WorkerPeerMessageDrainTimedOut`); a clean exit (quiet window with
    /// `remaining_messages == 0`) emits no exit event by design.
    WorkerPeerMessageDrainStarted {
        brain_session_id: String,
        target_delegation_id: crate::domain::delegation::DelegationId,
        candidates_at_start: u32,
        cap_ms: u64,
        quiet_window_ms: u64,
    },
    /// Diagnostic-only. Do NOT count in message-loss metrics — message loss
    /// is counted via WorkerPeerMessageIgnored per-message events. Use this
    /// for drain-health / worker-behavior dashboards.
    WorkerPeerMessageDrainCappedOut {
        brain_session_id: String,
        target_delegation_id: crate::domain::delegation::DelegationId,
        acks_received: u32,
        remaining_messages: u32,
        cap_ms: u64,
        actual_elapsed_ms: u64,
    },
    /// Diagnostic-only. Emitted when the drain exits via quiet-window
    /// timeout WITH non-terminal messages still in the mailbox. Does NOT
    /// count as message-loss observability — that is per-`WorkerPeerMessageIgnored`.
    /// Use this for drain-health dashboards (worker stopped acking but
    /// did not consume all peer messages).
    ///
    /// Mutually exclusive with `WorkerPeerMessageDrainCappedOut` for any
    /// given drain. Not emitted on the clean-exit path
    /// (`remaining_messages == 0`).
    WorkerPeerMessageDrainTimedOut {
        brain_session_id: String,
        target_delegation_id: crate::domain::delegation::DelegationId,
        acks_received: u32,
        remaining_messages: u32,
        cap_ms: u64,
        quiet_window_ms: u64,
        actual_elapsed_ms: u64,
    },
    /// Worker sent a terminal peer-message notification whose payload could
    /// not be parsed at the `_spur/*` boundary.
    ///
    /// Downstream consumers should treat this as observability for a rejected
    /// worker ack, not as a terminal state transition for the peer message.
    WorkerPeerMessageMalformed {
        brain_session_id: String,
        source_executor_id: String,
        method: String,
        reason: String,
    },
    WorkerPeerMessageExpired {
        brain_session_id: String,
        message_id: crate::domain::peer_message::PeerMessageId,
        target_delegation_id: crate::domain::delegation::DelegationId,
    },
    WorkerPeerMessageDropped {
        brain_session_id: String,
        message_id: crate::domain::peer_message::PeerMessageId,
        target_delegation_id: crate::domain::delegation::DelegationId,
        reason: String,
    },
    WorkerPeerMessageUndeliverable {
        brain_session_id: String,
        message_id: crate::domain::peer_message::PeerMessageId,
        target_delegation_id: crate::domain::delegation::DelegationId,
        reason: String,
    },
    WorkerPeerMessageAuditFailed {
        brain_session_id: String,
        message_id: crate::domain::peer_message::PeerMessageId,
        target_delegation_id: crate::domain::delegation::DelegationId,
        transition_kind: String,
        error: String,
    },
    /// Startup reconciliation found a peer message in an unexpected
    /// non-terminal state and intentionally left it untouched.
    ///
    /// "Stranded" means the reconciler refused to infer a safe lifecycle
    /// transition; operators should inspect the ledger state and reason.
    ///
    /// Stage-1 alerting note: this variant is unreachable in production today
    /// because `record_injection` always precedes the `DeliveredInflight`
    /// transition in the orchestrator. It becomes a real signal once the
    /// Stage-2 persistent ledger introduces failure modes (eviction, async
    /// write loss) that can leave a `DeliveredInflight` entry without
    /// injection records. Alerts on `inflight_stranded > 0` should be staged
    /// for that release.
    WorkerPeerMessageReconciledStranded {
        brain_session_id: String,
        message_id: crate::domain::peer_message::PeerMessageId,
        target_delegation_id: crate::domain::delegation::DelegationId,
        state: crate::domain::peer_message::LedgerState,
        reason: String,
    },
    /// Aggregated counts emitted at the end of `run_startup_reconcile`.
    ///
    /// Counter migration (post-bd-cpf.3): `inflight_stranded` replaces
    /// `inflight_reverted_to_queued` for any operational dashboard or alert
    /// that tracked reconciler-found anomalies. The `inflight_reverted_to_queued`
    /// field is retained for wire compatibility with older replay readers and
    /// is always emitted as 0 going forward — consumers should switch to
    /// `inflight_stranded`.
    WorkerPeerMailboxReconciled {
        brain_session_id: String,
        /// Count of `WorkerPeerMessageAuditFailed` events emitted during
        /// reconciliation. Always 0 prior to bd-cpf.5b. Use the
        /// `WorkerPeerMessageAuditFailed` event type (filtered by
        /// `transition_kind == "reconcile_to_delivered"`) for direct alerting
        /// rather than this counter.
        #[serde(default)]
        audit_failed_emitted: u32,
        inflight_forced_to_delivered: u32,
        /// Count of reconciler entries already in `Delivered` state at
        /// transition time (benign concurrent-advance races). See
        /// `ReconcileCounts::inflight_already_delivered`.
        #[serde(default)]
        inflight_already_delivered: u32,
        #[serde(default)]
        inflight_stranded: u32,
        inflight_reverted_to_queued: u32,
        guards_re_wrapped: u32,
    },
    WorkerSpawned {
        agent: String,
        session: SessionId,
        worktree: PathBuf,
    },
    SessionCompleted {
        session: SessionId,
        success: bool,
    },
    AgentNotification {
        session: SessionId,
        notification: Box<SessionNotification>,
    },
    /// Vendor-extension notification received from the agent side.
    /// Routing by `method` name is the receiver's responsibility.
    /// `method` is the wire form (e.g. `"_kiro.dev/commands/available"`),
    /// with the leading `_` preserved for reader convenience.
    AgentExtNotification {
        session: SessionId,
        method: String,
        params: serde_json::Value,
    },
    /// The cached `config_options` for a session changed and any consumer
    /// rendering advertised slash commands (e.g. `/model`, `/effort`)
    /// should rebuild from the new snapshot. Carries the options inline
    /// so subscribers do not need a separate query path.
    ///
    /// Emitted after `NewSessionResponse.config_options` is captured at
    /// session creation, after a `SetSessionConfigOption` response
    /// refreshes the cache, and (in a future plan) after a
    /// `session/update.ConfigOptionUpdate` notification from the agent.
    CommandRegistryDirty {
        session: SessionId,
        config_options: Vec<SessionConfigOption>,
    },
    DelegationRequested {
        /// Brain session that issued the delegation. Stamped by the MCP
        /// server onto every `DelegationRequest` and threaded through the
        /// orchestrator to this emission site.
        from: SessionId,
        to_agent: String,
        task: String,
        /// UUID matching the spur-mcp `DelegationRequest.id`. Surfaced so
        /// the brain conversation can correlate with the spawned executor
        /// via `DelegationDispatched`.
        request_id: String,
        /// Optional structured plan the brain passed alongside the
        /// delegate_* call. See design spec section C.7.
        #[serde(default)]
        delegation_plan: Option<crate::domain::DelegationPlan>,
        /// Issue ID linked to this delegation (if any). Set when the
        /// brain tool call carried an `issue_id` field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        issue_id: Option<String>,
    },
    /// Emitted after the worker worktree base has been finalized and before
    /// agent initialization, so lineage can explain which BaseSpec and overlay
    /// closure a dispatch attempt saw.
    DispatchOverlayApplied {
        request_id: String,
        base_spec: serde_json::Value,
        dispatched_base_oid: String,
        overlay_task_ids: Vec<String>,
    },
    /// Emitted immediately after the orchestrator spawns an executor
    /// for a brain delegation. Lets the brain-side session_detail
    /// view correlate its `DelegationRequested` trace entry with the
    /// new executor node so an inline executor card can render.
    DelegationDispatched {
        /// Brain session that issued the delegation. Stamped by the MCP
        /// server onto every `DelegationRequest` and threaded through the
        /// orchestrator to this emission site.
        from: SessionId,
        /// Matches the `request_id` on `DelegationRequested` /
        /// `DelegationRequest.id` (UUID).
        request_id: String,
        /// The executor node now spawned for this delegation.
        executor_id: String,
    },
    DelegationCompleted {
        worker_session: SessionId,
        status: DelegationStatus,
    },
    WorkerMcpDelegationSummary {
        delegation_id: String,
        brain_session_id: String,
        calls_total: u64,
        calls_by_tool: BTreeMap<String, u64>,
        p99_latency_ms: u64,
        errors: u64,
    },
    ConflictDetected {
        files: Vec<PathBuf>,
    },
    RateLimitDetected {
        agent: String,
        retry_after: Option<Duration>,
    },
    BrainFailover {
        from: String,
        to: String,
    },
    CostUpdate {
        session: SessionId,
        agent: String,
        estimated_cost_usd: f64,
    },
    IssueReceived {
        source: String,
        id: String,
    },
    PrCreated {
        url: String,
    },
    IssueUpdated {
        source: String,
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        assignee: Option<String>,
    },
    /// Emitted once at session start with all tracked issues.
    IssuesLoaded {
        issues: Vec<IssueSummaryEvent>,
    },

    /// Emitted when the Sprints surface requests persisted plan summaries.
    PlansLoaded {
        plans: Vec<PlanSummaryEvent>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        warnings: Vec<PlanLoadWarningEvent>,
    },

    /// Response to a TUI request for full issue detail.
    /// Follows SessionsListed / IssuesLoaded precedent for request-response on broadcast.
    IssueDetailFetched {
        /// The ID that was requested — TUI checks against focused issue
        /// to discard stale responses from navigation races.
        requested_id: String,
        /// Full issue data from PmService.
        issue: IssueDetailEvent,
    },

    /// Response to a TUI request for the dependency subgraph around one issue.
    IssueSubgraphLoaded {
        /// The ID that was requested — TUI checks this against graph loading
        /// state and current selection to discard stale responses.
        requested_id: String,
        nodes: Vec<GraphNodeEvent>,
        edges: Vec<GraphEdgeEvent>,
    },

    /// Feedback for a failed issue operation initiated from TUI.
    IssueCommandError {
        operation: String,
        error: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
    },

    /// Feedback for a failed plan operation initiated from TUI.
    PlanCommandError {
        operation: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_id: Option<String>,
        error: String,
    },

    /// Graph health alert summary from bv (beads_viewer) analysis.
    /// Emitted at startup and after each delegation completion.
    GraphAlertsSummary {
        total: usize,
        critical: usize,
        warning: usize,
        /// Human-readable alert messages (top 5) for TUI activity log.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        details: Vec<String>,
    },

    /// Licensing state snapshot, emitted at startup and whenever the
    /// provider refreshes cached state.
    LicenseUpdated {
        state: LicenseStateEvent,
    },

    // ── Interactive loop events ──────────────────────────────────────
    TurnComplete {
        session: SessionId,
    },
    BrainError {
        session: SessionId,
        message: String,
    },
    /// Brain subprocess appears to have died; a reconnect attempt is
    /// starting. Emitted BEFORE `connect_brain` runs so the TUI can
    /// display a banner immediately (subprocess spawn takes >1s).
    BrainReconnecting {
        session: SessionId,
        brain_name: String,
        /// Human-readable reason (usually the RPC error that tripped
        /// the detector).
        reason: String,
    },
    /// Reconnect succeeded. `outcome` says whether session state was
    /// restored or we fell back to a fresh session.
    BrainReconnected {
        session: SessionId,
        brain_name: String,
        outcome: LoadOutcome,
    },
    /// Reconnect attempt failed OR the circuit breaker tripped. The
    /// brain stays unset and the user must take an explicit action to
    /// retry.
    BrainReconnectFailed {
        session: SessionId,
        brain_name: String,
        reason: String,
    },
    /// Emitted at the start of `retire_active_brain` on the resume path.
    /// `from` is the session being retired (None if no active brain).
    /// `to` is the session the user asked to resume. Lets `SessionDetailView`
    /// render a "Retiring previous session…" initial state.
    SessionRetireStart {
        from: Option<SessionId>,
        to: SessionId,
    },
    /// Emitted when `retire_active_brain` completes (clean or forced).
    SessionRetireComplete {
        session: SessionId,
    },
    /// Emitted before `connect_brain` attempts to (re)spawn a brain process
    /// on the resume path. Lets the UI show "Connecting to claude-code…"
    /// while subprocess spawn (≥1s cold) is in flight.
    BrainConnecting {
        session: SessionId,
        brain_name: String,
    },
    /// Emitted before `load_brain_session` issues its ACP `session/load`
    /// RPC. Lets the UI show "Loading session history…".
    SessionLoading {
        session: SessionId,
    },
    /// Emitted after `load_brain_session` returns `Ok` and history replay
    /// has been dispatched. Terminal state for a successful resume.
    SessionLoaded {
        session: SessionId,
    },
    /// The agent subprocess reported that authentication is required
    /// (e.g. `authRequired` error code, "/login" prompt). The TUI renders
    /// this as a dismissable banner instructing the user to run
    /// `claude /login` externally.
    AuthRequired {
        session: SessionId,
        message: String,
    },
    // ── Executor lineage events ────────────────────────────────────
    ExecutorSpawned {
        id: String,
        parent_id: Option<String>,
        session_id: SessionId,
        agent: String,
        role: Role,
        task_spec: String,
    },
    ExecutorPhaseChanged {
        id: String,
        phase: LifecycleState,
    },
    ExecutorArtifact {
        id: String,
        artifact: Artifact,
    },
    ExecutorReviewRequested {
        id: String,
        /// Which attempt this review gates. Propagated back via
        /// `UserInput::SubmitReview` for supersession guard.
        attempt_n: u32,
        kind: ReviewKind,
        payload: ReviewPayload,
    },
    ExecutorReviewResolved {
        id: String,
        decision: ReviewDecision,
    },
    /// The orchestrator abandoned a pending review (e.g., because the
    /// brain's tool call was cancelled). Emitted so the lineage
    /// projection records the abandonment rather than showing a silent
    /// disappearance.
    ExecutorReviewCancelled {
        id: String,
        reason: String,
    },
    ExecutorRetryStarted {
        id: String,
        /// 1-based index of the new attempt; validated against the projection's
        /// current attempt count to detect dropped retry events.
        attempt_n: u32,
        reason: String,
        new_session_id: SessionId,
    },
    // ── Session picker events ───────────────────────────────────────
    SessionsListed {
        agent: String,
        sessions: Vec<SessionInfo>,
    },
    SessionsListError {
        message: String,
    },
    /// Replayed conversation history from disk (when agent doesn't support load_session).
    SessionHistory {
        session: SessionId,
        entries: Vec<HistoryEntry>,
    },

    // ── Worker _spur/* ExtNotification vocabulary (S5) ─────────────
    /// Worker emitted `_spur/heartbeat` — periodic alive signal.
    /// The TUI uses this to detect stalled workers.
    WorkerHeartbeat {
        brain_session_id: SessionId,
        executor_id: String,
        /// Wall-clock at the worker; informational only.
        worker_ts: Option<String>,
    },

    /// Worker emitted `_spur/progress_milestone` — named checkpoint.
    /// The TUI shows this in the executor card.
    WorkerProgress {
        brain_session_id: SessionId,
        executor_id: String,
        name: String,
        /// Optional 0..=100 percentage.
        pct: Option<u8>,
    },

    /// Worker invoked the curated `report_progress` MCP tool with a
    /// free-form status message. Distinct from `WorkerProgress`
    /// (executor-scoped, structured milestone `name`/`u8` percentage)
    /// — this carries an arbitrary delegation-scoped message and an
    /// optional `f64` percent so workers can stream rich progress text
    /// without minting milestone names.
    WorkerReportProgress {
        delegation_id: String,
        message: String,
        percent: Option<f64>,
    },

    /// Live session notification from a running worker agent. Emitted
    /// by the orchestrator for every `SessionNotification` received
    /// from a worker's `drive_prompt_notifications` stream. The TUI
    /// lineage projection converts these into `WorkerStreamEntry`
    /// items on the executor's `stream_buffer` for the detail-pane
    /// Stream tab.
    WorkerNotification {
        brain_session_id: SessionId,
        executor_id: String,
        notification: Box<SessionNotification>,
    },

    /// Worker read or wrote a file. Either emitted explicitly by the
    /// worker via `_spur/file_touched`, or synthesized by the
    /// orchestrator from observed ToolCall events with a 200ms
    /// de-duplication window.
    WorkerFileTouched {
        brain_session_id: SessionId,
        executor_id: String,
        path: std::path::PathBuf,
        kind: FileTouchKind,
    },

    /// Durable beads-backed plan state for a session.
    PlanSnapshotUpdated {
        session_id: SessionId,
        snapshot: Box<PlanSnapshot>,
    },

    /// Brain submitted a review verdict on a plan task.
    PlanTaskReviewed {
        plan_id: String,
        task_id: String,
        /// Human-readable task name derived from task text (first line, 60
        /// chars). `None` on replay of pre-Phase-2 events.
        #[serde(default)]
        task_name: Option<String>,
        /// "approve" | "reject" | "request_changes"
        decision: String,
        feedback: Option<String>,
        attempt: u32,
        /// Attempt budget. Carried in the event so renderers don't need a
        /// cross-crate const import. Defaults to 0 on pre-Phase-2 replay.
        #[serde(default)]
        max_attempts: u32,
    },

    /// A plan task was re-dispatched for iteration (attempt > 1).
    PlanTaskIterating {
        plan_id: String,
        task_id: String,
        /// Human-readable task name. `None` on replay of pre-Phase-2 events.
        #[serde(default)]
        task_name: Option<String>,
        /// New attempt number (the attempt that just started, i.e., old_attempt + 1).
        attempt: u32,
        /// Attempt budget. Defaults to 0 on pre-Phase-2 replay.
        #[serde(default)]
        max_attempts: u32,
        delegation_id: String,
    },

    /// A plan task reached a terminal failed state.
    PlanTaskFailed {
        plan_id: String,
        task_id: String,
        attempt: u32,
        max_attempts: u32,
        error: String,
        delegation_id: String,
    },

    /// A failed plan task was kept open and scheduled for an automatic retry.
    PlanTaskAutoRetried {
        plan_id: String,
        task_id: String,
        delegation_id: String,
        /// Attempt being retried, i.e. the failed attempt.
        attempt: u32,
        max_attempts: u32,
        error: String,
        #[serde(default)]
        worker_branch: Option<String>,
    },

    /// A plan task completed worker execution and is waiting for brain review.
    PlanTaskAwaitingReview {
        plan_id: String,
        task_id: String,
        delegation_id: String,
    },

    /// bd-2m2u Phase 2d — a plan task exhausted its auto-retry budget (1 attempt) and
    /// was promoted to `EscalatedToBrain`. Brain receives the matching
    /// `BrainContinuation { source: PlanTaskEscalated }` and resolves via
    /// `submit_plan_mutation`.
    PlanTaskEscalated {
        plan_id: String,
        task_id: String,
        delegation_id: String,
        attempt: u32,
        max_attempts: u32,
        last_error: String,
        #[serde(default)]
        worker_branch: Option<String>,
    },

    /// bd-88r — predispatch overlay preview predicted a setup conflict.
    /// Brain receives compiled git topology via `BrainContinuation` and
    /// resolves via `plan_truncate_and_restart` or `submit_plan_mutation`.
    PlanTaskBlockedOnSetupConflict {
        plan_id: String,
        task_id: String,
        delegation_id: String,
        dep_task_id: String,
        files: Vec<String>,
        #[serde(default)]
        topology: Option<crate::domain::continuation::SetupConflictTopology>,
    },

    /// bd-2m2u Phase 2d — `submit_plan_mutation` applied a `MutationBatch`
    /// successfully. Surfaces op tags and affected task ids so observers can
    /// follow the recovery action without parsing the audit log directly.
    PlanMutationApplied {
        plan_id: String,
        mutation_id: String,
        trigger_task_id: String,
        op_tags: Vec<String>,
        affected_task_ids: Vec<String>,
    },

    // ── Plan lifecycle events (INV-7) ─────────────────────────────────────────
    /// Emitted once when a submitted plan reaches a terminal state (no tasks
    /// left to dispatch). Counts reflect the final status of all tasks.
    /// Brain awaits this instead of polling get_plan_status.
    PlanCompleted {
        plan_id: String,
        approved: u32,
        rejected: u32,
        failed: u32,
        #[serde(default)]
        cancelled: u32,
    },
    /// Emitted when all tasks in a plan are Approved. Distinct from
    /// PlanCompleted (which fires on any terminal state). Brain treats this
    /// as the merge-authorization signal.
    PlanReadyToMerge {
        plan_id: String,
    },

    /// Startup sweep observed a stale `spur:plan-pending` epic.
    PlanPendingSweep {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        plan_id: Option<String>,
        epic_id: String,
        action: String,
        child_count: u32,
        age_secs: i64,
        reason: String,
    },

    /// Reconciler reclaimed an in-flight dispatch whose lease label expired.
    DispatchLeaseExpired {
        plan_id: String,
        task_id: String,
        issue_id: String,
        delegation_id: String,
        expired_at: i64,
        age_secs: i64,
    },

    /// A continuation reached a terminal non-delivered state.
    ContinuationDropped {
        delegation_id: DelegationId,
        attempt: u32,
        brain_session: SessionId,
        reason: DropReason,
    },

    /// A continuation was requeued for a later delivery attempt.
    ContinuationDeferred {
        delegation_id: DelegationId,
        attempt: u32,
        brain_session: SessionId,
        requeue_count: u32,
        reason: DeferReason,
    },

    /// A producer field was clipped before constructing a continuation body.
    ContinuationFieldTruncated {
        delegation_id: DelegationId,
        field: Cow<'static, str>,
        original_bytes: usize,
        kept_bytes: usize,
    },

    /// Graceful shutdown timed out and the MCP server had to be force-aborted.
    McpShutdownTimeout {
        session: SessionId,
        timeout_ms: u64,
    },

    /// Emitted immediately before the orchestrator calls
    /// `connection.prompt(...)` for a brain turn. Pairs with
    /// `DelegationCompleted` to make INV-C3 (UI-visible event precedes
    /// model-visible continuation) directly verifiable via `seq` ordering.
    ///
    /// `turn_kind` is one of `"user_only" | "merged" | "continuation_only"`;
    /// `continuations_count` is the number of `BrainContinuation`s
    /// materialized as self-describing `spur://continuation/{id}` blocks
    /// for this turn (0 for `user_only`).
    PromptDispatched {
        session: SessionId,
        turn_kind: String,
        continuations_count: usize,
    },

    /// Emitted by the orchestrator when a brain session is retired via
    /// `retire_active_brain` (e.g. `/clear`, `ResumeSession`, shutdown).
    ///
    /// The lineage projection folds this event by cascading the named
    /// brain and all non-terminal descendants to
    /// [`LifecycleState::Cancelled`], stamping
    /// `ended_at = event.occurred_at` on each current attempt and
    /// draining the cascaded ids from the pending-review queue.
    ///
    /// The orchestrator emits this **before** aborting the brain's
    /// background tasks so trailing notifications landing afterward
    /// project against the already-closed state deterministically.
    BrainRetired {
        session: SessionId,
        reason: BrainRetireReason,
    },

    /// Emitted by the startup orphan sweeper (T6/T7) for each stale
    /// agent process tree it killed. Surfaces orphan-reaping in the TUI
    /// activity log so users see that cleanup happened on launch.
    ///
    /// `agent_name` mirrors the `PgidRecord.agent_name`; `pgid` is the
    /// reaped process-group leader; `age_secs` is how long the record
    /// had been on disk (now − `spawned_at`).
    OrphanReaped {
        agent_name: String,
        pgid: i32,
        age_secs: i64,
    },
}

impl PartialEq for SpurEventBody {
    fn eq(&self, other: &Self) -> bool {
        match (serde_json::to_value(self), serde_json::to_value(other)) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    }
}

/// A single entry in a replayed conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub role: String,
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_mcp_delegation_summary_round_trip() {
        let mut calls_by_tool = BTreeMap::new();
        calls_by_tool.insert("get_issue".to_string(), 5u64);
        calls_by_tool.insert("update_issue".to_string(), 2u64);
        let event = SpurEventBody::WorkerMcpDelegationSummary {
            delegation_id: "abc-123".into(),
            brain_session_id: "session-99".into(),
            calls_total: 7,
            calls_by_tool,
            p99_latency_ms: 1234,
            errors: 1,
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: SpurEventBody = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, event);
    }
}

#[cfg(test)]
mod reconnect_event_tests {
    use super::*;
    use crate::SessionId;

    #[test]
    fn brain_connect_events_construct() {
        let _ = SpurEventBody::BrainConnectStarted {
            brain: "kiro".into(),
        };
        let _ = SpurEventBody::BrainConnected {
            brain: "kiro".into(),
        };
        let _ = SpurEventBody::BrainConnectFailed {
            brain: "kiro".into(),
            reason: "initialize failed".into(),
        };
    }

    #[test]
    fn load_outcome_variants_construct() {
        let _ = LoadOutcome::Restored;
        let _ = LoadOutcome::FellBackToNew {
            reason: "session/load returned error".into(),
        };
    }

    #[test]
    fn brain_reconnect_events_construct() {
        let s = SessionId::new();
        let _ = SpurEventBody::BrainReconnecting {
            session: s.clone(),
            brain_name: "kiro".into(),
            reason: "ACP thread died during prompt".into(),
        };
        let _ = SpurEventBody::BrainReconnected {
            session: s.clone(),
            brain_name: "kiro".into(),
            outcome: LoadOutcome::Restored,
        };
        let _ = SpurEventBody::BrainReconnectFailed {
            session: s,
            brain_name: "kiro".into(),
            reason: "circuit breaker tripped".into(),
        };
    }
}

#[cfg(test)]
mod cancel_mode_field_tests {
    use super::{SpurEvent, SpurEventBody};
    use crate::{CancelMode, SessionId};

    #[test]
    fn agent_session_ready_carries_cancel_mode() {
        let ev = SpurEvent::now(SpurEventBody::AgentSessionReady {
            session: SessionId("s".to_string()),
            acp_session_id: "acp-1".to_string(),
            brain: "kiro".to_string(),
            resumed: false,
            cancel_mode: CancelMode::AcpSoft,
            fs_unsafe: false,
            caps: None,
        });
        match ev.body {
            SpurEventBody::AgentSessionReady { cancel_mode, .. } => {
                assert_eq!(cancel_mode, CancelMode::AcpSoft);
            }
            _ => panic!("wrong variant"),
        }
    }
}

#[cfg(test)]
mod session_attach_event_tests {
    use super::SpurEventBody;
    use crate::session_lock::HolderInfo;

    #[test]
    fn session_attach_rejected_round_trips() {
        let body = SpurEventBody::SessionAttachRejected {
            acp_session_id: "acp-1".to_string(),
            holder: HolderInfo {
                pid: Some(42),
                ..Default::default()
            },
            fs_unsafe: false,
        };

        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        match back {
            SpurEventBody::SessionAttachRejected {
                acp_session_id,
                holder,
                fs_unsafe,
            } => {
                assert_eq!(acp_session_id, "acp-1");
                assert_eq!(holder.pid, Some(42));
                assert!(!fs_unsafe);
            }
            _ => panic!("wrong variant"),
        }
    }
}

#[cfg(test)]
mod review_payload_tests {
    use super::*;
    use crate::domain::DelegationPlan;

    #[test]
    fn review_payload_default_has_none_plan() {
        let p = ReviewPayload {
            summary: "s".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: None,
        };
        assert!(p.delegation_plan.is_none());
        assert!(p.chosen_matches_dispatched.is_none());
    }

    #[test]
    fn review_payload_round_trips_with_plan() {
        let plan = DelegationPlan {
            chosen: Some("kiro".into()),
            rationale: Some("because".into()),
            ..Default::default()
        };
        let p = ReviewPayload {
            summary: "".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: Some(plan),
            chosen_matches_dispatched: Some(true),
            peer_influence: None,
        };
        let json = serde_json::to_string(&p).unwrap();
        let back: ReviewPayload = serde_json::from_str(&json).unwrap();
        assert!(back.delegation_plan.is_some());
        assert_eq!(back.chosen_matches_dispatched, Some(true));
    }

    #[test]
    fn peer_influence_round_trips_through_serde() {
        let p = ReviewPayload {
            summary: "done".into(),
            diff_summary: None,
            pr_url: None,
            error: None,
            delegation_plan: None,
            chosen_matches_dispatched: None,
            peer_influence: Some(PeerInfluenceSummary {
                inbound_consumed: 2,
                inbound_ignored: 1,
                outbound_emitted: 3,
                undelivered: 4,
                from_unreviewed_source: true,
            }),
        };

        let json = serde_json::to_string(&p).unwrap();
        let back: ReviewPayload = serde_json::from_str(&json).unwrap();

        assert_eq!(
            back.peer_influence,
            Some(PeerInfluenceSummary {
                inbound_consumed: 2,
                inbound_ignored: 1,
                outbound_emitted: 3,
                undelivered: 4,
                from_unreviewed_source: true,
            })
        );
    }

    #[test]
    fn review_payload_default_has_no_peer_influence() {
        let json = r#"{"summary":"done","diff_summary":null,"pr_url":null,"error":null}"#;
        let p: ReviewPayload = serde_json::from_str(json).unwrap();

        assert!(p.peer_influence.is_none());
    }
}

#[cfg(test)]
mod delegation_requested_tests {
    use super::*;
    use crate::domain::DelegationPlan;
    use crate::SessionId;

    #[test]
    fn delegation_requested_event_carries_optional_plan() {
        let plan = DelegationPlan {
            chosen: Some("claude".into()),
            ..Default::default()
        };
        let body = SpurEventBody::DelegationRequested {
            from: SessionId::new(),
            to_agent: "claude".into(),
            task: "do things".into(),
            request_id: "req-1".into(),
            delegation_plan: Some(plan.clone()),
            issue_id: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains("\"delegation_plan\""));
    }

    #[test]
    fn delegation_requested_event_roundtrips_without_plan() {
        let body = SpurEventBody::DelegationRequested {
            from: SessionId::new(),
            to_agent: "codex".into(),
            task: "tiny fix".into(),
            request_id: "req-2".into(),
            delegation_plan: None,
            issue_id: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        match back {
            SpurEventBody::DelegationRequested {
                delegation_plan, ..
            } => {
                assert!(delegation_plan.is_none());
            }
            _ => panic!("wrong variant"),
        }
    }
}

#[cfg(test)]
mod dispatch_overlay_applied_tests {
    use super::*;

    #[test]
    fn dispatch_overlay_applied_event_roundtrips_opaque_base_spec() {
        let base_spec = serde_json::json!({
            "kind": "with_overlay",
            "base": { "kind": "branch", "name": "spur/plan-base" },
            "overlays": [
                {
                    "source_task_id": "T1",
                    "base_oid": "abc123",
                    "tip_oid": "def456"
                }
            ]
        });
        let body = SpurEventBody::DispatchOverlayApplied {
            request_id: "req-1".into(),
            base_spec: base_spec.clone(),
            dispatched_base_oid: "overlay-head".into(),
            overlay_task_ids: vec!["T1".into()],
        };

        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();

        match back {
            SpurEventBody::DispatchOverlayApplied {
                request_id,
                base_spec: parsed_base_spec,
                dispatched_base_oid,
                overlay_task_ids,
            } => {
                assert_eq!(request_id, "req-1");
                assert_eq!(parsed_base_spec, base_spec);
                assert_eq!(dispatched_base_oid, "overlay-head");
                assert_eq!(overlay_task_ids, vec!["T1"]);
            }
            _ => panic!("wrong variant"),
        }
    }
}

#[cfg(test)]
mod worker_peer_event_tests {
    use super::*;

    #[test]
    fn worker_peer_message_accepted_roundtrips() {
        use crate::domain::peer_message::{MessageKind, PeerMessageId};
        use uuid::Uuid;

        let body = SpurEventBody::WorkerPeerMessageAccepted {
            brain_session_id: "bs-1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            source_delegation_id: crate::domain::delegation::DelegationId("src".into()),
            target_delegation_id: crate::domain::delegation::DelegationId("tgt".into()),
            kind: MessageKind::Question,
            sequence: 1,
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            SpurEventBody::WorkerPeerMessageAccepted { .. }
        ));
    }

    #[test]
    fn worker_peer_message_malformed_roundtrips() {
        let body = SpurEventBody::WorkerPeerMessageMalformed {
            brain_session_id: "bs-1".into(),
            source_executor_id: "exec-1".into(),
            method: "_spur/peer_message_consumed".into(),
            reason: "malformed_message_id: invalid UUID".into(),
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        if let SpurEventBody::WorkerPeerMessageMalformed {
            brain_session_id,
            source_executor_id,
            method,
            reason,
        } = back
        {
            assert_eq!(brain_session_id, "bs-1");
            assert_eq!(source_executor_id, "exec-1");
            assert_eq!(method, "_spur/peer_message_consumed");
            assert_eq!(reason, "malformed_message_id: invalid UUID");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn worker_peer_message_delivered_carries_injected_chars() {
        use crate::domain::peer_message::PeerMessageId;
        use uuid::Uuid;

        let body = SpurEventBody::WorkerPeerMessageDelivered {
            brain_session_id: "bs-1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            target_delegation_id: crate::domain::delegation::DelegationId("tgt".into()),
            target_prompt_id: "prompt-uuid".into(),
            injected_chars: 1234,
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        if let SpurEventBody::WorkerPeerMessageDelivered { injected_chars, .. } = back {
            assert_eq!(injected_chars, 1234);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn worker_peer_mailbox_reconciled_carries_counts() {
        let body = SpurEventBody::WorkerPeerMailboxReconciled {
            brain_session_id: "bs-1".into(),
            audit_failed_emitted: 2,
            inflight_forced_to_delivered: 1,
            inflight_already_delivered: 5,
            inflight_stranded: 4,
            inflight_reverted_to_queued: 0,
            guards_re_wrapped: 3,
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            SpurEventBody::WorkerPeerMailboxReconciled { .. }
        ));
    }

    #[test]
    fn worker_peer_mailbox_reconciled_deserializes_with_missing_new_fields() {
        let json = r#"{
            "WorkerPeerMailboxReconciled": {
                "brain_session_id": "bs-1",
                "inflight_forced_to_delivered": 2,
                "inflight_stranded": 0,
                "inflight_reverted_to_queued": 0,
                "guards_re_wrapped": 1
            }
        }"#;
        let body: SpurEventBody = serde_json::from_str(json).unwrap();
        if let SpurEventBody::WorkerPeerMailboxReconciled {
            audit_failed_emitted,
            inflight_already_delivered,
            ..
        } = body
        {
            assert_eq!(audit_failed_emitted, 0);
            assert_eq!(inflight_already_delivered, 0);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn worker_peer_message_reconciled_stranded_round_trips() {
        use crate::domain::peer_message::{LedgerState, PeerMessageId};
        use uuid::Uuid;

        let body = SpurEventBody::WorkerPeerMessageReconciledStranded {
            brain_session_id: "bs-1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            target_delegation_id: crate::domain::delegation::DelegationId("tgt".into()),
            state: LedgerState::DeliveredInflight,
            reason: "delivered_inflight_without_injection_records".into(),
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            SpurEventBody::WorkerPeerMessageReconciledStranded {
                state: LedgerState::DeliveredInflight,
                ..
            }
        ));
    }

    #[test]
    fn worker_peer_message_drain_capped_out_round_trips() {
        let body = SpurEventBody::WorkerPeerMessageDrainCappedOut {
            brain_session_id: "bs-1".into(),
            target_delegation_id: crate::domain::delegation::DelegationId("tgt".into()),
            acks_received: 5,
            remaining_messages: 2,
            cap_ms: 5_000,
            actual_elapsed_ms: 5_001,
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        if let SpurEventBody::WorkerPeerMessageDrainCappedOut {
            brain_session_id,
            target_delegation_id,
            acks_received,
            remaining_messages,
            cap_ms,
            actual_elapsed_ms,
        } = back
        {
            assert_eq!(brain_session_id, "bs-1");
            assert_eq!(target_delegation_id.0, "tgt");
            assert_eq!(acks_received, 5);
            assert_eq!(remaining_messages, 2);
            assert_eq!(cap_ms, 5_000);
            assert_eq!(actual_elapsed_ms, 5_001);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn worker_peer_message_drain_started_round_trips() {
        let body = SpurEventBody::WorkerPeerMessageDrainStarted {
            brain_session_id: "bs-1".into(),
            target_delegation_id: crate::domain::delegation::DelegationId("tgt".into()),
            candidates_at_start: 3,
            cap_ms: 5_000,
            quiet_window_ms: 250,
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        if let SpurEventBody::WorkerPeerMessageDrainStarted {
            brain_session_id,
            target_delegation_id,
            candidates_at_start,
            cap_ms,
            quiet_window_ms,
        } = back
        {
            assert_eq!(brain_session_id, "bs-1");
            assert_eq!(target_delegation_id.0, "tgt");
            assert_eq!(candidates_at_start, 3);
            assert_eq!(cap_ms, 5_000);
            assert_eq!(quiet_window_ms, 250);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn worker_peer_message_drain_timed_out_round_trips() {
        let body = SpurEventBody::WorkerPeerMessageDrainTimedOut {
            brain_session_id: "bs-1".into(),
            target_delegation_id: crate::domain::delegation::DelegationId("tgt".into()),
            acks_received: 2,
            remaining_messages: 1,
            cap_ms: 5_000,
            quiet_window_ms: 250,
            actual_elapsed_ms: 251,
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        if let SpurEventBody::WorkerPeerMessageDrainTimedOut {
            brain_session_id,
            target_delegation_id,
            acks_received,
            remaining_messages,
            cap_ms,
            quiet_window_ms,
            actual_elapsed_ms,
        } = back
        {
            assert_eq!(brain_session_id, "bs-1");
            assert_eq!(target_delegation_id.0, "tgt");
            assert_eq!(acks_received, 2);
            assert_eq!(remaining_messages, 1);
            assert_eq!(cap_ms, 5_000);
            assert_eq!(quiet_window_ms, 250);
            assert_eq!(actual_elapsed_ms, 251);
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn worker_peer_message_rejected_carries_target_delegation_id() {
        use crate::domain::peer_message::PeerMessageId;
        use uuid::Uuid;

        let body = SpurEventBody::WorkerPeerMessageRejected {
            brain_session_id: "bs-1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            source_delegation_id: crate::domain::delegation::DelegationId("src".into()),
            target_delegation_id: crate::domain::delegation::DelegationId("tgt".into()),
            reason: "not_in_dag".into(),
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        if let SpurEventBody::WorkerPeerMessageRejected {
            target_delegation_id,
            ..
        } = back
        {
            assert_eq!(target_delegation_id.0, "tgt");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn worker_peer_message_dropped_carries_target_delegation_id() {
        use crate::domain::peer_message::PeerMessageId;
        use uuid::Uuid;

        let body = SpurEventBody::WorkerPeerMessageDropped {
            brain_session_id: "bs-1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            target_delegation_id: crate::domain::delegation::DelegationId("tgt".into()),
            reason: "plan_version_changed".into(),
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        if let SpurEventBody::WorkerPeerMessageDropped {
            target_delegation_id,
            ..
        } = back
        {
            assert_eq!(target_delegation_id.0, "tgt");
        } else {
            panic!("wrong variant");
        }
    }

    #[test]
    fn worker_peer_message_audit_failed_carries_target_delegation_id() {
        use crate::domain::peer_message::PeerMessageId;
        use uuid::Uuid;

        let body = SpurEventBody::WorkerPeerMessageAuditFailed {
            brain_session_id: "bs-1".into(),
            message_id: PeerMessageId(Uuid::new_v4()),
            target_delegation_id: crate::domain::delegation::DelegationId("tgt".into()),
            transition_kind: "delivered".into(),
            error: "beads write failed".into(),
        };
        let json = serde_json::to_string(&body).unwrap();
        let back: SpurEventBody = serde_json::from_str(&json).unwrap();
        if let SpurEventBody::WorkerPeerMessageAuditFailed {
            target_delegation_id,
            ..
        } = back
        {
            assert_eq!(target_delegation_id.0, "tgt");
        } else {
            panic!("wrong variant");
        }
    }
}

#[cfg(test)]
mod continuation_event_tests {
    use super::*;
    use crate::domain::continuation::{DeferReason, DropReason};
    use crate::domain::DelegationId;
    use crate::SessionId;

    fn delegation_id() -> DelegationId {
        DelegationId("del-1".into())
    }

    fn session_id() -> SessionId {
        SessionId("brain-session-1".into())
    }

    #[test]
    fn continuation_dropped_event_round_trips() {
        let body = SpurEventBody::ContinuationDropped {
            delegation_id: delegation_id(),
            attempt: 2,
            brain_session: session_id(),
            reason: DropReason::MismatchedCommitKeys,
        };

        let json = serde_json::to_string(&body).unwrap();
        let leaked: &'static str = Box::leak(json.into_boxed_str());
        let back: SpurEventBody = serde_json::from_str(leaked).unwrap();

        match back {
            SpurEventBody::ContinuationDropped {
                delegation_id,
                attempt,
                brain_session,
                reason,
            } => {
                assert_eq!(delegation_id, DelegationId("del-1".into()));
                assert_eq!(attempt, 2);
                assert_eq!(brain_session, SessionId("brain-session-1".into()));
                assert!(matches!(reason, DropReason::MismatchedCommitKeys));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn continuation_deferred_event_round_trips() {
        let body = SpurEventBody::ContinuationDeferred {
            delegation_id: delegation_id(),
            attempt: 3,
            brain_session: session_id(),
            requeue_count: 4,
            reason: DeferReason::BudgetSpill {
                budget_bytes: 4096,
                continuation_bytes: 1024,
            },
        };

        let json = serde_json::to_string(&body).unwrap();
        let leaked: &'static str = Box::leak(json.into_boxed_str());
        let back: SpurEventBody = serde_json::from_str(leaked).unwrap();

        match back {
            SpurEventBody::ContinuationDeferred {
                delegation_id,
                attempt,
                brain_session,
                requeue_count,
                reason,
            } => {
                assert_eq!(delegation_id, DelegationId("del-1".into()));
                assert_eq!(attempt, 3);
                assert_eq!(brain_session, SessionId("brain-session-1".into()));
                assert_eq!(requeue_count, 4);
                assert!(matches!(
                    reason,
                    DeferReason::BudgetSpill {
                        budget_bytes: 4096,
                        continuation_bytes: 1024
                    }
                ));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn continuation_field_truncated_event_round_trips() {
        let body = SpurEventBody::ContinuationFieldTruncated {
            delegation_id: delegation_id(),
            field: "summary".into(),
            original_bytes: 16_384,
            kept_bytes: 8_192,
        };

        let json = serde_json::to_string(&body).unwrap();
        let leaked: &'static str = Box::leak(json.into_boxed_str());
        let back: SpurEventBody = serde_json::from_str(leaked).unwrap();

        match back {
            SpurEventBody::ContinuationFieldTruncated {
                delegation_id,
                field,
                original_bytes,
                kept_bytes,
            } => {
                assert_eq!(delegation_id, DelegationId("del-1".into()));
                assert_eq!(field, "summary");
                assert_eq!(original_bytes, 16_384);
                assert_eq!(kept_bytes, 8_192);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn mcp_shutdown_timeout_event_round_trips() {
        let body = SpurEventBody::McpShutdownTimeout {
            session: session_id(),
            timeout_ms: 5_000,
        };

        let json = serde_json::to_string(&body).unwrap();
        let leaked: &'static str = Box::leak(json.into_boxed_str());
        let back: SpurEventBody = serde_json::from_str(leaked).unwrap();

        match back {
            SpurEventBody::McpShutdownTimeout {
                session,
                timeout_ms,
            } => {
                assert_eq!(session, SessionId("brain-session-1".into()));
                assert_eq!(timeout_ms, 5_000);
            }
            _ => panic!("wrong variant"),
        }
    }
}

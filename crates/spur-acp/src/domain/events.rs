use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

use agent_client_protocol::SessionNotification;
use crate::types::SessionId;
use crate::domain::delegation::DelegationStatus;

/// Events emitted by the orchestrator for TUI/cost-tracker consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SpurEvent {
    BrainSpawned { agent: String, session: SessionId },
    WorkerSpawned { agent: String, session: SessionId, worktree: PathBuf },
    SessionCompleted { session: SessionId, success: bool },
    AgentNotification { session: SessionId, notification: SessionNotification },
    DelegationRequested { from: SessionId, to_agent: String, task: String },
    DelegationCompleted { worker_session: SessionId, status: DelegationStatus },
    ConflictDetected { files: Vec<PathBuf> },
    RateLimitDetected { agent: String, retry_after: Option<Duration> },
    BrainFailover { from: String, to: String },
    CostUpdate { session: SessionId, agent: String, estimated_cost_usd: f64 },
    IssueReceived { source: String, id: String },
    PrCreated { url: String },
    IssueUpdated { source: String, id: String, status: String },
    // ── Interactive loop events ──────────────────────────────────────
    TurnComplete { session: SessionId },
    BrainError { session: SessionId, message: String },
}

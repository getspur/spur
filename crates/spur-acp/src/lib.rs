pub mod config;
pub mod connection;
pub mod domain;
pub mod protocol;
pub mod registry;
pub mod types;

pub use config::AgentConfig;
pub use connection::{AgentConnection, CliWrapAdapter, NativeAcpConnection, StdioAdapter, StreamJsonAdapter};
pub use registry::AgentRegistry;

// Re-export domain types
pub use domain::{DelegationResult, DelegationStatus, HistoryEntry, SpurEvent, SpurEventBody};
pub use crate::domain::events::{
    ExecutorArtifactPayload, ExecutorDiffSummary, ExecutorReviewDecision, ExecutorReviewKind,
    ExecutorReviewPayload,
};

// Re-export all remaining types for backward compatibility
pub use types::*;

// Re-export ACP SDK types for consumer crates (TUI, orchestrator).
pub use agent_client_protocol::{
    ContentBlock, ContentChunk, TextContent,
    SessionNotification, SessionUpdate,
    ToolCall as AcpToolCall, ToolCallUpdate as AcpToolCallUpdate,
    ToolCallStatus, ToolKind, ToolCallContent, ToolCallLocation,
    Plan, PlanEntry, PlanEntryStatus, PlanEntryPriority,
    RequestPermissionRequest, PermissionOption, PermissionOptionId,
    PermissionOptionKind, RequestPermissionOutcome, SelectedPermissionOutcome,
    SessionInfo, ListSessionsRequest, ListSessionsResponse, LoadSessionRequest,
    AuthenticateRequest, AuthenticateResponse, AuthMethodId,
    AvailableCommandsUpdate, CurrentModeUpdate, SessionModeId,
    SetSessionModeRequest, SetSessionModeResponse, UsageUpdate,
};

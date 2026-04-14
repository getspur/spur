pub mod config;
pub mod connection;
pub mod domain;
pub mod ext;
pub mod protocol;
pub mod registry;
pub mod types;

pub use config::{
    AgentConfig, AgentReviewPolicy, AgentsConfig, ArgsTemplateKind, CommandsConfig, ConfigError,
    DispatchKind, DisplayConfig, IngestBinding, IngestParserKind, ItemSchemaKind, PermissionsConfig,
    ResponseBinding, ResponseRenderKind, SpurConfig, validate_agent_config,
};
pub use connection::{AgentConnection, CliWrapAdapter, ExtNotificationPayload, NativeAcpConnection, StdioAdapter, StreamJsonAdapter};
pub use registry::AgentRegistry;

// Re-export domain types
pub use domain::{DelegationResult, DelegationStatus, HistoryEntry, SpurEvent, SpurEventBody, TimeoutFallback};
pub use crate::domain::events::{
    Artifact, DiffSummary, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role,
};

// Re-export all remaining types for backward compatibility
pub use types::*;

// Re-export ACP SDK types for consumer crates (TUI, orchestrator).
pub use agent_client_protocol::{
    ContentBlock, ContentChunk, TextContent, ResourceLink,
    SessionNotification, SessionUpdate,
    ToolCall as AcpToolCall, ToolCallUpdate as AcpToolCallUpdate,
    ToolCallStatus, ToolKind, ToolCallContent, ToolCallLocation,
    Plan, PlanEntry, PlanEntryStatus, PlanEntryPriority,
    RequestPermissionRequest, PermissionOption, PermissionOptionId,
    PermissionOptionKind, RequestPermissionOutcome, SelectedPermissionOutcome,
    SessionInfo, ListSessionsRequest, ListSessionsResponse, LoadSessionRequest,
    AuthenticateRequest, AuthenticateResponse, AuthMethodId,
    AvailableCommandsUpdate, AvailableCommand, AvailableCommandInput,
    UnstructuredCommandInput,
    CurrentModeUpdate, SessionModeId,
    SetSessionModeRequest, SetSessionModeResponse, UsageUpdate,
    ExtRequest, ExtResponse, ExtNotification,
};

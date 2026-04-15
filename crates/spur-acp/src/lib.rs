pub mod adapter;
pub mod config;
pub mod connection;
pub mod domain;
pub mod ext;
pub mod protocol;
pub mod registry;
pub mod types;

pub use config::{
    load_seed_template, validate_agent_config, AgentConfig, AgentReviewPolicy, AgentsConfig,
    ArgsTemplateKind, CommandsConfig, ConfigError, DispatchKind, DisplayConfig, IngestBinding,
    IngestParserKind, ItemSchemaKind, PermissionsConfig, ResponseBinding, ResponseRenderKind,
    SpurConfig, StaticCommandDecl,
};
pub use connection::{
    AgentConnection, CliWrapAdapter, ExtNotificationPayload, NativeAcpConnection, StdioAdapter,
    StreamJsonAdapter,
};
pub use registry::AgentRegistry;

// Re-export domain types
pub use crate::domain::events::{
    Artifact, DiffSummary, FileTouchKind, LifecycleState, LoadOutcome, ReviewDecision, ReviewKind,
    ReviewPayload, Role,
};
pub use domain::{
    DelegationResult, DelegationStatus, HistoryEntry, SpurEvent, SpurEventBody, TimeoutFallback,
};

// Re-export all remaining types for backward compatibility
pub use types::*;

// Re-export ACP SDK types for consumer crates (TUI, orchestrator).
pub use agent_client_protocol::{
    AuthMethodId, AuthenticateRequest, AuthenticateResponse, AvailableCommand,
    AvailableCommandInput, AvailableCommandsUpdate, ContentBlock, ContentChunk, CurrentModeUpdate,
    ExtNotification, ExtRequest, ExtResponse, ListSessionsRequest, ListSessionsResponse,
    LoadSessionRequest, PermissionOption, PermissionOptionId, PermissionOptionKind, Plan,
    PlanEntry, PlanEntryPriority, PlanEntryStatus, RequestPermissionOutcome,
    RequestPermissionRequest, ResourceLink, SelectedPermissionOutcome, SessionInfo, SessionModeId,
    SessionNotification, SessionUpdate, SetSessionModeRequest, SetSessionModeResponse, TextContent,
    ToolCall as AcpToolCall, ToolCallContent, ToolCallLocation, ToolCallStatus,
    ToolCallUpdate as AcpToolCallUpdate, ToolKind, UnstructuredCommandInput, UsageUpdate,
};

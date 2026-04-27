pub mod adapter;
pub mod agents;
pub mod config;
pub mod connection;
pub mod domain;
pub mod ext;
pub mod protocol;
pub mod registry;
pub mod session_liveness;
pub mod session_lock;
pub mod types;

pub use config::{
    load_seed_template, validate_agent_config, AgentConfig, AgentReviewPolicy, AgentsConfig,
    ArgsTemplateKind, BeadsPmConfig, CommandsConfig, ConfigError, DispatchKind, DisplayConfig,
    IngestBinding, IngestParserKind, ItemSchemaKind, PermissionsConfig, ResponseBinding,
    ResponseRenderKind, SpurConfig, StaticCommandDecl,
};
pub use connection::{
    AgentConnection, CliWrapAdapter, ExtNotificationPayload, NativeAcpConnection, StdioAdapter,
    StreamJsonAdapter,
};
pub use registry::AgentRegistry;
pub use session_liveness::SelfHeldSet;

// Re-export domain types
pub use crate::domain::events::{
    Artifact, DiffSummary, FileTouchKind, LifecycleState, LoadOutcome, PeerInfluenceSummary,
    PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask, ReviewDecision, ReviewKind, ReviewPayload,
    Role,
};
pub use domain::{
    ArtifactKind, CancelOutcome, CancellationControl, DelegationAbortHandle, DelegationAbortReason,
    DelegationId, DelegationPlan, DelegationResult, DelegationStatus, HistoryEntry,
    IssueDetailEvent, IssueSummaryEvent, LicenseBindingMode, LicensePlan, LicenseStateEvent,
    LicenseStatusEvent, LicenseSubjectKind, PlanCandidate, PlanSubtask, SpurEvent, SpurEventBody,
    TimeoutFallback, WorkerArtifact,
};

// Re-export all remaining types for backward compatibility
pub use types::*;

// Re-export ACP SDK types for consumer crates (TUI, orchestrator).
pub use adapter::config_options::{extract_choices, AdvertisedChoice, AdvertisedCommand};
pub use adapter::{extract_tool_meta, SpurToolMeta};
pub use agent_client_protocol::schema::{
    AuthMethodId, AuthenticateRequest, AuthenticateResponse, AvailableCommand,
    AvailableCommandInput, AvailableCommandsUpdate, ConfigOptionUpdate, ContentBlock, ContentChunk,
    CurrentModeUpdate, ExtNotification, ExtRequest, ExtResponse, ListSessionsRequest,
    ListSessionsResponse, LoadSessionRequest, PermissionOption, PermissionOptionId,
    PermissionOptionKind, Plan, PlanEntry, PlanEntryPriority, PlanEntryStatus,
    RequestPermissionOutcome, RequestPermissionRequest, ResourceLink, SelectedPermissionOutcome,
    SessionConfigId, SessionConfigOption, SessionConfigSelectOption, SessionInfo, SessionModeId,
    SessionNotification, SessionUpdate, SetSessionModeRequest, SetSessionModeResponse, TextContent,
    ToolCall as AcpToolCall, ToolCallContent, ToolCallId, ToolCallLocation, ToolCallStatus,
    ToolCallUpdate as AcpToolCallUpdate, ToolKind, UnstructuredCommandInput, UsageUpdate,
};

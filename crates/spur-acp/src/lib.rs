#![expect(
    clippy::doc_markdown,
    reason = "legacy ACP docs contain many protocol identifiers that need a dedicated cleanup pass"
)]
#![expect(
    clippy::enum_glob_use,
    reason = "legacy ACP discriminant helper imports SessionUpdate variants locally"
)]
#![expect(
    clippy::clone_on_ref_ptr,
    reason = "legacy ACP connection code uses clone syntax heavily around Arc/Rc channel state"
)]
#![expect(
    clippy::doc_link_with_quotes,
    reason = "legacy ACP config docs include array examples that need a dedicated markdown pass"
)]
#![expect(
    clippy::explicit_iter_loop,
    reason = "legacy ACP defaults code keeps iterator spelling explicit in config matching"
)]
#![expect(
    clippy::iter_over_hash_type,
    reason = "shutdown cleanup iterates a terminal map where order is not behaviorally significant"
)]
#![expect(
    clippy::map_err_ignore,
    reason = "legacy ACP channel error mapping intentionally replaces send/receive details with domain messages"
)]
#![expect(
    clippy::manual_let_else,
    reason = "legacy ACP parsing code still uses match-based early returns"
)]
#![expect(
    clippy::match_same_arms,
    reason = "legacy ACP status clipping keeps status-specific arms explicit"
)]
#![expect(
    clippy::ref_option,
    reason = "serde helper signatures are constrained by derive integration"
)]
#![expect(
    clippy::return_and_then,
    reason = "legacy adapter extraction keeps chained option flow compact"
)]
#![expect(
    clippy::single_match_else,
    reason = "legacy ACP process handling keeps success/error branches visually grouped"
)]
#![expect(
    clippy::str_to_string,
    reason = "legacy ACP adapter code predates the current to_owned style lint"
)]
#![expect(
    clippy::uninlined_format_args,
    reason = "legacy ACP error messages use pre-inline formatting style"
)]
#![expect(
    clippy::unnecessary_wraps,
    reason = "ACP permission callback signatures intentionally match Result-returning protocol hooks"
)]
#![expect(
    clippy::unused_trait_names,
    reason = "legacy ACP modules import extension traits by name for readability"
)]
#![expect(
    clippy::use_self,
    reason = "legacy ACP domain code often spells enum names explicitly for cross-module clarity"
)]

pub mod adapter;
pub mod agent_quirks;
pub mod agents;
pub mod config;
pub mod connection;
pub mod domain;
pub mod error;
pub mod ext;
pub mod orphan_registry;
pub mod orphan_sweeper;
pub mod process_inspector;
pub mod protocol;
pub mod registry;
pub mod session_info_cache;
pub mod session_liveness;
pub mod session_lock;
pub mod spur_agent_caps;
pub mod types;

pub use config::{
    load_seed_template, validate_agent_config, AgentConfig, AgentReviewPolicy, AgentsConfig,
    ArgsTemplateKind, BeadsPmConfig, CommandsConfig, ConfigError, DispatchKind, DisplayConfig,
    EditorMode, IngestBinding, IngestParserKind, ItemSchemaKind, PermissionsConfig,
    ResponseBinding, ResponseRenderKind, SpurConfig, StaticCommandDecl, TuiConfig,
};
pub use connection::{
    AgentConnection, CliWrapAdapter, ExtNotificationPayload, NativeAcpConnection, StdioAdapter,
    StreamJsonAdapter, TestStubConnection,
};
pub use error::AcpError;
pub use registry::AgentRegistry;
pub use session_info_cache::SessionInfoCache;
pub use session_liveness::SelfHeldSet;
pub use spur_agent_caps::SpurAgentCaps;

// Re-export domain types
pub use crate::domain::events::{
    Artifact, Column, DatasourceEntry, DatasourceKind, DiffSummary, FileTouchKind, LifecycleState,
    LoadOutcome, PeerInfluenceSummary, PlanLifecycleEvent, PlanLoadWarningEvent,
    PlanOwnerStateEvent, PlanSnapshot, PlanSnapshotCounts, PlanSnapshotTask,
    PlanSummaryCountsEvent, PlanSummaryEvent, ReviewDecision, ReviewKind, ReviewPayload, Role,
    Table,
};
pub use domain::{
    ArtifactKind, AttemptSetupError, CancelOutcome, CancellationControl, DelegationAbortHandle,
    DelegationAbortReason, DelegationDispatchError, DelegationId, DelegationPlan, DelegationResult,
    DelegationStatus, GraphEdgeEvent, GraphNodeEvent, HistoryEntry, IssueDetailEvent,
    IssueSummaryEvent, LicenseBindingMode, LicensePlan, LicenseStateEvent, LicenseStatusEvent,
    LicenseSubjectKind, PlanCandidate, PlanSubtask, SpurEvent, SpurEventBody, TimeoutFallback,
    WorkerArtifact,
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
    SessionConfigId, SessionConfigOption, SessionConfigSelectOption, SessionInfo,
    SessionInfoUpdate, SessionModeId, SessionNotification, SessionUpdate, SetSessionModeRequest,
    SetSessionModeResponse, TextContent, ToolCall as AcpToolCall, ToolCallContent, ToolCallId,
    ToolCallLocation, ToolCallStatus, ToolCallUpdate as AcpToolCallUpdate, ToolKind,
    UnstructuredCommandInput, UsageUpdate,
};

#![expect(
    clippy::allow_attributes,
    reason = "legacy core modules still use localized allow attributes; tracked as lint debt"
)]
#![expect(
    clippy::doc_markdown,
    reason = "legacy docs contain domain terms that are not consistently backticked yet"
)]
#![expect(
    clippy::clone_on_ref_ptr,
    reason = "legacy core code still uses method-call clone syntax for Arc values"
)]
#![expect(
    clippy::elidable_lifetime_names,
    reason = "legacy core code has explicit lifetimes kept for readability in packed continuation types"
)]
#![expect(
    clippy::explicit_iter_loop,
    reason = "legacy loops sometimes call iter explicitly for readability"
)]
#![expect(
    clippy::format_push_string,
    reason = "legacy prompt builders append formatted strings directly"
)]
#![expect(
    clippy::filter_map_next,
    reason = "relocated plan engine retains legacy filter_map().next() spelling"
)]
#![expect(
    clippy::future_not_send,
    reason = "legacy orchestrator futures capture non-Sync state and are not required to be Send"
)]
#![expect(
    clippy::ignored_unit_patterns,
    reason = "legacy select branches use wildcard unit patterns"
)]
#![expect(
    clippy::implicit_clone,
    reason = "relocated plan code keeps legacy cloning style pending mechanical cleanup"
)]
#![expect(
    clippy::iter_over_hash_type,
    reason = "legacy scheduler diagnostics iterate hash sets in debug-only paths"
)]
#![expect(
    clippy::let_underscore_future,
    reason = "relocated reconciler intentionally detaches tracked tasks"
)]
#![expect(
    clippy::large_stack_frames,
    reason = "legacy interactive loop is intentionally large pending a structural split"
)]
#![expect(
    clippy::manual_let_else,
    reason = "legacy core code still contains match-based early-return control flow"
)]
#![expect(
    clippy::map_err_ignore,
    reason = "relocated MCP handlers intentionally replace low-level errors with JSON-RPC domain messages"
)]
#![expect(
    clippy::match_same_arms,
    reason = "legacy exhaustive event matches intentionally document no-op event classes"
)]
#![expect(
    clippy::missing_fields_in_debug,
    reason = "legacy debug impls intentionally summarize large internal scheduler fields"
)]
#![expect(
    clippy::needless_pass_by_ref_mut,
    reason = "legacy orchestrator APIs keep mutable receiver signatures for trait and call-site compatibility"
)]
#![expect(
    clippy::needless_continue,
    reason = "legacy delegation loops keep explicit continues to document branch termination"
)]
#![expect(
    clippy::option_as_ref_cloned,
    reason = "legacy option cloning style is pending mechanical cleanup"
)]
#![expect(
    clippy::option_option,
    reason = "relocated worker audit plumbing distinguishes unset from explicitly empty targets"
)]
#![expect(
    clippy::or_fun_call,
    reason = "legacy status fallback construction uses unwrap_or"
)]
#![expect(
    clippy::path_buf_push_overwrite,
    reason = "legacy session path normalization handles root components explicitly"
)]
#![expect(
    clippy::ref_patterns,
    reason = "legacy pattern matches still use explicit ref bindings"
)]
#![expect(
    clippy::ref_option,
    reason = "relocated plan helpers retain borrowed option signatures for call-site compatibility"
)]
#![expect(
    clippy::redundant_type_annotations,
    reason = "relocated server test helpers keep serde_json::Value annotations explicit"
)]
#![expect(
    clippy::return_and_then,
    reason = "legacy JSON extraction helpers use and_then chains"
)]
#![expect(
    clippy::semicolon_if_nothing_returned,
    reason = "legacy event sink helpers omit semicolons in unit-returning expressions"
)]
#![expect(
    clippy::single_match_else,
    reason = "legacy core code uses match for destructuring branches with nontrivial else bodies"
)]
#![expect(
    clippy::single_option_map,
    reason = "relocated outcome materializer exposes an Option-map helper for callers"
)]
#![expect(
    clippy::str_to_string,
    reason = "legacy core code has many &str to String conversions pending mechanical cleanup"
)]
#![expect(
    clippy::string_add,
    reason = "legacy prompt assembly uses string concatenation in a few static builders"
)]
#![expect(
    clippy::uninlined_format_args,
    reason = "legacy tracing and debug messages have not all moved to captured format args"
)]
#![expect(
    clippy::unnecessary_wraps,
    reason = "legacy helper APIs preserve Option/Result return shapes used by callers"
)]
#![expect(
    clippy::unnecessary_safety_comment,
    reason = "legacy audit comments use the word safety outside unsafe-code contexts"
)]
#![expect(
    clippy::unnecessary_literal_bound,
    reason = "relocated plan test utilities keep trait-like string method signatures"
)]
#![expect(
    clippy::unnested_or_patterns,
    reason = "legacy event matching keeps separate or-pattern groups for readability"
)]
#![expect(
    clippy::unused_async,
    reason = "legacy async APIs preserve call-site and trait compatibility"
)]
#![expect(
    clippy::unused_self,
    reason = "legacy methods keep receiver shape for API consistency"
)]
#![expect(
    clippy::unused_trait_names,
    reason = "legacy modules import extension traits by name"
)]
#![expect(
    clippy::use_self,
    reason = "legacy core code often spells concrete type names in impl bodies"
)]
#![expect(
    clippy::useless_let_if_seq,
    reason = "relocated plan guard code keeps state mutation explicit"
)]
#![expect(
    clippy::while_immutable_condition,
    reason = "relocated reconciler poll loop is structured around breaks and returns"
)]

pub mod continuation_bridge;
pub use continuation_bridge::{
    new_overflow_buf, report_detached_completion, ContinuationEventSink, OverflowBuf,
};

pub mod delegation_types;
pub use delegation_types::{
    BaseSpec, BaseTarget, DelegationChannel, DelegationRequest, OverlayCommit,
};

pub mod delegation_watchdog;
pub mod explore;
pub mod scheduler;
pub use scheduler::{BrainScheduler, ScheduledAction};

pub mod agent_profiles;
pub mod event_funnel;
pub mod event_replay;
pub mod event_sink;
pub mod handlers;
pub mod license_runtime;
pub mod lineage;
pub mod mcp;
pub mod notebook;
pub(crate) mod notification_drain;
pub mod notification_pump;
pub mod orchestrator;
pub mod outcome_materializer;
pub mod peer_mailbox;
pub mod plan;
pub mod plan_projection;
pub mod project_root;
pub mod retry_loop;
pub mod review_sink;
pub mod server;
pub mod session_synopsis;
pub mod skills;
pub mod skip_perm;
pub mod spur_ext_interp;
mod submit_plan_dedup;
pub(crate) mod telemetry;
pub mod tool_schemas;
#[cfg(test)]
pub(crate) mod tracing_test_lock;
pub mod upgrade;
pub mod worker_server;
pub mod worktree_authority;

pub use lineage::{
    Attempt, AttemptStatus, ExecutorId, ExecutorLineage, ExecutorNode, PeerEdge, PeerEdgeState,
    ReviewRequest, WorkerStreamEntry, WorkerStreamKind,
};
#[cfg(any(test, feature = "test-support"))]
pub use orchestrator::test_support;
pub use orchestrator::{
    review_dispatcher_loop, BrainSession, InteractiveInput, Orchestrator, RunOpts, RunResult,
};
pub use plan_projection::{PlanProjectionStore, TrackedPlan, TrackedTask};
pub use review_sink::{ReviewSink, ReviewSinkError};
pub use server::{
    build_entries_with_task_map, build_epic_subgraph, build_worker_info, emit_plan_submit_audit,
    parse_parallel_tasks, plan_epic_issue_creates, validate_parallel_args, EpicSubgraph,
    McpCallbackServer, PlanSubmitAuditContext, WorkerInfo,
};
pub use session_synopsis::{SessionSynopsis, SessionSynopsisProjection, SynopsisExchange};
pub use spur_acp::{
    Artifact, DiffSummary, LifecycleState, ReviewDecision, ReviewKind, ReviewPayload, Role,
};
pub use upgrade::UpgradeBanner;
pub use worktree_authority::{AuthorityConfig, SweepReport, WorktreeAuthority};

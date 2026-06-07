#![expect(
    clippy::allow_attributes,
    reason = "legacy MCP plan and code-graph modules still contain many local allow markers"
)]
#![expect(
    clippy::doc_markdown,
    reason = "legacy MCP docs contain many tool and field identifiers that need a dedicated markdown pass"
)]
#![expect(
    clippy::clone_on_ref_ptr,
    reason = "legacy MCP server and mutation code uses clone syntax around Arc state"
)]
#![expect(
    clippy::derive_partial_eq_without_eq,
    reason = "legacy tool schema structs derive PartialEq for tests and can be tightened later"
)]
#![expect(
    clippy::elidable_lifetime_names,
    reason = "legacy reconciler trait implementation keeps explicit boxed future lifetimes"
)]
#![expect(
    clippy::explicit_iter_loop,
    reason = "legacy MCP plan handlers keep iterator spelling explicit in mutation loops"
)]
#![expect(
    clippy::filter_map_next,
    reason = "legacy MCP label helpers use filter_map().next() spelling"
)]
#![expect(
    clippy::format_push_string,
    reason = "legacy MCP plan prompt builders append formatted strings"
)]
#![expect(
    clippy::future_not_send,
    reason = "code-graph handlers intentionally use non-Send closures inside request-local async helpers"
)]
#![expect(
    clippy::ignored_unit_patterns,
    reason = "legacy worker select branches use ignored patterns in shutdown paths"
)]
#![expect(
    clippy::implicit_clone,
    reason = "legacy plan reconciler clones dependency vectors through to_vec"
)]
#![expect(
    clippy::iter_over_hash_type,
    reason = "legacy MCP plan scanning iterates hash sets where order is not semantic"
)]
#![expect(
    clippy::let_underscore_future,
    reason = "legacy reconciler intentionally detaches tracked tasks"
)]
#![expect(
    clippy::manual_let_else,
    reason = "legacy MCP handlers and reconciler use match-based early returns"
)]
#![expect(
    clippy::map_err_ignore,
    reason = "legacy MCP handlers intentionally replace low-level errors with JSON-RPC domain messages"
)]
#![expect(
    clippy::match_same_arms,
    reason = "legacy MCP error/status mapping keeps semantically distinct cases explicit"
)]
#![expect(
    clippy::ref_patterns,
    reason = "legacy MCP handlers use pre-2024 ref binding style in many serialization paths"
)]
#![expect(
    clippy::single_match_else,
    reason = "legacy MCP handlers keep success and fallback branches grouped"
)]
#![expect(
    clippy::option_as_ref_cloned,
    reason = "legacy server paths use as_ref().cloned() in optional Arc handoffs"
)]
#![expect(
    clippy::option_option,
    reason = "legacy worker audit plumbing distinguishes unset from explicitly empty targets"
)]
#![expect(
    clippy::needless_borrow,
    reason = "legacy code-graph helper passes borrowed paths through coercions"
)]
#![expect(
    clippy::redundant_type_annotations,
    reason = "legacy test helpers keep serde_json::Value annotations explicit"
)]
#![expect(
    clippy::ref_option,
    reason = "legacy server helper signatures accept borrowed options"
)]
#![expect(
    clippy::return_and_then,
    reason = "legacy plan snapshot and reconciler code keeps chained option flow compact"
)]
#![expect(
    clippy::semicolon_if_nothing_returned,
    reason = "legacy mutation/reconciler code omits semicolons in unit-return branches"
)]
#![expect(
    clippy::single_option_map,
    reason = "legacy outcome materializer exposes an Option-map helper for callers"
)]
#![expect(
    clippy::str_to_string,
    reason = "legacy MCP code predates the current to_owned style lint"
)]
#![expect(
    clippy::unnested_or_patterns,
    reason = "legacy MCP environment and server matching keeps alternatives unnested"
)]
#![expect(
    clippy::unused_async,
    reason = "legacy MCP async handler signatures are kept uniform for routing call sites"
)]
#![expect(
    clippy::unused_self,
    reason = "legacy MCP methods keep receiver signatures for handler grouping"
)]
#![expect(
    clippy::unused_trait_names,
    reason = "legacy MCP modules import extension traits by name for readability"
)]
#![expect(
    clippy::uninlined_format_args,
    reason = "legacy MCP error strings use pre-inline format argument style"
)]
#![expect(
    clippy::unnecessary_literal_bound,
    reason = "legacy MCP test utilities keep trait-like string method signatures"
)]
#![expect(
    clippy::unnecessary_wraps,
    reason = "legacy code-graph expansion helpers keep Result signatures for caller symmetry"
)]
#![expect(
    clippy::use_self,
    reason = "legacy MCP plan/code-graph code often spells type names explicitly for clarity"
)]
#![expect(
    clippy::useless_let_if_seq,
    reason = "legacy plan guard code keeps state mutation explicit"
)]
#![expect(
    clippy::while_immutable_condition,
    reason = "legacy reconciler poll loop is structured around breaks and returns"
)]

pub mod events;
pub mod handlers;
pub mod outcome_materializer;
pub mod plan;
pub mod server;
mod submit_plan_dedup;
pub mod token;
pub mod tool_schemas;
pub mod tools;
pub mod worker_server;

pub use plan::test_support;

pub use events::McpEventSink;
pub use server::{
    build_entries_with_task_map, build_epic_subgraph, build_worker_info, emit_plan_submit_audit,
    parse_parallel_tasks, plan_epic_issue_creates, validate_parallel_args, EpicSubgraph,
    McpCallbackServer, PlanSubmitAuditContext, WorkerInfo,
};
pub use tools::{tools_list, DelegationChannel, DelegationRequest, ToolDefinition};

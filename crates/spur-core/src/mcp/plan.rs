//! Plan / review / reconciler orchestration MCP surface (spur-core owned).
//!
//! Phase 4 of the MCP crate-ownership refactor moves the plan/reconciler engine
//! and its orchestration runtime state out of `spur-mcp` and into `spur-core`.
//! That move is an irreducible, multi-stage relocation (the engine, the plan
//! handlers, the `McpCallbackServer` state fields, the worker read tools, and
//! ~40 integration tests are mutually coupled). See
//! `docs/superpowers/plans/2026-06-21-phase4-plan-reconciler-core-extraction.md`.
//!
//! Stage 4 makes this module the owner of the plan/review/reconciler MCP tool
//! catalog and dispatch. The handlers still bridge to `McpCallbackServer`
//! while the remaining server relocation stages complete.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::outcome_materializer::OutcomeMaterializer;
use crate::plan::outcomes::OutcomeStore;
use crate::plan::{PlanRegistry, PmLike};
use crate::server::{CachedPlan, DetachedContinuationCtx, McpCallbackServer};
use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde_json::{json, Value};
use spur_acp::BrainSessionId;
use spur_blob_store::OutcomeStore as BlobOutcomeStore;
use spur_license::FeatureGate;
use spur_mcp::{McpEventSink, ToolCallContext, ToolDefinition, ToolModule, ToolResponse};
use spur_pm::PmService;
use tokio::sync::OnceCell;

const PLAN_MANAGEMENT_TOOL_NAMES: &[&str] = &["merge_plan", "resume_plan", "force_reclaim_plan"];

const PLAN_REMAINDER_TOOL_NAMES: &[&str] = &[
    "submit_plan",
    "spur_loop_doctor",
    "submit_loop",
    "execute_epic",
    "get_plan_status",
    "get_loop_status",
    "get_reconciler_status",
    "get_task_diff",
    "preview_task_base",
    "plan_truncate_and_restart",
    "recover_orphaned_dispatch",
    "pause_loop",
    "resume_loop",
    "kill_loop",
    "set_loop_autonomy",
    "review_task",
    "submit_plan_mutation",
];

const PLAN_TOOL_NAMES: &[&str] = &[
    "merge_plan",
    "resume_plan",
    "force_reclaim_plan",
    "submit_plan",
    "spur_loop_doctor",
    "submit_loop",
    "execute_epic",
    "get_plan_status",
    "get_loop_status",
    "get_reconciler_status",
    "get_task_diff",
    "preview_task_base",
    "plan_truncate_and_restart",
    "recover_orphaned_dispatch",
    "pause_loop",
    "resume_loop",
    "kill_loop",
    "set_loop_autonomy",
    "review_task",
    "submit_plan_mutation",
];

#[derive(Debug, Clone, Copy)]
enum PlanToolSection {
    All,
    Management,
    Remainder,
}

/// Core-owned MCP module for plan/review/reconciler tools.
#[derive(Clone)]
pub struct PlanMcpModule {
    deps: PlanMcpDeps,
    section: PlanToolSection,
}

impl PlanMcpModule {
    /// Advertise all plan/review/reconciler tools as a single module.
    pub fn new(deps: PlanMcpDeps) -> Self {
        Self {
            deps,
            section: PlanToolSection::All,
        }
    }

    /// Advertise the legacy pre-graph plan-management block.
    pub(crate) fn management(deps: PlanMcpDeps) -> Self {
        Self {
            deps,
            section: PlanToolSection::Management,
        }
    }

    /// Advertise the legacy post-analyst plan/review/reconciler block.
    pub(crate) fn remainder(deps: PlanMcpDeps) -> Self {
        Self {
            deps,
            section: PlanToolSection::Remainder,
        }
    }

    pub(crate) async fn call_with_server(
        &self,
        server: &McpCallbackServer,
        ctx: ToolCallContext<'_>,
        tool_name: &str,
        arguments: Value,
    ) -> spur_mcp::JsonRpcResponse {
        let _ = &self.deps;
        let id = ctx.request_id_value();

        match tool_name {
            "merge_plan" => server.handle_merge_plan(id, arguments).await,
            "resume_plan" => server.handle_resume_plan(id, arguments).await,
            "force_reclaim_plan" => server.handle_force_reclaim_plan(id, arguments).await,
            "submit_plan" => server.handle_submit_plan(id, arguments).await,
            "spur_loop_doctor" => server.handle_spur_loop_doctor(id, arguments).await,
            "submit_loop" => server.handle_submit_loop(id, arguments).await,
            "execute_epic" => server.handle_execute_epic(id, arguments).await,
            "get_plan_status" => server.handle_get_plan_status(id, arguments).await,
            "get_loop_status" => server.handle_get_loop_status(id, arguments).await,
            "get_reconciler_status" => server.handle_get_reconciler_status(id).await,
            "get_task_diff" => server.handle_get_task_diff(id, arguments).await,
            "preview_task_base" => server.handle_preview_task_base(id, arguments).await,
            "plan_truncate_and_restart" => {
                server.handle_plan_truncate_and_restart(id, arguments).await
            }
            "recover_orphaned_dispatch" => {
                server.handle_recover_orphaned_dispatch(id, arguments).await
            }
            "pause_loop" => server.handle_pause_loop(id, arguments).await,
            "resume_loop" => server.handle_resume_loop(id, arguments).await,
            "kill_loop" => server.handle_kill_loop(id, arguments).await,
            "set_loop_autonomy" => server.handle_set_loop_autonomy(id, arguments).await,
            "review_task" => {
                if let Some(plan_id) = arguments.get("plan_id").and_then(|v| v.as_str()) {
                    if let Err((code, message)) =
                        server.check_plan_owner_for_op(plan_id, "review_task").await
                    {
                        return spur_mcp::JsonRpcResponse::error(id, code, message);
                    }
                }
                match server.handle_review_task(&arguments).await {
                    Ok(text) => spur_mcp::JsonRpcResponse::success(
                        id,
                        json!({ "content": [{ "type": "text", "text": text }] }),
                    ),
                    Err(e) => spur_mcp::JsonRpcResponse::internal_error(id, e),
                }
            }
            "submit_plan_mutation" => server.handle_submit_plan_mutation(id, arguments).await,
            _ => spur_mcp::JsonRpcResponse::error(id, -32601, format!("Unknown tool: {tool_name}")),
        }
    }
}

#[async_trait]
impl ToolModule for PlanMcpModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        match self.section {
            PlanToolSection::All => plan_tool_definitions(),
            PlanToolSection::Management => plan_management_tool_definitions(),
            PlanToolSection::Remainder => plan_remainder_tool_definitions(),
        }
    }

    async fn call(
        &self,
        _ctx: ToolCallContext<'_>,
        name: &str,
        _args: Value,
    ) -> Result<ToolResponse, McpError> {
        Err(McpError::new(
            ErrorCode(-32603),
            format!("plan-owned tool {name} must be dispatched by spur-core"),
            None,
        ))
    }
}

pub(crate) fn is_plan_tool(name: &str) -> bool {
    PLAN_TOOL_NAMES.contains(&name)
}

pub(crate) fn worker_tool_definitions() -> Vec<ToolDefinition> {
    vec![get_task_diff_def(), get_plan_status_def()]
}

fn plan_tool_definitions() -> Vec<ToolDefinition> {
    let mut definitions = plan_management_tool_definitions();
    definitions.extend(plan_remainder_tool_definitions());
    definitions
}

fn plan_management_tool_definitions() -> Vec<ToolDefinition> {
    PLAN_MANAGEMENT_TOOL_NAMES
        .iter()
        .map(|name| plan_tool_definition(name))
        .collect()
}

fn plan_remainder_tool_definitions() -> Vec<ToolDefinition> {
    PLAN_REMAINDER_TOOL_NAMES
        .iter()
        .map(|name| plan_tool_definition(name))
        .collect()
}

fn plan_tool_definition(name: &str) -> ToolDefinition {
    match name {
        "merge_plan" => merge_plan_def(),
        "resume_plan" => resume_plan_def(),
        "force_reclaim_plan" => force_reclaim_plan_def(),
        "submit_plan" => submit_plan_def(),
        "spur_loop_doctor" => spur_loop_doctor_def(),
        "submit_loop" => submit_loop_def(),
        "execute_epic" => execute_epic_def(),
        "get_plan_status" => get_plan_status_def(),
        "get_loop_status" => get_loop_status_def(),
        "get_reconciler_status" => get_reconciler_status_def(),
        "get_task_diff" => get_task_diff_def(),
        "preview_task_base" => preview_task_base_def(),
        "plan_truncate_and_restart" => plan_truncate_and_restart_def(),
        "recover_orphaned_dispatch" => recover_orphaned_dispatch_def(),
        "pause_loop" => pause_loop_def(),
        "resume_loop" => resume_loop_def(),
        "kill_loop" => kill_loop_def(),
        "set_loop_autonomy" => set_loop_autonomy_def(),
        "review_task" => review_task_def(),
        "submit_plan_mutation" => submit_plan_mutation_def(),
        other => panic!("unknown plan MCP tool definition: {other}"),
    }
}

/// Orchestration-domain handles for the plan/review/reconciler MCP tools.
///
/// Bundles the plan/reconciler runtime state currently owned by
/// `McpCallbackServer`. The handles are clone-shared with the server, so the
/// staged migration can move the plan handlers onto this bundle without copying
/// or diverging state.
#[derive(Clone)]
pub struct PlanMcpDeps {
    /// Brain session owner cell used by plan ownership checks.
    pub brain_session_id: Arc<OnceCell<BrainSessionId>>,
    /// Versioned active-plan cache (`plan_id → CachedPlan`).
    pub active_plans: Arc<tokio::sync::Mutex<HashMap<String, CachedPlan>>>,
    /// `epic_id → plan_id` registry for idempotent execute/resume.
    pub plan_registry: Arc<tokio::sync::Mutex<PlanRegistry>>,
    /// Serializes current-brain plan-ownership claims.
    pub plan_claim_lock: Arc<tokio::sync::Mutex<()>>,
    /// Ephemeral reconciler outcome buffers (MUST NOT be persisted to beads).
    pub reconciler_outcomes: Arc<tokio::sync::Mutex<OutcomeStore>>,
    /// PM service for plan submission/projection.
    pub pm_service: Option<Arc<PmService>>,
    /// `PmLike` substrate handle used by the projector/reconciler.
    pub pm_service_like: Option<Arc<dyn PmLike>>,
    /// Test hook used to force persisted-plan version churn between reads.
    pub version_churn_epic_for_test: Arc<tokio::sync::Mutex<Option<String>>>,
    /// Feature gate shared with the license runtime.
    pub feature_gate: Arc<FeatureGate>,
    /// Detached-completion continuation bridge.
    pub continuation_ctx: Arc<DetachedContinuationCtx>,
    /// Outcome materializer for review/reconciler dispatch.
    pub materializer: OutcomeMaterializer,
    /// Blob outcome store backing materialization.
    pub outcome_store: Arc<dyn BlobOutcomeStore>,
    /// Optional MCP lifecycle event sink.
    pub event_sink: Option<Arc<dyn McpEventSink>>,
    /// Repository root for beads-backed plan automation.
    pub repo_root: Option<std::path::PathBuf>,
    /// Persisted-plan versioned-cache serving flag.
    pub versioned_cache_serve: bool,
    /// PR3 non-advisory review-write flag.
    pub nonadvisory_review_writes: bool,
    /// Reconciler-owned dispatch lease duration.
    pub dispatch_lease_duration: Duration,
    /// Opt-in auto-merge/PR on durable epic completion.
    pub auto_merge_approved_plans: bool,
    /// Startup quarantine grace for stale `spur:plan-pending` epics.
    pub plan_pending_grace: Duration,
    /// Whether the beads reconciler is enabled.
    pub reconciler_enabled: bool,
}

impl PlanMcpDeps {
    pub fn catalog_only() -> Self {
        let outcome_store = Arc::new(spur_blob_store::MemoryOutcomeStore::new());
        Self {
            brain_session_id: Arc::new(OnceCell::new()),
            active_plans: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            plan_registry: Arc::new(tokio::sync::Mutex::new(PlanRegistry::default())),
            plan_claim_lock: Arc::new(tokio::sync::Mutex::new(())),
            reconciler_outcomes: Arc::new(tokio::sync::Mutex::new(OutcomeStore::default())),
            pm_service: None,
            pm_service_like: None,
            version_churn_epic_for_test: Arc::new(tokio::sync::Mutex::new(None)),
            feature_gate: crate::server::community_feature_gate(),
            continuation_ctx: Arc::new(DetachedContinuationCtx {
                on_complete: Arc::new(|_, _| Box::pin(async {})),
            }),
            materializer: OutcomeMaterializer::new(outcome_store.clone()),
            outcome_store,
            event_sink: None,
            repo_root: None,
            versioned_cache_serve: false,
            nonadvisory_review_writes: false,
            dispatch_lease_duration: Duration::from_secs(600),
            auto_merge_approved_plans: false,
            plan_pending_grace: Duration::from_secs(300),
            reconciler_enabled: false,
        }
    }

    /// Capture the plan/reconciler orchestration handles off a brain server.
    ///
    /// The handles are `Arc`-shared with the server (see the `ptr_eq` test), so
    /// later stages can route plan handlers through this bundle while the
    /// `McpCallbackServer` still co-owns the same state during the migration.
    pub fn from_server(server: &McpCallbackServer) -> Self {
        Self {
            brain_session_id: server.brain_session_id_cell(),
            active_plans: server.active_plans_handle(),
            plan_registry: server.plan_registry_handle(),
            plan_claim_lock: server.plan_claim_lock_handle(),
            reconciler_outcomes: server.reconciler_outcomes_handle(),
            pm_service: server.pm_service_handle(),
            pm_service_like: server.pm_like_handle(),
            version_churn_epic_for_test: server.version_churn_epic_for_test_handle(),
            feature_gate: server.feature_gate(),
            continuation_ctx: server.continuation_ctx_handle(),
            materializer: server.outcome_materializer(),
            outcome_store: server.outcome_store_handle(),
            event_sink: server.event_sink_handle(),
            repo_root: server.repo_root().map(std::path::Path::to_path_buf),
            versioned_cache_serve: server.versioned_cache_serve(),
            nonadvisory_review_writes: server.nonadvisory_review_writes(),
            dispatch_lease_duration: server.dispatch_lease_duration(),
            auto_merge_approved_plans: server.auto_merge_approved_plans(),
            plan_pending_grace: server.plan_pending_grace(),
            reconciler_enabled: server.reconciler_enabled(),
        }
    }
}

fn merge_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "merge_plan".into(),
        description: "Integrate a fully approved plan onto a dedicated plan-scoped branch. Cherry-picks approved worker branches in deterministic topological order without mutating the active checkout. On success, returns a `merge_branch` you can pass to `create_pr`. On conflict, returns the partial branch plus the conflicting task and files.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The plan_id returned by submit_plan or execute_epic"
                }
            },
            "required": ["plan_id"]
        }),
    }
}

fn resume_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "resume_plan".into(),
        description: "Explicitly claim or resume a persisted beads plan. MVP claims unowned plans and refuses plans with active owners; active handoff is not implemented.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The persisted beads plan_id to claim or resume"
                }
            },
            "required": ["plan_id"]
        }),
    }
}

fn force_reclaim_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "force_reclaim_plan".into(),
        description: "Operator-initiated force-takeover of plan ownership. Removes any existing `spur:plan-owner:*` labels and stamps the current brain as the owner. Intended only for stuck/dead owners or governance-driven takeover; clobbers any concurrent owner brain's in-flight state. Requires explicit `confirm: true`. See docs/multi-brain-operations.md.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The plan_id whose ownership to force-reclaim"
                },
                "confirm": {
                    "type": "boolean",
                    "description": "Must be `true` to acknowledge that this clobbers any concurrent owner brain's in-flight state. `false` or missing returns an error."
                },
                "reason": {
                    "type": "string",
                    "description": "Optional human-readable reason recorded in the audit sentinel for accountability"
                }
            },
            "required": ["plan_id", "confirm"]
        }),
    }
}

fn submit_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "submit_plan".into(),
        description: "Submit a structured execution plan with dependency ordering. The orchestrator dispatches tasks to workers automatically: independent tasks run in parallel, dependent tasks wait for predecessors to complete. Use graph_plan to get dependency-aware tracks, then enrich each item with agent assignment and task description. Returns a plan_id — poll with get_plan_status.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "task_id": {
                                "type": "string",
                                "description": "Unique identifier for this task (use issue ID or descriptive slug)"
                            },
                            "agent": {
                                "type": "string",
                                "description": "Worker agent to execute this task"
                            },
                            "profile": {
                                "type": "string",
                                "description": "Named agent profile from `.spur/agents/<name>.md` (or a pass-through agent/mode name the worker binary already knows). Materialized into the worker worktree and selected on the fresh session; fail-soft on selection.",
                                "default": null
                            },
                            "skills": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Explore pool skill names to materialize into this task's worker worktree before its session starts. Each name must exist in the explore manifest with an accepted gate verdict."
                            },
                            "model": {
                                "type": "string",
                                "description": "Override the worker's model (config-option value id, e.g. \"gpt-5-codex\"). Fail-soft if the agent rejects it.",
                                "default": null
                            },
                            "effort": {
                                "type": "string",
                                "description": "Override the worker's reasoning effort (thought-level value id, e.g. \"low\"/\"medium\"/\"high\"). Fail-soft if the agent rejects it.",
                                "default": null
                            },
                            "config_overrides": {
                                "type": "object",
                                "additionalProperties": { "type": "string" },
                                "description": "Generic worker session config overrides by advertised config-option id. Fail-soft per entry if the agent rejects it.",
                                "default": null
                            },
                            "task": {
                                "type": "string",
                                "description": "Task description (CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT)"
                            },
                            "depends_on": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "task_ids that must complete before this task starts. Empty or omitted = ready immediately."
                            },
                            "issue_id": {
                                "type": "string",
                                "description": "Optional beads issue ID to auto-track"
                            },
                            "context_files": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional file paths for worker context"
                            }
                        },
                        "required": ["task_id", "agent", "task"]
                    },
                    "description": "Tasks with dependency edges forming a DAG. Tasks with no depends_on are dispatched immediately."
                },
                "epic_title": {
                    "type": "string",
                    "description": "Epic title. Required when the first task description is empty or whitespace-only."
                },
                "epic_body": {
                    "type": "string",
                    "description": "Epic description / rationale. Optional."
                },
                "client_idempotency_key": {
                    "type": "string",
                    "description": "Optional caller-supplied idempotency key. When repeated with a beads-backed persisted submit_plan within the dedup TTL, returns the existing plan_id without creating another epic."
                },
                "base": {
                    "description": "Optional explicit base for the plan. Omit (or pass {\"kind\":\"repo_main\"}) for legacy behavior — the plan engine snapshots the brain working tree HEAD. Pass {\"kind\":\"branch\",\"name\":\"<branch>\"} or {\"kind\":\"commit\",\"oid\":\"<oid>\"} to base the plan on a named ref instead; the brain working tree is not touched. Useful for stacking plans on a prior phase's integration branch.",
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": { "kind": { "const": "repo_main" } },
                            "required": ["kind"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "kind": { "const": "branch" },
                                "name": { "type": "string" }
                            },
                            "required": ["kind", "name"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "kind": { "const": "commit" },
                                "oid": { "type": "string" }
                            },
                            "required": ["kind", "oid"]
                        }
                    ]
                }
            },
            "required": ["tasks"]
        }),
    }
}

fn submit_loop_def() -> ToolDefinition {
    ToolDefinition {
        name: "submit_loop".into(),
        description: "Create a durable loop issue with a [[spur-loop v1]] spec sentinel. Validates cadence_secs >= 60, defaults omitted autonomy to l1, requires at least one template task marked spur:loop-triage-task, rejects non-positive governor caps, mints a compact loop_id, and labels the loop spur:loop-id:<id>, spur:autonomy:<level>, and spur:loop-next-run:<now> so it fires immediately.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::SubmitLoopParams>(),
    }
}

fn spur_loop_doctor_def() -> ToolDefinition {
    ToolDefinition {
        name: "spur_loop_doctor".into(),
        description: "Required validation gate for /spur-loop natural-language drafts. Validates and normalizes a structured draft, returns a friendly preview, canonical submit_loop params, approval fingerprint, and idempotency key when valid, and never creates durable loops.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::SpurLoopDoctorParams>(),
    }
}

fn get_plan_status_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_plan_status".into(),
        description: "Get the current status of a submitted execution plan. Returns per-task status: pending (waiting for deps), ready, dispatched (running), completed, or failed. Non-blocking — returns immediately.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The plan_id returned by submit_plan"
                }
            },
            "required": ["plan_id"]
        }),
    }
}

fn get_loop_status_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_loop_status".into(),
        description: "Return loop status as JSON for a loop_id: parsed LoopSpec, last recent_runs LoopRun audit records, effective backoff interval, consecutive failure count, paused flag, and next_run timestamp.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::GetLoopStatusParams>(),
    }
}

fn get_reconciler_status_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_reconciler_status".into(),
        description: "Get the reconciler's in-memory observability state across all plans, including recent dispatch outcomes, stuck tasks, and last tick time.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

fn get_task_diff_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_task_diff".to_string(),
        description: "Get the full unified diff for a plan task. Use after \
            get_plan_status shows tasks in awaiting_review, approved, rejected, or \
            failed state. Returns the complete diff, worker branch name, task \
            description, and summary for brain code review. Pass `attempt` to inspect \
            prior iteration attempts (see entry.history)."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": { "type": "string", "description": "The plan_id returned by submit_plan" },
                "task_id": { "type": "string", "description": "The task_id to inspect" },
                "attempt": {
                    "type": "integer",
                    "description": "Optional: inspect a prior attempt (1..current-1). Omit for the latest attempt."
                }
            },
            "required": ["plan_id", "task_id"]
        }),
    }
}

fn preview_task_base_def() -> ToolDefinition {
    ToolDefinition {
        name: "preview_task_base".into(),
        description: "Read-only: returns the overlay commits and predicted base OID for a given plan task without creating a worker worktree. Use this BEFORE approving a downstream task to surface integration conflicts early. Returns null `predicted_base_oid` and a `conflict` payload when overlays cannot be applied cleanly.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::PreviewTaskBaseInput>(),
    }
}

fn plan_truncate_and_restart_def() -> ToolDefinition {
    ToolDefinition {
        name: "plan_truncate_and_restart".into(),
        description: "Recovery tool for plans blocked by overlay conflicts. Cherry-picks approved task tips in DAG order onto a fresh `spur/plan-staging/{plan_id}` branch, marks remaining tasks Superseded in the original plan, and submits a new plan whose tasks dispatch against the staging branch. Use after `BlockedOnSetupConflict` when the conflict is across approved siblings (i.e. cannot be unwound by re-dispatching a single upstream task).".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::PlanTruncateAndRestartInput>(),
    }
}

fn recover_orphaned_dispatch_def() -> ToolDefinition {
    ToolDefinition {
        name: "recover_orphaned_dispatch".into(),
        description: "Brain-side recovery tool. Promote a stuck Dispatched beads task to AwaitingReview when the worker branch and dispatch base OID are known. Validates the task is still dispatched, the worker branch exists, and the branch contains exactly one commit over the dispatched base.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::RecoverOrphanedDispatchInput>(),
    }
}

fn pause_loop_def() -> ToolDefinition {
    ToolDefinition {
        name: "pause_loop".into(),
        description: "Pause a loop by adding spur:loop-paused to the loop issue identified by loop_id. Existing in-flight generations are not cancelled.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::LoopIdParams>(),
    }
}

fn resume_loop_def() -> ToolDefinition {
    ToolDefinition {
        name: "resume_loop".into(),
        description: "Resume a paused loop by removing spur:loop-paused and replacing any spur:loop-next-run:* label with spur:loop-next-run:<now>, clearing failure backoff so the scheduler may run it immediately.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::LoopIdParams>(),
    }
}

fn kill_loop_def() -> ToolDefinition {
    ToolDefinition {
        name: "kill_loop".into(),
        description: "Retire a loop by appending a terminal LoopRun audit record with outcome retired, removing all spur:loop-next-run:* labels, and closing the loop issue identified by loop_id. Repeated calls on an already-closed loop return current state without writing another record.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::LoopIdParams>(),
    }
}

fn set_loop_autonomy_def() -> ToolDefinition {
    ToolDefinition {
        name: "set_loop_autonomy".into(),
        description: "Set a loop's autonomy to l1, l2, or l3. Demotions are immediate; promotions advance one level at a time and require three consecutive approved real generations at the current level. Updates the [[spur-loop v1]] spec body and spur:autonomy:* label together.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::SetLoopAutonomyParams>(),
    }
}

fn review_task_def() -> ToolDefinition {
    ToolDefinition {
        name: "review_task".to_string(),
        description: "Submit a review decision for a plan task awaiting review. \
            Three decisions: 'approve' (task done, beads→closed), 'reject' (task \
            dead, beads→closed with `spur:review-rejected`, dependent tasks \
            auto-failed), or 'request_changes' (persist task back to open, clear \
            review ownership, and let the reconciler redispatch when ready — max 3 \
            attempts per task, requires `feedback`). Returns updated plan status \
            with counts and ready_to_merge flag."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The plan_id returned by submit_plan"
                },
                "task_id": {
                    "type": "string",
                    "description": "The task_id to review"
                },
                "decision": {
                    "type": "string",
                    "enum": ["approve", "reject", "request_changes"],
                    "description": "Review verdict. 'approve' → task marked done and persisted closed; newly-ready work is picked up by the reconciler. 'reject' → task terminal, pending/ready dependents cascaded to failed (dispatched/awaiting_review dependents flagged in warnings). 'request_changes' → task persisted back to open so the reconciler redispatch it later, bounded by max_attempts (3)."
                },
                "feedback": {
                    "type": "string",
                    "description": "Optional feedback. Recommended when decision is `request_changes` so the worker has context for the next attempt."
                },
                "reuse_prior_worktree": {
                    "type": "boolean",
                    "description": "When request_changes is the decision, opt in to having the prior rejected attempt's diff pre-applied as uncommitted changes in the next attempt's worktree."
                }
            },
            "required": ["plan_id", "task_id", "decision"]
        }),
    }
}

fn submit_plan_mutation_def() -> ToolDefinition {
    ToolDefinition {
        name: "submit_plan_mutation".into(),
        description: "Brain-side recovery tool. This is the 'Swiss Army knife' for \
            the brain agent to fix escalated/failed running plans. Apply an \
            atomic batch of plan-graph mutations to recover an escalated task: \
            retry it as-is, rewrite its spec (task body, agent, context, deps), \
            or abandon it. Wraps `apply_mutation` end-to-end with cycle \
            detection + rollback. Clears `signal:escalated` from every \
            affected issue on success."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["trigger_task_id", "ops"],
            "properties": {
                "trigger_task_id": {
                    "type": "string",
                    "description": "Beads issue id used as the audit anchor for the batch. Typically the escalated task."
                },
                "mutation_id": {
                    "type": "string",
                    "description": "Optional UUID; auto-generated when absent."
                },
                "ops": {
                    "type": "array",
                    "description": "Ordered list of `PlanMutationOp` JSON values. Supported tags: split_task, retry_task, modify_task_spec, abandon_task.",
                    "items": { "type": "object" }
                },
                "rationale": {
                    "type": "string",
                    "description": "Free-form explanation for the audit trail."
                }
            }
        }),
    }
}

fn execute_epic_def() -> ToolDefinition {
    ToolDefinition {
        name: "execute_epic".into(),
        description: "Execute a beads epic: hydrate a plan from the epic's \
            children subgraph and dispatch in dependency order. Agent routing \
            comes from the `spur:agent:<name>` label on each child issue \
            (inherited from the epic if unset, or from default_agent). Task \
            text comes from issue.body. Rejects nested sub-epic children. External blocked_by \
            references must already be `done`. After dispatch, the plan runs \
            under the normal review engine — use get_plan_status / \
            get_task_diff / review_task. Re-calling while a plan is active \
            for the same epic returns the existing plan_id (idempotent). \
            After terminal state, a new call starts a fresh plan."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "epic_id": {
                    "type": "string",
                    "description": "The beads ID of an issue with type=epic"
                },
                "default_agent": {
                    "type": "string",
                    "description": "Fallback agent when a child has no `spur:agent:<name>` label and the epic has no inherited label"
                }
            },
            "required": ["epic_id"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::community_feature_gate;
    use spur_acp::{BrainSessionId, SessionId};

    fn no_op_continuation() -> DetachedContinuationCtx {
        DetachedContinuationCtx {
            on_complete: Arc::new(|_, _| Box::pin(async {})),
        }
    }

    #[tokio::test]
    async fn from_server_captures_orchestration_state() {
        let session = BrainSessionId::new(SessionId("brain".into()));
        let (mut server, _channel) = McpCallbackServer::new(
            Some(&session),
            None,
            None,
            no_op_continuation(),
            Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
            community_feature_gate(),
        );
        server.set_dispatch_lease_duration(Duration::from_secs(123));
        server.set_versioned_cache_serve(true);
        server.set_nonadvisory_review_writes(true);

        let deps = PlanMcpDeps::from_server(&server);

        // Config snapshot is captured faithfully from the server.
        assert_eq!(deps.dispatch_lease_duration, Duration::from_secs(123));
        assert!(deps.versioned_cache_serve);
        assert!(deps.nonadvisory_review_writes);
        assert!(!deps.reconciler_enabled);
        assert!(!deps.auto_merge_approved_plans);
        assert!(deps.pm_service.is_none());
        assert!(deps.pm_service_like.is_none());

        // Plan-state handles are clone-shared with the server, not fresh copies.
        assert!(
            Arc::ptr_eq(&deps.brain_session_id, &server.brain_session_id_cell()),
            "brain_session_id must be shared with the server"
        );
        assert!(
            Arc::ptr_eq(&deps.active_plans, &server.active_plans_handle()),
            "active_plans must be shared with the server"
        );
        assert!(
            Arc::ptr_eq(&deps.plan_registry, &server.plan_registry_handle()),
            "plan_registry must be shared with the server"
        );
        assert!(
            Arc::ptr_eq(
                &deps.reconciler_outcomes,
                &server.reconciler_outcomes_handle()
            ),
            "reconciler_outcomes must be shared with the server"
        );
        assert!(
            Arc::ptr_eq(&deps.plan_claim_lock, &server.plan_claim_lock_handle()),
            "plan_claim_lock must be shared with the server"
        );
        assert!(
            Arc::ptr_eq(
                &deps.version_churn_epic_for_test,
                &server.version_churn_epic_for_test_handle()
            ),
            "version_churn_epic_for_test must be shared with the server"
        );
    }
}

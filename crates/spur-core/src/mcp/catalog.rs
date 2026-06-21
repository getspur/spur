use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde_json::{json, Value};

use spur_mcp::{ToolCallContext, ToolDefinition, ToolModule, ToolResponse};

const PM_CRUD_TOOL_NAMES: &[&str] = &[
    "get_issue",
    "list_issues",
    "update_issue",
    "create_issue",
    "add_dependency",
    "create_pr",
];

const PM_ISSUE_GRAPH_TOOL_NAMES: &[&str] = &[
    "graph_triage",
    "graph_plan",
    "graph_insights",
    "graph_alerts",
    "graph_subgraph",
];

const PLAN_TOOL_NAMES: &[&str] = &[
    "merge_plan",
    "resume_plan",
    "force_reclaim_plan",
    "submit_plan",
    "execute_epic",
    "get_plan_status",
    "get_reconciler_status",
    "get_task_diff",
    "preview_task_base",
    "plan_truncate_and_restart",
    "recover_orphaned_dispatch",
    "review_task",
    "submit_plan_mutation",
];

const ANALYST_TOOL_NAMES: &[&str] = &[
    "doc_navigate",
    "knowledge_context_pack",
    "knowledge_context_pack_2",
];

pub(crate) fn is_server_owned_tool(name: &str) -> bool {
    is_pm_tool(name) || is_plan_tool(name) || is_graph_tool(name) || is_analyst_tool(name)
}

pub(crate) fn is_pm_tool(name: &str) -> bool {
    PM_CRUD_TOOL_NAMES.contains(&name) || PM_ISSUE_GRAPH_TOOL_NAMES.contains(&name)
}

pub(crate) fn is_plan_tool(name: &str) -> bool {
    PLAN_TOOL_NAMES.contains(&name)
}

pub(crate) fn is_graph_tool(name: &str) -> bool {
    spur_graph::mcp::tool_definitions()
        .iter()
        .any(|definition| definition.name == name)
}

pub(crate) fn is_analyst_tool(name: &str) -> bool {
    ANALYST_TOOL_NAMES.contains(&name)
}

pub(crate) struct ServerCatalogMcpModule;

pub(crate) struct WorkerCatalogMcpModule;

#[async_trait]
impl ToolModule for ServerCatalogMcpModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        server_tool_definitions()
    }

    async fn call(
        &self,
        _ctx: ToolCallContext<'_>,
        name: &str,
        _args: Value,
    ) -> Result<ToolResponse, McpError> {
        Err(McpError::new(
            ErrorCode(-32603),
            format!("server-owned tool {name} must be dispatched by spur-core"),
            None,
        ))
    }
}

#[async_trait]
impl ToolModule for WorkerCatalogMcpModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        worker_tool_definitions()
    }

    async fn call(
        &self,
        _ctx: ToolCallContext<'_>,
        name: &str,
        _args: Value,
    ) -> Result<ToolResponse, McpError> {
        Err(McpError::new(
            ErrorCode(-32603),
            format!("worker-owned tool {name} must be dispatched by spur-core"),
            None,
        ))
    }
}

pub(crate) fn server_tool_definitions() -> Vec<ToolDefinition> {
    let mut definitions = Vec::new();
    definitions.extend(pm_tool_definitions_by_names(PM_CRUD_TOOL_NAMES));
    definitions.extend(plan_management_tool_definitions());
    definitions.extend(pm_tool_definitions_by_names(PM_ISSUE_GRAPH_TOOL_NAMES));
    definitions.extend(
        spur_graph::mcp::tool_definitions()
            .into_iter()
            .map(graph_tool_definition),
    );
    definitions.extend(
        spur_analyst::mcp::tool_definitions()
            .into_iter()
            .map(analyst_tool_definition),
    );
    definitions.extend(plan_remainder_tool_definitions());
    definitions
}

pub(crate) fn worker_tool_definitions() -> Vec<ToolDefinition> {
    let mut definitions = pm_tool_definitions_by_names(&["get_issue", "list_issues"]);
    definitions.extend([get_task_diff_def(), get_plan_status_def()]);
    definitions.extend(
        spur_graph::mcp::tool_definitions()
            .into_iter()
            .map(graph_tool_definition),
    );
    definitions.extend(
        spur_analyst::mcp::tool_definitions()
            .into_iter()
            .map(analyst_tool_definition),
    );
    definitions.push(crate::worker_server::fetch_outcome_artifact_tool_definition());
    definitions.extend(crate::worker_server::worker_signal_tool_definitions());
    definitions
}

fn pm_tool_definitions_by_names(names: &[&str]) -> Vec<ToolDefinition> {
    let definitions = spur_pm::mcp::tool_definitions();
    names
        .iter()
        .map(|name| {
            definitions
                .iter()
                .find(|definition| definition.name == *name)
                .unwrap_or_else(|| panic!("spur-pm MCP module missing tool definition {name}"))
                .clone()
        })
        .map(pm_tool_definition)
        .collect()
}

fn pm_tool_definition(definition: spur_pm::mcp::ToolDefinition) -> ToolDefinition {
    ToolDefinition {
        name: definition.name,
        description: definition.description,
        input_schema: definition.input_schema,
    }
}

fn graph_tool_definition(definition: spur_graph::mcp::ToolDefinition) -> ToolDefinition {
    ToolDefinition {
        name: definition.name,
        description: definition.description,
        input_schema: definition.input_schema,
    }
}

fn analyst_tool_definition(definition: spur_analyst::mcp::ToolDefinition) -> ToolDefinition {
    ToolDefinition {
        name: definition.name,
        description: definition.description,
        input_schema: definition.input_schema,
    }
}

fn plan_management_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        merge_plan_def(),
        resume_plan_def(),
        force_reclaim_plan_def(),
    ]
}

fn plan_remainder_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        submit_plan_def(),
        execute_epic_def(),
        get_plan_status_def(),
        get_reconciler_status_def(),
        get_task_diff_def(),
        preview_task_base_def(),
        plan_truncate_and_restart_def(),
        recover_orphaned_dispatch_def(),
        review_task_def(),
        submit_plan_mutation_def(),
    ]
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

pub(crate) fn issue_to_summary_event(
    issue: &spur_pm::Issue,
    source: &'static str,
) -> spur_acp::domain::events::IssueSummaryEvent {
    spur_acp::domain::events::IssueSummaryEvent {
        id: issue.id.clone(),
        source: source.to_string(),
        title: issue.title.clone(),
        status: issue.status.clone(),
        labels: issue.labels.clone(),
        priority: issue.priority,
        issue_type: issue.issue_type.clone(),
        assignee: issue.assignee.clone(),
        description: Some(issue.body.clone()).filter(|body| !body.trim().is_empty()),
    }
}

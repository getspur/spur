use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use spur_acp::DelegationResult;
use tokio::sync::oneshot;

// ─── Request/Response types for orchestrator communication ────────────

/// A delegation request sent from the MCP server to the orchestrator.
///
/// Each request carries a oneshot sender so the orchestrator can respond
/// directly to the originating handler — no shared response channel, no
/// ID-based matching, no dropped messages.
#[derive(Debug)]
pub struct DelegationRequest {
    pub id: String,
    pub agent: String,
    pub task: String,
    pub context_files: Vec<String>,
    /// Oneshot channel for the orchestrator to send the result back.
    pub respond_to: oneshot::Sender<DelegationResult>,
    /// Brain session that originated this request. Threaded through so
    /// `DelegationRequested.from` / `DelegationDispatched.from` can
    /// correctly identify the brain in lineage. Stamped at every
    /// construction site in the MCP server.
    pub brain_session_id: spur_acp::SessionId,
    /// Structured reasoning trace the brain passed with this call.
    /// None when brain omitted the parameter. Orchestrator uses this
    /// for reviewer-visibility and mismatch detection. See design
    /// spec section C.
    pub delegation_plan: Option<spur_acp::DelegationPlan>,
    /// Optional beads issue ID to auto-track for this delegation.
    pub issue_id: Option<String>,
}

/// Channel the orchestrator holds to receive requests from the MCP server.
pub struct DelegationChannel {
    pub request_rx: tokio::sync::mpsc::Receiver<DelegationRequest>,
}

// ─── Tool definition ──────────────────────────────────────────────────

/// Metadata for a single MCP tool, returned by `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

// ─── Tool definitions ─────────────────────────────────────────────────

fn delegate_to_worker_def() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_to_worker".into(),
        description: "Delegate a task to a worker agent. Blocks until the worker completes or a 90-second safety timeout is reached. If the worker is still running at timeout, returns a delegation_id — call check_delegation_status to poll for the result. Pass a `delegation_plan` parameter (at minimum `{chosen, rationale}`; more for multi-step work). Structure the `task` field as CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT. Use `list_available_workers` when routing is ambiguous.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "Name of the worker agent to delegate to"
                },
                "task": {
                    "type": "string",
                    "description": "Task description for the worker. Structure as CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT."
                },
                "context_files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional supplementary file paths. Prefer inlining relevant excerpts in the task field's CONTEXT section."
                },
                "delegation_plan": {
                    "type": "object",
                    "description": "Structured reasoning for this delegation. At minimum pass {chosen, rationale}. For 2+ subtasks or >3 files, include candidates and decomposition.",
                    "properties": {
                        "candidates": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "agent":     { "type": "string" },
                                    "rationale": { "type": "string" }
                                }
                            }
                        },
                        "decomposition": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "subtask":             { "type": "string" },
                                    "parallelizable_with": { "type": "array", "items": { "type": "string" } }
                                }
                            }
                        },
                        "chosen":    { "type": "string" },
                        "rationale": { "type": "string" }
                    }
                },
                "issue_id": {
                    "type": "string",
                    "description": "Optional beads issue ID to auto-track"
                }
            },
            "required": ["agent", "task"]
        }),
    }
}

fn delegate_parallel_def() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_parallel".into(),
        description: "Delegate multiple tasks in parallel. Blocks until all complete. The `delegation_plan.decomposition` field MUST demonstrate subtasks are independent — no shared state, no sequential data dependencies. If unsure, use `delegate_to_worker` serially.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent": { "type": "string", "description": "Worker agent name" },
                            "task":  { "type": "string", "description": "Task description" }
                        },
                        "required": ["agent", "task"]
                    },
                    "description": "List of tasks to delegate in parallel"
                },
                "delegation_plan": {
                    "type": "object",
                    "description": "Structured reasoning for the parallel dispatch. The `decomposition` section MUST demonstrate subtasks are independent.",
                    "properties": {
                        "candidates":    { "type": "array" },
                        "decomposition": { "type": "array" },
                        "chosen":        { "type": "string" },
                        "rationale":     { "type": "string" }
                    }
                },
                "issue_id": {
                    "type": "string",
                    "description": "Optional beads issue ID to auto-track"
                }
            },
            "required": ["tasks"]
        }),
    }
}

fn list_available_workers_def() -> ToolDefinition {
    ToolDefinition {
        name: "list_available_workers".into(),
        description: "Returns tier, description, good_for, avoid_for, output_shape, and cost_tier for each worker. Call when the system-prompt one-liner is insufficient.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

fn get_issue_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_issue".into(),
        description: "Retrieve an issue from the configured project management backend (beads, GitHub, etc.)."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "PM source override (github, linear, plane). Defaults to configured backend if omitted."
                },
                "id": {
                    "type": "string",
                    "description": "Issue identifier"
                }
            },
            "required": ["id"]
        }),
    }
}

fn list_issues_def() -> ToolDefinition {
    ToolDefinition {
        name: "list_issues".into(),
        description: "List issues from the configured project management backend with optional filters.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "description": "Filter by status: open, in_progress, blocked, closed"
                },
                "labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Filter by labels (issues must have all listed labels)"
                },
                "assignee": {
                    "type": "string",
                    "description": "Filter by assignee username"
                },
                "priority_min": {
                    "type": "integer",
                    "description": "Minimum priority (0=critical, 4=backlog)"
                },
                "priority_max": {
                    "type": "integer",
                    "description": "Maximum priority (0=critical, 4=backlog)"
                },
                "issue_type": {
                    "type": "string",
                    "description": "Filter by issue type: task, bug, feature, epic"
                },
                "text_search": {
                    "type": "string",
                    "description": "Search issue titles by text"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 20, max 100)"
                }
            }
        }),
    }
}

fn update_issue_def() -> ToolDefinition {
    ToolDefinition {
        name: "update_issue".into(),
        description: "Update an issue in the configured project management backend.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "PM source override (github, linear, plane). Defaults to configured backend if omitted."
                },
                "id": {
                    "type": "string",
                    "description": "Issue identifier"
                },
                "status": {
                    "type": "string",
                    "description": "New status to set"
                },
                "comment": {
                    "type": "string",
                    "description": "Comment to add to the issue"
                },
                "priority": {
                    "type": "integer",
                    "description": "New priority (0=critical, 4=backlog)"
                },
                "assignee": {
                    "type": "string",
                    "description": "Assignee username. Use empty string to unassign."
                },
                "add_labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Labels to add to the issue"
                },
                "remove_labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Labels to remove from the issue"
                }
            },
            "required": ["id"]
        }),
    }
}

fn create_pr_def() -> ToolDefinition {
    ToolDefinition {
        name: "create_pr".into(),
        description: "Create a pull request.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "PR title"
                },
                "body": {
                    "type": "string",
                    "description": "PR description body"
                },
                "branch": {
                    "type": "string",
                    "description": "Head branch name"
                }
            },
            "required": ["title", "body", "branch"]
        }),
    }
}

fn report_progress_def() -> ToolDefinition {
    ToolDefinition {
        name: "report_progress".into(),
        description: "Report progress to the orchestrator (fire-and-forget).".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "Progress message"
                }
            },
            "required": ["message"]
        }),
    }
}

fn get_session_cost_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_session_cost".into(),
        description: "Get the current cost breakdown for this session.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

fn delegate_async_def() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_async".into(),
        description: "Delegate a task to a worker agent without blocking. Returns a delegation_id that can be collected later with wait_delegation.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "agent": {
                    "type": "string",
                    "description": "Name of the worker agent to delegate to"
                },
                "task": {
                    "type": "string",
                    "description": "Task description for the worker"
                },
                "context_files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of file paths to provide as context"
                },
                "delegation_plan": {
                    "type": "object",
                    "description": "Structured reasoning for this delegation. At minimum pass {chosen, rationale}. For 2+ subtasks or >3 files, include candidates and decomposition.",
                    "properties": {
                        "candidates": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "agent":     { "type": "string" },
                                    "rationale": { "type": "string" }
                                }
                            }
                        },
                        "decomposition": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "subtask":             { "type": "string" },
                                    "parallelizable_with": { "type": "array", "items": { "type": "string" } }
                                }
                            }
                        },
                        "chosen":    { "type": "string" },
                        "rationale": { "type": "string" }
                    }
                },
                "issue_id": {
                    "type": "string",
                    "description": "Optional beads issue ID to auto-track"
                }
            },
            "required": ["agent", "task"]
        }),
    }
}

fn wait_delegation_def() -> ToolDefinition {
    ToolDefinition {
        name: "wait_delegation".into(),
        description: "Block until an async delegation completes and return its result. Use after delegate_async. If the worker is still running after 90 seconds, returns a 'still running' message — call check_delegation_status to poll again.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "delegation_id": {
                    "type": "string",
                    "description": "The delegation_id returned by delegate_async"
                }
            },
            "required": ["delegation_id"]
        }),
    }
}

fn check_delegation_status_def() -> ToolDefinition {
    ToolDefinition {
        name: "check_delegation_status".into(),
        description: "Non-blocking poll for a delegation result. Returns the result immediately if the worker has finished, or {\"status\":\"running\"} if still in progress. Use after delegate_async or when delegate_to_worker / wait_delegation returned a delegation_id due to timeout.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "delegation_id": {
                    "type": "string",
                    "description": "The delegation_id to check"
                }
            },
            "required": ["delegation_id"]
        }),
    }
}

fn cancel_delegation_def() -> ToolDefinition {
    ToolDefinition {
        name: "cancel_delegation".into(),
        description: "Request cancellation of a running delegation. If the delegation already completed, returns its result. Otherwise forwards the cancellation to the orchestrator and returns its response.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "delegation_id": {
                    "type": "string",
                    "description": "The delegation_id to cancel"
                }
            },
            "required": ["delegation_id"]
        }),
    }
}

// ─── Graph analysis tool definitions (bv robot protocol) ────────────

fn graph_triage_def() -> ToolDefinition {
    ToolDefinition {
        name: "graph_triage".into(),
        description: "Get PageRank-weighted project triage: top recommendations, quick wins, blockers to clear, and project health metrics. Complements list_issues (CRUD) with graph-based dependency analysis. Call this FIRST for orientation before starting work. Note: triage includes alert data — only call graph_alerts separately when you need standalone alert monitoring without the full triage context. Optionally scope to a label. Requires bv (beads_viewer) to be installed.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Scope analysis to issues with this label"
                }
            }
        }),
    }
}

fn graph_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "graph_plan".into(),
        description: "Get a dependency-aware parallel execution plan. Returns independent tracks of work that can proceed simultaneously. Use to identify what can be delegated in parallel.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Scope to issues with this label"
                }
            }
        }),
    }
}

fn graph_insights_def() -> ToolDefinition {
    ToolDefinition {
        name: "graph_insights".into(),
        description: "Get full graph metrics: PageRank (importance), betweenness (bottlenecks), HITS (hubs/authorities), critical path, cycles, articulation points. Use for deep structural analysis of the project dependency graph.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Scope to issues with this label"
                }
            }
        }),
    }
}

fn graph_alerts_def() -> ToolDefinition {
    ToolDefinition {
        name: "graph_alerts".into(),
        description: "Get active health alerts: stale issues, blocking cascades, priority mismatches, circular dependencies. Use for project health monitoring.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

fn graph_subgraph_def() -> ToolDefinition {
    ToolDefinition {
        name: "graph_subgraph".into(),
        description: "Get the dependency subgraph for a specific issue. Returns nodes and edges showing what this issue depends on and what depends on it. Use format=mermaid for visual dependency diagrams.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "root_id": {
                    "type": "string",
                    "description": "Issue ID to center the subgraph on"
                },
                "depth": {
                    "type": "integer",
                    "description": "Max traversal depth (0 = unlimited, default)"
                },
                "format": {
                    "type": "string",
                    "enum": ["json", "dot", "mermaid"],
                    "description": "Output format (default: json)"
                }
            },
            "required": ["root_id"]
        }),
    }
}

// ─── Issue creation + dependency tools ────────────────────────────

fn create_issue_def() -> ToolDefinition {
    ToolDefinition {
        name: "create_issue".into(),
        description: "Create a new issue in the project tracker. For task decomposition: create an epic first (type=epic), then create child tasks with parent=<epic_id>. Structure descriptions as CONTEXT / GOAL / CONSTRAINTS / ACCEPTANCE CRITERIA. Use depends_on to wire dependency edges so graph_plan can compute optimal execution ordering. After creating all tasks, call graph_plan() to get dependency-aware tracks, then submit_plan() to execute.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Issue title — concise, action-oriented"
                },
                "description": {
                    "type": "string",
                    "description": "Detailed description (markdown). Structure as CONTEXT / GOAL / CONSTRAINTS / ACCEPTANCE CRITERIA for tasks."
                },
                "type": {
                    "type": "string",
                    "enum": ["task", "bug", "feature", "epic"],
                    "description": "Issue type. Use 'epic' for grouping, 'task' for work items."
                },
                "priority": {
                    "type": "integer",
                    "description": "Priority 0-4 (0=critical, 4=backlog). Affects graph_triage ranking."
                },
                "labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Labels for categorization"
                },
                "parent": {
                    "type": "string",
                    "description": "Parent issue ID — creates parent-child dependency (e.g., epic ID for child tasks)"
                },
                "assignee": {
                    "type": "string",
                    "description": "Assignee username"
                },
                "estimate": {
                    "type": "integer",
                    "description": "Time estimate in minutes"
                },
                "depends_on": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Issue IDs this new issue depends on (blocking deps). Used by graph_plan for execution ordering."
                }
            },
            "required": ["title"]
        }),
    }
}

fn add_dependency_def() -> ToolDefinition {
    ToolDefinition {
        name: "add_dependency".into(),
        description: "Add a dependency edge between existing issues: issue_id is blocked by depends_on_id. Use after creating issues to wire the dependency graph. After wiring deps, call graph_plan() to get optimized execution ordering. Beads backend only — returns error for GitHub.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "issue_id": {
                    "type": "string",
                    "description": "The issue that is blocked"
                },
                "depends_on_id": {
                    "type": "string",
                    "description": "The issue that blocks it (must complete first)"
                }
            },
            "required": ["issue_id", "depends_on_id"]
        }),
    }
}

// ─── Plan execution tool definitions ──────────────────────────────

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
                "delegation_plan": {
                    "type": "object",
                    "description": "Structured reasoning for the overall plan."
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

/// Returns all tool definitions for the MCP `tools/list` response.
pub fn tools_list() -> Vec<ToolDefinition> {
    vec![
        delegate_to_worker_def(),
        delegate_parallel_def(),
        delegate_async_def(),
        wait_delegation_def(),
        check_delegation_status_def(),
        cancel_delegation_def(),
        list_available_workers_def(),
        get_issue_def(),
        list_issues_def(),
        update_issue_def(),
        create_issue_def(),
        add_dependency_def(),
        create_pr_def(),
        report_progress_def(),
        get_session_cost_def(),
        graph_triage_def(),
        graph_plan_def(),
        graph_insights_def(),
        graph_alerts_def(),
        graph_subgraph_def(),
        submit_plan_def(),
        get_plan_status_def(),
    ]
}

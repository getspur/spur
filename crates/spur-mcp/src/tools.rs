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
        description: "Delegate a task to a worker agent. Blocks until the worker completes."
            .into(),
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
                }
            },
            "required": ["agent", "task"]
        }),
    }
}

fn delegate_parallel_def() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_parallel".into(),
        description:
            "Delegate multiple tasks to worker agents in parallel. Blocks until all complete."
                .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent": {
                                "type": "string",
                                "description": "Name of the worker agent"
                            },
                            "task": {
                                "type": "string",
                                "description": "Task description for the worker"
                            }
                        },
                        "required": ["agent", "task"]
                    },
                    "description": "List of tasks to delegate in parallel"
                }
            },
            "required": ["tasks"]
        }),
    }
}

fn list_available_workers_def() -> ToolDefinition {
    ToolDefinition {
        name: "list_available_workers".into(),
        description: "List all available worker agents and their capabilities.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

fn get_issue_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_issue".into(),
        description: "Retrieve an issue from a project management tool (GitHub, Linear, Plane)."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "PM source: github, linear, or plane"
                },
                "id": {
                    "type": "string",
                    "description": "Issue identifier"
                }
            },
            "required": ["source", "id"]
        }),
    }
}

fn update_issue_def() -> ToolDefinition {
    ToolDefinition {
        name: "update_issue".into(),
        description: "Update an issue in a project management tool.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "PM source: github, linear, or plane"
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
                }
            },
            "required": ["source", "id"]
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

/// Returns all tool definitions for the MCP `tools/list` response.
pub fn tools_list() -> Vec<ToolDefinition> {
    vec![
        delegate_to_worker_def(),
        delegate_parallel_def(),
        list_available_workers_def(),
        get_issue_def(),
        update_issue_def(),
        create_pr_def(),
        report_progress_def(),
        get_session_cost_def(),
    ]
}

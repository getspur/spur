use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{Issue, IssueCreate, IssueFilter, IssueUpdate, PmService, PrParams};

pub use spur_mcp::tools::McpHandlerError;

/// Metadata for a single PM-owned MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct IssueCreatedEvent {
    pub issue: Issue,
    pub source: &'static str,
}

#[derive(Clone, Default)]
pub struct PmMcpDeps {
    pub pm_service: Option<Arc<PmService>>,
    pub on_issue_created: Option<Arc<dyn Fn(IssueCreatedEvent) + Send + Sync>>,
}

#[derive(Clone, Default)]
pub struct PmMcpModule {
    deps: PmMcpDeps,
}

impl PmMcpModule {
    pub fn new(deps: PmMcpDeps) -> Self {
        Self { deps }
    }

    pub fn tools(&self) -> Vec<ToolDefinition> {
        tool_definitions()
    }

    pub async fn call(&self, name: &str, args: Value) -> Result<Value, McpHandlerError> {
        match name {
            "get_issue" => {
                let pm = self.require_pm("No issue tracker configured")?;
                text_json(get_issue(pm, args).await?, |value| value.to_string())
            }
            "list_issues" => {
                let pm = self.require_pm("No issue tracker configured")?;
                text_json(list_issues(pm, args).await?, |value| format!("{value:?}"))
            }
            "update_issue" => {
                let pm = self.require_pm("No issue tracker configured")?;
                text_json(update_issue(pm, args).await?, |value| value.to_string())
            }
            "create_issue" => self.handle_create_issue(args).await,
            "add_dependency" => self.handle_add_dependency(args).await,
            "create_pr" => self.handle_create_pr(args).await,
            "graph_triage" => self.handle_graph_triage(args).await,
            "graph_plan" => self.handle_graph_plan(args).await,
            "graph_insights" => self.handle_graph_insights(args).await,
            "graph_alerts" => self.handle_graph_alerts().await,
            "graph_subgraph" => self.handle_graph_subgraph(args).await,
            other => Err(McpHandlerError::InvalidParams(format!(
                "unknown PM MCP tool: {other}"
            ))),
        }
    }

    fn require_pm(&self, message: &'static str) -> Result<&PmService, McpHandlerError> {
        self.deps
            .pm_service
            .as_deref()
            .ok_or_else(|| McpHandlerError::Internal(message.to_string()))
    }

    fn require_analyzer(&self) -> Result<&crate::BvAdapter, McpHandlerError> {
        let pm = self.require_pm("No PM service configured")?;
        pm.analyzer().ok_or_else(|| {
            McpHandlerError::Internal(
                "Graph analysis not available (beads database unavailable)".to_string(),
            )
        })
    }

    async fn handle_create_issue(&self, args: Value) -> Result<Value, McpHandlerError> {
        let pm = self.require_pm("No issue tracker configured")?;
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| {
                McpHandlerError::InvalidParams("Missing required field 'title'".to_string())
            })?;

        let labels: Vec<String> = args
            .get("labels")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        let depends_on: Vec<String> = args
            .get("depends_on")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();

        let params = IssueCreate {
            title,
            description: args
                .get("description")
                .and_then(Value::as_str)
                .map(String::from),
            issue_type: args.get("type").and_then(Value::as_str).map(String::from),
            priority: args
                .get("priority")
                .and_then(Value::as_i64)
                .map(|number| number as i32),
            labels,
            parent: args.get("parent").and_then(Value::as_str).map(String::from),
            assignee: args
                .get("assignee")
                .and_then(Value::as_str)
                .map(String::from),
            estimate_minutes: args
                .get("estimate")
                .and_then(Value::as_u64)
                .map(|number| number as u32),
            depends_on,
            external_ref: None,
            source_system: None,
            source_repo: None,
        };

        let issue_id = pm
            .create_issue(params)
            .await
            .map_err(|error| McpHandlerError::Internal(format!("create_issue failed: {error}")))?;

        if let Some(on_issue_created) = self.deps.on_issue_created.as_ref() {
            match pm.get_issue(&issue_id).await {
                Ok(issue) => on_issue_created(IssueCreatedEvent {
                    issue,
                    source: pm.source_str(),
                }),
                Err(error) => {
                    tracing::warn!(
                        issue_id = %issue_id,
                        %error,
                        "created issue event not emitted because fetching issue failed"
                    );
                }
            }
        }

        Ok(text_content(format!("Issue created: {issue_id}")))
    }

    async fn handle_add_dependency(&self, args: Value) -> Result<Value, McpHandlerError> {
        let pm = self.require_pm("No issue tracker configured")?;
        let issue_id = required_string(&args, "issue_id")?;
        let depends_on_id = required_string(&args, "depends_on_id")?;

        pm.add_dependency(issue_id, depends_on_id)
            .await
            .map_err(|error| {
                McpHandlerError::Internal(format!("add_dependency failed: {error}"))
            })?;

        Ok(text_content(format!(
            "Dependency added: {issue_id} depends on {depends_on_id}"
        )))
    }

    async fn handle_create_pr(&self, args: Value) -> Result<Value, McpHandlerError> {
        let pm = self.require_pm("No PR service configured")?;
        let title = required_string(&args, "title")?.to_string();
        let body = required_string(&args, "body")?.to_string();
        let head_branch = required_string(&args, "branch")?.to_string();

        let params = PrParams {
            title,
            body,
            head_branch,
            base_branch: args
                .get("base_branch")
                .and_then(Value::as_str)
                .map(String::from),
            repo: args.get("repo").and_then(Value::as_str).map(String::from),
        };

        let url = pm
            .create_pr(params)
            .await
            .map_err(|error| McpHandlerError::Internal(format!("create_pr failed: {error}")))?;

        Ok(text_content(format!("PR created: {url}")))
    }

    async fn handle_graph_triage(&self, args: Value) -> Result<Value, McpHandlerError> {
        let analyzer = self.require_analyzer()?;
        let label = args.get("label").and_then(Value::as_str);
        match analyzer.triage(label).await {
            Ok(report) => pretty_raw_content(report.raw),
            Err(error) => Err(McpHandlerError::Internal(format!(
                "graph_triage failed: {error}"
            ))),
        }
    }

    async fn handle_graph_plan(&self, args: Value) -> Result<Value, McpHandlerError> {
        let analyzer = self.require_analyzer()?;
        let label = args.get("label").and_then(Value::as_str);
        match analyzer.plan(label).await {
            Ok(report) => pretty_raw_content(report.raw),
            Err(error) => Err(McpHandlerError::Internal(format!(
                "graph_plan failed: {error}"
            ))),
        }
    }

    async fn handle_graph_insights(&self, args: Value) -> Result<Value, McpHandlerError> {
        let analyzer = self.require_analyzer()?;
        let label = args.get("label").and_then(Value::as_str);
        match analyzer.insights(label).await {
            Ok(report) => pretty_raw_content(report.raw),
            Err(error) => Err(McpHandlerError::Internal(format!(
                "graph_insights failed: {error}"
            ))),
        }
    }

    async fn handle_graph_alerts(&self) -> Result<Value, McpHandlerError> {
        let analyzer = self.require_analyzer()?;
        match analyzer.alerts().await {
            Ok(report) => pretty_raw_content(report.raw),
            Err(error) => Err(McpHandlerError::Internal(format!(
                "graph_alerts failed: {error}"
            ))),
        }
    }

    async fn handle_graph_subgraph(&self, args: Value) -> Result<Value, McpHandlerError> {
        let analyzer = self.require_analyzer()?;
        let root_id = args.get("root_id").and_then(Value::as_str).ok_or_else(|| {
            McpHandlerError::InvalidParams("Missing required field 'root_id'".to_string())
        })?;
        let depth = args
            .get("depth")
            .and_then(Value::as_u64)
            .map(|depth| depth as u32);
        let format = args.get("format").and_then(Value::as_str);

        match analyzer.subgraph(root_id, depth, format).await {
            Ok(report) => pretty_raw_content(report.raw),
            Err(error) => Err(McpHandlerError::Internal(format!(
                "graph_subgraph failed: {error}"
            ))),
        }
    }
}

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        get_issue_def(),
        list_issues_def(),
        update_issue_def(),
        create_issue_def(),
        add_dependency_def(),
        create_pr_def(),
        graph_triage_def(),
        graph_plan_def(),
        graph_insights_def(),
        graph_alerts_def(),
        graph_subgraph_def(),
    ]
}

pub async fn get_issue(pm: &PmService, args: Value) -> Result<Value, McpHandlerError> {
    let issue_id = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| McpHandlerError::InvalidParams("missing required field 'id'".into()))?;

    let issue = pm
        .get_issue(issue_id)
        .await
        .map_err(|error| McpHandlerError::UpstreamPm(format!("{error}")))?;

    serde_json::to_value(issue)
        .map_err(|error| McpHandlerError::Internal(format!("failed to serialize issue: {error}")))
}

pub async fn list_issues(pm: &PmService, args: Value) -> Result<Value, McpHandlerError> {
    let labels: Vec<String> = args
        .get("labels")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();

    let filter = IssueFilter {
        status: args.get("status").and_then(Value::as_str).map(String::from),
        assignee: args
            .get("assignee")
            .and_then(Value::as_str)
            .map(String::from),
        priority_min: args
            .get("priority_min")
            .and_then(Value::as_i64)
            .map(|number| number as i32),
        priority_max: args
            .get("priority_max")
            .and_then(Value::as_i64)
            .map(|number| number as i32),
        issue_type: args
            .get("issue_type")
            .and_then(Value::as_str)
            .map(String::from),
        text_search: args
            .get("text_search")
            .and_then(Value::as_str)
            .map(String::from),
        limit: Some(
            args.get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .min(100) as usize,
        ),
        offset: None,
        labels,
        since: None,
        include_closed: false,
    };

    let issues = pm
        .list_issues(filter)
        .await
        .map_err(|error| McpHandlerError::UpstreamPm(format!("{error}")))?;

    serde_json::to_value(issues)
        .map_err(|error| McpHandlerError::Internal(format!("failed to serialize issues: {error}")))
}

pub async fn update_issue(pm: &PmService, args: Value) -> Result<Value, McpHandlerError> {
    let id = args
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| McpHandlerError::InvalidParams("missing 'id'".into()))?;

    let comment = args
        .get("comment")
        .and_then(Value::as_str)
        .map(String::from);

    let add_labels: Vec<String> = args
        .get("add_labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let remove_labels: Vec<String> = args
        .get("remove_labels")
        .and_then(Value::as_array)
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let status = args.get("status").and_then(Value::as_str).map(String::from);
    let priority = args
        .get("priority")
        .and_then(Value::as_i64)
        .map(|number| number as i32);
    let assignee = args
        .get("assignee")
        .and_then(Value::as_str)
        .map(String::from);

    let update = IssueUpdate {
        status,
        comment,
        add_labels,
        remove_labels,
        priority,
        assignee,
        body: None,
        external_ref: None,
        source_system: None,
        source_repo: None,
    };

    pm.update_issue(id, update)
        .await
        .map_err(|error| McpHandlerError::UpstreamPm(format!("{error}")))?;

    Ok(json!({ "ok": true }))
}

fn text_json(
    value: Value,
    fallback: impl FnOnce(&Value) -> String,
) -> Result<Value, McpHandlerError> {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| fallback(&value));
    Ok(text_content(text))
}

fn pretty_raw_content(raw: Value) -> Result<Value, McpHandlerError> {
    let text = serde_json::to_string_pretty(&raw).unwrap_or_else(|_| raw.to_string());
    Ok(text_content(text))
}

fn text_content(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn required_string<'a>(args: &'a Value, field: &str) -> Result<&'a str, McpHandlerError> {
    args.get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| McpHandlerError::InvalidParams(format!("Missing required field '{field}'")))
}

fn get_issue_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_issue".into(),
        description: "Retrieve an issue from the configured project management backend (beads, GitHub, etc.)."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
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
        description:
            "List issues from the configured project management backend with optional filters."
                .into(),
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
                },
                "base_branch": {
                    "type": "string",
                    "description": "Base branch to merge into (optional, defaults to repo default)"
                },
                "repo": {
                    "type": "string",
                    "description": "Repository identifier (optional, for multi-repo setups)"
                }
            },
            "required": ["title", "body", "branch"]
        }),
    }
}

fn graph_triage_def() -> ToolDefinition {
    ToolDefinition {
        name: "graph_triage".into(),
        description: "Get PageRank-weighted project triage: top recommendations, quick wins, blockers to clear, and project health metrics. Complements list_issues (CRUD) with graph-based dependency analysis. Call this FIRST for orientation before starting work. Note: triage includes alert data — only call graph_alerts separately when you need standalone alert monitoring without the full triage context. Optionally scope to a label.".into(),
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
                    "description": "Traversal depth in hops. Defaults to 2 when omitted. Larger values fan out further; `0` returns only the seed node."
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

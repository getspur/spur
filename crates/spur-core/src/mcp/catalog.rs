use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde_json::Value;

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

const ANALYST_TOOL_NAMES: &[&str] = &[
    "doc_navigate",
    "knowledge_context_pack",
    "knowledge_context_pack_2",
];

pub(crate) fn is_server_owned_tool(name: &str) -> bool {
    is_pm_tool(name)
        || crate::mcp::plan::is_plan_tool(name)
        || is_graph_tool(name)
        || is_analyst_tool(name)
}

pub(crate) fn is_pm_tool(name: &str) -> bool {
    PM_CRUD_TOOL_NAMES.contains(&name) || PM_ISSUE_GRAPH_TOOL_NAMES.contains(&name)
}

pub(crate) fn is_graph_tool(name: &str) -> bool {
    spur_graph::mcp::tool_definitions()
        .iter()
        .any(|definition| definition.name == name)
}

pub(crate) fn is_analyst_tool(name: &str) -> bool {
    ANALYST_TOOL_NAMES.contains(&name)
}

#[derive(Debug, Clone, Copy)]
enum ServerCatalogSection {
    Prelude,
    Remainder,
}

#[derive(Debug, Clone, Copy)]
enum WorkerCatalogSection {
    Prelude,
    Remainder,
}

pub(crate) struct ServerCatalogMcpModule {
    section: ServerCatalogSection,
}

impl ServerCatalogMcpModule {
    pub(crate) fn prelude() -> Self {
        Self {
            section: ServerCatalogSection::Prelude,
        }
    }

    pub(crate) fn remainder() -> Self {
        Self {
            section: ServerCatalogSection::Remainder,
        }
    }
}

pub(crate) struct WorkerCatalogMcpModule {
    section: WorkerCatalogSection,
}

impl WorkerCatalogMcpModule {
    pub(crate) fn prelude() -> Self {
        Self {
            section: WorkerCatalogSection::Prelude,
        }
    }

    pub(crate) fn remainder() -> Self {
        Self {
            section: WorkerCatalogSection::Remainder,
        }
    }
}

#[async_trait]
impl ToolModule for ServerCatalogMcpModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        match self.section {
            ServerCatalogSection::Prelude => server_prelude_tool_definitions(),
            ServerCatalogSection::Remainder => server_remainder_tool_definitions(),
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
            format!("server-owned tool {name} must be dispatched by spur-core"),
            None,
        ))
    }
}

#[async_trait]
impl ToolModule for WorkerCatalogMcpModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        match self.section {
            WorkerCatalogSection::Prelude => worker_prelude_tool_definitions(),
            WorkerCatalogSection::Remainder => worker_remainder_tool_definitions(),
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
            format!("worker-owned tool {name} must be dispatched by spur-core"),
            None,
        ))
    }
}

fn server_prelude_tool_definitions() -> Vec<ToolDefinition> {
    pm_tool_definitions_by_names(PM_CRUD_TOOL_NAMES)
}

fn server_remainder_tool_definitions() -> Vec<ToolDefinition> {
    let mut definitions = Vec::new();
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
    definitions
}

fn worker_prelude_tool_definitions() -> Vec<ToolDefinition> {
    pm_tool_definitions_by_names(&["get_issue", "list_issues"])
}

fn worker_remainder_tool_definitions() -> Vec<ToolDefinition> {
    let mut definitions = Vec::new();
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

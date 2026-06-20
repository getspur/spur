use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde_json::Value;
use thiserror::Error;

use crate::server::types::JsonRpcResponse;
use crate::tools::ToolDefinition;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerKind {
    Brain,
    Worker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAuthority {
    Brain,
    Worker,
}

pub struct ToolCallContext<'a> {
    pub server_kind: ServerKind,
    pub authority: ToolAuthority,
    pub brain_session_id: Option<&'a spur_acp::BrainSessionId>,
    pub request_id: Option<&'a Value>,
    pub(crate) callback_server: Option<&'a crate::server::McpCallbackServer>,
}

impl<'a> ToolCallContext<'a> {
    pub fn new(
        server_kind: ServerKind,
        authority: ToolAuthority,
        brain_session_id: Option<&'a spur_acp::BrainSessionId>,
        request_id: Option<&'a Value>,
    ) -> Self {
        Self {
            server_kind,
            authority,
            brain_session_id,
            request_id,
            callback_server: None,
        }
    }

    pub(crate) fn brain_server(
        server: &'a crate::server::McpCallbackServer,
        request_id: &'a Value,
    ) -> Self {
        Self {
            server_kind: ServerKind::Brain,
            authority: ToolAuthority::Brain,
            brain_session_id: server.brain_session_id.get(),
            request_id: Some(request_id),
            callback_server: Some(server),
        }
    }

    pub(crate) fn request_id_value(&self) -> Value {
        self.request_id.cloned().unwrap_or(Value::Null)
    }

    pub(crate) fn callback_server(&self) -> Result<&'a crate::server::McpCallbackServer, McpError> {
        self.callback_server.ok_or_else(|| {
            McpError::internal_error("legacy MCP tool requires callback server context", None)
        })
    }
}

pub struct ToolResponse {
    envelope: JsonRpcResponse,
}

impl ToolResponse {
    pub fn json(result: Value) -> Self {
        Self {
            envelope: JsonRpcResponse::success(Value::Null, result),
        }
    }

    pub(crate) fn from_json_rpc(envelope: JsonRpcResponse) -> Self {
        Self { envelope }
    }

    pub(crate) fn into_json_rpc(self) -> JsonRpcResponse {
        self.envelope
    }
}

#[async_trait]
pub trait ToolModule: Send + Sync + 'static {
    fn tools(&self) -> Vec<ToolDefinition>;

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError>;
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ToolRegistryError {
    #[error("duplicate tool name: {name}")]
    DuplicateToolName { name: String },
    #[error("duplicate tool alias: {alias}")]
    DuplicateToolAlias { alias: String },
    #[error("tool alias `{alias}` targets unknown tool `{target}`")]
    UnknownAliasTarget { alias: String, target: String },
}

struct RegisteredTool {
    definition: ToolDefinition,
    module_index: usize,
}

pub struct ToolRegistry {
    modules: Vec<Box<dyn ToolModule>>,
    tools: Vec<RegisteredTool>,
    tool_indices: HashMap<String, usize>,
    aliases: HashMap<String, String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            modules: Vec::new(),
            tools: Vec::new(),
            tool_indices: HashMap::new(),
            aliases: HashMap::new(),
        }
    }

    pub fn builder() -> ToolRegistryBuilder {
        ToolRegistryBuilder::new()
    }

    pub fn register<M: ToolModule>(&mut self, module: M) -> Result<(), ToolRegistryError> {
        let definitions = module.tools();
        let mut local_names = HashSet::new();
        for definition in &definitions {
            if !local_names.insert(definition.name.clone())
                || self.tool_indices.contains_key(&definition.name)
                || self.aliases.contains_key(&definition.name)
            {
                return Err(ToolRegistryError::DuplicateToolName {
                    name: definition.name.clone(),
                });
            }
        }

        let module_index = self.modules.len();
        for definition in definitions {
            let tool_index = self.tools.len();
            self.tool_indices
                .insert(definition.name.clone(), tool_index);
            self.tools.push(RegisteredTool {
                definition,
                module_index,
            });
        }
        self.modules.push(Box::new(module));
        Ok(())
    }

    pub fn register_alias(
        &mut self,
        alias: impl Into<String>,
        target: impl Into<String>,
    ) -> Result<(), ToolRegistryError> {
        let alias = alias.into();
        let target = target.into();
        if self.tool_indices.contains_key(&alias) {
            return Err(ToolRegistryError::DuplicateToolName { name: alias });
        }
        if self.aliases.contains_key(&alias) {
            return Err(ToolRegistryError::DuplicateToolAlias { alias });
        }
        if !self.tool_indices.contains_key(&target) {
            return Err(ToolRegistryError::UnknownAliasTarget { alias, target });
        }

        self.aliases.insert(alias, target);
        Ok(())
    }

    pub fn list_tools(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .map(|entry| entry.definition.clone())
            .collect()
    }

    pub fn canonical_name<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        if self.tool_indices.contains_key(name) {
            return Some(name);
        }
        self.aliases.get(name).map(String::as_str)
    }

    pub async fn call_tool(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        let canonical_name = self.canonical_name(name).ok_or_else(|| {
            McpError::new(ErrorCode(-32601), format!("Unknown tool: {name}"), None)
        })?;
        let tool_index = self
            .tool_indices
            .get(canonical_name)
            .expect("canonical tool name must have a registry entry");
        let entry = &self.tools[*tool_index];
        self.modules[entry.module_index]
            .call(ctx, canonical_name, args)
            .await
    }

    pub(crate) async fn call_json_tool(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> JsonRpcResponse {
        let id = ctx.request_id_value();
        match self.call_tool(ctx, name, args).await {
            Ok(response) => response.into_json_rpc(),
            Err(error) => JsonRpcResponse::mcp_error(id, error),
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct ToolRegistryBuilder {
    registry: ToolRegistry,
}

impl ToolRegistryBuilder {
    fn new() -> Self {
        Self {
            registry: ToolRegistry::new(),
        }
    }

    pub fn with<M: ToolModule>(mut self, module: M) -> Result<Self, ToolRegistryError> {
        self.registry.register(module)?;
        Ok(self)
    }

    pub fn with_alias(
        mut self,
        alias: impl Into<String>,
        target: impl Into<String>,
    ) -> Result<Self, ToolRegistryError> {
        self.registry.register_alias(alias, target)?;
        Ok(self)
    }

    pub fn build(self) -> ToolRegistry {
        self.registry
    }
}

/// Future spur-core-owned MCP handles.
///
/// Owns active plans, plan registry, plan ownership locks, reconciler
/// handles/outcomes, delegation lifecycle state, continuation context, and
/// worker-signal lifecycle state currently stored on `McpCallbackServer`.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CoreMcpDeps {}

/// Future worker-server handles for authenticated worker calls and progress.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct WorkerMcpDeps {}

/// Future project-management handles for issue and PR tools.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct PmMcpDeps {}

pub use spur_graph::mcp::GraphMcpDeps;

/// Future analyst handles for DuckDB-backed evidence and graph reasoning.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AnalystMcpDeps {}

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

struct BrainPmMcpToolModule {
    tool_names: &'static [&'static str],
}

impl BrainPmMcpToolModule {
    fn crud() -> Self {
        Self {
            tool_names: PM_CRUD_TOOL_NAMES,
        }
    }

    fn issue_graph() -> Self {
        Self {
            tool_names: PM_ISSUE_GRAPH_TOOL_NAMES,
        }
    }

    fn deps_from_server(server: &crate::server::McpCallbackServer) -> spur_pm::mcp::PmMcpDeps {
        let event_sink = server.event_sink.clone();
        let on_issue_created = event_sink.map(|sink| {
            Arc::new(move |event: spur_pm::mcp::IssueCreatedEvent| {
                let issue = issue_to_summary_event(&event.issue, event.source);
                if sink
                    .try_emit(spur_acp::SpurEventBody::IssueCreated { issue })
                    .is_err()
                {
                    tracing::warn!(
                        issue_id = %event.issue.id,
                        "dropping IssueCreated event because broadcast sink is full"
                    );
                }
            }) as Arc<dyn Fn(spur_pm::mcp::IssueCreatedEvent) + Send + Sync>
        });

        spur_pm::mcp::PmMcpDeps {
            pm_service: server.pm_service.clone(),
            on_issue_created,
        }
    }
}

#[async_trait]
impl ToolModule for BrainPmMcpToolModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        let definitions = spur_pm::mcp::tool_definitions();
        self.tool_names
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

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        let id = ctx.request_id_value();
        let server = ctx.callback_server()?;
        let module = spur_pm::mcp::PmMcpModule::new(Self::deps_from_server(server));
        let response = match module.call(name, args).await {
            Ok(result) => JsonRpcResponse::success(id, result),
            Err(error) => pm_error_response(id, name, error),
        };
        Ok(ToolResponse::from_json_rpc(response))
    }
}

fn pm_tool_definition(definition: spur_pm::mcp::ToolDefinition) -> ToolDefinition {
    ToolDefinition {
        name: definition.name,
        description: definition.description,
        input_schema: definition.input_schema,
    }
}

fn pm_error_response(
    id: Value,
    tool_name: &str,
    error: spur_pm::mcp::McpHandlerError,
) -> JsonRpcResponse {
    match error {
        spur_pm::mcp::McpHandlerError::InvalidParams(message) => {
            JsonRpcResponse::invalid_params(id, message)
        }
        spur_pm::mcp::McpHandlerError::NotFound(message) => {
            JsonRpcResponse::error(id, -32004, message)
        }
        spur_pm::mcp::McpHandlerError::Unauthorized(message) => {
            JsonRpcResponse::error(id, -32001, message)
        }
        spur_pm::mcp::McpHandlerError::UpstreamPm(message) => {
            JsonRpcResponse::internal_error(id, format!("{tool_name} failed: {message}"))
        }
        spur_pm::mcp::McpHandlerError::Internal(message) => {
            JsonRpcResponse::internal_error(id, message)
        }
    }
}

fn issue_to_summary_event(
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

struct GraphMcpToolModule;

#[async_trait]
impl ToolModule for GraphMcpToolModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        spur_graph::mcp::tool_definitions()
            .into_iter()
            .map(graph_tool_definition)
            .collect()
    }

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        let id = ctx.request_id_value();
        let server = ctx.callback_server()?;
        let module = spur_graph::mcp::GraphMcpModule::new(server.graph_mcp_deps.clone());
        Ok(ToolResponse::from_json_rpc(
            graph_response(id, module.call(name, args).await).await,
        ))
    }
}

fn graph_tool_definition(definition: spur_graph::mcp::ToolDefinition) -> ToolDefinition {
    ToolDefinition {
        name: definition.name,
        description: definition.description,
        input_schema: definition.input_schema,
    }
}

async fn graph_response(id: Value, result: spur_graph::mcp::CodeGraphResult) -> JsonRpcResponse {
    match result {
        Ok(body) => {
            let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
            JsonRpcResponse::success(
                id,
                serde_json::json!({ "content": [{ "type": "text", "text": text }] }),
            )
        }
        Err(error) => {
            let error = error.into_error_response().await;
            match error.data {
                Some(data) => JsonRpcResponse::error_with_data(id, error.code, error.message, data),
                None => JsonRpcResponse::error(id, error.code, error.message),
            }
        }
    }
}

struct AnalystMcpToolModule;

impl AnalystMcpToolModule {
    fn read_only() -> Self {
        Self
    }
}

#[async_trait]
impl ToolModule for AnalystMcpToolModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        spur_analyst::mcp::tool_definitions()
            .into_iter()
            .map(analyst_tool_definition)
            .collect()
    }

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        let id = ctx.request_id_value();
        let module = spur_analyst::mcp::AnalystMcpModule::new();
        Ok(ToolResponse::from_json_rpc(analyst_response(
            id,
            module.call(name, args).await,
        )))
    }
}

fn analyst_tool_definition(definition: spur_analyst::mcp::ToolDefinition) -> ToolDefinition {
    ToolDefinition {
        name: definition.name,
        description: definition.description,
        input_schema: definition.input_schema,
    }
}

fn analyst_response(
    id: Value,
    result: Result<Value, spur_analyst::mcp::McpHandlerError>,
) -> JsonRpcResponse {
    match result {
        Ok(body) => {
            let text = serde_json::to_string_pretty(&body).unwrap_or_else(|_| body.to_string());
            JsonRpcResponse::success(
                id,
                serde_json::json!({ "content": [{ "type": "text", "text": text }] }),
            )
        }
        Err(spur_analyst::mcp::McpHandlerError::InvalidParams(message)) => {
            JsonRpcResponse::invalid_params(id, message)
        }
        Err(spur_analyst::mcp::McpHandlerError::NotFound(message)) => {
            JsonRpcResponse::error(id, -32004, message)
        }
        Err(error) => JsonRpcResponse::internal_error(id, error.to_string()),
    }
}

struct LegacyMcpToolModule {
    definitions: Vec<ToolDefinition>,
}

impl LegacyMcpToolModule {
    fn full_prelude() -> Self {
        Self {
            definitions: crate::tools::legacy_prelude_tool_definitions(),
        }
    }

    fn plan_management() -> Self {
        Self {
            definitions: crate::tools::legacy_plan_management_tool_definitions(),
        }
    }

    fn full_remainder() -> Self {
        Self {
            definitions: crate::tools::legacy_remainder_tool_definitions(),
        }
    }

    fn worker_prelude() -> Self {
        Self {
            definitions: crate::tools::legacy_worker_prelude_tool_definitions(),
        }
    }

    fn worker_remainder() -> Self {
        Self {
            definitions: crate::tools::legacy_worker_remainder_tool_definitions(),
        }
    }
}

#[async_trait]
impl ToolModule for LegacyMcpToolModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        self.definitions.clone()
    }

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        let server = ctx.callback_server()?;
        Ok(ToolResponse::from_json_rpc(
            server.handle_registered_tool_call(ctx, name, args).await,
        ))
    }
}

pub fn default_tool_registry() -> Result<&'static ToolRegistry, ToolRegistryError> {
    static REGISTRY: OnceLock<Result<ToolRegistry, ToolRegistryError>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            ToolRegistry::builder()
                .with(LegacyMcpToolModule::full_prelude())?
                .with(BrainPmMcpToolModule::crud())?
                .with(LegacyMcpToolModule::plan_management())?
                .with(BrainPmMcpToolModule::issue_graph())?
                .with(GraphMcpToolModule)?
                .with(AnalystMcpToolModule)?
                .with(LegacyMcpToolModule::full_remainder())?
                .with_alias("code_search", "code_symbol_search")
                .map(ToolRegistryBuilder::build)
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub fn default_worker_tool_registry() -> Result<&'static ToolRegistry, ToolRegistryError> {
    static REGISTRY: OnceLock<Result<ToolRegistry, ToolRegistryError>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            ToolRegistry::builder()
                .with(LegacyMcpToolModule::worker_prelude())?
                .with(GraphMcpToolModule)?
                .with(AnalystMcpToolModule::read_only())?
                .with(LegacyMcpToolModule::worker_remainder())?
                .with_alias("code_search", "code_symbol_search")
                .map(ToolRegistryBuilder::build)
        })
        .as_ref()
        .map_err(Clone::clone)
}

#[cfg(test)]
mod analyst_module_ownership_tests {
    #[test]
    fn default_registries_compose_analyst_module_explicitly() {
        let source = include_str!("registry.rs");
        assert!(
            source.contains(".with(AnalystMcpToolModule)?"),
            "brain registry must compose analyst-owned tools through AnalystMcpToolModule"
        );
        assert!(
            source.contains(".with(AnalystMcpToolModule::read_only())?"),
            "worker registry must compose analyst-owned tools through AnalystMcpToolModule"
        );
    }
}

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde_json::Value;
use thiserror::Error;

use crate::response::JsonRpcResponse;
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
        }
    }

    pub fn request_id_value(&self) -> Value {
        self.request_id.cloned().unwrap_or(Value::Null)
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

    pub fn json_text(id: Value, value: Value) -> Self {
        let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
        Self {
            envelope: JsonRpcResponse::success(
                id,
                serde_json::json!({ "content": [{ "type": "text", "text": text }] }),
            ),
        }
    }

    pub fn from_json_rpc(envelope: JsonRpcResponse) -> Self {
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
    denied_tool_calls: HashSet<String>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            modules: Vec::new(),
            tools: Vec::new(),
            tool_indices: HashMap::new(),
            aliases: HashMap::new(),
            denied_tool_calls: HashSet::new(),
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

    pub fn deny_tool_calls<I, S>(&mut self, names: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.denied_tool_calls
            .extend(names.into_iter().map(Into::into));
    }

    pub fn list_tools(&self) -> Vec<ToolDefinition> {
        self.tools
            .iter()
            .filter(|entry| !self.denied_tool_calls.contains(&entry.definition.name))
            .map(|entry| entry.definition.clone())
            .collect()
    }

    pub fn canonical_name<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        if self.tool_indices.contains_key(name) {
            return Some(name);
        }
        self.aliases.get(name).map(String::as_str)
    }

    pub fn tool_definition_for_call(&self, name: &str) -> Option<ToolDefinition> {
        let canonical_name = self.canonical_name(name)?;
        if self.denied_tool_calls.contains(canonical_name) || self.denied_tool_calls.contains(name)
        {
            return None;
        }
        self.tool_indices
            .get(canonical_name)
            .map(|index| self.tools[*index].definition.clone())
    }

    pub fn canonical_name_for_call<'a>(&'a self, name: &'a str) -> Result<&'a str, McpError> {
        if self.denied_tool_calls.contains(name) {
            return Err(worker_tool_authorization_error(name));
        }
        let canonical_name = self.canonical_name(name).ok_or_else(|| {
            McpError::new(ErrorCode(-32601), format!("Unknown tool: {name}"), None)
        })?;
        if self.denied_tool_calls.contains(canonical_name) {
            return Err(worker_tool_authorization_error(name));
        }
        Ok(canonical_name)
    }

    pub async fn call_tool(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        let canonical_name = self.canonical_name_for_call(name)?;
        let tool_index = self
            .tool_indices
            .get(canonical_name)
            .expect("canonical tool name must have a registry entry");
        let entry = &self.tools[*tool_index];
        self.modules[entry.module_index]
            .call(ctx, canonical_name, args)
            .await
    }

    pub async fn call_json_tool(
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

    pub fn with_denied_tool_calls<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.registry.deny_tool_calls(names);
        self
    }

    pub fn build(self) -> ToolRegistry {
        self.registry
    }
}

fn worker_tool_authorization_error(name: &str) -> McpError {
    McpError::new(
        ErrorCode(-32001),
        format!("worker is not authorized to call restricted tool: {name}"),
        None,
    )
}

/// Future spur-core-owned MCP handles.
///
/// Owns active plans, plan registry, plan ownership locks, reconciler
/// handles/outcomes, delegation lifecycle state, continuation context, and
/// worker-signal lifecycle state.
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

const WORKER_DENIED_TOOL_CALLS: &[&str] = &[
    "delegate_to_worker",
    "delegate_parallel",
    "check_delegation_status",
    "cancel_delegation",
    "list_available_workers",
    "update_issue",
    "create_issue",
    "add_dependency",
    "create_pr",
    "merge_plan",
    "resume_plan",
    "force_reclaim_plan",
    "submit_plan",
    "execute_epic",
    "get_reconciler_status",
    "preview_task_base",
    "plan_truncate_and_restart",
    "recover_orphaned_dispatch",
    "review_task",
    "submit_plan_mutation",
    "graph_triage",
    "graph_plan",
    "graph_insights",
    "graph_alerts",
    "graph_subgraph",
];

pub fn default_tool_registry() -> Result<&'static ToolRegistry, ToolRegistryError> {
    static REGISTRY: OnceLock<Result<ToolRegistry, ToolRegistryError>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Ok(ToolRegistry::new()))
        .as_ref()
        .map_err(Clone::clone)
}

pub fn legacy_brain_tool_registry_builder() -> Result<ToolRegistryBuilder, ToolRegistryError> {
    Ok(ToolRegistry::builder())
}

pub fn legacy_brain_tool_registry_builder_from(
    builder: ToolRegistryBuilder,
) -> Result<ToolRegistryBuilder, ToolRegistryError> {
    Ok(builder)
}

pub fn legacy_brain_tool_registry() -> Result<ToolRegistry, ToolRegistryError> {
    legacy_brain_tool_registry_builder().map(ToolRegistryBuilder::build)
}

pub fn default_worker_tool_registry() -> Result<&'static ToolRegistry, ToolRegistryError> {
    static REGISTRY: OnceLock<Result<ToolRegistry, ToolRegistryError>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            Ok(ToolRegistry::builder()
                .with_denied_tool_calls(WORKER_DENIED_TOOL_CALLS.iter().copied())
                .build())
        })
        .as_ref()
        .map_err(Clone::clone)
}

#[cfg(test)]
mod analyst_module_ownership_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_registries_are_infrastructure_empty_after_core_extraction() {
        let brain = default_tool_registry().expect("default brain registry");
        let worker = default_worker_tool_registry().expect("default worker registry");

        assert!(brain.list_tools().is_empty());
        assert!(worker.list_tools().is_empty());
    }

    #[tokio::test]
    async fn worker_registry_denies_brain_only_tool_calls_with_authorization_error() {
        let registry = default_worker_tool_registry().expect("default worker registry");
        let names: Vec<String> = registry
            .list_tools()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();

        for tool_name in [
            "submit_plan",
            "review_task",
            "delegate_to_worker",
            "delegate_parallel",
            "execute_epic",
        ] {
            assert!(
                !names.iter().any(|name| name == tool_name),
                "{tool_name} must not be advertised to workers"
            );

            let ctx = ToolCallContext::new(ServerKind::Worker, ToolAuthority::Worker, None, None);
            let Err(err) = registry.call_tool(ctx, tool_name, json!({})).await else {
                panic!("brain-only worker call must be rejected: {tool_name}");
            };

            assert_eq!(
                err.code,
                ErrorCode(-32001),
                "{tool_name} should fail authorization, not tool lookup"
            );
            assert!(
                err.message.contains("not authorized"),
                "{tool_name} denial should explain authorization: {}",
                err.message
            );
        }
    }
}

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

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

/// Future code-graph handles for symbol lookup, graph traversal, and rebuilds.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct GraphMcpDeps {}

/// Future analyst handles for DuckDB-backed evidence and graph reasoning.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct AnalystMcpDeps {}

struct LegacyMcpToolModule {
    definitions: Vec<ToolDefinition>,
}

impl LegacyMcpToolModule {
    fn full() -> Self {
        Self {
            definitions: crate::tools::legacy_tools_definitions(),
        }
    }

    fn worker() -> Self {
        Self {
            definitions: crate::tools::legacy_worker_tool_definitions(),
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
                .with(LegacyMcpToolModule::full())?
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
                .with(LegacyMcpToolModule::worker())?
                .with_alias("code_search", "code_symbol_search")
                .map(ToolRegistryBuilder::build)
        })
        .as_ref()
        .map_err(Clone::clone)
}

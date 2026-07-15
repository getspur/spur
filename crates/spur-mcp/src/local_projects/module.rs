use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::registry::{ToolCallContext, ToolModule, ToolResponse};
use crate::tools::ToolDefinition;

use super::{
    validate_project_name, LocalProjectCatalogStore, LocalProjectError, LocalProjectListEntry,
    LocalProjectResolver, LocalProjectStatus, LocalProjectValidator,
};

/// Explicitly composed management module for the user-level catalog.
#[derive(Clone)]
pub struct LocalProjectCatalogMcpModule {
    store: LocalProjectCatalogStore,
    resolver: LocalProjectResolver,
    validator: Arc<dyn LocalProjectValidator>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddRequest {
    name: String,
    path: String,
    #[serde(default)]
    replace: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveRequest {
    name: String,
}

impl LocalProjectCatalogMcpModule {
    #[must_use]
    pub fn new(store: LocalProjectCatalogStore, validator: Arc<dyn LocalProjectValidator>) -> Self {
        let resolver = LocalProjectResolver::new(store.clone(), Arc::clone(&validator));
        Self {
            store,
            resolver,
            validator,
        }
    }

    #[must_use]
    pub fn resolver(&self) -> LocalProjectResolver {
        self.resolver.clone()
    }

    #[must_use]
    pub fn store(&self) -> LocalProjectCatalogStore {
        self.store.clone()
    }

    fn dispatch(&self, name: &str, args: Value) -> Result<Value, LocalProjectError> {
        match name {
            "local_project_add" => self.add(args),
            "local_project_list" => {
                parse_empty_object(&args)?;
                serde_json::to_value(self.resolver.list()?).map_err(|error| {
                    LocalProjectError::CatalogRead {
                        path: self
                            .store
                            .catalog_path()
                            .unwrap_or_else(|_| PathBuf::from("<unresolved>")),
                        reason: error.to_string(),
                    }
                })
            }
            "local_project_remove" => self.remove(args),
            other => Err(LocalProjectError::InvalidRequest {
                reason: format!("unknown local-project MCP tool `{other}`"),
            }),
        }
    }

    fn add(&self, args: Value) -> Result<Value, LocalProjectError> {
        let request: AddRequest = parse_request(args)?;
        validate_project_name(&request.name)?;
        let requested_path = PathBuf::from(&request.path);
        if !requested_path.is_absolute() || requested_path.to_str().is_none() {
            return Err(LocalProjectError::InvalidPath {
                path: requested_path,
                reason: "path must be absolute UTF-8".to_owned(),
            });
        }
        let validated = self.validator.validate(&requested_path)?;
        if !validated.health.is_ready() {
            return Err(LocalProjectError::ProjectUnavailable {
                name: request.name,
                reason: validated
                    .health
                    .reason
                    .unwrap_or_else(|| "graph or analyst index is unavailable".to_owned()),
            });
        }
        let result = self
            .store
            .add(&request.name, &validated.canonical_root, request.replace)?;
        Ok(json!({
            "changed": result.changed,
            "project": LocalProjectListEntry {
                name: result.project.name,
                root: result.project.root,
                status: LocalProjectStatus::Ready,
                reason: None,
            },
            "catalog_generation": result.catalog_generation,
        }))
    }

    fn remove(&self, args: Value) -> Result<Value, LocalProjectError> {
        let request: RemoveRequest = parse_request(args)?;
        serde_json::to_value(self.store.remove(&request.name)?).map_err(|error| {
            LocalProjectError::CatalogRead {
                path: self
                    .store
                    .catalog_path()
                    .unwrap_or_else(|_| PathBuf::from("<unresolved>")),
                reason: error.to_string(),
            }
        })
    }
}

fn parse_request<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, LocalProjectError> {
    serde_json::from_value(args).map_err(|error| LocalProjectError::InvalidRequest {
        reason: error.to_string(),
    })
}

fn parse_empty_object(args: &Value) -> Result<(), LocalProjectError> {
    if args.as_object().is_some_and(serde_json::Map::is_empty) {
        return Ok(());
    }
    Err(LocalProjectError::InvalidRequest {
        reason: "local_project_list expects an empty object".to_owned(),
    })
}

#[async_trait]
impl ToolModule for LocalProjectCatalogMcpModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        tool_definitions()
    }

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        self.dispatch(name, args)
            .map(|body| ToolResponse::json_text(ctx.request_id_value(), body))
            .map_err(|error| {
                McpError::new(ErrorCode(error.json_rpc_code()), error.to_string(), None)
            })
    }
}

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "local_project_add".to_owned(),
            description: "Register an already-indexed local Git project by stable name. Registration validates existing graph and analyst indexes but never builds them.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$"},
                    "path": {"type": "string", "description": "Absolute UTF-8 path inside the local Git worktree"},
                    "replace": {"type": "boolean", "default": false}
                },
                "required": ["name", "path"],
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "local_project_list".to_owned(),
            description: "List registered local projects with live graph and analyst readiness.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: "local_project_remove".to_owned(),
            description: "Remove a local-project registration without deleting repository or index data.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "pattern": "^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$"}
                },
                "required": ["name"],
                "additionalProperties": false
            }),
        },
    ]
}

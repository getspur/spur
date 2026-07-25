//! MCP tools for typed constraints, raw SMT-LIB2, and persisted solve lookup.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use spur_mcp::{ErrorCode, McpError, ToolCallContext, ToolDefinition, ToolModule, ToolResponse};

use crate::{
    persist::PersistError,
    service::{SolverService, SolverServiceError},
    types::{
        SolveConstraintsRequest, SolveSmtRequest, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS,
        MAX_TIMEOUT_MS, MAX_VARIABLES,
    },
};

const INVALID_PARAMS_CODE: i32 = -32602;
const METHOD_NOT_FOUND_CODE: i32 = -32601;
const RESOURCE_NOT_FOUND_CODE: i32 = -32004;

/// Thin MCP adapter around one shared [`SolverService`].
///
/// Live modules hold an [`Arc`] supplied by the host, so every registry in the
/// process shares the same concurrency semaphore and process runner. A
/// catalog-only module advertises the same schemas without constructing a live
/// solver service.
#[derive(Clone)]
pub struct SolverMcpModule {
    service: Option<Arc<SolverService>>,
}

impl SolverMcpModule {
    /// Creates a live solver module backed by `service`.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    ///
    /// use spur_mcp::ToolModule;
    /// use spur_solver::{mcp::SolverMcpModule, service::SolverService};
    ///
    /// let module = SolverMcpModule::new(Arc::new(SolverService::new()));
    /// assert_eq!(module.tools().len(), 3);
    /// ```
    #[must_use]
    pub const fn new(service: Arc<SolverService>) -> Self {
        Self {
            service: Some(service),
        }
    }

    /// Creates a list-only module that does not own a Z3 process service.
    #[must_use]
    pub const fn catalog_only() -> Self {
        Self { service: None }
    }

    fn live_service(&self, tool_name: &str) -> Result<&SolverService, McpError> {
        self.service.as_deref().ok_or_else(|| {
            McpError::internal_error(
                format!("catalog-only solver tool `{tool_name}` cannot be called"),
                None,
            )
        })
    }
}

#[async_trait]
impl ToolModule for SolverMcpModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        tool_definitions()
    }

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        let result = match name {
            "solve_constraints" => {
                let request = parse_request::<SolveConstraintsRequest>(name, args)?;
                let response = self
                    .live_service(name)?
                    .solve_constraints(request)
                    .await
                    .map_err(service_error)?;
                serialize_response(name, response)?
            }
            "solve_smt" => {
                let request = parse_request::<SolveSmtRequest>(name, args)?;
                let response = self
                    .live_service(name)?
                    .solve_smt(request)
                    .await
                    .map_err(service_error)?;
                serialize_response(name, response)?
            }
            "get_solve_result" => {
                let request = parse_request::<GetSolveResultRequest>(name, args)?;
                let response = self
                    .live_service(name)?
                    .get_solve_result(&request.solve_id)
                    .map_err(service_error)?;
                serialize_response(name, response)?
            }
            other => {
                return Err(McpError::new(
                    ErrorCode(METHOD_NOT_FOUND_CODE),
                    format!("Unknown tool: {other}"),
                    None,
                ));
            }
        };

        Ok(ToolResponse::json_text(ctx.request_id_value(), result))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetSolveResultRequest {
    solve_id: String,
}

fn parse_request<T: DeserializeOwned>(tool_name: &str, args: Value) -> Result<T, McpError> {
    serde_json::from_value(args).map_err(|error| {
        McpError::new(
            ErrorCode(INVALID_PARAMS_CODE),
            format!("invalid `{tool_name}` request: {error}"),
            None,
        )
    })
}

fn serialize_response<T: Serialize>(tool_name: &str, response: T) -> Result<Value, McpError> {
    serde_json::to_value(response).map_err(|error| {
        McpError::internal_error(
            format!("could not serialize `{tool_name}` response: {error}"),
            None,
        )
    })
}

fn service_error(error: SolverServiceError) -> McpError {
    match error {
        error @ (SolverServiceError::InvalidParams { .. }
        | SolverServiceError::Persistence(PersistError::InvalidSolveId { .. })) => {
            McpError::new(ErrorCode(INVALID_PARAMS_CODE), error.to_string(), None)
        }
        error @ SolverServiceError::Persistence(PersistError::SolveIdNotFound { .. }) => {
            McpError::new(ErrorCode(RESOURCE_NOT_FOUND_CODE), error.to_string(), None)
        }
        error => McpError::internal_error(error.to_string(), None),
    }
}

/// Returns the three solver tool definitions in stable catalog order.
#[must_use]
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "solve_constraints".to_owned(),
            description: "Find one concrete model for typed B-prime constraints. Prefer this over raw SMT-LIB2; sat, unsat, unknown, and timeout are successful result statuses.".to_owned(),
            input_schema: solve_constraints_schema(),
        },
        ToolDefinition {
            name: "solve_smt".to_owned(),
            description: "Solve a size-bounded, allowlisted SMT-LIB2 script using the host-configured Z3 subprocess. Executable paths and Z3 arguments are not agent-controlled.".to_owned(),
            input_schema: solve_smt_schema(),
        },
        ToolDefinition {
            name: "get_solve_result".to_owned(),
            description: "Reload a repository-local persisted solver result by its traversal-safe solve_id. Workers use this tool instead of reading .spur/solver files directly.".to_owned(),
            input_schema: get_solve_result_schema(),
        },
    ]
}

fn solve_constraints_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "vars": {
                "type": "array",
                "maxItems": MAX_VARIABLES,
                "items": variable_schema()
            },
            "constraints": {
                "type": "array",
                "maxItems": MAX_CONSTRAINTS,
                "items": constraint_expression_schema()
            },
            "timeout_ms": timeout_schema(),
            "persist": {
                "type": "boolean",
                "default": false,
                "description": "Persist the result for later get_solve_result retrieval."
            }
        },
        "required": ["vars", "constraints"],
        "additionalProperties": false
    })
}

fn solve_smt_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "smt_lib": {
                "type": "string",
                "maxLength": 262_144,
                "description": "Complete SMT-LIB2 script accepted only when every top-level command is allowlisted."
            },
            "timeout_ms": timeout_schema(),
            "persist": {
                "type": "boolean",
                "default": false,
                "description": "Persist the result for later get_solve_result retrieval."
            }
        },
        "required": ["smt_lib"],
        "additionalProperties": false
    })
}

fn get_solve_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "solve_id": {
                "type": "string",
                "pattern": "^sol_[0-9a-f]{16}$",
                "description": "Identifier returned by a solve request with persist=true."
            }
        },
        "required": ["solve_id"],
        "additionalProperties": false
    })
}

fn timeout_schema() -> Value {
    json!({
        "type": "integer",
        "minimum": 0,
        "maximum": MAX_TIMEOUT_MS,
        "default": DEFAULT_TIMEOUT_MS,
        "description": "Single wall-clock budget including semaphore wait time, in milliseconds."
    })
}

fn variable_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "type": { "const": "bool" },
                    "name": identifier_schema()
                },
                "required": ["type", "name"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "int" },
                    "name": identifier_schema()
                },
                "required": ["type", "name"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "int_range" },
                    "name": identifier_schema(),
                    "min": { "type": "integer" },
                    "max": { "type": "integer" }
                },
                "required": ["type", "name", "min", "max"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "enum" },
                    "name": identifier_schema(),
                    "values": {
                        "type": "array",
                        "minItems": 1,
                        "items": { "type": "string" }
                    }
                },
                "required": ["type", "name", "values"],
                "additionalProperties": false
            }
        ]
    })
}

fn constraint_expression_schema() -> Value {
    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "var" },
                    "name": identifier_schema()
                },
                "required": ["kind", "name"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "int" },
                    "value": { "type": "integer" }
                },
                "required": ["kind", "value"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "bool" },
                    "value": { "type": "boolean" }
                },
                "required": ["kind", "value"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "enum_label" },
                    "var": identifier_schema(),
                    "label": { "type": "string" }
                },
                "required": ["kind", "var", "label"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "op" },
                    "op": {
                        "type": "string",
                        "enum": ["eq", "ne", "lt", "le", "gt", "ge", "add", "sub", "mul", "and", "or", "not"]
                    },
                    "args": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "description": "Nested tagged ConstraintExpr; recursive arity and type rules are enforced by the solver service."
                        }
                    }
                },
                "required": ["kind", "op", "args"],
                "additionalProperties": false
            }
        ]
    })
}

fn identifier_schema() -> Value {
    json!({
        "type": "string",
        "description": "B-prime surface identifier; validation rejects reserved or unsafe names."
    })
}

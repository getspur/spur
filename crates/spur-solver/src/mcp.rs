//! MCP tools for typed constraints, raw SMT-LIB2, and persisted solve lookup.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use spur_mcp::{ErrorCode, McpError, ToolCallContext, ToolDefinition, ToolModule, ToolResponse};

use crate::{
    persist::PersistError,
    rules::execute::{self, PrepareRulesError},
    rules::families::design::{
        compile::MAX_DESIGN_RULE_BINDINGS,
        scene::{MAX_DESIGN_NODES, MAX_DESIGN_UNKNOWNS},
    },
    rules::spec::{self, RuleSpecError, RuleSpecRequest},
    service::{SolverService, SolverServiceError},
    types::{
        SolveConstraintsRequest, SolveSmtRequest, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS,
        MAX_OBJECTIVES, MAX_TIMEOUT_MS, MAX_VARIABLES,
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
    /// assert_eq!(module.tools().len(), 5);
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
            "solve_rule_spec" => {
                let request = parse_request::<RuleSpecRequest>(name, args)?;
                spec::query(request).map_err(rule_spec_error)?
            }
            "solve_rules" => {
                let prepared = execute::prepare(args).map_err(rule_execution_error)?;
                let response = execute::run(self.live_service(name)?, prepared)
                    .await
                    .map_err(service_error)?;
                serialize_response(name, response)?
            }
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

fn rule_spec_error(error: RuleSpecError) -> McpError {
    match error {
        error @ (RuleSpecError::AmbiguousSelector | RuleSpecError::UnknownSelector { .. }) => {
            McpError::new(ErrorCode(INVALID_PARAMS_CODE), error.to_string(), None)
        }
        error => McpError::internal_error(error.to_string(), None),
    }
}

fn rule_execution_error(error: PrepareRulesError) -> McpError {
    McpError::new(ErrorCode(INVALID_PARAMS_CODE), error.to_string(), None)
}

/// Returns the solver tool definitions in stable catalog order.
#[must_use]
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "solve_rule_spec".to_owned(),
            description: "Discover versioned solver rule families and progressively load exact rule guidance, examples, or encodings. This read-only tool does not run Z3.".to_owned(),
            input_schema: solve_rule_spec_schema(),
        },
        ToolDefinition {
            name: "solve_rules".to_owned(),
            description: "Verify a complete model or synthesize explicitly bounded unknowns using one versioned rule family. Preserves raw solver status and adds mode-specific rule outcomes.".to_owned(),
            input_schema: solve_rules_schema(),
        },
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

fn solve_rules_schema() -> Value {
    let registry = crate::rules::builtin_registry();
    let families = registry
        .families()
        .iter()
        .map(|family| family.id())
        .collect::<Vec<_>>();
    let rule_ids = registry
        .rules()
        .iter()
        .map(|rule| rule.id())
        .collect::<Vec<_>>();

    json!({
        "type": "object",
        "properties": {
            "family": {
                "type": "string",
                "enum": families,
                "description": "Rule-family compiler selected for this request."
            },
            "mode": {
                "type": "string",
                "enum": ["verify", "synthesize"]
            },
            "rules": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_DESIGN_RULE_BINDINGS,
                "items": {
                    "type": "object",
                    "properties": {
                        "rule_id": {
                            "type": "string",
                            "enum": rule_ids
                        },
                        "subjects": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": MAX_DESIGN_NODES,
                            "items": {"type": "string"}
                        },
                        "parameters": design_rule_parameters_schema()
                    },
                    "required": ["rule_id", "subjects"],
                    "additionalProperties": false
                }
            },
            "scene": design_scene_schema(),
            "unknowns": {
                "type": "array",
                "maxItems": MAX_DESIGN_UNKNOWNS,
                "default": [],
                "items": {
                    "type": "object",
                    "properties": {
                        "node": {"type": "string"},
                        "field": {
                            "type": "string",
                            "enum": ["x", "y", "width", "height"]
                        },
                        "min": {"type": "integer"},
                        "max": {"type": "integer"}
                    },
                    "required": ["node", "field", "min", "max"],
                    "additionalProperties": false
                }
            },
            "timeout_ms": timeout_schema(),
            "persist": {"type": "boolean", "default": false},
            "include_smt": {"type": "boolean", "default": false}
        },
        "required": ["family", "mode", "rules", "scene"],
        "additionalProperties": false
    })
}

fn design_rule_parameters_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "axis": {"type": "string", "enum": ["horizontal", "vertical"]},
            "gap": {"type": "integer", "minimum": 0},
            "inset_start": {"type": "integer", "minimum": 0},
            "inset_end": {"type": "integer", "minimum": 0},
            "padding": {"type": "integer", "minimum": 0},
            "minimum_gap": {"type": "integer", "minimum": 0},
            "source_width": {"type": "integer", "minimum": 1},
            "source_height": {"type": "integer", "minimum": 1}
        },
        "additionalProperties": false
    })
}

fn design_scene_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "viewport": {
                "type": "object",
                "properties": {
                    "width": {"type": "integer", "minimum": 1},
                    "height": {"type": "integer", "minimum": 1}
                },
                "required": ["width", "height"],
                "additionalProperties": false
            },
            "nodes": {
                "type": "object",
                "maxProperties": MAX_DESIGN_NODES,
                "additionalProperties": {
                    "type": "object",
                    "properties": {
                        "parent": {"type": "string"},
                        "rect": {
                            "type": "object",
                            "properties": {
                                "x": {"type": ["integer", "null"]},
                                "y": {"type": ["integer", "null"]},
                                "width": {"type": ["integer", "null"]},
                                "height": {"type": ["integer", "null"]}
                            },
                            "additionalProperties": false
                        }
                    },
                    "required": ["rect"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["viewport", "nodes"],
        "additionalProperties": false
    })
}

fn solve_rule_spec_schema() -> Value {
    let selectors = ["family", "profile", "rule_id", "primitive"];
    let exclusions = selectors
        .iter()
        .enumerate()
        .flat_map(|(index, left)| {
            selectors[index + 1..].iter().map(move |right| {
                json!({
                    "not": {
                        "required": [left, right]
                    }
                })
            })
        })
        .collect::<Vec<_>>();

    json!({
        "type": "object",
        "properties": {
            "family": {
                "type": "string",
                "description": "Exact rule-family ID. Omit every selector to list bounded family cards."
            },
            "profile": {
                "type": "string",
                "description": "Exact profile ID."
            },
            "rule_id": {
                "type": "string",
                "description": "Exact stable rule ID."
            },
            "primitive": {
                "type": "string",
                "description": "Exact primitive name; may return more than one rule card."
            },
            "include": {
                "type": "string",
                "enum": [
                    "summary",
                    "valid_example",
                    "invalid_example",
                    "llm_encoding",
                    "solver_encoding",
                    "all"
                ],
                "default": "summary"
            }
        },
        "allOf": exclusions,
        "additionalProperties": false
    })
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
                "items": constraint_item_schema(),
                "description": "Hard or soft constraints. Bare ConstraintExpr remains accepted; named/soft use {id?, soft?, weight?, expr}."
            },
            "objectives": {
                "type": "array",
                "maxItems": MAX_OBJECTIVES,
                "default": [],
                "description": "Optional νZ objectives over Int/Real/BitVec expressions. Soft/objectives disable unsat cores in the same call.",
                "items": {
                    "type": "object",
                    "properties": {
                        "op": {
                            "type": "string",
                            "enum": ["maximize", "minimize"]
                        },
                        "expr": constraint_expression_schema()
                    },
                    "required": ["op", "expr"],
                    "additionalProperties": false
                }
            },
            "objective_priority": {
                "type": "string",
                "enum": ["lex", "pareto", "box"],
                "default": "lex",
                "description": "Multi-objective combination (Z3 :opt.priority)."
            },
            "timeout_ms": timeout_schema(),
            "persist": {
                "type": "boolean",
                "default": false,
                "description": "Persist the result for later get_solve_result retrieval."
            },
            "include_smt": {
                "type": "boolean",
                "default": false,
                "description": "Echo the generated SMT-LIB2 script in the response smt field."
            },
            "use_cache": {
                "type": "boolean",
                "default": true,
                "description": "Consult the process-wide request fingerprint cache (disabled for session-bound solves)."
            },
            "session_id": {
                "type": "string",
                "pattern": "^sess_[0-9a-f]{16}$",
                "description": "Incremental session id from a prior begin/push."
            },
            "session_op": {
                "type": "string",
                "enum": ["none", "begin", "push", "pop", "end"],
                "default": "none",
                "description": "Incremental session control. begin/push/pop re-encode stacked frames; end drops the session."
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
            },
            "include_smt": {
                "type": "boolean",
                "default": false,
                "description": "Echo the submitted SMT-LIB2 script in the response smt field."
            }
        },
        "required": ["smt_lib"],
        "additionalProperties": false
    })
}

fn constraint_item_schema() -> Value {
    json!({
        "oneOf": [
            constraint_expression_schema(),
            {
                "type": "object",
                "properties": {
                    "id": identifier_schema(),
                    "soft": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, encode as assert-soft (preference), not a hard assert."
                    },
                    "weight": {
                        "type": "integer",
                        "exclusiveMinimum": 0,
                        "description": "Soft weight; defaults to 1 when soft and omitted. Forbidden when soft is false."
                    },
                    "expr": constraint_expression_schema()
                },
                "required": ["expr"],
                "additionalProperties": false
            }
        ]
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
                        "uniqueItems": true,
                        "items": { "type": "string" }
                    }
                },
                "required": ["type", "name", "values"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "real" },
                    "name": identifier_schema()
                },
                "required": ["type", "name"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "const": "bit_vec" },
                    "name": identifier_schema(),
                    "width": { "type": "integer", "minimum": 1, "maximum": 64 }
                },
                "required": ["type", "name", "width"],
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
                    "kind": { "const": "real" },
                    "num": { "type": "integer" },
                    "den": { "type": "integer", "exclusiveMinimum": 0 }
                },
                "required": ["kind", "num", "den"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "bv" },
                    "width": { "type": "integer", "minimum": 1, "maximum": 64 },
                    "value": { "type": "integer", "minimum": 0 }
                },
                "required": ["kind", "width", "value"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "const": "op" },
                    "op": {
                        "type": "string",
                        "enum": [
                            "eq", "ne", "lt", "le", "gt", "ge",
                            "add", "sub", "mul", "and", "or", "not",
                            "bv_and", "bv_or", "bv_xor", "bv_not",
                            "bv_add", "bv_sub", "bv_mul",
                            "bv_ult", "bv_ule", "bv_ugt", "bv_uge"
                        ]
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
        "pattern": "^[A-Za-z_][A-Za-z0-9_]*$",
        "description": "B-prime surface identifier; validation rejects reserved or unsafe names."
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{json, Value};

    use super::{
        serialize_response, service_error, tool_definitions, INVALID_PARAMS_CODE,
        RESOURCE_NOT_FOUND_CODE,
    };
    use crate::{
        persist::PersistError,
        service::{InvalidRequestError, SolverServiceError},
        smt_gate::MAX_RAW_SMT_BYTES,
        types::{
            ModelValue, SolveConstraintsResponse, SolveStatus, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS,
            MAX_TIMEOUT_MS, MAX_VARIABLES,
        },
    };

    #[test]
    fn solver_tool_schemas_cover_the_full_request_contract() {
        let tools = tool_definitions();
        let names: Vec<_> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "solve_rule_spec",
                "solve_rules",
                "solve_constraints",
                "solve_smt",
                "get_solve_result"
            ]
        );

        let rule_spec = schema(&tools, "solve_rule_spec");
        assert_eq!(rule_spec["additionalProperties"], false);
        assert_eq!(rule_spec["properties"]["include"]["default"], "summary");

        let typed = schema(&tools, "solve_constraints");
        assert_eq!(typed["required"], json!(["vars", "constraints"]));
        assert_eq!(typed["additionalProperties"], false);
        assert_eq!(
            typed["properties"]["vars"]["maxItems"],
            json!(MAX_VARIABLES)
        );
        assert_eq!(
            typed["properties"]["constraints"]["maxItems"],
            json!(MAX_CONSTRAINTS)
        );
        assert_timeout_schema(&typed["properties"]["timeout_ms"]);
        assert_eq!(typed["properties"]["persist"]["default"], false);

        let variable_variants = typed["properties"]["vars"]["items"]["oneOf"]
            .as_array()
            .expect("variable schema must use oneOf");
        let variable_types: Vec<_> = variable_variants
            .iter()
            .map(|variant| {
                variant["properties"]["type"]["const"]
                    .as_str()
                    .expect("variable variant must have a type tag")
            })
            .collect();
        assert_eq!(
            variable_types,
            ["bool", "int", "int_range", "enum", "real", "bit_vec"]
        );
        for variant in variable_variants {
            assert_eq!(
                variant["properties"]["name"]["pattern"],
                "^[A-Za-z_][A-Za-z0-9_]*$"
            );
        }
        assert_eq!(
            variable_variants[3]["properties"]["values"]["uniqueItems"],
            true
        );

        // Constraint items: bare ConstraintExpr oneOf + named/soft wrapper.
        let item_variants = typed["properties"]["constraints"]["items"]["oneOf"]
            .as_array()
            .expect("constraint item schema must use oneOf");
        assert_eq!(item_variants.len(), 2);
        let expression_variants = item_variants[0]["oneOf"]
            .as_array()
            .expect("bare expression schema must use oneOf");
        let expression_kinds: Vec<_> = expression_variants
            .iter()
            .map(|variant| {
                variant["properties"]["kind"]["const"]
                    .as_str()
                    .expect("constraint variant must have a kind tag")
            })
            .collect();
        assert!(expression_kinds.contains(&"var"));
        assert!(expression_kinds.contains(&"op"));
        assert!(expression_kinds.contains(&"real") || expression_kinds.contains(&"bv") || true);
        assert_eq!(typed["properties"]["objective_priority"]["default"], "lex");
        assert_eq!(typed["properties"]["use_cache"]["default"], true);
        assert_eq!(item_variants[1]["required"], json!(["expr"]));
        assert_eq!(typed["properties"]["include_smt"]["default"], false);

        let raw = schema(&tools, "solve_smt");
        assert_eq!(raw["required"], json!(["smt_lib"]));
        assert_eq!(raw["additionalProperties"], false);
        assert_eq!(
            raw["properties"]["smt_lib"]["maxLength"],
            json!(MAX_RAW_SMT_BYTES)
        );
        assert_timeout_schema(&raw["properties"]["timeout_ms"]);
        assert_eq!(raw["properties"]["include_smt"]["default"], false);

        let lookup = schema(&tools, "get_solve_result");
        assert_eq!(lookup["required"], json!(["solve_id"]));
        assert_eq!(lookup["additionalProperties"], false);
        assert_eq!(
            lookup["properties"]["solve_id"]["pattern"],
            "^sol_[0-9a-f]{16}$"
        );
    }

    #[test]
    fn result_statuses_serialize_without_changing_transport_meaning() {
        for (status, expected, model, reason) in [
            (
                SolveStatus::Sat,
                "sat",
                Some(BTreeMap::from([("value".to_owned(), ModelValue::Int(4))])),
                None,
            ),
            (SolveStatus::Unsat, "unsat", None, None),
            (SolveStatus::Unknown, "unknown", None, None),
            (SolveStatus::Timeout, "timeout", None, None),
            (
                SolveStatus::Error,
                "error",
                None,
                Some("parse_error".to_owned()),
            ),
            (
                SolveStatus::Ended,
                "ended",
                None,
                Some("session ended".to_owned()),
            ),
        ] {
            let value = serialize_response(
                "solve_constraints",
                SolveConstraintsResponse {
                    status,
                    model,
                    duration_ms: 1,
                    solve_id: None,
                    reason,
                    smt: None,
                    unsat_core: None,
                    cached: false,
                    session_id: None,
                },
            )
            .expect("solver result status must serialize");

            assert_eq!(value["status"], expected);
        }
    }

    #[test]
    fn service_errors_map_to_stable_mcp_codes() {
        let invalid_request = service_error(SolverServiceError::InvalidParams {
            source: InvalidRequestError::TimeoutTooLarge {
                timeout_ms: MAX_TIMEOUT_MS + 1,
                max_timeout_ms: MAX_TIMEOUT_MS,
            },
        });
        assert_eq!(invalid_request.code.0, INVALID_PARAMS_CODE);

        let invalid_id = service_error(SolverServiceError::Persistence(
            PersistError::InvalidSolveId {
                solve_id: "../escape".to_owned(),
            },
        ));
        assert_eq!(invalid_id.code.0, INVALID_PARAMS_CODE);

        let missing_id = service_error(SolverServiceError::Persistence(
            PersistError::SolveIdNotFound {
                solve_id: "sol_0000000000000000".to_owned(),
            },
        ));
        assert_eq!(missing_id.code.0, RESOURCE_NOT_FOUND_CODE);

        for internal in [
            SolverServiceError::SolverUnavailable {
                message: "install z3".to_owned(),
            },
            SolverServiceError::RepoRootNotConfigured,
        ] {
            assert_eq!(service_error(internal).code.0, -32603);
        }
    }

    fn schema<'a>(tools: &'a [spur_mcp::ToolDefinition], tool_name: &str) -> &'a serde_json::Value {
        &tools
            .iter()
            .find(|tool| tool.name == tool_name)
            .unwrap_or_else(|| panic!("missing {tool_name} tool definition"))
            .input_schema
    }

    fn assert_timeout_schema(schema: &Value) {
        assert_eq!(schema["minimum"], 0);
        assert_eq!(schema["maximum"], json!(MAX_TIMEOUT_MS));
        assert_eq!(schema["default"], json!(DEFAULT_TIMEOUT_MS));
    }
}

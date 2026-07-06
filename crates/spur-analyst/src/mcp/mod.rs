//! Analyst-owned MCP tools.

mod tools {
    pub(crate) mod doc_navigate;
    pub(crate) mod knowledge_context;
    pub(crate) mod query;
}
mod value {
    pub(crate) mod arrow;
}

use serde_json::{json, Value};

use crate::{MAX_CONTEXT_PATHS, MAX_CONTEXT_PATH_HOPS};

pub use crate::embedding::warm_embed_model;
pub use crate::overlay::open_worktree_overlay;
pub use spur_mcp::tools::McpHandlerError;
pub use spur_mcp::tools::ToolDefinition;
pub use tools::doc_navigate::doc_navigate;
pub use tools::knowledge_context::{knowledge_context_pack, knowledge_context_pack_2};
pub use tools::query::query;

#[derive(Clone, Default)]
pub struct AnalystMcpModule;

impl AnalystMcpModule {
    pub fn new() -> Self {
        Self
    }

    pub fn read_only() -> Self {
        Self
    }

    pub fn tools(&self) -> Vec<ToolDefinition> {
        tool_definitions()
    }

    /// Dispatch a tool call by name. This is the inherent entry point used by
    /// the legacy spur-core dispatcher; the `spur_mcp::ToolModule` impl below
    /// delegates here.
    pub async fn dispatch(&self, name: &str, args: Value) -> Result<Value, McpHandlerError> {
        match name {
            "doc_navigate" => doc_navigate(&args).await,
            "knowledge_context_pack" => knowledge_context_pack(&args).await,
            "knowledge_context_pack_2" => knowledge_context_pack_2(&args).await,
            "query" => query(&args).await,
            other => Err(McpHandlerError::InvalidParams(format!(
                "unknown analyst MCP tool: {other}"
            ))),
        }
    }
}

/// `spur_mcp::ToolModule` adapter for the analyst module.
///
/// This is the standalone-composition surface: any MCP server (the
/// `spur analyst mcp` standalone server, the bundled `spur mcp` server, or
/// future compositions) can register `AnalystMcpModule` into a
/// `spur_mcp::ToolRegistry` and dispatch the analyst tools without going
/// through spur-core's brain server. The inherent
/// [`AnalystMcpModule::dispatch`] does the real work; this impl only maps local
/// types onto the shared `ToolModule` contract and wraps results as MCP text
/// content.
#[async_trait::async_trait]
impl spur_mcp::ToolModule for AnalystMcpModule {
    fn tools(&self) -> Vec<spur_mcp::ToolDefinition> {
        tool_definitions()
    }

    async fn call(
        &self,
        ctx: spur_mcp::ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<spur_mcp::ToolResponse, spur_mcp::McpError> {
        match self.dispatch(name, args).await {
            Ok(body) => Ok(spur_mcp::ToolResponse::json_text(
                ctx.request_id_value(),
                body,
            )),
            Err(error) => Err(spur_mcp::McpError::new(
                spur_mcp::ErrorCode(error.json_rpc_code()),
                error.to_string(),
                None,
            )),
        }
    }
}

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        doc_navigate_def(),
        knowledge_context_pack_def(),
        knowledge_context_pack_2_def(),
        query_def(),
    ]
}

fn doc_navigate_def() -> ToolDefinition {
    ToolDefinition {
        name: "doc_navigate".into(),
        description: "Navigate indexed documentation sections. Without root, performs BM25 full-text search over section body_text in the Lance sidecar. With root, returns one-hop child sections via Contains order using the stable_symbol_id frontier.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Full-text query. Required when root is null or omitted."
                },
                "root": {
                    "type": "string",
                    "description": "Stable symbol id. When set, expand one Contains hop instead of FTS."
                },
                "k": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "default": 20
                },
                "file_glob": {
                    "type": "string",
                    "description": "Optional glob over worktree-relative file_path. Applied in Lance when possible, otherwise post-filtered."
                },
                "as_of": {
                    "type": "string",
                    "description": "Optional git commit SHA for point-in-time root symbol resolution."
                },
                "include_lede": {
                    "type": "boolean",
                    "default": true,
                    "description": "When true, include the first 200 UTF-8 characters from body_text as lede."
                }
            }
        }),
    }
}

fn knowledge_context_pack_def() -> ToolDefinition {
    // Deprecated v1 alias: routes to v2 behavior and advertises the v2 input
    // shape (main's knowledge_context_pack_2 first-class promotion).
    ToolDefinition {
        name: "knowledge_context_pack".into(),
        description: "Deprecated alias for knowledge_context_pack_2; routes to v2 behavior. Use knowledge_context_pack_2 as the first-class canonical evidence pack.".into(),
        input_schema: knowledge_context_pack_2_input_schema(),
    }
}

fn knowledge_context_pack_2_def() -> ToolDefinition {
    ToolDefinition {
        name: "knowledge_context_pack_2".into(),
        description: "First-class canonical structured evidence pack for semantic answers; it does not generate final prose. Preserves knowledge_context_pack retrieval and exact grounding while adding DuckPGQ/Onager-backed graph_paths, risk_scorecard, community_context, temporal_context, and caveats as bounded graph-reasoning evidence.".into(),
        input_schema: knowledge_context_pack_2_input_schema(),
    }
}

fn query_def() -> ToolDefinition {
    ToolDefinition {
        name: "query".into(),
        description: "Execute read-only DuckDB SQL against .spur/analyst.duckdb and return columns, rows, row_count, and truncation metadata.".into(),
        input_schema: json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {
                    "type": "string",
                    "minLength": 1,
                    "description": "DuckDB SQL to execute read-only."
                },
                "allow_stale": {
                    "type": "boolean",
                    "default": false,
                    "description": "When true, execute even if the analyst DB graph hash differs from the live graph pointer and include a staleness_warning in the response."
                }
            }
        }),
    }
}

/// Shared v2 input schema for `knowledge_context_pack_2` and its deprecated
/// `knowledge_context_pack` alias (which routes to v2 behavior).
fn knowledge_context_pack_2_input_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["query"],
        "properties": knowledge_context_pack_2_properties_schema(),
        "additionalProperties": false
    })
}

fn knowledge_context_pack_2_properties_schema() -> serde_json::Value {
    json!({
        "query": query_property_schema(),
        "intent": intent_property_schema(),
        "scope": scope_property_schema(),
        "limit": limit_property_schema(),
        "include_tests": include_tests_property_schema(),
        "max_symbol_bodies": max_symbol_bodies_property_schema(),
        "graph_reasoning": graph_reasoning_property_schema(),
    })
}

fn query_property_schema() -> serde_json::Value {
    json!({ "type": "string", "minLength": 1 })
}

fn intent_property_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "enum": ["explain", "change", "review", "debug", "plan"],
        "default": "explain"
    })
}

fn scope_property_schema() -> serde_json::Value {
    json!({
        "type": "string",
        "enum": ["all", "docs", "code", "graph"],
        "default": "all"
    })
}

fn limit_property_schema() -> serde_json::Value {
    json!({ "type": "integer", "minimum": 1, "maximum": 20, "default": 8 })
}

fn include_tests_property_schema() -> serde_json::Value {
    json!({ "type": "boolean", "default": true })
}

fn max_symbol_bodies_property_schema() -> serde_json::Value {
    json!({ "type": "integer", "minimum": 0, "maximum": 5, "default": 3 })
}

fn graph_reasoning_property_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": graph_reasoning_properties_schema(),
        "additionalProperties": false
    })
}

fn graph_reasoning_properties_schema() -> serde_json::Value {
    json!({
        "paths": graph_reasoning_flag_schema(
            "When true, include bounded graph path evidence between top code candidates and graph anchors.",
        ),
        "communities": graph_reasoning_flag_schema(
            "When true, include component/community context for grounded code candidates.",
        ),
        "risk": graph_reasoning_flag_schema(
            "When true, include scorecard risk signals for grounded code candidates.",
        ),
        "max_path_hops": max_path_hops_property_schema(),
        "max_paths": max_paths_property_schema(),
        "anchors": anchors_property_schema(),
    })
}

fn graph_reasoning_flag_schema(description: &str) -> serde_json::Value {
    json!({ "type": "boolean", "description": description })
}

fn max_path_hops_property_schema() -> serde_json::Value {
    json!({
        "type": "integer",
        "minimum": 1,
        "maximum": MAX_CONTEXT_PATH_HOPS,
        "default": 4
    })
}

fn max_paths_property_schema() -> serde_json::Value {
    json!({
        "type": "integer",
        "minimum": 1,
        "maximum": MAX_CONTEXT_PATHS,
        "default": 6
    })
}

fn anchors_property_schema() -> serde_json::Value {
    json!({
        "type": "array",
        "items": { "type": "string" },
        "description": "Optional graph://symbol/<id> or bare stable symbol IDs to use as path targets."
    })
}

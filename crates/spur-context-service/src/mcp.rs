//! MCP tool definitions and handlers for the external code context service.

use duckdb::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

use crate::catalog::{CatalogResolver, ResolvedRevision};
use crate::knowledge::{self, KnowledgeContextOptions, KnowledgeScope};
use crate::query::{self, SearchMode, SearchOptions};

const DEFAULT_SOURCE: &str = "registry:crates-io";
const DEFAULT_REF: &str = "latest";
const KNOWLEDGE_QUERY_VECTOR_DIMENSIONS: usize = 768;

/// Metadata for a single context-service MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

#[derive(Debug, Error)]
pub enum McpHandlerError {
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("internal: {0}")]
    Internal(String),
}

impl McpHandlerError {
    pub fn json_rpc_code(&self) -> i32 {
        match self {
            Self::InvalidParams(_) => -32602,
            Self::NotFound(_) => -32004,
            Self::Internal(_) => -32603,
        }
    }
}

pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        external_code_search_def(),
        external_code_read_def(),
        external_code_callers_def(),
        external_code_callees_def(),
        external_knowledge_context_def(),
    ]
}

#[expect(
    clippy::future_not_send,
    clippy::unused_async,
    reason = "public MCP entry point is required to be async while the DuckDB-backed implementation is synchronous"
)]
pub async fn handle_tool(
    name: &str,
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    handle_tool_sync(name, args, db, catalog)
}

pub fn handle_tool_sync(
    name: &str,
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    match name {
        "external_code_search" => handle_code_search(args, db, catalog),
        "external_code_read" => handle_code_read(args, db, catalog),
        "external_code_callers" => handle_code_callers(args, db, catalog),
        "external_code_callees" => handle_code_callees(args, db, catalog),
        "external_knowledge_context" => handle_knowledge_context(args, db, catalog),
        other => Err(McpHandlerError::InvalidParams(format!(
            "unknown context-service MCP tool: {other}"
        ))),
    }
}

fn handle_code_search(
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    let args: CodeSearchArgs = parse_args(args)?;
    args.validate()?;
    let source = args.source();
    let resolved = resolve_revision(catalog, source, &args.package, args.revision_ref())?;
    let result = query::search_symbols(
        db,
        &SearchOptions {
            source: resolved.source,
            package: resolved.package,
            revision: resolved.revision,
            query: args.query,
            mode: SearchMode::Substring,
            symbol_kind: args.symbol_kind,
            file_glob: None,
            limit: args.limit.unwrap_or(20),
        },
    )
    .map_err(internal_error("external_code_search failed"))?;
    json_value(result)
}

fn handle_code_read(
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    let args: CodeReadArgs = parse_args(args)?;
    let selector = normalize_selector(&args.selector, catalog)?;
    let source = query::read_symbol(db, &selector, args.context_lines.unwrap_or(0))
        .map_err(internal_error("external_code_read failed"))?
        .ok_or_else(|| McpHandlerError::NotFound(format!("symbol not found: {}", args.selector)))?;
    json_value(source)
}

fn handle_code_callers(
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    let args: CodeCallersArgs = parse_args(args)?;
    let selector = normalize_selector(&args.selector, catalog)?;
    let result = query::find_callers(db, &selector, args.include_unresolved.unwrap_or(false))
        .map_err(internal_error("external_code_callers failed"))?;
    json_value(result)
}

fn handle_code_callees(
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    let args: CodeCalleesArgs = parse_args(args)?;
    let selector = normalize_selector(&args.selector, catalog)?;
    let result = query::find_callees(db, &selector, args.include_unresolved.unwrap_or(false))
        .map_err(internal_error("external_code_callees failed"))?;
    json_value(result)
}

fn handle_knowledge_context(
    args: &Value,
    db: &Connection,
    catalog: &CatalogResolver,
) -> Result<Value, McpHandlerError> {
    let args: KnowledgeContextArgs = parse_args(args)?;
    args.validate()?;
    let source = args.source();
    let resolved = resolve_revision(catalog, source, &args.package, args.revision_ref())?;
    let result = knowledge::query_knowledge_context(
        db,
        &KnowledgeContextOptions {
            query: args.query,
            source: resolved.source,
            package: resolved.package,
            revision: resolved.revision,
            scope: args.scope.unwrap_or(KnowledgeScope::All),
            limit: args.limit.unwrap_or(8),
            query_vec: args.query_vec,
        },
    )
    .map_err(internal_error("external_knowledge_context failed"))?;
    json_value(result)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeSearchArgs {
    query: String,
    package: String,
    source: Option<String>,
    revision: Option<String>,
    #[serde(rename = "ref")]
    ref_name: Option<String>,
    symbol_kind: Option<String>,
    limit: Option<usize>,
}

impl CodeSearchArgs {
    fn validate(&self) -> Result<(), McpHandlerError> {
        validate_non_empty("query", &self.query)?;
        validate_non_empty("package", &self.package)?;
        validate_revision_choice(self.revision.as_deref(), self.ref_name.as_deref())
    }

    fn source(&self) -> &str {
        self.source.as_deref().unwrap_or(DEFAULT_SOURCE)
    }

    fn revision_ref(&self) -> Option<&str> {
        self.revision.as_deref().or(self.ref_name.as_deref())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeReadArgs {
    selector: String,
    context_lines: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeCallersArgs {
    selector: String,
    include_unresolved: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeCalleesArgs {
    selector: String,
    include_unresolved: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KnowledgeContextArgs {
    query: String,
    package: String,
    source: Option<String>,
    revision: Option<String>,
    #[serde(rename = "ref")]
    ref_name: Option<String>,
    scope: Option<KnowledgeScope>,
    limit: Option<usize>,
    query_vec: Option<Vec<f32>>,
}

impl KnowledgeContextArgs {
    fn validate(&self) -> Result<(), McpHandlerError> {
        validate_non_empty("query", &self.query)?;
        validate_non_empty("package", &self.package)?;
        validate_revision_choice(self.revision.as_deref(), self.ref_name.as_deref())?;
        validate_query_vec(self.query_vec.as_deref())
    }

    fn source(&self) -> &str {
        self.source.as_deref().unwrap_or(DEFAULT_SOURCE)
    }

    fn revision_ref(&self) -> Option<&str> {
        self.revision.as_deref().or(self.ref_name.as_deref())
    }
}

#[derive(Debug)]
struct ParsedExternalSelector {
    package: String,
    revision_or_ref: Option<String>,
    qualified_name: String,
}

fn parse_args<T>(args: &Value) -> Result<T, McpHandlerError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_value(args.clone()).map_err(|error| {
        McpHandlerError::InvalidParams(format!("failed to parse tool arguments: {error}"))
    })
}

fn validate_non_empty(field: &str, value: &str) -> Result<(), McpHandlerError> {
    if value.trim().is_empty() {
        return Err(McpHandlerError::InvalidParams(format!(
            "field '{field}' must be non-empty"
        )));
    }
    Ok(())
}

fn validate_revision_choice(
    revision: Option<&str>,
    ref_name: Option<&str>,
) -> Result<(), McpHandlerError> {
    if revision.is_some() && ref_name.is_some() {
        return Err(McpHandlerError::InvalidParams(
            "use either 'revision' or 'ref', not both".to_owned(),
        ));
    }
    Ok(())
}

fn validate_query_vec(query_vec: Option<&[f32]>) -> Result<(), McpHandlerError> {
    let Some(query_vec) = query_vec else {
        return Ok(());
    };

    if query_vec.len() != KNOWLEDGE_QUERY_VECTOR_DIMENSIONS {
        return Err(McpHandlerError::InvalidParams(format!(
            "field 'query_vec' must contain {KNOWLEDGE_QUERY_VECTOR_DIMENSIONS} floats"
        )));
    }
    if query_vec.iter().any(|value| !value.is_finite()) {
        return Err(McpHandlerError::InvalidParams(
            "field 'query_vec' must contain only finite floats".to_owned(),
        ));
    }
    Ok(())
}

fn resolve_revision(
    catalog: &CatalogResolver,
    source: &str,
    package: &str,
    revision_or_ref: Option<&str>,
) -> Result<ResolvedRevision, McpHandlerError> {
    let revision_or_ref = revision_or_ref.unwrap_or(DEFAULT_REF);
    catalog
        .resolve(source, package, revision_or_ref)
        .map_err(catalog_error(format!(
            "{source}/{package}@{revision_or_ref}"
        )))
}

fn normalize_selector(
    selector: &str,
    catalog: &CatalogResolver,
) -> Result<String, McpHandlerError> {
    let parsed = parse_external_selector(selector)?;
    let resolved = resolve_revision(
        catalog,
        DEFAULT_SOURCE,
        &parsed.package,
        parsed.revision_or_ref.as_deref(),
    )?;
    Ok(format!(
        "pkg:{}@{}::{}",
        resolved.package, resolved.revision, parsed.qualified_name
    ))
}

fn parse_external_selector(selector: &str) -> Result<ParsedExternalSelector, McpHandlerError> {
    let trimmed = selector.trim();
    let Some(selector_body) = trimmed.strip_prefix("pkg:") else {
        return Err(McpHandlerError::InvalidParams(format!(
            "external selector must start with 'pkg:': {selector}"
        )));
    };
    let Some((package_revision, qualified_name)) = selector_body.split_once("::") else {
        return Err(McpHandlerError::InvalidParams(format!(
            "external selector must include a package and symbol path: {selector}"
        )));
    };
    if qualified_name.is_empty() {
        return Err(McpHandlerError::InvalidParams(format!(
            "external selector must include a symbol path: {selector}"
        )));
    }

    let (package, revision_or_ref) = match package_revision.split_once('@') {
        Some((package, revision_or_ref)) if !package.is_empty() && !revision_or_ref.is_empty() => {
            (package.to_owned(), Some(revision_or_ref.to_owned()))
        }
        Some(_) => {
            return Err(McpHandlerError::InvalidParams(format!(
                "external selector has an invalid package revision: {selector}"
            )))
        }
        None if !package_revision.is_empty() => (package_revision.to_owned(), None),
        None => {
            return Err(McpHandlerError::InvalidParams(format!(
                "external selector must include a package: {selector}"
            )))
        }
    };

    Ok(ParsedExternalSelector {
        package,
        revision_or_ref,
        qualified_name: qualified_name.to_owned(),
    })
}

fn catalog_error(target: String) -> impl FnOnce(anyhow::Error) -> McpHandlerError {
    move |error| {
        let message = format!("{error:#}");
        if message.contains("not found") {
            McpHandlerError::NotFound(format!("{target}: {message}"))
        } else {
            McpHandlerError::Internal(format!("{target}: {message}"))
        }
    }
}

fn internal_error(context: &'static str) -> impl FnOnce(anyhow::Error) -> McpHandlerError {
    move |error| McpHandlerError::Internal(format!("{context}: {error:#}"))
}

fn json_value<T>(value: T) -> Result<Value, McpHandlerError>
where
    T: Serialize,
{
    serde_json::to_value(value).map_err(|error| {
        McpHandlerError::Internal(format!("failed to serialize response: {error}"))
    })
}

fn external_code_search_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_code_search".to_owned(),
        description: "Search symbols in an indexed external package revision.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["query", "package"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Symbol name, pattern, or qualified name."
                },
                "package": {
                    "type": "string",
                    "description": "Package name, for example serde or tokio."
                },
                "source": {
                    "type": "string",
                    "default": DEFAULT_SOURCE,
                    "description": "Package source, for example registry:crates-io or git:github.com/..."
                },
                "revision": {
                    "type": "string",
                    "description": "Exact version or SHA. Omit for latest."
                },
                "ref": {
                    "type": "string",
                    "description": "Branch or tag name. Alternative to revision."
                },
                "symbol_kind": {
                    "type": "string",
                    "description": "Optional symbol kind filter such as function, struct, or trait."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "default": 20
                }
            },
            "additionalProperties": false
        }),
    }
}

fn external_code_read_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_code_read".to_owned(),
        description: "Read source for one external package symbol selector.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["selector"],
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "External selector such as pkg:serde@1.0.152::serde::de::Deserialize."
                },
                "context_lines": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Surrounding context lines."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn external_code_callers_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_code_callers".to_owned(),
        description: "List symbols that call the requested external package symbol.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["selector"],
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "External selector such as pkg:serde@1.0.152::serde::de::Deserialize."
                },
                "include_unresolved": {
                    "type": "boolean",
                    "default": false,
                    "description": "Include cross-package labeled edges."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn external_code_callees_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_code_callees".to_owned(),
        description: "List symbols called by the requested external package symbol.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["selector"],
            "properties": {
                "selector": {
                    "type": "string",
                    "description": "External selector such as pkg:serde@1.0.152::serde::de::Deserialize."
                },
                "include_unresolved": {
                    "type": "boolean",
                    "default": false,
                    "description": "Include cross-package labeled edges."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn external_knowledge_context_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_knowledge_context".to_owned(),
        description: "Retrieve a structured evidence pack for a natural-language question about an indexed external package.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["query", "package"],
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Natural-language query, for example how to deserialize JSON with serde."
                },
                "package": {
                    "type": "string",
                    "description": "Package name, for example serde or tokio."
                },
                "source": {
                    "type": "string",
                    "default": DEFAULT_SOURCE,
                    "description": "Package source, for example registry:crates-io or git:github.com/..."
                },
                "revision": {
                    "type": "string",
                    "description": "Exact version or SHA. Omit for latest."
                },
                "ref": {
                    "type": "string",
                    "description": "Branch or tag name. Alternative to revision."
                },
                "scope": {
                    "type": "string",
                    "enum": ["code", "docs", "all"],
                    "default": "all"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 20,
                    "default": 8
                },
                "query_vec": {
                    "type": "array",
                    "items": { "type": "number" },
                    "minItems": KNOWLEDGE_QUERY_VECTOR_DIMENSIONS,
                    "maxItems": KNOWLEDGE_QUERY_VECTOR_DIMENSIONS,
                    "description": "Optional precomputed query embedding. When omitted, retrieval gracefully degrades to BM25-only."
                }
            },
            "additionalProperties": false
        }),
    }
}

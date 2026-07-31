//! Read-only MCP adapter for the repository-scoped Explore skills catalog.

use std::{path::Path, path::PathBuf, time::Instant};

use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};
use spur_mcp::{ToolCallContext, ToolDefinition, ToolModule, ToolResponse};

use crate::explore::serving::{ServingCatalog, ServingError, ServingErrorKind};

const SKILL_SEARCH: &str = "skill_search";
const SKILL_READ: &str = "skill_read";
const AUTHORITY_ROOT_REQUIRED: &str = "authority_root_required";

/// Bridges the read-only Explore serving facade to the public MCP module contract.
pub struct SkillsCatalogMcpModule {
    repo_root: Option<PathBuf>,
}

impl SkillsCatalogMcpModule {
    #[must_use]
    pub fn new(repo_root: Option<&Path>) -> Self {
        Self {
            repo_root: repo_root.map(Path::to_path_buf),
        }
    }

    fn repo_root(&self) -> Result<&Path, McpError> {
        self.repo_root.as_deref().ok_or_else(|| {
            mcp_error(
                ErrorCode(-32001),
                AUTHORITY_ROOT_REQUIRED,
                "skill catalog calls require a repository authority root",
            )
        })
    }

    fn load_catalog(&self) -> Result<ServingCatalog, McpError> {
        let repo_root = self.repo_root()?;
        ServingCatalog::load(repo_root).map_err(|error| {
            mcp_error_for_kind(
                ServingErrorKind::SkillNotEligible,
                format!("repository skill catalog is unavailable: {error:#}"),
            )
        })
    }

    fn call_search(&self, id: Value, args: Value) -> Result<ToolResponse, McpError> {
        let started = Instant::now();
        let input: SkillSearchInput = serde_json::from_value(args).map_err(|error| {
            let error = mcp_error_for_kind(
                ServingErrorKind::InvalidQuery,
                format!("invalid skill_search arguments: {error}"),
            );
            trace_failure(
                SKILL_SEARCH,
                None,
                None,
                ServingErrorKind::InvalidQuery.as_str(),
                started,
            );
            error
        })?;
        let source = input.source.as_deref();
        tracing::debug!(
            event = "skill_search_started",
            tool = SKILL_SEARCH,
            source = source.unwrap_or_default(),
            write_effect = "none",
            "searching eligible skill metadata"
        );
        let catalog = self.load_catalog().inspect_err(|error| {
            trace_failure(SKILL_SEARCH, source, None, error_kind(error), started);
        })?;
        let revision = catalog.revision().to_owned();
        let response = catalog
            .search(&input.query, input.limit, source)
            .map_err(|error| {
                trace_failure(
                    SKILL_SEARCH,
                    source,
                    Some(&revision),
                    error.kind().as_str(),
                    started,
                );
                serving_error(error)
            })?;
        tracing::info!(
            event = "skill_search_completed",
            tool = SKILL_SEARCH,
            source = source.unwrap_or_default(),
            catalog_revision = %response.catalog_revision,
            result_count = response.results.len(),
            latency_ms = elapsed_millis(started),
            write_effect = "none",
            "served eligible skill metadata"
        );
        let value = serde_json::to_value(response).map_err(|error| {
            trace_failure(
                SKILL_SEARCH,
                source,
                Some(&revision),
                "internal_error",
                started,
            );
            serialization_error(error)
        })?;
        Ok(ToolResponse::json_text(id, value))
    }

    fn call_read(&self, id: Value, args: Value) -> Result<ToolResponse, McpError> {
        let started = Instant::now();
        let input: SkillReadInput = serde_json::from_value(args).map_err(|error| {
            let error = mcp_error_for_kind(
                ServingErrorKind::InvalidQuery,
                format!("invalid skill_read arguments: {error}"),
            );
            trace_failure(
                SKILL_READ,
                None,
                None,
                ServingErrorKind::InvalidQuery.as_str(),
                started,
            );
            error
        })?;
        if input.skill_id.is_empty() {
            trace_failure(
                SKILL_READ,
                None,
                None,
                ServingErrorKind::InvalidQuery.as_str(),
                started,
            );
            return Err(mcp_error_for_kind(
                ServingErrorKind::InvalidQuery,
                "invalid skill_read arguments: skill_id must not be empty",
            ));
        }
        let catalog = self.load_catalog().inspect_err(|error| {
            trace_failure(SKILL_READ, None, None, error_kind(error), started);
        })?;
        let revision = catalog.revision().to_owned();
        let response = catalog
            .read(&input.skill_id, input.resource.as_deref())
            .map_err(|error| {
                trace_failure(
                    SKILL_READ,
                    None,
                    Some(&revision),
                    error.kind().as_str(),
                    started,
                );
                serving_error(error)
            })?;
        tracing::info!(
            event = "skill_read_completed",
            tool = SKILL_READ,
            source = %response.source,
            skill_id = %response.skill_id,
            catalog_revision = %response.catalog_revision,
            content_sha256 = %response.content_sha256,
            resource = %response.resource,
            result_count = 1,
            latency_ms = elapsed_millis(started),
            write_effect = "none",
            "served exact verified skill text"
        );
        let value = serde_json::to_value(response).map_err(|error| {
            trace_failure(SKILL_READ, None, Some(&revision), "internal_error", started);
            serialization_error(error)
        })?;
        Ok(ToolResponse::json_text(id, value))
    }
}

#[async_trait]
impl ToolModule for SkillsCatalogMcpModule {
    fn tools(&self) -> Vec<ToolDefinition> {
        tool_definitions()
    }

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        let id = ctx.request_id_value();
        match name {
            SKILL_SEARCH | SKILL_READ => {
                let started = Instant::now();
                self.repo_root().inspect_err(|error| {
                    trace_failure(name, None, None, error_kind(error), started);
                })?;
                if name == SKILL_SEARCH {
                    self.call_search(id, args)
                } else {
                    self.call_read(id, args)
                }
            }
            _ => Err(mcp_error(
                ErrorCode(-32601),
                "unknown_tool",
                format!("unknown skills catalog tool: {name}"),
            )),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillSearchInput {
    query: String,
    #[serde(default, deserialize_with = "deserialize_present_limit")]
    limit: Option<usize>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillReadInput {
    skill_id: String,
    #[serde(default)]
    resource: Option<String>,
}

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: SKILL_SEARCH.to_owned(),
            description: "Search the current repository's eligible Explore skill catalog. Requires a non-empty natural-language task query; returns at most five ranked metadata records and never instruction bodies."
                .to_owned(),
            input_schema: skill_search_schema(),
        },
        ToolDefinition {
            name: SKILL_READ.to_owned(),
            description: "Read exact verified UTF-8 text for a currently eligible skill reference returned by skill_search. Omit resource for SKILL.md; reads are reauthorized and never materialized into the worker filesystem."
                .to_owned(),
            input_schema: skill_read_schema(),
        },
    ]
}

fn skill_search_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": { "type": "string", "minLength": 1 },
            "limit": {
                "type": "integer",
                "minimum": 1,
                "maximum": 5,
                "default": 5
            },
            "source": { "type": ["string", "null"] }
        },
        "required": ["query"],
        "additionalProperties": false
    })
}

fn skill_read_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "skill_id": { "type": "string", "minLength": 1 },
            "resource": { "type": ["string", "null"] }
        },
        "required": ["skill_id"],
        "additionalProperties": false
    })
}

fn serving_error(error: ServingError) -> McpError {
    mcp_error_for_kind(error.kind(), error.to_string())
}

fn mcp_error_for_kind(kind: ServingErrorKind, message: impl Into<String>) -> McpError {
    let code = if kind == ServingErrorKind::InvalidQuery {
        ErrorCode(-32602)
    } else {
        ErrorCode(-32004)
    };
    mcp_error(code, kind.as_str(), message)
}

fn mcp_error(code: ErrorCode, kind: &str, message: impl Into<String>) -> McpError {
    McpError::new(
        code,
        message.into(),
        Some(json!({
            "error_kind": kind,
            "write_effect": "none"
        })),
    )
}

fn serialization_error(error: serde_json::Error) -> McpError {
    McpError::internal_error(
        format!("serialize skills catalog response: {error}"),
        Some(json!({
            "error_kind": "internal_error",
            "write_effect": "none"
        })),
    )
}

fn deserialize_present_limit<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: Deserializer<'de>,
{
    usize::deserialize(deserializer).map(Some)
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn error_kind(error: &McpError) -> &str {
    error
        .data
        .as_ref()
        .and_then(|data| data["error_kind"].as_str())
        .unwrap_or("internal_error")
}

fn trace_failure(
    tool: &str,
    source: Option<&str>,
    catalog_revision: Option<&str>,
    error_kind: &str,
    started: Instant,
) {
    let event = if tool == SKILL_READ {
        "skill_read_failed"
    } else {
        "skill_search_failed"
    };
    tracing::warn!(
        event,
        tool,
        source = source.unwrap_or_default(),
        catalog_revision = catalog_revision.unwrap_or_default(),
        result_count = 0,
        error_kind,
        latency_ms = elapsed_millis(started),
        write_effect = "none",
        "skill catalog call failed closed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_deserialization_preserves_defaults_and_exact_source() {
        let defaulted: SkillSearchInput =
            serde_json::from_value(json!({ "query": "verify changes" })).expect("search input");
        assert_eq!(defaulted.limit, None);
        assert_eq!(defaulted.source, None);

        let filtered: SkillSearchInput = serde_json::from_value(json!({
            "query": "verify changes",
            "limit": 5,
            "source": "Git:Example/CaseSensitive"
        }))
        .expect("filtered search input");
        assert_eq!(filtered.limit, Some(5));
        assert_eq!(
            filtered.source.as_deref(),
            Some("Git:Example/CaseSensitive")
        );

        assert!(serde_json::from_value::<SkillSearchInput>(json!({})).is_err());
        assert!(serde_json::from_value::<SkillSearchInput>(json!({
            "query": "verify",
            "limit": null
        }))
        .is_err());
        assert!(serde_json::from_value::<SkillSearchInput>(json!({
            "query": "verify",
            "unexpected": true
        }))
        .is_err());
    }

    #[test]
    fn every_serving_error_kind_is_stable_and_write_free() {
        let kinds = [
            ServingErrorKind::InvalidQuery,
            ServingErrorKind::SkillNotFound,
            ServingErrorKind::SkillNotEligible,
            ServingErrorKind::StaleSkillRef,
            ServingErrorKind::ResourceNotFound,
            ServingErrorKind::ResourceDenied,
            ServingErrorKind::ContentTooLarge,
            ServingErrorKind::IntegrityMismatch,
        ];

        for kind in kinds {
            let error = mcp_error_for_kind(kind, "safe metadata-only error");
            let expected_code = if kind == ServingErrorKind::InvalidQuery {
                ErrorCode(-32602)
            } else {
                ErrorCode(-32004)
            };
            assert_eq!(error.code, expected_code, "unexpected code for {kind:?}");
            assert_eq!(
                error.data,
                Some(json!({
                    "error_kind": kind.as_str(),
                    "write_effect": "none"
                }))
            );
        }

        let serde_error = serde_json::from_str::<Value>("{").expect_err("invalid JSON");
        let error = serialization_error(serde_error);
        assert_eq!(error.code, ErrorCode(-32603));
        assert_eq!(
            error.data,
            Some(json!({
                "error_kind": "internal_error",
                "write_effect": "none"
            }))
        );
    }
}

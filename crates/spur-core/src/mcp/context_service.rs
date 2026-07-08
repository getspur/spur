use async_trait::async_trait;
use rmcp::model::{ErrorCode, ErrorData as McpError};
use serde_json::{json, Value};
use spur_mcp::{ToolCallContext, ToolDefinition, ToolModule, ToolResponse};
use std::time::Duration;

const DEFAULT_SOURCE: &str = "registry:crates-io";
const DEFAULT_INDEX_SOURCE: &str = "git:custom";
const KNOWLEDGE_QUERY_VECTOR_DIMENSIONS: usize = 768;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub struct ContextServiceClient {
    client: reqwest::Client,
    base_url: String,
    bearer_token: Option<String>,
}

impl ContextServiceClient {
    pub fn new(base_url: impl Into<String>, bearer_token: impl Into<String>) -> Self {
        Self::with_optional_token(base_url, Some(bearer_token.into()))
    }

    pub fn with_optional_token(base_url: impl Into<String>, bearer_token: Option<String>) -> Self {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client with static timeout configuration should build");
        Self::with_client(client, base_url, bearer_token)
    }

    fn with_client(
        client: reqwest::Client,
        base_url: impl Into<String>,
        bearer_token: Option<String>,
    ) -> Self {
        Self {
            client,
            base_url: base_url.into(),
            bearer_token: normalize_bearer_token(bearer_token),
        }
    }

    pub fn from_env() -> Option<Self> {
        let base_url = std::env::var("SPUR_CONTEXT_SERVICE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())?;
        let bearer_token = std::env::var("SPUR_CONTEXT_SERVICE_TOKEN")
            .ok()
            .filter(|value| !value.trim().is_empty());
        Some(Self::with_optional_token(base_url, bearer_token))
    }
}

fn normalize_bearer_token(bearer_token: Option<String>) -> Option<String> {
    bearer_token
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[async_trait]
impl ToolModule for ContextServiceClient {
    fn tools(&self) -> Vec<ToolDefinition> {
        tool_definitions()
    }

    async fn call(
        &self,
        ctx: ToolCallContext<'_>,
        name: &str,
        args: Value,
    ) -> Result<ToolResponse, McpError> {
        let mut request = self
            .client
            .post(&self.base_url)
            .json(&json!({ "tool": name, "args": args }));
        if let Some(bearer_token) = &self.bearer_token {
            request = request.bearer_auth(bearer_token);
        }

        let response = request.send().await.map_err(|error| {
            McpError::internal_error(format!("context service request failed: {error}"), None)
        })?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|error| format!("<failed to read response body: {error}>"));
            return Err(McpError::internal_error(
                format!("context service HTTP {status}: {body}"),
                None,
            ));
        }

        let value = response.json::<Value>().await.map_err(|error| {
            McpError::internal_error(
                format!("context service response was not valid JSON: {error}"),
                None,
            )
        })?;

        if let Some(error) = lambda_error_envelope(&value) {
            return Err(error);
        }

        Ok(ToolResponse::json_text(ctx.request_id_value(), value))
    }
}

fn lambda_error_envelope(value: &Value) -> Option<McpError> {
    let error = value.get("error")?;
    let code = i32::try_from(error.get("code")?.as_i64()?).ok()?;
    let message = error.get("message")?.as_str()?.to_owned();
    Some(McpError::new(ErrorCode(code), message, None))
}

pub(crate) fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        external_catalog_def(),
        external_code_search_def(),
        external_code_read_def(),
        external_code_callers_def(),
        external_code_callees_def(),
        external_knowledge_context_def(),
        external_index_def(),
        external_index_status_def(),
    ]
}

fn external_catalog_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_catalog".to_owned(),
        description:
            "Browse indexed external packages, revisions, file tree entries, and file symbols."
                .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "default": DEFAULT_SOURCE,
                    "description": "Package source, for example registry:crates-io or git:github.com/..."
                },
                "package": {
                    "type": "string",
                    "description": "Optional package name. Omit to list indexed packages."
                },
                "revision": {
                    "type": "string",
                    "description": "Exact version or SHA for file tree or symbol descent."
                },
                "ref": {
                    "type": "string",
                    "description": "Branch or tag name. Alternative to revision."
                },
                "path": {
                    "type": "string",
                    "description": "Directory prefix or exact file path inside the package revision."
                },
                "name_filter": {
                    "type": "string",
                    "description": "Optional substring filter for package names or file symbols."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "default": 50
                },
                "cursor": {
                    "type": "string",
                    "description": "Opaque cursor returned by a previous external_catalog response."
                }
            },
            "additionalProperties": false
        }),
    }
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
                "source": {
                    "type": "string",
                    "default": DEFAULT_SOURCE,
                    "description": "Package source, for example registry:crates-io or git:github.com/..."
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
                "source": {
                    "type": "string",
                    "default": DEFAULT_SOURCE,
                    "description": "Package source, for example registry:crates-io or git:github.com/..."
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
                "source": {
                    "type": "string",
                    "default": DEFAULT_SOURCE,
                    "description": "Package source, for example registry:crates-io or git:github.com/..."
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

fn external_index_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_index".to_owned(),
        description: "Queue on-demand indexing for a fetchable external package source.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["package", "revision", "source_url"],
            "properties": {
                "package": {
                    "type": "string",
                    "description": "Package name, for example serde or tokio."
                },
                "revision": {
                    "type": "string",
                    "description": "Version, branch, tag, or SHA to index."
                },
                "source_url": {
                    "type": "string",
                    "description": "Fetchable git or tarball URL for the source."
                },
                "source_kind": {
                    "type": "string",
                    "enum": ["git", "tarball"],
                    "description": "Optional source fetch strategy. When omitted it is inferred from source_url."
                },
                "source": {
                    "type": "string",
                    "default": DEFAULT_INDEX_SOURCE,
                    "description": "Catalog source namespace for this package revision."
                },
                "force": {
                    "type": "boolean",
                    "default": false,
                    "description": "Bypass the warm-path catalog hit check."
                }
            },
            "additionalProperties": false
        }),
    }
}

fn external_index_status_def() -> ToolDefinition {
    ToolDefinition {
        name: "external_index_status".to_owned(),
        description: "Return the queued indexing job status for a job_id.".to_owned(),
        input_schema: json!({
            "type": "object",
            "required": ["job_id"],
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "Job identifier returned by external_index."
                }
            },
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
#[allow(unsafe_code)] // Env mutation is process-global and requires an unsafe block on current Rust.
mod tests {
    use super::*;
    use axum::extract::State;
    use axum::http::header::AUTHORIZATION;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::json;
    use std::sync::{Arc, Mutex};
    use tokio::sync::oneshot;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[derive(Clone)]
    struct MockService {
        response: Value,
        observed: Arc<Mutex<Option<oneshot::Sender<ObservedRequest>>>>,
    }

    #[derive(Debug)]
    struct ObservedRequest {
        authorization: Option<String>,
        body: Value,
    }

    #[tokio::test]
    async fn envelope_success_path_produces_tool_response() {
        let (base_url, observed) = spawn_mock_service(json!({
            "matches": [
                { "selector": "pkg:serde@1.0.0::serde::Deserialize" }
            ]
        }))
        .await;
        let client = ContextServiceClient::new(base_url, "secret-token");
        let request_id = json!("req-1");
        let ctx = ToolCallContext::new(
            spur_mcp::ServerKind::Brain,
            spur_mcp::ToolAuthority::Brain,
            None,
            Some(&request_id),
        );

        let response = client
            .call(
                ctx,
                "external_code_search",
                json!({ "query": "Deserialize", "package": "serde" }),
            )
            .await;

        assert!(
            response.is_ok(),
            "success envelope should produce ToolResponse"
        );
        let observed = observed.await.expect("mock service should receive request");
        assert_eq!(
            observed.authorization.as_deref(),
            Some("Bearer secret-token")
        );
        assert_eq!(
            observed.body,
            json!({
                "tool": "external_code_search",
                "args": { "query": "Deserialize", "package": "serde" }
            })
        );
    }

    #[tokio::test]
    async fn omits_authorization_header_when_token_is_not_configured() {
        let (base_url, observed) = spawn_mock_service(json!({
            "matches": []
        }))
        .await;
        let client = ContextServiceClient::with_optional_token(base_url, None);
        let ctx = ToolCallContext::new(
            spur_mcp::ServerKind::Brain,
            spur_mcp::ToolAuthority::Brain,
            None,
            None,
        );

        client
            .call(
                ctx,
                "external_code_search",
                json!({ "query": "Deserialize", "package": "serde" }),
            )
            .await
            .expect("success envelope should produce ToolResponse");

        let observed = observed.await.expect("mock service should receive request");
        assert_eq!(observed.authorization, None);
    }

    #[tokio::test]
    async fn error_envelope_maps_to_mcp_error() {
        let (base_url, _observed) = spawn_mock_service(json!({
            "error": {
                "code": -32602,
                "message": "field 'package' is required"
            }
        }))
        .await;
        let client = ContextServiceClient::new(base_url, "secret-token");
        let ctx = ToolCallContext::new(
            spur_mcp::ServerKind::Brain,
            spur_mcp::ToolAuthority::Brain,
            None,
            None,
        );

        let err = match client
            .call(
                ctx,
                "external_code_search",
                json!({ "query": "Deserialize" }),
            )
            .await
        {
            Ok(_) => panic!("error envelope should map to McpError"),
            Err(err) => err,
        };

        assert_eq!(err.code, ErrorCode(-32602));
        assert_eq!(err.message, "field 'package' is required");
    }

    #[test]
    fn from_env_returns_none_when_unconfigured() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev_url = std::env::var_os("SPUR_CONTEXT_SERVICE_URL");
        let prev_token = std::env::var_os("SPUR_CONTEXT_SERVICE_TOKEN");

        unsafe {
            std::env::remove_var("SPUR_CONTEXT_SERVICE_URL");
            std::env::remove_var("SPUR_CONTEXT_SERVICE_TOKEN");
        }
        let configured = ContextServiceClient::from_env();
        restore_env("SPUR_CONTEXT_SERVICE_URL", prev_url);
        restore_env("SPUR_CONTEXT_SERVICE_TOKEN", prev_token);

        assert!(configured.is_none());
    }

    #[test]
    fn from_env_accepts_url_without_token() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev_url = std::env::var_os("SPUR_CONTEXT_SERVICE_URL");
        let prev_token = std::env::var_os("SPUR_CONTEXT_SERVICE_TOKEN");

        unsafe {
            std::env::set_var("SPUR_CONTEXT_SERVICE_URL", "https://context.example.test");
            std::env::remove_var("SPUR_CONTEXT_SERVICE_TOKEN");
        }
        let configured = ContextServiceClient::from_env();
        restore_env("SPUR_CONTEXT_SERVICE_URL", prev_url);
        restore_env("SPUR_CONTEXT_SERVICE_TOKEN", prev_token);

        assert!(configured.is_some());
    }

    async fn spawn_mock_service(response: Value) -> (String, oneshot::Receiver<ObservedRequest>) {
        let (tx, rx) = oneshot::channel();
        let state = MockService {
            response,
            observed: Arc::new(Mutex::new(Some(tx))),
        };
        let app = Router::new()
            .route("/", post(mock_handler))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock context service");
        let addr = listener.local_addr().expect("mock local addr");
        tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve mock context service");
        });
        (format!("http://{addr}"), rx)
    }

    async fn mock_handler(
        State(state): State<MockService>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let authorization = headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if let Some(sender) = state
            .observed
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = sender.send(ObservedRequest {
                authorization,
                body,
            });
        }
        Json(state.response)
    }

    fn restore_env(name: &str, value: Option<std::ffi::OsString>) {
        match value {
            Some(value) => unsafe { std::env::set_var(name, value) },
            None => unsafe { std::env::remove_var(name) },
        }
    }
}

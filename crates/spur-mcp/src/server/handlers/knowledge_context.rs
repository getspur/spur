use serde_json::{json, Value};

use crate::handlers::McpHandlerError;

use super::McpCallbackServer;
use super::*;

impl McpCallbackServer {
    pub(crate) async fn handle_knowledge_context_pack(
        &self,
        id: Value,
        args: Value,
    ) -> JsonRpcResponse {
        match knowledge_context_pack(&args).await {
            Ok(result) => {
                let text =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| result.to_string());
                JsonRpcResponse::success(
                    id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                )
            }
            Err(McpHandlerError::InvalidParams(error)) => {
                JsonRpcResponse::invalid_params(id, error)
            }
            Err(error) => JsonRpcResponse::internal_error(id, error.to_string()),
        }
    }
}

pub(crate) async fn knowledge_context_pack(args: &Value) -> Result<Value, McpHandlerError> {
    let request = KnowledgeContextPackRequest::parse(args)?;
    Ok(json!({
        "query": request.query,
        "intent": request.intent,
        "scope": request.scope,
        "limit": request.limit,
        "include_tests": request.include_tests,
        "max_symbol_bodies": request.max_symbol_bodies,
        "answerable": false,
        "confidence": "low",
        "error": {
            "code": "not_implemented"
        }
    }))
}

struct KnowledgeContextPackRequest {
    query: String,
    intent: String,
    scope: String,
    limit: u64,
    include_tests: bool,
    max_symbol_bodies: u64,
}

impl KnowledgeContextPackRequest {
    fn parse(args: &Value) -> Result<Self, McpHandlerError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| {
                McpHandlerError::InvalidParams(
                    "knowledge_context_pack requires non-empty string field 'query'".into(),
                )
            })?
            .to_string();
        let intent = parse_enum(
            args,
            "intent",
            &["explain", "change", "review", "debug", "plan"],
            "explain",
        )?;
        let scope = parse_enum(args, "scope", &["all", "docs", "code", "graph"], "all")?;
        let limit = parse_u64(args, "limit", 8, 1, 20)?;
        let include_tests = args
            .get("include_tests")
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    McpHandlerError::InvalidParams(
                        "knowledge_context_pack field 'include_tests' must be a boolean".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(true);
        let max_symbol_bodies = parse_u64(args, "max_symbol_bodies", 3, 0, 5)?;

        Ok(Self {
            query,
            intent,
            scope,
            limit,
            include_tests,
            max_symbol_bodies,
        })
    }
}

fn parse_enum(
    args: &Value,
    field: &str,
    allowed: &[&str],
    default: &str,
) -> Result<String, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(default.to_string());
    };
    let value = value.as_str().ok_or_else(|| {
        McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be a string"
        ))
    })?;
    if allowed.contains(&value) {
        Ok(value.to_string())
    } else {
        Err(McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be one of {}",
            allowed.join("|")
        )))
    }
}

fn parse_u64(
    args: &Value,
    field: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(default);
    };
    let value = value.as_u64().ok_or_else(|| {
        McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be an integer"
        ))
    })?;
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be between {min} and {max}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn knowledge_context_pack_returns_structured_not_implemented() {
        let result = knowledge_context_pack(&json!({
            "query": "semantic search",
            "intent": "debug",
            "scope": "code",
            "limit": 4,
            "include_tests": false,
            "max_symbol_bodies": 1
        }))
        .await
        .expect("structured response");

        assert_eq!(result["query"], "semantic search");
        assert_eq!(result["intent"], "debug");
        assert_eq!(result["scope"], "code");
        assert_eq!(result["limit"], 4);
        assert_eq!(result["include_tests"], false);
        assert_eq!(result["max_symbol_bodies"], 1);
        assert_eq!(result["answerable"], false);
        assert_eq!(result["confidence"], "low");
        assert_eq!(result["error"]["code"], "not_implemented");
    }

    #[tokio::test]
    async fn knowledge_context_pack_rejects_empty_query() {
        let error = knowledge_context_pack(&json!({ "query": "   " }))
            .await
            .expect_err("empty query must be rejected");
        assert_eq!(error.json_rpc_code(), -32602);
        assert!(
            error.to_string().contains("non-empty string field 'query'"),
            "unexpected error: {error}"
        );
    }
}

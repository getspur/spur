use serde_json::Value;

use crate::mcp::McpHandlerError;
use crate::pack::{service, KnowledgeContextPackRequest, KnowledgeContextPackV2Request};

pub async fn knowledge_context_pack(args: &Value) -> Result<Value, McpHandlerError> {
    let request = KnowledgeContextPackRequest::parse(args)?;
    service::knowledge_context_pack(request).await
}

pub async fn knowledge_context_pack_2(args: &Value) -> Result<Value, McpHandlerError> {
    let request = KnowledgeContextPackV2Request::parse(args)?;
    service::knowledge_context_pack_2(request).await
}

use std::future::Future;

use serde_json::Value;

use crate::mcp::McpHandlerError;

pub fn doc_navigate(args: &Value) -> impl Future<Output = Result<Value, McpHandlerError>> + '_ {
    crate::doc_nav::doc_navigate(args)
}

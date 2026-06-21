use rmcp::ErrorData as McpError;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    pub fn into_mcp_error(self) -> McpError {
        McpError::new(
            rmcp::model::ErrorCode(self.code as i32),
            self.message,
            self.data,
        )
    }
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
        }
    }

    pub fn error_with_data(id: Value, code: i64, message: impl Into<String>, data: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: Some(data),
            }),
        }
    }

    pub fn invalid_params(id: Value, msg: impl Into<String>) -> Self {
        Self::error(id, -32602, msg)
    }

    pub fn internal_error(id: Value, msg: impl Into<String>) -> Self {
        Self::error(id, -32603, msg)
    }

    pub fn mcp_error(id: Value, error: McpError) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: i64::from(error.code.0),
                message: error.message.into_owned(),
                data: error.data,
            }),
        }
    }
}

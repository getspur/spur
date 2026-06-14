use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::BRIDGE_TIMEOUT;
use crate::mcp::ServerDeps;

const METHOD: &str = "notebook_set_schedule";
const BRIDGE_METHOD: &str = "notebook.set_cell_metadata";

#[derive(Debug, Deserialize)]
struct SetScheduleParams {
    cell_id: String,
    trigger: Option<Value>,
    expected_version: u64,
}

#[derive(Debug, Deserialize, Serialize)]
struct SetScheduleResult {
    ok: bool,
    version: u64,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Set or clear recurring schedule metadata on one cell. \
         Pass trigger=null to clear the schedule. Requires expected_version for \
         optimistic concurrency.",
        rmcp_object(json!({
            "type": "object",
            "required": ["cell_id", "trigger", "expected_version"],
            "properties": {
                "cell_id": { "type": "string", "minLength": 1 },
                "trigger": {
                    "anyOf": [
                        {
                            "type": "object",
                            "properties": {
                                "enabled": { "type": "boolean" },
                                "cron": { "type": "string", "minLength": 1 },
                                "timezone": { "type": "string", "minLength": 1 },
                                "run_target": {
                                    "type": "string",
                                    "enum": ["cell_only", "cascade"]
                                },
                                "skip_if_running": { "type": "boolean" },
                                "catch_up": { "type": "boolean" }
                            },
                            "additionalProperties": false
                        },
                        { "type": "null" }
                    ]
                },
                "expected_version": { "type": "integer", "minimum": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let bridge = deps.bridge.as_ref();
    let params: SetScheduleParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook_set_schedule requires { cell_id, trigger, expected_version }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    if params.cell_id.is_empty() {
        return Err(McpError::invalid_params(
            "notebook_set_schedule cell_id must not be empty",
            None,
        ));
    }
    if let Some(trigger) = &params.trigger {
        if !trigger.is_object() {
            return Err(McpError::invalid_params(
                "notebook_set_schedule trigger must be an object or null",
                None,
            ));
        }
    }
    if params.expected_version == 0 {
        return Err(McpError::invalid_params(
            "notebook_set_schedule expected_version must be >= 1",
            None,
        ));
    }

    let value = bridge
        .request(
            BRIDGE_METHOD,
            json!({
                "id": params.cell_id,
                "patch": {
                    "spur": {
                        "cron": params.trigger
                    }
                },
                "expected_version": params.expected_version
            }),
            BRIDGE_TIMEOUT,
        )
        .await
        .map_err(|error| error.into_mcp_error())?;
    let result: SetScheduleResult = serde_json::from_value(value).map_err(|error| {
        McpError::internal_error(
            "invalid notebook.set_cell_metadata bridge response",
            Some(json!({ "error": error.to_string() })),
        )
    })?;

    Ok(CallToolResult::structured(json!(result)))
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use crate::mcp::{
        bridge::{BridgeRequestFuture, BridgeRequester},
        ServerDeps,
    };

    use super::{call, BRIDGE_METHOD};

    #[derive(Default)]
    struct CapturingBridge {
        method: Mutex<Option<&'static str>>,
        params: Mutex<Option<Value>>,
    }

    impl CapturingBridge {
        fn take_request(&self) -> (&'static str, Value) {
            let method = self
                .method
                .lock()
                .expect("method lock")
                .take()
                .expect("bridge method captured");
            let params = self
                .params
                .lock()
                .expect("params lock")
                .take()
                .expect("bridge params captured");
            (method, params)
        }
    }

    impl BridgeRequester for CapturingBridge {
        fn listener_registered(&self) -> bool {
            true
        }

        fn window_alive(&self) -> bool {
            true
        }

        fn notebook_open(&self) -> bool {
            true
        }

        fn request<'a>(
            &'a self,
            method: &'static str,
            params: Value,
            _timeout: std::time::Duration,
        ) -> BridgeRequestFuture<'a> {
            *self.method.lock().expect("method lock") = Some(method);
            *self.params.lock().expect("params lock") = Some(params);
            Box::pin(async { Ok(json!({ "ok": true, "version": 8 })) })
        }
    }

    #[tokio::test]
    async fn sends_bridge_params_with_cron_patch() {
        let bridge = Arc::new(CapturingBridge::default());
        let deps = ServerDeps::from_bridge(bridge.clone());

        let result = call(
            &deps,
            json!({
                "cell_id": "cell-1",
                "trigger": {
                    "enabled": true,
                    "cron": "*/15 * * * *",
                    "timezone": "UTC",
                    "run_target": "cascade",
                    "skip_if_running": true,
                    "catch_up": false
                },
                "expected_version": 7
            }),
        )
        .await
        .expect("set schedule succeeds");

        assert_eq!(
            result.structured_content.expect("structured content"),
            json!({ "ok": true, "version": 8 })
        );
        let (method, params) = bridge.take_request();
        assert_eq!(method, BRIDGE_METHOD);
        assert_eq!(
            params,
            json!({
                "id": "cell-1",
                "patch": {
                    "spur": {
                        "cron": {
                            "enabled": true,
                            "cron": "*/15 * * * *",
                            "timezone": "UTC",
                            "run_target": "cascade",
                            "skip_if_running": true,
                            "catch_up": false
                        }
                    }
                },
                "expected_version": 7
            })
        );
    }

    #[tokio::test]
    async fn sends_null_cron_patch_to_clear_schedule() {
        let bridge = Arc::new(CapturingBridge::default());
        let deps = ServerDeps::from_bridge(bridge.clone());

        call(
            &deps,
            json!({
                "cell_id": "cell-1",
                "trigger": null,
                "expected_version": 7
            }),
        )
        .await
        .expect("clear schedule succeeds");

        let (method, params) = bridge.take_request();
        assert_eq!(method, BRIDGE_METHOD);
        assert_eq!(
            params,
            json!({
                "id": "cell-1",
                "patch": {
                    "spur": {
                        "cron": null
                    }
                },
                "expected_version": 7
            })
        );
    }
}

use std::time::Duration;

use rmcp::model::CallToolResult;
use serde_json::{json, Value};
use spur_notebook::mcp::{
    bridge::{BridgeError, BridgeRequestFuture, BridgeRequester},
    tools::{kernel_info, read_cell, snapshot},
};
use tokio::sync::Mutex;

#[derive(Default)]
struct MockBridge {
    calls: Mutex<Vec<(String, Value)>>,
    responses: Mutex<Vec<Result<Value, BridgeError>>>,
}

impl MockBridge {
    async fn push_response(&self, response: Result<Value, BridgeError>) {
        self.responses.lock().await.push(response);
    }

    async fn calls(&self) -> Vec<(String, Value)> {
        self.calls.lock().await.clone()
    }
}

impl BridgeRequester for MockBridge {
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
        _timeout: Duration,
    ) -> BridgeRequestFuture<'a> {
        Box::pin(async move {
            self.calls.lock().await.push((method.to_string(), params));
            self.responses.lock().await.remove(0)
        })
    }
}

fn structured(result: CallToolResult) -> Value {
    assert_eq!(result.is_error, Some(false));
    result.structured_content.expect("structured content")
}

#[tokio::test]
async fn snapshot_returns_preview_and_blake3_16_hash_for_all_cells() {
    let bridge = MockBridge::default();
    let long_source = format!("{}tail", "a".repeat(200));
    bridge
        .push_response(Ok(json!([
            {
                "id": "code-1",
                "kind": "code",
                "version": 7,
                "exec_count": 3,
                "status": "success",
                "source": long_source
            },
            {
                "id": "markdown-1",
                "kind": "markdown",
                "version": 2,
                "exec_count": null,
                "status": "idle",
                "source": "# Notes"
            }
        ])))
        .await;

    let body = structured(snapshot::call(&bridge).await.expect("snapshot succeeds"));
    let cells = body.as_array().expect("snapshot is an array");

    assert_eq!(cells.len(), 2);
    assert_eq!(cells[0]["id"], "code-1");
    assert_eq!(cells[0]["source_preview"], "a".repeat(160));
    let expected_hash = blake3_16_hex(&long_source);
    assert_eq!(cells[0]["source_hash"], expected_hash);
    assert_eq!(cells[1]["kind"], "markdown");
    assert_eq!(cells[1]["exec_count"], Value::Null);

    assert_eq!(
        bridge.calls().await,
        vec![("notebook.snapshot".to_string(), json!({}))]
    );
}

#[tokio::test]
async fn read_cell_returns_full_source_and_outputs_for_one_cell() {
    let bridge = MockBridge::default();
    bridge
        .push_response(Ok(json!({
            "id": "code-1",
            "kind": "code",
            "version": 4,
            "source": "print('full source')",
            "exec_count": 1,
            "status": "success",
            "outputs": [
                {
                    "output_type": "stream",
                    "name": "stdout",
                    "text": "full output\n"
                }
            ]
        })))
        .await;

    let body = structured(
        read_cell::call(&bridge, json!({ "id": "code-1" }))
            .await
            .expect("read_cell succeeds"),
    );

    assert_eq!(body["id"], "code-1");
    assert_eq!(body["source"], "print('full source')");
    assert_eq!(body["outputs"][0]["text"], "full output\n");
    assert_eq!(
        bridge.calls().await,
        vec![("notebook.read_cell".to_string(), json!({ "id": "code-1" }))]
    );
}

#[tokio::test]
async fn kernel_info_returns_slot_generation_and_usage() {
    let bridge = MockBridge::default();
    bridge
        .push_response(Ok(json!({
            "kernel_id": "notebook:/tmp/demo.ipynb",
            "spec_name": "python3",
            "generation": 2,
            "status": "idle",
            "cpu_pct": 3.5,
            "mem_mb": 128.25
        })))
        .await;

    let body = structured(
        kernel_info::call(&bridge)
            .await
            .expect("kernel_info succeeds"),
    );

    assert_eq!(body["kernel_id"], "notebook:/tmp/demo.ipynb");
    assert_eq!(body["spec_name"], "python3");
    assert_eq!(body["generation"], 2);
    assert_eq!(body["status"], "idle");
    assert_eq!(
        bridge.calls().await,
        vec![("notebook.kernel_info".to_string(), json!({}))]
    );
}

#[tokio::test]
async fn no_registered_notebook_reports_notebook_not_open() {
    let bridge = spur_notebook::mcp::bridge::TauriBridgeRequester::without_app(
        std::sync::Arc::new(spur_notebook::mcp::bridge::AgentBridge::new()),
    );

    let error = kernel_info::call(&bridge)
        .await
        .expect_err("missing active notebook should be a tool error");

    assert_eq!(error.data.unwrap()["code"], "notebook_not_open");
}

#[tokio::test]
async fn handler_notebook_not_open_error_is_preserved_as_mcp_error_data() {
    let bridge = MockBridge::default();
    bridge
        .push_response(Err(BridgeError::Handler {
            code: "notebook_not_open".to_string(),
            message: "No notebook is loaded".to_string(),
        }))
        .await;

    let error = kernel_info::call(&bridge)
        .await
        .expect_err("notebook_not_open is an MCP error");

    assert_eq!(error.data.unwrap()["code"], "notebook_not_open");
}

fn blake3_16_hex(source: &str) -> String {
    let hash = blake3::hash(source.as_bytes());
    hash.as_bytes()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

use std::sync::Arc;
use std::time::Duration;

use rmcp::model::CallToolResult;
use serde_json::{json, Value};
use spur_notebook::mcp::{
    bridge::{BridgeError, BridgeRequestFuture, BridgeRequester},
    tools::{read_cell, snapshot},
    ServerDeps,
};
use tokio::sync::Mutex;

fn deps_with(bridge: Arc<dyn BridgeRequester>) -> ServerDeps {
    ServerDeps::from_bridge(bridge)
}

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
    let bridge = Arc::new(MockBridge::default());
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

    let deps = deps_with(bridge.clone());
    let body = structured(snapshot::call(&deps).await.expect("snapshot succeeds"));
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
    let bridge = Arc::new(MockBridge::default());
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

    let deps = deps_with(bridge.clone());
    let body = structured(
        read_cell::call(&deps, json!({ "id": "code-1" }))
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

fn blake3_16_hex(source: &str) -> String {
    let hash = blake3::hash(source.as_bytes());
    hash.as_bytes()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

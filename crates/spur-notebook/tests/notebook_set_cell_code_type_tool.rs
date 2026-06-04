use std::{sync::Arc, time::Duration};

use rmcp::{model::CallToolResult, ErrorData as McpError};
use serde_json::{json, Value};
use spur_notebook::mcp::{
    bridge::{BridgeError, BridgeRequestFuture, BridgeRequester},
    tools::{insert_cell, notebook_set_cell_code_type},
    ServerDeps,
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
    result.structured_content.expect("structured content")
}

fn error_message(error: McpError) -> String {
    serde_json::to_value(error).expect("error serializes")["message"]
        .as_str()
        .expect("error message")
        .to_string()
}

#[tokio::test]
async fn notebook_insert_cell_requires_code_type_for_code_cells() {
    let bridge = Arc::new(MockBridge::default());
    let deps = ServerDeps::from_bridge(bridge.clone());

    let error = insert_cell::call(&deps, json!({ "kind": "code", "source": "x = 1" }))
        .await
        .expect_err("code cells require code_type");

    assert_eq!(error_message(error), "code_type required for code cells");
    assert!(bridge.calls().await.is_empty());
}

#[tokio::test]
async fn notebook_insert_cell_rejects_code_type_for_non_code_cells() {
    for kind in ["markdown", "raw"] {
        let bridge = Arc::new(MockBridge::default());
        let deps = ServerDeps::from_bridge(bridge.clone());

        let error = insert_cell::call(
            &deps,
            json!({
                "kind": kind,
                "source": "notes",
                "code_type": "python"
            }),
        )
        .await
        .expect_err("non-code cells reject code_type");

        assert_eq!(
            error_message(error),
            "code_type must be absent for non-code cells"
        );
        assert!(bridge.calls().await.is_empty());
    }
}

#[tokio::test]
async fn notebook_insert_cell_forwards_code_type_for_code_cells() {
    let bridge = Arc::new(MockBridge::default());
    bridge
        .push_response(Ok(json!({ "id": "cell-1", "version": 1 })))
        .await;
    let deps = ServerDeps::from_bridge(bridge.clone());

    let body = structured(
        insert_cell::call(
            &deps,
            json!({
                "kind": "code",
                "source": "x = 1",
                "code_type": "python"
            }),
        )
        .await
        .expect("insert cell succeeds"),
    );

    assert_eq!(body, json!({ "id": "cell-1", "version": 1 }));
    assert_eq!(
        bridge.calls().await,
        vec![(
            "notebook.insert_cell".to_string(),
            json!({
                "kind": "code",
                "source": "x = 1",
                "code_type": "python",
                "last_edited_by": "brain"
            })
        )]
    );
}

#[tokio::test]
async fn notebook_set_cell_code_type_builds_spur_code_type_patch_request() {
    let bridge = Arc::new(MockBridge::default());
    bridge
        .push_response(Ok(json!({ "ok": true, "version": 7 })))
        .await;
    let deps = ServerDeps::from_bridge(bridge.clone());

    let body = structured(
        notebook_set_cell_code_type::call(
            &deps,
            json!({
                "id": "cell-1",
                "code_type": "javascript",
                "expected_version": 6
            }),
        )
        .await
        .expect("set cell code_type succeeds"),
    );

    assert_eq!(body, json!({ "ok": true, "version": 7 }));
    assert_eq!(
        bridge.calls().await,
        vec![(
            "notebook.set_cell_metadata".to_string(),
            json!({
                "id": "cell-1",
                "patch": {
                    "spur": {
                        "code_type": "javascript"
                    }
                },
                "expected_version": 6
            })
        )]
    );
}

#[tokio::test]
async fn notebook_set_cell_code_type_rejects_zero_expected_version() {
    let bridge = Arc::new(MockBridge::default());
    let deps = ServerDeps::from_bridge(bridge.clone());

    let error = notebook_set_cell_code_type::call(
        &deps,
        json!({
            "id": "cell-1",
            "code_type": "rust",
            "expected_version": 0
        }),
    )
    .await
    .expect_err("expected_version must be positive");

    assert_eq!(
        error_message(error),
        "notebook_set_cell_code_type expected_version must be >= 1"
    );
    assert!(bridge.calls().await.is_empty());
}

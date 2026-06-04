use std::{sync::Arc, time::Duration};

use rmcp::model::CallToolResult;
use serde_json::{json, Value};
use spur_notebook::mcp::{
    bridge::{BridgeError, BridgeRequestFuture, BridgeRequester},
    tools::notebook_set_dag_metadata,
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

#[tokio::test]
async fn notebook_set_dag_metadata_builds_spur_dag_patch_request() {
    let bridge = Arc::new(MockBridge::default());
    bridge
        .push_response(Ok(json!({ "ok": true, "version": 7 })))
        .await;
    let deps = ServerDeps::from_bridge(bridge.clone());
    let dag = json!({
        "produces": [{ "port": "x", "repr": "arrow" }],
        "consumes": [],
        "source": { "kind": "param", "port": "p" }
    });

    let body = structured(
        notebook_set_dag_metadata::call(
            &deps,
            json!({
                "id": "cell-1",
                "dag": dag,
                "expected_version": 6
            }),
        )
        .await
        .expect("set dag metadata succeeds"),
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
                        "dag": {
                            "produces": [{ "port": "x", "repr": "arrow" }],
                            "consumes": [],
                            "source": { "kind": "param", "port": "p" }
                        }
                    }
                },
                "expected_version": 6
            })
        )]
    );
}

use std::{collections::HashMap, time::Duration};

use rmcp::model::CallToolResult;
use serde_json::{json, Value};
use spur_notebook::mcp::{
    bridge::{BridgeError, BridgeRequestFuture, BridgeRequester},
    tools::{delete_cell, insert_cell, run_cell, write_cell},
};
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
struct MockCell {
    kind: String,
    source: String,
    version: u64,
    last_edited_by: Option<String>,
}

#[derive(Default)]
struct MockNotebook {
    cells: Mutex<HashMap<String, MockCell>>,
    order: Mutex<Vec<String>>,
    next_id: Mutex<u64>,
    run_sources: Mutex<Vec<String>>,
}

impl BridgeRequester for MockNotebook {
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
            match method {
                "notebook.insert_cell" => {
                    let kind = params["kind"].as_str().unwrap().to_string();
                    let source = params["source"].as_str().unwrap().to_string();
                    let after_id = params.get("after_id").and_then(Value::as_str);
                    let mut next_id = self.next_id.lock().await;
                    *next_id += 1;
                    let id = format!("c{}", *next_id);
                    self.cells.lock().await.insert(
                        id.clone(),
                        MockCell {
                            kind,
                            source,
                            version: 1,
                            last_edited_by: params
                                .get("last_edited_by")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        },
                    );
                    let mut order = self.order.lock().await;
                    match after_id.and_then(|after_id| order.iter().position(|id| id == after_id)) {
                        Some(index) => order.insert(index + 1, id.clone()),
                        None => order.push(id.clone()),
                    }
                    Ok(json!({ "id": id, "version": 1 }))
                }
                "notebook.read_cell" => {
                    let id = params["id"].as_str().unwrap();
                    let cells = self.cells.lock().await;
                    let cell = cells.get(id).unwrap();
                    Ok(json!({
                        "id": id,
                        "kind": &cell.kind,
                        "version": cell.version,
                        "source": &cell.source,
                        "exec_count": null,
                        "status": "idle",
                        "outputs": []
                    }))
                }
                "notebook.kernel_info" => Ok(json!({
                    "kernel_id": "kernel-1",
                    "spec_name": "python3",
                    "generation": 1,
                    "status": "idle",
                    "cpu_pct": 0.0,
                    "mem_mb": 0.0
                })),
                "notebook.run_cell" => {
                    let id = params["id"].as_str().unwrap();
                    let cells = self.cells.lock().await;
                    let cell = cells.get(id).unwrap();
                    self.run_sources.lock().await.push(cell.source.clone());
                    let (exec_count, outputs) = match cell.source.as_str() {
                        "x = 2 + 2" => (1, json!([])),
                        "print(x)" => (
                            2,
                            json!([
                                {
                                    "output_type": "stream",
                                    "name": "stdout",
                                    "text": "4\n"
                                }
                            ]),
                        ),
                        "print(x + 1)" => (
                            3,
                            json!([
                                {
                                    "output_type": "stream",
                                    "name": "stdout",
                                    "text": "5\n"
                                }
                            ]),
                        ),
                        _ => (0, json!([])),
                    };
                    Ok(json!({
                        "id": id,
                        "status": "success",
                        "exec_count": exec_count,
                        "outputs": outputs,
                        "events": []
                    }))
                }
                "notebook.write_cell" => {
                    let id = params["id"].as_str().unwrap();
                    let expected_version = params["expected_version"].as_u64().unwrap();
                    let source = params["source"].as_str().unwrap().to_string();
                    let mut cells = self.cells.lock().await;
                    let cell = cells.get_mut(id).unwrap();
                    if cell.version != expected_version {
                        return Err(BridgeError::Handler {
                            code: "stale_version".to_string(),
                            message: "Cell version is stale".to_string(),
                        });
                    }
                    cell.source = source;
                    cell.version += 1;
                    cell.last_edited_by = params
                        .get("last_edited_by")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    Ok(json!({ "version": cell.version }))
                }
                "notebook.delete_cell" => {
                    let id = params["id"].as_str().unwrap();
                    let expected_version = params["expected_version"].as_u64().unwrap();
                    let mut cells = self.cells.lock().await;
                    let cell = cells.get(id).unwrap();
                    if cell.version != expected_version {
                        return Err(BridgeError::Handler {
                            code: "stale_version".to_string(),
                            message: "Cell version is stale".to_string(),
                        });
                    }
                    cells.remove(id);
                    self.order.lock().await.retain(|cell_id| cell_id != id);
                    Ok(json!({ "deleted": true }))
                }
                _ => unreachable!("unexpected method {method}"),
            }
        })
    }
}

fn structured(result: CallToolResult) -> Value {
    assert_eq!(result.is_error, Some(false));
    result.structured_content.expect("structured content")
}

#[tokio::test]
async fn m5_smoke_sequence_runs_against_in_process_kernel_mock() {
    let notebook = MockNotebook::default();
    let mut progress = run_cell::RecordingProgress::default();

    let c1 = structured(
        insert_cell::call(&notebook, json!({ "kind": "code", "source": "x = 2 + 2" }))
            .await
            .unwrap(),
    );
    assert_eq!(c1["version"], 1);

    structured(
        run_cell::call_with_progress(&notebook, json!({ "id": c1["id"] }), &mut progress)
            .await
            .unwrap(),
    );

    let c2 = structured(
        insert_cell::call(
            &notebook,
            json!({
                "after_id": c1["id"],
                "kind": "code",
                "source": "print(x)"
            }),
        )
        .await
        .unwrap(),
    );

    structured(
        run_cell::call_with_progress(&notebook, json!({ "id": c2["id"] }), &mut progress)
            .await
            .unwrap(),
    );

    let write = structured(
        write_cell::call(
            &notebook,
            json!({
                "id": c2["id"],
                "source": "print(x + 1)",
                "expected_version": 1
            }),
        )
        .await
        .unwrap(),
    );
    assert_eq!(write["version"], 2);
    {
        let cells = notebook.cells.lock().await;
        assert_eq!(
            cells
                .get(c1["id"].as_str().unwrap())
                .unwrap()
                .last_edited_by
                .as_deref(),
            Some("brain")
        );
        assert_eq!(
            cells
                .get(c2["id"].as_str().unwrap())
                .unwrap()
                .last_edited_by
                .as_deref(),
            Some("brain")
        );
    }

    structured(
        run_cell::call_with_progress(&notebook, json!({ "id": c2["id"] }), &mut progress)
            .await
            .unwrap(),
    );

    assert!(progress.events().is_empty());
    assert_eq!(
        *notebook.run_sources.lock().await,
        vec![
            "x = 2 + 2".to_string(),
            "print(x)".to_string(),
            "print(x + 1)".to_string()
        ]
    );

    let stale = write_cell::call(
        &notebook,
        json!({
            "id": c2["id"],
            "source": "print(0)",
            "expected_version": 1
        }),
    )
    .await
    .expect_err("stale version is rejected");
    assert_eq!(stale.data.unwrap()["code"], "stale_version");

    structured(
        delete_cell::call(&notebook, json!({ "id": c2["id"], "expected_version": 2 }))
            .await
            .unwrap(),
    );
}

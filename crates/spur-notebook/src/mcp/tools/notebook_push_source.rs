use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    commands::{push_validated_source_intent, ExternalSourcePushIntent, SourcePushIntentOrigin},
    dag::SourcePayload,
    mcp::{tools::parse_byte_payload, ServerDeps},
};

const METHOD: &str = "notebook_push_source";

#[derive(Debug, Deserialize)]
struct PushSourceParams {
    port: String,
    payload: Value,
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Push Arrow IPC bytes into a declared notebook source port and queue the reactive engine.",
        rmcp_object(json!({
            "type": "object",
            "required": ["port", "payload"],
            "properties": {
                "port": { "type": "string", "minLength": 1 },
                "payload": {
                    "type": "array",
                    "items": { "type": "integer", "minimum": 0, "maximum": 255 }
                }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    let params: PushSourceParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            "notebook_push_source requires { port, payload }",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let payload = parse_byte_payload(METHOD, params.payload)?;
    let state = deps.state.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_push_source requires notebook daemon state",
            Some(json!({ "code": "notebook_state_unavailable" })),
        )
    })?;
    let daemon = deps.daemon.as_ref().ok_or_else(|| {
        McpError::internal_error(
            "notebook_push_source requires notebook daemon control",
            Some(json!({ "code": "daemon_unavailable" })),
        )
    })?;
    let engine = daemon.reactive_engine_client().await.ok_or_else(|| {
        McpError::internal_error(
            "notebook_push_source reactive engine is unavailable",
            Some(json!({ "code": "reactive_engine_unavailable" })),
        )
    })?;

    let (root, _) = state.get_notebook().snapshot();
    push_validated_source_intent(
        &root,
        &engine,
        ExternalSourcePushIntent {
            origin: SourcePushIntentOrigin::Agent { tool: METHOD },
            port: params.port.clone(),
            payload: SourcePayload::IpcBytes(payload),
        },
    )
    .await
    .map_err(source_push_error_for_mcp)?;

    Ok(CallToolResult::structured(json!({
        "port": params.port,
        "accepted": true,
    })))
}

fn source_push_error_for_mcp(error: crate::commands::SourcePushIntentError) -> McpError {
    match error.code {
        "source_push_failed" => McpError::internal_error(
            "notebook_push_source failed to queue source push",
            Some(json!({ "error": error.message, "code": error.code })),
        ),
        "source_port_not_declared" => McpError::invalid_params(
            "notebook_push_source source port is not declared",
            Some(json!({ "port": error.port, "code": error.code })),
        ),
        "ambiguous_source_port" => McpError::invalid_params(
            "notebook_push_source source port is ambiguous",
            Some(json!({ "port": error.port, "code": error.code })),
        ),
        "invalid_source_port" => McpError::invalid_params(
            "notebook_push_source port must not be empty",
            Some(json!({ "port": error.port, "code": error.code })),
        ),
        _ => McpError::invalid_params(
            error.message,
            Some(json!({ "port": error.port, "code": error.code })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::{path::Path, sync::Arc};

    use jute::{
        backend::notebook::{
            Cell, CellDagMetadata, CellMetadata, CodeCell, DagSource, MultilineString,
            NotebookMetadata, NotebookRoot, PortSpec, SpurCellMetadata,
        },
        state::State,
    };
    use serde_json::{json, Value};
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    use crate::{
        dag::{ReactiveEngineClient, SourcePayload, SourcePush},
        mcp::{
            bridge::{AgentBridge, BridgeError, BridgeRequestFuture, BridgeRequester},
            DaemonWindowOps, NotebookDaemonControl, ServerDeps,
        },
    };

    #[derive(Default)]
    struct TestBridge;

    impl BridgeRequester for TestBridge {
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
            _method: &'static str,
            _params: Value,
            _timeout: std::time::Duration,
        ) -> BridgeRequestFuture<'a> {
            Box::pin(async { Ok(Value::Null) })
        }
    }

    #[derive(Default)]
    struct TestWindows;

    impl DaemonWindowOps for TestWindows {
        fn show_and_focus(&self, _label: &str) -> bool {
            false
        }

        fn hide(&self, _label: &str) {}

        fn open_notebook_path(&self, _path: &Path) -> Result<String, BridgeError> {
            Ok("test".to_string())
        }

        fn emit_recents_changed(&self, _event: &jute::commands::RecentsChangedEvent) {}

        fn exit(&self) {}
    }

    fn notebook(cells: Vec<Cell>) -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Default::default(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells,
        }
    }

    fn cell(id: &str, dag: CellDagMetadata) -> Cell {
        Cell::Code(CodeCell {
            id: Some(id.to_string()),
            metadata: CellMetadata {
                spur: Some(SpurCellMetadata {
                    version: 1,
                    last_edited_by: None,
                    datasource_setup: None,
                    dag: Some(dag),
                    code_type: None,
                    frontend: None,
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single("print('ok')".to_string()),
            execution_count: None,
            outputs: Vec::new(),
        })
    }

    fn dag(source: Option<DagSource>) -> CellDagMetadata {
        CellDagMetadata {
            produces: vec![PortSpec {
                port: "raw".to_string(),
                repr: "arrow".to_string(),
                display: None,
            }],
            consumes: Vec::new(),
            source,
        }
    }

    async fn deps(root: NotebookRoot) -> (ServerDeps, mpsc::Receiver<SourcePush>, TempDir) {
        let temp = TempDir::new().expect("temp dir");
        let state = Arc::new(State::new());
        state
            .get_notebook()
            .load(temp.path().join("nb.ipynb"), root);
        let control = NotebookDaemonControl::new_with_parts_for_test(
            Arc::new(AgentBridge::new()),
            Arc::new(TestBridge),
            Arc::clone(&state),
            Arc::new(TestWindows),
            None,
        );
        control
            .set_current_path_for_test(temp.path().join("nb.ipynb"))
            .await;
        let (tx, rx) = mpsc::channel(4);
        control
            .set_reactive_engine_client(ReactiveEngineClient::new(tx))
            .await;
        (
            ServerDeps {
                bridge: Arc::new(TestBridge),
                state: Some(state),
                app: None,
                daemon: Some(control),
                plugins: None,
            },
            rx,
            temp,
        )
    }

    #[tokio::test]
    async fn queues_declared_source_port_for_reactive_engine() {
        let (deps, mut rx, _temp) = deps(notebook(vec![cell(
            "source",
            dag(Some(DagSource {
                kind: "csv".to_string(),
                port: "sales".to_string(),
            })),
        )]))
        .await;

        let result = super::call(&deps, json!({ "port": "sales", "payload": [1, 2, 3] }))
            .await
            .expect("push source succeeds");

        let body = result.structured_content.expect("structured content");
        assert_eq!(body["port"], "sales");
        assert_eq!(body["accepted"], true);
        let push = rx.recv().await.expect("source push queued");
        assert_eq!(push.source.kind, "csv");
        assert_eq!(push.source.port, "sales");
        assert_eq!(push.payload, SourcePayload::IpcBytes(vec![1, 2, 3]));
    }

    #[tokio::test]
    async fn rejects_port_that_is_not_declared_as_source() {
        let (deps, _rx, _temp) = deps(notebook(vec![cell("plain", dag(None))])).await;

        let error = super::call(&deps, json!({ "port": "sales", "payload": [1] }))
            .await
            .expect_err("undeclared source is invalid");

        assert!(error.message.contains("source port is not declared"));
    }
}

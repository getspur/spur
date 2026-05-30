use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use jute::{
    backend::notebook::{
        Cell, CellMetadata, CodeCell, MultilineString, NotebookMetadata, NotebookRoot,
        SpurCellMetadata,
    },
    state::State,
};
use rmcp::model::CallToolResult;
use serde_json::{json, Value};
use spur_notebook::mcp::{
    bridge::{AgentBridge, BridgeError, BridgeRequestFuture, BridgeRequester},
    tools::{delete_cell, insert_cell, write_cell},
    DaemonControlRequest, DaemonWindowOps, NotebookDaemonControl, ServerDeps,
};
use tokio::sync::Mutex;

fn daemon_request(command: jute::commands::DaemonControlCommand) -> DaemonControlRequest {
    DaemonControlRequest {
        id: None,
        request: jute::commands::DaemonControlRequest::new(command),
    }
}

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
    flush_path: Mutex<Option<PathBuf>>,
    calls: Mutex<Vec<String>>,
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
            self.calls.lock().await.push(method.to_string());
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
                "notebook.load" => {
                    let path = PathBuf::from(params["path"].as_str().unwrap());
                    let contents = tokio::fs::read_to_string(&path).await.unwrap();
                    let root: NotebookRoot = serde_json::from_str(&contents).unwrap();
                    self.load_notebook(path, root).await;
                    Ok(Value::Null)
                }
                _ => unreachable!("unexpected method {method}"),
            }
        })
    }

    fn flush_pending<'a>(&'a self, _timeout: Duration) -> BridgeRequestFuture<'a> {
        Box::pin(async move {
            self.calls
                .lock()
                .await
                .push("notebook.flush_pending".to_string());
            let path = self
                .flush_path
                .lock()
                .await
                .clone()
                .expect("flush path is set");
            Ok(json!({
                "path": path,
                "contents": self.export_notebook().await
            }))
        })
    }
}

impl MockNotebook {
    async fn load_single_code_cell(&self, path: PathBuf, id: &str, source: &str) {
        self.flush_path.lock().await.replace(path);
        self.cells.lock().await.insert(
            id.to_string(),
            MockCell {
                kind: "code".to_string(),
                source: source.to_string(),
                version: 1,
                last_edited_by: None,
            },
        );
        self.order.lock().await.push(id.to_string());
    }

    async fn load_notebook(&self, path: PathBuf, root: NotebookRoot) {
        let mut loaded_cells = HashMap::new();
        let mut loaded_order = Vec::new();
        for (index, cell) in root.cells.into_iter().enumerate() {
            let (kind, id, source, version, last_edited_by) = match cell {
                Cell::Raw(cell) => (
                    "raw",
                    cell.id,
                    source_text(&cell.source),
                    cell.metadata.spur.as_ref().map_or(1, |spur| spur.version),
                    cell.metadata
                        .spur
                        .and_then(|spur| spur.last_edited_by.clone()),
                ),
                Cell::Markdown(cell) => (
                    "markdown",
                    cell.id,
                    source_text(&cell.source),
                    cell.metadata.spur.as_ref().map_or(1, |spur| spur.version),
                    cell.metadata
                        .spur
                        .and_then(|spur| spur.last_edited_by.clone()),
                ),
                Cell::Code(cell) => (
                    "code",
                    cell.id,
                    source_text(&cell.source),
                    cell.metadata.spur.as_ref().map_or(1, |spur| spur.version),
                    cell.metadata
                        .spur
                        .and_then(|spur| spur.last_edited_by.clone()),
                ),
            };
            let id = id.unwrap_or_else(|| format!("loaded-{index}"));
            loaded_order.push(id.clone());
            loaded_cells.insert(
                id,
                MockCell {
                    kind: kind.to_string(),
                    source,
                    version,
                    last_edited_by,
                },
            );
        }
        let next_id = loaded_order.len() as u64;
        *self.cells.lock().await = loaded_cells;
        *self.order.lock().await = loaded_order;
        *self.next_id.lock().await = next_id;
        self.flush_path.lock().await.replace(path);
    }

    async fn calls(&self) -> Vec<String> {
        self.calls.lock().await.clone()
    }

    async fn export_notebook(&self) -> NotebookRoot {
        let cells = self.cells.lock().await;
        let order = self.order.lock().await;
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
            cells: order
                .iter()
                .map(|id| {
                    let cell = cells.get(id).expect("cell in order exists");
                    Cell::Code(CodeCell {
                        id: Some(id.clone()),
                        metadata: CellMetadata {
                            spur: Some(SpurCellMetadata {
                                version: cell.version,
                                last_edited_by: cell.last_edited_by.clone(),
                                datasource_setup: None,
                                dag: None,
                            }),
                            jute_deck: None,
                            other: Default::default(),
                        },
                        source: MultilineString::Single(cell.source.clone()),
                        execution_count: None,
                        outputs: Vec::new(),
                    })
                })
                .collect(),
        }
    }
}

#[derive(Default)]
struct RecordingWindowOps {
    opened: StdMutex<Vec<PathBuf>>,
    path_to_check: StdMutex<Option<PathBuf>>,
    switch_path: StdMutex<Option<PathBuf>>,
    source_seen_before_switch_open: StdMutex<Option<String>>,
}

impl RecordingWindowOps {
    fn check_before_opening_switch(&self, path_to_check: PathBuf, switch_path: PathBuf) {
        self.path_to_check
            .lock()
            .expect("path_to_check lock")
            .replace(path_to_check);
        self.switch_path
            .lock()
            .expect("switch_path lock")
            .replace(switch_path);
    }

    fn opened(&self) -> Vec<PathBuf> {
        self.opened.lock().expect("opened lock").clone()
    }

    fn source_seen_before_switch_open(&self) -> Option<String> {
        self.source_seen_before_switch_open
            .lock()
            .expect("source lock")
            .clone()
    }
}

impl DaemonWindowOps for RecordingWindowOps {
    fn show_and_focus(&self, _label: &str) -> bool {
        false
    }

    fn hide(&self, _label: &str) {}

    fn open_notebook_path(&self, path: &Path) -> Result<String, BridgeError> {
        let should_check = self
            .switch_path
            .lock()
            .expect("switch_path lock")
            .as_ref()
            .is_some_and(|switch_path| switch_path == path);
        if should_check {
            let path_to_check = self
                .path_to_check
                .lock()
                .expect("path_to_check lock")
                .clone()
                .expect("path to check is set");
            self.source_seen_before_switch_open
                .lock()
                .expect("source lock")
                .replace(first_source_on_disk(&path_to_check));
        }

        let mut opened = self.opened.lock().expect("opened lock");
        opened.push(path.to_path_buf());
        Ok(format!("window-{}", opened.len()))
    }

    fn emit_recents_changed(&self, _event: &jute::commands::RecentsChangedEvent) {}

    fn exit(&self) {}
}

fn structured(result: CallToolResult) -> Value {
    assert_eq!(result.is_error, Some(false));
    result.structured_content.expect("structured content")
}

fn first_source(contents: &NotebookRoot) -> String {
    let Cell::Code(cell) = &contents.cells[0] else {
        panic!("expected code cell");
    };
    source_text(&cell.source)
}

fn source_text(source: &MultilineString) -> String {
    match source {
        MultilineString::Single(source) => source.clone(),
        MultilineString::Multi(lines) => lines.join(""),
    }
}

fn first_source_on_disk(path: &Path) -> String {
    let contents = std::fs::read_to_string(path).expect("notebook reads");
    let parsed: NotebookRoot = serde_json::from_str(&contents).expect("notebook parses");
    first_source(&parsed)
}

async fn write_notebook(path: &Path, notebook: &NotebookRoot) {
    tokio::fs::write(path, serde_json::to_vec_pretty(notebook).unwrap())
        .await
        .expect("notebook writes");
}

#[test]
fn legacy_tauri_daemon_commands_are_not_exported() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let main_rs = std::fs::read_to_string(manifest_dir.join("src/main.rs")).expect("read main.rs");
    let commands_rs =
        std::fs::read_to_string(manifest_dir.join("jute-notebook/src-tauri/src/commands.rs"))
            .expect("read commands.rs");
    let home_page =
        std::fs::read_to_string(manifest_dir.join("jute-notebook/src/pages/HomePage.tsx"))
            .expect("read HomePage.tsx");

    assert!(
        main_rs.contains("jute::commands::daemon_control"),
        "typed daemon_control command must be registered"
    );

    for command in [
        "list_recent_notebooks",
        "remove_notebook_from_recents",
        "set_notebook_pinned",
        "open_notebook_via_daemon",
        "rename_notebook",
        "new_notebook_via_daemon",
        "new_notebook_at_via_daemon",
        "reopen_notebook_via_daemon",
        "close_notebook_via_daemon",
    ] {
        assert!(
            !main_rs.contains(&format!("jute::commands::{command}")),
            "{command} must not be registered as a Tauri command"
        );
        assert!(
            !commands_rs.contains(&format!("pub async fn {command}")),
            "{command} must not remain a public Tauri command"
        );
        assert!(
            !home_page.contains(&format!("\"{command}\"")),
            "HomePage must not invoke legacy Tauri command {command}"
        );
    }
}

#[tokio::test]
async fn m5_smoke_sequence_runs_against_in_process_kernel_mock() {
    let notebook = Arc::new(MockNotebook::default());
    let deps = ServerDeps::from_bridge(notebook.clone());

    let c1 = structured(
        insert_cell::call(&deps, json!({ "kind": "code", "source": "x = 2 + 2" }))
            .await
            .unwrap(),
    );
    assert_eq!(c1["version"], 1);

    let c2 = structured(
        insert_cell::call(
            &deps,
            json!({
                "after_id": c1["id"],
                "kind": "code",
                "source": "print(x)"
            }),
        )
        .await
        .unwrap(),
    );

    let write = structured(
        write_cell::call(
            &deps,
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

    let stale = write_cell::call(
        &deps,
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
        delete_cell::call(&deps, json!({ "id": c2["id"], "expected_version": 2 }))
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn daemon_open_flushes_pending_browser_edit_before_opening_next_notebook() {
    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-m5-daemon-flush-")
        .tempdir()
        .expect("temp dir");
    let notebook_a = temp_dir.path().join("a.ipynb");
    let notebook_b = temp_dir.path().join("b.ipynb");
    let last_record_path = temp_dir.path().join("last.json");

    let notebook = Arc::new(MockNotebook::default());
    notebook
        .load_single_code_cell(notebook_a.clone(), "c1", "original")
        .await;
    write_notebook(&notebook_a, &notebook.export_notebook().await).await;
    write_notebook(&notebook_b, &notebook.export_notebook().await).await;

    let windows = Arc::new(RecordingWindowOps::default());
    windows.check_before_opening_switch(notebook_a.clone(), notebook_b.clone());
    let control = NotebookDaemonControl::new_with_parts_for_test(
        Arc::new(AgentBridge::new()),
        notebook.clone(),
        Arc::new(State::new()),
        windows.clone(),
        Some(last_record_path),
    );

    let first_open = control
        .handle(daemon_request(jute::commands::DaemonControlCommand::Open {
            path: notebook_a.display().to_string(),
        }))
        .await;
    assert!(first_open.ok, "{:?}", first_open.error);

    let deps = ServerDeps::from_bridge(notebook.clone());
    let write = structured(
        write_cell::call(
            &deps,
            json!({
                "id": "c1",
                "source": "edited before switch",
                "expected_version": 1
            }),
        )
        .await
        .unwrap(),
    );
    assert_eq!(write["version"], 2);

    let second_open = control
        .handle(daemon_request(jute::commands::DaemonControlCommand::Open {
            path: notebook_b.display().to_string(),
        }))
        .await;
    assert!(second_open.ok, "{:?}", second_open.error);

    assert_eq!(
        windows.opened(),
        vec![notebook_a.clone(), notebook_b.clone()]
    );
    assert_eq!(
        windows.source_seen_before_switch_open().as_deref(),
        Some("edited before switch")
    );
    assert_eq!(first_source_on_disk(&notebook_a), "edited before switch");
    assert_eq!(
        notebook.calls().await,
        vec![
            "notebook.load",
            "notebook.write_cell",
            "notebook.flush_pending",
            "notebook.load"
        ]
    );
}

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use jute::state::{notebook_slot_id, KernelSlot, State};
use jute::{
    backend::notebook::{
        Cell, CellMetadata, CodeCell, MultilineString, NotebookMetadata, NotebookRoot,
        SpurCellMetadata,
    },
    commands::{install_kernel_in_slot, start_local_kernel},
};
use rmcp::model::CallToolResult;
use serde_json::{json, Value};
#[cfg(feature = "datasource-introspect")]
use spur_notebook::mcp::tools::list_datasources;
use spur_notebook::{
    dag::{notebook_port_root, PortStore},
    mcp::{
        bridge::{
            AgentBridge, BridgeError, BridgeRequestFuture, BridgeRequester, TauriBridgeRequester,
        },
        tools::{kernel_info, read_cell, run_cell, save, snapshot, start_kernel, stop_kernel},
        DaemonControlRequest, DaemonWindowOps, NotebookDaemonControl, ServerDeps,
    },
};
use tokio::sync::Mutex;
use tokio::time::timeout;

fn daemon_request(command: jute::commands::DaemonControlCommand) -> DaemonControlRequest {
    DaemonControlRequest {
        id: None,
        request: jute::commands::DaemonControlRequest::new(command),
    }
}

fn deps_with(bridge: Arc<dyn BridgeRequester>) -> ServerDeps {
    ServerDeps::from_bridge(bridge)
}

fn deps_with_state(state: Arc<State>) -> ServerDeps {
    ServerDeps {
        bridge: Arc::new(TauriBridgeRequester::without_app(Arc::new(
            AgentBridge::new(),
        ))),
        state: Some(state),
        app: None,
        daemon: None,
    }
}

fn deps_with_state_and_daemon(state: Arc<State>, daemon: NotebookDaemonControl) -> ServerDeps {
    ServerDeps {
        bridge: Arc::new(TauriBridgeRequester::without_app(Arc::new(
            AgentBridge::new(),
        ))),
        state: Some(state),
        app: None,
        daemon: Some(daemon),
    }
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

#[derive(Default)]
struct RecordingWindowOps {
    opened: StdMutex<Vec<PathBuf>>,
}

impl DaemonWindowOps for RecordingWindowOps {
    fn show_and_focus(&self, _label: &str) -> bool {
        false
    }

    fn hide(&self, _label: &str) {}

    fn open_notebook_path(&self, path: &Path) -> Result<String, BridgeError> {
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

fn combined_stream_text(outputs: &[Value]) -> String {
    outputs
        .iter()
        .filter_map(|event| {
            event
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| event.get("data").and_then(Value::as_str))
        })
        .collect::<Vec<_>>()
        .join("")
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

#[tokio::test]
async fn kernel_info_returns_slot_generation_and_usage() {
    let state = Arc::new(State::new());
    let slot_id = "notebook:/tmp/notebook_read_tools.ipynb".to_string();
    state
        .kernels
        .insert(slot_id.clone(), KernelSlot::new("python3".to_string()));

    let deps = deps_with_state(state);
    let body = structured(
        kernel_info::call(&deps, json!({ "kernel_id": slot_id }))
            .await
            .expect("kernel_info succeeds"),
    );

    assert_eq!(body["kernel_id"], slot_id);
    assert_eq!(body["spec_name"], "python3");
    // Empty slot reports dead with generation 0 and zeroed usage.
    assert_eq!(body["status"], "dead");
    assert_eq!(body["generation"], 0);
    assert!(body["cpu_pct"].is_number());
    assert!(body["mem_mb"].is_number());
}

#[tokio::test]
async fn save_writes_notebook_through_save_coordinator() {
    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-it-save-")
        .tempdir()
        .expect("temp dir");
    let path = temp_dir.path().join("saved.ipynb");
    let contents = json!({
        "metadata": {},
        "nbformat_minor": 5,
        "nbformat": 4,
        "cells": [
            { "cell_type": "markdown", "id": "c1", "metadata": {}, "source": "hello" }
        ]
    });
    let deps = deps_with_state(Arc::new(State::new()));

    let body = structured(
        save::call(
            &deps,
            json!({
                "path": path.display().to_string(),
                "contents": contents,
            }),
        )
        .await
        .expect("save succeeds"),
    );
    assert_eq!(body["ok"], true);

    let written: Value = serde_json::from_slice(
        &tokio::fs::read(&path)
            .await
            .expect("notebook file was written"),
    )
    .expect("written notebook is valid JSON");
    assert_eq!(written["cells"][0]["source"], "hello");
    assert_eq!(written["nbformat"], 4);
    // deps.app is None so the notebook://saved emit path is skipped; that
    // branch is exercised by the daemon integration tests where a Tauri
    // AppHandle is wired up.
}

// Live-kernel tests below need a working python3 kernel on the host (and a
// usable spawn path through start_local_kernel). They're gated behind
// `#[ignore]` so the default `cargo test -p spur-notebook --test
// notebook_read_tools` invocation stays hermetic; run explicitly with
// `cargo test -p spur-notebook --test notebook_read_tools -- --ignored`.

static TEST_KERNELSPEC_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &OsStr) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.as_ref() {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

struct TestKernelSpec {
    _env_lock: tokio::sync::MutexGuard<'static, ()>,
    _jupyter_path: EnvVarGuard,
    _temp_dir: tempfile::TempDir,
}

struct TestDenoKernelSpec {
    _env_lock: tokio::sync::MutexGuard<'static, ()>,
    _home: EnvVarGuard,
    _runtime_dir: EnvVarGuard,
    _deno_path: EnvVarGuard,
    _temp_dir: tempfile::TempDir,
}

async fn command_succeeds(command: &str, args: &[&str]) -> bool {
    let Ok(Ok(status)) = timeout(
        Duration::from_secs(120),
        tokio::process::Command::new(command)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await
    else {
        return false;
    };
    status.success()
}

fn deno_binary_for_test() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("DENO_PATH").map(PathBuf::from) {
        if path.is_absolute() && path.exists() {
            return Some(path);
        }
    }

    let paths = std::env::var_os("PATH")?;
    let names = if cfg!(windows) {
        vec!["deno.exe", "deno"]
    } else {
        vec!["deno"]
    };
    for dir in std::env::split_paths(&paths) {
        for name in &names {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    None
}

async fn python_modules_available(python: &str, modules: &[&str]) -> bool {
    let code = modules
        .iter()
        .map(|module| format!("import {module}"))
        .collect::<Vec<_>>()
        .join("; ");
    command_succeeds(python, &["-c", &code]).await
}

async fn uv_modules_available(packages: &[&str], modules: &[&str]) -> bool {
    let code = modules
        .iter()
        .map(|module| format!("import {module}"))
        .collect::<Vec<_>>()
        .join("; ");
    let mut command = tokio::process::Command::new("uv");
    command.arg("run");
    for package in packages {
        command.args(["--with", package]);
    }
    command.args(["python", "-c", &code]);

    let Ok(Ok(status)) = timeout(
        Duration::from_secs(120),
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await
    else {
        return false;
    };
    status.success()
}

async fn install_python_packages_into(python: &str, target: &Path, packages: &[&str]) -> bool {
    let Ok(Ok(status)) = timeout(
        Duration::from_secs(240),
        tokio::process::Command::new(python)
            .args(["-m", "pip", "install", "--quiet", "--target"])
            .arg(target)
            .args(packages)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await
    else {
        return false;
    };
    status.success()
}

async fn install_test_python3_kernelspec() -> Option<TestKernelSpec> {
    install_test_python3_kernelspec_with(&[]).await
}

async fn install_test_deno_kernelspec() -> Option<TestDenoKernelSpec> {
    let Some(deno) = deno_binary_for_test() else {
        eprintln!("skipping Deno port contract test: deno binary is not available");
        return None;
    };
    let deno_string = deno.to_string_lossy().into_owned();
    if !command_succeeds(&deno_string, &["--version"]).await {
        eprintln!(
            "skipping Deno port contract test: deno binary did not run: {}",
            deno.display()
        );
        return None;
    }

    let env_lock = TEST_KERNELSPEC_ENV.lock().await;
    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-it-deno-kernelspec-")
        .tempdir()
        .expect("temp dir");
    let home = temp_dir.path().join("home");
    let runtime_dir = temp_dir.path().join("runtime");
    tokio::fs::create_dir_all(&home).await.expect("home dir");
    tokio::fs::create_dir_all(&runtime_dir)
        .await
        .expect("runtime dir");

    let home = EnvVarGuard::set("HOME", home.as_os_str());
    let runtime_dir = EnvVarGuard::set("JUPYTER_RUNTIME_DIR", runtime_dir.as_os_str());
    let deno_path = EnvVarGuard::set("DENO_PATH", deno.as_os_str());
    jute::kernel_provision::ensure_deno_kernelspec()
        .await
        .expect("deno kernelspec provisions");

    Some(TestDenoKernelSpec {
        _env_lock: env_lock,
        _home: home,
        _runtime_dir: runtime_dir,
        _deno_path: deno_path,
        _temp_dir: temp_dir,
    })
}

async fn install_test_python3_kernelspec_with(extra_packages: &[&str]) -> Option<TestKernelSpec> {
    let env_lock = TEST_KERNELSPEC_ENV.lock().await;
    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-it-kernelspec-")
        .tempdir()
        .expect("temp dir");
    let jupyter_root = temp_dir.path().join("jupyter");
    let kernelspec_dir = jupyter_root.join("kernels").join("python3");
    tokio::fs::create_dir_all(&kernelspec_dir)
        .await
        .expect("kernelspec dir");
    let python = std::env::var("PYTHON_PATH").unwrap_or_else(|_| "python3".to_string());
    let site_packages = temp_dir.path().join("site-packages");
    let mut packages = vec!["ipykernel"];
    packages.extend_from_slice(extra_packages);
    let mut modules = vec!["ipykernel", "zmq"];
    modules.extend_from_slice(extra_packages);
    let argv = if python_modules_available(&python, &modules).await {
        vec![
            python,
            "-m".to_string(),
            "ipykernel_launcher".to_string(),
            "-f".to_string(),
            "{connection_file}".to_string(),
        ]
    } else if uv_modules_available(&packages, &modules).await {
        let mut argv = vec!["uv".to_string(), "run".to_string()];
        for package in &packages {
            argv.push("--with".to_string());
            argv.push((*package).to_string());
        }
        argv.extend([
            "python".to_string(),
            "-m".to_string(),
            "ipykernel_launcher".to_string(),
            "-f".to_string(),
            "{connection_file}".to_string(),
        ]);
        argv
    } else if install_python_packages_into(&python, &site_packages, &packages).await {
        let site_packages = site_packages.to_string_lossy().into_owned();
        vec![
            python,
            "-c".to_string(),
            concat!(
                "import runpy, sys; ",
                "sys.path.insert(0, sys.argv[1]); ",
                "sys.argv = ['ipykernel_launcher', '-f', sys.argv[2]]; ",
                "runpy.run_module('ipykernel_launcher', run_name='__main__')"
            )
            .to_string(),
            site_packages,
            "{connection_file}".to_string(),
        ]
    } else {
        eprintln!(
            "skipping live kernel test: python3 modules unavailable: {}",
            modules.join(", ")
        );
        return None;
    };
    tokio::fs::write(
        kernelspec_dir.join("kernel.json"),
        serde_json::to_vec_pretty(&json!({
            "argv": argv,
            "display_name": "Python 3",
            "language": "python"
        }))
        .expect("kernelspec serializes"),
    )
    .await
    .expect("kernelspec writes");
    let jupyter_path = EnvVarGuard::set("JUPYTER_PATH", jupyter_root.as_os_str());

    Some(TestKernelSpec {
        _env_lock: env_lock,
        _jupyter_path: jupyter_path,
        _temp_dir: temp_dir,
    })
}

fn notebook_with_code_cells(ids: &[&str]) -> NotebookRoot {
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
        cells: ids
            .iter()
            .map(|id| {
                Cell::Code(CodeCell {
                    id: Some((*id).to_string()),
                    metadata: CellMetadata {
                        spur: Some(SpurCellMetadata {
                            version: 1,
                            last_edited_by: None,
                            datasource_setup: None,
                            dag: None,
                        }),
                        jute_deck: None,
                        other: Default::default(),
                    },
                    source: MultilineString::Single(String::new()),
                    execution_count: None,
                    outputs: Vec::new(),
                })
            })
            .collect(),
    }
}

#[tokio::test]
async fn deno_write_port_is_readable_from_deno_and_rust() {
    let Some(_kernelspec) = install_test_deno_kernelspec().await else {
        return;
    };
    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-it-deno-port-contract-")
        .tempdir()
        .expect("temp dir");
    let notebook_path = temp_dir.path().join("deno-port.ipynb");
    let notebook_path_string = notebook_path.to_string_lossy().into_owned();
    let slot_id = notebook_slot_id(&notebook_path_string);
    let state = Arc::new(State::new());
    state.get_notebook().load(
        notebook_path.clone(),
        notebook_with_code_cells(&["deno-put", "deno-get"]),
    );
    let deps = deps_with_state(state);

    let started = structured(
        start_kernel::call(
            &deps,
            json!({
                "spec_name": "deno",
                "slot_id": slot_id,
            }),
        )
        .await
        .expect("deno kernel starts"),
    );
    assert_eq!(started["slot_id"], slot_id);

    let put = structured(
        run_cell::call(
            &deps,
            json!({
                "cell_id": "deno-put",
                "kernel_id": slot_id,
                "code": "await spur.put('t', [{ id: 1 }, { id: 2 }]);",
            }),
        )
        .await
        .expect("deno put cell runs"),
    );
    assert_eq!(put["status"], "ok", "put outputs={:?}", put["outputs"]);

    let get = structured(
        run_cell::call(
            &deps,
            json!({
                "cell_id": "deno-get",
                "kernel_id": slot_id,
                "code": concat!(
                    "const table = spur.get('t');\n",
                    "if (table.numRows !== 2) {\n",
                    "  throw new Error(`expected 2 rows, got ${table.numRows}`);\n",
                    "}\n",
                    "table"
                ),
            }),
        )
        .await
        .expect("deno get cell runs"),
    );
    assert_eq!(get["status"], "ok", "get outputs={:?}", get["outputs"]);

    let root = notebook_port_root(&notebook_path);
    let store = PortStore::open_read_only_at(root).expect("port store opens read-only");
    let entry_schema = store
        .manifest()
        .get("t")
        .expect("port manifest contains t")
        .schema
        .clone();
    let read = store.get("t").expect("rust reads deno-written port");
    let row_count = read
        .batches
        .iter()
        .map(arrow_array::RecordBatch::num_rows)
        .sum::<usize>();

    assert_eq!(row_count, 2);
    assert_eq!(entry_schema, read.schema.as_ref().clone());
    assert_eq!(read.schema.fields().len(), 1);
    assert_eq!(read.schema.field(0).name(), "id");

    let _ = stop_kernel::call(&deps, json!({ "kernel_id": slot_id })).await;
}

#[tokio::test]
#[ignore = "requires a working python3 kernel; run with --ignored"]
async fn start_kernel_then_stop_kernel_cycles_slot() {
    let Some(_kernelspec) = install_test_python3_kernelspec().await else {
        return;
    };
    let state = Arc::new(State::new());
    let slot_id = "mcp:notebook-read-tools-stop".to_string();
    let kernel = start_local_kernel("python3")
        .await
        .expect("python3 kernel starts");
    let (generation, previous) =
        install_kernel_in_slot(&state, &slot_id, "python3".to_string(), kernel);
    assert_eq!(generation, 1);
    assert!(previous.is_none());

    let deps = deps_with_state(state.clone());
    let body = structured(
        stop_kernel::call(&deps, json!({ "kernel_id": slot_id }))
            .await
            .expect("stop_kernel succeeds"),
    );
    assert_eq!(body["ok"], true);

    // After stop the slot is empty: kernel_info reports "dead" and a second
    // stop_kernel surfaces the kernel_disconnect error from take_kernel_from_slot.
    let info = structured(
        kernel_info::call(&deps, json!({ "kernel_id": slot_id }))
            .await
            .expect("kernel_info after stop succeeds"),
    );
    assert_eq!(info["status"], "dead");

    stop_kernel::call(&deps, json!({ "kernel_id": slot_id }))
        .await
        .expect_err("second stop reports kernel disconnect");
}

#[tokio::test]
#[ignore = "requires a working python3 kernel; run with --ignored"]
async fn run_cell_collects_events_against_in_process_kernel_mock() {
    let Some(_kernelspec) = install_test_python3_kernelspec().await else {
        return;
    };
    let state = Arc::new(State::new());
    let slot_id = "mcp:notebook-read-tools-run".to_string();
    let kernel = start_local_kernel("python3")
        .await
        .expect("python3 kernel starts");
    install_kernel_in_slot(&state, &slot_id, "python3".to_string(), kernel);

    let deps = deps_with_state(state.clone());
    let body = structured(
        run_cell::call(
            &deps,
            json!({
                "cell_id": "code-run-1",
                "kernel_id": slot_id,
                "code": "print(2 + 2)",
            }),
        )
        .await
        .expect("run_cell succeeds"),
    );

    assert_eq!(body["id"], "code-run-1");
    assert_eq!(body["status"], "ok");
    let outputs = body["outputs"].as_array().expect("outputs is an array");
    let combined = combined_stream_text(outputs);
    assert!(
        combined.contains('4'),
        "expected stdout to contain '4', got outputs={outputs:?}"
    );

    // Tear down the kernel so the test process doesn't leak a python child.
    let _ = stop_kernel::call(&deps, json!({ "kernel_id": slot_id })).await;
}

#[tokio::test]
#[ignore = "requires a working python3 kernel; run with --ignored"]
async fn run_cell_omitted_kernel_id_uses_current_notebook_slot() {
    let Some(_kernelspec) = install_test_python3_kernelspec().await else {
        return;
    };
    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-it-shared-slot-")
        .tempdir()
        .expect("temp dir");
    let path = temp_dir.path().join("shared.ipynb");
    tokio::fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "metadata": {},
            "nbformat_minor": 5,
            "nbformat": 4,
            "cells": []
        }))
        .expect("notebook serializes"),
    )
    .await
    .expect("notebook writes");

    let state = Arc::new(State::new());
    let bridge = Arc::new(MockBridge::default());
    bridge.push_response(Ok(json!({ "ok": true }))).await;
    let control = NotebookDaemonControl::new_with_parts_for_test(
        Arc::new(AgentBridge::new()),
        bridge,
        state.clone(),
        Arc::new(RecordingWindowOps::default()),
        None,
    );
    let open = control
        .handle(daemon_request(jute::commands::DaemonControlCommand::Open {
            path: path.display().to_string(),
        }))
        .await;
    assert!(open.ok, "{:?}", open.error);

    let slot_id = notebook_slot_id(path.to_string_lossy().as_ref());
    let kernel = start_local_kernel("python3")
        .await
        .expect("python3 kernel starts");
    install_kernel_in_slot(&state, &slot_id, "python3".to_string(), kernel);

    let deps = deps_with_state_and_daemon(state.clone(), control);
    let define = structured(
        run_cell::call(
            &deps,
            json!({
                "cell_id": "code-run-define",
                "code": "x = 42",
            }),
        )
        .await
        .expect("run_cell defaults to current notebook slot"),
    );
    assert_eq!(define["status"], "ok");

    let print = structured(
        run_cell::call(
            &deps,
            json!({
                "cell_id": "code-run-print",
                "code": "print(x)",
            }),
        )
        .await
        .expect("run_cell reuses the current notebook slot"),
    );
    assert_eq!(print["status"], "ok");
    let outputs = print["outputs"].as_array().expect("outputs is an array");
    let combined = combined_stream_text(outputs);
    assert!(
        combined.contains("42"),
        "expected stdout to contain 42, got outputs={outputs:?}"
    );

    let _ = stop_kernel::call(&deps, json!({ "kernel_id": slot_id })).await;
}

#[cfg(feature = "datasource-introspect")]
#[tokio::test]
#[ignore = "requires a working python3 kernel with duckdb; run with --ignored"]
async fn canonical_demo_attach_csv_runs_setup_and_renders_html_chart() {
    let Some(_kernelspec) = install_test_python3_kernelspec_with(&["duckdb"]).await else {
        return;
    };
    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-it-canonical-demo-")
        .tempdir()
        .expect("temp dir");
    let csv = temp_dir.path().join("sales.csv");
    tokio::fs::write(
        &csv,
        concat!(
            "month,region,revenue\n",
            "2026-01,west,10\n",
            "2026-01,east,20\n",
            "2026-02,west,30\n",
            "2026-02,east,40\n",
        ),
    )
    .await
    .expect("csv fixture writes");
    let notebook_path = temp_dir.path().join("analysis.ipynb");
    let empty_notebook = json!({
        "metadata": {},
        "nbformat_minor": 5,
        "nbformat": 4,
        "cells": []
    });
    tokio::fs::write(
        &notebook_path,
        serde_json::to_vec_pretty(&empty_notebook).expect("empty notebook serializes"),
    )
    .await
    .expect("notebook fixture writes");

    let state = Arc::new(State::new());
    let notebook_root: jute::backend::notebook::NotebookRoot =
        serde_json::from_value(empty_notebook).expect("empty notebook parses");
    state
        .get_notebook()
        .load(notebook_path.clone(), notebook_root);

    let control = NotebookDaemonControl::new_with_parts_for_test(
        Arc::new(AgentBridge::new()),
        Arc::new(MockBridge::default()),
        state.clone(),
        Arc::new(RecordingWindowOps::default()),
        None,
    );

    for _ in 0..2 {
        let attach = control
            .handle(daemon_request(
                jute::commands::DaemonControlCommand::AttachDatasource {
                    name: "sales".to_string(),
                    path: csv.display().to_string(),
                    group: Some("demo".to_string()),
                },
            ))
            .await;
        assert!(attach.ok, "{:?}", attach.error);
    }

    let deps = deps_with_state_and_daemon(state.clone(), control);
    let listed = structured(
        list_datasources::call(&deps, json!({}))
            .await
            .expect("notebook_list_datasources succeeds"),
    );
    let entries = listed["entries"]
        .as_array()
        .expect("datasource entries are an array");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "sales");
    assert_eq!(entries[0]["kind"], "csv");
    assert_eq!(entries[0]["group"], "demo");
    assert_eq!(entries[0]["rowCount"], 4);
    assert_eq!(entries[0]["columns"][0]["name"], "month");
    assert_eq!(entries[0]["columns"][1]["name"], "region");
    assert_eq!(entries[0]["columns"][2]["name"], "revenue");

    let (root, _) = state.get_notebook().snapshot();
    let notebook = serde_json::to_value(root).expect("notebook serializes");
    let setup_cells = notebook["cells"]
        .as_array()
        .expect("cells array")
        .iter()
        .filter(|cell| {
            cell["cell_type"] == "code"
                && cell["metadata"]["spur"]["datasource_setup"] == json!(true)
        })
        .collect::<Vec<_>>();
    assert_eq!(setup_cells.len(), 1);
    let setup_cell = setup_cells[0];
    let setup_cell_id = setup_cell["id"]
        .as_str()
        .expect("setup cell has an id")
        .to_string();
    let setup_source = setup_cell["source"].as_str().expect("setup cell source");
    assert!(setup_source.contains("# SPUR datasource setup cell v1"));
    assert_eq!(setup_source.matches("CREATE OR REPLACE VIEW").count(), 1);
    assert!(setup_source.contains("read_csv_auto"));
    assert!(setup_source.contains("sales"));

    let slot_id = "mcp:notebook-read-tools-canonical-demo".to_string();
    let kernel = start_local_kernel("python3")
        .await
        .expect("python3 kernel starts");
    install_kernel_in_slot(&state, &slot_id, "python3".to_string(), kernel);

    let setup = structured(
        run_cell::call(
            &deps,
            json!({
                "cell_id": setup_cell_id,
                "kernel_id": slot_id,
                "code": setup_source,
            }),
        )
        .await
        .expect("setup cell runs"),
    );
    assert_eq!(
        setup["status"], "ok",
        "setup outputs={:?}",
        setup["outputs"]
    );

    let analysis = structured(
        run_cell::call(
            &deps,
            json!({
                "cell_id": "code-canonical-demo-chart",
                "kernel_id": slot_id,
                "code": r#"
from IPython.display import HTML, display

rows = duckdb.sql("""
    SELECT region, SUM(revenue) AS revenue
    FROM sales
    GROUP BY region
    ORDER BY region
""").fetchall()
max_revenue = max((revenue for _, revenue in rows), default=1)
bars = "".join(
    f"<div><span>{region}</span>"
    f"<div style='background:#2563eb;color:white;width:{int((revenue / max_revenue) * 160)}px'>"
    f"{revenue}</div></div>"
    for region, revenue in rows
)
display(HTML(f"<section data-spur-demo-chart='sales'>{bars}</section>"))
"#,
            }),
        )
        .await
        .expect("analysis cell runs"),
    );
    assert_eq!(
        analysis["status"], "ok",
        "analysis outputs={:?}",
        analysis["outputs"]
    );
    let outputs = analysis["outputs"]
        .as_array()
        .expect("analysis outputs are an array");
    let display = outputs
        .iter()
        .find(|output| {
            output["event"] == "display_data"
                && (output["data"]["data"].get("text/html").is_some()
                    || output["data"]["data"].get("image/png").is_some())
        })
        .unwrap_or_else(|| panic!("expected display_data HTML or image/png, got {outputs:?}"));
    let html = display["data"]["data"]["text/html"]
        .as_str()
        .unwrap_or_default();
    assert!(
        html.contains("data-spur-demo-chart='sales'")
            && html.contains("east")
            && html.contains("west"),
        "expected rendered chart HTML to include sales regions, got {html:?}"
    );

    let _ = stop_kernel::call(&deps, json!({ "kernel_id": slot_id })).await;
}

fn blake3_16_hex(source: &str) -> String {
    let hash = blake3::hash(source.as_bytes());
    hash.as_bytes()[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

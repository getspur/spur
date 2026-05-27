use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use jute::commands::{install_kernel_in_slot, start_local_kernel};
use jute::state::{notebook_slot_id, KernelSlot, State};
use rmcp::model::CallToolResult;
use serde_json::{json, Value};
use spur_notebook::mcp::{
    bridge::{
        AgentBridge, BridgeError, BridgeRequestFuture, BridgeRequester, TauriBridgeRequester,
    },
    tools::{kernel_info, read_cell, run_cell, save, snapshot, stop_kernel},
    DaemonControlRequest, DaemonWindowOps, NotebookDaemonControl, ServerDeps,
};
use tokio::sync::Mutex;
use tokio::time::timeout;

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

    fn emit_recents_changed(&self) {}

    fn exit(&self) {}
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

async fn install_ipykernel_into(python: &str, target: &Path) -> bool {
    let Ok(Ok(status)) = timeout(
        Duration::from_secs(240),
        tokio::process::Command::new(python)
            .args(["-m", "pip", "install", "--quiet", "--target"])
            .arg(target)
            .arg("ipykernel")
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
    let argv = if command_succeeds(&python, &["-c", "import ipykernel, zmq"]).await {
        vec![
            python,
            "-m".to_string(),
            "ipykernel_launcher".to_string(),
            "-f".to_string(),
            "{connection_file}".to_string(),
        ]
    } else if command_succeeds(
        "uv",
        &[
            "run",
            "--with",
            "ipykernel",
            "python",
            "-c",
            "import ipykernel, zmq",
        ],
    )
    .await
    {
        vec![
            "uv".to_string(),
            "run".to_string(),
            "--with".to_string(),
            "ipykernel".to_string(),
            "python".to_string(),
            "-m".to_string(),
            "ipykernel_launcher".to_string(),
            "-f".to_string(),
            "{connection_file}".to_string(),
        ]
    } else if install_ipykernel_into(&python, &site_packages).await {
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
        eprintln!("skipping live kernel test: python3 ipykernel is unavailable");
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
    let combined = outputs
        .iter()
        .filter_map(|event| event.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
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
        .handle(DaemonControlRequest {
            id: None,
            daemon: None,
            command: "open".to_string(),
            path: Some(path.clone()),
            pinned: None,
            ..Default::default()
        })
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
    let combined = outputs
        .iter()
        .filter_map(|event| event.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    assert!(
        combined.contains("42"),
        "expected stdout to contain 42, got outputs={outputs:?}"
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

//! Real-kernel e2e for the AFM comm-send seam.
//!
//! Drives the production path end to end: a frontend `model-state.update`
//! intent → the real default `JuteModelStateCommGateway` → real
//! `jute::commands::send_comm_msg` → a live `ipykernel` comm. A kernel-side
//! `Comm` is opened so the real `run_cell` path records `comm_id → slot` in
//! `comm_owner`; the handler then resolves that slot through the real gateway
//! and delivers the `comm_msg`, which the kernel's `on_msg` callback captures.
//!
//! Skips gracefully (does not fail) when python3/ipykernel is unavailable,
//! mirroring `python_binary_for_test` in `rust_go_ports_e2e.rs`.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jute::backend::notebook::{
    Cell, CellMetadata, CodeCell, CodeType, MultilineString, NotebookMetadata, NotebookRoot,
    SpurCellMetadata,
};
use jute::state::{notebook_slot_id, State};
use rmcp::model::CallToolResult;
use serde_json::{json, Value};
use spur_notebook::commands::{handle_anywidget_command_intent, AnyWidgetCommandIntent};
use spur_notebook::mcp::{
    bridge::{AgentBridge, TauriBridgeRequester},
    tools::{run_cell, start_kernel, stop_kernel},
    ServerDeps,
};
use tokio::sync::Mutex;
use tokio::time::timeout;

static TEST_ENV: Mutex<()> = Mutex::const_new(());

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

struct TestEnv {
    _guards: Vec<EnvVarGuard>,
    _temp_dir: tempfile::TempDir,
    _env_lock: tokio::sync::MutexGuard<'static, ()>,
}

async fn command_succeeds(command: impl AsRef<Path>, args: &[&str]) -> bool {
    let Ok(Ok(status)) = timeout(
        Duration::from_secs(120),
        tokio::process::Command::new(command.as_ref())
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

fn absolute_path(path: PathBuf) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn binary_path_names(binary: &str) -> Vec<String> {
    if cfg!(windows) {
        vec![format!("{binary}.exe"), binary.to_string()]
    } else {
        vec![binary.to_string()]
    }
}

fn python_binary_for_test() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("PYTHON_PATH") {
        if path.is_empty() {
            return Err("PYTHON_PATH is set but empty".to_string());
        }
        let path = absolute_path(PathBuf::from(path))
            .map_err(|error| format!("PYTHON_PATH current_dir failed: {error}"))?;
        return if path.exists() {
            Ok(path)
        } else {
            Err(format!(
                "PYTHON_PATH is set but does not exist: {}",
                path.display()
            ))
        };
    }

    let Some(paths) = std::env::var_os("PATH") else {
        return Err("python3 is not available: PATH is unset".to_string());
    };
    for dir in std::env::split_paths(&paths) {
        for name in binary_path_names("python3") {
            let candidate = dir.join(name);
            if candidate.exists() {
                return absolute_path(candidate)
                    .map_err(|error| format!("current_dir could not be read: {error}"));
            }
        }
    }

    Err("python3 is not available from PYTHON_PATH or PATH".to_string())
}

async fn python_modules_available(python: &Path, modules: &[&str]) -> bool {
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

async fn install_python_packages_into(python: &Path, target: &Path, packages: &[&str]) -> bool {
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

/// Write a `python3` kernelspec into `JUPYTER_PATH` using whichever launcher
/// can import `ipykernel` (direct, `uv run`, or a `--target` pip install).
/// `ipykernel` ships `ipykernel.comm.Comm`, so no extra deps are required for
/// the comm round-trip. Returns `None` (graceful skip) when none works.
async fn write_test_python3_kernelspec(
    python: &Path,
    jupyter_root: &Path,
    scratch_dir: &Path,
) -> Option<()> {
    let kernelspec_dir = jupyter_root.join("kernels").join("python3");
    tokio::fs::create_dir_all(&kernelspec_dir)
        .await
        .expect("python kernelspec dir");
    let site_packages = scratch_dir.join("site-packages");
    let packages = ["ipykernel"];
    let modules = ["ipykernel", "zmq"];
    let python_string = python.to_string_lossy().into_owned();

    let argv = if python_modules_available(python, &modules).await {
        vec![
            python_string,
            "-m".to_string(),
            "ipykernel_launcher".to_string(),
            "-f".to_string(),
            "{connection_file}".to_string(),
        ]
    } else if uv_modules_available(&packages, &modules).await {
        let mut argv = vec!["uv".to_string(), "run".to_string()];
        for package in packages {
            argv.push("--with".to_string());
            argv.push(package.to_string());
        }
        argv.extend([
            "python".to_string(),
            "-m".to_string(),
            "ipykernel_launcher".to_string(),
            "-f".to_string(),
            "{connection_file}".to_string(),
        ]);
        argv
    } else if install_python_packages_into(python, &site_packages, &packages).await {
        let site_packages = site_packages.to_string_lossy().into_owned();
        vec![
            python_string,
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
            "skipping AFM comm-send e2e: python3 modules unavailable: {}",
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
        .expect("python kernelspec serializes"),
    )
    .await
    .expect("python kernelspec writes");

    Some(())
}

async fn setup_test_env() -> Option<TestEnv> {
    let python = match python_binary_for_test() {
        Ok(path) => path,
        Err(reason) => {
            eprintln!("skipping AFM comm-send e2e: {reason}");
            return None;
        }
    };
    if !command_succeeds(&python, &["--version"]).await {
        eprintln!(
            "skipping AFM comm-send e2e: python3 did not run: {}",
            python.display()
        );
        return None;
    }

    let env_lock = TEST_ENV.lock().await;
    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-it-afm-comm-send-")
        .tempdir()
        .expect("temp dir");
    let home = temp_dir.path().join("home");
    let runtime_dir = temp_dir.path().join("runtime");
    let jupyter_root = temp_dir.path().join("jupyter");
    for dir in [&home, &runtime_dir, &jupyter_root] {
        tokio::fs::create_dir_all(dir).await.expect("test env dir");
    }

    let guards = vec![
        EnvVarGuard::set("HOME", home.as_os_str()),
        EnvVarGuard::set("JUPYTER_RUNTIME_DIR", runtime_dir.as_os_str()),
        EnvVarGuard::set("JUPYTER_PATH", jupyter_root.as_os_str()),
        EnvVarGuard::set("PYTHON_PATH", python.as_os_str()),
    ];

    write_test_python3_kernelspec(&python, &jupyter_root, temp_dir.path()).await?;

    Some(TestEnv {
        _guards: guards,
        _temp_dir: temp_dir,
        _env_lock: env_lock,
    })
}

fn deps_with_state(state: Arc<State>) -> ServerDeps {
    ServerDeps {
        bridge: Arc::new(TauriBridgeRequester::without_app(Arc::new(
            AgentBridge::new(),
        ))),
        state: Some(state),
        app: None,
        daemon: None,
        plugins: None,
    }
}

fn structured(result: CallToolResult) -> Value {
    assert_eq!(result.is_error, Some(false));
    result.structured_content.expect("structured content")
}

/// Build a notebook whose code cells carry the given (id, python source).
///
/// The `run_cell` MCP tool runs each cell's *stored* source (it intentionally
/// has no `code` param), so the Python must live in the cell body here.
fn python_notebook(cells: &[(&str, &str)]) -> NotebookRoot {
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
        cells: cells
            .iter()
            .map(|(id, source)| {
                Cell::Code(CodeCell {
                    id: Some((*id).to_string()),
                    metadata: CellMetadata {
                        spur: Some(SpurCellMetadata {
                            version: 1,
                            last_edited_by: None,
                            datasource_setup: None,
                            dag: None,
                            code_type: Some(CodeType::Python),
                            frontend: None,
                        }),
                        jute_deck: None,
                        other: Default::default(),
                    },
                    source: MultilineString::Single((*source).to_string()),
                    execution_count: None,
                    outputs: Vec::new(),
                })
            })
            .collect(),
    }
}

async fn start_python_kernel(deps: &ServerDeps, slot_id: &str) {
    // setup_test_env writes the python3 kernelspec into JUPYTER_PATH, so
    // start_kernel validates it directly without a Tauri app handle.
    let started = structured(
        start_kernel::call(
            deps,
            json!({
                "spec_name": "python3",
                "slot_id": slot_id,
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("python3 kernel starts: {error:?}")),
    );
    assert_eq!(started["slot_id"], slot_id);
}

async fn run_cell_ok(
    deps: &ServerDeps,
    notebook_path: &str,
    cell_id: &str,
    kernel_id: &str,
) -> Value {
    let result = structured(
        run_cell::call(
            deps,
            json!({
                "cell_id": cell_id,
                "notebook_path": notebook_path,
                "kernel_id": kernel_id,
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{cell_id} cell runs: {error:?}")),
    );
    assert_eq!(
        result["status"], "ok",
        "{cell_id} status={:?} outputs={:?}",
        result["status"], result["outputs"]
    );
    result
}

/// Concatenate the text of every `stdout` event in a `run_cell` summary.
fn stdout_text(result: &Value) -> String {
    result["outputs"]
        .as_array()
        .map(|outputs| {
            outputs
                .iter()
                .filter(|output| output["event"] == "stdout")
                .filter_map(|output| output["data"].as_str())
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// Frontend intent → real `JuteModelStateCommGateway` → `send_comm_msg` →
/// live `ipykernel` comm, asserting the kernel-side `on_msg` actually received
/// the `{method:"update", state:{...}}` payload.
#[tokio::test]
async fn model_state_update_reaches_live_kernel_comm() {
    let Some(_env) = setup_test_env().await else {
        return;
    };

    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-it-afm-comm-send-nb-")
        .tempdir()
        .expect("temp dir");
    let notebook_path = temp_dir.path().join("afm-comm-send.ipynb");
    let notebook_path_string = notebook_path.to_string_lossy().into_owned();
    let slot_base = notebook_slot_id(&notebook_path_string);
    let python_slot_id = format!("{slot_base}#python3");

    let state = Arc::new(State::new());
    state.get_notebook().load(
        notebook_path.clone(),
        python_notebook(&[
            (
                "open-comm",
                concat!(
                    "from ipykernel.comm import Comm\n",
                    "_spur_rx = []\n",
                    "_c = Comm(target_name=\"spur.afm.test\", data={})\n",
                    "@_c.on_msg\n",
                    "def _on(msg):\n",
                    "    _spur_rx.append(msg[\"content\"][\"data\"])\n",
                    "print(\"COMM_ID=\" + _c.comm_id)\n",
                ),
            ),
            (
                "readback",
                concat!(
                    "import json, time\n",
                    "for _ in range(50):\n",
                    "    if _spur_rx:\n",
                    "        break\n",
                    "    time.sleep(0.05)\n",
                    "print(\"RX=\" + json.dumps(_spur_rx))\n",
                ),
            ),
        ]),
    );
    let deps = deps_with_state(Arc::clone(&state));

    start_python_kernel(&deps, &python_slot_id).await;

    // Opening the Comm sends comm_open kernel→frontend; the real run_cell path
    // records comm_id → slot in comm_owner. Draining to terminal guarantees the
    // open event is recorded before we send.
    let open_result = run_cell_ok(&deps, &notebook_path_string, "open-comm", &python_slot_id).await;
    let open_stdout = stdout_text(&open_result);
    let comm_id = open_stdout
        .lines()
        .find_map(|line| line.strip_prefix("COMM_ID="))
        .map(str::trim)
        .filter(|comm_id| !comm_id.is_empty())
        .unwrap_or_else(|| panic!("comm_open printed COMM_ID; stdout={open_stdout:?}"))
        .to_string();

    // The real default-gateway handler resolves the slot from comm_owner and
    // delivers the comm_msg through jute::commands::send_comm_msg.
    let intent = AnyWidgetCommandIntent {
        id: "e2e-1".to_string(),
        kind: "anywidget-command".to_string(),
        name: "model-state.update".to_string(),
        comm_id: Some(comm_id.clone()),
        msg: json!({ "state": { "value": 99 } }),
        buffers: Vec::new(),
    };
    let resp = handle_anywidget_command_intent(&state, None, intent).await;
    assert_eq!(
        resp.response["kernelDelivery"]["status"], "sent",
        "expected delivery 'sent', got kernelDelivery={:?}",
        resp.response["kernelDelivery"]
    );
    assert_eq!(resp.response["kernelDelivery"]["commId"], comm_id);
    assert_eq!(resp.response["method"], "update");
    assert_eq!(resp.response["state"]["value"], 99);

    // The kernel's on_msg callback should have appended the delivered payload.
    let readback = run_cell_ok(&deps, &notebook_path_string, "readback", &python_slot_id).await;
    let rx_stdout = stdout_text(&readback);
    let rx_json = rx_stdout
        .lines()
        .find_map(|line| line.strip_prefix("RX="))
        .unwrap_or_else(|| panic!("readback printed RX; stdout={rx_stdout:?}"));
    let received: Vec<Value> = serde_json::from_str(rx_json).expect("RX is a JSON array");
    assert!(
        received
            .iter()
            .any(|entry| entry == &json!({ "method": "update", "state": { "value": 99 } })),
        "kernel comm did not receive the update payload; received={received:?}"
    );

    let _ = stop_kernel::call(&deps, json!({ "kernel_id": python_slot_id })).await;
}

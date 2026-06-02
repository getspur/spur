use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use jute::backend::notebook::{
    Cell, CellMetadata, CodeCell, CodeType, MultilineString, NotebookMetadata, NotebookRoot,
    SpurCellMetadata,
};
use jute::commands::{install_kernel_in_slot, start_local_kernel};
use jute::kernel_provision::{ensure_evcxr_kernelspec, ensure_gonb_kernelspec};
use jute::state::{notebook_slot_id, State};
use rmcp::model::CallToolResult;
use serde_json::{json, Value};
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

fn binary_for_test(env_var: &'static str, binary: &str) -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os(env_var) {
        if path.is_empty() {
            return Err(format!("{env_var} is set but empty"));
        }
        let path = absolute_path(PathBuf::from(path)).map_err(|error| {
            format!("{env_var} is set but current_dir could not be read: {error}")
        })?;
        return if path.exists() {
            Ok(path)
        } else {
            Err(format!(
                "{env_var} is set but does not exist: {}",
                path.display()
            ))
        };
    }

    let Some(paths) = std::env::var_os("PATH") else {
        return Err(format!("{binary} is not available: PATH is unset"));
    };
    for dir in std::env::split_paths(&paths) {
        for name in binary_path_names(binary) {
            let candidate = dir.join(name);
            if candidate.exists() {
                return absolute_path(candidate)
                    .map_err(|error| format!("current_dir could not be read: {error}"));
            }
        }
    }

    Err(format!("{binary} is not available from {env_var} or PATH"))
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
    let packages = ["ipykernel", "pyarrow"];
    let modules = ["ipykernel", "zmq", "pyarrow"];
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
            "skipping Rust/Go port e2e: python3 modules unavailable: {}",
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
    let cargo = match binary_for_test("CARGO_PATH", "cargo") {
        Ok(path) => path,
        Err(reason) => {
            eprintln!("skipping Rust/Go port e2e: {reason}");
            return None;
        }
    };
    if !command_succeeds(&cargo, &["--version"]).await {
        eprintln!(
            "skipping Rust/Go port e2e: cargo did not run: {}",
            cargo.display()
        );
        return None;
    }

    let go = match binary_for_test("GO_PATH", "go") {
        Ok(path) => path,
        Err(reason) => {
            eprintln!("skipping Rust/Go port e2e: {reason}");
            return None;
        }
    };
    if !command_succeeds(&go, &["version"]).await {
        eprintln!(
            "skipping Rust/Go port e2e: go did not run: {}",
            go.display()
        );
        return None;
    }

    let python = match python_binary_for_test() {
        Ok(path) => path,
        Err(reason) => {
            eprintln!("skipping Rust/Go port e2e: {reason}");
            return None;
        }
    };
    if !command_succeeds(&python, &["--version"]).await {
        eprintln!(
            "skipping Rust/Go port e2e: python3 did not run: {}",
            python.display()
        );
        return None;
    }

    let env_lock = TEST_ENV.lock().await;
    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-it-rust-go-ports-")
        .tempdir()
        .expect("temp dir");
    let home = temp_dir.path().join("home");
    let runtime_dir = temp_dir.path().join("runtime");
    let jupyter_root = temp_dir.path().join("jupyter");
    let cargo_home = temp_dir.path().join("cargo-home");
    let go_path = temp_dir.path().join("go-path");
    let go_bin = temp_dir.path().join("go-bin");
    for dir in [
        &home,
        &runtime_dir,
        &jupyter_root,
        &cargo_home,
        &go_path,
        &go_bin,
    ] {
        tokio::fs::create_dir_all(dir).await.expect("test env dir");
    }

    let guards = vec![
        EnvVarGuard::set("HOME", home.as_os_str()),
        EnvVarGuard::set("JUPYTER_RUNTIME_DIR", runtime_dir.as_os_str()),
        EnvVarGuard::set("JUPYTER_PATH", jupyter_root.as_os_str()),
        EnvVarGuard::set("CARGO_PATH", cargo.as_os_str()),
        EnvVarGuard::set("CARGO_HOME", cargo_home.as_os_str()),
        EnvVarGuard::set("GO_PATH", go.as_os_str()),
        EnvVarGuard::set("GOPATH", go_path.as_os_str()),
        EnvVarGuard::set("GOBIN", go_bin.as_os_str()),
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
    }
}

fn structured(result: CallToolResult) -> Value {
    assert_eq!(result.is_error, Some(false));
    result.structured_content.expect("structured content")
}

fn notebook_with_code_cells(cells: &[(&str, CodeType)]) -> NotebookRoot {
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
            .map(|(id, code_type)| {
                Cell::Code(CodeCell {
                    id: Some((*id).to_string()),
                    metadata: CellMetadata {
                        spur: Some(SpurCellMetadata {
                            version: 1,
                            last_edited_by: None,
                            datasource_setup: None,
                            dag: None,
                            code_type: Some(*code_type),
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

async fn start_managed_kernel(deps: &ServerDeps, spec_name: &str, slot_id: &str) {
    match spec_name {
        "evcxr" => ensure_evcxr_kernelspec()
            .await
            .expect("evcxr kernelspec provisions"),
        "gonb" => ensure_gonb_kernelspec()
            .await
            .expect("gonb kernelspec provisions"),
        other => panic!("unexpected managed kernel spec: {other}"),
    }

    let started = structured(
        start_kernel::call(
            deps,
            json!({
                "spec_name": spec_name,
                "slot_id": slot_id,
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{spec_name} kernel starts after ensure: {error:?}")),
    );
    assert_eq!(started["slot_id"], slot_id);
}

async fn run_cell_ok(
    deps: &ServerDeps,
    notebook_path: &str,
    cell_id: &str,
    kernel_id: &str,
    code: &str,
) -> Value {
    let result = structured(
        run_cell::call(
            deps,
            json!({
                "cell_id": cell_id,
                "notebook_path": notebook_path,
                "kernel_id": kernel_id,
                "code": code,
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

/// Cross-language Arrow port contract for Python, Rust/evcxr, and Go/gonb.
#[tokio::test]
async fn rust_go_ports_round_trip_through_python() {
    let Some(_env) = setup_test_env().await else {
        return;
    };

    let temp_dir = tempfile::Builder::new()
        .prefix("spur-notebook-it-rust-go-round-trip-")
        .tempdir()
        .expect("temp dir");
    let notebook_path = temp_dir.path().join("rust-go-ports.ipynb");
    let notebook_path_string = notebook_path.to_string_lossy().into_owned();
    let slot_base = notebook_slot_id(&notebook_path_string);
    let python_slot_id = format!("{slot_base}#python3");
    let rust_slot_id = format!("{slot_base}#evcxr");
    let go_slot_id = format!("{slot_base}#gonb");

    let state = Arc::new(State::new());
    state.get_notebook().load(
        notebook_path.clone(),
        notebook_with_code_cells(&[
            ("py-put-t", CodeType::Python),
            ("rs-get-t", CodeType::Rust),
            ("go-get-t", CodeType::Go),
            ("rs-put-t", CodeType::Rust),
            ("go-put-t", CodeType::Go),
            ("py-get-rust-go", CodeType::Python),
        ]),
    );
    let deps = deps_with_state(Arc::clone(&state));

    let python_kernel = start_local_kernel("python3")
        .await
        .expect("python3 kernel starts");
    install_kernel_in_slot(
        &state,
        &python_slot_id,
        "python3".to_string(),
        python_kernel,
    );
    start_managed_kernel(&deps, "evcxr", &rust_slot_id).await;
    start_managed_kernel(&deps, "gonb", &go_slot_id).await;

    run_cell_ok(
        &deps,
        &notebook_path_string,
        "py-put-t",
        &python_slot_id,
        concat!(
            "import pyarrow as pa\n",
            "spur.put('t', pa.table({\n",
            "    'id': pa.array([1, 2], type=pa.int64()),\n",
            "    'label': pa.array(['alpha', 'beta'], type=pa.string()),\n",
            "}))\n",
        ),
    )
    .await;

    run_cell_ok(
        &deps,
        &notebook_path_string,
        "rs-get-t",
        &rust_slot_id,
        concat!(
            "{\n",
            "let batches = spur.get(\"t\").expect(\"rust reads python port\");\n",
            "assert_eq!(batches.len(), 1);\n",
            "let batch = &batches[0];\n",
            "assert_eq!(batch.num_rows(), 2);\n",
            "assert_eq!(batch.num_columns(), 2);\n",
            "assert_eq!(batch.schema().field(0).name(), \"id\");\n",
            "assert_eq!(batch.schema().field(1).name(), \"label\");\n",
            "let ids = batch.column(0).as_any().downcast_ref::<arrow::array::Int64Array>().expect(\"id int64 array\");\n",
            "let labels = batch.column(1).as_any().downcast_ref::<arrow::array::StringArray>().expect(\"label string array\");\n",
            "assert_eq!(ids.value(0), 1);\n",
            "assert_eq!(ids.value(1), 2);\n",
            "assert_eq!(labels.value(0), \"alpha\");\n",
            "assert_eq!(labels.value(1), \"beta\");\n",
            "}\n",
        ),
    )
    .await;

    run_cell_ok(
        &deps,
        &notebook_path_string,
        "go-get-t",
        &go_slot_id,
        concat!(
            "records, err := spur.Get(\"t\")\n",
            "if err != nil { panic(err) }\n",
            "if len(records) != 1 { panic(fmt.Sprintf(\"expected 1 record, got %d\", len(records))) }\n",
            "record := records[0]\n",
            "if record.NumRows() != 2 { panic(fmt.Sprintf(\"expected 2 rows, got %d\", record.NumRows())) }\n",
            "if record.NumCols() != 2 { panic(fmt.Sprintf(\"expected 2 columns, got %d\", record.NumCols())) }\n",
            "if record.Schema().Field(0).Name != \"id\" { panic(record.Schema().Field(0).Name) }\n",
            "if record.Schema().Field(1).Name != \"label\" { panic(record.Schema().Field(1).Name) }\n",
            "if fmt.Sprint(record.Column(0).GetOneForMarshal(0)) != \"1\" { panic(\"row 0 id mismatch\") }\n",
            "if fmt.Sprint(record.Column(0).GetOneForMarshal(1)) != \"2\" { panic(\"row 1 id mismatch\") }\n",
            "if fmt.Sprint(record.Column(1).GetOneForMarshal(0)) != \"alpha\" { panic(\"row 0 label mismatch\") }\n",
            "if fmt.Sprint(record.Column(1).GetOneForMarshal(1)) != \"beta\" { panic(\"row 1 label mismatch\") }\n",
        ),
    )
    .await;

    run_cell_ok(
        &deps,
        &notebook_path_string,
        "rs-put-t",
        &rust_slot_id,
        concat!(
            "{\n",
            "let mut batches = spur.get(\"t\").expect(\"rust reads python port before put\");\n",
            "let batch = batches.remove(0);\n",
            "spur.put(\"from_rust\", batch).expect(\"rust writes port\");\n",
            "}\n",
        ),
    )
    .await;

    run_cell_ok(
        &deps,
        &notebook_path_string,
        "go-put-t",
        &go_slot_id,
        concat!(
            "records, err := spur.Get(\"t\")\n",
            "if err != nil { panic(err) }\n",
            "if len(records) == 0 { panic(\"expected records from t\") }\n",
            "_, err = spur.Put(\"from_go\", records[0])\n",
            "if err != nil { panic(err) }\n",
        ),
    )
    .await;

    run_cell_ok(
        &deps,
        &notebook_path_string,
        "py-get-rust-go",
        &python_slot_id,
        concat!(
            "def _rows(value):\n",
            "    if hasattr(value, 'to_pylist'):\n",
            "        return value.to_pylist()\n",
            "    return value.to_dict('records')\n",
            "expected = [{'id': 1, 'label': 'alpha'}, {'id': 2, 'label': 'beta'}]\n",
            "assert _rows(spur.get('from_rust')) == expected\n",
            "assert _rows(spur.get('from_go')) == expected\n",
        ),
    )
    .await;

    let _ = stop_kernel::call(&deps, json!({ "kernel_id": python_slot_id })).await;
    let _ = stop_kernel::call(&deps, json!({ "kernel_id": rust_slot_id })).await;
    let _ = stop_kernel::call(&deps, json!({ "kernel_id": go_slot_id })).await;
}

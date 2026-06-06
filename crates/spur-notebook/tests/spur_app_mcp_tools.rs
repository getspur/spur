use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use jute::state::State;
use rmcp::{
    model::{CallToolRequestParams, CallToolResult},
    ErrorData as McpError, ServiceExt,
};
use serde_json::{json, Value};
use spur_notebook::{
    mcp::{
        bridge::{AgentBridge, BridgeError, BridgeRequestFuture, BridgeRequester},
        start_server,
        tools::{self, export_spur_app, import_spur_app, save},
        transport::LengthPrefixedJsonTransport,
        DaemonWindowOps, NotebookDaemonControl, ServerDeps,
    },
    spur_app::{archive::write_entries, SpurAppManifest, SPUR_APP_MANIFEST, SPUR_APP_SCHEMA},
};
use tokio::net::UnixStream;

#[test]
fn spur_app_mcp_tools_include_import_export() {
    let names = tools::tools()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();

    assert!(names.iter().any(|name| name == "notebook_export_spur_app"));
    assert!(names.iter().any(|name| name == "notebook_import_spur_app"));
}

#[tokio::test]
async fn spur_app_mcp_server_dispatches_export_tool() {
    let temp = tempfile::tempdir().expect("tempdir");
    let socket_path = temp.path().join("notebook.sock");
    let source_path = temp.path().join("server-forecast.ipynb");
    let output_path = temp.path().join("server-forecast.spurapp");
    fs::write(&source_path, minimal_notebook()).expect("seed notebook");

    let _server = start_server(&socket_path).await.expect("server starts");
    let stream = UnixStream::connect(&socket_path)
        .await
        .expect("client connects");
    let transport = LengthPrefixedJsonTransport::new(stream);
    let client = rmcp::model::ClientInfo::default()
        .serve(transport)
        .await
        .expect("client initializes");
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "notebook_path".to_string(),
        json!(source_path.to_string_lossy()),
    );
    arguments.insert(
        "output_path".to_string(),
        json!(output_path.to_string_lossy()),
    );

    let result = client
        .call_tool(CallToolRequestParams::new("notebook_export_spur_app").with_arguments(arguments))
        .await
        .expect("export succeeds through server dispatch");

    let body = structured(result);
    assert_eq!(PathBuf::from(body["path"].as_str().unwrap()), output_path);
    assert_eq!(body["manifest"]["entry_notebook"], "app.ipynb");

    client.cancel().await.expect("client closes");
}

#[tokio::test]
async fn spur_app_mcp_export_returns_structured_package_result() {
    let temp = tempfile::tempdir().expect("tempdir");
    let notebook_path = temp.path().join("forecast.ipynb");
    let output_path = temp.path().join("forecast.spurapp");
    fs::write(&notebook_path, minimal_notebook()).expect("seed notebook");

    let result = export_spur_app::call(
        &deps(),
        json!({
            "notebook_path": notebook_path,
            "output_path": output_path,
            "name": "Forecast Dashboard"
        }),
    )
    .await
    .expect("export succeeds");

    let body = structured(result);
    assert_eq!(body["ok"], true);
    assert_eq!(PathBuf::from(body["path"].as_str().unwrap()), output_path);
    assert_eq!(body["manifest"]["schema"], SPUR_APP_SCHEMA);
    assert_eq!(body["manifest"]["name"], "Forecast Dashboard");
    assert_eq!(body["manifest"]["entry_notebook"], "app.ipynb");
    assert_eq!(body["asset_count"], 0);
    assert_eq!(body["preflight"]["missing_dependency_locks"], json!([]));
}

#[tokio::test]
async fn spur_app_mcp_export_includes_dependency_locks_from_notebook_parent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let notebook_path = temp.path().join("forecast.ipynb");
    let output_path = temp.path().join("forecast.spurapp");
    let requirements = "pandas==2.2.0\n";
    fs::write(&notebook_path, minimal_notebook()).expect("seed notebook");
    fs::write(temp.path().join("requirements.txt"), requirements).expect("seed requirements");

    let result = export_spur_app::call(
        &deps(),
        json!({
            "notebook_path": notebook_path,
            "output_path": output_path,
            "name": "Forecast Dashboard"
        }),
    )
    .await
    .expect("export succeeds");

    let body = structured(result);
    assert_eq!(
        body["manifest"]["dependencies"]["python"],
        "env/requirements.txt"
    );

    let package = fs::read(&output_path).expect("read package");
    let manifest = spur_notebook::spur_app::archive::read_manifest(Cursor::new(package.as_slice()))
        .expect("read manifest from package");
    assert_eq!(
        manifest.dependencies.python.as_deref(),
        Some("env/requirements.txt")
    );
    let archived_requirements =
        spur_notebook::spur_app::archive::read_entry(Cursor::new(package), "env/requirements.txt")
            .expect("read requirements entry");
    assert_eq!(archived_requirements, requirements.as_bytes());
}

#[tokio::test]
async fn spur_app_mcp_import_returns_structured_notebook_result() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("home dir");
    let _home_guard = HomeGuard::set(&home).await;

    let package_path = temp.path().join("forecast.spurapp");
    write_spurapp(&package_path, "Forecast Import", minimal_notebook());

    let result = import_spur_app::call(&deps(), json!({ "path": package_path }))
        .await
        .expect("import succeeds");

    let body = structured(result);
    let notebook_path = PathBuf::from(body["notebook_path"].as_str().unwrap());
    assert_eq!(body["ok"], true);
    assert!(notebook_path.exists());
    assert_eq!(
        fs::read_to_string(&notebook_path).unwrap(),
        minimal_notebook()
    );
    assert_eq!(body["manifest"]["schema"], SPUR_APP_SCHEMA);
    assert_eq!(body["manifest"]["name"], "Forecast Import");
    assert_eq!(body["preflight"]["missing_dependency_locks"], json!([]));
}

#[tokio::test]
async fn spur_app_mcp_import_can_open_imported_notebook_through_daemon() {
    let temp = tempfile::tempdir().expect("tempdir");
    let home = temp.path().join("home");
    fs::create_dir_all(&home).expect("home dir");
    let _home_guard = HomeGuard::set(&home).await;

    let package_path = temp.path().join("forecast.spurapp");
    write_spurapp(&package_path, "Forecast Open", minimal_notebook());

    let windows = Arc::new(RecordingWindowOps::default());
    let state = Arc::new(State::new());
    let daemon = NotebookDaemonControl::new_with_parts_for_test(
        Arc::new(AgentBridge::new()),
        Arc::new(ReadyBridge),
        state.clone(),
        windows.clone(),
        Some(temp.path().join("last.json")),
    );
    let deps = ServerDeps {
        bridge: Arc::new(NullBridge),
        state: Some(state),
        app: None,
        daemon: Some(daemon),
    };

    let result = import_spur_app::call(&deps, json!({ "path": package_path, "open": true }))
        .await
        .expect("import and open succeeds");

    let body = structured(result);
    let notebook_path = PathBuf::from(body["notebook_path"].as_str().unwrap());
    assert_eq!(windows.opened(), vec![notebook_path]);
}

#[tokio::test]
async fn spur_app_mcp_path_validation_keeps_spurapp_scoped_to_spur_app_tools() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source_package = temp.path().join("source.spurapp");
    let output_package = temp.path().join("output.spurapp");
    let raw_notebook = temp.path().join("raw.ipynb");

    let export_error = export_spur_app::call(
        &deps(),
        json!({
            "notebook_path": source_package,
            "output_path": output_package
        }),
    )
    .await
    .expect_err("export source must be .ipynb");
    assert_invalid_path(export_error, ".ipynb");

    let import_error = import_spur_app::call(&deps(), json!({ "path": raw_notebook }))
        .await
        .expect_err("import path must be .spurapp");
    assert_invalid_path(import_error, ".spurapp");

    let save_error = save::call(
        &deps(),
        json!({
            "path": temp.path().join("notebook.spurapp"),
            "contents": minimal_notebook_value()
        }),
    )
    .await
    .expect_err("existing notebook tools must still reject .spurapp");
    assert_invalid_path(save_error, ".ipynb");
}

#[derive(Default)]
struct NullBridge;

impl BridgeRequester for NullBridge {
    fn listener_registered(&self) -> bool {
        false
    }

    fn window_alive(&self) -> bool {
        false
    }

    fn notebook_open(&self) -> bool {
        false
    }

    fn request<'a>(
        &'a self,
        method: &'static str,
        _params: Value,
        _timeout: Duration,
    ) -> BridgeRequestFuture<'a> {
        Box::pin(async move {
            Err(BridgeError::Handler {
                code: "unexpected_bridge_call".to_string(),
                message: format!("unexpected bridge call to {method}"),
            })
        })
    }
}

#[derive(Default)]
struct ReadyBridge;

impl BridgeRequester for ReadyBridge {
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
        _timeout: Duration,
    ) -> BridgeRequestFuture<'a> {
        Box::pin(async { Ok(Value::Null) })
    }
}

#[derive(Default)]
struct RecordingWindowOps {
    opened: Mutex<Vec<PathBuf>>,
}

impl RecordingWindowOps {
    fn opened(&self) -> Vec<PathBuf> {
        self.opened.lock().expect("opened lock").clone()
    }
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

struct HomeGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    original: Option<std::ffi::OsString>,
}

static HOME_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl HomeGuard {
    async fn set(path: &Path) -> Self {
        let lock = HOME_LOCK.lock().await;
        let original = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        Self {
            _lock: lock,
            original,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.original.take() {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn deps() -> ServerDeps {
    ServerDeps::from_bridge(Arc::new(NullBridge))
}

fn structured(result: CallToolResult) -> Value {
    assert_eq!(result.is_error, Some(false));
    result.structured_content.expect("structured content")
}

fn assert_invalid_path(error: McpError, expected_extension: &str) {
    assert!(
        error.message.contains(expected_extension),
        "expected {expected_extension} in error message: {}",
        error.message
    );
    let serialized = serde_json::to_value(&error).expect("serialize error");
    assert_eq!(serialized["data"]["code"], "invalid_path");
}

fn write_spurapp(path: &Path, name: &str, notebook_json: &str) {
    let manifest = SpurAppManifest::minimal(name, "app.ipynb");
    let mut package = Cursor::new(Vec::new());
    write_entries(
        &mut package,
        vec![
            (
                SPUR_APP_MANIFEST.to_string(),
                serde_json::to_vec(&manifest).expect("serialize manifest"),
            ),
            ("app.ipynb".to_string(), notebook_json.as_bytes().to_vec()),
        ],
    )
    .expect("write package");
    fs::write(path, package.into_inner()).expect("persist package");
}

fn minimal_notebook() -> &'static str {
    r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#
}

fn minimal_notebook_value() -> Value {
    serde_json::from_str(minimal_notebook()).expect("notebook json")
}

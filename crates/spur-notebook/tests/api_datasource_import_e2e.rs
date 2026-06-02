use std::{
    path::Path,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use arrow_array::{Int64Array, StringArray};
use chrono::Utc;
use duckdb::Connection;
use jute::state::State;
use serde_json::{json, Value};
use spur_notebook::connection_store::{self, ConnectionTemplate};
use spur_notebook::mcp::{
    bridge::{AgentBridge, BridgeError, BridgeRequestFuture, BridgeRequester},
    DaemonControlRequest, DaemonWindowOps, NotebookDaemonControl,
};
use spur_rest_table_gateway::{
    adapter::{
        manifest::Manifest,
        manifest_adapter::ManifestAdapter,
        nango::{manifest_to_toml, parse_providers, provider_to_manifest_stub},
        openapi, Adapter, ResolvedAuth, ScanRequest,
    },
    vtab::{bridge::IoBridge, register::register_tables},
};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const NANGO_PROVIDERS_SNAPSHOT: &str =
    include_str!("../jute-notebook/src-tauri/src/nango_providers_snapshot.yaml");

const OPENAPI_SCORES_SPEC: &str = r#"
openapi: 3.0.3
info:
  title: Scores
  version: "1"
paths:
  /scores:
    get:
      operationId: scores
      responses:
        "200":
          description: OK
          content:
            application/json:
              schema:
                type: object
                properties:
                  data:
                    type: array
                    items:
                      type: object
                      properties:
                        id:
                          type: string
                        score:
                          type: integer
"#;

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn preserve(key: &'static str) -> Self {
        Self {
            key,
            previous: std::env::var(key).ok(),
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.key, previous);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

#[derive(Default)]
struct ClosedNotebookBridge;

impl BridgeRequester for ClosedNotebookBridge {
    fn listener_registered(&self) -> bool {
        true
    }

    fn window_alive(&self) -> bool {
        true
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
                code: "unexpected_bridge_request".to_string(),
                message: format!("unexpected notebook bridge request: {method}"),
            })
        })
    }
}

#[derive(Default)]
struct RecordingWindowOps {
    connections_changed: StdMutex<Vec<Value>>,
}

impl RecordingWindowOps {
    fn connections_changed_count(&self) -> usize {
        self.connections_changed
            .lock()
            .expect("connections_changed lock")
            .len()
    }
}

impl DaemonWindowOps for RecordingWindowOps {
    fn show_and_focus(&self, _label: &str) -> bool {
        false
    }

    fn hide(&self, _label: &str) {}

    fn open_notebook_path(&self, path: &Path) -> Result<String, BridgeError> {
        Ok(format!("window-{}", path.display()))
    }

    fn emit_recents_changed(&self, _event: &jute::commands::RecentsChangedEvent) {}

    fn emit_connections_changed(&self, payload: &Value) {
        self.connections_changed
            .lock()
            .expect("connections_changed lock")
            .push(payload.clone());
    }

    fn exit(&self) {}
}

fn scan_request(table: &str) -> ScanRequest {
    ScanRequest {
        table: table.to_string(),
        predicates: Vec::new(),
        projection: None,
        tvf_args: Vec::new(),
        auth: ResolvedAuth::None,
    }
}

fn datasource_entry_from_response(
    response: &spur_notebook::mcp::DaemonControlResponse,
) -> jute::commands::DatasourceEntry {
    match serde_json::from_value(response.result.clone().expect("datasource control result"))
        .expect("daemon control result decodes")
    {
        jute::commands::DaemonControlResult::Datasource(entry) => entry,
        result => panic!("unexpected daemon control result: {result:?}"),
    }
}

fn manifest_for_stripe_scores_import() -> Manifest {
    let providers = parse_providers(NANGO_PROVIDERS_SNAPSHOT).expect("providers yaml parses");
    let provider = providers.get("stripe").expect("stripe provider is present");
    let manifest_stub = provider_to_manifest_stub("stripe", provider);
    let mut manifest_toml = manifest_to_toml(&manifest_stub);
    let spec = openapi::parse_spec(OPENAPI_SCORES_SPEC).expect("OpenAPI spec parses");
    let tables = openapi::spec_to_tables(&spec);
    manifest_toml.push_str(&openapi::tables_to_toml(&tables));
    Manifest::from_toml(&manifest_toml).expect("imported manifest parses")
}

fn saved_connection_manifest_toml(missing_env_var: &str) -> String {
    format!(
        r#"
[source]
name = "saved"
base_url = "https://example.invalid"
auth = {{ scheme = "bearer", env = "{missing_env_var}" }}

[[table]]
name = "scores"
path = "/scores"

[table.columns]
id = {{ json = "$.id", type = "Utf8" }}
score = {{ json = "$.score", type = "Int64" }}
"#
    )
}

fn daemon_request(value: Value) -> jute::commands::DaemonControlRequest {
    serde_json::from_value(value).expect("daemon control request deserializes")
}

#[tokio::test]
async fn imported_api_datasource_registers_and_scans_typed_rows() {
    let _api_key_guard = EnvGuard::preserve("STRIPE_API_KEY");
    let _base_url_guard = EnvGuard::preserve("SPUR_CONN_stripe_base_url");
    let server = MockServer::start().await;
    let name = format!("scores_{}", uuid::Uuid::new_v4().simple());
    let api_key_value = "Bearer test-api-key";
    let base_url_value = server.uri();
    let _cleanup = connection_store::remove(&name).await;

    Mock::given(method("GET"))
        .and(path("/scores"))
        .and(header("authorization", api_key_value))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "id": "a", "score": 1 },
                { "id": "b", "score": 2 }
            ]
        })))
        .mount(&server)
        .await;

    let jute_state = Arc::new(State::new());
    let control = NotebookDaemonControl::new_with_parts_for_test(
        Arc::new(AgentBridge::new()),
        Arc::new(ClosedNotebookBridge),
        Arc::clone(&jute_state),
        Arc::new(RecordingWindowOps::default()),
        None,
    );
    let request = serde_json::from_value::<jute::commands::DaemonControlRequest>(json!({
        "daemon": "notebook.v1",
        "command": "add_api_datasource_from_import",
        "name": name.clone(),
        "provider": "stripe",
        "spec_text": OPENAPI_SCORES_SPEC,
        "credentials": [
            ["STRIPE_API_KEY", api_key_value],
            ["SPUR_CONN_stripe_base_url", base_url_value]
        ]
    }))
    .expect("add_api_datasource_from_import command deserializes");

    let response = control
        .handle(DaemonControlRequest { id: None, request })
        .await;

    assert!(response.ok, "{:?}", response.error);
    let entry = datasource_entry_from_response(&response);
    assert_eq!(entry.name, name);
    assert_eq!(entry.path, "stripe");
    assert_eq!(entry.kind, jute::commands::DatasourceKind::ApiTables);
    assert_eq!(entry.group.as_deref(), Some("API"));
    assert_eq!(
        entry.tables,
        vec![jute::commands::Table {
            name: "stripe_scores".to_string(),
            columns: vec![
                jute::commands::Column {
                    name: "id".to_string(),
                    sql_type: "VARCHAR".to_string(),
                },
                jute::commands::Column {
                    name: "score".to_string(),
                    sql_type: "BIGINT".to_string(),
                },
            ],
            row_count: None,
        }]
    );
    let templates = connection_store::list()
        .await
        .expect("saved connections list after import");
    let saved = templates
        .iter()
        .find(|template| template.name == entry.name)
        .expect("import saved a reusable connection template");
    assert_eq!(saved.provider.as_deref(), Some("stripe"));
    assert_eq!(saved.group.as_deref(), Some("API"));
    assert_eq!(saved.tables, entry.tables);
    assert_eq!(
        saved.credential_env_vars,
        vec![
            "STRIPE_API_KEY".to_string(),
            "SPUR_CONN_stripe_base_url".to_string()
        ]
    );
    let serialized = serde_json::to_string(saved).expect("saved template serializes");
    assert!(serialized.contains("STRIPE_API_KEY"));
    assert!(serialized.contains("SPUR_CONN_stripe_base_url"));
    assert!(!serialized.contains(api_key_value));
    assert!(!serialized.contains(&base_url_value));
    assert!(!saved.manifest_toml.contains(api_key_value));
    assert!(!saved.manifest_toml.contains(&base_url_value));
    connection_store::remove(&entry.name)
        .await
        .expect("saved import template cleans up");
    assert_eq!(jute_state.datasource_catalog.lock().list(), vec![entry]);

    let adapter = ManifestAdapter::new(manifest_for_stripe_scores_import());
    let batches = adapter
        .scan(scan_request("scores"))
        .await
        .expect("imported manifest scans wiremock rows");
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 2);

    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id column is Utf8");
    let scores = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("score column is Int64");
    assert_eq!(ids.value(0), "a");
    assert_eq!(ids.value(1), "b");
    assert_eq!(scores.value(0), 1);
    assert_eq!(scores.value(1), 2);

    let adapter: Arc<dyn Adapter> = Arc::new(adapter);
    let conn = Connection::open_in_memory().expect("duckdb opens in memory");
    let bridge = Arc::new(IoBridge::new());
    assert_eq!(
        register_tables(&conn, adapter, bridge).expect("table function registers"),
        1
    );

    let rows = tokio::task::spawn_blocking(move || {
        let mut stmt = conn
            .prepare("SELECT id, score FROM stripe_scores() ORDER BY id")
            .expect("query prepares");
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .expect("query maps rows");

        rows.collect::<duckdb::Result<Vec<_>>>()
            .expect("rows collect")
    })
    .await
    .expect("blocking duckdb query joins");

    assert_eq!(rows, vec![("a".to_string(), 1), ("b".to_string(), 2)]);
}

#[tokio::test]
async fn saved_connection_list_attach_delete_roundtrip_reports_missing_env() {
    let missing_env_var = "SPUR_SAVED_CONNECTION_ATTACH_MISSING_E2E";
    let _missing_guard = EnvGuard::preserve(missing_env_var);
    std::env::remove_var(missing_env_var);

    let name = format!("saved_scores_{}", uuid::Uuid::new_v4().simple());
    let _cleanup = connection_store::remove(&name).await;

    connection_store::upsert(ConnectionTemplate {
        name: name.clone(),
        provider: Some("stripe".to_string()),
        group: Some("API".to_string()),
        manifest_toml: saved_connection_manifest_toml(missing_env_var),
        tables: Vec::new(),
        credential_env_vars: vec![missing_env_var.to_string()],
        created_at: Utc::now(),
        updated_at: Utc::now(),
    })
    .await
    .expect("saved connection template writes");

    let jute_state = Arc::new(State::new());
    let windows = Arc::new(RecordingWindowOps::default());
    let control = NotebookDaemonControl::new_with_parts_for_test(
        Arc::new(AgentBridge::new()),
        Arc::new(ClosedNotebookBridge),
        Arc::clone(&jute_state),
        windows.clone(),
        None,
    );

    let list_response = control
        .handle(DaemonControlRequest {
            id: None,
            request: daemon_request(json!({
                "daemon": "notebook.v1",
                "command": "list_saved_connections",
            })),
        })
        .await;
    assert!(list_response.ok, "{:?}", list_response.error);
    let list_result = list_response.result.expect("list result");
    assert_eq!(list_result["type"], "savedConnections");
    let listed = list_result["data"].as_array().expect("saved list data");
    assert!(listed
        .iter()
        .any(|template| template["name"] == name && template["provider"] == "stripe"));

    let attach_response = control
        .handle(DaemonControlRequest {
            id: None,
            request: daemon_request(json!({
                "daemon": "notebook.v1",
                "command": "attach_saved_connection",
                "name": name,
            })),
        })
        .await;
    assert!(attach_response.ok, "{:?}", attach_response.error);
    let attach_result = attach_response.result.expect("attach result");
    assert_eq!(attach_result["type"], "attachedSavedConnection");
    let payload = &attach_result["data"];
    assert_eq!(payload["missing_env_vars"], json!([missing_env_var]));
    assert_eq!(payload["entry"]["name"], name);
    assert_eq!(payload["entry"]["path"], "saved");
    assert_eq!(payload["entry"]["tables"][0]["name"], "saved_scores");

    let catalog = jute_state.datasource_catalog.lock().list();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0].name, name);
    assert_eq!(catalog[0].tables[0].name, "saved_scores");
    assert_eq!(windows.connections_changed_count(), 1);

    let delete_response = control
        .handle(DaemonControlRequest {
            id: None,
            request: daemon_request(json!({
                "daemon": "notebook.v1",
                "command": "delete_saved_connection",
                "name": name,
            })),
        })
        .await;
    assert!(delete_response.ok, "{:?}", delete_response.error);
    assert_eq!(windows.connections_changed_count(), 2);
    let templates = connection_store::list()
        .await
        .expect("saved connections list after delete");
    assert!(!templates.iter().any(|template| template.name == name));
}

use std::{path::Path, sync::Arc, time::Duration};

use arrow_array::{Int64Array, StringArray};
use duckdb::Connection;
use jute::state::State;
use serde_json::{json, Value};
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
struct RecordingWindowOps;

impl DaemonWindowOps for RecordingWindowOps {
    fn show_and_focus(&self, _label: &str) -> bool {
        false
    }

    fn hide(&self, _label: &str) {}

    fn open_notebook_path(&self, path: &Path) -> Result<String, BridgeError> {
        Ok(format!("window-{}", path.display()))
    }

    fn emit_recents_changed(&self, _event: &jute::commands::RecentsChangedEvent) {}

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

#[tokio::test]
async fn imported_api_datasource_registers_and_scans_typed_rows() {
    let _api_key_guard = EnvGuard::preserve("STRIPE_API_KEY");
    let _base_url_guard = EnvGuard::preserve("SPUR_CONN_stripe_base_url");
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/scores"))
        .and(header("authorization", "Bearer test-api-key"))
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
        Arc::new(RecordingWindowOps),
        None,
    );
    let request = serde_json::from_value::<jute::commands::DaemonControlRequest>(json!({
        "daemon": "notebook.v1",
        "command": "add_api_datasource_from_import",
        "name": "scores",
        "provider": "stripe",
        "spec_text": OPENAPI_SCORES_SPEC,
        "credentials": [
            ["STRIPE_API_KEY", "Bearer test-api-key"],
            ["SPUR_CONN_stripe_base_url", server.uri()]
        ]
    }))
    .expect("add_api_datasource_from_import command deserializes");

    let response = control
        .handle(DaemonControlRequest { id: None, request })
        .await;

    assert!(response.ok, "{:?}", response.error);
    let entry = datasource_entry_from_response(&response);
    assert_eq!(entry.name, "scores");
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

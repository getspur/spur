use arrow_array::{Int64Array, StringArray};
use spur_rest_table_gateway::adapter::manifest::Manifest;
use spur_rest_table_gateway::adapter::manifest_adapter::ManifestAdapter;
use spur_rest_table_gateway::adapter::{openapi, Adapter, ResolvedAuth, ScanRequest};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OPENAPI_SPEC: &str = r#"
openapi: 3.0.3
info:
  title: OpenAPI import e2e
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

fn scan_request(table: &str) -> ScanRequest {
    ScanRequest {
        table: table.to_string(),
        predicates: Vec::new(),
        projection: None,
        tvf_args: Vec::new(),
        auth: ResolvedAuth::None,
    }
}

fn unique_temp_dir(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after UNIX epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "spur-rest-table-gateway-{name}-{}-{nanos}",
        std::process::id()
    ))
}

#[tokio::test]
async fn generated_openapi_table_scans_typed_rows() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/scores"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                { "id": "a", "score": 1 },
                { "id": "b", "score": 2 }
            ]
        })))
        .mount(&server)
        .await;

    let spec = openapi::parse_spec(OPENAPI_SPEC).expect("OpenAPI spec should parse");
    let tables = openapi::spec_to_tables(&spec);
    assert_eq!(tables.len(), 1);

    let combined_toml = format!(
        r#"[source]
name = "openapi_import_e2e"
base_url = "{}"
auth = {{ scheme = "none" }}
{}
"#,
        server.uri(),
        openapi::tables_to_toml(&tables)
    );
    let manifest = Manifest::from_toml(&combined_toml).expect("manifest TOML should parse");
    let adapter = ManifestAdapter::new(manifest);

    let batches = adapter
        .scan(scan_request("scores"))
        .await
        .expect("generated table should scan");

    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 2);

    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id should be Utf8");
    let scores = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("score should be Int64");

    assert_eq!(ids.value(0), "a");
    assert_eq!(ids.value(1), "b");
    assert_eq!(scores.value(0), 1);
    assert_eq!(scores.value(1), 2);
}

#[test]
fn openapi_import_cli_appends_tables_to_connection_stub() {
    let bin = option_env!("CARGO_BIN_EXE_openapi-import")
        .expect("openapi-import binary should be built for integration tests");
    let temp = unique_temp_dir("cli-into");
    let spec_path = temp.join("scores.yaml");
    let out_dir = temp.join("tables");
    let stub_path = temp.join("scores.connection.toml");

    std::fs::create_dir_all(&temp).expect("temp dir should be created");
    std::fs::write(&spec_path, OPENAPI_SPEC).expect("spec should be written");
    std::fs::write(
        &stub_path,
        r#"[source]
name = "scores"
base_url = "https://example.invalid"
auth = { scheme = "none" }
"#,
    )
    .expect("stub should be written");

    let output = Command::new(bin)
        .arg(&spec_path)
        .arg(&out_dir)
        .arg("--into")
        .arg(&stub_path)
        .output()
        .expect("openapi-import should run");

    assert!(
        output.status.success(),
        "openapi-import failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "generated 1 tables (0 GET endpoints skipped)"
    );

    let combined = std::fs::read_to_string(&stub_path).expect("stub should be updated");
    let manifest = Manifest::from_toml(&combined).expect("updated stub should parse");
    assert_eq!(manifest.tables.len(), 1);
    assert_eq!(manifest.tables[0].name, "scores");

    std::fs::remove_dir_all(temp).ok();
}

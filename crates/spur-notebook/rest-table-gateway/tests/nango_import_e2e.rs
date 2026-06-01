use arrow_array::{BooleanArray, Float64Array, Int64Array, StringArray};
use spur_rest_table_gateway::adapter::manifest::Manifest;
use spur_rest_table_gateway::adapter::manifest_adapter::ManifestAdapter;
use spur_rest_table_gateway::adapter::nango::{
    manifest_to_toml, parse_providers, provider_to_manifest_stub,
};
use spur_rest_table_gateway::adapter::{Adapter, ResolvedAuth, ScanRequest};
use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PROVIDERS_YAML: &str = r#"
nango-import-e2e-widgets:
  display_name: Nango Import E2E Widgets
  categories:
    - tests
  auth_mode: API_KEY
  proxy:
    base_url: "${connectionConfig.nango_import_e2e_base_url}"
    headers:
      x-api-key: "${apiKey}"
    paginate:
      type: cursor
      cursor_path_in_response: "$.meta.next_cursor"
      cursor_name_in_request: cursor
      response_path: "$.data.items"
"#;

struct EnvGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
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

fn scan_request(table: &str) -> ScanRequest {
    ScanRequest {
        table: table.to_string(),
        predicates: Vec::new(),
        projection: None,
        tvf_args: Vec::new(),
        auth: ResolvedAuth::None,
    }
}

#[tokio::test]
async fn nango_imported_api_key_manifest_scans_cursor_paginated_envelope() {
    let server = MockServer::start().await;
    let _base_url = EnvGuard::set("SPUR_CONN_nango_import_e2e_base_url", &server.uri());
    let _api_key = EnvGuard::set("NANGO_IMPORT_E2E_WIDGETS_API_KEY", "test-api-key");

    Mock::given(method("GET"))
        .and(path("/widgets"))
        .and(header("x-api-key", "test-api-key"))
        .and(query_param_is_missing("cursor"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "items": [
                    { "id": "w1", "quantity": 7, "active": true, "price": "12.50" }
                ]
            },
            "meta": { "next_cursor": "page-2" }
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/widgets"))
        .and(header("x-api-key", "test-api-key"))
        .and(query_param("cursor", "page-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "items": [
                    { "id": "w2", "quantity": "8", "active": false, "price": 4.25 },
                    { "id": "w3", "quantity": 9, "active": true, "price": "0.75" }
                ]
            },
            "meta": { "next_cursor": null }
        })))
        .mount(&server)
        .await;

    let providers = parse_providers(PROVIDERS_YAML).expect("providers yaml should parse");
    let provider = providers
        .get("nango-import-e2e-widgets")
        .expect("provider should be present");
    let manifest_stub = provider_to_manifest_stub("nango-import-e2e-widgets", provider);
    let manifest_toml = manifest_to_toml(&manifest_stub);
    let combined_toml = format!(
        r#"{manifest_toml}
[[table]]
name = "widgets"
path = "/widgets"
response_path = "$.data.items"

[table.columns]
id = {{ json = "$.id", type = "Utf8" }}
quantity = {{ json = "$.quantity", type = "Int64" }}
active = {{ json = "$.active", type = "Boolean" }}
price = {{ json = "$.price", type = "Float64" }}
"#
    );

    let manifest = Manifest::from_toml(&combined_toml).expect("manifest toml should parse");
    let adapter = ManifestAdapter::new(manifest);

    let batches = adapter
        .scan(scan_request("widgets"))
        .await
        .expect("scan should fetch paginated typed rows");

    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 3);

    let ids = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("id should be Utf8");
    let quantities = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("quantity should be Int64");
    let active = batch
        .column(2)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("active should be Boolean");
    let prices = batch
        .column(3)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("price should be Float64");

    assert_eq!(ids.value(0), "w1");
    assert_eq!(ids.value(1), "w2");
    assert_eq!(ids.value(2), "w3");
    assert_eq!(quantities.value(0), 7);
    assert_eq!(quantities.value(1), 8);
    assert_eq!(quantities.value(2), 9);
    assert!(active.value(0));
    assert!(!active.value(1));
    assert!(active.value(2));
    assert!((prices.value(0) - 12.50).abs() < f64::EPSILON);
    assert!((prices.value(1) - 4.25).abs() < f64::EPSILON);
    assert!((prices.value(2) - 0.75).abs() < f64::EPSILON);
}

use arrow_array::{BooleanArray, Int64Array, StringArray};
use spur_rest_table_gateway::adapter::manifest::{AuthCfg, Manifest};
use spur_rest_table_gateway::adapter::manifest_adapter::ManifestAdapter;
use spur_rest_table_gateway::adapter::{
    Adapter, Predicate, PredicateOp, ResolvedAuth, ScalarValue, ScanRequest,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn scan_request(table: &str, predicates: Vec<Predicate>) -> ScanRequest {
    ScanRequest {
        table: table.to_string(),
        predicates,
        projection: None,
        tvf_args: Vec::new(),
        auth: ResolvedAuth::None,
    }
}

#[tokio::test]
async fn algolia_supported_manifest_scans_indices_with_connection_auth() {
    let server = MockServer::start().await;
    let _app_id = EnvGuard::set("SPUR_CONN_algolia_app_id", "ALGAPPID");
    let _api_key = EnvGuard::set("ALGOLIA_API_KEY", "algolia-test-key");

    Mock::given(method("GET"))
        .and(path("/1/indexes"))
        .and(header("x-algolia-api-key", "algolia-test-key"))
        .and(header("x-algolia-application-id", "ALGAPPID"))
        .and(query_param("hitsPerPage", "2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "items": [
                {
                    "name": "products",
                    "entries": 125,
                    "dataSize": 4096,
                    "fileSize": 2048,
                    "updatedAt": "2026-06-12T00:00:00Z",
                    "primary": "products",
                    "pendingTask": false
                },
                {
                    "name": "products_replica",
                    "entries": "125",
                    "dataSize": "4096",
                    "fileSize": "2048",
                    "updatedAt": "2026-06-12T00:05:00Z",
                    "primary": "products",
                    "pendingTask": true
                }
            ]
        })))
        .mount(&server)
        .await;

    let manifest_toml = include_str!("../connections/supported/algolia.connection.toml").replace(
        "https://${connectionConfig.algolia_app_id}-dsn.algolia.net",
        &server.uri(),
    );
    let manifest = Manifest::from_toml(&manifest_toml).expect("algolia manifest parses");

    assert_eq!(manifest.source.name, "algolia");
    assert_eq!(
        manifest.source.connection_config,
        vec!["algolia_app_id".to_string()]
    );
    assert!(matches!(
        manifest.source.auth,
        AuthCfg::Header { ref name, ref env }
            if name == "x-algolia-api-key" && env == "ALGOLIA_API_KEY"
    ));
    assert_eq!(
        manifest
            .source
            .headers
            .get("x-algolia-application-id")
            .map(String::as_str),
        Some("${connectionConfig.algolia_app_id}")
    );

    let adapter = ManifestAdapter::new(manifest);
    let batches = adapter
        .scan(scan_request(
            "list_indices",
            vec![Predicate {
                column: "hits_per_page".to_string(),
                op: PredicateOp::Eq,
                value: ScalarValue::Int64(2),
            }],
        ))
        .await
        .expect("algolia indices scan succeeds");

    assert_eq!(batches.len(), 1);
    let batch = &batches[0];
    assert_eq!(batch.num_rows(), 2);

    let names = batch
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("name should be Utf8");
    let entries = batch
        .column(1)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("entries should be Int64");
    let pending = batch
        .column(6)
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("pending_task should be Boolean");

    assert_eq!(names.value(0), "products");
    assert_eq!(names.value(1), "products_replica");
    assert_eq!(entries.value(0), 125);
    assert_eq!(entries.value(1), 125);
    assert!(!pending.value(0));
    assert!(pending.value(1));
}

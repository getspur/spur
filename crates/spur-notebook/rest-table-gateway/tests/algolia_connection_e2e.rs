#[path = "support/provider_manifest_harness.rs"]
mod provider_manifest_harness;

use spur_rest_table_gateway::adapter::manifest::AuthCfg;
use spur_rest_table_gateway::adapter::{Predicate, PredicateOp, ScalarValue};
use wiremock::MockServer;

use provider_manifest_harness::{
    scan_request_with_predicates, ExpectedRequest, ProviderManifestHarness, TypedCell,
};

#[tokio::test]
async fn algolia_supported_manifest_scans_indices_with_connection_auth() {
    let server = MockServer::start().await;
    let mut harness = ProviderManifestHarness::from_toml(
        "algolia",
        include_str!("../connections/supported/algolia.connection.toml"),
    )
    .expect("algolia manifest parses");
    harness.replace_base_url(
        "https://${connectionConfig.algolia_app_id}-dsn.algolia.net",
        &server.uri(),
    );
    let _env = harness.install_env();

    ExpectedRequest::get("/1/indexes")
        .with_manifest_auth(harness.manifest(), "algolia")
        .header("x-algolia-application-id", "algolia_algolia_app_id_value")
        .query_param("hitsPerPage", "2")
        .respond_json(serde_json::json!({
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
        }))
        .mount(&server)
        .await;

    let manifest = harness.manifest();
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

    let batches = harness
        .scan(scan_request_with_predicates(
            "list_indices",
            vec![Predicate {
                column: "hits_per_page".to_string(),
                op: PredicateOp::Eq,
                value: ScalarValue::Int64(2),
            }],
        ))
        .await
        .expect("algolia indices scan succeeds");

    harness.assert_typed_rows("list_indices", &batches, 2);
    harness.assert_typed_cell(&batches[0], "name", 0, TypedCell::Utf8("products"));
    harness.assert_typed_cell(&batches[0], "name", 1, TypedCell::Utf8("products_replica"));
    harness.assert_typed_cell(&batches[0], "entries", 0, TypedCell::Int64(125));
    harness.assert_typed_cell(&batches[0], "entries", 1, TypedCell::Int64(125));
    harness.assert_typed_cell(&batches[0], "pending_task", 0, TypedCell::Boolean(false));
    harness.assert_typed_cell(&batches[0], "pending_task", 1, TypedCell::Boolean(true));
}

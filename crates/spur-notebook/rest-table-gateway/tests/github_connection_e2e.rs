#[path = "support/provider_manifest_harness.rs"]
mod provider_manifest_harness;

use spur_rest_table_gateway::adapter::manifest::AuthCfg;
use spur_rest_table_gateway::adapter::{Predicate, PredicateOp, ScalarValue};
use wiremock::MockServer;

use provider_manifest_harness::{
    scan_request_with_predicates, ExpectedRequest, ProviderManifestHarness, TypedCell,
};

#[tokio::test]
async fn github_supported_manifest_scans_advisories_with_bearer_auth() {
    let server = MockServer::start().await;
    let mut harness = ProviderManifestHarness::from_toml(
        "github",
        include_str!("../connections/supported/github.connection.toml"),
    )
    .expect("github manifest parses");
    harness.replace_base_url("https://api.github.com", &server.uri());
    let _env = harness.install_env();

    ExpectedRequest::get("/advisories")
        .with_manifest_auth(harness.manifest(), "github")
        .header("x-github-api-version", "2022-11-28")
        .query_param("severity", "high")
        .respond_json(serde_json::json!([
            {
                "ghsa_id": "GHSA-xxxx-yyyy-zzzz",
                "cve_id": "CVE-2026-0001",
                "summary": "Mock advisory",
                "severity": "high",
                "published_at": "2026-06-01T00:00:00Z",
                "updated_at": "2026-06-02T00:00:00Z"
            }
        ]))
        .mount(&server)
        .await;

    assert_eq!(harness.manifest().source.name, "github");
    assert!(
        matches!(harness.manifest().source.auth, AuthCfg::Bearer { ref env } if env == "GITHUB_TOKEN")
    );
    assert!(harness
        .manifest()
        .tables
        .iter()
        .any(|table| table.name == "security_advisories"));

    let batches = harness
        .scan(scan_request_with_predicates(
            "security_advisories",
            vec![Predicate {
                column: "severity".to_string(),
                op: PredicateOp::Eq,
                value: ScalarValue::Utf8("high".to_string()),
            }],
        ))
        .await
        .expect("github advisories scan succeeds");

    harness.assert_one_typed_row("security_advisories", &batches);
    harness.assert_typed_cell(
        &batches[0],
        "ghsa_id",
        0,
        TypedCell::Utf8("GHSA-xxxx-yyyy-zzzz"),
    );
    harness.assert_typed_cell(&batches[0], "severity", 0, TypedCell::Utf8("high"));
}

#[tokio::test]
async fn github_supported_manifest_scans_authenticated_repos() {
    let server = MockServer::start().await;
    let mut harness = ProviderManifestHarness::from_toml(
        "github",
        include_str!("../connections/supported/github.connection.toml"),
    )
    .expect("github manifest parses");
    harness.replace_base_url("https://api.github.com", &server.uri());
    let _env = harness.install_env();

    ExpectedRequest::get("/user/repos")
        .with_manifest_auth(harness.manifest(), "github")
        .respond_json(serde_json::json!([
            {
                "id": 42,
                "name": "spur",
                "full_name": "acme/spur",
                "private": true,
                "html_url": "https://github.com/acme/spur",
                "default_branch": "main",
                "updated_at": "2026-06-12T00:00:00Z"
            }
        ]))
        .mount(&server)
        .await;

    let batches = harness
        .scan(scan_request_with_predicates(
            "authenticated_repos",
            vec![Predicate {
                column: "visibility".to_string(),
                op: PredicateOp::Eq,
                value: ScalarValue::Utf8("private".to_string()),
            }],
        ))
        .await
        .expect("github repos scan succeeds");

    harness.assert_one_typed_row("authenticated_repos", &batches);
    harness.assert_typed_cell(&batches[0], "id", 0, TypedCell::Int64(42));
    harness.assert_typed_cell(&batches[0], "name", 0, TypedCell::Utf8("spur"));
    harness.assert_typed_cell(&batches[0], "private", 0, TypedCell::Boolean(true));
}

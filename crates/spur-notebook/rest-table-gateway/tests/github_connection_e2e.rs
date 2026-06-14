#[path = "support/provider_manifest_harness.rs"]
mod provider_manifest_harness;

use spur_rest_table_gateway::adapter::manifest::{ArgLocation, AuthCfg};
use spur_rest_table_gateway::adapter::manifest_adapter::ManifestAdapter;
use spur_rest_table_gateway::adapter::{Adapter, Predicate, PredicateOp, ScalarValue, TableKind};
use wiremock::matchers::body_json;
use wiremock::Mock;
use wiremock::MockServer;

use provider_manifest_harness::{
    action_request, scan_request_with_predicates, ExpectedRequest, ProviderManifestHarness,
    TypedCell,
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

#[tokio::test]
async fn github_supported_manifest_scans_notifications() {
    let server = MockServer::start().await;
    let mut harness = ProviderManifestHarness::from_toml(
        "github",
        include_str!("../connections/supported/github.connection.toml"),
    )
    .expect("github manifest parses");
    harness.replace_base_url("https://api.github.com", &server.uri());
    let _env = harness.install_env();

    ExpectedRequest::get("/notifications")
        .with_manifest_auth(harness.manifest(), "github")
        .header("x-github-api-version", "2022-11-28")
        .query_param("all", "true")
        .respond_json(serde_json::json!([
            {
                "id": "1001",
                "reason": "mention",
                "unread": true,
                "updated_at": "2026-06-13T00:00:00Z",
                "subject": {
                    "title": "Review requested",
                    "type": "PullRequest",
                    "url": "https://api.github.com/repos/acme/spur/pulls/7",
                    "latest_comment_url": "https://api.github.com/repos/acme/spur/issues/comments/8"
                },
                "repository": {
                    "full_name": "acme/spur"
                }
            }
        ]))
        .mount(&server)
        .await;

    let batches = harness
        .scan(scan_request_with_predicates(
            "notifications",
            vec![Predicate {
                column: "all".to_string(),
                op: PredicateOp::Eq,
                value: ScalarValue::Bool(true),
            }],
        ))
        .await
        .expect("github notifications scan succeeds");

    harness.assert_one_typed_row("notifications", &batches);
    harness.assert_typed_cell(&batches[0], "id", 0, TypedCell::Utf8("1001"));
    harness.assert_typed_cell(
        &batches[0],
        "subject_title",
        0,
        TypedCell::Utf8("Review requested"),
    );
    harness.assert_typed_cell(
        &batches[0],
        "repository_full_name",
        0,
        TypedCell::Utf8("acme/spur"),
    );
    harness.assert_typed_cell(&batches[0], "unread", 0, TypedCell::Boolean(true));
}

#[tokio::test]
async fn github_supported_manifest_exposes_and_invokes_write_actions() {
    let server = MockServer::start().await;
    let mut harness = ProviderManifestHarness::from_toml(
        "github",
        include_str!("../connections/supported/github.connection.toml"),
    )
    .expect("github manifest parses");
    harness.replace_base_url("https://api.github.com", &server.uri());
    let _env = harness.install_env();

    let manifest = harness.manifest();
    assert!(manifest.source.allow_writes);
    assert_eq!(
        manifest.tables.len(),
        12,
        "existing GitHub tables stay intact"
    );

    let action_names: Vec<_> = manifest
        .actions
        .iter()
        .map(|action| action.name.as_str())
        .collect();
    assert_eq!(
        action_names,
        [
            "create_issue",
            "add_issue_comment",
            "update_issue",
            "create_pull_request",
            "create_release"
        ]
    );

    let create_issue = manifest
        .actions
        .iter()
        .find(|action| action.name == "create_issue")
        .expect("create_issue action present");
    assert_eq!(create_issue.method, "POST");
    assert_eq!(create_issue.path, "/repos/{owner}/{repo}/issues");
    assert_eq!(create_issue.args["owner"].in_, ArgLocation::Path);
    assert!(create_issue.args["owner"].required);
    assert_eq!(create_issue.args["title"].in_, ArgLocation::Body);
    assert!(create_issue.args["title"].required);
    assert!(create_issue
        .columns
        .as_ref()
        .expect("create_issue typed columns")
        .contains_key("html_url"));

    let catalog = ManifestAdapter::new(manifest.clone()).catalog();
    for action_name in action_names {
        assert!(
            catalog
                .iter()
                .any(|def| def.name == action_name && matches!(def.kind, TableKind::Action { .. })),
            "{action_name} should build an action table definition"
        );
    }

    Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/repos/acme/spur/issues"))
        .and(wiremock::matchers::header(
            "authorization",
            "Bearer github_token",
        ))
        .and(wiremock::matchers::header(
            "x-github-api-version",
            "2022-11-28",
        ))
        .and(body_json(serde_json::json!({
            "title": "Ship actions",
            "body": "Make GitHub writes available",
            "labels": ["rest-gateway"]
        })))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "number": 7,
                "html_url": "https://github.com/acme/spur/issues/7",
                "state": "open"
            })),
        )
        .mount(&server)
        .await;

    let batches = harness
        .act(action_request(
            "create_issue",
            "POST",
            "/repos/acme/spur/issues",
            Vec::new(),
            Some(serde_json::json!({
                "title": "Ship actions",
                "body": "Make GitHub writes available",
                "labels": ["rest-gateway"]
            })),
        ))
        .await
        .expect("create_issue action succeeds");

    harness.assert_one_typed_row("create_issue", &batches);
    harness.assert_typed_cell(&batches[0], "number", 0, TypedCell::Int64(7));
    harness.assert_typed_cell(
        &batches[0],
        "html_url",
        0,
        TypedCell::Utf8("https://github.com/acme/spur/issues/7"),
    );
    harness.assert_typed_cell(&batches[0], "state", 0, TypedCell::Utf8("open"));
}

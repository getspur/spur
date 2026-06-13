#[path = "support/provider_manifest_harness.rs"]
mod provider_manifest_harness;

use wiremock::MockServer;

use provider_manifest_harness::{
    action_request, mount_oauth_refresh, scan_request, scan_request_with_predicates,
    ExpectedRequest, ProviderManifestHarness, TypedCell,
};
use spur_rest_table_gateway::adapter::manifest::{AuthCfg, Manifest};
use spur_rest_table_gateway::adapter::{Predicate, PredicateOp, ScalarValue};

#[tokio::test]
async fn harness_scans_api_key_query_manifest() {
    let server = MockServer::start().await;
    let toml = format!(
        r#"
[source]
name = "queryauth"
base_url = "{base}"
auth = {{ scheme = "api_key_query", param = "api_key", env = "QUERYAUTH_API_KEY" }}

[[table]]
name = "things"
path = "/things"

[table.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
        base = server.uri()
    );
    let harness = ProviderManifestHarness::from_toml("queryauth", &toml).expect("manifest parses");
    let _env = harness.install_env();

    ExpectedRequest::get("/things")
        .with_manifest_auth(harness.manifest(), "queryauth")
        .respond_json(serde_json::json!([{ "id": "thing-1" }]))
        .mount(&server)
        .await;

    let batches = harness
        .scan(scan_request("things"))
        .await
        .expect("query auth scan succeeds");

    harness.assert_one_typed_row("things", &batches);
    harness.assert_typed_cell(&batches[0], "id", 0, TypedCell::Utf8("thing-1"));
}

#[tokio::test]
async fn harness_invokes_oauth_refresh_action_with_typed_rows() {
    let server = MockServer::start().await;
    mount_oauth_refresh(&server, "/oauth/token", "minted-access-token").await;
    let toml = format!(
        r#"
[source]
name = "oauthaction"
base_url = "{base}"
allow_writes = true
auth = {{ scheme = "oauth2_refresh", token_url = "{base}/oauth/token", client_id_env = "OAUTHACTION_CLIENT_ID", client_secret_env = "OAUTHACTION_CLIENT_SECRET", refresh_token_env = "OAUTHACTION_REFRESH_TOKEN" }}

[[action]]
name = "create_thing"
method = "POST"
path = "/things"
response_path = "$.data"

[action.args]
name = {{ in = "body", type = "Utf8", required = true }}

[action.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
        base = server.uri()
    );
    let harness =
        ProviderManifestHarness::from_toml("oauthaction", &toml).expect("manifest parses");
    let _env = harness.install_env();

    ExpectedRequest::post("/things")
        .with_oauth_bearer(&harness.manifest().source.auth, "minted-access-token")
        .respond_json(serde_json::json!({
            "data": [{ "id": "thing-2" }]
        }))
        .mount(&server)
        .await;

    let batches = harness
        .act(action_request(
            "create_thing",
            "POST",
            "/things",
            Vec::new(),
            Some(serde_json::json!({ "name": "from harness" })),
        ))
        .await
        .expect("oauth action succeeds");

    harness.assert_one_typed_row("create_thing", &batches);
    harness.assert_typed_cell(&batches[0], "id", 0, TypedCell::Utf8("thing-2"));
}

#[tokio::test]
async fn provider_manifest_harness_scans_simple_auth_promotion_batch() {
    let provider_cases = [
        ProviderCase {
            provider_name: "github-pat",
            source_name: "github",
            manifest_file: "github.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "API_KEY",
                expected_base_url: "https://api.github.com",
                expected_auth: ExpectedAuth::Bearer,
                connection_config: &[],
            },
            table_name: "authenticated_repos",
            path: "/user/repos",
            response: serde_json::json!([{
                "id": 42,
                "name": "spur",
                "full_name": "acme/spur",
                "private": true,
                "html_url": "https://github.com/acme/spur",
                "default_branch": "main",
                "updated_at": "2026-06-12T00:00:00Z"
            }]),
            predicates: vec![Predicate {
                column: "visibility".to_string(),
                op: PredicateOp::Eq,
                value: ScalarValue::Utf8("private".to_string()),
            }],
            typed_column: "name",
            typed_value: TypedCell::Utf8("spur"),
        },
        ProviderCase {
            provider_name: "1password-events",
            source_name: "1password_events",
            manifest_file: "1password_events.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "API_KEY",
                expected_base_url: "https://${connectionConfig.domain}",
                expected_auth: ExpectedAuth::Bearer,
                connection_config: &["domain"],
            },
            table_name: "auth_introspection",
            path: "/api/v2/auth/introspect",
            response: serde_json::json!({
                "UUID": "svc-123",
                "IssuedAt": "2026-06-13T00:00:00Z",
                "Features": ["signinattempts"]
            }),
            predicates: Vec::new(),
            typed_column: "feature",
            typed_value: TypedCell::Utf8("signinattempts"),
        },
        ProviderCase {
            provider_name: "atlassian-admin",
            source_name: "atlassian_admin",
            manifest_file: "atlassian_admin.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "API_KEY",
                expected_base_url: "https://api.atlassian.com",
                expected_auth: ExpectedAuth::Bearer,
                connection_config: &["organizationId"],
            },
            table_name: "organizations",
            path: "/admin/v1/orgs",
            response: serde_json::json!({
                "data": [{
                    "id": "9fa3d2b7-k9l3-4bq1-z3d8-7x1m0a9e2b76",
                    "name": "SPUR"
                }]
            }),
            predicates: Vec::new(),
            typed_column: "name",
            typed_value: TypedCell::Utf8("SPUR"),
        },
        ProviderCase {
            provider_name: "azure-devops",
            source_name: "azure_devops",
            manifest_file: "azure_devops.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "BASIC",
                expected_base_url: "https://${connectionConfig.organizationUrl}",
                expected_auth: ExpectedAuth::Basic,
                connection_config: &["organizationUrl"],
            },
            table_name: "projects",
            path: "/_apis/projects",
            response: serde_json::json!({
                "value": [{
                    "id": "11111111-2222-3333-4444-555555555555",
                    "name": "SPUR",
                    "url": "https://dev.azure.com/acme/_apis/projects/SPUR",
                    "state": "wellFormed"
                }]
            }),
            predicates: Vec::new(),
            typed_column: "name",
            typed_value: TypedCell::Utf8("SPUR"),
        },
        ProviderCase {
            provider_name: "clicksend",
            source_name: "clicksend",
            manifest_file: "clicksend.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "BASIC",
                expected_base_url: "https://rest.clicksend.com",
                expected_auth: ExpectedAuth::Basic,
                connection_config: &[],
            },
            table_name: "account",
            path: "/v3/account",
            response: serde_json::json!({
                "data": {
                    "username": "launch",
                    "email": "launch@example.com",
                    "user_id": 428
                },
                "http_code": 200,
                "response_code": "SUCCESS"
            }),
            predicates: Vec::new(),
            typed_column: "email",
            typed_value: TypedCell::Utf8("launch@example.com"),
        },
    ];

    for case in provider_cases {
        let server = MockServer::start().await;
        let manifest_toml = read_supported_manifest(case.manifest_file);
        let mut harness = ProviderManifestHarness::from_toml(case.provider_name, &manifest_toml)
            .unwrap_or_else(|error| panic!("{} manifest parses: {error}", case.provider_name));
        assert_manifest_contract(&case, harness.manifest());
        harness.replace_base_url(case.contract.expected_base_url, &server.uri());
        let _env = harness.install_env();

        assert_eq!(harness.manifest().source.name, case.source_name);
        assert!(
            harness
                .manifest()
                .tables
                .iter()
                .any(|table| table.name == case.table_name),
            "{} should expose {} as a ready table",
            case.provider_name,
            case.table_name
        );

        let mut request = ExpectedRequest::get(case.path)
            .with_manifest_auth(harness.manifest(), case.provider_name);
        for predicate in &case.predicates {
            let param =
                manifest_filter_param(harness.manifest(), case.table_name, &predicate.column);
            request = request.query_param(&param, scalar_value_string(&predicate.value));
        }
        request.respond_json(case.response).mount(&server).await;

        let batches = harness
            .scan(scan_request_with_predicates(
                case.table_name,
                case.predicates.clone(),
            ))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "{} {} scan should succeed: {error}",
                    case.provider_name, case.table_name
                )
            });

        harness.assert_one_typed_row(case.table_name, &batches);
        harness.assert_typed_cell(&batches[0], case.typed_column, 0, case.typed_value);
    }
}

struct ProviderCase {
    provider_name: &'static str,
    source_name: &'static str,
    manifest_file: &'static str,
    contract: ProviderContract,
    table_name: &'static str,
    path: &'static str,
    response: serde_json::Value,
    predicates: Vec<Predicate>,
    typed_column: &'static str,
    typed_value: TypedCell<'static>,
}

#[derive(Clone, Copy)]
struct ProviderContract {
    nango_auth_mode: &'static str,
    expected_base_url: &'static str,
    expected_auth: ExpectedAuth,
    connection_config: &'static [&'static str],
}

#[derive(Clone, Copy, Debug)]
enum ExpectedAuth {
    Bearer,
    Basic,
}

fn assert_manifest_contract(case: &ProviderCase, manifest: &Manifest) {
    assert_eq!(
        manifest.source.base_url, case.contract.expected_base_url,
        "{} base URL should match local Nango/API.guru grounding",
        case.provider_name
    );
    assert_eq!(
        manifest.source.connection_config, case.contract.connection_config,
        "{} connection_config should match local Nango metadata",
        case.provider_name
    );
    match (case.contract.expected_auth, &manifest.source.auth) {
        (ExpectedAuth::Bearer, AuthCfg::Bearer { .. }) => {}
        (ExpectedAuth::Basic, AuthCfg::Basic { .. }) => {}
        _ => panic!(
            "{} should map Nango {} auth to {:?}",
            case.provider_name, case.contract.nango_auth_mode, case.contract.expected_auth
        ),
    }
    let table = manifest
        .tables
        .iter()
        .find(|table| table.name == case.table_name)
        .unwrap_or_else(|| {
            panic!(
                "{} should expose {} as a ready table",
                case.provider_name, case.table_name
            )
        });
    assert_eq!(
        table.path, case.path,
        "{} {} path should match local Nango/API.guru grounding",
        case.provider_name, case.table_name
    );
}

fn read_supported_manifest(file_name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("connections")
        .join("supported")
        .join(file_name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} should be committed: {error}", path.display()))
}

fn manifest_filter_param(
    manifest: &spur_rest_table_gateway::adapter::manifest::Manifest,
    table_name: &str,
    column: &str,
) -> String {
    manifest
        .tables
        .iter()
        .find(|table| table.name == table_name)
        .and_then(|table| table.filters.get(column))
        .map(|filter| filter.param.clone())
        .unwrap_or_else(|| panic!("{table_name} should map {column} to a query parameter"))
}

fn scalar_value_string(value: &ScalarValue) -> String {
    match value {
        ScalarValue::Utf8(value) => value.clone(),
        ScalarValue::Int64(value) => value.to_string(),
        ScalarValue::Float64(value) => value.to_string(),
        ScalarValue::Bool(value) => value.to_string(),
    }
}

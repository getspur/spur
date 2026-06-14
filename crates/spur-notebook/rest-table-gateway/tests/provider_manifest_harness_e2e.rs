#[path = "support/provider_manifest_harness.rs"]
mod provider_manifest_harness;

use wiremock::MockServer;

use provider_manifest_harness::{
    action_request, connection_config_value, mount_oauth_refresh, scan_request,
    scan_request_with_predicates, ExpectedRequest, ProviderManifestHarness, TypedCell,
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

#[tokio::test]
async fn provider_manifest_harness_scans_visible_oauth_promotion_batch() {
    let provider_cases = [
        ProviderCase {
            provider_name: "asana",
            source_name: "asana",
            manifest_file: "asana.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://app.asana.com/api/1.0",
                expected_auth: ExpectedAuth::Bearer,
                connection_config: &[],
            },
            table_name: "workspaces",
            path: "/workspaces",
            response: serde_json::json!({
                "data": [{
                    "gid": "1200000000000001",
                    "resource_type": "workspace",
                    "name": "SPUR Workspace"
                }]
            }),
            predicates: Vec::new(),
            typed_column: "name",
            typed_value: TypedCell::Utf8("SPUR Workspace"),
        },
        ProviderCase {
            provider_name: "bitbucket",
            source_name: "bitbucket",
            manifest_file: "bitbucket.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://api.bitbucket.org",
                expected_auth: ExpectedAuth::Oauth2Refresh,
                connection_config: &[],
            },
            table_name: "repositories",
            path: "/repositories",
            response: serde_json::json!({
                "values": [{
                    "name": "spur"
                }]
            }),
            predicates: Vec::new(),
            typed_column: "name",
            typed_value: TypedCell::Utf8("spur"),
        },
        ProviderCase {
            provider_name: "box",
            source_name: "box",
            manifest_file: "box.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://api.box.com",
                expected_auth: ExpectedAuth::Oauth2Refresh,
                connection_config: &[],
            },
            table_name: "get_events",
            path: "/events",
            response: serde_json::json!({
                "entries": [{
                    "event_id": "evt_1"
                }]
            }),
            predicates: Vec::new(),
            typed_column: "event_id",
            typed_value: TypedCell::Utf8("evt_1"),
        },
        ProviderCase {
            provider_name: "digitalocean",
            source_name: "digitalocean",
            manifest_file: "digitalocean.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://api.digitalocean.com",
                expected_auth: ExpectedAuth::Oauth2Refresh,
                connection_config: &[],
            },
            table_name: "apps_list_tiers",
            path: "/v2/apps/tiers",
            response: serde_json::json!({
                "tiers": [{
                    "name": "Basic"
                }]
            }),
            predicates: Vec::new(),
            typed_column: "name",
            typed_value: TypedCell::Utf8("Basic"),
        },
        ProviderCase {
            provider_name: "instagram",
            source_name: "instagram",
            manifest_file: "instagram.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://graph.instagram.com",
                expected_auth: ExpectedAuth::Oauth2Refresh,
                connection_config: &[],
            },
            table_name: "locations_search",
            path: "/locations/search",
            response: serde_json::json!({
                "data": [{
                    "name": "SPUR Place"
                }]
            }),
            predicates: Vec::new(),
            typed_column: "name",
            typed_value: TypedCell::Utf8("SPUR Place"),
        },
        ProviderCase {
            provider_name: "slack",
            source_name: "slack",
            manifest_file: "slack.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://slack.com/api",
                expected_auth: ExpectedAuth::Bearer,
                connection_config: &[],
            },
            table_name: "users",
            path: "/users.list",
            response: serde_json::json!({
                "ok": true,
                "cache_ts": 1710000000,
                "members": [{
                    "id": "U123",
                    "team_id": "T123",
                    "name": "kevin",
                    "real_name": "Kevin",
                    "is_bot": false,
                    "deleted": false
                }]
            }),
            predicates: Vec::new(),
            typed_column: "real_name",
            typed_value: TypedCell::Utf8("Kevin"),
        },
        ProviderCase {
            provider_name: "jira",
            source_name: "jira",
            manifest_file: "jira.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url:
                    "https://api.atlassian.com/ex/jira/${connectionConfig.cloudId}/rest/api/3",
                expected_auth: ExpectedAuth::Bearer,
                connection_config: &["cloudId"],
            },
            table_name: "projects",
            path: "/project/search",
            response: serde_json::json!({
                "values": [{
                    "id": "10000",
                    "key": "SPUR",
                    "name": "SPUR",
                    "projectTypeKey": "software",
                    "simplified": true
                }],
                "total": 1,
                "isLast": true
            }),
            predicates: Vec::new(),
            typed_column: "key",
            typed_value: TypedCell::Utf8("SPUR"),
        },
        ProviderCase {
            provider_name: "notion",
            source_name: "notion",
            manifest_file: "notion.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://api.notion.com",
                expected_auth: ExpectedAuth::Bearer,
                connection_config: &[],
            },
            table_name: "comments",
            path: "/v1/comments",
            response: serde_json::json!({
                "object": "list",
                "type": "comment",
                "results": [{
                    "object": "comment",
                    "id": "ed4c62f2-c0ad-4081-b6b8-dad025637741",
                    "discussion_id": "ce18f8c6-ef2a-427f-b416-43531fc7c117",
                    "created_time": "2022-07-15T21:38:00.000Z",
                    "last_edited_time": "2022-07-15T21:38:00.000Z",
                    "created_by": {
                        "object": "user",
                        "id": "952f41bb-da96-4d36-9c2e-74924eee8ef1"
                    }
                }]
            }),
            predicates: vec![Predicate {
                column: "block_id".to_string(),
                op: PredicateOp::Eq,
                value: ScalarValue::Utf8("5d4ca33c-d6b7-4675-93d9-84b70af45d1c".to_string()),
            }],
            typed_column: "discussion_id",
            typed_value: TypedCell::Utf8("ce18f8c6-ef2a-427f-b416-43531fc7c117"),
        },
        ProviderCase {
            provider_name: "spotify",
            source_name: "spotify",
            manifest_file: "spotify.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://api.spotify.com",
                expected_auth: ExpectedAuth::Oauth2Refresh,
                connection_config: &[],
            },
            table_name: "get_multiple_albums",
            path: "/albums",
            response: serde_json::json!({
                "albums": [{
                    "name": "SPUR Album"
                }]
            }),
            predicates: Vec::new(),
            typed_column: "name",
            typed_value: TypedCell::Utf8("SPUR Album"),
        },
        ProviderCase {
            provider_name: "squareup",
            source_name: "squareup",
            manifest_file: "squareup.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://connect.squareup.com",
                expected_auth: ExpectedAuth::Oauth2Refresh,
                connection_config: &[],
            },
            table_name: "listemployeeroles",
            path: "/v1/me/roles",
            response: serde_json::json!([{
                "name": "SPUR Role"
            }]),
            predicates: Vec::new(),
            typed_column: "name",
            typed_value: TypedCell::Utf8("SPUR Role"),
        },
        ProviderCase {
            provider_name: "twitter-v2",
            source_name: "twitter-v2",
            manifest_file: "twitter_v2.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://api.twitter.com",
                expected_auth: ExpectedAuth::Oauth2Refresh,
                connection_config: &[],
            },
            table_name: "listbatchcompliancejobs",
            path: "/2/compliance/jobs",
            response: serde_json::json!({
                "data": [{
                    "name": "SPUR Compliance"
                }]
            }),
            predicates: Vec::new(),
            typed_column: "name",
            typed_value: TypedCell::Utf8("SPUR Compliance"),
        },
        ProviderCase {
            provider_name: "vimeo",
            source_name: "vimeo",
            manifest_file: "vimeo.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://api.vimeo.com",
                expected_auth: ExpectedAuth::Oauth2Refresh,
                connection_config: &[],
            },
            table_name: "get_albums_alt1",
            path: "/me/albums",
            response: serde_json::json!([{
                "name": "SPUR Album"
            }]),
            predicates: Vec::new(),
            typed_column: "name",
            typed_value: TypedCell::Utf8("SPUR Album"),
        },
        ProviderCase {
            provider_name: "zoom",
            source_name: "zoom",
            manifest_file: "zoom.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "OAUTH2",
                expected_base_url: "https://api.zoom.us/v2",
                expected_auth: ExpectedAuth::Oauth2Refresh,
                connection_config: &[],
            },
            table_name: "groups",
            path: "/groups",
            response: serde_json::json!({
                "groups": [{
                    "name": "SPUR Group"
                }]
            }),
            predicates: Vec::new(),
            typed_column: "name",
            typed_value: TypedCell::Utf8("SPUR Group"),
        },
        ProviderCase {
            provider_name: "autotask",
            source_name: "autotask",
            manifest_file: "autotask.connection.toml",
            contract: ProviderContract {
                nango_auth_mode: "API_KEY",
                expected_base_url:
                    "https://${connectionConfig.subdomain}.autotask.net/atservicesrest",
                expected_auth: ExpectedAuth::Header,
                connection_config: &["subdomain", "apiIntegrationCode", "username"],
            },
            table_name: "companies",
            path: "/V1.0/Companies/query",
            response: serde_json::json!({
                "items": [{
                    "id": 1001,
                    "companyName": "SPUR",
                    "companyNumber": "ACME-001",
                    "isActive": true,
                    "webAddress": "https://example.com"
                }],
                "pageDetails": { "count": 1 }
            }),
            predicates: vec![Predicate {
                column: "search".to_string(),
                op: PredicateOp::Eq,
                value: ScalarValue::Utf8("{\"filter\":[]}".to_string()),
            }],
            typed_column: "company_name",
            typed_value: TypedCell::Utf8("SPUR"),
        },
    ];

    for case in provider_cases {
        let server = MockServer::start().await;
        let manifest_toml = read_supported_manifest(case.manifest_file);
        let mut harness = ProviderManifestHarness::from_toml(case.provider_name, &manifest_toml)
            .unwrap_or_else(|error| panic!("{} manifest parses: {error}", case.provider_name));
        assert_manifest_contract(&case, harness.manifest());
        harness.replace_base_url(case.contract.expected_base_url, &server.uri());
        let oauth_access_token = "minted-access-token";
        let uses_oauth_refresh = matches!(case.contract.expected_auth, ExpectedAuth::Oauth2Refresh);
        if uses_oauth_refresh {
            harness.replace_oauth_token_url(
                expected_oauth_token_url(case.provider_name),
                &format!("{}/oauth/token", server.uri()),
            );
            mount_oauth_refresh(&server, "/oauth/token", oauth_access_token).await;
        }
        let _env = harness.install_env();

        assert_eq!(harness.manifest().source.name, case.source_name);

        let mut request = ExpectedRequest::get(case.path);
        if uses_oauth_refresh {
            request =
                request.with_oauth_bearer(&harness.manifest().source.auth, oauth_access_token);
        } else {
            request = request.with_manifest_auth(harness.manifest(), case.provider_name);
        }
        for (name, value) in provider_static_header_expectations(&case) {
            request = request.header(name, value);
        }
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
    Header,
    Oauth2Refresh,
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
        (ExpectedAuth::Header, AuthCfg::Header { .. }) => {}
        (ExpectedAuth::Oauth2Refresh, AuthCfg::Oauth2Refresh { .. }) => {}
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

fn expected_oauth_token_url(provider_name: &str) -> &'static str {
    match provider_name {
        "bitbucket" => "https://bitbucket.org/site/oauth2/access_token",
        "box" => "https://api.box.com/oauth2/token",
        "digitalocean" => "https://cloud.digitalocean.com/v1/oauth/token",
        "instagram" => "https://api.instagram.com/oauth/access_token",
        "spotify" => "https://accounts.spotify.com/api/token",
        "squareup" => "https://connect.squareup.com/oauth2/token",
        "twitter-v2" => "https://api.twitter.com/2/oauth2/token",
        "vimeo" => "https://api.vimeo.com/oauth/access_token",
        "zoom" => "https://zoom.us/oauth/token",
        _ => panic!("{provider_name} should not use oauth2_refresh auth"),
    }
}

fn provider_static_header_expectations(case: &ProviderCase) -> Vec<(&'static str, String)> {
    match case.provider_name {
        "notion" => vec![("notion-version", "2022-06-28".to_string())],
        "autotask" => vec![
            (
                "apiintegrationcode",
                connection_config_value(case.provider_name, "apiIntegrationCode"),
            ),
            (
                "username",
                connection_config_value(case.provider_name, "username"),
            ),
        ],
        _ => Vec::new(),
    }
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

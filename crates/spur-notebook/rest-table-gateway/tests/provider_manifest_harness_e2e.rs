#[path = "support/provider_manifest_harness.rs"]
mod provider_manifest_harness;

use wiremock::MockServer;

use provider_manifest_harness::{
    action_request, mount_oauth_refresh, scan_request, ExpectedRequest, ProviderManifestHarness,
    TypedCell,
};

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

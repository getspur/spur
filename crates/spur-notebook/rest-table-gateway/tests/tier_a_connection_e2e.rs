use std::collections::BTreeSet;

#[path = "support/provider_manifest_harness.rs"]
mod provider_manifest_harness;

use spur_rest_table_gateway::adapter::manifest::Manifest;
use wiremock::MockServer;

use provider_manifest_harness::{
    first_scannable_table, response_body_for_table, scan_request, ExpectedRequest,
    ProviderManifestHarness,
};

const SUPPORTED_TIER_A_TABLE_PROVIDERS: &[(&str, &str)] = &[
    (
        "datadog",
        include_str!("../connections/supported/datadog.connection.toml"),
    ),
    (
        "elevenlabs",
        include_str!("../connections/supported/elevenlabs.connection.toml"),
    ),
    (
        "jira_basic",
        include_str!("../connections/supported/jira_basic.connection.toml"),
    ),
    (
        "mailchimp",
        include_str!("../connections/supported/mailchimp.connection.toml"),
    ),
    (
        "openai",
        include_str!("../connections/supported/openai.connection.toml"),
    ),
    (
        "sendgrid",
        include_str!("../connections/supported/sendgrid.connection.toml"),
    ),
    (
        "square",
        include_str!("../connections/supported/square.connection.toml"),
    ),
    (
        "stripe",
        include_str!("../connections/supported/stripe.connection.toml"),
    ),
    (
        "stripe_api_key",
        include_str!("../connections/supported/stripe_api_key.connection.toml"),
    ),
    (
        "twilio",
        include_str!("../connections/supported/twilio.connection.toml"),
    ),
    (
        "vercel",
        include_str!("../connections/supported/vercel.connection.toml"),
    ),
    (
        "zendesk",
        include_str!("../connections/supported/zendesk.connection.toml"),
    ),
];

#[tokio::test]
async fn generated_tier_a_supported_manifests_scan_one_table_each() {
    let provider_names = SUPPORTED_TIER_A_TABLE_PROVIDERS
        .iter()
        .map(|(name, _)| *name)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        provider_names,
        BTreeSet::from([
            "datadog",
            "elevenlabs",
            "jira_basic",
            "mailchimp",
            "openai",
            "sendgrid",
            "square",
            "stripe",
            "stripe_api_key",
            "twilio",
            "vercel",
            "zendesk",
        ])
    );

    for (provider_name, manifest_toml) in SUPPORTED_TIER_A_TABLE_PROVIDERS {
        let manifest = Manifest::from_toml(manifest_toml)
            .unwrap_or_else(|error| panic!("{provider_name} manifest should parse: {error}"));
        assert_eq!(manifest.source.name.replace('-', "_"), *provider_name);
        assert!(
            !manifest.tables.is_empty(),
            "{provider_name} should expose table scans"
        );

        let table = first_scannable_table(&manifest)
            .unwrap_or_else(|| panic!("{provider_name} should have a path-param-free table"));

        let server = MockServer::start().await;
        let mut harness = ProviderManifestHarness::new(*provider_name, manifest);
        harness.set_base_url(server.uri());
        let _env = harness.install_env();

        ExpectedRequest::get(table.path.as_str())
            .with_manifest_auth(harness.manifest(), provider_name)
            .respond_json(response_body_for_table(&table))
            .mount(&server)
            .await;

        let batches = harness
            .scan(scan_request(&table.name))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "{provider_name} {} scan should succeed: {error}",
                    table.name
                )
            });

        harness.assert_one_typed_row(&table.name, &batches);
    }
}

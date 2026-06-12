use std::collections::BTreeSet;

use arrow_array::RecordBatch;
use indexmap::IndexMap;
use serde_json::{Map, Number, Value};
use spur_rest_table_gateway::adapter::manifest::{AuthCfg, ColumnCfg, Manifest, TableCfg};
use spur_rest_table_gateway::adapter::manifest_adapter::ManifestAdapter;
use spur_rest_table_gateway::adapter::{Adapter, ResolvedAuth, ScanRequest};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SUPPORTED_TIER_A_TABLE_PROVIDERS: &[(&str, &str)] = &[
    (
        "datadog",
        include_str!("../connections/supported/datadog.connection.toml"),
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
        "twilio",
        include_str!("../connections/supported/twilio.connection.toml"),
    ),
    (
        "zendesk",
        include_str!("../connections/supported/zendesk.connection.toml"),
    ),
];

struct EnvGuard {
    key: String,
    previous: Option<String>,
}

impl EnvGuard {
    fn set(key: impl Into<String>, value: &str) -> Self {
        let key = key.into();
        let previous = std::env::var(&key).ok();
        std::env::set_var(&key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(&self.key, previous);
        } else {
            std::env::remove_var(&self.key);
        }
    }
}

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
            "mailchimp",
            "openai",
            "sendgrid",
            "square",
            "stripe",
            "twilio",
            "zendesk",
        ])
    );

    for (provider_name, manifest_toml) in SUPPORTED_TIER_A_TABLE_PROVIDERS {
        let mut manifest = Manifest::from_toml(manifest_toml)
            .unwrap_or_else(|error| panic!("{provider_name} manifest should parse: {error}"));
        assert_eq!(manifest.source.name, *provider_name);
        assert!(
            !manifest.tables.is_empty(),
            "{provider_name} should expose table scans"
        );

        let table = first_scannable_table(&manifest)
            .unwrap_or_else(|| panic!("{provider_name} should have a path-param-free table"));
        let response = response_body_for(&table);

        let server = MockServer::start().await;
        let _env = install_auth_env(&manifest.source.auth, provider_name);
        let _config = install_connection_config_env(
            &manifest.source.name,
            &manifest.source.connection_config,
            provider_name,
        );
        let mock = Mock::given(method("GET")).and(path(table.path.as_str()));
        let mock = match provider_name.as_ref() {
            "twilio" | "zendesk" => mock.and(header(
                "authorization",
                basic_auth_header(
                    &format!("{provider_name}_user"),
                    &format!("{provider_name}_pass"),
                ),
            )),
            _ => match &manifest.source.auth {
                AuthCfg::Bearer { .. } => mock.and(header(
                    "authorization",
                    format!("Bearer {provider_name}_token"),
                )),
                AuthCfg::Header { name, .. } => {
                    mock.and(header(name.as_str(), format!("{provider_name}_token")))
                }
                _ => mock,
            },
        };
        mock.respond_with(ResponseTemplate::new(200).set_body_json(response))
            .mount(&server)
            .await;

        manifest.source.base_url = server.uri();
        let adapter = ManifestAdapter::new(manifest);
        let batches = adapter
            .scan(scan_request(&table.name))
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "{provider_name} {} scan should succeed: {error}",
                    table.name
                )
            });

        assert_one_row(provider_name, &table.name, &batches);
    }
}

fn first_scannable_table(manifest: &Manifest) -> Option<TableCfg> {
    manifest
        .tables
        .iter()
        .find(|table| !table.path.contains('{') && !table.columns.is_empty())
        .cloned()
}

fn install_auth_env(auth: &AuthCfg, provider_name: &str) -> Vec<EnvGuard> {
    match auth {
        AuthCfg::None => Vec::new(),
        AuthCfg::Bearer { env } => vec![EnvGuard::set(env, &format!("{provider_name}_token"))],
        AuthCfg::Header { env, .. } => vec![EnvGuard::set(env, &format!("{provider_name}_token"))],
        AuthCfg::Basic { user_env, pass_env } => vec![
            EnvGuard::set(user_env, &format!("{provider_name}_user")),
            EnvGuard::set(pass_env, &format!("{provider_name}_pass")),
        ],
        AuthCfg::ApiKeyQuery { env, .. } => {
            vec![EnvGuard::set(env, &format!("{provider_name}_token"))]
        }
        AuthCfg::Oauth2Refresh { .. } => {
            panic!("{provider_name} should not require OAuth refresh in Tier A table E2E")
        }
    }
}

fn install_connection_config_env(
    source_name: &str,
    keys: &[String],
    provider_name: &str,
) -> Vec<EnvGuard> {
    keys.iter()
        .map(|key| {
            EnvGuard::set(
                format!("SPUR_CONN_{key}"),
                &format!("{provider_name}_{key}_value"),
            )
        })
        .chain(keys.iter().map(|key| {
            EnvGuard::set(
                format!("SPUR_CONN_{source_name}_{key}"),
                &format!("{provider_name}_{key}_value"),
            )
        }))
        .collect()
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

fn response_body_for(table: &TableCfg) -> Value {
    let row = row_for_columns(&table.columns);
    match table.response_path.as_deref() {
        Some("$.data") => object_with_array("data", row),
        Some("$.items") => object_with_array("items", row),
        Some("$.logs") => object_with_array("logs", row),
        Some("$.keys") => object_with_array("keys", row),
        Some("$.accounts") => object_with_array("accounts", row),
        Some("$.api_keys") => object_with_array("api_keys", row),
        Some("$.application_keys") => object_with_array("application_keys", row),
        Some(path) => {
            let key = path
                .strip_prefix("$.")
                .unwrap_or_else(|| panic!("unsupported response path in {}: {path}", table.name));
            object_with_array(key, row)
        }
        None => Value::Array(vec![row]),
    }
}

fn object_with_array(key: &str, row: Value) -> Value {
    Value::Object(Map::from_iter([(key.to_string(), Value::Array(vec![row]))]))
}

fn row_for_columns(columns: &IndexMap<String, ColumnCfg>) -> Value {
    let mut row = Map::new();
    for column in columns.values() {
        insert_json_path(&mut row, &column.json, value_for_type(&column.ty));
    }
    Value::Object(row)
}

fn value_for_type(ty: &str) -> Value {
    match ty {
        "Boolean" => Value::Bool(true),
        "Float64" => Value::Number(Number::from_f64(42.5).expect("finite float")),
        "Int64" => Value::Number(Number::from(42)),
        _ => Value::String("value".to_string()),
    }
}

fn basic_auth_header(user: &str, pass: &str) -> String {
    format!(
        "Basic {}",
        base64_encode(format!("{user}:{pass}").as_bytes())
    )
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0b0000_0011) << 4) | (b1 >> 4)) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(((b1 & 0b0000_1111) << 2) | (b2 >> 6)) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(b2 & 0b0011_1111) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

fn insert_json_path(row: &mut Map<String, Value>, path: &str, value: Value) {
    if path == "$" {
        return;
    }
    let mut parts = path
        .strip_prefix("$.")
        .unwrap_or_else(|| panic!("unsupported column JSON path: {path}"))
        .split('.')
        .peekable();
    let mut current = row;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            current.insert(part.to_string(), value);
            return;
        }
        current = current
            .entry(part.to_string())
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()
            .expect("intermediate JSON path node should be an object");
    }
}

fn assert_one_row(provider: &str, table: &str, batches: &[RecordBatch]) {
    assert_eq!(
        batches.len(),
        1,
        "{provider} {table} should return one batch"
    );
    assert_eq!(
        batches[0].num_rows(),
        1,
        "{provider} {table} should return one typed row"
    );
}

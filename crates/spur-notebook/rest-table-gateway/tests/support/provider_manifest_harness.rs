#![allow(dead_code)]

use arrow_array::{BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use indexmap::IndexMap;
use serde_json::{Map, Number, Value};
use spur_rest_table_gateway::adapter::manifest::{
    ActionCfg, AuthCfg, ColumnCfg, Manifest, TableCfg,
};
use spur_rest_table_gateway::adapter::manifest_adapter::ManifestAdapter;
use spur_rest_table_gateway::adapter::{
    ActionRequest, Adapter, Predicate, ResolvedAuth, ScanRequest,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockBuilder, MockServer, ResponseTemplate};

pub struct EnvGuard {
    key: String,
    previous: Option<String>,
}

impl EnvGuard {
    pub fn set(key: impl Into<String>, value: impl AsRef<str>) -> Self {
        let key = key.into();
        let previous = std::env::var(&key).ok();
        std::env::set_var(&key, value.as_ref());
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

pub struct ProviderManifestHarness {
    provider_name: String,
    manifest: Manifest,
}

impl ProviderManifestHarness {
    pub fn from_toml(provider_name: impl Into<String>, toml: &str) -> anyhow::Result<Self> {
        let provider_name = provider_name.into();
        let manifest = Manifest::from_toml(toml)?;
        Ok(Self::new(provider_name, manifest))
    }

    pub fn new(provider_name: impl Into<String>, manifest: Manifest) -> Self {
        Self {
            provider_name: provider_name.into(),
            manifest,
        }
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn set_base_url(&mut self, base_url: impl Into<String>) {
        self.manifest.source.base_url = base_url.into();
    }

    pub fn replace_base_url(&mut self, expected: &str, replacement: &str) {
        assert_eq!(
            self.manifest.source.base_url, expected,
            "{} base_url fixture changed",
            self.provider_name
        );
        self.manifest.source.base_url = replacement.to_string();
    }

    pub fn install_env(&self) -> Vec<EnvGuard> {
        install_manifest_env(&self.manifest, &self.provider_name)
    }

    pub async fn scan(
        &self,
        req: ScanRequest,
    ) -> spur_rest_table_gateway::error::Result<Vec<RecordBatch>> {
        ManifestAdapter::new(self.manifest.clone()).scan(req).await
    }

    pub async fn act(
        &self,
        req: ActionRequest,
    ) -> spur_rest_table_gateway::error::Result<Vec<RecordBatch>> {
        ManifestAdapter::new(self.manifest.clone()).act(req).await
    }

    pub fn assert_one_typed_row(&self, table: &str, batches: &[RecordBatch]) {
        self.assert_typed_rows(table, batches, 1);
    }

    pub fn assert_typed_rows(&self, table: &str, batches: &[RecordBatch], expected_rows: usize) {
        assert_eq!(
            batches.len(),
            1,
            "{} {table} should return one batch",
            self.provider_name
        );
        assert_eq!(
            batches[0].num_rows(),
            expected_rows,
            "{} {table} should return {expected_rows} typed row(s)",
            self.provider_name
        );
        assert!(
            batches[0]
                .schema()
                .column_with_name("http_status")
                .is_none(),
            "{} {table} should expose typed provider columns",
            self.provider_name
        );
    }

    pub fn assert_typed_cell(
        &self,
        batch: &RecordBatch,
        column: &str,
        row: usize,
        expected: TypedCell<'_>,
    ) {
        assert_typed_cell(batch, column, row, expected);
    }
}

pub struct ExpectedRequest {
    builder: MockBuilder,
}

impl ExpectedRequest {
    pub fn get(path_value: &str) -> Self {
        Self::new("GET", path_value)
    }

    pub fn post(path_value: &str) -> Self {
        Self::new("POST", path_value)
    }

    pub fn new(method_value: &str, path_value: &str) -> Self {
        Self {
            builder: Mock::given(method(method_value)).and(path(path_value.to_string())),
        }
    }

    pub fn with_manifest_auth(self, manifest: &Manifest, provider_name: &str) -> Self {
        self.with_auth(&manifest.source.auth, provider_name)
    }

    pub fn with_auth(mut self, auth: &AuthCfg, provider_name: &str) -> Self {
        self.builder = apply_auth_expectation(self.builder, auth, provider_name, None);
        self
    }

    pub fn with_oauth_bearer(mut self, auth: &AuthCfg, access_token: &str) -> Self {
        self.builder = apply_auth_expectation(self.builder, auth, "", Some(access_token));
        self
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.builder = self.builder.and(header(name.to_string(), value.into()));
        self
    }

    pub fn query_param(mut self, name: &str, value: impl Into<String>) -> Self {
        self.builder = self
            .builder
            .and(query_param(name.to_string(), value.into()));
        self
    }

    pub fn respond_json(self, body: Value) -> PendingMock {
        PendingMock {
            builder: self
                .builder
                .respond_with(ResponseTemplate::new(200).set_body_json(body)),
        }
    }
}

pub struct PendingMock {
    builder: Mock,
}

impl PendingMock {
    pub async fn mount(self, server: &MockServer) {
        self.builder.mount(server).await;
    }
}

#[derive(Clone, Debug)]
pub enum TypedCell<'a> {
    Utf8(&'a str),
    Int64(i64),
    Float64(f64),
    Boolean(bool),
}

pub fn scan_request(table: &str) -> ScanRequest {
    scan_request_with_predicates(table, Vec::new())
}

pub fn scan_request_with_predicates(table: &str, predicates: Vec<Predicate>) -> ScanRequest {
    ScanRequest {
        table: table.to_string(),
        predicates,
        projection: None,
        tvf_args: Vec::new(),
        auth: ResolvedAuth::None,
    }
}

pub fn action_request(
    name: &str,
    method: &str,
    path: &str,
    query: Vec<(String, String)>,
    body: Option<Value>,
) -> ActionRequest {
    ActionRequest {
        name: name.to_string(),
        method: method.to_string(),
        path: path.to_string(),
        query,
        body,
        idempotency_key: None,
        dry_run: false,
    }
}

pub fn first_scannable_table(manifest: &Manifest) -> Option<TableCfg> {
    manifest
        .tables
        .iter()
        .find(|table| !table.path.contains('{') && !table.columns.is_empty())
        .cloned()
}

pub fn response_body_for_table(table: &TableCfg) -> Value {
    response_body_for_columns(&table.columns, table.response_path.as_deref(), &table.name)
}

pub fn response_body_for_action(action: &ActionCfg) -> Value {
    let Some(columns) = &action.columns else {
        return serde_json::json!({ "ok": true });
    };
    response_body_for_columns(columns, action.response_path.as_deref(), &action.name)
}

pub async fn mount_oauth_refresh(server: &MockServer, path_value: &str, access_token: &str) {
    ExpectedRequest::post(path_value)
        .respond_json(serde_json::json!({
            "access_token": access_token,
            "expires_in": 3600
        }))
        .mount(server)
        .await;
}

pub fn install_manifest_env(manifest: &Manifest, provider_name: &str) -> Vec<EnvGuard> {
    install_auth_env(&manifest.source.auth, provider_name)
        .into_iter()
        .chain(install_connection_config_env(
            &manifest.source.name,
            &manifest.source.connection_config,
            provider_name,
        ))
        .collect()
}

pub fn install_auth_env(auth: &AuthCfg, provider_name: &str) -> Vec<EnvGuard> {
    match auth {
        AuthCfg::None => Vec::new(),
        AuthCfg::Bearer { env } => vec![EnvGuard::set(env, token_value(provider_name))],
        AuthCfg::Header { env, .. } => vec![EnvGuard::set(env, token_value(provider_name))],
        AuthCfg::Basic { user_env, pass_env } => vec![
            EnvGuard::set(user_env, basic_user_value(provider_name)),
            EnvGuard::set(pass_env, basic_pass_value(provider_name)),
        ],
        AuthCfg::ApiKeyQuery { env, .. } => vec![EnvGuard::set(env, token_value(provider_name))],
        AuthCfg::Oauth2Refresh {
            client_id_env,
            client_secret_env,
            refresh_token_env,
            ..
        } => vec![
            EnvGuard::set(client_id_env, format!("{provider_name}_client_id")),
            EnvGuard::set(client_secret_env, format!("{provider_name}_client_secret")),
            EnvGuard::set(refresh_token_env, format!("{provider_name}_refresh_token")),
        ],
    }
}

pub fn install_connection_config_env(
    source_name: &str,
    keys: &[String],
    provider_name: &str,
) -> Vec<EnvGuard> {
    keys.iter()
        .flat_map(|key| {
            [
                EnvGuard::set(
                    format!("SPUR_CONN_{key}"),
                    connection_config_value(provider_name, key),
                ),
                EnvGuard::set(
                    format!("SPUR_CONN_{source_name}_{key}"),
                    connection_config_value(provider_name, key),
                ),
            ]
        })
        .collect()
}

pub fn token_value(provider_name: &str) -> String {
    format!("{provider_name}_token")
}

pub fn connection_config_value(provider_name: &str, key: &str) -> String {
    format!("{provider_name}_{key}_value")
}

fn basic_user_value(provider_name: &str) -> String {
    format!("{provider_name}_user")
}

fn basic_pass_value(provider_name: &str) -> String {
    format!("{provider_name}_pass")
}

fn apply_auth_expectation(
    builder: MockBuilder,
    auth: &AuthCfg,
    provider_name: &str,
    oauth_access_token: Option<&str>,
) -> MockBuilder {
    match auth {
        AuthCfg::None => builder,
        AuthCfg::Bearer { .. } => builder.and(header(
            "authorization",
            format!("Bearer {}", token_value(provider_name)),
        )),
        AuthCfg::Header { name, .. } => {
            builder.and(header(name.to_string(), token_value(provider_name)))
        }
        AuthCfg::Basic { .. } => builder.and(header(
            "authorization",
            basic_auth_header(
                &basic_user_value(provider_name),
                &basic_pass_value(provider_name),
            ),
        )),
        AuthCfg::ApiKeyQuery { param, .. } => {
            builder.and(query_param(param.to_string(), token_value(provider_name)))
        }
        AuthCfg::Oauth2Refresh { .. } => builder.and(header(
            "authorization",
            format!(
                "Bearer {}",
                oauth_access_token.expect("OAuth refresh auth needs expected access token")
            ),
        )),
    }
}

fn response_body_for_columns(
    columns: &IndexMap<String, ColumnCfg>,
    response_path: Option<&str>,
    name: &str,
) -> Value {
    let row = row_for_columns(columns);
    match response_path {
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
                .unwrap_or_else(|| panic!("unsupported response path in {name}: {path}"));
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

fn assert_typed_cell(batch: &RecordBatch, column: &str, row: usize, expected: TypedCell<'_>) {
    let schema = batch.schema();
    let (column_index, field) = schema
        .column_with_name(column)
        .unwrap_or_else(|| panic!("missing typed column {column}"));
    match expected {
        TypedCell::Utf8(expected) => {
            let array = batch
                .column(column_index)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap_or_else(|| panic!("{column} should be Utf8, got {:?}", field.data_type()));
            assert_eq!(array.value(row), expected, "value for {column}[{row}]");
        }
        TypedCell::Int64(expected) => {
            let array = batch
                .column(column_index)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap_or_else(|| panic!("{column} should be Int64, got {:?}", field.data_type()));
            assert_eq!(array.value(row), expected, "value for {column}[{row}]");
        }
        TypedCell::Float64(expected) => {
            let array = batch
                .column(column_index)
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap_or_else(|| {
                    panic!("{column} should be Float64, got {:?}", field.data_type())
                });
            assert_eq!(array.value(row), expected, "value for {column}[{row}]");
        }
        TypedCell::Boolean(expected) => {
            let array = batch
                .column(column_index)
                .as_any()
                .downcast_ref::<BooleanArray>()
                .unwrap_or_else(|| {
                    panic!("{column} should be Boolean, got {:?}", field.data_type())
                });
            assert_eq!(array.value(row), expected, "value for {column}[{row}]");
        }
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

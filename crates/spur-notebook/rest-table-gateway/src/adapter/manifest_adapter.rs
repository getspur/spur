use std::sync::Arc;

use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use async_trait::async_trait;
use indexmap::IndexMap;
use reqwest::Client;
use serde_json::{Map, Number, Value};

use crate::adapter::graphql::{fetch_graphql_rows, GraphqlFetch};
use crate::adapter::http::{fetch_rows, send_request, HttpAction, HttpFetch};
use crate::adapter::json_to_batch::{arrow_type, json_path_get, rows_to_batch, ColumnExtract};
use crate::adapter::manifest::{
    ActionCfg, ArgLocation as ManifestArgLocation, AuthCfg, ColumnCfg, GraphqlTableCfg, Manifest,
    TableCfg, Transport,
};
use crate::adapter::templating::{resolve_template, ConnectionContext};
use crate::adapter::{
    ActionRequest, Adapter, ArgLocation, ArgSpec, Predicate, PredicateOp, ResolvedAuth,
    ScalarValue, ScanRequest, TableDef, TableKind,
};
use crate::error::{GatewayError, Result};

pub struct ManifestAdapter {
    manifest: Manifest,
    client: Client,
}

/// Template each static header value via `resolve_template`, returning name/value
/// pairs ready to attach to a request. `authorization` is reserved (it would
/// shadow the resolved auth header, which `reqwest` appends rather than
/// replaces).
fn resolve_headers(
    headers: &IndexMap<String, String>,
    ctx: &ConnectionContext,
) -> Result<Vec<(String, String)>> {
    let mut out = Vec::with_capacity(headers.len());
    for (name, value) in headers {
        if name.eq_ignore_ascii_case("authorization") {
            return Err(GatewayError::Manifest(
                "static header 'authorization' is reserved; use [source.auth]".to_string(),
            ));
        }
        out.push((name.clone(), resolve_template(value, ctx)?));
    }
    Ok(out)
}

impl ManifestAdapter {
    pub fn new(manifest: Manifest) -> Self {
        Self {
            manifest,
            client: crate::adapter::default_http_client(),
        }
    }

    fn table<'a>(&'a self, name: &str) -> Result<&'a TableCfg> {
        self.manifest
            .tables
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| GatewayError::UnknownTable(name.to_string()))
    }

    async fn resolve_auth(&self) -> Result<ResolvedAuth> {
        Ok(match &self.manifest.source.auth {
            AuthCfg::None => ResolvedAuth::None,
            AuthCfg::Bearer { env } => std::env::var(env)
                .map(ResolvedAuth::Bearer)
                .unwrap_or(ResolvedAuth::None),
            AuthCfg::Header { name, env } => std::env::var(env)
                .map(|value| ResolvedAuth::Header {
                    name: name.clone(),
                    value,
                })
                .unwrap_or(ResolvedAuth::None),
            AuthCfg::Basic { user_env, pass_env } => {
                match (std::env::var(user_env), std::env::var(pass_env)) {
                    (Ok(user), Ok(pass)) => ResolvedAuth::Basic { user, pass },
                    _ => ResolvedAuth::None,
                }
            }
            AuthCfg::ApiKeyQuery { param, env } => std::env::var(env)
                .map(|value| ResolvedAuth::QueryParam {
                    param: param.clone(),
                    value,
                })
                .unwrap_or(ResolvedAuth::None),
            AuthCfg::Oauth2Refresh {
                token_url,
                client_id_env,
                client_secret_env,
                refresh_token_env,
                scope,
            } => {
                let ctx = ConnectionContext::from_env(&self.manifest.source.connection_config);
                let token_url = resolve_template(token_url, &ctx)?;
                let read = |name: &str| {
                    std::env::var(name).map_err(|_| {
                        GatewayError::Auth(format!("missing credential env var {name}"))
                    })
                };
                let client_id = read(client_id_env)?;
                let client_secret = read(client_secret_env)?;
                let refresh_token = read(refresh_token_env)?;
                let grant = crate::adapter::oauth::RefreshGrant {
                    token_url: &token_url,
                    client_id: &client_id,
                    client_secret: &client_secret,
                    refresh_token: &refresh_token,
                    scope: scope.as_deref(),
                };
                let token = crate::adapter::oauth::access_token(&self.client, &grant).await?;
                ResolvedAuth::Bearer(token)
            }
        })
    }

    fn schema_from_columns(columns: &IndexMap<String, ColumnCfg>) -> Result<SchemaRef> {
        let fields = columns
            .iter()
            .map(|(name, column)| Ok(Field::new(name.clone(), arrow_type(&column.ty)?, true)))
            .collect::<Result<Vec<_>>>()?;

        Ok(Arc::new(Schema::new(fields)))
    }

    fn schema_for_table(table: &TableCfg) -> Result<SchemaRef> {
        Self::schema_from_columns(&table.columns)
    }

    fn table_def(table: &TableCfg) -> Result<TableDef> {
        Ok(TableDef {
            name: table.name.clone(),
            schema: Self::schema_for_table(table)?,
            kind: TableKind::Table,
        })
    }

    fn generic_action_schema() -> SchemaRef {
        Arc::new(Schema::new(vec![
            Field::new("http_status", DataType::Int64, false),
            Field::new("body", DataType::Utf8, true),
        ]))
    }

    fn action_response_schema(action: &ActionCfg) -> Result<SchemaRef> {
        match &action.columns {
            Some(columns) => Self::schema_from_columns(columns),
            None => Ok(Self::generic_action_schema()),
        }
    }

    fn arg_location(location: ManifestArgLocation) -> ArgLocation {
        match location {
            ManifestArgLocation::Path => ArgLocation::Path,
            ManifestArgLocation::Body => ArgLocation::Body,
            ManifestArgLocation::Query => ArgLocation::Query,
        }
    }

    fn action_arg_specs(action: &ActionCfg) -> Result<Vec<ArgSpec>> {
        action
            .args
            .iter()
            .map(|(name, cfg)| {
                Ok(ArgSpec {
                    name: name.clone(),
                    location: Self::arg_location(cfg.in_),
                    ty: arrow_type(&cfg.ty)?,
                    required: cfg.required,
                    json_key: cfg.json.clone().unwrap_or_else(|| name.clone()),
                    query_param: cfg.param.clone().unwrap_or_else(|| name.clone()),
                })
            })
            .collect()
    }

    fn action_def(action: &ActionCfg) -> Result<TableDef> {
        const ALLOWED_WRITE_METHODS: &[&str] = &["POST", "PUT", "PATCH", "DELETE"];
        let method = action.method.to_ascii_uppercase();
        if !ALLOWED_WRITE_METHODS.contains(&method.as_str()) {
            return Err(GatewayError::Manifest(format!(
                "action '{}' uses unsupported method '{}' (allowed: POST, PUT, PATCH, DELETE)",
                action.name, action.method
            )));
        }

        Ok(TableDef {
            name: action.name.clone(),
            schema: Self::action_response_schema(action)?,
            kind: TableKind::Action {
                method,
                path: action.path.clone(),
                arg_specs: Self::action_arg_specs(action)?,
                dry_run_arg: action.dry_run_arg.clone(),
                idempotency_header: action.idempotency_header.clone(),
            },
        })
    }

    fn column_extracts_from_columns(
        columns: &IndexMap<String, ColumnCfg>,
    ) -> Result<Vec<ColumnExtract>> {
        columns
            .iter()
            .map(|(name, column)| {
                Ok(ColumnExtract {
                    name: name.clone(),
                    data_type: arrow_type(&column.ty)?,
                    json_path: column.json.clone(),
                })
            })
            .collect()
    }

    fn column_extracts(table: &TableCfg) -> Result<Vec<ColumnExtract>> {
        Self::column_extracts_from_columns(&table.columns)
    }

    fn action_column_extracts(action: &ActionCfg) -> Result<Vec<ColumnExtract>> {
        match &action.columns {
            Some(columns) => Self::column_extracts_from_columns(columns),
            None => Ok(vec![]),
        }
    }

    fn action_rows(body: &Value, response_path: Option<&str>) -> Result<Vec<Value>> {
        let value = match response_path {
            Some(path) => json_path_get(body, path).ok_or_else(|| {
                GatewayError::Http(format!("expected JSON value at {path}, got null"))
            })?,
            None => body,
        };

        Ok(match value {
            Value::Array(rows) => rows.clone(),
            Value::Null => vec![],
            value => vec![value.clone()],
        })
    }

    fn render_generic_row(status: u16, body: Value) -> Result<Vec<RecordBatch>> {
        let body_value = if body.is_null() {
            None
        } else {
            Some(body.to_string())
        };
        let arrays: Vec<ArrayRef> = vec![
            Arc::new(Int64Array::from(vec![i64::from(status)])) as ArrayRef,
            Arc::new(StringArray::from(vec![body_value])) as ArrayRef,
        ];
        let batch = RecordBatch::try_new(Self::generic_action_schema(), arrays)
            .map_err(|e| GatewayError::Schema(e.to_string()))?;
        Ok(vec![batch])
    }

    fn render_typed_dry_run(action: &ActionCfg) -> Result<Vec<RecordBatch>> {
        let schema = Self::action_response_schema(action)?;
        let arrays: Vec<ArrayRef> = schema
            .fields()
            .iter()
            .map(|field| arrow_array::new_null_array(field.data_type(), 1))
            .collect();
        let batch = RecordBatch::try_new(schema, arrays)
            .map_err(|e| GatewayError::Schema(e.to_string()))?;
        Ok(vec![batch])
    }

    fn query_value(value: &ScalarValue) -> String {
        match value {
            ScalarValue::Utf8(value) => value.clone(),
            ScalarValue::Int64(value) => value.to_string(),
            ScalarValue::Float64(value) => value.to_string(),
            ScalarValue::Bool(value) => value.to_string(),
        }
    }

    fn query_params(table: &TableCfg, predicates: &[Predicate]) -> Vec<(String, String)> {
        predicates
            .iter()
            .filter_map(|predicate| {
                if predicate.op != PredicateOp::Eq {
                    return None;
                }

                table
                    .filters
                    .get(&predicate.column)
                    .map(|filter| (filter.param.clone(), Self::query_value(&predicate.value)))
            })
            .collect()
    }

    fn graphql_value(value: &ScalarValue) -> Result<Value> {
        Ok(match value {
            ScalarValue::Utf8(value) => Value::String(value.clone()),
            ScalarValue::Int64(value) => Value::Number(Number::from(*value)),
            ScalarValue::Float64(value) => {
                Number::from_f64(*value).map(Value::Number).ok_or_else(|| {
                    GatewayError::Manifest(format!(
                        "non-finite Float64 cannot be used as a GraphQL variable: {value}"
                    ))
                })?
            }
            ScalarValue::Bool(value) => Value::Bool(*value),
        })
    }

    fn graphql_variable_name<'a>(
        table: &'a TableCfg,
        graphql: &'a GraphqlTableCfg,
        column: &str,
    ) -> Option<&'a str> {
        graphql
            .arg_vars
            .get(column)
            .map(String::as_str)
            .or_else(|| {
                table
                    .filters
                    .get(column)
                    .map(|filter| filter.param.as_str())
            })
    }

    fn graphql_variables(
        table: &TableCfg,
        graphql: &GraphqlTableCfg,
        predicates: &[Predicate],
    ) -> Result<Map<String, Value>> {
        let mut variables = graphql.variables.as_object().cloned().unwrap_or_default();

        for predicate in predicates {
            if predicate.op != PredicateOp::Eq {
                continue;
            }

            if let Some(variable_name) =
                Self::graphql_variable_name(table, graphql, &predicate.column)
            {
                variables.insert(
                    variable_name.to_string(),
                    Self::graphql_value(&predicate.value)?,
                );
            }
        }

        Ok(variables)
    }
}

#[async_trait]
impl Adapter for ManifestAdapter {
    fn name(&self) -> &str {
        &self.manifest.source.name
    }

    fn catalog(&self) -> Vec<TableDef> {
        let mut defs: Vec<TableDef> = self
            .manifest
            .tables
            .iter()
            .filter_map(|table| Self::table_def(table).ok())
            .collect();

        if self.manifest.source.allow_writes {
            defs.extend(
                self.manifest
                    .actions
                    .iter()
                    .filter_map(|action| Self::action_def(action).ok()),
            );
        }

        defs
    }

    async fn scan(&self, req: ScanRequest) -> Result<Vec<RecordBatch>> {
        let table = self.table(&req.table)?;
        let columns = Self::column_extracts(table)?;
        let auth = self.resolve_auth().await?;
        let connection_ctx = ConnectionContext::from_env(&self.manifest.source.connection_config);
        let base_url = resolve_template(&self.manifest.source.base_url, &connection_ctx)?;

        let rows = match &self.manifest.source.transport {
            Transport::Rest => {
                let query = Self::query_params(table, &req.predicates);

                // v1 ignores projection and leaves non-Eq or undeclared predicates as
                // residual filters for DuckDB to apply after this scan returns rows.
                let fetch = HttpFetch {
                    client: &self.client,
                    base_url: &base_url,
                    path: &table.path,
                    query,
                    pagination: self.manifest.source.pagination.as_ref(),
                    auth: &auth,
                    response_path: table.response_path.clone(),
                    headers: resolve_headers(&self.manifest.source.headers, &connection_ctx)?,
                };
                fetch_rows(&fetch).await?
            }
            Transport::Graphql => {
                let graphql = table.graphql.as_ref().ok_or_else(|| {
                    GatewayError::Manifest(format!(
                        "table '{}' missing graphql config for graphql transport",
                        table.name
                    ))
                })?;
                let variables = Self::graphql_variables(table, graphql, &req.predicates)?;
                let fetch = GraphqlFetch {
                    client: &self.client,
                    endpoint: &base_url,
                    query: &graphql.query,
                    variables,
                    auth: &auth,
                    pagination: self.manifest.source.pagination.as_ref(),
                    response_path: table.response_path.clone(),
                };
                fetch_graphql_rows(&fetch).await?
            }
        };
        let batch = rows_to_batch(&columns, &rows)?;

        Ok(vec![batch])
    }

    async fn act(&self, req: ActionRequest) -> Result<Vec<RecordBatch>> {
        let ActionRequest {
            name,
            method,
            path,
            query,
            body,
            idempotency_key,
            dry_run,
        } = req;

        let action = self
            .manifest
            .actions
            .iter()
            .find(|action| action.name == name)
            .ok_or_else(|| GatewayError::Adapter(format!("unknown action {name}")))?;
        let connection_ctx = ConnectionContext::from_env(&self.manifest.source.connection_config);
        let base_url = resolve_template(&self.manifest.source.base_url, &connection_ctx)?;
        let url = format!("{}{}", base_url.trim_end_matches('/'), path);

        if dry_run {
            return match &action.columns {
                Some(_) => Self::render_typed_dry_run(action),
                None => Self::render_generic_row(
                    0,
                    serde_json::json!({
                        "dry_run": true,
                        "method": method,
                        "url": url,
                        "query": query,
                        "body": body,
                    }),
                ),
            };
        }

        let auth = self.resolve_auth().await?;
        let idempotency_key = match (&action.idempotency_header, idempotency_key) {
            (Some(header), Some(value)) => Some((header.clone(), value)),
            _ => None,
        };
        let http_action = HttpAction {
            client: &self.client,
            method: reqwest::Method::from_bytes(method.as_bytes())
                .map_err(|e| GatewayError::Http(e.to_string()))?,
            url,
            query,
            body,
            auth: &auth,
            idempotency_key,
            headers: resolve_headers(&self.manifest.source.headers, &connection_ctx)?,
        };

        let (status, body) = send_request(&http_action).await?;

        match &action.columns {
            Some(_) => {
                let columns = Self::action_column_extracts(action)?;
                let rows = Self::action_rows(&body, action.response_path.as_deref())?;
                Ok(vec![rows_to_batch(&columns, &rows)?])
            }
            None => Self::render_generic_row(status, body),
        }
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{resolve_headers, ManifestAdapter};
    use crate::adapter::manifest::Manifest;
    use crate::adapter::templating::ConnectionContext;
    use crate::adapter::{
        ActionRequest, Adapter, Predicate, PredicateOp, ResolvedAuth, ScalarValue, ScanRequest,
        TableKind,
    };
    use crate::error::GatewayError;

    #[test]
    fn resolve_headers_returns_literals_and_rejects_authorization() {
        use indexmap::IndexMap;
        let ctx = ConnectionContext::from_env(&[]);

        let mut headers = IndexMap::new();
        headers.insert("x-api-version".to_string(), "v17".to_string());
        let out = resolve_headers(&headers, &ctx).expect("resolve");
        assert_eq!(out, vec![("x-api-version".to_string(), "v17".to_string())]);

        let mut bad = IndexMap::new();
        bad.insert("Authorization".to_string(), "Bearer x".to_string());
        assert!(matches!(
            resolve_headers(&bad, &ctx),
            Err(GatewayError::Manifest(_))
        ));
    }

    #[tokio::test]
    async fn read_table_sends_static_header() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/items"))
            .and(wiremock::matchers::header("x-api-version", "v17"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{ "id": "a" }])),
            )
            .mount(&server)
            .await;

        let toml = format!(
            r#"
[source]
name = "svc"
base_url = "{base}"
[source.headers]
x-api-version = "v17"
[[table]]
name = "items"
path = "/items"
[table.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
            base = server.uri()
        );
        let adapter = ManifestAdapter::new(Manifest::from_toml(&toml).unwrap());
        let batches = adapter
            .scan(ScanRequest {
                table: "items".to_string(),
                predicates: vec![],
                projection: None,
                tvf_args: vec![],
                auth: ResolvedAuth::None,
            })
            .await
            .unwrap();
        assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 1);
    }

    #[tokio::test]
    async fn action_sends_bearer_and_static_header_together() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/orders"))
            .and(wiremock::matchers::header("authorization", "Bearer tok-1"))
            .and(wiremock::matchers::header("developer-token", "dev-123"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "order": { "id": "o1" } })),
            )
            .mount(&server)
            .await;

        let toml = format!(
            r#"
[source]
name = "svc"
base_url = "{base}"
allow_writes = true
[source.headers]
developer-token = "dev-123"
[[action]]
name = "create"
method = "POST"
path = "/orders"
response_path = "$.order"
[action.args]
price = {{ in = "body", type = "Float64", required = true }}
[action.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
            base = server.uri()
        );
        let adapter = ManifestAdapter::new(Manifest::from_toml(&toml).unwrap());
        let req = ActionRequest {
            name: "create".to_string(),
            method: "POST".to_string(),
            path: "/orders".to_string(),
            query: vec![],
            body: Some(serde_json::json!({ "price": 0.5 })),
            auth: ResolvedAuth::Bearer("tok-1".to_string()),
            idempotency_key: None,
            dry_run: false,
        };
        let batches = adapter.act(req).await.unwrap();
        assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 1);
    }

    #[tokio::test]
    async fn google_ads_gaql_sends_developer_token_and_bearer() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/customers/123/googleAds:search"))
            .and(wiremock::matchers::header("authorization", "Bearer ya29-test"))
            .and(wiremock::matchers::header("developer-token", "dev-tok-xyz"))
            .and(wiremock::matchers::body_string_contains("SELECT campaign.id"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "results": [
                    { "campaign": { "id": "9482117003" }, "metrics": { "impressions": 128940 } },
                    { "campaign": { "id": "9482117884" }, "metrics": { "impressions": 86552 } }
                ]
            })))
            .mount(&server)
            .await;

        // ConnectionContext::from_env maps a param to SPUR_CONN_{name} case-preserving,
        // so the param is named DEVELOPER_TOKEN (uppercase) to read SPUR_CONN_DEVELOPER_TOKEN.
        std::env::set_var("SPUR_CONN_DEVELOPER_TOKEN", "dev-tok-xyz");

        let toml = format!(
            r#"
[source]
name = "google_ads"
base_url = "{base}"
allow_writes = true
connection_config = ["DEVELOPER_TOKEN"]
[source.headers]
developer-token = "${{connectionConfig.DEVELOPER_TOKEN}}"
[[action]]
name = "google_ads_search"
method = "POST"
path = "/customers/{{customer_id}}/googleAds:search"
response_path = "$.results"
[action.args]
customer_id = {{ in = "path", type = "Utf8", required = true }}
query       = {{ in = "body", type = "Utf8", required = true }}
[action.columns]
campaign_id = {{ json = "$.campaign.id", type = "Utf8" }}
impressions = {{ json = "$.metrics.impressions", type = "Int64" }}
"#,
            base = server.uri()
        );
        let adapter = ManifestAdapter::new(Manifest::from_toml(&toml).unwrap());

        let req = ActionRequest {
            name: "google_ads_search".to_string(),
            method: "POST".to_string(),
            path: "/customers/123/googleAds:search".to_string(),
            query: vec![],
            body: Some(serde_json::json!({
                "query": "SELECT campaign.id, metrics.impressions FROM campaign"
            })),
            auth: ResolvedAuth::Bearer("ya29-test".to_string()),
            idempotency_key: None,
            dry_run: false,
        };
        let batches = adapter.act(req).await.unwrap();

        std::env::remove_var("SPUR_CONN_DEVELOPER_TOKEN");
        assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 2);
    }

    #[test]
    fn oauth2_refresh_toml_roundtrips() {
        let manifest = Manifest::from_toml(
            r#"
[source]
name = "notion"
base_url = "https://api.notion.com/v1"
auth = { scheme = "oauth2_refresh", token_url = "https://api.notion.com/v1/oauth/token", client_id_env = "NOTION_CLIENT_ID", client_secret_env = "NOTION_CLIENT_SECRET", refresh_token_env = "NOTION_REFRESH_TOKEN" }

[[table]]
name = "pages"
path = "/pages"

[table.columns]
id = { json = "$.id", type = "Utf8" }
"#,
        )
        .expect("manifest should parse");
        match manifest.source.auth {
            crate::adapter::manifest::AuthCfg::Oauth2Refresh {
                token_url,
                refresh_token_env,
                ..
            } => {
                assert_eq!(token_url, "https://api.notion.com/v1/oauth/token");
                assert_eq!(refresh_token_env, "NOTION_REFRESH_TOKEN");
            }
            other => panic!("expected oauth2_refresh, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn oauth2_refresh_scan_sends_minted_bearer() {
        let token_srv = MockServer::start().await;
        let api_srv = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "minted-xyz", "expires_in": 3600
            })))
            .mount(&token_srv)
            .await;
        Mock::given(method("GET"))
            .and(path("/things"))
            .and(header("authorization", "Bearer minted-xyz"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!([{ "id": "t1" }])),
            )
            .mount(&api_srv)
            .await;

        std::env::set_var("OAUTHSCAN_CLIENT_ID", "cid");
        std::env::set_var("OAUTHSCAN_CLIENT_SECRET", "csec");
        std::env::set_var("OAUTHSCAN_REFRESH_TOKEN", "oauth2_refresh_scan_rt");

        let toml = format!(
            r#"
[source]
name = "oauthscan"
base_url = "{api}"
auth = {{ scheme = "oauth2_refresh", token_url = "{token}/oauth/token", client_id_env = "OAUTHSCAN_CLIENT_ID", client_secret_env = "OAUTHSCAN_CLIENT_SECRET", refresh_token_env = "OAUTHSCAN_REFRESH_TOKEN" }}

[[table]]
name = "things"
path = "/things"

[table.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
            api = api_srv.uri(),
            token = token_srv.uri()
        );
        let adapter = ManifestAdapter::new(Manifest::from_toml(&toml).expect("parse"));
        let batches = adapter
            .scan(ScanRequest {
                table: "things".to_string(),
                predicates: vec![],
                projection: None,
                tvf_args: vec![],
                auth: ResolvedAuth::None,
            })
            .await
            .expect("scan should succeed");
        assert_eq!(batches[0].num_rows(), 1);
    }

    #[tokio::test]
    async fn scans_with_pushdown() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(query_param("active", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "m1",
                    "question": "Will it rain?",
                    "active": true,
                    "volume": 12.5
                },
                {
                    "id": "m2",
                    "question": "Will it snow?",
                    "active": true,
                    "volume": 7.25
                }
            ])))
            .mount(&server)
            .await;

        let manifest = Manifest::from_toml(&format!(
            r#"
[source]
name = "polymarket"
base_url = "{}"

[[table]]
name = "markets"
path = "/markets"

[table.columns]
id = {{ json = "$.id", type = "Utf8" }}
question = {{ json = "$.question", type = "Utf8" }}
active = {{ json = "$.active", type = "Boolean" }}
volume = {{ json = "$.volume", type = "Float64" }}

[table.filters]
active = {{ param = "active" }}
"#,
            server.uri()
        ))
        .expect("manifest should parse");
        let adapter = ManifestAdapter::new(manifest);

        let catalog = adapter.catalog();
        assert_eq!(catalog[0].schema.fields().len(), 4);

        let batches = adapter
            .scan(ScanRequest {
                table: "markets".to_string(),
                predicates: vec![Predicate {
                    column: "active".to_string(),
                    op: PredicateOp::Eq,
                    value: ScalarValue::Bool(true),
                }],
                projection: None,
                tvf_args: vec![],
                auth: ResolvedAuth::None,
            })
            .await
            .expect("scan should succeed");

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 2);
        let fields = batches[0].schema().fields().clone();
        let field_names: Vec<_> = fields.iter().map(|field| field.name().as_str()).collect();
        assert_eq!(field_names, ["id", "question", "active", "volume"]);
    }

    #[tokio::test]
    async fn action_post_applies_bearer_auth() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/orders"))
            .and(header("authorization", "Bearer tok-act-123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .mount(&server)
            .await;

        std::env::set_var("SPUR_TEST_ACT_BEARER", "tok-act-123");
        let manifest = Manifest::from_toml(&format!(
            r#"
[source]
name = "svc"
base_url = "{}"
allow_writes = true
auth = {{ scheme = "bearer", env = "SPUR_TEST_ACT_BEARER" }}

[[action]]
name = "create"
method = "POST"
path = "/orders"

[action.args]
"#,
            server.uri()
        ))
        .expect("manifest parses");

        let adapter = ManifestAdapter::new(manifest);
        let req = ActionRequest {
            name: "create".to_string(),
            method: "POST".to_string(),
            path: "/orders".to_string(),
            query: vec![],
            body: None,
            idempotency_key: None,
            dry_run: false,
        };
        let batches = adapter
            .act(req)
            .await
            .expect("authenticated action succeeds");
        std::env::remove_var("SPUR_TEST_ACT_BEARER");
        assert_eq!(batches.len(), 1);
    }

    #[tokio::test]
    async fn action_post_renders_typed_columns() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/orders/tok1"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "order": { "id": "o9" } })),
            )
            .mount(&server)
            .await;

        let toml = format!(
            r#"
[source]
name = "pm"
base_url = "{base}"
allow_writes = true

[[action]]
name = "place_order"
method = "POST"
path = "/orders/{{token_id}}"
response_path = "$.order"

[action.args]
token_id = {{ in = "path", type = "Utf8", required = true }}
price    = {{ in = "body", type = "Float64", required = true }}

[action.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
            base = server.uri()
        );
        let manifest = Manifest::from_toml(&toml).unwrap();
        let adapter = ManifestAdapter::new(manifest);

        assert!(adapter
            .catalog()
            .iter()
            .any(|t| t.name == "place_order" && matches!(t.kind, TableKind::Action { .. })));

        let req = ActionRequest {
            name: "place_order".to_string(),
            method: "POST".to_string(),
            path: "/orders/tok1".to_string(),
            query: vec![],
            body: Some(serde_json::json!({ "price": 0.5 })),
            idempotency_key: None,
            dry_run: false,
        };
        let batches = adapter.act(req).await.unwrap();
        assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 1);
    }

    #[tokio::test]
    async fn dry_run_with_columns_matches_typed_schema() {
        let manifest = Manifest::from_toml(
            r#"
[source]
name = "svc"
base_url = "https://example.invalid"
allow_writes = true

[[action]]
name = "create"
method = "POST"
path = "/orders"
dry_run_arg = "dry_run"

[action.args]

[action.columns]
order_id = { json = "$.id", type = "Utf8" }
"#,
        )
        .expect("parses");
        let adapter = ManifestAdapter::new(manifest);
        let req = ActionRequest {
            name: "create".into(),
            method: "POST".into(),
            path: "/orders".into(),
            query: vec![],
            body: None,
            idempotency_key: None,
            dry_run: true,
        };
        let batches = adapter.act(req).await.expect("dry-run ok");
        let typed = ManifestAdapter::new(
            Manifest::from_toml(
                "[source]\nname=\"svc\"\nbase_url=\"https://example.invalid\"\nallow_writes=true\n\n[[action]]\nname=\"create\"\nmethod=\"POST\"\npath=\"/orders\"\n\n[action.args]\n\n[action.columns]\norder_id = { json = \"$.id\", type = \"Utf8\" }\n",
            )
            .unwrap(),
        );
        let expected = typed
            .catalog()
            .into_iter()
            .find(|d| d.name == "create")
            .unwrap()
            .schema;

        assert_eq!(batches[0].schema().fields(), expected.fields());
    }

    #[test]
    fn action_def_rejects_non_write_method() {
        let toml = r#"
[source]
name = "svc"
base_url = "https://x"
allow_writes = true

[[action]]
name = "bad"
method = "GET"
path = "/x"

[action.args]
"#;
        let manifest = Manifest::from_toml(toml).expect("parses");
        // catalog() filter_maps Ok(...) only, so a GET action must be dropped/erroring.
        let defs = ManifestAdapter::new(manifest).catalog();
        assert!(
            !defs.iter().any(|d| d.name == "bad"),
            "non-write method must not produce an action def"
        );
    }

    #[test]
    fn action_def_accepts_write_methods() {
        for method in ["post", "PUT", "Patch", "DELETE"] {
            let toml = format!(
                "[source]\nname=\"s\"\nbase_url=\"https://x\"\nallow_writes=true\n\n[[action]]\nname=\"a\"\nmethod=\"{method}\"\npath=\"/x\"\n\n[action.args]\n"
            );
            let manifest = Manifest::from_toml(&toml).expect("parses");
            let defs = ManifestAdapter::new(manifest).catalog();
            let def = defs
                .into_iter()
                .find(|d| d.name == "a")
                .expect("write method must produce an action def");

            if let TableKind::Action { method: stored, .. } = def.kind {
                assert_eq!(stored, method.to_ascii_uppercase());
            } else {
                panic!("write method must produce an action def");
            }
        }
    }

    #[tokio::test]
    async fn action_hidden_when_writes_disabled() {
        let toml = r#"
[source]
name = "pm"
base_url = "https://example.com"

[[action]]
name = "place_order"
method = "POST"
path = "/orders"

[action.args]
price = { in = "body", type = "Float64", required = true }
"#;
        let adapter = ManifestAdapter::new(Manifest::from_toml(toml).unwrap());
        assert!(!adapter.catalog().iter().any(|t| t.name == "place_order"));
    }

    #[tokio::test]
    async fn basic_auth_applied() {
        let server = MockServer::start().await;
        std::env::set_var("SPUR_REST_GATEWAY_BASIC_USER", "user");
        std::env::set_var("SPUR_REST_GATEWAY_BASIC_PASS", "pass");

        Mock::given(method("GET"))
            .and(path("/accounts"))
            .and(header("authorization", "Basic dXNlcjpwYXNz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "a1" }
            ])))
            .mount(&server)
            .await;

        let manifest = Manifest::from_toml(&format!(
            r#"
[source]
name = "nango"
base_url = "{}"
auth = {{ scheme = "basic", user_env = "SPUR_REST_GATEWAY_BASIC_USER", pass_env = "SPUR_REST_GATEWAY_BASIC_PASS" }}

[[table]]
name = "accounts"
path = "/accounts"

[table.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
            server.uri()
        ))
        .expect("manifest should parse");
        let adapter = ManifestAdapter::new(manifest);

        let batches = adapter
            .scan(ScanRequest {
                table: "accounts".to_string(),
                predicates: vec![],
                projection: None,
                tvf_args: vec![],
                auth: ResolvedAuth::None,
            })
            .await
            .expect("scan should succeed");

        assert_eq!(batches[0].num_rows(), 1);
    }

    #[tokio::test]
    async fn api_key_query_applied() {
        let server = MockServer::start().await;
        std::env::set_var("SPUR_REST_GATEWAY_API_KEY_QUERY", "secret");

        Mock::given(method("GET"))
            .and(path("/accounts"))
            .and(query_param("api_key", "secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "a1" }
            ])))
            .mount(&server)
            .await;

        let manifest = Manifest::from_toml(&format!(
            r#"
[source]
name = "nango"
base_url = "{}"
auth = {{ scheme = "api_key_query", param = "api_key", env = "SPUR_REST_GATEWAY_API_KEY_QUERY" }}

[[table]]
name = "accounts"
path = "/accounts"

[table.columns]
id = {{ json = "$.id", type = "Utf8" }}
"#,
            server.uri()
        ))
        .expect("manifest should parse");
        let adapter = ManifestAdapter::new(manifest);

        let batches = adapter
            .scan(ScanRequest {
                table: "accounts".to_string(),
                predicates: vec![],
                projection: None,
                tvf_args: vec![],
                auth: ResolvedAuth::None,
            })
            .await
            .expect("scan should succeed");

        assert_eq!(batches[0].num_rows(), 1);
    }

    #[tokio::test]
    async fn base_url_templated() {
        let server = MockServer::start().await;
        std::env::set_var("SPUR_CONN_base_url_templated_host", server.uri());

        Mock::given(method("GET"))
            .and(path("/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "a1" }
            ])))
            .mount(&server)
            .await;

        let manifest = Manifest::from_toml(
            r#"
[source]
name = "nango"
base_url = "${connectionConfig.base_url_templated_host}"
connection_config = ["base_url_templated_host"]

[[table]]
name = "accounts"
path = "/accounts"

[table.columns]
id = { json = "$.id", type = "Utf8" }
"#,
        )
        .expect("manifest should parse");
        let adapter = ManifestAdapter::new(manifest);

        let batches = adapter
            .scan(ScanRequest {
                table: "accounts".to_string(),
                predicates: vec![],
                projection: None,
                tvf_args: vec![],
                auth: ResolvedAuth::None,
            })
            .await
            .expect("scan should succeed");

        assert_eq!(batches[0].num_rows(), 1);
    }
}

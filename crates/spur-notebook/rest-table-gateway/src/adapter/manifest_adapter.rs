use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::{Field, Schema, SchemaRef};
use async_trait::async_trait;
use reqwest::Client;

use crate::adapter::http::{fetch_rows, HttpFetch};
use crate::adapter::json_to_batch::{arrow_type, rows_to_batch, ColumnExtract};
use crate::adapter::manifest::{AuthCfg, Manifest, TableCfg};
use crate::adapter::templating::{resolve_template, ConnectionContext};
use crate::adapter::{
    Adapter, Predicate, PredicateOp, ResolvedAuth, ScalarValue, ScanRequest, TableDef, TableKind,
};
use crate::error::{GatewayError, Result};

pub struct ManifestAdapter {
    manifest: Manifest,
    client: Client,
}

impl ManifestAdapter {
    pub fn new(manifest: Manifest) -> Self {
        Self {
            manifest,
            client: Client::new(),
        }
    }

    fn table<'a>(&'a self, name: &str) -> Result<&'a TableCfg> {
        self.manifest
            .tables
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| GatewayError::UnknownTable(name.to_string()))
    }

    fn resolve_auth(&self) -> ResolvedAuth {
        match &self.manifest.source.auth {
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
        }
    }

    fn schema_for_table(table: &TableCfg) -> Result<SchemaRef> {
        let fields = table
            .columns
            .iter()
            .map(|(name, column)| Ok(Field::new(name.clone(), arrow_type(&column.ty)?, true)))
            .collect::<Result<Vec<_>>>()?;

        Ok(Arc::new(Schema::new(fields)))
    }

    fn table_def(table: &TableCfg) -> Result<TableDef> {
        Ok(TableDef {
            name: table.name.clone(),
            schema: Self::schema_for_table(table)?,
            kind: TableKind::Table,
        })
    }

    fn column_extracts(table: &TableCfg) -> Result<Vec<ColumnExtract>> {
        table
            .columns
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
}

#[async_trait]
impl Adapter for ManifestAdapter {
    fn name(&self) -> &str {
        &self.manifest.source.name
    }

    fn catalog(&self) -> Vec<TableDef> {
        self.manifest
            .tables
            .iter()
            .filter_map(|table| Self::table_def(table).ok())
            .collect()
    }

    async fn scan(&self, req: ScanRequest) -> Result<Vec<RecordBatch>> {
        let table = self.table(&req.table)?;
        let columns = Self::column_extracts(table)?;
        let query = Self::query_params(table, &req.predicates);
        let auth = self.resolve_auth();
        let connection_ctx = ConnectionContext::from_env(&self.manifest.source.connection_config);
        let base_url = resolve_template(&self.manifest.source.base_url, &connection_ctx)?;

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
        };
        let rows = fetch_rows(&fetch).await?;
        let batch = rows_to_batch(&columns, &rows)?;

        Ok(vec![batch])
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::ManifestAdapter;
    use crate::adapter::manifest::Manifest;
    use crate::adapter::{Adapter, Predicate, PredicateOp, ResolvedAuth, ScalarValue, ScanRequest};

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

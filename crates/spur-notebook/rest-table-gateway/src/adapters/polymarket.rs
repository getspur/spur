use std::sync::Arc;

use arrow_array::{Float64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use async_trait::async_trait;
use reqwest::Client;

use crate::adapter::manifest::Manifest;
use crate::adapter::manifest_adapter::ManifestAdapter;
use crate::adapter::{Adapter, ScalarValue, ScanRequest, TableDef, TableKind};
use crate::error::{GatewayError, Result};

/// Per live grounding, Gamma's `active` filter is reliable only for `active=true`.
pub const MARKETS_MANIFEST: &str = "[source]\nname = \"polymarket\"\nbase_url = \"{gamma_base}\"\npagination = { style = \"offset\", limit_param = \"limit\", offset_param = \"offset\", page_size = 500 }\n[[table]]\nname = \"markets\"\npath = \"/markets\"\n[table.columns]\nid = { json = \"$.id\", type = \"Utf8\" }\nquestion = { json = \"$.question\", type = \"Utf8\" }\nactive = { json = \"$.active\", type = \"Boolean\" }\nvolume = { json = \"$.volume\", type = \"Float64\" }\n[table.filters]\nactive = { param = \"active\" }\n";

pub struct PolymarketAdapter {
    inner: ManifestAdapter,
    clob_base: String,
    client: Client,
}

impl PolymarketAdapter {
    /// `gamma_base` feeds the manifest tables; `clob_base` feeds the orderbook TVF.
    pub fn new(gamma_base: &str, clob_base: &str) -> Result<Self> {
        let toml = MARKETS_MANIFEST.replace("{gamma_base}", gamma_base);
        let manifest = Manifest::from_toml(&toml)?;
        Ok(Self {
            inner: ManifestAdapter::new(manifest),
            clob_base: clob_base.trim_end_matches('/').to_string(),
            client: Client::new(),
        })
    }

    fn orderbook_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            Field::new("price", DataType::Float64, true),
            Field::new("size", DataType::Float64, true),
        ]))
    }

    async fn scan_orderbook(&self, args: &[ScalarValue]) -> Result<Vec<RecordBatch>> {
        let token = match args.first() {
            Some(ScalarValue::Utf8(s)) => s.clone(),
            _ => {
                return Err(GatewayError::Adapter(
                    "orderbook(token_id, depth): arg 0 must be a string token id".into(),
                ));
            }
        };
        let depth = match args.get(1) {
            Some(ScalarValue::Int64(n)) => *n,
            _ => 50,
        };
        let url = format!("{}/book", self.clob_base);
        let resp = self
            .client
            .get(url)
            .query(&[("token_id", token)])
            .send()
            .await
            .map_err(|e| GatewayError::Http(e.to_string()))?;
        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::Http(e.to_string()))?;
        let levels = body
            .get("bids")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        // The CLOB API has no depth query param, so limit the full book locally.
        let take = (depth.max(0) as usize).min(levels.len());
        let levels = &levels[..take];
        let price: Float64Array = levels
            .iter()
            .map(|l| {
                l.get("price")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .or_else(|| l.get("price").and_then(|v| v.as_f64()))
            })
            .collect();
        let size: Float64Array = levels
            .iter()
            .map(|l| {
                l.get("size")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<f64>().ok())
                    .or_else(|| l.get("size").and_then(|v| v.as_f64()))
            })
            .collect();
        let batch = RecordBatch::try_new(
            Self::orderbook_schema(),
            vec![Arc::new(price), Arc::new(size)],
        )
        .map_err(|e| GatewayError::Schema(e.to_string()))?;
        Ok(vec![batch])
    }
}

#[async_trait]
impl Adapter for PolymarketAdapter {
    fn name(&self) -> &str {
        "polymarket"
    }

    fn catalog(&self) -> Vec<TableDef> {
        let mut v = self.inner.catalog();
        v.push(TableDef {
            name: "orderbook".into(),
            schema: Self::orderbook_schema(),
            kind: TableKind::TableFunction {
                arg_names: vec!["token_id".into(), "depth".into()],
            },
        });
        v
    }

    async fn scan(&self, req: ScanRequest) -> Result<Vec<RecordBatch>> {
        if req.table == "orderbook" {
            self.scan_orderbook(&req.tvf_args).await
        } else {
            self.inner.scan(req).await
        }
    }
}

#[cfg(test)]
mod tests {
    use arrow_array::Float64Array;
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::PolymarketAdapter;
    use crate::adapter::{Adapter, ResolvedAuth, ScalarValue, ScanRequest, TableKind};

    fn scan_request(table: &str, tvf_args: Vec<ScalarValue>) -> ScanRequest {
        ScanRequest {
            table: table.to_string(),
            predicates: vec![],
            projection: None,
            tvf_args,
            auth: ResolvedAuth::None,
        }
    }

    #[tokio::test]
    async fn polymarket_markets_table() {
        let gamma = MockServer::start().await;
        let clob = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/markets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "m1",
                    "question": "Will BTC close above 100k?",
                    "active": true,
                    "volume": 42.5
                }
            ])))
            .mount(&gamma)
            .await;

        let adapter =
            PolymarketAdapter::new(&gamma.uri(), &clob.uri()).expect("adapter should construct");

        let batches = adapter
            .scan(scan_request("markets", vec![]))
            .await
            .expect("markets scan should succeed");

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 1);
        assert_eq!(batches[0].num_columns(), 4);
    }

    #[tokio::test]
    async fn polymarket_orderbook_tvf() {
        let gamma = MockServer::start().await;
        let clob = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/book"))
            .and(query_param("token_id", "0xabc"))
            .and(query_param_is_missing("depth"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bids": [
                    { "price": "0.51", "size": "120" },
                    { "price": "0.50", "size": "80" }
                ]
            })))
            .mount(&clob)
            .await;

        let adapter =
            PolymarketAdapter::new(&gamma.uri(), &clob.uri()).expect("adapter should construct");
        let orderbook = adapter
            .catalog()
            .into_iter()
            .find(|table| table.name == "orderbook")
            .expect("catalog should include orderbook");
        assert!(matches!(
            orderbook.kind,
            TableKind::TableFunction { ref arg_names }
                if arg_names == &["token_id".to_string(), "depth".to_string()]
        ));

        let batches = adapter
            .scan(scan_request(
                "orderbook",
                vec![
                    ScalarValue::Utf8("0xabc".to_string()),
                    ScalarValue::Int64(50),
                ],
            ))
            .await
            .expect("orderbook scan should succeed");

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 2);
        let prices = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("price should be Float64Array");
        assert_eq!(prices.value(0), 0.51);
    }

    #[tokio::test]
    async fn orderbook_truncates_to_depth() {
        let gamma = MockServer::start().await;
        let clob = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/book"))
            .and(query_param("token_id", "0xabc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bids": [
                    { "price": "0.55", "size": "100" },
                    { "price": "0.54", "size": "90" },
                    { "price": "0.53", "size": "80" },
                    { "price": "0.52", "size": "70" },
                    { "price": "0.51", "size": "60" }
                ]
            })))
            .mount(&clob)
            .await;

        let adapter =
            PolymarketAdapter::new(&gamma.uri(), &clob.uri()).expect("adapter should construct");

        let batches = adapter
            .scan(scan_request(
                "orderbook",
                vec![
                    ScalarValue::Utf8("0xabc".to_string()),
                    ScalarValue::Int64(3),
                ],
            ))
            .await
            .expect("orderbook scan should succeed");

        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 3);
    }
}

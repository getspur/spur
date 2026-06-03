pub mod graphql;
pub mod http;
pub mod json_to_batch;
pub mod manifest;
pub mod manifest_adapter;
pub mod nango;
pub(crate) mod oauth;
pub mod openapi;
pub mod templating;

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::{DataType, SchemaRef};
use async_trait::async_trait;

use crate::error::Result;

/// User-Agent sent on every outbound request. Many real-world provider APIs
/// (GitHub, CoinGecko, anything behind a WAF/Cloudflare) reject requests with
/// no User-Agent header with `403 Forbidden`, so a bare `reqwest::Client::new()`
/// — which sets none — cannot talk to them. Always build the client through
/// [`default_http_client`] so every adapter carries this identifier.
pub const DEFAULT_USER_AGENT: &str = concat!("spur-rest-table-gateway/", env!("CARGO_PKG_VERSION"));

/// reqwest client preconfigured with [`DEFAULT_USER_AGENT`]. Falls back to a
/// default client if the builder fails (mirrors `Client::new()`'s own panic
/// path, but without the missing User-Agent).
pub fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(DEFAULT_USER_AGENT)
        .build()
        .unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub enum ScalarValue {
    Utf8(String),
    Int64(i64),
    Float64(f64),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredicateOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Predicate {
    pub column: String,
    pub op: PredicateOp,
    pub value: ScalarValue,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum ResolvedAuth {
    #[default]
    None,
    Bearer(String),
    Header {
        name: String,
        value: String,
    },
    Basic {
        user: String,
        pass: String,
    },
    QueryParam {
        param: String,
        value: String,
    },
}

#[derive(Debug, Clone)]
pub struct ScanRequest {
    pub table: String,
    pub predicates: Vec<Predicate>,
    pub projection: Option<Vec<String>>,
    pub tvf_args: Vec<ScalarValue>,
    pub auth: ResolvedAuth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgLocation {
    Path,
    Body,
    Query,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArgSpec {
    pub name: String,
    pub location: ArgLocation,
    pub ty: DataType,
    pub required: bool,
    pub json_key: String,
    pub query_param: String,
}

#[derive(Debug, Clone)]
pub struct ActionRequest {
    pub name: String,
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Option<serde_json::Value>,
    pub idempotency_key: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TableKind {
    Table,
    TableFunction {
        arg_names: Vec<String>,
    },
    Action {
        method: String,
        path: String,
        arg_specs: Vec<ArgSpec>,
        dry_run_arg: Option<String>,
        idempotency_header: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct TableDef {
    pub name: String,
    pub schema: SchemaRef,
    pub kind: TableKind,
}

#[async_trait]
pub trait Adapter: Send + Sync {
    fn name(&self) -> &str;
    fn catalog(&self) -> Vec<TableDef>;
    async fn scan(&self, req: ScanRequest) -> Result<Vec<RecordBatch>>;
    async fn act(&self, _req: ActionRequest) -> Result<Vec<RecordBatch>> {
        Err(crate::error::GatewayError::Adapter(
            "this adapter does not support actions".to_string(),
        ))
    }
}

#[derive(Default)]
pub struct AdapterRegistry {
    adapters: HashMap<String, Arc<dyn Adapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, adapter: Arc<dyn Adapter>) {
        self.adapters.insert(adapter.name().to_string(), adapter);
    }

    pub fn get(&self, source: &str) -> Option<Arc<dyn Adapter>> {
        self.adapters.get(source).cloned()
    }

    pub fn sources(&self) -> Vec<String> {
        let mut v: Vec<String> = self.adapters.keys().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::RecordBatch;
    use async_trait::async_trait;

    use super::{Adapter, AdapterRegistry, ScanRequest, TableDef};
    use crate::error::Result;

    struct TestAdapter(&'static str);

    #[async_trait]
    impl Adapter for TestAdapter {
        fn name(&self) -> &str {
            self.0
        }

        fn catalog(&self) -> Vec<TableDef> {
            vec![]
        }

        async fn scan(&self, _req: ScanRequest) -> Result<Vec<RecordBatch>> {
            Ok(vec![])
        }
    }

    #[test]
    fn registry_registers_and_lists_sources() {
        let mut registry = AdapterRegistry::new();

        registry.register(Arc::new(TestAdapter("b")));
        registry.register(Arc::new(TestAdapter("a")));

        assert_eq!(registry.sources(), ["a".to_string(), "b".to_string()]);
        assert!(registry.get("a").is_some());
    }
}

pub mod manifest;
pub mod json_to_batch;
pub mod http;
pub mod manifest_adapter;

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use async_trait::async_trait;

use crate::error::Result;

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

#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedAuth {
    None,
    Bearer(String),
    Header { name: String, value: String },
}

impl Default for ResolvedAuth {
    fn default() -> Self {
        ResolvedAuth::None
    }
}

#[derive(Debug, Clone)]
pub struct ScanRequest {
    pub table: String,
    pub predicates: Vec<Predicate>,
    pub projection: Option<Vec<String>>,
    pub tvf_args: Vec<ScalarValue>,
    pub auth: ResolvedAuth,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TableKind {
    Table,
    TableFunction { arg_names: Vec<String> },
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

use std::sync::Arc;

use duckdb::Connection;

use crate::adapter::{Adapter, TableKind};
use crate::error::{GatewayError, Result};
use crate::vtab::bridge::IoBridge;
use crate::vtab::table_fn::{ApiTableExtra, ApiTableVTab};

/// Register every kind=Table entry of `adapter` as a zero-arg DuckDB table
/// function named `{source}_{table}`.
pub fn register_tables(
    conn: &Connection,
    adapter: Arc<dyn Adapter>,
    bridge: Arc<IoBridge>,
) -> Result<usize> {
    let mut n = 0;
    for t in adapter.catalog() {
        if !matches!(t.kind, TableKind::Table) {
            continue;
        }

        let fn_name = format!("{}_{}", adapter.name(), t.name);
        let extra = ApiTableExtra {
            bridge: Arc::clone(&bridge),
            adapter: Arc::clone(&adapter),
            table: t.name,
            schema: t.schema,
        };
        conn.register_table_function_with_extra_info::<ApiTableVTab, _>(&fn_name, &extra)
            .map_err(|e| GatewayError::Adapter(e.to_string()))?;
        n += 1;
    }

    Ok(n)
}

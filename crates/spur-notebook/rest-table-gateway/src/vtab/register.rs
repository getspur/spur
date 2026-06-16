use std::sync::Arc;

use duckdb::Connection;

use crate::adapter::{Adapter, TableKind};
use crate::error::{GatewayError, Result};
use crate::vtab::bridge::IoBridge;
use crate::vtab::table_fn::{ApiTableExtra, ApiTableVTab};

pub fn api_table_function_name(source: &str, table: &str) -> String {
    format!("{source}_{table}")
}

pub fn register_api_relation_view(conn: &Connection, source: &str, table: &str) -> Result<()> {
    let schema = duckdb_identifier(source);
    let relation = duckdb_identifier(table);
    let function = duckdb_identifier(&api_table_function_name(source, table));

    conn.execute(&format!("CREATE SCHEMA IF NOT EXISTS {schema}"), [])
        .map_err(|e| GatewayError::Adapter(e.to_string()))?;
    conn.execute(
        &format!("CREATE OR REPLACE VIEW {schema}.{relation} AS SELECT * FROM {function}()"),
        [],
    )
    .map_err(|e| GatewayError::Adapter(e.to_string()))?;

    Ok(())
}

fn duckdb_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

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

        let table_name = t.name;
        let fn_name = api_table_function_name(adapter.name(), &table_name);
        let extra = ApiTableExtra {
            bridge: Arc::clone(&bridge),
            adapter: Arc::clone(&adapter),
            table: table_name.clone(),
            schema: t.schema,
        };
        conn.register_table_function_with_extra_info::<ApiTableVTab, _>(&fn_name, &extra)
            .map_err(|e| GatewayError::Adapter(e.to_string()))?;
        register_api_relation_view(conn, adapter.name(), &table_name)?;
        n += 1;
    }

    Ok(n)
}

//! Datasource schema-probe boundary.
//!
//! This module intentionally exposes only attach-time metadata probing. Query
//! execution and analysis stay in the Python kernel.

use std::path::Path;

use anyhow::{Context as _, Result};
use duckdb::Connection;
use jute::commands::{Column, DatasourceKind};

/// Schema metadata discovered for a datasource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasourceSchema {
    /// Columns returned by `DuckDB`'s read function.
    pub columns: Vec<Column>,
    /// Best-effort row count.
    pub row_count: Option<u64>,
}

/// Probe a datasource using a read-only `DuckDB` query.
pub fn introspect_datasource(path: &Path, kind: DatasourceKind) -> Result<DatasourceSchema> {
    let path = path
        .to_str()
        .context("datasource path must be valid UTF-8")?;
    let scan = scan_expression(path, kind);
    let conn = Connection::open_in_memory().context("failed to open DuckDB schema probe")?;

    let describe_sql = format!("DESCRIBE SELECT * FROM {scan}");
    let mut statement = conn
        .prepare(&describe_sql)
        .with_context(|| format!("failed to prepare schema probe for {path}"))?;
    let rows = statement
        .query_map([], |row| {
            Ok(Column {
                name: row.get(0)?,
                sql_type: row.get(1)?,
            })
        })
        .with_context(|| format!("failed to run schema probe for {path}"))?;

    let mut columns = Vec::new();
    for row in rows {
        columns.push(row.with_context(|| format!("failed to read schema row for {path}"))?);
    }

    let count_sql = format!("SELECT count(*) FROM {scan}");
    let row_count = conn
        .query_row(&count_sql, [], |row| row.get::<_, i64>(0))
        .ok()
        .and_then(|count| u64::try_from(count).ok());

    Ok(DatasourceSchema { columns, row_count })
}

fn scan_expression(path: &str, kind: DatasourceKind) -> String {
    let literal = sql_string_literal(path);
    match kind {
        DatasourceKind::Csv => format!("read_csv_auto({literal})"),
        DatasourceKind::Parquet => format!("read_parquet({literal})"),
        DatasourceKind::Json => format!("read_json_auto({literal})"),
    }
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use duckdb::Connection;

    #[test]
    fn duckdb_links() -> duckdb::Result<()> {
        let conn = Connection::open_in_memory()?;
        let selected: i64 = conn.query_row("SELECT 1", [], |row| row.get(0))?;

        assert_eq!(selected, 1);
        Ok(())
    }
}

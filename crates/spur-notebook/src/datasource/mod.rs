//! Datasource schema-probe boundary.
//!
//! This module intentionally exposes only attach-time metadata probing. Query
//! execution and analysis stay in the Python kernel.

use std::path::Path;

use anyhow::{Context as _, Result};
use duckdb::Connection;
use jute::commands::{Column, DatasourceKind, Table};

/// Schema metadata discovered for a datasource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasourceSchema {
    /// Columns returned by `DuckDB`'s read function.
    pub columns: Vec<Column>,
    /// Best-effort row count.
    pub row_count: Option<u64>,
    /// Tables discovered for multi-table datasources.
    pub tables: Vec<Table>,
}

/// Probe a datasource using a read-only `DuckDB` query.
pub fn introspect_datasource(path: &Path, kind: DatasourceKind) -> Result<DatasourceSchema> {
    let path = path
        .to_str()
        .context("datasource path must be valid UTF-8")?;
    if kind == DatasourceKind::DuckDb {
        return introspect_duckdb(path);
    }

    let scan = scan_expression(path, kind);
    let conn = Connection::open_in_memory().context("failed to open DuckDB schema probe")?;

    let columns = describe_columns(&conn, &scan, path)?;
    let row_count = row_count(&conn, &scan);

    Ok(DatasourceSchema {
        columns,
        row_count,
        tables: Vec::new(),
    })
}

fn introspect_duckdb(path: &str) -> Result<DatasourceSchema> {
    const PROBE_ALIAS: &str = "__spur_probe";

    let conn = Connection::open_in_memory().context("failed to open DuckDB schema probe")?;
    conn.execute_batch(&format!(
        "ATTACH {} AS {} (READ_ONLY)",
        sql_string_literal(path),
        sql_identifier(PROBE_ALIAS)
    ))
    .with_context(|| format!("failed to attach DuckDB datasource {path}"))?;

    let mut statement = conn
        .prepare(
            "SELECT table_name FROM duckdb_tables() \
             WHERE database_name = '__spur_probe' AND schema_name = 'main' AND NOT internal \
             ORDER BY table_name",
        )
        .with_context(|| format!("failed to prepare DuckDB table probe for {path}"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(0))
        .with_context(|| format!("failed to run DuckDB table probe for {path}"))?;

    let mut table_names = Vec::new();
    for row in rows {
        table_names
            .push(row.with_context(|| format!("failed to read DuckDB table row for {path}"))?);
    }

    let mut tables = Vec::with_capacity(table_names.len());
    for table_name in table_names {
        let table_ref = format!(
            "{}.{}.{}",
            sql_identifier(PROBE_ALIAS),
            sql_identifier("main"),
            sql_identifier(&table_name)
        );
        tables.push(Table {
            name: table_name,
            columns: describe_columns(&conn, &table_ref, path)?,
            row_count: row_count(&conn, &table_ref),
        });
    }

    Ok(DatasourceSchema {
        columns: Vec::new(),
        row_count: None,
        tables,
    })
}

fn describe_columns(conn: &Connection, relation: &str, path: &str) -> Result<Vec<Column>> {
    let describe_sql = format!("DESCRIBE SELECT * FROM {relation}");
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
    Ok(columns)
}

fn row_count(conn: &Connection, relation: &str) -> Option<u64> {
    let count_sql = format!("SELECT count(*) FROM {relation}");
    conn.query_row(&count_sql, [], |row| row.get::<_, i64>(0))
        .ok()
        .and_then(|count| u64::try_from(count).ok())
}

fn scan_expression(path: &str, kind: DatasourceKind) -> String {
    let literal = sql_string_literal(path);
    match kind {
        DatasourceKind::Csv => format!("read_csv_auto({literal})"),
        DatasourceKind::Parquet => format!("read_parquet({literal})"),
        DatasourceKind::Json => format!("read_json_auto({literal})"),
        DatasourceKind::DuckDb => unreachable!("DuckDB files are probed through ATTACH"),
    }
}

fn sql_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use duckdb::Connection;

    #[test]
    fn duckdb_links() -> duckdb::Result<()> {
        let conn = Connection::open_in_memory()?;
        let selected: i64 = conn.query_row("SELECT 1", [], |row| row.get(0))?;

        assert_eq!(selected, 1);
        Ok(())
    }

    #[test]
    fn attach_duckdb_introspects_all_tables() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let db_path = tempdir.path().join("warehouse.duckdb");
        {
            let conn = Connection::open(&db_path)?;
            conn.execute_batch(
                r#"
                CREATE TABLE sales(region VARCHAR, revenue DOUBLE);
                INSERT INTO sales VALUES ('west', 10.0), ('east', 20.0);
                CREATE TABLE inventory(sku VARCHAR, quantity INTEGER);
                INSERT INTO inventory VALUES ('a', 3);
                "#,
            )?;
        }

        let schema = introspect_datasource(&db_path, DatasourceKind::DuckDb)?;

        assert!(schema.columns.is_empty());
        assert_eq!(schema.row_count, None);
        assert_eq!(
            schema.tables,
            vec![
                jute::commands::Table {
                    name: "inventory".to_string(),
                    columns: vec![
                        Column {
                            name: "sku".to_string(),
                            sql_type: "VARCHAR".to_string(),
                        },
                        Column {
                            name: "quantity".to_string(),
                            sql_type: "INTEGER".to_string(),
                        },
                    ],
                    row_count: Some(1),
                },
                jute::commands::Table {
                    name: "sales".to_string(),
                    columns: vec![
                        Column {
                            name: "region".to_string(),
                            sql_type: "VARCHAR".to_string(),
                        },
                        Column {
                            name: "revenue".to_string(),
                            sql_type: "DOUBLE".to_string(),
                        },
                    ],
                    row_count: Some(2),
                },
            ]
        );
        Ok(())
    }
}

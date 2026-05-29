//! Datasource schema-probe boundary.
//!
//! Introspection is implemented in a later task. This module currently exists
//! only to keep DuckDB behind the `datasource-introspect` feature and prove the
//! dependency links when enabled.

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

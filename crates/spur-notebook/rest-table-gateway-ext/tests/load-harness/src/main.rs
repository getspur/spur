use std::env;
use std::path::Path;

use duckdb::{Config, Connection};

fn sql_string(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

fn main() -> duckdb::Result<()> {
    let extension_path = env::args()
        .nth(1)
        .expect("usage: load-harness <extension-path>");

    let config = Config::default().allow_unsigned_extensions()?;
    let conn = Connection::open_in_memory_with_flags(config)?;
    conn.execute(&format!("LOAD '{}'", sql_string(Path::new(&extension_path))), [])?;

    let mut stmt = conn.prepare("SELECT id, volume FROM polymarket_markets() ORDER BY id")?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<f64>>(1)?))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;

    println!("polymarket_markets rows: {rows:?}");

    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, "m1");
    assert!(rows[0].1.is_some(), "string volume should be non-null");
    assert!((rows[0].1.unwrap() - 782_375.55).abs() < 0.000_01);

    let mut stmt = conn.prepare(
        "SELECT price, size FROM polymarket_orderbook(token_id := '0xabc', depth := 1)",
    )?;
    let orderbook_rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, Option<f64>>(0)?, row.get::<_, Option<f64>>(1)?))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;

    println!("polymarket_orderbook rows: {orderbook_rows:?}");

    assert_eq!(orderbook_rows.len(), 1);
    assert!((orderbook_rows[0].0.expect("price should be non-null") - 0.51).abs() < 0.000_01);
    assert!((orderbook_rows[0].1.expect("size should be non-null") - 120.0).abs() < 0.000_01);

    Ok(())
}

use std::sync::Arc;

use duckdb::Connection;
use spur_rest_table_gateway::adapters::polymarket::PolymarketAdapter;
use spur_rest_table_gateway::vtab::bridge::IoBridge;
use spur_rest_table_gateway::vtab::register::register_tables;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn polymarket_markets_table_function_e2e() {
    let rt = tokio::runtime::Runtime::new().unwrap();

    rt.block_on(async {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(query_param("limit", "500"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "m1",
                    "question": "Will this test pass?",
                    "active": true,
                    "volume": "782375.55"
                },
                {
                    "id": "m2",
                    "question": "Will this stay deterministic?",
                    "active": true,
                    "volume": "12.25"
                }
            ])))
            .mount(&server)
            .await;

        let adapter = Arc::new(PolymarketAdapter::new(&server.uri(), &server.uri()).unwrap());
        let conn = Connection::open_in_memory().unwrap();
        let bridge = Arc::new(IoBridge::new());
        register_tables(&conn, adapter, bridge).unwrap();

        let rows = tokio::task::spawn_blocking(move || {
            let mut stmt = conn
                .prepare("SELECT id, volume FROM polymarket_markets()")
                .unwrap();
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<f64>>(1)?))
                })
                .unwrap();

            rows.collect::<duckdb::Result<Vec<_>>>().unwrap()
        })
        .await
        .unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "m1");
        let volume = rows[0].1.expect("volume should be non-null");
        assert!((volume - 782_375.55).abs() < 0.000_01);
    });
}

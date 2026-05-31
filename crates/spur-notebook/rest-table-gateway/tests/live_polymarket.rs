use std::error::Error;

use arrow_array::{
    Array, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray, UInt64Array,
};
use arrow_schema::DataType;
use reqwest::Client;
use serde_json::Value;
use spur_rest_table_gateway::adapter::{
    Adapter, Predicate, PredicateOp, ResolvedAuth, ScalarValue, ScanRequest,
};
use spur_rest_table_gateway::adapters::polymarket::PolymarketAdapter;

const GAMMA_BASE: &str = "https://gamma-api.polymarket.com";
const CLOB_BASE: &str = "https://clob.polymarket.com";

fn scan_request(
    table: &str,
    predicates: Vec<Predicate>,
    tvf_args: Vec<ScalarValue>,
) -> ScanRequest {
    ScanRequest {
        table: table.to_string(),
        predicates,
        projection: None,
        tvf_args,
        auth: ResolvedAuth::None,
    }
}

fn active_predicate() -> Predicate {
    Predicate {
        column: "active".to_string(),
        op: PredicateOp::Eq,
        value: ScalarValue::Bool(true),
    }
}

fn array_value_to_string(array: &dyn Array, row: usize) -> String {
    if array.is_null(row) {
        return "NULL".to_string();
    }

    match array.data_type() {
        DataType::Utf8 => array
            .as_any()
            .downcast_ref::<StringArray>()
            .map(|values| values.value(row).to_string())
            .unwrap_or_else(|| "<invalid Utf8 array>".to_string()),
        DataType::Boolean => array
            .as_any()
            .downcast_ref::<BooleanArray>()
            .map(|values| values.value(row).to_string())
            .unwrap_or_else(|| "<invalid Boolean array>".to_string()),
        DataType::Float64 => array
            .as_any()
            .downcast_ref::<Float64Array>()
            .map(|values| values.value(row).to_string())
            .unwrap_or_else(|| "<invalid Float64 array>".to_string()),
        DataType::Int64 => array
            .as_any()
            .downcast_ref::<Int64Array>()
            .map(|values| values.value(row).to_string())
            .unwrap_or_else(|| "<invalid Int64 array>".to_string()),
        DataType::UInt64 => array
            .as_any()
            .downcast_ref::<UInt64Array>()
            .map(|values| values.value(row).to_string())
            .unwrap_or_else(|| "<invalid UInt64 array>".to_string()),
        other => format!("<unsupported {other:?}>"),
    }
}

fn print_first_row(batch: &RecordBatch) {
    println!("schema:\n{:#?}", batch.schema());
    println!("first row:");
    for (column_index, field) in batch.schema().fields().iter().enumerate() {
        println!(
            "  {} = {}",
            field.name(),
            array_value_to_string(batch.column(column_index).as_ref(), 0)
        );
    }
}

async fn fetch_markets(
    client: &Client,
    params: &[(&str, &str)],
) -> Result<Vec<Value>, Box<dyn Error>> {
    let markets: Value = client
        .get(format!("{GAMMA_BASE}/markets"))
        .query(params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    markets
        .as_array()
        .cloned()
        .ok_or_else(|| "expected /markets to return a JSON array".into())
}

async fn fetch_one_market(client: &Client) -> Result<Value, Box<dyn Error>> {
    let markets = fetch_markets(
        client,
        &[
            ("limit", "1"),
            ("offset", "0"),
            ("active", "true"),
            ("closed", "false"),
        ],
    )
    .await?;

    let market = markets
        .first()
        .cloned()
        .ok_or("expected at least one live Polymarket market")?;

    Ok(market)
}

fn extract_token_id(market: &Value) -> Result<String, Box<dyn Error>> {
    let clob_token_ids = market
        .get("clobTokenIds")
        .ok_or("market is missing clobTokenIds")?;

    match clob_token_ids {
        Value::Array(ids) => ids
            .first()
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| "clobTokenIds array did not contain a string token id".into()),
        Value::String(raw) => serde_json::from_str::<Vec<String>>(raw)
            .ok()
            .and_then(|ids| ids.into_iter().next())
            .ok_or_else(|| "clobTokenIds string was not a JSON string array".into()),
        other => Err(format!("unsupported clobTokenIds shape: {other:?}").into()),
    }
}

#[tokio::test]
#[ignore = "hits live Polymarket Gamma API"]
async fn live_markets_scan_prints_schema_and_first_row() -> Result<(), Box<dyn Error>> {
    let adapter = PolymarketAdapter::new(GAMMA_BASE, CLOB_BASE)?;
    let client = Client::new();

    let raw_market = fetch_one_market(&client).await?;
    println!(
        "RAW /markets[0]:\n{}",
        serde_json::to_string_pretty(&raw_market)?
    );
    assert_eq!(
        raw_market.get("active").and_then(Value::as_bool),
        Some(true),
        "active=true should return an active market"
    );
    assert_eq!(
        raw_market.get("closed").and_then(Value::as_bool),
        Some(false),
        "closed=false should return an open market"
    );

    let inactive_markets = fetch_markets(
        &client,
        &[("limit", "1"), ("offset", "0"), ("active", "false")],
    )
    .await?;
    let inactive_active = inactive_markets
        .first()
        .and_then(|market| market.get("active"))
        .and_then(Value::as_bool);
    println!("active=false first row active field: {inactive_active:?}");

    let offset_one = fetch_markets(
        &client,
        &[
            ("limit", "1"),
            ("offset", "1"),
            ("active", "true"),
            ("closed", "false"),
        ],
    )
    .await?;
    let offset_zero_id = raw_market.get("id").and_then(Value::as_str);
    let offset_one_id = offset_one
        .first()
        .and_then(|market| market.get("id"))
        .and_then(Value::as_str);
    println!("pagination id offset=0: {offset_zero_id:?}");
    println!("pagination id offset=1: {offset_one_id:?}");
    assert_ne!(
        offset_zero_id, offset_one_id,
        "offset=1 should return a different first market than offset=0"
    );

    let batches = adapter
        .scan(scan_request("markets", vec![active_predicate()], vec![]))
        .await?;

    assert!(!batches.is_empty(), "markets scan returned no batches");
    assert!(
        batches[0].num_rows() > 0,
        "markets scan returned an empty first batch"
    );
    print_first_row(&batches[0]);

    Ok(())
}

#[tokio::test]
#[ignore = "hits live Polymarket Gamma and CLOB APIs"]
async fn live_orderbook_tvf_prints_raw_book_and_rows() -> Result<(), Box<dyn Error>> {
    let adapter = PolymarketAdapter::new(GAMMA_BASE, CLOB_BASE)?;
    let client = Client::new();

    let raw_market = fetch_one_market(&client).await?;
    let token_id = extract_token_id(&raw_market)?;
    println!("token_id from clobTokenIds: {token_id}");
    println!(
        "RAW /markets[0] clobTokenIds: {}",
        raw_market
            .get("clobTokenIds")
            .map(Value::to_string)
            .unwrap_or_else(|| "null".to_string())
    );

    let raw_book: Value = client
        .get(format!("{CLOB_BASE}/book"))
        .query(&[("token_id", token_id.as_str())])
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    println!(
        "RAW /book response:\n{}",
        serde_json::to_string_pretty(&raw_book)?
    );

    let batches = adapter
        .scan(scan_request(
            "orderbook",
            vec![],
            vec![ScalarValue::Utf8(token_id), ScalarValue::Int64(10)],
        ))
        .await?;

    assert!(!batches.is_empty(), "orderbook scan returned no batches");
    let batch = &batches[0];
    println!("orderbook schema:\n{:#?}", batch.schema());
    println!("orderbook rows:");
    for row in 0..batch.num_rows() {
        let price = array_value_to_string(batch.column(0).as_ref(), row);
        let size = array_value_to_string(batch.column(1).as_ref(), row);
        println!("  row {row}: price={price}, size={size}");
    }

    Ok(())
}

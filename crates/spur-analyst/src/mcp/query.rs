use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use duckdb::arrow::array::{
    BooleanArray, Date32Array, Date64Array, Float32Array, Float64Array, Int16Array, Int32Array,
    Int64Array, Int8Array, LargeStringArray, StringArray, Time32MillisecondArray,
    Time32SecondArray, Time64MicrosecondArray, Time64NanosecondArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray, UInt16Array,
    UInt32Array, UInt64Array, UInt8Array,
};
use duckdb::arrow::datatypes::DataType;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::open_analyst_connection_read_only;

use super::knowledge_context::analyst_db_path;
use super::McpHandlerError;

const MAX_QUERY_ROWS: usize = 1000;
const STALE_ANALYST_DB_MESSAGE: &str =
    "The analyst DB lags the live graph. Run `spur graph build` to refresh, or set allow_stale=true to override.";
static LANCE_INSTALLED: OnceLock<()> = OnceLock::new();
static DUCKPGQ_INSTALLED: OnceLock<()> = OnceLock::new();

pub async fn query(args: &Value) -> Result<Value, McpHandlerError> {
    let request = QueryRequest::parse(args)?;
    reject_write_statement(&request.query)?;
    let db_path = analyst_db_path()?;
    match query_read_only(&db_path, &request.query, request.allow_stale) {
        Ok(result) => Ok(result),
        Err(error) => Ok(query_error(&db_path, &error)),
    }
}

struct QueryRequest {
    query: String,
    allow_stale: bool,
}

impl QueryRequest {
    fn parse(args: &Value) -> Result<Self, McpHandlerError> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| {
                McpHandlerError::InvalidParams(
                    "query requires non-empty string field 'query'".into(),
                )
            })?
            .to_owned();
        let allow_stale = args
            .get("allow_stale")
            .map(|value| {
                value.as_bool().ok_or_else(|| {
                    McpHandlerError::InvalidParams(
                        "query field 'allow_stale' must be a boolean".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(false);
        Ok(Self { query, allow_stale })
    }
}

fn reject_write_statement(query: &str) -> Result<(), McpHandlerError> {
    let token = first_token(query).to_ascii_uppercase();
    if matches!(
        token.as_str(),
        "INSERT"
            | "UPDATE"
            | "DELETE"
            | "CREATE"
            | "ALTER"
            | "DROP"
            | "ATTACH"
            | "DETACH"
            | "COPY"
            | "PRAGMA"
            | "CALL"
            | "EXPORT"
            | "BEGIN"
            | "COMMIT"
            | "ROLLBACK"
            | "CHECKPOINT"
            | "VACUUM"
            | "REVOKE"
            | "GRANT"
            | "SET"
    ) {
        return Err(McpHandlerError::InvalidParams(format!(
            "query is read-only; statement token {token} is not allowed"
        )));
    }
    Ok(())
}

fn first_token(query: &str) -> &str {
    let trimmed = query.trim_start();
    let end = trimmed
        .find(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .unwrap_or(trimmed.len());
    &trimmed[..end]
}

fn query_read_only(db_path: &Path, sql: &str, allow_stale: bool) -> Result<Value, McpHandlerError> {
    let conn = open_analyst_connection_read_only(db_path)
        .map_err(|error| McpHandlerError::Internal(format!("{error:#}")))?;

    let staleness_warning = match freshness_gate(&conn, db_path, allow_stale)? {
        FreshnessGate::Proceed { warning } => warning,
        FreshnessGate::Block(response) => return Ok(response),
    };

    let _ = conn.execute_batch("INSTALL icu; LOAD icu;");
    LANCE_INSTALLED.get_or_init(|| {
        let _ = conn.execute_batch("INSTALL lance;");
    });
    let _ = conn.execute_batch("LOAD lance;");
    DUCKPGQ_INSTALLED.get_or_init(|| {
        let _ = conn.execute_batch("INSTALL duckpgq FROM community;");
    });
    let _ = conn.execute_batch("LOAD duckpgq;");

    let mut stmt = conn.prepare(sql).map_err(|error| {
        McpHandlerError::Internal(format!("failed to prepare DuckDB query: {error}"))
    })?;
    let mut reader = stmt.query_arrow([]).map_err(|error| {
        McpHandlerError::Internal(format!("failed to execute DuckDB query: {error}"))
    })?;
    let schema = reader.get_schema();
    let columns = schema
        .fields()
        .iter()
        .map(|field| field.name().to_owned())
        .collect::<Vec<_>>();

    let mut rows = Vec::new();
    let mut truncated = false;
    'batches: for batch in &mut reader {
        for row in 0..batch.num_rows() {
            if rows.len() == MAX_QUERY_ROWS {
                truncated = true;
                break 'batches;
            }
            let mut values = Vec::with_capacity(batch.num_columns());
            for column in batch.columns() {
                values.push(arrow_value(column.as_ref(), row)?);
            }
            rows.push(Value::Array(values));
        }
    }
    let row_count = rows.len();

    let mut response = json!({
        "db_path": db_path.display().to_string(),
        "columns": columns,
        "rows": rows,
        "row_count": row_count,
        "truncated": truncated
    });
    if let Some(warning) = staleness_warning {
        response["staleness_warning"] = Value::String(warning);
    }
    Ok(response)
}

enum FreshnessGate {
    Proceed { warning: Option<String> },
    Block(Value),
}

fn freshness_gate(
    conn: &duckdb::Connection,
    db_path: &Path,
    allow_stale: bool,
) -> Result<FreshnessGate, McpHandlerError> {
    if allow_stale {
        return Ok(FreshnessGate::Proceed {
            warning: Some("allow_stale".into()),
        });
    }

    let Some(live_hash) = read_live_graph_hash(db_path)? else {
        return Ok(FreshnessGate::Proceed {
            warning: Some("no_live_pointer".into()),
        });
    };
    let analyst_hash = query_analyst_graph_hash(conn)?;
    if analyst_hash.as_deref() == Some(live_hash.as_str()) {
        return Ok(FreshnessGate::Proceed { warning: None });
    }

    Ok(FreshnessGate::Block(json!({
        "error": "analyst_db_stale",
        "analyst_hash": analyst_hash,
        "live_hash": live_hash,
        "message": STALE_ANALYST_DB_MESSAGE
    })))
}

fn query_analyst_graph_hash(conn: &duckdb::Connection) -> Result<Option<String>, McpHandlerError> {
    let mut stmt = conn
        .prepare("SELECT graph_content_hash FROM _meta LIMIT 1")
        .map_err(|error| {
            McpHandlerError::Internal(format!(
                "failed to prepare analyst freshness query: {error}"
            ))
        })?;
    let mut rows = stmt.query([]).map_err(|error| {
        McpHandlerError::Internal(format!("failed to query analyst freshness: {error}"))
    })?;
    let Some(row) = rows.next().map_err(|error| {
        McpHandlerError::Internal(format!("failed to read analyst freshness row: {error}"))
    })?
    else {
        return Ok(None);
    };
    row.get(0).map(Some).map_err(|error| {
        McpHandlerError::Internal(format!(
            "failed to read analyst graph_content_hash: {error}"
        ))
    })
}

fn read_live_graph_hash(db_path: &Path) -> Result<Option<String>, McpHandlerError> {
    let Some(spur_dir) = db_path.parent() else {
        return Ok(None);
    };
    for pointer_path in live_pointer_paths(spur_dir) {
        match fs::read(&pointer_path) {
            Ok(bytes) => {
                let pointer: LiveGraphPointer =
                    serde_json::from_slice(&bytes).map_err(|error| {
                        McpHandlerError::Internal(format!(
                            "invalid live graph pointer `{}`: {error}",
                            pointer_path.display()
                        ))
                    })?;
                return Ok(Some(pointer.graph_content_hash));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(McpHandlerError::Internal(format!(
                    "failed to read live graph pointer `{}`: {error}",
                    pointer_path.display()
                )));
            }
        }
    }
    Ok(None)
}

fn live_pointer_paths(spur_dir: &Path) -> [PathBuf; 2] {
    [
        spur_dir.join("graph").join("pointer.json"),
        spur_dir.join("graph-index.pointer.json"),
    ]
}

#[derive(Deserialize)]
struct LiveGraphPointer {
    graph_content_hash: String,
}

fn arrow_value(
    array: &dyn duckdb::arrow::array::Array,
    row: usize,
) -> Result<Value, McpHandlerError> {
    if array.is_null(row) {
        return Ok(Value::Null);
    }
    match array.data_type() {
        DataType::Utf8 => string_value::<StringArray, _>(array, row, |array, row| array.value(row)),
        DataType::LargeUtf8 => {
            string_value::<LargeStringArray, _>(array, row, |array, row| array.value(row))
        }
        DataType::Boolean => primitive_value::<BooleanArray, _>(array, row, Value::Bool),
        DataType::Int8 => primitive_value::<Int8Array, _>(array, row, |value| json!(value as i64)),
        DataType::Int16 => {
            primitive_value::<Int16Array, _>(array, row, |value| json!(value as i64))
        }
        DataType::Int32 => {
            primitive_value::<Int32Array, _>(array, row, |value| json!(value as i64))
        }
        DataType::Int64 => primitive_value::<Int64Array, _>(array, row, |value| json!(value)),
        DataType::UInt8 => {
            primitive_value::<UInt8Array, _>(array, row, |value| json!(value as u64))
        }
        DataType::UInt16 => {
            primitive_value::<UInt16Array, _>(array, row, |value| json!(value as u64))
        }
        DataType::UInt32 => {
            primitive_value::<UInt32Array, _>(array, row, |value| json!(value as u64))
        }
        DataType::UInt64 => primitive_value::<UInt64Array, _>(array, row, |value| json!(value)),
        DataType::Float32 => {
            primitive_value::<Float32Array, _>(array, row, |value| json!(value as f64))
        }
        DataType::Float64 => primitive_value::<Float64Array, _>(array, row, |value| json!(value)),
        DataType::Date32 => temporal_value::<Date32Array, _>(array, row, |array, row| {
            array.value_as_date(row).map(|value| value.to_string())
        }),
        DataType::Date64 => temporal_value::<Date64Array, _>(array, row, |array, row| {
            array.value_as_date(row).map(|value| value.to_string())
        }),
        DataType::Time32(_) => time32_value(array, row),
        DataType::Time64(_) => time64_value(array, row),
        DataType::Timestamp(_, _) => timestamp_value(array, row),
        DataType::Null => Ok(Value::Null),
        _ => Ok(Value::String(format!("{:?}", array.slice(row, 1)))),
    }
}

fn string_value<T, F>(
    array: &dyn duckdb::arrow::array::Array,
    row: usize,
    value: F,
) -> Result<Value, McpHandlerError>
where
    T: duckdb::arrow::array::Array + 'static,
    F: for<'a> FnOnce(&'a T, usize) -> &'a str,
{
    let array = array.as_any().downcast_ref::<T>().ok_or_else(|| {
        McpHandlerError::Internal(format!(
            "DuckDB Arrow column type mismatch for {:?}",
            array.data_type()
        ))
    })?;
    Ok(Value::String(value(array, row).to_owned()))
}

fn primitive_value<T, F>(
    array: &dyn duckdb::arrow::array::Array,
    row: usize,
    map: F,
) -> Result<Value, McpHandlerError>
where
    T: duckdb::arrow::array::Array + 'static,
    F: FnOnce(<T as PrimitiveValueAt>::Value) -> Value,
    T: PrimitiveValueAt,
{
    let array = array.as_any().downcast_ref::<T>().ok_or_else(|| {
        McpHandlerError::Internal(format!(
            "DuckDB Arrow column type mismatch for {:?}",
            array.data_type()
        ))
    })?;
    Ok(map(array.primitive_value(row)))
}

trait PrimitiveValueAt {
    type Value;

    fn primitive_value(&self, row: usize) -> Self::Value;
}

macro_rules! impl_primitive_value_at {
    ($array:ty, $value:ty) => {
        impl PrimitiveValueAt for $array {
            type Value = $value;

            fn primitive_value(&self, row: usize) -> Self::Value {
                self.value(row)
            }
        }
    };
}

impl_primitive_value_at!(BooleanArray, bool);
impl_primitive_value_at!(Int8Array, i8);
impl_primitive_value_at!(Int16Array, i16);
impl_primitive_value_at!(Int32Array, i32);
impl_primitive_value_at!(Int64Array, i64);
impl_primitive_value_at!(UInt8Array, u8);
impl_primitive_value_at!(UInt16Array, u16);
impl_primitive_value_at!(UInt32Array, u32);
impl_primitive_value_at!(UInt64Array, u64);
impl_primitive_value_at!(Float32Array, f32);
impl_primitive_value_at!(Float64Array, f64);

fn temporal_value<T, F>(
    array: &dyn duckdb::arrow::array::Array,
    row: usize,
    map: F,
) -> Result<Value, McpHandlerError>
where
    T: duckdb::arrow::array::Array + 'static,
    F: FnOnce(&T, usize) -> Option<String>,
{
    let array = array.as_any().downcast_ref::<T>().ok_or_else(|| {
        McpHandlerError::Internal(format!(
            "DuckDB Arrow column type mismatch for {:?}",
            array.data_type()
        ))
    })?;
    Ok(map(array, row)
        .map(Value::String)
        .unwrap_or_else(|| Value::String(format!("{:?}", array.slice(row, 1)))))
}

fn time32_value(
    array: &dyn duckdb::arrow::array::Array,
    row: usize,
) -> Result<Value, McpHandlerError> {
    if let Some(array) = array.as_any().downcast_ref::<Time32SecondArray>() {
        return Ok(array
            .value_as_time(row)
            .map(|value| Value::String(value.to_string()))
            .unwrap_or_else(|| Value::String(array.value(row).to_string())));
    }
    temporal_value::<Time32MillisecondArray, _>(array, row, |array, row| {
        array.value_as_time(row).map(|value| value.to_string())
    })
}

fn time64_value(
    array: &dyn duckdb::arrow::array::Array,
    row: usize,
) -> Result<Value, McpHandlerError> {
    if let Some(array) = array.as_any().downcast_ref::<Time64MicrosecondArray>() {
        return Ok(array
            .value_as_time(row)
            .map(|value| Value::String(value.to_string()))
            .unwrap_or_else(|| Value::String(array.value(row).to_string())));
    }
    temporal_value::<Time64NanosecondArray, _>(array, row, |array, row| {
        array.value_as_time(row).map(|value| value.to_string())
    })
}

fn timestamp_value(
    array: &dyn duckdb::arrow::array::Array,
    row: usize,
) -> Result<Value, McpHandlerError> {
    if let Some(array) = array.as_any().downcast_ref::<TimestampSecondArray>() {
        return Ok(timestamp_array_value(array, row));
    }
    if let Some(array) = array.as_any().downcast_ref::<TimestampMillisecondArray>() {
        return Ok(timestamp_array_value(array, row));
    }
    if let Some(array) = array.as_any().downcast_ref::<TimestampMicrosecondArray>() {
        return Ok(timestamp_array_value(array, row));
    }
    Ok(timestamp_array_value(
        array
            .as_any()
            .downcast_ref::<TimestampNanosecondArray>()
            .ok_or_else(|| {
                McpHandlerError::Internal(format!(
                    "DuckDB Arrow column type mismatch for {:?}",
                    array.data_type()
                ))
            })?,
        row,
    ))
}

fn timestamp_array_value<T>(array: &T, row: usize) -> Value
where
    T: duckdb::arrow::array::Array + TimestampValueAt,
{
    array
        .timestamp_string(row)
        .map(Value::String)
        .unwrap_or_else(|| Value::String(format!("{:?}", array.slice(row, 1))))
}

trait TimestampValueAt {
    fn timestamp_string(&self, row: usize) -> Option<String>;
}

macro_rules! impl_timestamp_value_at {
    ($array:ty) => {
        impl TimestampValueAt for $array {
            fn timestamp_string(&self, row: usize) -> Option<String> {
                self.value_as_datetime(row).map(|value| value.to_string())
            }
        }
    };
}

impl_timestamp_value_at!(TimestampSecondArray);
impl_timestamp_value_at!(TimestampMillisecondArray);
impl_timestamp_value_at!(TimestampMicrosecondArray);
impl_timestamp_value_at!(TimestampNanosecondArray);

fn query_error(db_path: &Path, error: &McpHandlerError) -> Value {
    json!({
        "db_path": db_path.display().to_string(),
        "error": {
            "code": query_error_code(error),
            "message": error.to_string()
        }
    })
}

fn query_error_code(error: &McpHandlerError) -> &'static str {
    match error {
        McpHandlerError::InvalidParams(_) => "invalid_params",
        McpHandlerError::NotFound(_) => "not_found",
        McpHandlerError::Unauthorized(_) => "unauthorized",
        McpHandlerError::UpstreamPm(_) => "upstream_pm",
        McpHandlerError::Internal(_) => "internal",
    }
}

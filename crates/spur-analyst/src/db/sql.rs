#[cfg(test)]
use std::path::Path;

use duckdb::arrow::{
    array::{
        Array, BooleanArray, Date32Array, Date64Array, Float32Array, Float64Array, Int16Array,
        Int32Array, Int64Array, Int8Array, LargeStringArray, StringArray, Time32MillisecondArray,
        Time32SecondArray, Time64MicrosecondArray, Time64NanosecondArray,
        TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
        TimestampSecondArray, UInt16Array, UInt32Array, UInt64Array, UInt8Array,
    },
    datatypes::DataType,
};
use serde_json::{json, Value};

use crate::mcp::McpHandlerError;

pub(crate) const MAX_QUERY_ROWS: usize = 1000;

pub(crate) struct QueryRows {
    pub(crate) columns: Vec<String>,
    pub(crate) rows: Vec<Value>,
    pub(crate) truncated: bool,
}

impl QueryRows {
    pub(crate) fn row_count(&self) -> usize {
        self.rows.len()
    }
}

pub(crate) fn query_rows(
    conn: &duckdb::Connection,
    sql: &str,
    max_rows: usize,
) -> Result<QueryRows, McpHandlerError> {
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
            if rows.len() == max_rows {
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

    Ok(QueryRows {
        columns,
        rows,
        truncated,
    })
}

pub(crate) fn sql_escape_literal(value: &str) -> String {
    value.replace('\'', "''")
}

pub(crate) fn sql_string_literal(value: &str) -> String {
    format!("'{}'", sql_escape_literal(value))
}

#[cfg(test)]
pub(crate) fn sql_escape_path(path: &Path) -> String {
    sql_escape_literal(&path.display().to_string())
}

fn arrow_value(array: &dyn Array, row: usize) -> Result<Value, McpHandlerError> {
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

fn string_value<T, F>(array: &dyn Array, row: usize, value: F) -> Result<Value, McpHandlerError>
where
    T: Array + 'static,
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

fn primitive_value<T, F>(array: &dyn Array, row: usize, map: F) -> Result<Value, McpHandlerError>
where
    T: Array + PrimitiveValueAt + 'static,
    F: FnOnce(<T as PrimitiveValueAt>::Value) -> Value,
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

fn temporal_value<T, F>(array: &dyn Array, row: usize, map: F) -> Result<Value, McpHandlerError>
where
    T: Array + 'static,
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

fn time32_value(array: &dyn Array, row: usize) -> Result<Value, McpHandlerError> {
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

fn time64_value(array: &dyn Array, row: usize) -> Result<Value, McpHandlerError> {
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

fn timestamp_value(array: &dyn Array, row: usize) -> Result<Value, McpHandlerError> {
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
    T: Array + TimestampValueAt,
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn db_sql_string_literal_wraps_and_escapes_quotes() {
        assert_eq!(super::sql_string_literal("O'Malley"), "'O''Malley'");
    }

    #[test]
    fn query_rows_returns_columns_and_caps_rows() {
        let conn = duckdb::Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            r#"
            CREATE TABLE many AS
            SELECT range AS value FROM range(3);
            "#,
        )
        .expect("seed db");

        let result =
            super::query_rows(&conn, "SELECT value FROM many ORDER BY value", 2).expect("query");

        assert_eq!(result.columns, vec!["value"]);
        assert_eq!(result.rows, vec![json!([0]), json!([1])]);
        assert!(result.truncated);
        assert_eq!(result.row_count(), 2);
    }
}

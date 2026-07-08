#[cfg(test)]
use std::path::Path;

use duckdb::arrow::{
    array::{
        Array, BinaryArray, BooleanArray, Date32Array, Date64Array, Decimal128Array,
        Decimal256Array, Decimal32Array, Decimal64Array, FixedSizeBinaryArray, FixedSizeListArray,
        Float32Array, Float64Array, Int16Array, Int32Array, Int64Array, Int8Array,
        LargeBinaryArray, LargeListArray, LargeStringArray, ListArray, MapArray, StringArray,
        StructArray, Time32MillisecondArray, Time32SecondArray, Time64MicrosecondArray,
        Time64NanosecondArray, TimestampMicrosecondArray, TimestampMillisecondArray,
        TimestampNanosecondArray, TimestampSecondArray, UInt16Array, UInt32Array, UInt64Array,
        UInt8Array,
    },
    datatypes::DataType,
};
use serde_json::{json, Map, Value};

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
        DataType::Decimal32(_, scale) => decimal_value::<Decimal32Array, _>(array, row, *scale),
        DataType::Decimal64(_, scale) => decimal_value::<Decimal64Array, _>(array, row, *scale),
        DataType::Decimal128(_, scale) => decimal_value::<Decimal128Array, _>(array, row, *scale),
        DataType::Decimal256(_, scale) => decimal_value::<Decimal256Array, _>(array, row, *scale),
        DataType::List(_) => list_value::<ListArray>(array, row),
        DataType::LargeList(_) => list_value::<LargeListArray>(array, row),
        DataType::FixedSizeList(_, _) => list_value::<FixedSizeListArray>(array, row),
        DataType::Struct(_) => struct_value(array, row),
        DataType::Map(_, _) => map_value(array, row),
        DataType::Binary => {
            binary_value::<BinaryArray, _>(array, row, |array, row| array.value(row))
        }
        DataType::LargeBinary => {
            binary_value::<LargeBinaryArray, _>(array, row, |array, row| array.value(row))
        }
        DataType::FixedSizeBinary(_) => {
            binary_value::<FixedSizeBinaryArray, _>(array, row, |array, row| array.value(row))
        }
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
        _ => Ok(Value::String(format!(
            "<unsupported Arrow type {}>",
            array.data_type()
        ))),
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

fn decimal_value<T, V>(array: &dyn Array, row: usize, scale: i8) -> Result<Value, McpHandlerError>
where
    T: Array + PrimitiveValueAt<Value = V> + 'static,
    V: ToString,
{
    let array = array.as_any().downcast_ref::<T>().ok_or_else(|| {
        McpHandlerError::Internal(format!(
            "DuckDB Arrow column type mismatch for {:?}",
            array.data_type()
        ))
    })?;
    Ok(Value::String(format_decimal(
        array.primitive_value(row),
        scale,
    )))
}

impl_primitive_value_at!(Decimal32Array, i32);
impl_primitive_value_at!(Decimal64Array, i64);
impl_primitive_value_at!(Decimal128Array, i128);
impl_primitive_value_at!(Decimal256Array, duckdb::arrow::datatypes::i256);

fn format_decimal<V: ToString>(value: V, scale: i8) -> String {
    let mut digits = value.to_string();
    let is_negative = digits.strip_prefix('-').is_some();
    if is_negative {
        digits.remove(0);
    }

    if scale < 0 {
        digits.push_str(&"0".repeat(scale.unsigned_abs() as usize));
    } else if scale > 0 {
        let scale = scale as usize;
        if digits.len() <= scale {
            digits.insert_str(0, &"0".repeat(scale + 1 - digits.len()));
        }
        let point = digits.len() - scale;
        digits.insert(point, '.');
    }

    if is_negative {
        digits.insert(0, '-');
    }
    digits
}

trait ListValueAt {
    fn list_value(&self, row: usize) -> duckdb::arrow::array::ArrayRef;
}

impl ListValueAt for ListArray {
    fn list_value(&self, row: usize) -> duckdb::arrow::array::ArrayRef {
        self.value(row)
    }
}

impl ListValueAt for LargeListArray {
    fn list_value(&self, row: usize) -> duckdb::arrow::array::ArrayRef {
        self.value(row)
    }
}

impl ListValueAt for FixedSizeListArray {
    fn list_value(&self, row: usize) -> duckdb::arrow::array::ArrayRef {
        self.value(row)
    }
}

fn list_value<T>(array: &dyn Array, row: usize) -> Result<Value, McpHandlerError>
where
    T: Array + ListValueAt + 'static,
{
    let array = array.as_any().downcast_ref::<T>().ok_or_else(|| {
        McpHandlerError::Internal(format!(
            "DuckDB Arrow column type mismatch for {:?}",
            array.data_type()
        ))
    })?;
    let values = array.list_value(row);
    let mut json_values = Vec::with_capacity(values.len());
    for value_row in 0..values.len() {
        json_values.push(arrow_value(values.as_ref(), value_row)?);
    }
    Ok(Value::Array(json_values))
}

fn struct_value(array: &dyn Array, row: usize) -> Result<Value, McpHandlerError> {
    let array = array
        .as_any()
        .downcast_ref::<StructArray>()
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "DuckDB Arrow column type mismatch for {:?}",
                array.data_type()
            ))
        })?;
    let mut object = Map::new();
    for (field, column) in array.fields().iter().zip(array.columns()) {
        object.insert(field.name().to_owned(), arrow_value(column.as_ref(), row)?);
    }
    Ok(Value::Object(object))
}

fn map_value(array: &dyn Array, row: usize) -> Result<Value, McpHandlerError> {
    let array = array.as_any().downcast_ref::<MapArray>().ok_or_else(|| {
        McpHandlerError::Internal(format!(
            "DuckDB Arrow column type mismatch for {:?}",
            array.data_type()
        ))
    })?;
    let entries = array.value(row);
    let keys = entries.column(0);
    let values = entries.column(1);
    let mut object = Map::new();
    for entry_row in 0..entries.len() {
        let key = json_key(arrow_value(keys.as_ref(), entry_row)?);
        object.insert(key, arrow_value(values.as_ref(), entry_row)?);
    }
    Ok(Value::Object(object))
}

fn json_key(value: Value) -> String {
    match value {
        Value::String(value) => value,
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

fn binary_value<T, F>(array: &dyn Array, row: usize, value: F) -> Result<Value, McpHandlerError>
where
    T: Array + 'static,
    F: for<'a> FnOnce(&'a T, usize) -> &'a [u8],
{
    let array = array.as_any().downcast_ref::<T>().ok_or_else(|| {
        McpHandlerError::Internal(format!(
            "DuckDB Arrow column type mismatch for {:?}",
            array.data_type()
        ))
    })?;
    Ok(Value::String(hex_string(value(array, row))))
}

fn hex_string(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

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

    #[test]
    fn query_rows_serializes_decimal_as_json_string() {
        let conn = duckdb::Connection::open_in_memory().expect("open in-memory db");

        let result =
            super::query_rows(&conn, "SELECT 12.34::DECIMAL(10, 2) AS amount", 10).expect("query");

        assert_eq!(result.rows, vec![json!(["12.34"])]);
    }

    #[test]
    fn query_rows_serializes_list_as_json_array() {
        let conn = duckdb::Connection::open_in_memory().expect("open in-memory db");

        let result =
            super::query_rows(&conn, "SELECT [1, 2, 3]::INTEGER[] AS node_ids", 10).expect("query");

        assert_eq!(result.rows, vec![json!([[1, 2, 3]])]);
    }

    #[test]
    fn query_rows_serializes_struct_as_json_object() {
        let conn = duckdb::Connection::open_in_memory().expect("open in-memory db");

        let result = super::query_rows(
            &conn,
            "SELECT struct_pack(name := 'alpha', count := 2) AS node",
            10,
        )
        .expect("query");

        assert_eq!(result.rows, vec![json!([{"name": "alpha", "count": 2}])]);
    }

    #[test]
    fn query_rows_serializes_map_as_json_object() {
        let conn = duckdb::Connection::open_in_memory().expect("open in-memory db");

        let result = super::query_rows(
            &conn,
            "SELECT map(['alpha', 'beta'], [1, 2]) AS weights",
            10,
        )
        .expect("query");

        assert_eq!(result.rows, vec![json!([{"alpha": 1, "beta": 2}])]);
    }
}

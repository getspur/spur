use std::sync::Arc;

use arrow_array::{ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use serde_json::Value;

use crate::error::{GatewayError, Result};

/// Map a manifest type string to an Arrow DataType. v1 set only.
pub fn arrow_type(ty: &str) -> Result<DataType> {
    Ok(match ty {
        "Utf8" => DataType::Utf8,
        "Int64" => DataType::Int64,
        "Float64" => DataType::Float64,
        "Boolean" => DataType::Boolean,
        other => {
            return Err(GatewayError::Schema(format!(
                "unsupported column type {other}"
            )));
        }
    })
}

/// Resolve a simple `$.a.b` dotted JSON path against a row. Returns None when absent/null.
pub fn json_path_get<'a>(row: &'a Value, path: &str) -> Option<&'a Value> {
    let p = path.strip_prefix("$.").unwrap_or(path);
    let mut cur = row;
    for seg in p.split('.') {
        cur = cur.get(seg)?;
    }
    if cur.is_null() {
        None
    } else {
        Some(cur)
    }
}

/// A column spec: output field + JSON path into each row.
pub struct ColumnExtract {
    pub name: String,
    pub data_type: DataType,
    pub json_path: String,
}

pub fn rows_to_batch(cols: &[ColumnExtract], rows: &[Value]) -> Result<RecordBatch> {
    let fields: Vec<Field> = cols
        .iter()
        .map(|c| Field::new(&c.name, c.data_type.clone(), true))
        .collect();
    let schema: SchemaRef = Arc::new(Schema::new(fields));
    let mut arrays: Vec<ArrayRef> = Vec::with_capacity(cols.len());

    for c in cols {
        let arr: ArrayRef = match &c.data_type {
            DataType::Utf8 => Arc::new(
                rows.iter()
                    .map(|r| {
                        json_path_get(r, &c.json_path).and_then(|v| {
                            v.as_str()
                                .map(|s| s.to_string())
                                .or_else(|| Some(v.to_string()))
                        })
                    })
                    .collect::<StringArray>(),
            ),
            DataType::Int64 => Arc::new(
                rows.iter()
                    .map(|r| json_path_get(r, &c.json_path).and_then(|v| v.as_i64()))
                    .collect::<Int64Array>(),
            ),
            DataType::Float64 => Arc::new(
                rows.iter()
                    .map(|r| json_path_get(r, &c.json_path).and_then(|v| v.as_f64()))
                    .collect::<Float64Array>(),
            ),
            DataType::Boolean => Arc::new(
                rows.iter()
                    .map(|r| json_path_get(r, &c.json_path).and_then(|v| v.as_bool()))
                    .collect::<BooleanArray>(),
            ),
            dt => return Err(GatewayError::Schema(format!("unsupported {dt:?}"))),
        };
        arrays.push(arr);
    }

    RecordBatch::try_new(schema, arrays).map_err(|e| GatewayError::Schema(e.to_string()))
}

#[cfg(test)]
mod tests {
    use arrow_array::{Array, BooleanArray, Float64Array};
    use arrow_schema::DataType;
    use serde_json::json;

    use super::{rows_to_batch, ColumnExtract};

    #[test]
    fn builds_typed_batch() {
        let rows = vec![
            json!({"id":"m1","question":"Q?","active":true,"volume":12.5}),
            json!({"id":"m2","question":"Another?","active":false}),
        ];
        let cols = vec![
            ColumnExtract {
                name: "id".to_string(),
                data_type: DataType::Utf8,
                json_path: "$.id".to_string(),
            },
            ColumnExtract {
                name: "active".to_string(),
                data_type: DataType::Boolean,
                json_path: "$.active".to_string(),
            },
            ColumnExtract {
                name: "volume".to_string(),
                data_type: DataType::Float64,
                json_path: "$.volume".to_string(),
            },
        ];

        let batch = rows_to_batch(&cols, &rows).expect("batch");

        assert_eq!(batch.num_rows(), 2);
        assert_eq!(batch.num_columns(), 3);

        let volume = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("volume column");
        assert_eq!(volume.value(0), 12.5);
        assert!(volume.is_null(1));

        let active = batch
            .column(1)
            .as_any()
            .downcast_ref::<BooleanArray>()
            .expect("active column");
        assert!(active.value(0));
    }
}

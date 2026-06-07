use super::PortContext;
use crate::dag::ports::PortRead;

use std::fmt::Write as _;

use arrow_array::{
    Array, BooleanArray, Float32Array, Float64Array, Int16Array, Int32Array, Int64Array, Int8Array,
    LargeStringArray, StringArray, UInt16Array, UInt32Array, UInt64Array, UInt8Array,
};
use arrow_schema::DataType;

const MAX_ROWS: usize = 50;

/// Render an Arrow port to a compact, model-friendly text table:
/// a header line of column names, then up to `MAX_ROWS` comma-joined rows.
pub fn render_port_context(port: &str, read: &PortRead) -> PortContext {
    let PortRead::Arrow {
        schema, batches, ..
    } = read
    else {
        let PortRead::Media { mime, bytes, .. } = read else {
            unreachable!("PortRead variants are exhaustive")
        };
        return PortContext {
            port: port.to_owned(),
            rendered: format!("media,{mime},{} bytes\n", bytes.len()),
        };
    };

    let mut rendered = String::new();
    let columns: Vec<String> = schema
        .fields()
        .iter()
        .map(|field| field.name().clone())
        .collect();
    rendered.push_str(&columns.join(","));
    rendered.push('\n');

    let total_rows: usize = batches.iter().map(|batch| batch.num_rows()).sum();
    let mut emitted = 0usize;

    'batches: for batch in batches {
        for row in 0..batch.num_rows() {
            if emitted >= MAX_ROWS {
                break 'batches;
            }

            let cells: Vec<String> = batch
                .columns()
                .iter()
                .map(|column| array_value_to_string(column.as_ref(), row))
                .collect();
            rendered.push_str(&cells.join(","));
            rendered.push('\n');
            emitted += 1;
        }
    }

    if total_rows > emitted {
        let _ = writeln!(
            rendered,
            "... ({} more rows truncated)",
            total_rows - emitted
        );
    }

    PortContext {
        port: port.to_owned(),
        rendered,
    }
}

fn array_value_to_string(array: &dyn Array, row: usize) -> String {
    if array.is_null(row) {
        return "NULL".to_owned();
    }

    match array.data_type() {
        DataType::Utf8 => {
            downcast_value::<StringArray, _>(array, "Utf8", |values| values.value(row).to_owned())
        }
        DataType::LargeUtf8 => {
            downcast_value::<LargeStringArray, _>(array, "LargeUtf8", |values| {
                values.value(row).to_owned()
            })
        }
        DataType::Boolean => downcast_value::<BooleanArray, _>(array, "Boolean", |values| {
            values.value(row).to_string()
        }),
        DataType::Int8 => {
            downcast_value::<Int8Array, _>(array, "Int8", |values| values.value(row).to_string())
        }
        DataType::Int16 => {
            downcast_value::<Int16Array, _>(array, "Int16", |values| values.value(row).to_string())
        }
        DataType::Int32 => {
            downcast_value::<Int32Array, _>(array, "Int32", |values| values.value(row).to_string())
        }
        DataType::Int64 => {
            downcast_value::<Int64Array, _>(array, "Int64", |values| values.value(row).to_string())
        }
        DataType::UInt8 => {
            downcast_value::<UInt8Array, _>(array, "UInt8", |values| values.value(row).to_string())
        }
        DataType::UInt16 => downcast_value::<UInt16Array, _>(array, "UInt16", |values| {
            values.value(row).to_string()
        }),
        DataType::UInt32 => downcast_value::<UInt32Array, _>(array, "UInt32", |values| {
            values.value(row).to_string()
        }),
        DataType::UInt64 => downcast_value::<UInt64Array, _>(array, "UInt64", |values| {
            values.value(row).to_string()
        }),
        DataType::Float32 => downcast_value::<Float32Array, _>(array, "Float32", |values| {
            values.value(row).to_string()
        }),
        DataType::Float64 => downcast_value::<Float64Array, _>(array, "Float64", |values| {
            values.value(row).to_string()
        }),
        other => format!("<unsupported {other:?}>"),
    }
}

fn downcast_value<T, F>(array: &dyn Array, type_name: &str, render: F) -> String
where
    T: 'static,
    F: FnOnce(&T) -> String,
{
    array
        .as_any()
        .downcast_ref::<T>()
        .map(render)
        .unwrap_or_else(|| format!("<invalid {type_name} array>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{path::PathBuf, sync::Arc};

    use arrow_array::{Int64Array, RecordBatch, StringArray};
    use arrow_buffer::Buffer;
    use arrow_schema::{DataType, Field, Schema};

    fn sample_read() -> crate::dag::ports::PortRead {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(vec![1, 2])),
                Arc::new(StringArray::from(vec!["alice", "bob"])),
            ],
        )
        .expect("test batch is valid");

        crate::dag::ports::PortRead::Arrow {
            path: PathBuf::from("df.arrow"),
            version: 1,
            schema,
            batches: vec![batch],
            ipc_bytes: Buffer::from(Vec::<u8>::new()),
        }
    }

    #[test]
    fn renders_header_and_rows() {
        let read = sample_read();
        let ctx = render_port_context("df", &read);
        assert_eq!(ctx.port, "df");
        assert_eq!(ctx.rendered.lines().next().unwrap(), "id,name");
        assert!(ctx.rendered.contains("1,alice"));
        assert!(ctx.rendered.contains("2,bob"));
    }

    #[test]
    fn adds_truncation_note_after_max_rows() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, false),
        ]));
        let names: Vec<String> = (0..51).map(|id| format!("name-{id}")).collect();
        let name_refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from((0..51).collect::<Vec<i64>>())),
                Arc::new(StringArray::from(name_refs)),
            ],
        )
        .expect("test batch is valid");
        let read = crate::dag::ports::PortRead::Arrow {
            path: PathBuf::from("df.arrow"),
            version: 1,
            schema,
            batches: vec![batch],
            ipc_bytes: Buffer::from(Vec::<u8>::new()),
        };

        let ctx = render_port_context("df", &read);
        let lines: Vec<&str> = ctx.rendered.lines().collect();

        assert_eq!(lines.len(), 52);
        assert_eq!(lines[50], "49,name-49");
        assert_eq!(lines[51], "... (1 more rows truncated)");
        assert!(!ctx.rendered.contains("50,name-50"));
    }
}

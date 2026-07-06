use arrow_array::{
    Array as _, Float32Array, LargeStringArray, RecordBatch, StringArray, UInt32Array, UInt64Array,
    UInt8Array,
};
use serde_json::{json, Value};

use crate::mcp::McpHandlerError;

const LEDE_CHARS: usize = 200;

#[derive(Debug)]
pub(super) struct DocHit {
    pub(super) stable_symbol_id: String,
    qualified_name: String,
    pub(super) file_path: String,
    heading_level: u8,
    child_count: u32,
    score: Option<f32>,
    lede: Option<String>,
    pub(super) body_byte_start: u64,
}

impl DocHit {
    pub(super) fn into_value(self, include_lede: bool) -> Value {
        let mut value = json!({
            "stable_symbol_id": self.stable_symbol_id,
            "qualified_name": self.qualified_name,
            "file_path": self.file_path,
            "heading_level": self.heading_level,
            "child_count": self.child_count,
        });
        if let Some(score) = self.score {
            value["score"] = json!(score);
        }
        if include_lede {
            if let Some(lede) = self.lede {
                value["lede"] = json!(lede);
            }
        }
        value
    }
}

pub(super) fn project_batch(
    batch: &RecordBatch,
    include_score: bool,
) -> Result<Vec<DocHit>, McpHandlerError> {
    let stable_symbol_id = string_column(batch, "stable_symbol_id")?;
    let qualified_name = string_column(batch, "qualified_name")?;
    let file_path = string_column(batch, "file_path")?;
    let heading_level = u8_column(batch, "heading_level")?;
    let body_text = large_string_column(batch, "body_text")?;
    let body_byte_start = u64_column(batch, "body_byte_start")?;
    let child_count = u32_column(batch, "child_count")?;
    let score = if include_score {
        batch
            .column_by_name("_score")
            .and_then(|column| column.as_any().downcast_ref::<Float32Array>())
    } else {
        None
    };

    let mut hits = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        hits.push(DocHit {
            stable_symbol_id: stable_symbol_id.value(row).to_owned(),
            qualified_name: qualified_name.value(row).to_owned(),
            file_path: file_path.value(row).to_owned(),
            heading_level: heading_level.value(row),
            child_count: child_count.value(row),
            score: score.and_then(|scores| (!scores.is_null(row)).then(|| scores.value(row))),
            lede: Some(lede(body_text.value(row))),
            body_byte_start: body_byte_start.value(row),
        });
    }
    Ok(hits)
}

fn string_column<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a StringArray, McpHandlerError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<StringArray>())
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "Lance sections column `{name}` is missing or invalid"
            ))
        })
}

fn large_string_column<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a LargeStringArray, McpHandlerError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<LargeStringArray>())
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "Lance sections column `{name}` is missing or invalid"
            ))
        })
}

fn u8_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt8Array, McpHandlerError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<UInt8Array>())
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "Lance sections column `{name}` is missing or invalid"
            ))
        })
}

fn u32_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt32Array, McpHandlerError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<UInt32Array>())
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "Lance sections column `{name}` is missing or invalid"
            ))
        })
}

fn u64_column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a UInt64Array, McpHandlerError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<UInt64Array>())
        .ok_or_else(|| {
            McpHandlerError::Internal(format!(
                "Lance sections column `{name}` is missing or invalid"
            ))
        })
}

fn lede(body_text: &str) -> String {
    body_text.chars().take(LEDE_CHARS).collect()
}

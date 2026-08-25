use serde_json::{json, Value};

use crate::pack::doc_next_tools;

const LEDE_CHARS: usize = 200;

#[derive(Debug)]
pub(super) struct DocHit {
    pub(super) stable_symbol_id: String,
    pub(super) qualified_name: String,
    pub(super) file_path: String,
    pub(super) heading_level: u8,
    pub(super) child_count: u32,
    pub(super) score: Option<f32>,
    pub(super) lede: Option<String>,
}

impl DocHit {
    pub(super) fn into_value(self, include_lede: bool) -> Value {
        let next = doc_next_tools(&self.stable_symbol_id, Some(self.child_count));
        let mut value = json!({
            "stable_symbol_id": self.stable_symbol_id,
            "qualified_name": self.qualified_name,
            "file_path": self.file_path,
            "heading_level": self.heading_level,
            "child_count": self.child_count,
            "next": next,
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

pub(super) fn lede(body_text: &str) -> String {
    body_text.chars().take(LEDE_CHARS).collect()
}

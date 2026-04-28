//! Per-agent extractors that convert native storage formats into DuckDB-appendable rows.
//!
//! Currently houses Gemini's JSON-document parser. Future PRs may move
//! Kimi (JSONL pre/post pairing, currently inline in `engine.rs`) and
//! OpenCode (SQLite via rusqlite, currently inline) into this module.

use chrono::{DateTime, Utc};

#[cfg(feature = "duckdb")]
pub mod gemini;

/// Shape every extractor produces. Matches the per-agent table schema
/// in `engine.rs` so the appender call site is uniform.
#[derive(Debug, Clone)]
pub struct ExtractedRow {
    pub timestamp: DateTime<Utc>,
    pub session_id: String,
    pub model: Option<String>,
    pub project: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cost_usd: Option<f64>,
}

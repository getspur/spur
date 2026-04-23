//! Codex data ingestor.
//!
//! Discovers and parses Codex JSONL session logs.
//!
//! # Data Locations
//!
//! | Source | Path |
//! |--------|------|
//! | Env var | `$CODEX_HOME/sessions/**/*.jsonl` |
//! | Default | `~/.codex/sessions/**/*.jsonl` |
//!
//! # JSONL Schema
//!
//! Codex writes a stream of events. Two types matter for usage:
//!
//! **1. `turn_context` — sets the current model:**
//! ```json
//! {"type": "turn_context", "timestamp": "...", "payload": {"model": "gpt-5"}}
//! ```
//!
//! **2. `event_msg` with `token_count` — carries usage:**
//! ```json
//! {
//!   "type": "event_msg",
//!   "timestamp": "...",
//!   "payload": {
//!     "type": "token_count",
//!     "info": {
//!       "total_token_usage": {"input_tokens": 1200, "output_tokens": 500, ...},
//!       "last_token_usage":  {"input_tokens": 200,  "output_tokens": 100, ...},
//!       "model": "gpt-5"
//!     }
//!   }
//! }
//! ```
//!
//! `last_token_usage` is the delta for this turn. If absent, we subtract the
//! previous cumulative `total_token_usage` to derive the delta.
//!
//! Codex reports `cached_input_tokens` (same as `cache_read_input_tokens`).
//! Reasoning tokens are included in `output_tokens` and must not be double-counted.

use super::{parse_jsonl_line, TokenEvent};
use anyhow::Result;
use serde::Deserialize;
use std::io::BufRead;
use std::path::{Path, PathBuf};

// ─── Config ───────────────────────────────────────────────────────────

/// Environment variable for Codex home directory.
pub const CODEX_HOME_ENV: &str = "CODEX_HOME";
/// Default Codex data directory.
pub const DEFAULT_CODEX_DIR: &str = ".codex";
/// Sessions subdirectory.
pub const CODEX_SESSIONS_DIR: &str = "sessions";

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

// ─── Raw Schema ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct CodexEntry {
    #[serde(rename = "type")]
    entry_type: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    payload: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize, Clone)]
struct CodexTokenUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(
        default,
        alias = "cached_input_tokens",
        alias = "cache_read_input_tokens"
    )]
    cached_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

// ─── Ingestor ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct CodexIngestor;

impl Default for CodexIngestor {
    fn default() -> Self {
        Self
    }
}

impl CodexIngestor {
    pub fn new() -> Self {
        Self
    }
}

impl super::Ingestor for CodexIngestor {
    fn name(&self) -> &str {
        "codex"
    }

    fn discover_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();

        // 1. Environment variable
        if let Ok(env) = std::env::var(CODEX_HOME_ENV) {
            let p = PathBuf::from(env.trim()).join(CODEX_SESSIONS_DIR);
            if p.is_dir() {
                paths.push(p);
                return paths;
            }
        }

        // 2. Default: ~/.codex/sessions
        if let Some(home) = home_dir() {
            let p = home.join(DEFAULT_CODEX_DIR).join(CODEX_SESSIONS_DIR);
            if p.is_dir() {
                paths.push(p);
            }
        }

        paths
    }

    fn load_file(&self, path: &Path) -> Result<Vec<TokenEvent>> {
        let session_id = extract_session_id(path);
        let file = std::fs::File::open(path)?;
        let reader = std::io::BufReader::new(file);

        let mut events = Vec::new();
        let mut current_model: Option<String> = None;
        let mut previous_total: Option<CodexTokenUsage> = None;
        let mut legacy_fallback_used = false;

        for line in reader.lines() {
            let line = line?;
            let Some(entry): Option<CodexEntry> = parse_jsonl_line(&line) else {
                continue;
            };

            // turn_context sets the current model
            if entry.entry_type == "turn_context" {
                if let Some(model) = extract_model_from_turn_context(&entry.payload) {
                    current_model = Some(model);
                }
                continue;
            }

            // We only care about event_msg containing token_count
            if entry.entry_type != "event_msg" {
                continue;
            }

            let Some(payload) = &entry.payload else {
                continue;
            };
            let payload_type = payload.get("type").and_then(|v| v.as_str());
            if payload_type != Some("token_count") {
                continue;
            }

            let Some(info) = payload.get("info") else {
                continue;
            };

            // Extract model from this event if present
            if let Some(model) = extract_model_from_info(info) {
                current_model = Some(model);
            }

            // Parse usage
            let last_usage = info
                .get("last_token_usage")
                .and_then(|v| serde_json::from_value::<CodexTokenUsage>(v.clone()).ok());

            let total_usage = info
                .get("total_token_usage")
                .and_then(|v| serde_json::from_value::<CodexTokenUsage>(v.clone()).ok());

            let raw_delta = if let Some(last) = last_usage {
                last
            } else if let Some(total) = total_usage.clone() {
                // Derive delta from cumulative totals
                if let Some(prev) = previous_total.clone() {
                    subtract_usage(&total, &prev)
                } else {
                    total
                }
            } else {
                continue;
            };

            // Update previous total for next iteration
            if let Some(total) = total_usage {
                previous_total = Some(total);
            }

            // Skip zero-delta events
            if raw_delta.input_tokens == 0
                && raw_delta.output_tokens == 0
                && raw_delta.cached_input_tokens == 0
            {
                continue;
            }

            // Model fallback for legacy logs
            let mut is_fallback_model = false;
            if current_model.is_none() {
                current_model = Some("gpt-5".to_string());
                legacy_fallback_used = true;
                is_fallback_model = true;
            }

            let timestamp = entry
                .timestamp
                .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
                .unwrap_or_else(chrono::Utc::now);

            // Cap cache read at input to avoid over-billing
            let cache_read = raw_delta.cached_input_tokens.min(raw_delta.input_tokens);

            let model = current_model.clone().filter(|m| m != "<synthetic>");

            events.push(TokenEvent {
                timestamp,
                session_id: Some(session_id.clone()),
                agent: "codex".to_string(),
                model,
                project: None, // Codex doesn't store project in path
                input_tokens: raw_delta.input_tokens,
                output_tokens: raw_delta.output_tokens,
                cache_creation_tokens: 0, // Codex doesn't distinguish cache creation
                cache_read_tokens: cache_read,
                cost_usd: None, // Codex doesn't embed pre-calculated cost
                source_file: path.to_path_buf(),
            });

            if is_fallback_model {
                // Don't permanently set fallback; next event might have real model
                current_model = None;
            }
        }

        if legacy_fallback_used {
            tracing::debug!(
                file = %path.display(),
                "Codex session lacked model metadata; applied gpt-5 fallback"
            );
        }

        Ok(events)
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────

fn extract_session_id(path: &Path) -> String {
    let relative = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    relative.to_string()
}

fn extract_model_from_turn_context(payload: &Option<serde_json::Value>) -> Option<String> {
    let payload = payload.as_ref()?;
    payload
        .get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn extract_model_from_info(info: &serde_json::Value) -> Option<String> {
    // Try info.model, info.model_name, info.metadata.model
    for key in &["model", "model_name"] {
        if let Some(m) = info.get(key).and_then(|v| v.as_str()) {
            return Some(m.to_string());
        }
    }
    if let Some(meta) = info.get("metadata") {
        if let Some(m) = meta.get("model").and_then(|v| v.as_str()) {
            return Some(m.to_string());
        }
    }
    None
}

fn subtract_usage(current: &CodexTokenUsage, previous: &CodexTokenUsage) -> CodexTokenUsage {
    CodexTokenUsage {
        input_tokens: current.input_tokens.saturating_sub(previous.input_tokens),
        cached_input_tokens: current
            .cached_input_tokens
            .saturating_sub(previous.cached_input_tokens),
        output_tokens: current.output_tokens.saturating_sub(previous.output_tokens),
        reasoning_output_tokens: current
            .reasoning_output_tokens
            .saturating_sub(previous.reasoning_output_tokens),
        total_tokens: current.total_tokens.saturating_sub(previous.total_tokens),
    }
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::Ingestor;

    #[test]
    fn test_parse_codex_turn_context_then_token_count() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("session.jsonl");
        let content = r#"
{"type":"turn_context","timestamp":"2026-04-20T10:00:00Z","payload":{"model":"gpt-5"}}
{"type":"event_msg","timestamp":"2026-04-20T10:01:00Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":1000,"cached_input_tokens":200,"output_tokens":500,"reasoning_output_tokens":0,"total_tokens":1700},"total_token_usage":{"input_tokens":1000,"cached_input_tokens":200,"output_tokens":500,"reasoning_output_tokens":0,"total_tokens":1700},"model":"gpt-5"}}}
{"type":"event_msg","timestamp":"2026-04-20T10:02:00Z","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":2000,"cached_input_tokens":300,"output_tokens":800,"reasoning_output_tokens":0,"total_tokens":2800}}}}
"#;
        std::fs::write(&path, content).unwrap();

        let ingestor = CodexIngestor::new();
        let events = ingestor.load_file(&path).unwrap();
        assert_eq!(events.len(), 2);

        // First event: last_token_usage (direct delta)
        assert_eq!(events[0].input_tokens, 1000);
        assert_eq!(events[0].output_tokens, 500);
        assert_eq!(events[0].cache_read_tokens, 200);
        assert_eq!(events[0].model, Some("gpt-5".to_string()));

        // Second event: derived from cumulative total (2800 - 1700 = 1100 tokens)
        assert_eq!(events[1].input_tokens, 1000); // 2000 - 1000
        assert_eq!(events[1].output_tokens, 300); // 800 - 500
        assert_eq!(events[1].cache_read_tokens, 100); // 300 - 200
    }

    #[test]
    fn test_legacy_fallback_model() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("legacy.jsonl");
        let content = r#"
{"type":"event_msg","timestamp":"2026-04-20T10:00:00Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":500,"output_tokens":200,"total_tokens":700}}}}
"#;
        std::fs::write(&path, content).unwrap();

        let ingestor = CodexIngestor::new();
        let events = ingestor.load_file(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model, Some("gpt-5".to_string()));
    }

    #[test]
    fn test_skips_zero_delta() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.jsonl");
        let content = r#"
{"type":"turn_context","timestamp":"2026-04-20T10:00:00Z","payload":{"model":"gpt-5"}}
{"type":"event_msg","timestamp":"2026-04-20T10:01:00Z","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}}}}
"#;
        std::fs::write(&path, content).unwrap();

        let ingestor = CodexIngestor::new();
        let events = ingestor.load_file(&path).unwrap();
        assert!(events.is_empty());
    }
}

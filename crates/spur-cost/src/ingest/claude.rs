//! Claude Code data ingestor.
//!
//! Discovers and parses Claude Code JSONL usage logs.
//!
//! # Data Locations
//!
//! | Source | Path |
//! |--------|------|
//! | Env var | `$CLAUDE_CONFIG_DIR/projects/**/*.jsonl` |
//! | XDG default | `~/.config/claude/projects/**/*.jsonl` |
//! | Legacy default | `~/.claude/projects/**/*.jsonl` |
//!
//! # JSONL Schema
//!
//! ```json
//! {
//!   "timestamp": "2026-04-23T18:00:00Z",
//!   "sessionId": "sess-abc",
//!   "message": {
//!     "usage": {
//!       "input_tokens": 1000,
//!       "output_tokens": 500,
//!       "cache_creation_input_tokens": 0,
//!       "cache_read_input_tokens": 200,
//!       "speed": "standard"
//!     },
//!     "model": "claude-sonnet-4-20250514",
//!     "id": "msg_123"
//!   },
//!   "costUSD": 0.05,
//!   "requestId": "req_456"
//! }
//! ```

use super::{parse_jsonl_lines, TokenEvent};
use anyhow::Result;
use serde::Deserialize;
use std::path::{Path, PathBuf};

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

// ─── Config ───────────────────────────────────────────────────────────

/// Environment variable for overriding Claude data directory.
pub const CLAUDE_CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";
/// Default XDG config path component.
pub const DEFAULT_CLAUDE_CONFIG_PATH: &str = ".config/claude";
/// Legacy path component.
pub const DEFAULT_CLAUDE_CODE_PATH: &str = ".claude";
/// Projects subdirectory containing usage JSONL files.
pub const CLAUDE_PROJECTS_DIR: &str = "projects";

// ─── Raw Schema ───────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ClaudeUsageEntry {
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    timestamp: String,
    #[serde(default)]
    message: ClaudeMessage,
    #[serde(default, rename = "costUSD")]
    cost_usd: Option<f64>,
    #[expect(
        dead_code,
        reason = "Claude log schema keeps request_id for compatibility"
    )]
    #[serde(default)]
    request_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    usage: ClaudeUsage,
    #[serde(default)]
    model: Option<String>,
    #[expect(
        dead_code,
        reason = "Claude log schema keeps message id for compatibility"
    )]
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

// ─── Ingestor ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub struct ClaudeIngestor;

impl Default for ClaudeIngestor {
    fn default() -> Self {
        Self
    }
}

impl ClaudeIngestor {
    pub fn new() -> Self {
        Self
    }
}

impl super::Ingestor for ClaudeIngestor {
    fn name(&self) -> &str {
        "claude"
    }

    fn discover_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let mut seen = std::collections::HashSet::new();

        // 1. Environment variable (comma-separated)
        if let Ok(env) = std::env::var(CLAUDE_CONFIG_DIR_ENV) {
            for part in env.split(',') {
                let p = PathBuf::from(part.trim());
                let projects = p.join(CLAUDE_PROJECTS_DIR);
                if projects.is_dir() && seen.insert(projects.clone()) {
                    paths.push(projects);
                }
            }
            if !paths.is_empty() {
                return paths;
            }
        }

        // 2. XDG default: ~/.config/claude/projects
        if let Some(home) = home_dir() {
            let xdg = home
                .join(DEFAULT_CLAUDE_CONFIG_PATH)
                .join(CLAUDE_PROJECTS_DIR);
            if xdg.is_dir() && seen.insert(xdg.clone()) {
                paths.push(xdg);
            }

            // 3. Legacy default: ~/.claude/projects
            let legacy = home
                .join(DEFAULT_CLAUDE_CODE_PATH)
                .join(CLAUDE_PROJECTS_DIR);
            if legacy.is_dir() && seen.insert(legacy.clone()) {
                paths.push(legacy);
            }
        }

        paths
    }

    fn load_file(&self, path: &Path) -> Result<Vec<TokenEvent>> {
        let project = extract_project_from_path(path);
        let raw_entries: Vec<ClaudeUsageEntry> = parse_jsonl_lines(path)?;

        let mut events = Vec::with_capacity(raw_entries.len());
        for entry in raw_entries {
            let timestamp = entry
                .timestamp
                .parse::<chrono::DateTime<chrono::Utc>>()
                .unwrap_or_else(|_| chrono::Utc::now());

            // Skip synthetic model
            let model = entry.message.model.filter(|m| m != "<synthetic>");

            events.push(TokenEvent {
                timestamp,
                session_id: entry.session_id,
                agent: "claude".to_string(),
                model,
                project: project.clone(),
                input_tokens: entry.message.usage.input_tokens,
                output_tokens: entry.message.usage.output_tokens,
                cache_creation_tokens: entry.message.usage.cache_creation_input_tokens,
                cache_read_tokens: entry.message.usage.cache_read_input_tokens,
                cost_usd: entry.cost_usd,
                source_file: path.to_path_buf(),
            });
        }

        Ok(events)
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────

fn extract_project_from_path(jsonl_path: &Path) -> Option<String> {
    let segments: Vec<_> = jsonl_path.iter().map(|s| s.to_string_lossy()).collect();
    if let Some(idx) = segments.iter().position(|s| s == CLAUDE_PROJECTS_DIR) {
        segments.get(idx + 1).map(|s| s.to_string())
    } else {
        None
    }
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingest::Ingestor;

    #[test]
    fn test_extract_project_from_path() {
        let p = PathBuf::from("/home/user/.config/claude/projects/my-app/session.jsonl");
        assert_eq!(extract_project_from_path(&p), Some("my-app".to_string()));

        let p2 = PathBuf::from("/random/path/file.jsonl");
        assert_eq!(extract_project_from_path(&p2), None);
    }

    #[test]
    fn test_parse_claude_entry() {
        let json = r#"{
            "timestamp": "2026-04-20T10:00:00Z",
            "sessionId": "sess-1",
            "message": {
                "usage": {
                    "input_tokens": 1000,
                    "output_tokens": 500,
                    "cache_creation_input_tokens": 100,
                    "cache_read_input_tokens": 50
                },
                "model": "claude-sonnet-4"
            },
            "costUSD": 0.05
        }"#;

        let entry: ClaudeUsageEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.session_id, Some("sess-1".to_string()));
        assert_eq!(entry.message.usage.input_tokens, 1000);
        assert_eq!(entry.message.model, Some("claude-sonnet-4".to_string()));
        assert_eq!(entry.cost_usd, Some(0.05));
    }

    #[test]
    fn test_skips_synthetic_model() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.jsonl");
        std::fs::write(&path, r#"{"timestamp":"2026-04-20T10:00:00Z","message":{"usage":{"input_tokens":100,"output_tokens":50},"model":"<synthetic>"}}"#).unwrap();

        let ingestor = ClaudeIngestor::new();
        let events = ingestor.load_file(&path).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].model, None); // synthetic stripped
    }
}

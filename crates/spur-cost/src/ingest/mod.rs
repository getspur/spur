//! Agent data ingestion layer.
//!
//! Inspired by ccusage's per-app data loaders, this module discovers and parses
//! session data stored natively by each agent (Claude Code, Codex, Kiro, etc.)
//! and normalizes it into a common `TokenEvent` stream.
//!
//! Each agent stores usage data in its own format and location:
//!
//! | Agent | Location | Format |
//! |-------|----------|--------|
//! | Claude Code | `~/.config/claude/projects/**/*.jsonl` | `{timestamp, message: {usage, model}}` |
//! | Codex | `~/.codex/sessions/**/*.jsonl` | `{type: "event_msg", payload: {type: "token_count", info: {total_token_usage, model}}}` |
//! | Kiro | TBD | TBD |
//!
//! The ingestion pipeline:
//! 1. **Discover** agent data directories (env var → default path → skip if missing)
//! 2. **Glob** JSONL files recursively
//! 3. **Stream-parse** line-by-line (memory-efficient for large logs)
//! 4. **Normalize** to `TokenEvent` (common schema across all agents)
//! 5. **Deduplicate** using `(session_id, timestamp, total_tokens)` hash
//! 6. **Calculate cost** via `PricingRegistry` when agent doesn't provide it

pub mod claude;
pub mod codex;
pub mod kiro;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::collections::HashSet;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

// ─── Common Event Type ────────────────────────────────────────────────

/// A normalized token usage event from any agent.
///
/// All agent-specific ingestors convert their native formats into this
/// common shape so downstream reporting can be agent-agnostic.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenEvent {
    /// Event timestamp (UTC).
    pub timestamp: DateTime<Utc>,
    /// Session identifier, if available.
    pub session_id: Option<String>,
    /// Agent kind label (e.g. "claude", "codex", "kiro").
    pub agent: String,
    /// Model name, if reported by the agent.
    pub model: Option<String>,
    /// Project name, if inferable from path.
    pub project: Option<String>,
    /// Input (prompt) tokens.
    pub input_tokens: u64,
    /// Output (generated) tokens.
    pub output_tokens: u64,
    /// Cache-creation input tokens.
    pub cache_creation_tokens: u64,
    /// Cache-read input tokens.
    pub cache_read_tokens: u64,
    /// Pre-calculated cost from the agent, if available.
    pub cost_usd: Option<f64>,
    /// Raw file path this event was loaded from (for debugging).
    pub source_file: PathBuf,
}

impl TokenEvent {
    /// Total billable tokens.
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_tokens + self.cache_read_tokens
    }

    /// Unique deduplication key.
    pub fn dedup_key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.session_id.as_deref().unwrap_or(""),
            self.timestamp.timestamp_millis(),
            self.total_tokens()
        )
    }
}

// ─── Ingestor Trait ───────────────────────────────────────────────────

/// Trait for agent-specific data loaders.
///
/// Implementors know where an agent stores its data, how to discover files,
/// and how to parse each line into `TokenEvent`s.
pub trait Ingestor: Send + Sync + fmt::Debug {
    /// Human-readable agent name ("claude", "codex", etc.).
    fn name(&self) -> &str;

    /// Discover all data directories for this agent.
    ///
    /// Returns paths that actually exist on disk. The pipeline will glob
    /// `**/*.jsonl` under each returned directory.
    fn discover_paths(&self) -> Vec<PathBuf>;

    /// Parse a single JSONL file into normalized token events.
    fn load_file(&self, path: &Path) -> Result<Vec<TokenEvent>>;
}

// ─── Pipeline ─────────────────────────────────────────────────────────

/// Combines multiple ingestors into a unified event stream.
#[derive(Debug)]
pub struct IngestionPipeline {
    ingestors: Vec<Box<dyn Ingestor>>,
}

impl Default for IngestionPipeline {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl IngestionPipeline {
    /// Create an empty pipeline.
    pub fn new() -> Self {
        Self {
            ingestors: Vec::new(),
        }
    }

    /// Create a pipeline with all built-in ingestors registered.
    pub fn with_defaults() -> Self {
        let mut p = Self::new();
        p.register(Box::new(claude::ClaudeIngestor::new()));
        p.register(Box::new(codex::CodexIngestor::new()));
        p.register(Box::new(kiro::KiroIngestor::new()));
        p
    }

    /// Register an ingestor.
    pub fn register(&mut self, ingestor: Box<dyn Ingestor>) {
        self.ingestors.push(ingestor);
    }

    /// Load events from all registered ingestors.
    ///
    /// Events are deduplicated globally using `TokenEvent::dedup_key()`.
    pub fn load_all(&self) -> Result<Vec<TokenEvent>> {
        let mut all_events = Vec::new();
        let mut seen = HashSet::new();

        for ingestor in &self.ingestors {
            let paths = ingestor.discover_paths();
            if paths.is_empty() {
                continue;
            }

            for dir in &paths {
                let files = glob_jsonl_files(dir)?;
                for file in files {
                    match ingestor.load_file(&file) {
                        Ok(events) => {
                            for ev in events {
                                let key = ev.dedup_key();
                                if seen.insert(key) {
                                    all_events.push(ev);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                ingestor = ingestor.name(),
                                file = %file.display(),
                                error = %e,
                                "failed to load file"
                            );
                        }
                    }
                }
            }
        }

        // Sort chronologically
        all_events.sort_by_key(|event| event.timestamp);
        Ok(all_events)
    }

    /// Load events filtered to a time range.
    pub fn load_range(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<TokenEvent>> {
        let events = self.load_all()?;
        Ok(events
            .into_iter()
            .filter(|e| e.timestamp >= from && e.timestamp < to)
            .collect())
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────

/// Recursively find all `.jsonl` files under a directory.
pub fn glob_jsonl_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if !dir.is_dir() {
        return Ok(files);
    }
    glob_jsonl_recursive(dir, &mut files)?;
    files.sort();
    Ok(files)
}

fn glob_jsonl_recursive(dir: &Path, acc: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            glob_jsonl_recursive(&path, acc)?;
        } else if path.extension().and_then(|s| s.to_str()) == Some("jsonl") {
            acc.push(path);
        }
    }
    Ok(())
}

/// Stream-parse a JSONL file line-by-line.
pub fn parse_jsonl_lines<T>(path: &Path) -> Result<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut results = Vec::new();

    for (line_no, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<T>(trimmed) {
            Ok(record) => results.push(record),
            Err(e) => {
                tracing::debug!(
                    file = %path.display(),
                    line = line_no + 1,
                    error = %e,
                    "skipping malformed JSONL line"
                );
            }
        }
    }

    Ok(results)
}

/// Parse a single JSONL line (for streaming). Returns `None` on empty/malformed.
pub fn parse_jsonl_line<T>(line: &str) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize, Clone, PartialEq)]
    struct TestRecord {
        id: u32,
    }

    #[test]
    fn test_parse_jsonl_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("test.jsonl");
        std::fs::write(&path, "{\"id\":1}\n\n{\"id\":2}\n{\"bad\":\n{\"id\":3}\n").unwrap();

        let records = parse_jsonl_lines::<TestRecord>(&path).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[0].id, 1);
        assert_eq!(records[1].id, 2);
        assert_eq!(records[2].id, 3);
    }

    #[test]
    fn test_glob_jsonl_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("sub")).unwrap();
        std::fs::write(tmp.path().join("a.jsonl"), "").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "").unwrap();
        std::fs::write(tmp.path().join("sub/c.jsonl"), "").unwrap();

        let files = glob_jsonl_files(tmp.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|p| p.file_name().unwrap() == "a.jsonl"));
        assert!(files.iter().any(|p| p.file_name().unwrap() == "c.jsonl"));
    }

    #[test]
    fn test_dedup_key_consistency() {
        let ev1 = TokenEvent {
            timestamp: "2026-04-20T10:00:00Z".parse().unwrap(),
            session_id: Some("s1".to_string()),
            agent: "claude".to_string(),
            model: None,
            project: None,
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_tokens: 0,
            cache_read_tokens: 10,
            cost_usd: None,
            source_file: PathBuf::from("/tmp/test.jsonl"),
        };
        let ev2 = ev1.clone();
        assert_eq!(ev1.dedup_key(), ev2.dedup_key());
    }
}

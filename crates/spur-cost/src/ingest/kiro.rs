//! Kiro data ingestor.
//!
//! Kiro session files are ACP protocol logs (`Prompt`, `AssistantMessage`,
//! `ToolResults`, `Compaction`, `Clear`). They do **not** contain token usage
//! or cost data — those are transmitted via ACP `UsageUpdate` messages and
//! should be captured by the SPUR orchestrator (`spur-core`) into the cost
//! ledger.
//!
//! This ingestor therefore returns an empty event list. It exists to satisfy
//! the `Ingestor` trait contract and to document the architectural boundary:
//! Kiro billing data lives in the ACP layer, not the filesystem.
//!
//! # Data Location
//!
//! | Source | Path |
//! |--------|------|
//! | Env var | `$KIRO_HOME/sessions/**/*.jsonl` |
//! | Default | `~/.kiro/sessions/**/*.jsonl` |

use super::TokenEvent;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Environment variable for Kiro home directory.
pub const KIRO_HOME_ENV: &str = "KIRO_HOME";
/// Default Kiro data directory.
pub const DEFAULT_KIRO_DIR: &str = ".kiro";
/// Sessions subdirectory.
pub const KIRO_SESSIONS_DIR: &str = "sessions";

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[derive(Debug)]
pub struct KiroIngestor;

impl Default for KiroIngestor {
    fn default() -> Self {
        Self
    }
}

impl KiroIngestor {
    pub fn new() -> Self {
        Self
    }
}

impl super::Ingestor for KiroIngestor {
    fn name(&self) -> &str {
        "kiro"
    }

    fn discover_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();

        if let Ok(env) = std::env::var(KIRO_HOME_ENV) {
            let p = PathBuf::from(env.trim()).join(KIRO_SESSIONS_DIR);
            if p.is_dir() {
                paths.push(p);
                return paths;
            }
        }

        if let Some(home) = home_dir() {
            let p = home.join(DEFAULT_KIRO_DIR).join(KIRO_SESSIONS_DIR);
            if p.is_dir() {
                paths.push(p);
            }
        }

        paths
    }

    fn load_file(&self, _path: &Path) -> Result<Vec<TokenEvent>> {
        // Kiro session files are ACP protocol logs. They do not contain token
        // usage or cost data. Billing events for Kiro should be captured via
        // ACP UsageUpdate messages in the SPUR orchestrator.
        Ok(Vec::new())
    }
}

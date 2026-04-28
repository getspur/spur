//! Gemini CLI JSON-document extractor.

use super::ExtractedRow;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct SessionDoc {
    #[serde(rename = "sessionId")]
    session_id: String,
    #[serde(default, rename = "projectHash")]
    project_hash: Option<String>,
    #[serde(default)]
    messages: Vec<Message>,
}

#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    tokens: Option<Tokens>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct Tokens {
    #[serde(default)]
    input: i64,
    #[serde(default)]
    output: i64,
    #[serde(default)]
    cached: i64,
    #[serde(default)]
    thoughts: i64,
    #[serde(default)]
    tool: i64,
}

/// Extract all Gemini session chat files under `tmp_root`.
///
/// `tmp_root` is `~/.gemini/tmp` — direct parent of per-session UUID dirs.
/// Recursively walks `<uuid>/chats/session-*.json` files.
pub fn extract(tmp_root: &Path) -> Result<Vec<ExtractedRow>> {
    let mut out = Vec::new();
    if !tmp_root.is_dir() {
        return Ok(out);
    }
    for path in discover_session_files(tmp_root)? {
        extract_file(&path, &mut out)
            .with_context(|| format!("failed to extract {}", path.display()))?;
    }
    Ok(out)
}

fn discover_session_files(tmp_root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !tmp_root.is_dir() {
        return Ok(out);
    }

    let mut session_entries = fs::read_dir(tmp_root)?.collect::<std::io::Result<Vec<_>>>()?;
    session_entries.sort_by_key(|entry| entry.path());

    for entry in session_entries {
        let session_dir = entry.path();
        if !session_dir.is_dir() {
            continue;
        }

        let chats_dir = session_dir.join("chats");
        if !chats_dir.is_dir() {
            continue;
        }

        let mut chat_entries = fs::read_dir(&chats_dir)?.collect::<std::io::Result<Vec<_>>>()?;
        chat_entries.sort_by_key(|entry| entry.path());

        for chat_entry in chat_entries {
            let path = chat_entry.path();
            if is_session_json(&path) {
                out.push(path);
            }
        }
    }

    Ok(out)
}

fn is_session_json(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some("json")
        && path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|name| name.starts_with("session-"))
}

fn extract_file(path: &Path, out: &mut Vec<ExtractedRow>) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let doc: SessionDoc =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;

    for message in &doc.messages {
        if message.kind.as_deref() != Some("gemini") {
            continue;
        }

        let tokens = message.tokens.clone().unwrap_or_default();
        let timestamp = message
            .timestamp
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        out.push(ExtractedRow {
            timestamp,
            session_id: doc.session_id.clone(),
            model: message.model.clone(),
            project: doc.project_hash.clone(),
            input_tokens: tokens.input + tokens.tool,
            output_tokens: tokens.output + tokens.thoughts,
            cache_read_tokens: tokens.cached,
            cache_creation_tokens: 0,
            cost_usd: None,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    const SESSION_ONE: &str = "9c90babd-aaaa-bbbb-cccc-ddddddddddd1";

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/gemini/two_session_synthetic")
    }

    fn session_one_file() -> PathBuf {
        fixture_dir()
            .join(SESSION_ONE)
            .join("chats")
            .join("session-2026-04-28T01-00-aaaaaaaa.json")
    }

    #[test]
    fn extract_synthetic_session() {
        let mut rows = Vec::new();
        extract_file(&session_one_file(), &mut rows).unwrap();

        assert_eq!(rows.len(), 2, "two gemini messages in fixture session 1");
        let r0 = &rows[0];
        assert_eq!(r0.session_id, SESSION_ONE);
        assert_eq!(r0.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(r0.project.as_deref(), Some("abc123"));
        assert_eq!(r0.input_tokens, 100);
        assert_eq!(r0.output_tokens, 25);
        assert_eq!(r0.cache_read_tokens, 0);
        assert!(r0.cost_usd.is_none());

        let r1 = &rows[1];
        assert_eq!(r1.session_id, SESSION_ONE);
        assert_eq!(r1.input_tokens, 205);
        assert_eq!(r1.output_tokens, 40);
        assert_eq!(r1.cache_read_tokens, 80);
        assert!(r1.cost_usd.is_none());
    }

    #[test]
    fn extract_multiple_synthetic_sessions() {
        let rows = extract(&fixture_dir()).unwrap();
        assert_eq!(rows.len(), 3, "three gemini messages across two fixtures");

        let mut by_session = BTreeMap::new();
        for row in rows {
            *by_session.entry(row.session_id).or_insert(0usize) += 1;
        }
        assert_eq!(by_session.get(SESSION_ONE), Some(&2));
        assert_eq!(
            by_session.get("9c90babd-eeee-ffff-aaaa-bbbbbbbbbbb2"),
            Some(&1)
        );
    }

    #[test]
    #[ignore]
    fn smoke_real_gemini_dir() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        let tmp = home.join(".gemini/tmp");
        if !tmp.is_dir() {
            return;
        }
        let rows = extract(&tmp).unwrap();
        assert!(
            !rows.is_empty(),
            "expected real Gemini sessions on this dev machine"
        );
        let total_input: i64 = rows.iter().map(|r| r.input_tokens).sum();
        let total_output: i64 = rows.iter().map(|r| r.output_tokens).sum();
        eprintln!(
            "gemini smoke: rows={} input={} output={}",
            rows.len(),
            total_input,
            total_output
        );
    }
}

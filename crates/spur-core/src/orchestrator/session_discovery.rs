use std::path::{Path, PathBuf};

use agent_client_protocol::schema::SessionInfo;
use anyhow::Result;
use spur_acp::AgentKind;

// ── Public API ──────────────────────────────────────────────────────────

/// Zero-cost enum dispatch for agent-specific session discovery backends.
///
/// Each variant maps to an `AgentKind` that stores session metadata on
/// local disk. Agents without a known disk format (e.g. generic ACP
/// speakers that rely entirely on RPC) return `None` from
/// [`discovery_for_kind`].
#[derive(Debug, Clone, Copy)]
pub enum SessionDiscoveryKind {
    Codex,
    Kiro,
    Kimi,
    OpenCode,
}

impl SessionDiscoveryKind {
    /// Scan the agent's local storage and return all sessions found on disk.
    pub fn discover(&self) -> Result<Vec<SessionInfo>> {
        match self {
            Self::Codex => codex::discover(),
            Self::Kiro => kiro::discover(),
            Self::Kimi => kimi::discover(),
            Self::OpenCode => opencode::discover(),
        }
    }
}

/// Map an [`AgentKind`] to its disk fallback, if any.
pub fn discovery_for_kind(kind: AgentKind) -> Option<SessionDiscoveryKind> {
    match kind {
        AgentKind::CodexAcp => Some(SessionDiscoveryKind::Codex),
        AgentKind::Kiro => Some(SessionDiscoveryKind::Kiro),
        AgentKind::Kimi => Some(SessionDiscoveryKind::Kimi),
        AgentKind::OpenCode => Some(SessionDiscoveryKind::OpenCode),
        _ => None,
    }
}

/// Classify discovered sessions into brain-eligible and worker sessions.
///
/// * **Brain** — `cwd` is inside `repo_root` but outside `.spur/worktrees`.
/// * **Worker** — `cwd` is inside `repo_root/.spur/worktrees`.
///
/// Sessions outside `repo_root` are dropped (same behaviour as the old
/// `filter_sessions_for_repo`).
pub fn classify_sessions(
    sessions: Vec<SessionInfo>,
    repo_root: &Path,
) -> (Vec<SessionInfo>, Vec<SessionInfo>) {
    let worktree_root = repo_root.join(".spur/worktrees");

    let mut brain = Vec::new();
    let mut worker = Vec::new();

    for session in sessions {
        let cwd = &session.cwd;
        if cwd.starts_with(&worktree_root) {
            worker.push(session);
        } else if cwd.starts_with(repo_root) {
            brain.push(session);
        }
    }

    (brain, worker)
}

// ── Codex backend ───────────────────────────────────────────────────────

mod codex {
    use super::*;

    pub(crate) const DISK_ROLLOUT_SCAN_LIMIT: usize = 500;

    pub(super) fn discover() -> Result<Vec<SessionInfo>> {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let sessions_root = home.join(".codex/sessions");
        discover_from_root(&sessions_root)
    }

    pub(crate) fn discover_from_root(sessions_root: &Path) -> Result<Vec<SessionInfo>> {
        if !sessions_root.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        for path in rollout_paths(sessions_root)? {
            if let Some(session) = parse_rollout_header(&path) {
                sessions.push(session);
            }
        }

        sessions.sort_by(|a, b| {
            let a_time = a.updated_at.as_deref().unwrap_or("");
            let b_time = b.updated_at.as_deref().unwrap_or("");
            b_time.cmp(a_time)
        });

        Ok(sessions)
    }

    fn rollout_paths(sessions_root: &Path) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        'outer: for year_dir in sorted_child_dirs(sessions_root)? {
            for month_dir in sorted_child_dirs(&year_dir)? {
                for day_dir in sorted_child_dirs(&month_dir)? {
                    for path in sorted_rollout_files(&day_dir)? {
                        paths.push(path);
                        if paths.len() >= DISK_ROLLOUT_SCAN_LIMIT {
                            break 'outer;
                        }
                    }
                }
            }
        }
        Ok(paths)
    }

    fn sorted_child_dirs(path: &Path) -> Result<Vec<PathBuf>> {
        let mut dirs = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                dirs.push(entry.path());
            }
        }
        dirs.sort_by(|a, b| b.cmp(a));
        Ok(dirs)
    }

    fn sorted_rollout_files(day_dir: &Path) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(day_dir)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => continue,
            };
            if !file_type.is_file() {
                continue;
            }

            let path = entry.path();
            let is_rollout = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("rollout-"));
            let is_jsonl = path.extension().and_then(|ext| ext.to_str()) == Some("jsonl");
            if is_rollout && is_jsonl {
                paths.push(path);
            }
        }
        paths.sort_by(|a, b| b.cmp(a));
        Ok(paths)
    }

    fn parse_rollout_header(path: &Path) -> Option<SessionInfo> {
        use std::io::{BufRead, Read};

        const MAX_HEADER_BYTES: u64 = 1 << 20; // 1 MiB

        let file = std::fs::File::open(path).ok()?;
        let mut reader = std::io::BufReader::new(file.take(MAX_HEADER_BYTES));
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }

        parse_session_header(&line)
    }

    pub(crate) fn parse_session_header(line: &str) -> Option<SessionInfo> {
        let json: serde_json::Value = serde_json::from_str(line).ok()?;
        let payload = json.get("payload").unwrap_or(&json);

        let session_id = json_string_field(payload, "id")
            .or_else(|| json_string_field(payload, "session_id"))
            .or_else(|| json_string_field(&json, "id"))
            .or_else(|| json_string_field(&json, "session_id"))?;
        let cwd = json_string_field(payload, "cwd").or_else(|| json_string_field(&json, "cwd"))?;
        let updated_at = json_string_field(payload, "timestamp")
            .or_else(|| json_string_field(&json, "timestamp"))
            .map(str::to_string);
        let title = json_string_field(payload, "title")
            .or_else(|| json_string_field(&json, "title"))
            .or_else(|| json_string_field(payload, "instructions"))
            .map(str::to_string);

        let mut info = SessionInfo::new(session_id.to_string(), PathBuf::from(cwd));
        info = info.updated_at(updated_at);
        info = info.title(title);
        Some(info)
    }
}

// ── Kiro backend ────────────────────────────────────────────────────────

mod kiro {
    use super::*;
    use tracing::info;

    pub(super) fn discover() -> Result<Vec<SessionInfo>> {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let sessions_dir = home.join(".kiro/sessions/cli");

        if !sessions_dir.exists() {
            return Ok(Vec::new());
        }

        let mut sessions: Vec<SessionInfo> = Vec::new();
        for entry in std::fs::read_dir(&sessions_dir)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let json: serde_json::Value = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let session_id = match json.get("session_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => continue,
            };
            let cwd = json
                .get("cwd")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let title = json
                .get("title")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let updated_at = json
                .get("updated_at")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let mut info = SessionInfo::new(session_id, PathBuf::from(cwd));
            info = info.title(title);
            info = info.updated_at(updated_at);
            sessions.push(info);
        }

        sessions.sort_by(|a, b| {
            let a_time = a.updated_at.as_deref().unwrap_or("");
            let b_time = b.updated_at.as_deref().unwrap_or("");
            b_time.cmp(a_time)
        });

        info!(
            count = sessions.len(),
            "Loaded sessions from kiro disk storage"
        );
        Ok(sessions)
    }
}

// ── Kimi backend ────────────────────────────────────────────────────────

mod kimi {
    use super::*;
    use tracing::debug;

    pub(super) fn discover() -> Result<Vec<SessionInfo>> {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let sessions_root = home.join(".kimi/sessions");

        if !sessions_root.exists() {
            return Ok(Vec::new());
        }

        let mut sessions = Vec::new();
        for hash_entry in std::fs::read_dir(&sessions_root)? {
            let hash_entry = match hash_entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let hash_path = hash_entry.path();
            if !hash_path.is_dir() {
                continue;
            }

            for uuid_entry in std::fs::read_dir(&hash_path)? {
                let uuid_entry = match uuid_entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let uuid_path = uuid_entry.path();
                if !uuid_path.is_dir() {
                    continue;
                }

                if let Some(session) = parse_kimi_session(&uuid_path) {
                    sessions.push(session);
                }
            }
        }

        sessions.sort_by(|a, b| {
            let a_time = a.updated_at.as_deref().unwrap_or("");
            let b_time = b.updated_at.as_deref().unwrap_or("");
            b_time.cmp(a_time)
        });

        debug!(
            count = sessions.len(),
            "Loaded sessions from kimi disk storage"
        );
        Ok(sessions)
    }

    fn parse_kimi_session(uuid_path: &Path) -> Option<SessionInfo> {
        let state_path = uuid_path.join("state.json");
        let context_path = uuid_path.join("context.jsonl");

        let state_json: serde_json::Value = match std::fs::read_to_string(&state_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            Some(v) => v,
            None => return None,
        };

        // Skip archived sessions.
        if state_json
            .get("archived")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return None;
        }

        let session_id = uuid_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())?;

        let title = state_json
            .get("custom_title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let cwd = extract_kimi_cwd(&context_path).unwrap_or_default();

        // Fall back to file mtime for updated_at since wire_mtime is often null.
        let updated_at = state_json
            .get("wire_mtime")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| file_mtime_iso(&state_path));

        let mut info = SessionInfo::new(session_id, PathBuf::from(cwd));
        info = info.title(title);
        info = info.updated_at(updated_at);
        Some(info)
    }

    /// Parse the first user message in context.jsonl to extract the working directory.
    fn extract_kimi_cwd(context_path: &Path) -> Option<String> {
        use std::io::BufRead;
        let file = std::fs::File::open(context_path).ok()?;
        let reader = std::io::BufReader::new(file);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(_) => continue,
            };
            if line.trim().is_empty() {
                continue;
            }
            let json: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let content = json.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(rest) = content.strip_prefix("Working directory: ") {
                let cwd = rest.split('\n').next().unwrap_or(rest).trim();
                return Some(cwd.to_string());
            }
        }
        None
    }

    fn file_mtime_iso(path: &Path) -> Option<String> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        let dt = chrono::DateTime::<chrono::Utc>::from(mtime);
        Some(dt.to_rfc3339())
    }
}

// ── OpenCode backend ────────────────────────────────────────────────────

mod opencode {
    use super::*;
    use tracing::{debug, warn};

    pub(super) fn discover() -> Result<Vec<SessionInfo>> {
        let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
        let db_path = home.join(".local/share/opencode/opencode.db");

        if !db_path.exists() {
            return Ok(Vec::new());
        }

        let conn = match rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "Failed to open opencode database");
                return Ok(Vec::new());
            }
        };

        let mut stmt = match conn.prepare(
            "SELECT id, directory, title, time_created, time_updated FROM session ORDER BY time_updated DESC"
        ) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "Failed to prepare opencode session query");
                return Ok(Vec::new());
            }
        };

        let rows = stmt.query_map([], |row| {
            let id: String = row.get(0)?;
            let directory: String = row.get(1)?;
            let title: Option<String> = row.get(2)?;
            let time_created: i64 = row.get(3)?;
            let time_updated: i64 = row.get(4)?;
            Ok((id, directory, title, time_created, time_updated))
        });

        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "Failed to query opencode sessions");
                return Ok(Vec::new());
            }
        };

        let mut sessions = Vec::new();
        for row in rows {
            let (id, directory, title, _time_created, time_updated) = match row {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Skipping malformed opencode session row");
                    continue;
                }
            };

            let updated_at = ms_to_iso(time_updated);
            let mut info = SessionInfo::new(id, PathBuf::from(directory));
            info = info.title(title);
            info = info.updated_at(updated_at);
            sessions.push(info);
        }

        debug!(
            count = sessions.len(),
            "Loaded sessions from opencode disk storage"
        );
        Ok(sessions)
    }

    fn ms_to_iso(ts_ms: i64) -> Option<String> {
        let ts_s = ts_ms / 1000;
        let naive = chrono::DateTime::from_timestamp(ts_s, 0)?;
        Some(naive.to_rfc3339())
    }
}

// ── Shared helpers ──────────────────────────────────────────────────────

fn json_string_field<'a>(json: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    json.get(field).and_then(|value| value.as_str())
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn write_codex_rollout(path: &Path, id: &str, timestamp: &str) {
        std::fs::write(
            path,
            format!(
                r#"{{"timestamp":"{timestamp}","type":"session_meta","payload":{{"id":"{id}","timestamp":"{timestamp}","cwd":"/repo/spur"}}}}"#
            ),
        )
        .expect("write rollout");
    }

    #[test]
    fn classifies_sessions_into_brain_and_worker() {
        let repo_root = Path::new("/repo/spur");
        let sessions = vec![
            SessionInfo::new("root", "/repo/spur"),
            SessionInfo::new("worker", "/repo/spur/.spur/worktrees/worker-1"),
            SessionInfo::new("sibling-prefix", "/repo/spur-other"),
            SessionInfo::new("other", "/repo/other"),
        ];

        let (brain, worker) = classify_sessions(sessions, repo_root);
        let brain_ids: Vec<_> = brain.iter().map(|s| s.session_id.0.as_ref()).collect();
        let worker_ids: Vec<_> = worker.iter().map(|s| s.session_id.0.as_ref()).collect();

        assert_eq!(brain_ids, vec!["root"]);
        assert_eq!(worker_ids, vec!["worker"]);
    }

    #[test]
    fn parses_codex_rollout_headers_from_newest_day_dirs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions_root = temp.path().join(".codex/sessions");
        let newest = sessions_root.join("2026/05/09");
        let older = sessions_root.join("2026/05/08");
        std::fs::create_dir_all(&newest).expect("newest dir");
        std::fs::create_dir_all(&older).expect("older dir");

        std::fs::write(
            newest.join("rollout-2026-05-09T09-00-00-new.jsonl"),
            r#"{"timestamp":"2026-05-09T02:00:00.000Z","type":"session_meta","payload":{"id":"new","timestamp":"2026-05-09T01:59:00.000Z","cwd":"/repo/spur/.spur/worktrees/worker","title":"Worker task"}}"#,
        )
        .expect("new rollout");
        std::fs::write(
            older.join("rollout-2026-05-08T09-00-00-old.jsonl"),
            r#"{"timestamp":"2026-05-08T02:00:00.000Z","type":"session_meta","payload":{"session_id":"old","cwd":"/repo/spur"}}"#,
        )
        .expect("old rollout");
        std::fs::write(
            newest.join("not-a-rollout.jsonl"),
            r#"{"timestamp":"2026-05-09T03:00:00.000Z","payload":{"id":"ignored","cwd":"/repo/spur"}}"#,
        )
        .expect("ignored file");
        std::fs::write(newest.join("rollout-invalid.jsonl"), "not json").expect("invalid rollout");

        let sessions = codex::discover_from_root(&sessions_root).expect("codex sessions from disk");

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id.0.as_ref(), "new");
        assert_eq!(
            sessions[0].cwd,
            PathBuf::from("/repo/spur/.spur/worktrees/worker")
        );
        assert_eq!(
            sessions[0].updated_at.as_deref(),
            Some("2026-05-09T01:59:00.000Z")
        );
        assert_eq!(sessions[0].title.as_deref(), Some("Worker task"));
        assert_eq!(sessions[1].session_id.0.as_ref(), "old");
        assert_eq!(
            sessions[1].updated_at.as_deref(),
            Some("2026-05-08T02:00:00.000Z")
        );
    }

    #[test]
    fn codex_disk_walk_respects_scan_limit() {
        use crate::orchestrator::session_discovery::codex::DISK_ROLLOUT_SCAN_LIMIT;

        let temp = tempfile::tempdir().expect("tempdir");
        let sessions_root = temp.path().join(".codex/sessions");
        let newest = sessions_root.join("2026/05/09");
        let next_newest = sessions_root.join("2026/05/08");
        let oldest = sessions_root.join("2026/05/07");
        std::fs::create_dir_all(&newest).expect("newest dir");
        std::fs::create_dir_all(&next_newest).expect("next newest dir");
        std::fs::create_dir_all(&oldest).expect("oldest dir");

        let newest_count = DISK_ROLLOUT_SCAN_LIMIT / 2;
        let next_newest_count = DISK_ROLLOUT_SCAN_LIMIT - newest_count;
        for i in 0..newest_count {
            write_codex_rollout(
                &newest.join(format!("rollout-2026-05-09T00-{:03}.jsonl", i)),
                &format!("newest-{i}"),
                &format!("2026-05-09T00:{i:03}:00.000Z"),
            );
        }
        for i in 0..next_newest_count {
            write_codex_rollout(
                &next_newest.join(format!("rollout-2026-05-08T00-{:03}.jsonl", i)),
                &format!("next-newest-{i}"),
                &format!("2026-05-08T00:{i:03}:00.000Z"),
            );
        }
        for i in 0..5 {
            write_codex_rollout(
                &oldest.join(format!("rollout-2026-05-07T00-{:03}.jsonl", i)),
                &format!("oldest-{i}"),
                &format!("2026-05-07T00:{i:03}:00.000Z"),
            );
        }

        let sessions = codex::discover_from_root(&sessions_root).expect("codex sessions from disk");
        let ids: Vec<_> = sessions
            .iter()
            .map(|session| session.session_id.0.as_ref())
            .collect();

        assert_eq!(ids.len(), DISK_ROLLOUT_SCAN_LIMIT);
        assert_eq!(
            ids.iter().filter(|id| id.starts_with("newest-")).count(),
            newest_count
        );
        assert_eq!(
            ids.iter()
                .filter(|id| id.starts_with("next-newest-"))
                .count(),
            next_newest_count
        );
        assert!(
            ids.iter().all(|id| !id.starts_with("oldest-")),
            "oldest rollout should be outside scan cap"
        );
    }

    #[test]
    fn parse_codex_session_header_rejects_missing_required_fields() {
        assert!(codex::parse_session_header(r#"{"payload":{"cwd":"/repo/spur"}}"#).is_none());
        assert!(
            codex::parse_session_header(r#"{"payload":{"id":"session-without-cwd"}}"#).is_none()
        );
    }

    #[test]
    fn kiro_discovery_reads_json_sessions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = temp.path().join(".kiro/sessions/cli");
        std::fs::create_dir_all(&sessions_dir).unwrap();

        std::fs::write(
            sessions_dir.join("session-a.json"),
            r#"{"session_id":"a","cwd":"/repo/spur","title":"First","updated_at":"2026-05-09T10:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            sessions_dir.join("session-b.json"),
            r#"{"session_id":"b","cwd":"/repo/spur/.spur/worktrees/w1","updated_at":"2026-05-08T10:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(sessions_dir.join("not-json.txt"), "hello").unwrap();
        std::fs::write(sessions_dir.join("broken.json"), "not json").unwrap();

        // Override HOME so kiro::discover() reads from our temp dir.
        let orig_home = std::env::var_os("HOME");
        std::env::set_var("HOME", temp.path());
        let sessions = kiro::discover().expect("kiro discovery");
        if let Some(home) = orig_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }

        assert_eq!(sessions.len(), 2);
        // Sorted by updated_at descending.
        assert_eq!(sessions[0].session_id.0.as_ref(), "a");
        assert_eq!(sessions[0].title.as_deref(), Some("First"));
        assert_eq!(sessions[1].session_id.0.as_ref(), "b");
    }

    #[test]
    fn kimi_discovery_reads_state_and_context() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = temp.path().join(".kimi/sessions/hash123");
        let uuid_dir = sessions_dir.join("uuid-abc");
        std::fs::create_dir_all(&uuid_dir).unwrap();

        std::fs::write(
            uuid_dir.join("state.json"),
            r#"{"custom_title":"Kimi Task","archived":false,"wire_mtime":"2026-05-09T10:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            uuid_dir.join("context.jsonl"),
            r#"{"role":"user","content":"Working directory: /repo/spur\n\nTask: do thing"}"#,
        )
        .unwrap();

        let orig_home = std::env::var_os("HOME");
        std::env::set_var("HOME", temp.path());
        let sessions = kimi::discover().expect("kimi discovery");
        if let Some(home) = orig_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id.0.as_ref(), "uuid-abc");
        assert_eq!(sessions[0].title.as_deref(), Some("Kimi Task"));
        assert_eq!(sessions[0].cwd, PathBuf::from("/repo/spur"));
        assert_eq!(
            sessions[0].updated_at.as_deref(),
            Some("2026-05-09T10:00:00Z")
        );
    }

    #[test]
    fn kimi_discovery_skips_archived_sessions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions_dir = temp.path().join(".kimi/sessions/hash456");
        let uuid_dir = sessions_dir.join("uuid-archived");
        std::fs::create_dir_all(&uuid_dir).unwrap();

        std::fs::write(
            uuid_dir.join("state.json"),
            r#"{"custom_title":"Old","archived":true}"#,
        )
        .unwrap();
        std::fs::write(
            uuid_dir.join("context.jsonl"),
            r#"{"role":"user","content":"Working directory: /repo/spur"}"#,
        )
        .unwrap();

        let orig_home = std::env::var_os("HOME");
        std::env::set_var("HOME", temp.path());
        let sessions = kimi::discover().expect("kimi discovery");
        if let Some(home) = orig_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }

        assert!(sessions.is_empty(), "archived sessions should be skipped");
    }

    #[test]
    fn opencode_discovery_reads_sqlite_sessions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let db_path = temp.path().join("opencode.db");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                parent_id TEXT,
                slug TEXT NOT NULL,
                directory TEXT NOT NULL,
                title TEXT NOT NULL,
                version TEXT NOT NULL,
                share_url TEXT,
                summary_additions INTEGER,
                summary_deletions INTEGER,
                summary_files INTEGER,
                summary_diffs TEXT,
                revert TEXT,
                permission TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                time_compacting INTEGER,
                time_archived INTEGER,
                workspace_id TEXT
            )",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
             VALUES ('ses_a', 'proj1', 'slug-a', '/repo/spur', 'Task A', '1', 1776761103000, 1776783891000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, project_id, slug, directory, title, version, time_created, time_updated)
             VALUES ('ses_b', 'proj1', 'slug-b', '/repo/spur/.spur/worktrees/w1', 'Task B', '1', 1776761103000, 1776782891000)",
            [],
        )
        .unwrap();
        drop(conn);

        // Override the db path by temporarily swapping HOME/.local/share
        let orig_home = std::env::var_os("HOME");
        let fake_home = temp.path().join("home");
        let share_dir = fake_home.join(".local/share/opencode");
        std::fs::create_dir_all(&share_dir).unwrap();
        std::fs::copy(&db_path, share_dir.join("opencode.db")).unwrap();
        std::env::set_var("HOME", &fake_home);

        let sessions = opencode::discover().expect("opencode discovery");
        if let Some(home) = orig_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].session_id.0.as_ref(), "ses_a");
        assert_eq!(sessions[0].cwd, PathBuf::from("/repo/spur"));
        assert_eq!(sessions[0].title.as_deref(), Some("Task A"));
        assert_eq!(sessions[1].session_id.0.as_ref(), "ses_b");
        assert_eq!(
            sessions[1].cwd,
            PathBuf::from("/repo/spur/.spur/worktrees/w1")
        );
    }

    #[test]
    fn discovery_for_kind_maps_expected_variants() {
        assert!(discovery_for_kind(AgentKind::CodexAcp).is_some());
        assert!(discovery_for_kind(AgentKind::Kiro).is_some());
        assert!(discovery_for_kind(AgentKind::Kimi).is_some());
        assert!(discovery_for_kind(AgentKind::OpenCode).is_some());
        assert!(discovery_for_kind(AgentKind::ClaudeCodeAcp).is_none());
        assert!(discovery_for_kind(AgentKind::ClaudeStreamJson).is_none());
        assert!(discovery_for_kind(AgentKind::Gemini).is_none());
        assert!(discovery_for_kind(AgentKind::Generic).is_none());
    }
}

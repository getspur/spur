use std::path::{Path, PathBuf};

use agent_client_protocol::schema::SessionInfo;
use anyhow::Result;

const CODEX_DISK_ROLLOUT_SCAN_LIMIT: usize = 500;

pub(super) fn filter_sessions_for_repo(
    sessions: Vec<SessionInfo>,
    repo_root: &Path,
) -> Vec<SessionInfo> {
    sessions
        .into_iter()
        .filter(|session| session.cwd.starts_with(repo_root))
        .collect()
}

pub(super) fn list_codex_sessions_from_disk_root(sessions_root: &Path) -> Result<Vec<SessionInfo>> {
    if !sessions_root.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    for path in codex_rollout_paths(sessions_root)? {
        if let Some(session) = parse_codex_rollout_header(&path) {
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

fn codex_rollout_paths(sessions_root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    'outer: for year_dir in sorted_child_dirs(sessions_root)? {
        for month_dir in sorted_child_dirs(&year_dir)? {
            for day_dir in sorted_child_dirs(&month_dir)? {
                for path in sorted_rollout_files(&day_dir)? {
                    paths.push(path);
                    if paths.len() >= CODEX_DISK_ROLLOUT_SCAN_LIMIT {
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

fn parse_codex_rollout_header(path: &Path) -> Option<SessionInfo> {
    use std::io::{BufRead, Read};

    const MAX_HEADER_BYTES: u64 = 1 << 20; // 1 MiB

    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file.take(MAX_HEADER_BYTES));
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }

    parse_codex_session_header(&line)
}

fn parse_codex_session_header(line: &str) -> Option<SessionInfo> {
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

fn json_string_field<'a>(json: &'a serde_json::Value, field: &str) -> Option<&'a str> {
    json.get(field).and_then(|value| value.as_str())
}

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
    fn filters_sessions_to_repo_root_prefix() {
        let repo_root = Path::new("/repo/spur");
        let sessions = vec![
            SessionInfo::new("root", "/repo/spur"),
            SessionInfo::new("worker", "/repo/spur/.spur/worktrees/worker-1"),
            SessionInfo::new("sibling-prefix", "/repo/spur-other"),
            SessionInfo::new("other", "/repo/other"),
        ];

        let filtered = filter_sessions_for_repo(sessions, repo_root);
        let ids: Vec<_> = filtered
            .iter()
            .map(|session| session.session_id.0.as_ref())
            .collect();

        assert_eq!(ids, vec!["root", "worker"]);
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

        let sessions =
            list_codex_sessions_from_disk_root(&sessions_root).expect("codex sessions from disk");

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
        let temp = tempfile::tempdir().expect("tempdir");
        let sessions_root = temp.path().join(".codex/sessions");
        let newest = sessions_root.join("2026/05/09");
        let next_newest = sessions_root.join("2026/05/08");
        let oldest = sessions_root.join("2026/05/07");
        std::fs::create_dir_all(&newest).expect("newest dir");
        std::fs::create_dir_all(&next_newest).expect("next newest dir");
        std::fs::create_dir_all(&oldest).expect("oldest dir");

        let newest_count = CODEX_DISK_ROLLOUT_SCAN_LIMIT / 2;
        let next_newest_count = CODEX_DISK_ROLLOUT_SCAN_LIMIT - newest_count;
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

        let sessions =
            list_codex_sessions_from_disk_root(&sessions_root).expect("codex sessions from disk");
        let ids: Vec<_> = sessions
            .iter()
            .map(|session| session.session_id.0.as_ref())
            .collect();

        assert_eq!(ids.len(), CODEX_DISK_ROLLOUT_SCAN_LIMIT);
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
        assert!(parse_codex_session_header(r#"{"payload":{"cwd":"/repo/spur"}}"#).is_none());
        assert!(
            parse_codex_session_header(r#"{"payload":{"id":"session-without-cwd"}}"#).is_none()
        );
    }
}

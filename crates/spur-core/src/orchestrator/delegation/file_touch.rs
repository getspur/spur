use agent_client_protocol::schema::{SessionNotification, SessionUpdate};

/// De-dup key for the 200ms file-touch window.
#[derive(Hash, Eq, PartialEq, Clone)]
pub(in crate::orchestrator) struct FileTouchKey {
    executor_id: String,
    path: std::path::PathBuf,
    kind: spur_acp::domain::events::FileTouchKind,
}

/// Per-worker-attempt de-dup for `WorkerFileTouched` synthesis.
/// Coalesces repeated ToolCall / ToolCallUpdate events for the same
/// (executor, path, kind) within a 200ms window, so a single logical
/// file operation emits at most one `WorkerFileTouched` per window.
///
/// Scope is a single `run_one_worker_attempt` invocation; cross-worker
/// coordination isn't needed because `executor_id` is unique per worker.
pub(in crate::orchestrator) struct FileTouchDedup {
    last_seen: std::sync::Mutex<std::collections::HashMap<FileTouchKey, std::time::Instant>>,
    ttl: std::time::Duration,
}

impl FileTouchDedup {
    pub(in crate::orchestrator) fn new() -> Self {
        Self {
            last_seen: std::sync::Mutex::new(std::collections::HashMap::new()),
            ttl: std::time::Duration::from_millis(200),
        }
    }

    /// Returns true if this (executor, path, kind) is fresh and should
    /// be emitted. Updates the last-seen map.
    fn should_emit(&self, key: &FileTouchKey) -> bool {
        let now = std::time::Instant::now();
        let mut map = self.last_seen.lock().unwrap();
        // Garbage collect stale entries opportunistically.
        map.retain(|_, t| now.duration_since(*t) < self.ttl * 5);
        match map.get(key) {
            Some(last) if now.duration_since(*last) < self.ttl => false,
            _ => {
                map.insert(key.clone(), now);
                true
            }
        }
    }
}

/// If `notification` is a ToolCall matching a known file-op tool name,
/// synthesize a WorkerFileTouched event (subject to dedup).
///
/// The `title` field of the ACP `ToolCall` struct carries the tool name
/// as populated by adapters (e.g. claude_events maps Anthropic's
/// `tool_use.name` into `title`). Path extraction tries `raw_input`'s
/// `path` / `file_path` fields first, then falls back to the first
/// entry in `locations` if raw_input is missing the key.
pub(in crate::orchestrator) fn maybe_synthesize_file_touch(
    notification: &SessionNotification,
    brain_session_id: &spur_acp::types::SessionId,
    executor_id: &str,
    dedup: &FileTouchDedup,
    funnel: &crate::event_funnel::FunnelHandle,
) {
    let tc = match &notification.update {
        SessionUpdate::ToolCall(tc) => tc,
        _ => return,
    };
    let kind = match tc.title.as_str() {
        "read_file" | "Read" => spur_acp::domain::events::FileTouchKind::Read,
        "write_file" | "Write" | "edit_file" | "Edit" => {
            spur_acp::domain::events::FileTouchKind::Write
        }
        _ => return,
    };
    // Prefer explicit raw_input path; fall back to first location entry.
    let path = tc
        .raw_input
        .as_ref()
        .and_then(|v| {
            v.get("path")
                .and_then(|p| p.as_str())
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    v.get("file_path")
                        .and_then(|p| p.as_str())
                        .map(std::path::PathBuf::from)
                })
        })
        .or_else(|| tc.locations.first().map(|loc| loc.path.clone()));
    let Some(path) = path else { return };
    let key = FileTouchKey {
        executor_id: executor_id.to_string(),
        path: path.clone(),
        kind,
    };
    if dedup.should_emit(&key) {
        funnel.emit(spur_acp::SpurEventBody::WorkerFileTouched {
            brain_session_id: brain_session_id.clone(),
            executor_id: executor_id.to_string(),
            path,
            kind,
        });
    }
}

//! App-level owner of per-executor `ReactTrace` instances. Receives
//! every `SpurEventBody::WorkerNotification` via `App::handle_spur_event`
//! (wired in Task 1.4) and routes the `SessionUpdate` through the shared
//! dispatcher (`react_trace::dispatch::dispatch_session_update`).
//!
//! ## Invariants
//! - This is the ONLY place per-executor streams are materialized.
//! - `ExecutorNode.stream_buffer` is retained for card summary
//!   compatibility but is NOT a rendering input for the Stream tab.
//! - On `ExecutorRetryStarted`, `reset(executor_id)` is called to match
//!   the lineage projection's `stream_buffer.clear()` semantics.

use std::collections::HashMap;

use spur_acp::{AgentKind, SessionUpdate};
use spur_core::lineage::types::{WorkerStreamEntry, WorkerStreamKind};

use crate::components::react_trace::dispatch::{dispatch_session_update, DispatchCtx};
use crate::components::react_trace::{ActStatus, ReactTrace, TraceEntry, TraceKind};

pub struct WorkerStreams {
    traces: HashMap<String, ReactTrace>,
    depths: HashMap<String, HashMap<String, u8>>,
    /// Remember the resolved `AgentKind` per executor so `reset` can
    /// rebuild the trace with the correct accent color without needing
    /// to peek inside `ReactTrace`.
    kinds: HashMap<String, AgentKind>,
}

impl WorkerStreams {
    pub fn new() -> Self {
        Self {
            traces: HashMap::new(),
            depths: HashMap::new(),
            kinds: HashMap::new(),
        }
    }

    /// Route a live `SessionUpdate` for `executor_id` into that
    /// executor's `ReactTrace`, creating the trace if needed.
    pub fn route(&mut self, executor_id: &str, agent_name: &str, update: &SessionUpdate) {
        let kind = AgentKind::from_name(agent_name);
        self.kinds.insert(executor_id.to_string(), kind);
        let trace = self
            .traces
            .entry(executor_id.to_string())
            .or_insert_with(|| ReactTrace::with_kind(kind));
        let depths = self.depths.entry(executor_id.to_string()).or_default();
        let mut ctx = DispatchCtx {
            agent_name,
            agent_kind: kind,
            now_stamp: now_stamp_hhmm,
            tool_depth: depths,
            skip_plan_trace: false,
        };
        dispatch_session_update(trace, update, &mut ctx);
    }

    /// Route a delegation-scoped free-form progress report into the executor's
    /// activity stream.
    pub fn route_progress(
        &mut self,
        executor_id: &str,
        agent_name: &str,
        message: &str,
        percent: Option<f64>,
    ) {
        let kind = AgentKind::from_name(agent_name);
        self.kinds.insert(executor_id.to_string(), kind);
        let trace = self
            .traces
            .entry(executor_id.to_string())
            .or_insert_with(|| ReactTrace::with_kind(kind));
        trace.push(TraceEntry {
            kind: TraceKind::Observe { payload: None },
            text: format_progress_report(message, percent),
            timestamp: now_stamp_hhmm(),
            #[cfg(feature = "markdown")]
            markdown: None,
        });
    }

    /// Advance the spinner frame on all live traces. Called from App's
    /// tick loop (Task 1.6) so Act entries with `Pending` / `InProgress`
    /// status animate consistently with the brain view.
    pub fn tick_all(&mut self) {
        for trace in self.traces.values_mut() {
            trace.tick();
        }
    }

    /// Seed a trace from persisted `stream_buffer` entries. Used on
    /// startup for executors that pre-date the current process.
    /// Produces coarse entries only — full fidelity resumes once live
    /// `WorkerNotification` events flow.
    pub fn seed_from_stream_buffer<'a, I>(
        &mut self,
        executor_id: &str,
        agent_name: &str,
        entries: I,
    ) where
        I: IntoIterator<Item = &'a WorkerStreamEntry>,
    {
        let kind = AgentKind::from_name(agent_name);
        self.kinds.insert(executor_id.to_string(), kind);
        let trace = self
            .traces
            .entry(executor_id.to_string())
            .or_insert_with(|| ReactTrace::with_kind(kind));
        for e in entries {
            let (entry_kind, text) = match e.kind {
                WorkerStreamKind::Thought => (TraceKind::Think, e.text.clone()),
                WorkerStreamKind::Message => (
                    TraceKind::AgentMessage {
                        agent: agent_name.to_string(),
                    },
                    e.text.clone(),
                ),
                WorkerStreamKind::ToolCall => (
                    TraceKind::Act {
                        tool: e.text.clone(),
                        family: spur_acp::adapter::ToolFamily::Unknown,
                        input: spur_acp::adapter::ToolInputDisplay::Empty,
                        tool_call_id: None,
                        status: ActStatus::Completed(None),
                    },
                    String::new(),
                ),
            };
            trace.push(TraceEntry {
                kind: entry_kind,
                text,
                timestamp: format_system_time(&e.occurred_at),
                #[cfg(feature = "markdown")]
                markdown: None,
            });
        }
    }

    pub fn get(&self, executor_id: &str) -> Option<&ReactTrace> {
        self.traces.get(executor_id)
    }

    pub fn get_mut(&mut self, executor_id: &str) -> Option<&mut ReactTrace> {
        self.traces.get_mut(executor_id)
    }

    /// Drop a trace when its executor is garbage-collected.
    pub fn remove(&mut self, executor_id: &str) {
        self.traces.remove(executor_id);
        self.depths.remove(executor_id);
        self.kinds.remove(executor_id);
    }

    /// Reset a trace on retry. Clears entries + tool-depth namespace
    /// but keeps the HashMap slot and remembered `AgentKind`, so the
    /// next `route` call reuses the same slot. Matches the lineage
    /// projection's `stream_buffer.clear()` on `ExecutorRetryStarted`.
    pub fn reset(&mut self, executor_id: &str) {
        if let Some(depths) = self.depths.get_mut(executor_id) {
            depths.clear();
        }
        let kind = self
            .kinds
            .get(executor_id)
            .copied()
            .unwrap_or(AgentKind::Generic);
        if let Some(slot) = self.traces.get_mut(executor_id) {
            *slot = ReactTrace::with_kind(kind);
        }
    }
}

impl Default for WorkerStreams {
    fn default() -> Self {
        Self::new()
    }
}

fn now_stamp_hhmm() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    format!("{:02}:{:02}", h, m)
}

fn format_system_time(t: &std::time::SystemTime) -> String {
    use std::time::UNIX_EPOCH;
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    format!("{:02}:{:02}", h, m)
}

fn format_progress_report(message: &str, percent: Option<f64>) -> String {
    match percent {
        Some(percent) => format!("progress {}%: {}", percent, message),
        None => format!("progress: {}", message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spur_acp::{ContentBlock, ContentChunk, SessionUpdate, TextContent};

    fn msg(text: &str) -> SessionUpdate {
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            text,
        ))))
    }

    #[test]
    fn route_creates_trace_on_first_notification() {
        let mut ws = WorkerStreams::new();
        ws.route("exec-1", "claude", &msg("hi"));
        assert!(ws.get("exec-1").is_some());
        assert_eq!(ws.get("exec-1").unwrap().entry_count(), 1);
    }

    #[test]
    fn route_progress_appends_observe_entry_with_percent() {
        let mut ws = WorkerStreams::new();
        ws.route_progress(
            "exec-1",
            "codex",
            "Running targeted verification",
            Some(67.5),
        );

        let trace = ws.get("exec-1").expect("trace");
        assert_eq!(trace.entry_count(), 1);
        let entry = &trace.entries()[0];
        assert!(matches!(entry.kind, TraceKind::Observe { .. }));
        assert_eq!(entry.text, "progress 67.5%: Running targeted verification");
    }

    #[test]
    fn route_multiple_executors_are_isolated() {
        let mut ws = WorkerStreams::new();
        ws.route("a", "claude", &msg("hi-a"));
        ws.route("b", "codex", &msg("hi-b1"));
        ws.route("b", "codex", &msg("hi-b2"));
        assert_eq!(ws.get("a").unwrap().entry_count(), 1);
        // "b" receives two messages, but append_message coalesces same-agent
        // consecutive chunks into one entry.
        assert_eq!(ws.get("b").unwrap().entry_count(), 1);
    }

    #[test]
    fn reset_clears_entries_and_depths_but_keeps_kind() {
        let mut ws = WorkerStreams::new();
        ws.route("exec-r", "claude", &msg("hi"));
        assert_eq!(ws.get("exec-r").unwrap().entry_count(), 1);
        ws.reset("exec-r");
        assert_eq!(
            ws.get("exec-r").unwrap().entry_count(),
            0,
            "reset clears entries"
        );
        ws.route("exec-r", "claude", &msg("hi-again"));
        assert_eq!(
            ws.get("exec-r").unwrap().entry_count(),
            1,
            "reset preserves slot for reuse"
        );
    }

    #[test]
    fn tick_all_advances_every_trace_without_panic() {
        let mut ws = WorkerStreams::new();
        ws.route("a", "claude", &msg("x"));
        ws.route("b", "codex", &msg("y"));
        ws.tick_all();
        ws.tick_all();
        assert!(ws.get("a").is_some());
        assert!(ws.get("b").is_some());
    }

    #[test]
    fn seed_from_stream_buffer_hydrates_pre_existing_entries() {
        use spur_core::lineage::types::{WorkerStreamEntry, WorkerStreamKind};
        use std::time::SystemTime;

        let mut ws = WorkerStreams::new();
        let entries = [
            WorkerStreamEntry {
                kind: WorkerStreamKind::Thought,
                text: "plan".into(),
                occurred_at: SystemTime::now(),
            },
            WorkerStreamEntry {
                kind: WorkerStreamKind::Message,
                text: "hi".into(),
                occurred_at: SystemTime::now(),
            },
        ];
        ws.seed_from_stream_buffer("exec-1", "claude", entries.iter());
        let t = ws.get("exec-1").expect("seeded trace");
        assert_eq!(t.entry_count(), 2);
    }
}

//! Session-event standardizer for the `pi` coding agent (`pi-acp`).
//!
//! Wire observations (pi-acp 0.0.33, pi 0.85.1, probed 2026-09-19):
//!
//! pi executes tools internally and streams command output exclusively
//! through a vendor extension on `tool_call_update` frames:
//!
//! ```json
//! {"sessionUpdate": "tool_call_update", "toolCallId": "call_x",
//!  "status": "in_progress",
//!  "_meta": {"terminal_output": {"terminal_id": "call_x", "data": "line-1\n"}}}
//! ```
//!
//! Completion arrives as:
//!
//! ```json
//! {"sessionUpdate": "tool_call_update", "toolCallId": "call_x",
//!  "status": "completed",
//!  "_meta": {"terminal_exit": {"terminal_id": "call_x", "exit_code": 0,
//!                              "signal": null}}}
//! ```
//!
//! The spec-compliant `content` blocks are never populated and
//! `raw_output` is never set, so a spec-compliant client renders nothing
//! for pi command output. The standardizer accumulates the chunked
//! `_meta.terminal_output.data` per `toolCallId` and re-emits each update
//! with a **cumulative** `raw_output` (the TUI replaces act text per
//! update, so cumulative output streams correctly), and maps
//! `_meta.terminal_exit` to `Completed` / `Failed` with an exit footer.

use std::collections::HashMap;

use agent_client_protocol::schema::v1::{
    SessionNotification, SessionUpdate, ToolCallStatus, ToolCallUpdate,
};
use serde_json::{json, Value};

/// Cap on accumulated output per tool call, matching the conservative
/// in-band display bound (the terminal side-channel default is larger).
const ACCUMULATOR_CAP_BYTES: usize = 64 * 1024;

/// Accumulated `_meta.terminal_output` text per tool call id.
#[derive(Debug, Default)]
pub struct SessionStandardizer {
    terminal_output: HashMap<String, String>,
}

impl SessionStandardizer {
    pub fn standardize(&mut self, mut notification: SessionNotification) -> SessionNotification {
        if let SessionUpdate::ToolCallUpdate(ref mut update) = notification.update {
            self.standardize_tool_call_update(update);
        }
        notification
    }

    fn standardize_tool_call_update(&mut self, update: &mut ToolCallUpdate) {
        let Some(meta) = update.meta.clone() else {
            return;
        };
        let key = update.tool_call_id.0.to_string();

        if let Some(data) = meta
            .get("terminal_output")
            .and_then(|v| v.get("data"))
            .and_then(Value::as_str)
        {
            let entry = self.terminal_output.entry(key.clone()).or_default();
            append_capped(entry, data, ACCUMULATOR_CAP_BYTES);
            update.fields.raw_output = Some(json!(entry.clone()));
            // Keep the chunk in `_meta` untouched: it is vendor diagnostic
            // data and other consumers MUST NOT depend on it.
        }

        if let Some(exit) = meta.get("terminal_exit") {
            let exit_code = exit.get("exit_code").and_then(Value::as_i64);
            let accumulated = self.terminal_output.remove(&key).unwrap_or_default();
            let (status, raw_output) = match exit_code {
                Some(0) | None => (ToolCallStatus::Completed, accumulated),
                Some(code) => (
                    ToolCallStatus::Failed,
                    format!("{accumulated}[exit code: {code}]"),
                ),
            };
            update.fields.status = Some(status);
            update.fields.raw_output = Some(json!(raw_output));
        }
    }
}

/// Append `data` keeping the accumulator at or below `cap` bytes,
/// trimming from the head on overflow at a char boundary.
fn append_capped(buffer: &mut String, data: &str, cap: usize) {
    buffer.push_str(data);
    if buffer.len() > cap {
        let mut cut = buffer.len() - cap;
        while cut < buffer.len() && !buffer.is_char_boundary(cut) {
            cut += 1;
        }
        let trimmed = buffer[cut..].to_string();
        *buffer = trimmed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{ToolCallId, ToolCallUpdateFields};

    fn tcu_frame(
        tool_call_id: &str,
        status: Option<ToolCallStatus>,
        meta: Value,
    ) -> SessionNotification {
        let mut fields = ToolCallUpdateFields::new();
        if let Some(status) = status {
            fields = fields.status(status);
        }
        let mut update = ToolCallUpdate::new(ToolCallId::new(tool_call_id), fields);
        update.meta = Some(
            serde_json::from_value(meta).expect("meta must deserialize into the extension map"),
        );
        SessionNotification::new("session", SessionUpdate::ToolCallUpdate(update))
    }

    fn raw_output_of(notification: &SessionNotification) -> Option<&Value> {
        match &notification.update {
            SessionUpdate::ToolCallUpdate(update) => update.fields.raw_output.as_ref(),
            _ => panic!("expected tool_call_update"),
        }
    }

    #[test]
    fn terminal_output_chunks_accumulate_into_cumulative_raw_output() {
        let mut standardizer = SessionStandardizer::default();
        let first = standardizer.standardize(tcu_frame(
            "call-1",
            Some(ToolCallStatus::InProgress),
            json!({"terminal_output": {"terminal_id": "call-1", "data": "line-1\n"}}),
        ));
        assert_eq!(raw_output_of(&first), Some(&json!("line-1\n")));

        let second = standardizer.standardize(tcu_frame(
            "call-1",
            Some(ToolCallStatus::InProgress),
            json!({"terminal_output": {"terminal_id": "call-1", "data": "line-2\n"}}),
        ));
        assert_eq!(
            raw_output_of(&second),
            Some(&json!("line-1\nline-2\n")),
            "TUI replaces act text per update, so raw_output must be cumulative"
        );
    }

    #[test]
    fn terminal_exit_zero_maps_completed_and_flushes_accumulator() {
        let mut standardizer = SessionStandardizer::default();
        standardizer.standardize(tcu_frame(
            "call-1",
            Some(ToolCallStatus::InProgress),
            json!({"terminal_output": {"terminal_id": "call-1", "data": "out\n"}}),
        ));
        let exit = standardizer.standardize(tcu_frame(
            "call-1",
            Some(ToolCallStatus::Completed),
            json!({"terminal_exit": {"terminal_id": "call-1", "exit_code": 0, "signal": null}}),
        ));
        match &exit.update {
            SessionUpdate::ToolCallUpdate(update) => {
                assert_eq!(update.fields.status, Some(ToolCallStatus::Completed));
                assert_eq!(update.fields.raw_output, Some(json!("out\n")));
            }
            _ => panic!("expected tool_call_update"),
        }
        assert!(standardizer.terminal_output.is_empty());
    }

    #[test]
    fn terminal_exit_nonzero_maps_failed_with_exit_footer() {
        let mut standardizer = SessionStandardizer::default();
        standardizer.standardize(tcu_frame(
            "call-2",
            Some(ToolCallStatus::InProgress),
            json!({"terminal_output": {"terminal_id": "call-2", "data": "boom\n"}}),
        ));
        let exit = standardizer.standardize(tcu_frame(
            "call-2",
            Some(ToolCallStatus::Completed),
            json!({"terminal_exit": {"terminal_id": "call-2", "exit_code": 2, "signal": null}}),
        ));
        match &exit.update {
            SessionUpdate::ToolCallUpdate(update) => {
                assert_eq!(update.fields.status, Some(ToolCallStatus::Failed));
                assert_eq!(
                    update.fields.raw_output,
                    Some(json!("boom\n[exit code: 2]"))
                );
            }
            _ => panic!("expected tool_call_update"),
        }
    }

    #[test]
    fn accumulator_trims_at_cap_on_char_boundary() {
        let mut standardizer = SessionStandardizer::default();
        let big = "é".repeat(40 * 1024); // 80 KiB of UTF-8
        standardizer.standardize(tcu_frame(
            "call-3",
            Some(ToolCallStatus::InProgress),
            json!({"terminal_output": {"terminal_id": "call-3", "data": big}}),
        ));
        // A follow-up chunk proves the accumulator was capped on the first push.
        let after = standardizer.standardize(tcu_frame(
            "call-3",
            Some(ToolCallStatus::InProgress),
            json!({"terminal_output": {"terminal_id": "call-3", "data": "x"}}),
        ));
        let raw = raw_output_of(&after)
            .and_then(Value::as_str)
            .expect("raw output");
        assert!(raw.len() <= ACCUMULATOR_CAP_BYTES + 1);
        assert!(raw.ends_with('x'));
    }

    #[test]
    fn updates_without_terminal_meta_pass_through_untouched() {
        let mut standardizer = SessionStandardizer::default();
        let plain = tcu_frame("call-4", Some(ToolCallStatus::Completed), json!({}));
        let out = standardizer.standardize(plain);
        match &out.update {
            SessionUpdate::ToolCallUpdate(update) => {
                assert_eq!(update.fields.status, Some(ToolCallStatus::Completed));
                assert_eq!(update.fields.raw_output, None);
            }
            _ => panic!("expected tool_call_update"),
        }
    }
}

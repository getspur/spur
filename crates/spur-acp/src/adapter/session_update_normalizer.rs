use std::collections::HashMap;

use agent_client_protocol::schema::{
    ContentBlock, SessionNotification, SessionUpdate, ToolCall, ToolCallContent, ToolCallId,
    ToolCallStatus, ToolCallUpdate, ToolKind,
};
use serde_json::{json, Value};

/// Normalizes ACP session updates into the field shapes Spur renderers prefer.
///
/// Some ACP agents stream tool arguments/results through replacement
/// `ToolCallContent` instead of the structured `rawInput`/`rawOutput` fields.
/// This stateful adapter preserves the original content while filling the
/// standard fields when it can infer them.
#[derive(Debug, Default)]
pub struct SessionUpdateNormalizer {
    tool_calls: HashMap<ToolCallId, ToolCall>,
}

impl SessionUpdateNormalizer {
    pub fn normalize(&mut self, mut notification: SessionNotification) -> SessionNotification {
        match &mut notification.update {
            SessionUpdate::ToolCall(tool_call) => {
                if matches!(tool_call.kind, ToolKind::Other) {
                    if let Some(kind) = infer_tool_kind(&tool_call.title) {
                        tool_call.kind = kind;
                    }
                }
                self.tool_calls
                    .insert(tool_call.tool_call_id.clone(), tool_call.clone());
            }
            SessionUpdate::ToolCallUpdate(update) => {
                self.normalize_tool_call_update(update);
            }
            _ => {}
        }
        notification
    }

    fn normalize_tool_call_update(&mut self, update: &mut ToolCallUpdate) {
        let prior = self.tool_calls.get(&update.tool_call_id).cloned();
        let status = update.fields.status;
        let terminal = matches!(
            status,
            Some(ToolCallStatus::Completed | ToolCallStatus::Failed)
        );

        if update.fields.kind.is_none() {
            let title = update
                .fields
                .title
                .as_deref()
                .or_else(|| prior.as_ref().map(|tool_call| tool_call.title.as_str()));
            let prior_kind = prior.as_ref().map(|tool_call| tool_call.kind);
            if let Some(kind) = prior_kind.filter(|kind| !matches!(kind, ToolKind::Other)) {
                update.fields.kind = Some(kind);
            } else if let Some(title) = title {
                update.fields.kind = infer_tool_kind(title);
            }
        }

        if !terminal && update.fields.raw_input.is_none() {
            if let Some(args) = update
                .fields
                .content
                .as_ref()
                .and_then(|content| tool_content_text(content))
                .and_then(|text| parse_json_object_or_array(text.trim()))
            {
                update.fields.raw_input = Some(args);
            }
        }

        if terminal && update.fields.raw_output.is_none() {
            if let Some(text) = update
                .fields
                .content
                .as_ref()
                .and_then(|content| tool_content_text(content))
                .filter(|text| !text.is_empty())
            {
                let raw_input = update.fields.raw_input.as_ref().or_else(|| {
                    prior
                        .as_ref()
                        .and_then(|tool_call| tool_call.raw_input.as_ref())
                });
                update.fields.raw_output = Some(content_text_to_raw_output(
                    &text,
                    raw_input,
                    matches!(status, Some(ToolCallStatus::Failed)),
                ));
            }
        }

        let terminal_after_update = matches!(
            update.fields.status,
            Some(ToolCallStatus::Completed | ToolCallStatus::Failed)
        );
        if let Some(existing) = self.tool_calls.get_mut(&update.tool_call_id) {
            existing.update(update.fields.clone());
        } else if let Ok(tool_call) = ToolCall::try_from(update.clone()) {
            self.tool_calls
                .insert(update.tool_call_id.clone(), tool_call);
        }
        if terminal_after_update {
            self.tool_calls.remove(&update.tool_call_id);
        }
    }
}

fn tool_content_text(content: &[ToolCallContent]) -> Option<String> {
    let mut out = String::new();
    for item in content {
        if let ToolCallContent::Content(content) = item {
            if let ContentBlock::Text(text) = &content.content {
                out.push_str(&text.text);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn parse_json_object_or_array(text: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(text).ok()?;
    match value {
        Value::Object(_) | Value::Array(_) => Some(value),
        _ => None,
    }
}

fn content_text_to_raw_output(text: &str, raw_input: Option<&Value>, failed: bool) -> Value {
    if failed {
        return json!({
            "error": true,
            "message": text,
        });
    }

    if let Some(path) = raw_input.and_then(path_from_raw_input) {
        return json!({
            "path": path,
            "content": text,
        });
    }

    Value::String(text.to_string())
}

fn path_from_raw_input(raw_input: &Value) -> Option<&str> {
    let obj = raw_input.as_object()?;
    for key in ["path", "file_path", "file", "filename", "target"] {
        if let Some(path) = obj.get(key).and_then(Value::as_str) {
            return Some(path);
        }
    }
    None
}

fn infer_tool_kind(title: &str) -> Option<ToolKind> {
    let normalized: String = title
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();

    if normalized.starts_with("read") || normalized.contains("readfile") {
        return Some(ToolKind::Read);
    }
    if normalized.starts_with("write")
        || normalized.starts_with("edit")
        || normalized.contains("writefile")
        || normalized.contains("editfile")
        || normalized.contains("replace")
    {
        return Some(ToolKind::Edit);
    }
    if normalized.starts_with("search")
        || normalized.starts_with("grep")
        || normalized.starts_with("find")
    {
        return Some(ToolKind::Search);
    }
    if normalized.starts_with("bash")
        || normalized.starts_with("shell")
        || normalized.starts_with("terminal")
        || normalized.starts_with("execute")
        || normalized.starts_with("run")
    {
        return Some(ToolKind::Execute);
    }
    if normalized.starts_with("fetch") || normalized.starts_with("webfetch") {
        return Some(ToolKind::Fetch);
    }
    None
}

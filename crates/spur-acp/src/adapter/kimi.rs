use std::collections::HashMap;

use agent_client_protocol::schema::{
    ContentBlock, SessionNotification, SessionUpdate, ToolCall, ToolCallContent, ToolCallId,
    ToolCallStatus, ToolCallUpdate, ToolKind,
};
use serde_json::{json, Value};

use super::{generic, ObservePayload, SpurToolMeta, ToolFamily, ToolInputDisplay};

/// Kimi streams ACP tool arguments/results through replacement content chunks.
/// Keep that vendor behavior contained here so downstream renderers receive
/// ordinary ACP fields when possible.
#[derive(Debug, Default)]
pub struct SessionStandardizer {
    tool_calls: HashMap<(String, ToolCallId), ToolCall>,
}

impl SessionStandardizer {
    pub fn standardize(&mut self, mut notification: SessionNotification) -> SessionNotification {
        let session_id = notification.session_id.to_string();
        match &mut notification.update {
            SessionUpdate::ToolCall(tool_call) => {
                standardize_tool_call(tool_call);
                let key = (session_id, tool_call.tool_call_id.clone());
                if is_terminal(tool_call.status) {
                    synthesize_terminal_raw_output(
                        tool_call.status,
                        &tool_call.content,
                        tool_call.raw_input.as_ref(),
                        Some(tool_call.kind),
                        &mut tool_call.raw_output,
                    );
                    self.tool_calls.remove(&key);
                } else {
                    self.tool_calls.insert(key, tool_call.clone());
                }
            }
            SessionUpdate::ToolCallUpdate(update) => {
                self.standardize_tool_call_update(session_id, update);
            }
            _ => {}
        }
        notification
    }

    fn standardize_tool_call_update(&mut self, session_id: String, update: &mut ToolCallUpdate) {
        let key = (session_id, update.tool_call_id.clone());
        let prior = self.tool_calls.get(&key).cloned();
        let terminal = update.fields.status.is_some_and(is_terminal);

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

        if terminal {
            let raw_input = update.fields.raw_input.as_ref().or_else(|| {
                prior
                    .as_ref()
                    .and_then(|tool_call| tool_call.raw_input.as_ref())
            });
            let effective_kind = update
                .fields
                .kind
                .or_else(|| prior.as_ref().map(|tool_call| tool_call.kind));
            synthesize_terminal_raw_output(
                update.fields.status.unwrap_or(ToolCallStatus::Completed),
                update.fields.content.as_deref().unwrap_or_default(),
                raw_input,
                effective_kind,
                &mut update.fields.raw_output,
            );
        }

        if let Some(existing) = self.tool_calls.get_mut(&key) {
            existing.update(update.fields.clone());
        } else if let Ok(tool_call) = ToolCall::try_from(update.clone()) {
            self.tool_calls.insert(key.clone(), tool_call);
        }
        if terminal {
            self.tool_calls.remove(&key);
        }
    }
}

pub fn refine(title: &str, base: ToolFamily) -> ToolFamily {
    let base = if matches!(base, ToolFamily::Unknown) {
        infer_tool_kind(title).map(ToolFamily::from).unwrap_or(base)
    } else {
        base
    };
    generic::refine(title, base)
}

pub fn try_format_input(raw: &Value) -> Option<ToolInputDisplay> {
    let obj = raw.as_object()?;
    for key in ["path", "file_path", "file", "filename", "target"] {
        if let Some(path) = obj.get(key).and_then(Value::as_str) {
            return Some(ToolInputDisplay::Path(path.to_string()));
        }
    }
    if let Some(description) = obj.get("description").and_then(Value::as_str) {
        return Some(ToolInputDisplay::Text(description.to_string()));
    }
    if let Some(prompt) = obj.get("prompt").and_then(Value::as_str) {
        return Some(ToolInputDisplay::Text(prompt.to_string()));
    }
    None
}

pub fn try_extract_observe(raw: &Value) -> Option<ObservePayload> {
    let obj = raw.as_object()?;
    let content = obj.get("content").and_then(Value::as_str)?;
    Some(ObservePayload::FileRead {
        path: obj.get("path").and_then(Value::as_str).map(str::to_string),
        content: content.to_string(),
        truncated: obj
            .get("__truncated__")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

pub fn extract_tool_meta(_tc: &ToolCall) -> SpurToolMeta {
    SpurToolMeta::default()
}

fn standardize_tool_call(tool_call: &mut ToolCall) {
    if matches!(tool_call.kind, ToolKind::Other) {
        if let Some(kind) = infer_tool_kind(&tool_call.title) {
            tool_call.kind = kind;
        }
    }
}

fn synthesize_terminal_raw_output(
    status: ToolCallStatus,
    content: &[ToolCallContent],
    raw_input: Option<&Value>,
    kind: Option<ToolKind>,
    raw_output: &mut Option<Value>,
) {
    if raw_output.is_some() {
        return;
    }
    let Some(text) = tool_content_text(content).filter(|text| !text.is_empty()) else {
        return;
    };
    *raw_output = Some(content_text_to_raw_output(
        &text,
        raw_input,
        kind,
        matches!(status, ToolCallStatus::Failed),
    ));
}

fn is_terminal(status: ToolCallStatus) -> bool {
    matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed)
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

fn content_text_to_raw_output(
    text: &str,
    raw_input: Option<&Value>,
    kind: Option<ToolKind>,
    failed: bool,
) -> Value {
    if failed {
        return json!({
            "error": true,
            "message": text,
        });
    }

    if matches!(kind, Some(ToolKind::Read)) {
        if let Some(path) = raw_input.and_then(path_from_raw_input) {
            return json!({
                "path": path,
                "content": text,
            });
        }
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

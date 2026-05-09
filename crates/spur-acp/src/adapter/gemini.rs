use agent_client_protocol::schema::{
    Content, ContentBlock, SessionNotification, SessionUpdate, TextContent, ToolCall,
    ToolCallContent, ToolCallUpdate, ToolKind,
};
use serde_json::{json, Value};

use super::{generic, ObservePayload, SpurToolMeta, ToolFamily, ToolInputDisplay};

/// Gemini's ACP `invoke_agent` frame carries its useful native-subagent
/// identity only in the title and leaves content/raw input empty. Normalize
/// that title-derived identity into ordinary ACP fields so downstream renderers
/// do not need Gemini-specific title parsing.
#[derive(Debug, Default)]
pub struct SessionStandardizer;

impl SessionStandardizer {
    pub fn standardize(&mut self, mut notification: SessionNotification) -> SessionNotification {
        match &mut notification.update {
            SessionUpdate::ToolCall(tool_call) => standardize_tool_call(tool_call),
            SessionUpdate::ToolCallUpdate(update) => standardize_tool_call_update(update),
            _ => {}
        }
        notification
    }
}

pub fn refine(title: &str, base: ToolFamily) -> ToolFamily {
    let base = if delegate_agent_name(title).is_some() && matches!(base, ToolFamily::Unknown) {
        ToolFamily::Think
    } else {
        base
    };
    generic::refine(title, base)
}

pub fn try_format_input(raw: &Value) -> Option<ToolInputDisplay> {
    let obj = raw.as_object()?;
    if let Some(agent) = obj.get("agent").and_then(Value::as_str) {
        return Some(ToolInputDisplay::Text(format!("agent: {agent}")));
    }
    if let Some(description) = obj.get("description").and_then(Value::as_str) {
        return Some(ToolInputDisplay::Text(description.to_string()));
    }
    None
}

pub fn try_extract_observe(_raw: &Value) -> Option<ObservePayload> {
    None
}

pub fn extract_tool_meta(_tc: &ToolCall) -> SpurToolMeta {
    SpurToolMeta::default()
}

fn standardize_tool_call(tool_call: &mut ToolCall) {
    let Some(agent) = delegate_agent_name(&tool_call.title).map(str::to_string) else {
        return;
    };
    if matches!(tool_call.kind, ToolKind::Other) {
        tool_call.kind = ToolKind::Think;
    }
    fill_delegate_fields(
        &tool_call.title,
        &agent,
        &mut tool_call.raw_input,
        &mut tool_call.content,
    );
}

fn standardize_tool_call_update(update: &mut ToolCallUpdate) {
    let Some(title) = update.fields.title.as_deref() else {
        return;
    };
    let Some(agent) = delegate_agent_name(title).map(str::to_string) else {
        return;
    };
    if matches!(update.fields.kind, None | Some(ToolKind::Other)) {
        update.fields.kind = Some(ToolKind::Think);
    }
    fill_delegate_fields(
        title,
        &agent,
        &mut update.fields.raw_input,
        update.fields.content.get_or_insert_with(Vec::new),
    );
}

fn fill_delegate_fields(
    title: &str,
    agent: &str,
    raw_input: &mut Option<Value>,
    content: &mut Vec<ToolCallContent>,
) {
    if raw_input.is_none() {
        *raw_input = Some(json!({
            "agent": agent,
            "description": title,
        }));
    }
    if content.is_empty() {
        content.push(text_tool_content(format!("agent: {agent}")));
    }
}

fn text_tool_content(text: String) -> ToolCallContent {
    ToolCallContent::Content(Content::new(ContentBlock::Text(TextContent::new(text))))
}

fn delegate_agent_name(title: &str) -> Option<&str> {
    let rest = title.strip_prefix("Delegating to agent '")?;
    let end = rest.find('\'')?;
    let agent = &rest[..end];
    if agent.is_empty() {
        None
    } else {
        Some(agent)
    }
}

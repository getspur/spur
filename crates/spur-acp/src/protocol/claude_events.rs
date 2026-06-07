//! Serde types for Claude Code's stream-json protocol.
//!
//! Claude Code emits newline-delimited JSON on stdout when invoked with
//! `--output-format stream-json`. This module defines the event types,
//! a parser, and a mapper to ACP `SessionNotification` types.

use agent_client_protocol::schema::{
    ContentBlock, ContentChunk, SessionId, SessionNotification, SessionUpdate, TextContent,
    ToolCall as AcpToolCall, ToolCallId, ToolCallUpdate as AcpToolCallUpdate, ToolCallUpdateFields,
};
use serde::Deserialize;

// ─── Claude stream-json event types ────────────────────────────────────

/// Top-level event from Claude Code's stream-json stdout.
/// Each line is one JSON object matching one of these variants.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ClaudeEvent {
    #[serde(rename = "system")]
    System(SystemEvent),
    #[serde(rename = "assistant")]
    Assistant(AssistantEvent),
    #[serde(rename = "result")]
    Result(ResultEvent),
}

/// Emitted once at startup. Contains session metadata.
#[derive(Debug, Deserialize)]
pub struct SystemEvent {
    pub subtype: String,
    pub session_id: String,
    pub model: Option<String>,
    pub tools: Option<Vec<String>>,
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<String>,
}

/// An assistant response containing one or more content blocks.
#[derive(Debug, Deserialize)]
pub struct AssistantEvent {
    pub message: AssistantMessage,
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AssistantMessage {
    pub content: Vec<AssistantContentBlock>,
}

/// Content block types within an assistant message.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum AssistantContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
    },
}

/// End-of-turn event with cost and usage data.
#[derive(Debug, Deserialize)]
pub struct ResultEvent {
    pub subtype: String,
    pub is_error: Option<bool>,
    pub duration_ms: Option<u64>,
    pub result: Option<String>,
    pub total_cost_usd: Option<f64>,
    pub session_id: String,
}

// ─── Stdin message type ─────────────────────────────────────────────────

/// User message written to Claude's stdin in stream-json input mode.
#[derive(Debug, serde::Serialize)]
pub struct UserMessage<'a> {
    #[serde(rename = "type")]
    msg_type: &'static str,
    pub content: &'a str,
}

impl<'a> UserMessage<'a> {
    pub fn new(content: &'a str) -> Self {
        Self {
            msg_type: "user",
            content,
        }
    }
}

// ─── Parsing ────────────────────────────────────────────────────────────

/// Parse a single stdout line into a `ClaudeEvent`.
/// Returns Err for unknown or malformed event types.
pub fn parse_event(line: &str) -> Result<ClaudeEvent, serde_json::Error> {
    serde_json::from_str(line)
}

// ─── Mapping to ACP types ───────────────────────────────────────────────

/// Map a `ClaudeEvent` to zero or more ACP `SessionNotifications`.
///
/// System and Result events are handled internally by the adapter
/// and produce no notifications.
pub fn map_to_notifications(
    event: &ClaudeEvent,
    session_id: &SessionId,
) -> Vec<SessionNotification> {
    match event {
        ClaudeEvent::Assistant(evt) => evt
            .message
            .content
            .iter()
            .map(|block| {
                let update = match block {
                    AssistantContentBlock::Text { text } => {
                        let chunk =
                            ContentChunk::new(ContentBlock::Text(TextContent::new(text.clone())));
                        SessionUpdate::AgentMessageChunk(chunk)
                    }
                    AssistantContentBlock::Thinking { thinking } => {
                        let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(
                            thinking.clone(),
                        )));
                        SessionUpdate::AgentThoughtChunk(chunk)
                    }
                    AssistantContentBlock::ToolUse { id, name, input } => {
                        let mut tc = AcpToolCall::new(ToolCallId::new(id.as_str()), name.clone());
                        tc.raw_input = Some(input.clone());
                        SessionUpdate::ToolCall(tc)
                    }
                    AssistantContentBlock::ToolResult {
                        tool_use_id,
                        content,
                    } => {
                        let fields = ToolCallUpdateFields::new().raw_output(content.clone());
                        let tcu =
                            AcpToolCallUpdate::new(ToolCallId::new(tool_use_id.as_str()), fields);
                        SessionUpdate::ToolCallUpdate(tcu)
                    }
                };
                SessionNotification::new(session_id.clone(), update)
            })
            .collect(),
        ClaudeEvent::System(_) | ClaudeEvent::Result(_) => vec![],
    }
}

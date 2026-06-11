use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Streamed to the frontend over a Tauri Channel, one per agent notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatEvent {
    MessageChunk {
        text: String,
    },
    ToolCall {
        name: String,
        args_summary: String,
    },
    ToolResult {
        summary: String,
    },
    PermissionRequest {
        id: String,
        title: String,
        options: Vec<PermissionOptionView>,
    },
    Usage {
        input: Option<u64>,
        output: Option<u64>,
    },
    Done,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOptionView {
    pub id: String,
    pub label: String,
}

/// The scope a session is created with. `mcp_servers`/`skill` come from the app.
#[derive(Debug, Clone)]
pub struct AppScope {
    pub cwd: PathBuf,
    pub mcp_servers: Vec<agent_client_protocol::schema::McpServer>,
    pub skill: Option<String>,
    /// Stable key used to map app -> live session (app dir path, or "notebook").
    pub app_key: String,
    /// Display label for the chat header.
    pub label: String,
}

/// Identifies which app session a turn targets.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRef {
    pub app_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_event_message_chunk_roundtrips_camel_case() {
        let ev = ChatEvent::MessageChunk { text: "hi".into() };
        let json = serde_json::to_value(&ev).unwrap();

        assert_eq!(json, json!({ "type": "messageChunk", "text": "hi" }));

        let back: ChatEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn chat_event_permission_request_roundtrips_camel_case() {
        let ev = ChatEvent::PermissionRequest {
            id: "perm-1".into(),
            title: "Allow notebook edit?".into(),
            options: vec![
                PermissionOptionView {
                    id: "allow".into(),
                    label: "Allow".into(),
                },
                PermissionOptionView {
                    id: "deny".into(),
                    label: "Deny".into(),
                },
            ],
        };
        let json = serde_json::to_value(&ev).unwrap();

        assert_eq!(
            json,
            json!({
                "type": "permissionRequest",
                "id": "perm-1",
                "title": "Allow notebook edit?",
                "options": [
                    { "id": "allow", "label": "Allow" },
                    { "id": "deny", "label": "Deny" }
                ]
            })
        );

        let back: ChatEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn chat_event_variants_roundtrip_camel_case() {
        let events = vec![
            ChatEvent::ToolCall {
                name: "notebook_read_cell".into(),
                args_summary: "cell 1".into(),
            },
            ChatEvent::ToolResult {
                summary: "read 32 lines".into(),
            },
            ChatEvent::Usage {
                input: Some(10),
                output: Some(4),
            },
            ChatEvent::Done,
            ChatEvent::Error {
                message: "agent disconnected".into(),
            },
        ];

        for ev in events {
            let json = serde_json::to_value(&ev).unwrap();
            let back: ChatEvent = serde_json::from_value(json).unwrap();
            assert_eq!(back, ev);
        }
    }

    #[test]
    fn session_ref_roundtrips() {
        let session_ref = SessionRef {
            app_key: "notebook".into(),
        };
        let json = serde_json::to_value(&session_ref).unwrap();

        assert_eq!(json, json!({ "app_key": "notebook" }));

        let back: SessionRef = serde_json::from_value(json).unwrap();
        assert_eq!(back, session_ref);
    }
}

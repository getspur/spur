use std::path::PathBuf;
use std::sync::Arc;

use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, PromptRequest, ProtocolVersion, SessionId, SessionUpdate,
    TextContent,
};
use futures::StreamExt;
use spur_acp::connection::AgentConnection;
use tokio::sync::Mutex;

use crate::dag::ai::{AiError, AiNodeBackend, AiRunOutput, AiRunRequest};

/// Tier-1 AI backend: one ACP session per notebook, one prompt turn per run.
pub struct AcpAgentBackend {
    conn: Arc<Mutex<dyn AgentConnection>>,
    cwd: PathBuf,
    session_id: Mutex<Option<SessionId>>,
}

impl AcpAgentBackend {
    pub fn new(conn: Arc<Mutex<dyn AgentConnection>>, cwd: PathBuf) -> Self {
        Self {
            conn,
            cwd,
            session_id: Mutex::new(None),
        }
    }

    async fn ensure_session(&self) -> Result<SessionId, AiError> {
        let mut session_id = self.session_id.lock().await;
        if let Some(existing) = session_id.as_ref() {
            return Ok(existing.clone());
        }

        let mut conn = self.conn.lock().await;
        conn.initialize(InitializeRequest::new(ProtocolVersion::LATEST))
            .await
            .map_err(|err| AiError::Init(err.to_string()))?;
        let response = conn
            .new_session(self.cwd.clone(), Vec::new())
            .await
            .map_err(|err| AiError::Init(err.to_string()))?;

        *session_id = Some(response.session_id.clone());
        Ok(response.session_id)
    }
}

#[async_trait::async_trait]
impl AiNodeBackend for AcpAgentBackend {
    async fn run(&self, req: AiRunRequest) -> Result<AiRunOutput, AiError> {
        let session_id = self.ensure_session().await?;
        if req.cancel.is_cancelled() {
            return Err(AiError::Cancelled);
        }

        let mut prompt = String::new();
        for context in &req.context {
            prompt.push_str("## Context: ");
            prompt.push_str(&context.port);
            prompt.push('\n');
            prompt.push_str(&context.rendered);
            prompt.push_str("\n\n");
        }
        prompt.push_str(&req.prompt);

        let prompt_request = PromptRequest::new(
            session_id.clone(),
            vec![ContentBlock::Text(TextContent::new(prompt))],
        );

        let mut conn = self.conn.lock().await;
        let mut stream = conn
            .prompt(prompt_request)
            .await
            .map_err(|err| AiError::Prompt(err.to_string()))?;

        let mut text = String::new();
        loop {
            tokio::select! {
                _ = req.cancel.cancelled() => {
                    let _ = conn.cancel(session_id.0.as_ref()).await;
                    return Err(AiError::Cancelled);
                }
                item = stream.next() => {
                    match item {
                        Some(notification) => {
                            if let SessionUpdate::AgentMessageChunk(chunk) = notification.update {
                                if let ContentBlock::Text(text_content) = chunk.content {
                                    text.push_str(&text_content.text);
                                }
                            }
                        }
                        None => break,
                    }
                }
            }
        }

        Ok(AiRunOutput { text, usage: None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag::ai::{AiNodeBackend, AiRunRequest};
    use agent_client_protocol::schema::{
        ContentBlock, ContentChunk, InitializeRequest, InitializeResponse, McpServer,
        NewSessionResponse, PromptRequest, ProtocolVersion, SessionId, SessionNotification,
        SessionUpdate, TextContent,
    };
    use async_trait::async_trait;
    use spur_acp::connection::AgentConnection;
    use spur_acp::types::AgentHealth;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    struct FakeConn {
        lines: Vec<String>,
    }

    impl FakeConn {
        fn with_lines(lines: impl IntoIterator<Item = impl Into<String>>) -> Self {
            Self {
                lines: lines.into_iter().map(Into::into).collect(),
            }
        }
    }

    #[async_trait]
    impl AgentConnection for FakeConn {
        async fn initialize(
            &mut self,
            _request: InitializeRequest,
        ) -> anyhow::Result<InitializeResponse> {
            Ok(InitializeResponse::new(ProtocolVersion::LATEST))
        }

        async fn new_session(
            &mut self,
            _cwd: PathBuf,
            _mcp_servers: Vec<McpServer>,
        ) -> anyhow::Result<NewSessionResponse> {
            Ok(NewSessionResponse::new(SessionId::new("test-session")))
        }

        async fn prompt(
            &mut self,
            request: PromptRequest,
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = SessionNotification> + Send>>>
        {
            let lines = self.lines.clone();
            let notifications = lines.into_iter().map(move |line| {
                let chunk = ContentChunk::new(ContentBlock::Text(TextContent::new(line)));
                SessionNotification::new(
                    request.session_id.clone(),
                    SessionUpdate::AgentMessageChunk(chunk),
                )
            });
            Ok(Box::pin(futures::stream::iter(notifications)))
        }

        async fn cancel(&mut self, _session_id: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn shutdown(&mut self) -> anyhow::Result<()> {
            Ok(())
        }

        fn health(&self) -> AgentHealth {
            AgentHealth::Ready
        }
    }

    #[tokio::test]
    async fn drains_prompt_stream_to_text() {
        let conn: Arc<Mutex<dyn spur_acp::connection::AgentConnection>> =
            Arc::new(Mutex::new(FakeConn::with_lines(["Hello, ", "world"])));
        let backend = AcpAgentBackend::new(conn, std::path::PathBuf::from("/tmp/nb"));
        let out = backend
            .run(AiRunRequest {
                cell_id: "c1".into(),
                prompt: "say hi".into(),
                context: vec![],
                cancel: CancellationToken::new(),
            })
            .await
            .unwrap();
        assert_eq!(out.text, "Hello, world");
    }
}

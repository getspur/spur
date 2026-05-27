#![cfg(madsim)]

extern crate madsim_tokio as tokio;

#[path = "../src/notification_drain.rs"]
mod notification_drain;

use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use agent_client_protocol::schema::{
    AvailableCommand, AvailableCommandsUpdate, ContentBlock, InitializeRequest, InitializeResponse,
    McpServer, NewSessionResponse, PromptRequest, SessionId, SessionNotification, SessionUpdate,
    TextContent,
};
use async_trait::async_trait;
use futures::Stream;
use spur_acp::connection::AgentConnection;
use spur_acp::types::AgentHealth;
use tokio::sync::broadcast;

struct BroadcastPromptConnection {
    tx: broadcast::Sender<SessionNotification>,
}

#[async_trait]
impl AgentConnection for BroadcastPromptConnection {
    async fn initialize(
        &mut self,
        _request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse> {
        unimplemented!("not needed for notification drain test")
    }

    async fn new_session(
        &mut self,
        _cwd: PathBuf,
        _mcp_servers: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        unimplemented!("not needed for notification drain test")
    }

    async fn prompt(
        &mut self,
        _request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        Ok(Box::pin(futures::stream::empty()))
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

    fn subscribe_session_notifications(&self) -> Option<broadcast::Receiver<SessionNotification>> {
        Some(self.tx.subscribe())
    }
}

#[test]
fn broadcast_before_grace_deadline_survives_after_deadline_is_dropped() {
    madsim::runtime::Builder::from_env().run(|| async {
        if let Ok(seed) = std::env::var("MADSIM_TEST_SEED") {
            assert_eq!(
                madsim::runtime::Handle::current().seed(),
                seed.parse::<u64>().expect("MADSIM_TEST_SEED must be u64"),
            );
        }

        let (tx, _rx) = broadcast::channel(16);
        let mut connection = BroadcastPromptConnection { tx: tx.clone() };

        let before_tx = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(99)).await;
            let _ = before_tx.send(notification("before-grace"));
        });

        let after_tx = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(101)).await;
            let _ = after_tx.send(notification("after-grace"));
        });

        let mut received = Vec::new();
        notification_drain::drive_prompt_notifications(
            &mut connection,
            prompt_request(),
            |notif| received.push(command_name(&notif).to_string()),
        )
        .await
        .expect("drain should complete after grace deadline");

        assert_eq!(
            received,
            vec!["before-grace"],
            "only notifications emitted before the 100 ms grace deadline should be delivered",
        );
    });
}

fn prompt_request() -> PromptRequest {
    PromptRequest::new(
        SessionId::new("madsim-session".to_string()),
        vec![ContentBlock::Text(TextContent::new("prompt".to_string()))],
    )
}

fn notification(name: &str) -> SessionNotification {
    SessionNotification::new(
        SessionId::new("madsim-session".to_string()),
        SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate::new(vec![
            AvailableCommand::new(name, "test marker"),
        ])),
    )
}

fn command_name(notif: &SessionNotification) -> &str {
    match &notif.update {
        SessionUpdate::AvailableCommandsUpdate(update) => {
            update.available_commands[0].name.as_str()
        }
        other => panic!("expected AvailableCommandsUpdate, got {other:?}"),
    }
}

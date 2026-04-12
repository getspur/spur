//! `StreamJsonAdapter` — connects to Claude Code via bidirectional stream-json.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::unfold;
use futures::Stream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::mpsc;

use agent_client_protocol::{
    InitializeRequest, InitializeResponse, McpServer, NewSessionResponse,
    PromptRequest, ProtocolVersion, SessionId, SessionNotification, ContentBlock,
};

use crate::connection::AgentConnection;
use crate::protocol::claude_events::{parse_event, map_to_notifications, ClaudeEvent, UserMessage};
use crate::types::AgentHealth;

/// Connects to Claude Code via its bidirectional stream-json protocol.
pub struct StreamJsonAdapter {
    agent_name: String,
    command: String,
    extra_args: Vec<String>,
    child: Option<Child>,
    stdin: Option<BufWriter<ChildStdin>>,
    sender_tx: Option<mpsc::Sender<mpsc::Sender<SessionNotification>>>,
    session_id: Option<SessionId>,
    model: Option<String>,
    total_cost: Arc<Mutex<f64>>,
    health_status: AgentHealth,
}

impl StreamJsonAdapter {
    pub fn new(
        agent_name: impl Into<String>,
        command: impl Into<String>,
        extra_args: Vec<String>,
    ) -> Self {
        Self {
            agent_name: agent_name.into(),
            command: command.into(),
            extra_args,
            child: None,
            stdin: None,
            sender_tx: None,
            session_id: None,
            model: None,
            total_cost: Arc::new(Mutex::new(0.0)),
            health_status: AgentHealth::Unknown,
        }
    }

    pub fn total_cost(&self) -> f64 {
        *self.total_cost.lock().unwrap()
    }
}

#[async_trait]
impl AgentConnection for StreamJsonAdapter {
    async fn initialize(
        &mut self,
        _request: InitializeRequest,
    ) -> anyhow::Result<InitializeResponse> {
        tracing::debug!(
            agent = %self.agent_name,
            command = %self.command,
            "StreamJsonAdapter: spawning Claude Code process"
        );

        let mut child = tokio::process::Command::new(&self.command)
            .args(&self.extra_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                self.health_status = AgentHealth::Error(format!("Failed to spawn: {e}"));
                anyhow::anyhow!(
                    "StreamJsonAdapter '{}': failed to spawn '{}': {e}",
                    self.agent_name, self.command
                )
            })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            anyhow::anyhow!("StreamJsonAdapter '{}': failed to capture stdin", self.agent_name)
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            anyhow::anyhow!("StreamJsonAdapter '{}': failed to capture stdout", self.agent_name)
        })?;

        self.stdin = Some(BufWriter::new(stdin));
        self.child = Some(child);

        // Read first line — expect system/init event.
        let mut reader = BufReader::new(stdout);
        let mut first_line = String::new();
        reader.read_line(&mut first_line).await.map_err(|e| {
            anyhow::anyhow!("StreamJsonAdapter '{}': failed to read init: {e}", self.agent_name)
        })?;

        let init_event = parse_event(first_line.trim()).map_err(|e| {
            anyhow::anyhow!(
                "StreamJsonAdapter '{}': failed to parse init: {e}\nLine: {}",
                self.agent_name, first_line.trim()
            )
        })?;

        let (session_id_str, model) = match init_event {
            ClaudeEvent::System(ref sys) if sys.subtype == "init" => {
                (sys.session_id.clone(), sys.model.clone())
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "StreamJsonAdapter '{}': expected system/init, got: {}",
                    self.agent_name, first_line.trim()
                ));
            }
        };

        let session_id = SessionId::new(session_id_str);
        self.session_id = Some(session_id.clone());
        self.model = model;

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            "StreamJsonAdapter: initialized, spawning background reader"
        );

        // Spawn persistent background reader.
        let (sender_tx, sender_rx) = mpsc::channel::<mpsc::Sender<SessionNotification>>(4);
        self.sender_tx = Some(sender_tx);

        let agent_name = self.agent_name.clone();
        let cost = Arc::clone(&self.total_cost);
        let bg_session_id = session_id.clone();

        tokio::spawn(async move {
            reader_loop(reader, sender_rx, bg_session_id, cost, agent_name).await;
        });

        self.health_status = AgentHealth::Ready;
        Ok(InitializeResponse::new(ProtocolVersion::LATEST))
    }

    async fn new_session(
        &mut self,
        _cwd: PathBuf,
        _mcp_servers: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        let session_id = self.session_id.clone().ok_or_else(|| {
            anyhow::anyhow!("StreamJsonAdapter '{}': new_session before initialize", self.agent_name)
        })?;
        Ok(NewSessionResponse::new(session_id))
    }

    async fn prompt(
        &mut self,
        request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        let prompt_text: String = request
            .prompt
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Text(tc) = block {
                    Some(tc.text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        tracing::debug!(
            agent = %self.agent_name,
            prompt_len = prompt_text.len(),
            "StreamJsonAdapter: sending prompt"
        );

        // Create per-turn channel.
        let (notif_tx, notif_rx) = mpsc::channel::<SessionNotification>(64);

        // Send sender to background reader.
        let sender_tx = self.sender_tx.as_ref().ok_or_else(|| {
            anyhow::anyhow!("StreamJsonAdapter '{}': not initialized", self.agent_name)
        })?;
        sender_tx.send(notif_tx).await.map_err(|_| {
            anyhow::anyhow!("StreamJsonAdapter '{}': background reader died", self.agent_name)
        })?;

        // Write user message to stdin.
        let stdin = self.stdin.as_mut().ok_or_else(|| {
            anyhow::anyhow!("StreamJsonAdapter '{}': stdin not available", self.agent_name)
        })?;
        let user_msg = UserMessage { msg_type: "user", content: &prompt_text };
        let json = serde_json::to_string(&user_msg)?;
        stdin.write_all(json.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;

        // Return receiver as Stream.
        let stream = unfold(notif_rx, |mut rx| async move {
            rx.recv().await.map(|notif| (notif, rx))
        });
        Ok(Box::pin(stream))
    }

    async fn cancel(&mut self, session_id: &str) -> anyhow::Result<()> {
        if let Some(ref mut child) = self.child {
            tracing::debug!(agent = %self.agent_name, session = %session_id, "StreamJsonAdapter: killing");
            child.kill().await.map_err(|e| {
                anyhow::anyhow!("StreamJsonAdapter '{}': kill failed: {e}", self.agent_name)
            })?;
        }
        self.child = None;
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        tracing::debug!(agent = %self.agent_name, "StreamJsonAdapter: shutting down");
        self.sender_tx.take();
        self.stdin.take();

        if let Some(ref mut child) = self.child {
            match tokio::time::timeout(std::time::Duration::from_secs(3), child.wait()).await {
                Ok(Ok(status)) => {
                    tracing::debug!(agent = %self.agent_name, status = %status, "exited cleanly");
                }
                Ok(Err(e)) => {
                    tracing::error!(agent = %self.agent_name, "wait error: {e}");
                }
                Err(_) => {
                    tracing::debug!(agent = %self.agent_name, "timeout, sending SIGKILL");
                    let _ = child.kill().await;
                }
            }
        }

        self.child.take();
        self.health_status = AgentHealth::Unknown;
        Ok(())
    }

    fn health(&self) -> AgentHealth {
        if let Some(ref child) = self.child {
            match child.id() {
                Some(_) => self.health_status.clone(),
                None => AgentHealth::Error("subprocess has exited".into()),
            }
        } else {
            self.health_status.clone()
        }
    }
}

// ─── Background reader ─────────────────────────────────────────────────

async fn reader_loop(
    stdout: BufReader<ChildStdout>,
    mut sender_rx: mpsc::Receiver<mpsc::Sender<SessionNotification>>,
    session_id: SessionId,
    cost: Arc<Mutex<f64>>,
    agent_name: String,
) {
    let mut lines = stdout.lines();
    let mut current_tx: Option<mpsc::Sender<SessionNotification>> = None;

    loop {
        if current_tx.is_none() {
            match sender_rx.recv().await {
                Some(tx) => current_tx = Some(tx),
                None => break,
            }
        }

        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break,
            Err(e) => {
                tracing::error!(agent = %agent_name, "reader_loop: read error: {e}");
                break;
            }
        };

        let event = match parse_event(&line) {
            Ok(e) => e,
            Err(_) => {
                tracing::debug!(agent = %agent_name, "reader_loop: unparseable, skipping");
                continue;
            }
        };

        if let ClaudeEvent::Result(ref r) = event {
            if let Some(c) = r.total_cost_usd {
                if let Ok(mut total) = cost.lock() {
                    *total += c;
                }
            }
            current_tx = None;
            continue;
        }

        if let Some(ref tx) = current_tx {
            for notif in map_to_notifications(&event, &session_id) {
                if tx.send(notif).await.is_err() {
                    current_tx = None;
                    break;
                }
            }
        }
    }
}

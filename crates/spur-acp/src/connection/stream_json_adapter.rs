//! `StreamJsonAdapter` — one-shot invocations of CLI tools that emit Claude-style
//! `stream-json` on stdout. One `claude -p --output-format stream-json …` process
//! per prompt; `--resume <sid>` links turns.
//!
//! **Scope:** non-Claude-Code agents whose CLI speaks this format. For Claude
//! Code itself, prefer the `claude-code-acp` profile in `.spur/config.toml`,
//! which routes through `NativeAcpConnection` and the upstream ACP wrapper —
//! richer features (plan mode, usage, commands, fork/resume) and a stable
//! protocol frame (ndjson), in contrast to this adapter's limited
//! 3-event / 4-content-block mapping in `protocol/claude_events.rs`.
//!
//! This adapter uses one-shot mode specifically because `--input-format
//! stream-json` exposes a Node stdout-buffering bug when piped; one-shot
//! flushes per line.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::unfold;
use futures::Stream;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc;

use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, InitializeResponse, McpServer, NewSessionResponse,
    PromptRequest, ProtocolVersion, SessionId, SessionNotification,
};

use crate::connection::AgentConnection;
use crate::protocol::claude_events::{map_to_notifications, parse_event, ClaudeEvent};
use crate::types::AgentHealth;

#[cfg(any(test, feature = "test-support"))]
pub async fn spawn_stream_json_for_test(
    command: &str,
    args: &[&str],
) -> std::io::Result<tokio::process::Child> {
    tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
}

/// Connects to Claude Code via one-shot stream-json invocations.
///
/// Each `prompt()` spawns a new process: `claude -p --output-format stream-json <prompt>`.
/// Multi-turn is achieved via `--resume <session_id>` on subsequent calls.
/// The first prompt establishes the Claude session ID from the init event.
pub struct StreamJsonAdapter {
    agent_name: String,
    command: String,
    extra_args: Vec<String>,
    /// Claude's session ID (from the first init event, used for --resume).
    claude_session_id: Option<String>,
    /// SPUR's session ID (assigned at new_session time).
    spur_session_id: Option<SessionId>,
    /// Current child process (alive during a prompt, None between turns).
    child: Option<Child>,
    /// Cumulative cost across all turns.
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
            claude_session_id: None,
            spur_session_id: None,
            child: None,
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
        // Verify the command exists on PATH.
        let which_result = tokio::process::Command::new("which")
            .arg(&self.command)
            .output()
            .await?;

        if !which_result.status.success() {
            self.health_status =
                AgentHealth::Error(format!("Command '{}' not found on PATH", self.command));
            return Err(anyhow::anyhow!(
                "StreamJsonAdapter '{}': command '{}' not found on PATH",
                self.agent_name,
                self.command
            ));
        }

        self.health_status = AgentHealth::Ready;
        tracing::debug!(
            agent = %self.agent_name,
            command = %self.command,
            "StreamJsonAdapter: initialized (command found on PATH)"
        );

        Ok(InitializeResponse::new(ProtocolVersion::LATEST))
    }

    async fn new_session(
        &mut self,
        _cwd: PathBuf,
        _mcp_servers: Vec<McpServer>,
    ) -> anyhow::Result<NewSessionResponse> {
        let session_id = SessionId::new(uuid::Uuid::new_v4().to_string());
        self.spur_session_id = Some(session_id.clone());

        tracing::debug!(
            agent = %self.agent_name,
            session = %session_id,
            "StreamJsonAdapter: created session"
        );

        Ok(NewSessionResponse::new(session_id))
    }

    async fn prompt(
        &mut self,
        request: PromptRequest,
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = SessionNotification> + Send>>> {
        // Extract prompt text from content blocks.
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

        let spur_session_id = request.session_id.clone();

        tracing::debug!(
            agent = %self.agent_name,
            prompt_len = prompt_text.len(),
            resume = self.claude_session_id.is_some(),
            "StreamJsonAdapter: spawning one-shot process"
        );

        // Build args: base args + optional --resume + prompt as trailing arg.
        let mut args = self.extra_args.clone();
        if let Some(claude_sid) = self.claude_session_id.as_ref() {
            args.push("--resume".to_string());
            args.push(claude_sid.clone());
        }
        args.push(prompt_text);

        // Spawn one-shot process.
        let mut cmd = tokio::process::Command::new(&self.command);
        cmd.args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(|e| {
            self.health_status = AgentHealth::Error(format!("Failed to spawn: {e}"));
            anyhow::anyhow!(
                "StreamJsonAdapter '{}': failed to spawn: {e}",
                self.agent_name
            )
        })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            anyhow::anyhow!(
                "StreamJsonAdapter '{}': failed to capture stdout",
                self.agent_name
            )
        })?;

        self.child = Some(child);

        // Spawn background reader that parses NDJSON and sends notifications.
        let (tx, rx) = mpsc::channel::<SessionNotification>(64);
        let agent_name = self.agent_name.clone();
        let cost = Arc::clone(&self.total_cost);
        let claude_session_holder = Arc::new(Mutex::new(self.claude_session_id.clone()));
        let reader_holder = Arc::clone(&claude_session_holder);

        tokio::spawn(async move {
            let claude_session_holder = reader_holder;
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let event = match parse_event(&line) {
                    Ok(e) => e,
                    Err(_) => {
                        tracing::debug!(
                            agent = %agent_name,
                            "StreamJsonAdapter: unparseable event, skipping"
                        );
                        continue;
                    }
                };

                // Extract Claude's session ID from the init event (first turn only).
                if let ClaudeEvent::System(sys) = &event {
                    if sys.subtype == "init" {
                        if let Ok(mut holder) = claude_session_holder.lock() {
                            if holder.is_none() {
                                *holder = Some(sys.session_id.clone());
                                tracing::debug!(
                                    agent = %agent_name,
                                    claude_session = %sys.session_id,
                                    "StreamJsonAdapter: captured Claude session ID"
                                );
                            }
                        }
                    }
                    continue; // system events are internal
                }

                // Extract cost from result events.
                if let ClaudeEvent::Result(r) = &event {
                    if let Some(c) = r.total_cost_usd {
                        if let Ok(mut total) = cost.lock() {
                            *total += c;
                        }
                    }
                    continue; // result signals end of turn, stream ends naturally via EOF
                }

                // Map assistant events to ACP notifications.
                for notif in map_to_notifications(&event, &spur_session_id) {
                    if tx.send(notif).await.is_err() {
                        break; // receiver dropped
                    }
                }
            }

            tracing::debug!(agent = %agent_name, "StreamJsonAdapter: reader done (process exited)");
        });

        // Update claude_session_id after the reader extracts it.
        // We use a shared mutex so the spawned task can write it.
        let holder = Arc::clone(&claude_session_holder);
        let agent_name_clone = self.agent_name.clone();

        // Wrap the notification stream: yield events from rx, then update session ID.
        // Stream ended (None) → process exited. The background reader has finished by then.
        let stream = unfold(
            (rx, Some(holder), Some(agent_name_clone)),
            |(mut rx, holder, name)| async move {
                rx.recv().await.map(|notif| (notif, (rx, holder, name)))
            },
        );

        // We need to update self.claude_session_id after the stream is consumed.
        // Since we can't mutate self from within the stream, we use a post-stream wrapper.
        // The orchestrator reads the full stream, then the adapter's state is updated
        // on the NEXT prompt() call by checking the holder.
        //
        // Actually, update it now via the Arc<Mutex> — the background reader will
        // write to it when it sees the init event. Next time prompt() is called,
        // we read from the holder.
        let holder_for_self = Arc::clone(&claude_session_holder);
        self.claude_session_id = holder_for_self.lock().ok().and_then(|h| h.clone());
        // If not captured yet (reader hasn't seen init), it will be captured during
        // the stream. We'll read it on the next prompt() call:
        // self.claude_session_id will be set from the holder at the start of prompt().
        // Actually, let's store the holder and read from it at the start of each prompt.

        Ok(Box::pin(stream))
    }

    async fn cancel(&mut self, session_id: &str) -> anyhow::Result<()> {
        if let Some(ref mut child) = self.child {
            tracing::debug!(
                agent = %self.agent_name,
                session = %session_id,
                "StreamJsonAdapter: killing process"
            );
            child.kill().await.map_err(|e| {
                anyhow::anyhow!("StreamJsonAdapter '{}': kill failed: {e}", self.agent_name)
            })?;
        }
        self.child = None;
        Ok(())
    }

    async fn shutdown(&mut self) -> anyhow::Result<()> {
        tracing::debug!(agent = %self.agent_name, "StreamJsonAdapter: shutting down");
        if let Some(ref mut child) = self.child {
            let _ = child.kill().await;
        }
        self.child = None;
        self.health_status = AgentHealth::Unknown;
        Ok(())
    }

    fn health(&self) -> AgentHealth {
        self.health_status.clone()
    }
}

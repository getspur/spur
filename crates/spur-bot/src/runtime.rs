use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::Context;

use crate::commands::{parse_chat_input, BotCommand, ParsedChatInput};
use crate::state::{
    BindingState, BotStateStore, PersistedBotState, PersistedThreadRecord, ThreadKey,
};
use crate::telegram::format::{split_for_final_answer, TELEGRAM_TEXT_MAX_UTF16_UNITS};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptButton {
    pub token: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeRender {
    ServiceMessage {
        text: String,
    },
    WorkingStatus {
        text: String,
    },
    StreamChunk {
        /// Stable per-turn key. Use `format!("stream-{session_id}-{turn_seq}")`.
        /// Different turns get different draft_ids so each turn animates as a
        /// new draft rather than overwriting the prior turn's final.
        draft_id: String,
        text: String,
    },
    FinalAnswer {
        text: String,
    },
    ReviewPrompt {
        text: String,
        buttons: Vec<PromptButton>,
    },
    PermissionPrompt {
        text: String,
        buttons: Vec<PromptButton>,
    },
    AnswerCallback {
        query_id: String,
        text: String,
    },
    FinalizePrompt {
        token: String,
        text: String,
    },
    CreateTopic {
        topic_name: String,
    },
}

struct PendingPrompt {
    thread_key: ThreadKey,
    /// The topic's live runtime session binding at prompt creation time. Using
    /// the live `SessionId` (rather than the persisted `acp_session_id`) makes
    /// callbacks stale whenever the topic's binding identity changes — whether
    /// by `/resume` rebinding to a different session, or by archive/detach that
    /// drops the live session while the record still retains `acp_session_id`
    /// for `/sessions` history.
    live_session: Option<spur_acp::SessionId>,
    kind: PromptKind,
}

enum PromptKind {
    Review {
        executor_id: String,
        attempt_n: u32,
        decision: spur_acp::ReviewDecision,
    },
    Permission {
        prompt_id: String,
        option_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum PromptGroup {
    Review { executor_id: String, attempt_n: u32 },
    Permission { prompt_id: String },
}

pub struct ThreadRecord {
    pub topic_name: String,
    pub archived: bool,
    pub binding: BindingState,
    pub acp_session_id: Option<String>,
    pub brain: Option<String>,
    pub live_session: Option<spur_acp::SessionId>,
    pub archived_previous: Vec<String>,
}

pub struct BotRuntime {
    state_store: BotStateStore,
    persisted: PersistedBotState,
    threads: HashMap<ThreadKey, ThreadRecord>,
    prompts: HashMap<String, PendingPrompt>,
    prompt_groups: HashMap<PromptGroup, Vec<String>>,
    permission_reply_txs:
        HashMap<String, tokio::sync::oneshot::Sender<spur_acp::types::PermissionResponse>>,
    /// Accumulates `AgentMessageChunk` text per session so that `TurnComplete`
    /// can emit a single `FinalAnswer`.
    output_buffers: HashMap<spur_acp::SessionId, String>,
    /// Increments at the start of each ACP turn (when AgentNotification arrives
    /// for a session whose output_buffer is currently empty), so each turn's
    /// streaming text uses a distinct draft_id.
    turn_seqs: HashMap<spur_acp::SessionId, u32>,
    /// Input queued while a persisted session is being restored, keyed by thread.
    pending_inputs: HashMap<ThreadKey, spur_core::InteractiveInput>,
    /// Maps live session to the thread it belongs to.
    session_threads: HashMap<spur_acp::SessionId, ThreadKey>,
    /// Threads waiting for a new session to become ready, in FIFO order.
    /// Multiple topics can start fresh sessions before any `AgentSessionReady`
    /// returns; preserving arrival order is the only way to bind each session
    /// to the right topic.
    pending_new_session_keys: VecDeque<ThreadKey>,
    /// Per-topic guard that mirrors `pending_new_session_keys`. A topic is
    /// in the guard exactly when it has one `NewSessionWithMessage` in
    /// flight and is still waiting to be bound. The guard blocks a second
    /// `NewSessionWithMessage` from the same topic, and — because
    /// `/resume`/archive paths evict from the guard — lets late fresh
    /// `AgentSessionReady` events skip stale FIFO entries without
    /// resurrecting rebound or archived topics.
    pending_new_session_guard: HashSet<ThreadKey>,
    /// Threads waiting for a resumed session to become ready, keyed by acp_session_id.
    pending_resume: HashMap<String, ThreadKey>,
    /// Executor ID -> session mapping so that reviews can be routed.
    executor_sessions: HashMap<String, spur_acp::SessionId>,
}

impl BotRuntime {
    pub fn new(state_store: BotStateStore) -> anyhow::Result<Self> {
        let persisted = state_store
            .load()
            .context("loading persisted bot state; refusing to start with empty state")?;
        let mut threads = HashMap::new();

        for (thread_id, record) in &persisted.threads {
            let key = ThreadKey {
                chat_id: persisted.operator_chat_id.unwrap_or(0),
                message_thread_id: Some(*thread_id),
            };
            let binding = match &record.binding_state {
                BindingState::Active {
                    acp_session_id,
                    brain,
                    ..
                } => BindingState::RestorePending {
                    acp_session_id: acp_session_id.clone(),
                    brain: brain.clone(),
                },
                other => other.clone(),
            };
            threads.insert(
                key,
                ThreadRecord {
                    topic_name: record.topic_name.clone(),
                    archived: record.archived,
                    binding,
                    acp_session_id: record.acp_session_id.clone(),
                    brain: record.brain.clone(),
                    live_session: None,
                    archived_previous: Vec::new(),
                },
            );
        }

        Ok(Self {
            state_store,
            persisted,
            threads,
            prompts: HashMap::new(),
            prompt_groups: HashMap::new(),
            permission_reply_txs: HashMap::new(),
            output_buffers: HashMap::new(),
            turn_seqs: HashMap::new(),
            pending_inputs: HashMap::new(),
            session_threads: HashMap::new(),
            pending_new_session_keys: VecDeque::new(),
            pending_new_session_guard: HashSet::new(),
            pending_resume: HashMap::new(),
            executor_sessions: HashMap::new(),
        })
    }

    pub fn state_store(&self) -> &BotStateStore {
        &self.state_store
    }

    pub fn bound_chat_id(&self) -> Option<i64> {
        self.persisted.operator_chat_id
    }

    /// Ensures an `Unbound` topic record exists for the given key and persists
    /// it to disk before returning. The transport calls this immediately after
    /// a successful `createForumTopic`; persistence here is what lets the topic
    /// survive a restart that occurs before the operator sends the first
    /// message.
    pub async fn ensure_topic_record(
        &mut self,
        chat_id: i64,
        message_thread_id: i32,
        topic_name: String,
    ) -> anyhow::Result<()> {
        let key = ThreadKey {
            chat_id,
            message_thread_id: Some(message_thread_id),
        };
        if self.threads.contains_key(&key) {
            return Ok(());
        }
        self.threads.insert(key.clone(), ThreadRecord {
            topic_name,
            archived: false,
            binding: BindingState::Unbound,
            acp_session_id: None,
            brain: None,
            live_session: None,
            archived_previous: Vec::new(),
        });
        if let Err(err) = self.state_store.save(&self.persistable_state()).await {
            self.threads.remove(&key);
            return Err(err);
        }
        Ok(())
    }

    /// Test helper: pre-seed a RestorePending binding.
    pub fn restore_topic_binding(
        &mut self,
        chat_id: i64,
        message_thread_id: i32,
        topic_name: String,
        acp_session_id: String,
        brain: String,
    ) {
        let key = ThreadKey {
            chat_id,
            message_thread_id: Some(message_thread_id),
        };
        self.threads.insert(
            key,
            ThreadRecord {
                topic_name,
                archived: false,
                binding: BindingState::RestorePending {
                    acp_session_id: acp_session_id.clone(),
                    brain: brain.clone(),
                },
                acp_session_id: Some(acp_session_id),
                brain: Some(brain),
                live_session: None,
                archived_previous: Vec::new(),
            },
        );
    }

    /// Test helper: activate a binding directly.
    pub fn activate_topic_binding(
        &mut self,
        chat_id: i64,
        message_thread_id: i32,
        topic_name: String,
        acp_session_id: String,
        brain: String,
    ) {
        let session = spur_acp::SessionId(format!("spur_{acp_session_id}"));
        let key = ThreadKey {
            chat_id,
            message_thread_id: Some(message_thread_id),
        };
        self.threads.insert(
            key.clone(),
            ThreadRecord {
                topic_name,
                archived: false,
                binding: BindingState::Active {
                    session: session.clone(),
                    acp_session_id: acp_session_id.clone(),
                    brain: brain.clone(),
                },
                acp_session_id: Some(acp_session_id),
                brain: Some(brain),
                live_session: Some(session.clone()),
                archived_previous: Vec::new(),
            },
        );
        self.session_threads.insert(session, key);
    }

    /// Test helper: pre-seed an archived/detached thread record (for `/sessions` coverage).
    pub fn seed_archived_topic_record(
        &mut self,
        chat_id: i64,
        message_thread_id: i32,
        topic_name: String,
        acp_session_id: String,
        brain: String,
    ) {
        let key = ThreadKey {
            chat_id,
            message_thread_id: Some(message_thread_id),
        };
        self.threads.insert(
            key,
            ThreadRecord {
                topic_name,
                archived: true,
                binding: BindingState::ArchivedDetached,
                acp_session_id: Some(acp_session_id),
                brain: Some(brain),
                live_session: None,
                archived_previous: Vec::new(),
            },
        );
    }

    /// Test helper: get a thread record by message_thread_id (uses the persisted chat_id).
    pub fn thread_record(&self, message_thread_id: i32) -> Option<&ThreadRecord> {
        let key = ThreadKey {
            chat_id: self.persisted.operator_chat_id.unwrap_or(0),
            message_thread_id: Some(message_thread_id),
        };
        self.threads.get(&key)
    }

    async fn ensure_known_topic(&mut self, key: &ThreadKey) -> anyhow::Result<()> {
        let Some(message_thread_id) = key.message_thread_id else {
            return Ok(());
        };
        if self.threads.contains_key(key) {
            return Ok(());
        }
        self.ensure_topic_record(
            key.chat_id,
            message_thread_id,
            format!("Topic {message_thread_id}"),
        )
        .await
    }

    pub async fn handle_chat_text(
        &mut self,
        handle: &spur_interactive::InteractiveFrontendHandle,
        chat_id: i64,
        message_thread_id: Option<i32>,
        text: &str,
    ) -> anyhow::Result<Vec<RuntimeRender>> {
        self.persisted.operator_chat_id = Some(chat_id);
        self.state_store.save(&self.persistable_state()).await?;

        let key = ThreadKey {
            chat_id,
            message_thread_id,
        };
        match parse_chat_input(text) {
            ParsedChatInput::PlainText(_body) if message_thread_id.is_none() => {
                Ok(vec![RuntimeRender::ServiceMessage {
                    text: "Use /new in the lobby to create a topic, or send a message inside an existing topic.".into(),
                }])
            }
            ParsedChatInput::PlainText(body) => {
                self.ensure_known_topic(&key).await?;
                let blocks = vec![spur_acp::ContentBlock::Text(spur_acp::TextContent::new(body))];
                let record = self.threads.get_mut(&key).ok_or_else(|| anyhow::anyhow!("unknown topic"))?;
                match &record.binding {
                    BindingState::Unbound => {
                        if self.pending_new_session_guard.contains(&key) {
                            // A fresh `NewSessionWithMessage` is already in
                            // flight for this topic. Emitting another would
                            // spawn a second brain session that nothing is
                            // waiting to bind, so queue the latest message
                            // for flush after the first ready binds.
                            self.pending_inputs.insert(
                                key.clone(),
                                spur_core::InteractiveInput::Message {
                                    blocks,
                                    interrupt: false,
                                },
                            );
                            return Ok(vec![RuntimeRender::WorkingStatus {
                                text: "Working…".into(),
                            }]);
                        }
                        handle
                            .send_command(spur_core::InteractiveInput::NewSessionWithMessage {
                                blocks,
                                interrupt: false,
                            })
                            .await?;
                        self.pending_new_session_keys.push_back(key.clone());
                        self.pending_new_session_guard.insert(key.clone());
                    }
                    BindingState::RestorePending { acp_session_id, .. } => {
                        if !self.pending_inputs.contains_key(&key) {
                            handle
                                .send_command(spur_core::InteractiveInput::ResumeSession {
                                    session_id: acp_session_id.clone(),
                                })
                                .await?;
                            self.pending_resume.insert(acp_session_id.clone(), key.clone());
                        }
                        self.pending_inputs.insert(
                            key.clone(),
                            spur_core::InteractiveInput::Message {
                                blocks,
                                interrupt: false,
                            },
                        );
                        return Ok(vec![RuntimeRender::WorkingStatus {
                            text: "Restoring session…".into(),
                        }]);
                    }
                    BindingState::Active { .. } => {
                        handle
                            .send_command(spur_core::InteractiveInput::Message {
                                blocks,
                                interrupt: false,
                            })
                            .await?;
                    }
                    BindingState::ArchivedDetached => {
                        return Ok(vec![RuntimeRender::ServiceMessage {
                            text: "This topic is archived. Use /resume <id> to rebind it.".into(),
                        }]);
                    }
                }
                Ok(vec![RuntimeRender::WorkingStatus {
                    text: "Working…".into(),
                }])
            }
            ParsedChatInput::Command(cmd) => {
                self.handle_command(handle, key, cmd).await
            }
        }
    }

    async fn handle_command(
        &mut self,
        handle: &spur_interactive::InteractiveFrontendHandle,
        key: ThreadKey,
        cmd: BotCommand,
    ) -> anyhow::Result<Vec<RuntimeRender>> {
        match cmd {
            BotCommand::New if key.message_thread_id.is_none() => {
                let topic_name = format!("Session {}", self.persisted.next_topic_seq);
                self.persisted.next_topic_seq += 1;
                self.state_store.save(&self.persistable_state()).await?;
                Ok(vec![RuntimeRender::CreateTopic { topic_name }])
            }
            BotCommand::New => Ok(vec![RuntimeRender::ServiceMessage {
                text: "Use /new in the lobby to create a topic.".into(),
            }]),
            BotCommand::Sessions => Ok(vec![RuntimeRender::ServiceMessage {
                text: self.render_registry_summary(),
            }]),
            BotCommand::Resume { session_id } => {
                if key.message_thread_id.is_none() {
                    return Ok(vec![RuntimeRender::ServiceMessage {
                        text: "Use /resume inside a topic.".into(),
                    }]);
                }
                self.ensure_known_topic(&key).await?;

                // Archive any OTHER topic that currently owns the requested
                // ACP session. This is the atomicity rule from the spec: no
                // ACP session may be live-bound to multiple topics at once.
                let conflicting_keys: Vec<ThreadKey> = self
                    .threads
                    .iter()
                    .filter(|(k, r)| {
                        *k != &key
                            && r.acp_session_id.as_deref() == Some(session_id.as_str())
                            && !matches!(r.binding, BindingState::ArchivedDetached)
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                for conflict in conflicting_keys {
                    if let Some(other) = self.threads.get_mut(&conflict) {
                        if let Some(old_session) = other.live_session.take() {
                            self.session_threads.remove(&old_session);
                        }
                        if let Some(old_acp) = other.acp_session_id.clone() {
                            other.archived_previous.push(old_acp);
                        }
                        other.binding = BindingState::ArchivedDetached;
                        other.archived = true;
                    }
                    self.pending_inputs.remove(&conflict);
                    self.evict_pending_new(&conflict);
                }

                let record = self
                    .threads
                    .get_mut(&key)
                    .expect("presence verified above");

                // Archive any current live binding for this topic.
                if let Some(old_session) = record.live_session.take() {
                    if let Some(old_acp) = record.acp_session_id.clone() {
                        record.archived_previous.push(old_acp);
                    }
                    self.session_threads.remove(&old_session);
                }

                let brain = record.brain.clone().unwrap_or_else(|| "kimi".into());
                record.binding = BindingState::RestorePending {
                    acp_session_id: session_id.clone(),
                    brain: brain.clone(),
                };
                record.acp_session_id = Some(session_id.clone());
                record.brain = Some(brain);
                self.pending_inputs.remove(&key);
                self.evict_pending_new(&key);
                self.supersede_pending_resume(&key);

                handle
                    .send_command(spur_core::InteractiveInput::ResumeSession {
                        session_id: session_id.clone(),
                    })
                    .await?;
                self.pending_resume.insert(session_id.clone(), key.clone());
                self.state_store.save(&self.persistable_state()).await?;

                Ok(vec![RuntimeRender::WorkingStatus {
                    text: format!("Resuming `{session_id}`…"),
                }])
            }
            BotCommand::Current => {
                let text = if let Some(record) = self.threads.get(&key) {
                    match &record.binding {
                        BindingState::Unbound => "No current session in this topic.".into(),
                        BindingState::RestorePending {
                            acp_session_id,
                            brain,
                        } => {
                            format!("Restore pending: `{acp_session_id}` via `{brain}`.")
                        }
                        BindingState::Active {
                            acp_session_id,
                            brain,
                            ..
                        } => {
                            format!("Current session: `{acp_session_id}` via `{brain}`.")
                        }
                        BindingState::ArchivedDetached => "This topic is archived.".into(),
                    }
                } else if key.message_thread_id.is_none() {
                    "Lobby — no session binding.".into()
                } else {
                    "Unknown topic.".into()
                };
                Ok(vec![RuntimeRender::ServiceMessage { text }])
            }
            BotCommand::Cancel => {
                if let Some(record) = self.threads.get(&key) {
                    if let BindingState::Active { session, .. } = &record.binding {
                        handle
                            .send_command(spur_core::InteractiveInput::CancelStream {
                                session: session.clone(),
                            })
                            .await?;
                        return Ok(vec![RuntimeRender::ServiceMessage {
                            text: "Cancel requested for the current turn.".into(),
                        }]);
                    }
                }
                Ok(vec![RuntimeRender::ServiceMessage {
                    text: "No in-flight turn is currently running.".into(),
                }])
            }
            BotCommand::Start | BotCommand::Help => Ok(vec![RuntimeRender::ServiceMessage {
                text: "Lobby commands: /new /sessions /help\nTopic commands: plain text /resume <id> /current /cancel".into(),
            }]),
        }
    }

    pub async fn handle_spur_event(
        &mut self,
        event: spur_acp::SpurEvent,
    ) -> anyhow::Result<(Option<ThreadKey>, Vec<RuntimeRender>)> {
        match event.body {
            spur_acp::SpurEventBody::AgentSessionReady {
                session,
                acp_session_id,
                brain,
                resumed,
                ..
            } => {
                let key = if resumed {
                    let Some(key) = self.resolve_resumed_ready(&acp_session_id) else {
                        return Ok((None, vec![]));
                    };
                    key
                } else {
                    // Pop FIFO until we find a topic still eligible for a
                    // fresh bind. Eviction on `/resume` or archive leaves
                    // stale FIFO entries whose guard membership has been
                    // cleared; skipping them here is what prevents an
                    // in-flight `NewSessionWithMessage` from resurrecting a
                    // rebound or archived topic.
                    let mut chosen = None;
                    while let Some(candidate) = self.pending_new_session_keys.pop_front() {
                        if self.pending_new_session_guard.remove(&candidate) {
                            chosen = Some(candidate);
                            break;
                        }
                    }
                    let Some(key) = chosen else {
                        tracing::warn!(
                            %acp_session_id,
                            "AgentSessionReady arrived with no eligible pending topic; dropping"
                        );
                        return Ok((None, vec![]));
                    };
                    key
                };

                self.bind_active_session(
                    &key,
                    session.clone(),
                    acp_session_id.clone(),
                    brain.clone(),
                );
                self.output_buffers.remove(&session);
                self.state_store.save(&self.persistable_state()).await?;

                Ok((
                    Some(key),
                    vec![RuntimeRender::ServiceMessage {
                        text: if resumed {
                            format!("Restored session `{acp_session_id}` via `{brain}`.")
                        } else {
                            format!("Started session `{acp_session_id}` via `{brain}`.")
                        },
                    }],
                ))
            }
            spur_acp::SpurEventBody::AgentNotification {
                session,
                notification,
            } => {
                let key = self.session_threads.get(&session).cloned();
                if let Some(text) = extract_agent_text(&notification) {
                    if !self.output_buffers.contains_key(&session) {
                        *self.turn_seqs.entry(session.clone()).or_insert(0) += 1;
                    }

                    let full_buffered_text = {
                        let buffer = self.output_buffers.entry(session.clone()).or_default();
                        buffer.push_str(&text);
                        buffer.clone()
                    };
                    let turn_seq = self.turn_seqs.get(&session).copied().unwrap_or(1);

                    return Ok((
                        key,
                        vec![RuntimeRender::StreamChunk {
                            draft_id: format!(
                                "stream-{session_id}-{turn_seq}",
                                session_id = session.0
                            ),
                            text: full_buffered_text,
                        }],
                    ));
                }
                Ok((key, vec![]))
            }
            spur_acp::SpurEventBody::TurnComplete { session } => {
                let key = self.session_threads.get(&session).cloned();
                let render = if let Some(text) = self
                    .output_buffers
                    .remove(&session)
                    .filter(|s| !s.trim().is_empty())
                {
                    split_for_final_answer(&text, TELEGRAM_TEXT_MAX_UTF16_UNITS)
                        .into_iter()
                        .map(|text| RuntimeRender::FinalAnswer { text })
                        .collect()
                } else {
                    vec![]
                };
                Ok((key, render))
            }
            spur_acp::SpurEventBody::BrainError { session, message } => {
                let key = self.session_threads.get(&session).cloned();
                self.output_buffers.remove(&session);
                Ok((
                    key,
                    vec![RuntimeRender::ServiceMessage {
                        text: format!("Error: {message}"),
                    }],
                ))
            }
            // `/sessions` is now served locally from the thread registry, so
            // inbound `SessionsListed` events are informational only.
            spur_acp::SpurEventBody::SessionsListed { .. } => Ok((None, vec![])),
            spur_acp::SpurEventBody::ExecutorSpawned { id, session_id, .. } => {
                self.executor_sessions.insert(id, session_id.clone());
                let key = self.session_threads.get(&session_id).cloned();
                Ok((key, vec![]))
            }
            spur_acp::SpurEventBody::ExecutorReviewRequested {
                id,
                attempt_n,
                payload,
                ..
            } => {
                let key = self
                    .executor_sessions
                    .get(&id)
                    .and_then(|session| self.session_threads.get(session).cloned())
                    .unwrap_or_else(|| {
                        ThreadKey::lobby(self.persisted.operator_chat_id.unwrap_or(0))
                    });
                let prompt_live_session =
                    self.threads.get(&key).and_then(|r| r.live_session.clone());

                let group = PromptGroup::Review {
                    executor_id: id.clone(),
                    attempt_n,
                };
                let mut buttons = Vec::new();
                let mut tokens = Vec::new();
                for (decision, label) in [
                    (spur_acp::ReviewDecision::Approve, "Approve"),
                    (
                        spur_acp::ReviewDecision::Reject {
                            reason: String::new(),
                        },
                        "Reject",
                    ),
                    (
                        spur_acp::ReviewDecision::Retry {
                            new_constraints: String::new(),
                        },
                        "Retry",
                    ),
                ] {
                    let token = uuid::Uuid::new_v4().simple().to_string();
                    self.prompts.insert(
                        token.clone(),
                        PendingPrompt {
                            thread_key: key.clone(),
                            live_session: prompt_live_session.clone(),
                            kind: PromptKind::Review {
                                executor_id: id.clone(),
                                attempt_n,
                                decision,
                            },
                        },
                    );
                    tokens.push(token.clone());
                    buttons.push(PromptButton {
                        token,
                        label: label.into(),
                    });
                }
                self.prompt_groups.insert(group, tokens);
                Ok((
                    Some(key),
                    vec![RuntimeRender::ReviewPrompt {
                        text: format!("Review required for `{id}`: {}", payload.summary),
                        buttons,
                    }],
                ))
            }
            _ => Ok((None, vec![])),
        }
    }

    /// Remove a topic from every pending-new bookkeeping structure. Called
    /// from any path that turns an `Unbound` topic into something else
    /// (`/resume` on the topic itself, or `/resume` on another topic that
    /// archives this one) so that a late fresh `AgentSessionReady` cannot
    /// resurrect the topic via its stale FIFO entry.
    fn evict_pending_new(&mut self, key: &ThreadKey) {
        self.pending_new_session_guard.remove(key);
        self.pending_new_session_keys.retain(|k| k != key);
    }

    fn supersede_pending_resume(&mut self, key: &ThreadKey) {
        self.pending_resume
            .retain(|_, pending_key| pending_key != key);
    }

    fn resolve_resumed_ready(&mut self, acp_session_id: &str) -> Option<ThreadKey> {
        let key = self.pending_resume.remove(acp_session_id)?;
        let still_expects = self.threads.get(&key).is_some_and(|record| {
            matches!(
                &record.binding,
                BindingState::RestorePending {
                    acp_session_id: expected,
                    ..
                } if expected == acp_session_id
            )
        });
        still_expects.then_some(key)
    }

    fn bind_active_session(
        &mut self,
        key: &ThreadKey,
        session: spur_acp::SessionId,
        acp_session_id: String,
        brain: String,
    ) {
        if let Some(existing_key) = self.session_threads.get(&session).cloned() {
            debug_assert_eq!(
                existing_key, *key,
                "session_threads collision: session already routed to another topic"
            );
        }

        if let Some(record) = self.threads.get_mut(key) {
            if let Some(old_session) = record.live_session.clone() {
                self.session_threads.remove(&old_session);
            }

            record.binding = BindingState::Active {
                session: session.clone(),
                acp_session_id: acp_session_id.clone(),
                brain: brain.clone(),
            };
            record.live_session = Some(session.clone());
            record.acp_session_id = Some(acp_session_id);
            record.brain = Some(brain);
        }

        self.session_threads.insert(session, key.clone());
    }

    /// Send any input that was queued while waiting for a persisted session to
    /// restore. Call this after `handle_spur_event` whenever the binding may
    /// have transitioned to `Active`.
    pub async fn flush_pending(
        &mut self,
        handle: &spur_interactive::InteractiveFrontendHandle,
        key: &ThreadKey,
    ) -> anyhow::Result<Vec<RuntimeRender>> {
        if let Some(record) = self.threads.get(key) {
            if matches!(record.binding, BindingState::Active { .. }) {
                if let Some(input) = self.pending_inputs.remove(key) {
                    handle.send_command(input).await?;
                }
            }
        }
        Ok(vec![])
    }

    pub fn handle_permission_request(
        &mut self,
        request: spur_acp::types::PermissionRequest,
    ) -> anyhow::Result<(ThreadKey, Vec<RuntimeRender>)> {
        let session_str = request.args.session_id.0.to_string();
        let session_id = spur_acp::SessionId(session_str);
        let key = self
            .session_threads
            .get(&session_id)
            .cloned()
            .unwrap_or_else(|| ThreadKey::lobby(self.persisted.operator_chat_id.unwrap_or(0)));
        let prompt_live_session = self.threads.get(&key).and_then(|r| r.live_session.clone());

        let prompt_id = uuid::Uuid::new_v4().simple().to_string();
        self.permission_reply_txs
            .insert(prompt_id.clone(), request.reply_tx);
        let mut buttons = Vec::new();
        let mut tokens = Vec::new();
        for opt in &request.args.options {
            let token = uuid::Uuid::new_v4().simple().to_string();
            self.prompts.insert(
                token.clone(),
                PendingPrompt {
                    thread_key: key.clone(),
                    live_session: prompt_live_session.clone(),
                    kind: PromptKind::Permission {
                        prompt_id: prompt_id.clone(),
                        option_id: opt.option_id.to_string(),
                    },
                },
            );
            tokens.push(token.clone());
            buttons.push(PromptButton {
                token,
                label: opt.name.to_string(),
            });
        }
        self.prompt_groups.insert(
            PromptGroup::Permission {
                prompt_id: prompt_id.clone(),
            },
            tokens,
        );
        Ok((
            key,
            vec![RuntimeRender::PermissionPrompt {
                text: format!(
                    "Permission required for `{}`",
                    request.args.tool_call.tool_call_id
                ),
                buttons,
            }],
        ))
    }

    pub async fn handle_callback(
        &mut self,
        handle: &spur_interactive::InteractiveFrontendHandle,
        thread_key: &ThreadKey,
        query_id: &str,
        token: &str,
    ) -> anyhow::Result<Vec<RuntimeRender>> {
        let Some(prompt) = self.prompts.remove(token) else {
            return Ok(vec![RuntimeRender::AnswerCallback {
                query_id: query_id.into(),
                text: "This action expired after restart.".into(),
            }]);
        };

        if &prompt.thread_key != thread_key {
            return Ok(vec![RuntimeRender::AnswerCallback {
                query_id: query_id.into(),
                text: "This action expired after restart.".into(),
            }]);
        }

        // Even when the `ThreadKey` still matches, the prompt is stale if the
        // topic's live runtime binding has changed — either because the topic
        // was rebound by `/resume` (live_session swapped), or because it was
        // archived/detached (live_session cleared, even if `acp_session_id` is
        // preserved for `/sessions` history). A missing record (e.g. the
        // implicit lobby) is treated as neutral so lobby-fallback prompts still
        // work — their captured `live_session` is `None` to match.
        let (is_archived, current_live) = match self.threads.get(thread_key) {
            Some(r) => (
                matches!(r.binding, BindingState::ArchivedDetached),
                r.live_session.clone(),
            ),
            None => (false, None),
        };
        if is_archived || prompt.live_session != current_live {
            // Also clear prompt-group siblings so that Approve/Reject/Retry
            // triplet doesn't leave orphan tokens behind.
            let group = match &prompt.kind {
                PromptKind::Review {
                    executor_id,
                    attempt_n,
                    ..
                } => PromptGroup::Review {
                    executor_id: executor_id.clone(),
                    attempt_n: *attempt_n,
                },
                PromptKind::Permission { prompt_id, .. } => PromptGroup::Permission {
                    prompt_id: prompt_id.clone(),
                },
            };
            if let Some(siblings) = self.prompt_groups.remove(&group) {
                for sibling in siblings {
                    if sibling != token {
                        self.prompts.remove(&sibling);
                    }
                }
            }
            return Ok(vec![RuntimeRender::AnswerCallback {
                query_id: query_id.into(),
                text: "This action expired after restart.".into(),
            }]);
        }

        let group = match &prompt.kind {
            PromptKind::Review {
                executor_id,
                attempt_n,
                ..
            } => PromptGroup::Review {
                executor_id: executor_id.clone(),
                attempt_n: *attempt_n,
            },
            PromptKind::Permission { prompt_id, .. } => PromptGroup::Permission {
                prompt_id: prompt_id.clone(),
            },
        };
        if let Some(siblings) = self.prompt_groups.remove(&group) {
            for sibling in siblings {
                if sibling != token {
                    self.prompts.remove(&sibling);
                }
            }
        }

        match prompt.kind {
            PromptKind::Review {
                executor_id,
                attempt_n,
                decision,
            } => {
                handle
                    .send_review(spur_interactive::ReviewSubmission {
                        executor_id: executor_id.clone(),
                        attempt_n,
                        decision,
                    })
                    .await?;
                Ok(vec![
                    RuntimeRender::AnswerCallback {
                        query_id: query_id.into(),
                        text: "Review decision received.".into(),
                    },
                    RuntimeRender::FinalizePrompt {
                        token: token.into(),
                        text: format!("Review resolved for `{executor_id}` attempt {attempt_n}."),
                    },
                ])
            }
            PromptKind::Permission {
                prompt_id,
                option_id,
            } => {
                let Some(reply_tx) = self.permission_reply_txs.remove(&prompt_id) else {
                    return Ok(vec![RuntimeRender::AnswerCallback {
                        query_id: query_id.into(),
                        text: "This action expired after restart.".into(),
                    }]);
                };
                let _ = reply_tx.send(spur_acp::types::PermissionResponse { option_id });
                Ok(vec![
                    RuntimeRender::AnswerCallback {
                        query_id: query_id.into(),
                        text: "Permission decision sent.".into(),
                    },
                    RuntimeRender::FinalizePrompt {
                        token: token.into(),
                        text: "Permission request resolved.".into(),
                    },
                ])
            }
        }
    }

    fn render_registry_summary(&self) -> String {
        let mut entries: Vec<(i32, &ThreadRecord)> = self
            .threads
            .iter()
            .filter_map(|(k, r)| k.message_thread_id.map(|tid| (tid, r)))
            .collect();
        if entries.is_empty() {
            return "No topics yet. Use /new in the lobby to create one.".into();
        }
        entries.sort_by_key(|(tid, _)| *tid);
        let mut out = String::from("Topics:\n");
        for (_, record) in entries {
            let state = match &record.binding {
                BindingState::Unbound => "unbound",
                BindingState::RestorePending { .. } => "restore-pending",
                BindingState::Active { .. } => "active",
                BindingState::ArchivedDetached => "archived",
            };
            out.push_str(&format!("• {} — {}", record.topic_name, state));
            if let Some(acp) = &record.acp_session_id {
                out.push_str(&format!(" — `{acp}`"));
            }
            if record.archived && !matches!(record.binding, BindingState::ArchivedDetached) {
                out.push_str(" (archived)");
            }
            out.push('\n');
        }
        out.trim_end().to_string()
    }

    fn persistable_state(&self) -> PersistedBotState {
        let mut persisted = self.persisted.clone();
        for (key, record) in &self.threads {
            if let Some(thread_id) = key.message_thread_id {
                let mut binding_state = record.binding.clone();
                if let BindingState::Active {
                    acp_session_id,
                    brain,
                    ..
                } = &binding_state
                {
                    binding_state = BindingState::RestorePending {
                        acp_session_id: acp_session_id.clone(),
                        brain: brain.clone(),
                    };
                }
                persisted.threads.insert(
                    thread_id,
                    PersistedThreadRecord {
                        topic_name: record.topic_name.clone(),
                        archived: record.archived,
                        acp_session_id: record.acp_session_id.clone(),
                        brain: record.brain.clone(),
                        binding_state,
                    },
                );
            }
        }
        persisted
    }
}

fn extract_agent_text(
    notification: &agent_client_protocol::schema::SessionNotification,
) -> Option<String> {
    match &notification.update {
        spur_acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            spur_acp::ContentBlock::Text(tc) => Some(tc.text.clone()),
            _ => None,
        },
        _ => None,
    }
}

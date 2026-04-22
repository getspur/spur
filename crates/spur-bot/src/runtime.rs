use std::collections::HashMap;

use crate::commands::{parse_chat_input, BotCommand, ParsedChatInput};
use crate::state::{BindingState, BotStateStore, PersistedBotState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptButton {
    pub token: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeRender {
    ServiceMessage { text: String },
    WorkingStatus { text: String },
    FinalAnswer { text: String },
    ReviewPrompt { text: String, buttons: Vec<PromptButton> },
    PermissionPrompt { text: String, buttons: Vec<PromptButton> },
    AnswerCallback { query_id: String, text: String },
    FinalizePrompt { token: String, text: String },
}

enum PendingPrompt {
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
    Review {
        executor_id: String,
        attempt_n: u32,
    },
    Permission {
        prompt_id: String,
    },
}

pub struct BotRuntime {
    state_store: BotStateStore,
    binding: BindingState,
    persisted: PersistedBotState,
    prompts: HashMap<String, PendingPrompt>,
    prompt_groups: HashMap<PromptGroup, Vec<String>>,
    permission_reply_txs:
        HashMap<String, tokio::sync::oneshot::Sender<spur_acp::types::PermissionResponse>>,
}

impl BotRuntime {
    pub fn new(state_store: BotStateStore) -> Self {
        let persisted = state_store.load().unwrap_or_default();
        let binding = match (
            persisted.current_acp_session_id.clone(),
            persisted.current_brain.clone(),
        ) {
            (Some(acp_session_id), Some(brain)) => BindingState::RestorePending {
                acp_session_id,
                brain,
            },
            _ => BindingState::NoSession,
        };
        Self {
            state_store,
            binding,
            persisted,
            prompts: HashMap::new(),
            prompt_groups: HashMap::new(),
            permission_reply_txs: HashMap::new(),
        }
    }

    pub fn state_store(&self) -> &BotStateStore {
        &self.state_store
    }

    pub fn bound_chat_id(&self) -> Option<i64> {
        self.persisted.operator_chat_id
    }

    pub async fn handle_chat_text(
        &mut self,
        handle: &spur_interactive::InteractiveFrontendHandle,
        chat_id: i64,
        text: &str,
    ) -> anyhow::Result<Vec<RuntimeRender>> {
        self.persisted.operator_chat_id = Some(chat_id);
        self.state_store.save(&self.persisted)?;

        match parse_chat_input(text) {
            ParsedChatInput::PlainText(body) => {
                let blocks = vec![spur_acp::ContentBlock::Text(spur_acp::TextContent::new(body))];
                match &self.binding {
                    BindingState::NoSession => {
                        handle
                            .send_command(spur_core::InteractiveInput::NewSessionWithMessage {
                                blocks,
                                interrupt: false,
                            })
                            .await?;
                    }
                    BindingState::RestorePending { .. } | BindingState::Active { .. } => {
                        handle
                            .send_command(spur_core::InteractiveInput::Message {
                                blocks,
                                interrupt: false,
                            })
                            .await?;
                    }
                }
                Ok(vec![RuntimeRender::WorkingStatus {
                    text: "Working…".into(),
                }])
            }
            ParsedChatInput::Command(cmd) => self.handle_command(handle, cmd).await,
        }
    }

    async fn handle_command(
        &mut self,
        handle: &spur_interactive::InteractiveFrontendHandle,
        cmd: BotCommand,
    ) -> anyhow::Result<Vec<RuntimeRender>> {
        match cmd {
            BotCommand::New => {
                self.binding = BindingState::NoSession;
                self.persisted.current_acp_session_id = None;
                self.persisted.current_brain = None;
                self.state_store.save(&self.persisted)?;
                Ok(vec![RuntimeRender::ServiceMessage {
                    text: "Current session cleared. The next plain message starts a new session.".into(),
                }])
            }
            BotCommand::Sessions => {
                handle
                    .send_command(spur_core::InteractiveInput::ListSessions)
                    .await?;
                Ok(vec![RuntimeRender::WorkingStatus {
                    text: "Listing resumable sessions…".into(),
                }])
            }
            BotCommand::Resume { session_id } => {
                handle
                    .send_command(spur_core::InteractiveInput::ResumeSession {
                        session_id: session_id.clone(),
                    })
                    .await?;
                Ok(vec![RuntimeRender::WorkingStatus {
                    text: format!("Resuming `{session_id}`…"),
                }])
            }
            BotCommand::Current => Ok(vec![RuntimeRender::ServiceMessage {
                text: match &self.binding {
                    BindingState::NoSession => "No current session.".into(),
                    BindingState::RestorePending { acp_session_id, brain } => {
                        format!("Restore pending: `{acp_session_id}` via `{brain}`.")
                    }
                    BindingState::Active {
                        acp_session_id,
                        brain,
                        ..
                    } => format!("Current session: `{acp_session_id}` via `{brain}`."),
                },
            }]),
            BotCommand::Cancel => {
                if let BindingState::Active { session, .. } = &self.binding {
                    handle
                        .send_command(spur_core::InteractiveInput::CancelStream {
                            session: session.clone(),
                        })
                        .await?;
                    Ok(vec![RuntimeRender::ServiceMessage {
                        text: "Cancel requested for the current turn.".into(),
                    }])
                } else {
                    Ok(vec![RuntimeRender::ServiceMessage {
                        text: "No in-flight turn is currently running.".into(),
                    }])
                }
            }
            BotCommand::Start | BotCommand::Help => Ok(vec![RuntimeRender::ServiceMessage {
                text: "Send plain text to talk to SPUR. Commands: /new /sessions /resume <id> /current /cancel".into(),
            }]),
        }
    }

    pub fn handle_spur_event(&mut self, event: spur_acp::SpurEvent) -> anyhow::Result<Vec<RuntimeRender>> {
        match event.body {
            spur_acp::SpurEventBody::AgentSessionReady {
                session,
                acp_session_id,
                brain,
                resumed,
                ..
            } => {
                self.binding = BindingState::Active {
                    session,
                    acp_session_id: acp_session_id.clone(),
                    brain: brain.clone(),
                };
                self.persisted.current_acp_session_id = Some(acp_session_id.clone());
                self.persisted.current_brain = Some(brain.clone());
                self.state_store.save(&self.persisted)?;
                Ok(vec![RuntimeRender::ServiceMessage {
                    text: if resumed {
                        format!("Restored session `{acp_session_id}` via `{brain}`.")
                    } else {
                        format!("Started session `{acp_session_id}` via `{brain}`.")
                    },
                }])
            }
            spur_acp::SpurEventBody::SessionsListed { sessions, .. } => Ok(vec![RuntimeRender::ServiceMessage {
                text: sessions
                    .iter()
                    .take(5)
                    .map(|s| s.session_id.0.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            }]),
            spur_acp::SpurEventBody::ExecutorReviewRequested {
                id,
                attempt_n,
                payload,
                ..
            } => {
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
                        PendingPrompt::Review {
                            executor_id: id.clone(),
                            attempt_n,
                            decision,
                        },
                    );
                    tokens.push(token.clone());
                    buttons.push(PromptButton {
                        token,
                        label: label.into(),
                    });
                }
                self.prompt_groups.insert(group, tokens);
                Ok(vec![RuntimeRender::ReviewPrompt {
                    text: format!("Review required for `{id}`: {}", payload.summary),
                    buttons,
                }])
            }
            _ => Ok(vec![]),
        }
    }

    pub fn handle_permission_request(
        &mut self,
        request: spur_acp::types::PermissionRequest,
    ) -> anyhow::Result<Vec<RuntimeRender>> {
        let prompt_id = uuid::Uuid::new_v4().simple().to_string();
        self.permission_reply_txs
            .insert(prompt_id.clone(), request.reply_tx);
        let mut buttons = Vec::new();
        let mut tokens = Vec::new();
        for opt in &request.args.options {
            let token = uuid::Uuid::new_v4().simple().to_string();
            self.prompts.insert(
                token.clone(),
                PendingPrompt::Permission {
                    prompt_id: prompt_id.clone(),
                    option_id: opt.option_id.to_string(),
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
        Ok(vec![RuntimeRender::PermissionPrompt {
            text: format!("Permission required for `{}`", request.args.tool_call.tool_call_id),
            buttons,
        }])
    }

    pub async fn handle_callback(
        &mut self,
        handle: &spur_interactive::InteractiveFrontendHandle,
        query_id: &str,
        token: &str,
    ) -> anyhow::Result<Vec<RuntimeRender>> {
        let Some(prompt) = self.prompts.remove(token) else {
            return Ok(vec![RuntimeRender::AnswerCallback {
                query_id: query_id.into(),
                text: "This action expired after restart.".into(),
            }]);
        };

        let group = match &prompt {
            PendingPrompt::Review {
                executor_id,
                attempt_n,
                ..
            } => PromptGroup::Review {
                executor_id: executor_id.clone(),
                attempt_n: *attempt_n,
            },
            PendingPrompt::Permission { prompt_id, .. } => PromptGroup::Permission {
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

        match prompt {
            PendingPrompt::Review {
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
            PendingPrompt::Permission {
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
}

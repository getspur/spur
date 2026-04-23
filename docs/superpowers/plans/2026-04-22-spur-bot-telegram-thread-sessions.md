# SPUR Bot Telegram Thread Sessions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend `spur-bot::telegram` from a single sticky DM session into a thread-native Telegram private-topics frontend where the lobby is control-only and each topic owns at most one live SPUR session binding.

**Architecture:** Keep the existing `spur-interactive` host and `frankenstein` transport boundary, but make the bot runtime and persistence explicitly thread-aware. The implementation should introduce a per-thread registry, normalize Telegram General-topic behavior (`message_thread_id = 1`), add startup topic-capability checks, and route all chat output, prompts, and callbacks by `ThreadKey` instead of one global current session.

**Tech Stack:** Rust 2021, `tokio`, `frankenstein` 0.49 (`client-reqwest`), `spur-core`, `spur-acp`, `serde`, `serde_json`, `cargo test`, `cargo clippy`

---

## File Map

### Existing files to modify

| File | Responsibility |
|---|---|
| `crates/spur-bot/src/state.rs` | Replace flat persisted state with thread registry + migration from legacy single-binding format |
| `crates/spur-bot/src/runtime.rs` | Replace single binding with `ThreadKey`/`ThreadRecord` model, lobby routing, topic-local prompts, rebinding, lazy per-thread restore |
| `crates/spur-bot/src/telegram/router.rs` | Propagate normalized `message_thread_id` for messages and callbacks |
| `crates/spur-bot/src/telegram/client.rs` | Add `get_me`, `create_forum_topic`, topic-aware send/edit/draft helpers, omit `message_thread_id` for lobby/General |
| `crates/spur-bot/src/telegram/render.rs` | Render to `(chat_id, message_thread_id)` targets instead of one chat-wide surface |
| `crates/spur-bot/src/telegram/sender.rs` | Carry optional `message_thread_id` in queued draft updates |
| `crates/spur-bot/src/telegram/mod.rs` | Startup capability gate, topic-aware intake loop, per-thread render dispatch, `/new` topic creation integration |
| `crates/spur-bot/src/commands.rs` | Keep command parser stable, but verify no global-session-only copy leaks into thread mode |
| `crates/spur-bot/tests/state_store.rs` | Add registry migration and archived-thread persistence tests |
| `crates/spur-bot/tests/runtime_flow.rs` | Replace single-session assumptions with thread-native runtime tests |
| `crates/spur-bot/tests/telegram_router.rs` | Add message-thread and callback-thread extraction coverage |
| `crates/spur-bot/tests/telegram_sender.rs` | Add topic-aware draft queue assertions |
| `crates/spur-bot/tests/telegram_poll_loop.rs` | Keep poll-loop assertions intact; no semantic change expected |
| `crates/spur-cli/tests/bot_cli.rs` | Keep CLI smoke green after transport signature changes |

### New test file

| File | Responsibility |
|---|---|
| `crates/spur-bot/tests/telegram_client_topics.rs` | Topic capability, General-topic omission, and `createForumTopic` request-shape tests |

## Implementation Notes

- Treat `ThreadKey { chat_id, message_thread_id: None }` as the lobby.
- Normalize inbound `message_thread_id = 1` to `None`.
- Omit outbound `message_thread_id` for lobby / General sends.
- Keep `AgentSessionReady` as the only commit point that turns a thread binding into `Active`.
- Store prompts by both token and `ThreadKey`.
- Do not eagerly resume all persisted bindings on startup.
- When a topic is rebound with `/resume <id>`, archive the previous live binding; do not discard it.
- Keep the current user/operator model unchanged: one `operator_user_id`, one private `chat_id`.

---

### Task 1: Add thread-aware state model and migration tests

**Files:**
- Modify: `crates/spur-bot/src/state.rs`
- Modify: `crates/spur-bot/tests/state_store.rs`

- [ ] **Step 1: Write the failing migration and registry tests**

Add to `crates/spur-bot/tests/state_store.rs`:

```rust
use spur_bot::state::{
    BindingState, BotStateStore, PersistedBotState, PersistedThreadRecord, ThreadKey,
};

#[test]
fn legacy_single_binding_loads_without_data_loss() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.json");
    std::fs::write(
        &path,
        serde_json::json!({
            "version": 1,
            "operator_chat_id": 42,
            "current_acp_session_id": "acp-1",
            "current_brain": "kimi"
        })
        .to_string(),
    )
    .unwrap();

    let store = BotStateStore::new(path);
    let state = store.load().unwrap();

    assert_eq!(state.operator_chat_id, Some(42));
    assert_eq!(state.next_topic_seq, 1);
    assert_eq!(state.threads.len(), 1);
    let only = state.threads.values().next().unwrap();
    assert!(only.archived);
    assert_eq!(only.acp_session_id.as_deref(), Some("acp-1"));
    assert_eq!(only.brain.as_deref(), Some("kimi"));
}

#[test]
fn registry_round_trips_archived_and_live_threads() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("state.json");
    let store = BotStateStore::new(path.clone());

    let mut state = PersistedBotState::default();
    state.operator_chat_id = Some(42);
    state.next_topic_seq = 3;
    state.threads.insert(
        11,
        PersistedThreadRecord {
            topic_name: "Session 1".into(),
            archived: false,
            acp_session_id: Some("acp-11".into()),
            brain: Some("kimi".into()),
            binding_state: BindingState::RestorePending {
                acp_session_id: "acp-11".into(),
                brain: "kimi".into(),
            },
        },
    );
    state.threads.insert(
        12,
        PersistedThreadRecord {
            topic_name: "Session 2".into(),
            archived: true,
            acp_session_id: Some("acp-12".into()),
            brain: Some("kimi".into()),
            binding_state: BindingState::ArchivedDetached,
        },
    );

    store.save(&state).unwrap();
    let loaded = store.load().unwrap();

    assert_eq!(loaded, state);
}
```

- [ ] **Step 2: Run the focused state-store tests and verify they fail**

Run:

```bash
cargo test -p spur-bot --test state_store -- --nocapture
```

Expected: FAIL because `PersistedThreadRecord`, `ThreadKey`, `next_topic_seq`, and `ArchivedDetached` do not exist yet.

- [ ] **Step 3: Replace the flat state model with a registry and migration loader**

Update `crates/spur-bot/src/state.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ThreadKey {
    pub chat_id: i64,
    pub message_thread_id: Option<i32>,
}

impl ThreadKey {
    pub fn lobby(chat_id: i64) -> Self {
        Self {
            chat_id,
            message_thread_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingState {
    Unbound,
    RestorePending { acp_session_id: String, brain: String },
    Active {
        #[serde(skip)]
        session: spur_acp::SessionId,
        acp_session_id: String,
        brain: String,
    },
    ArchivedDetached,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedThreadRecord {
    pub topic_name: String,
    pub archived: bool,
    pub acp_session_id: Option<String>,
    pub brain: Option<String>,
    pub binding_state: BindingState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedBotState {
    pub version: u32,
    pub operator_chat_id: Option<i64>,
    pub next_topic_seq: u32,
    pub threads: HashMap<i32, PersistedThreadRecord>,
}

impl Default for PersistedBotState {
    fn default() -> Self {
        Self {
            version: 2,
            operator_chat_id: None,
            next_topic_seq: 1,
            threads: HashMap::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct LegacyPersistedBotState {
    version: u32,
    operator_chat_id: Option<i64>,
    current_acp_session_id: Option<String>,
    current_brain: Option<String>,
}

pub struct BotStateStore {
    path: PathBuf,
}

impl BotStateStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> anyhow::Result<PersistedBotState> {
        if !self.path.exists() {
            return Ok(PersistedBotState::default());
        }

        let raw = std::fs::read_to_string(&self.path)?;
        if let Ok(state) = serde_json::from_str::<PersistedBotState>(&raw) {
            return Ok(state);
        }

        let legacy: LegacyPersistedBotState = serde_json::from_str(&raw)?;
        let mut migrated = PersistedBotState {
            operator_chat_id: legacy.operator_chat_id,
            ..PersistedBotState::default()
        };

        if let (Some(acp_session_id), Some(brain)) =
            (legacy.current_acp_session_id, legacy.current_brain)
        {
            migrated.threads.insert(
                -1,
                PersistedThreadRecord {
                    topic_name: "Legacy Session".into(),
                    archived: true,
                    acp_session_id: Some(acp_session_id.clone()),
                    brain: Some(brain.clone()),
                    binding_state: BindingState::ArchivedDetached,
                },
            );
        }

        Ok(migrated)
    }

    pub fn save(&self, state: &PersistedBotState) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, serde_json::to_vec_pretty(state)?)?;
        Ok(())
    }
}
```

- [ ] **Step 4: Run the state-store tests and verify they pass**

Run:

```bash
cargo test -p spur-bot --test state_store -- --nocapture
```

Expected: PASS with the new migration and round-trip coverage.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-bot/src/state.rs crates/spur-bot/tests/state_store.rs
git commit -m "feat(spur-bot): T2 add thread registry state model"
```

---

### Task 2: Make the Telegram router thread-aware and normalize General

**Files:**
- Modify: `crates/spur-bot/src/telegram/router.rs`
- Modify: `crates/spur-bot/tests/telegram_router.rs`

- [ ] **Step 1: Write the failing router tests for topic extraction**

Add to `crates/spur-bot/tests/telegram_router.rs`:

```rust
#[test]
fn private_topic_message_preserves_non_general_thread_id() {
    let update = test_update_with_message_thread(338086459, 9001, Some(77), "hello");
    let input = spur_bot::telegram::router::normalize_update(&update, 338086459).unwrap();

    assert!(matches!(
        input,
        spur_bot::telegram::router::TelegramInput::Text {
            chat_id: 9001,
            message_thread_id: Some(77),
            text,
            ..
        } if text == "hello"
    ));
}

#[test]
fn general_topic_normalizes_to_lobby() {
    let update = test_update_with_message_thread(338086459, 9001, Some(1), "hello");
    let input = spur_bot::telegram::router::normalize_update(&update, 338086459).unwrap();

    assert!(matches!(
        input,
        spur_bot::telegram::router::TelegramInput::Text {
            message_thread_id: None,
            ..
        }
    ));
}

#[test]
fn callback_uses_message_thread_id_from_callback_message() {
    let update = test_callback_update_with_thread(338086459, 9001, Some(88), "cb-1", "tok-1");
    let input = spur_bot::telegram::router::normalize_update(&update, 338086459).unwrap();

    assert!(matches!(
        input,
        spur_bot::telegram::router::TelegramInput::Callback {
            chat_id: 9001,
            message_thread_id: Some(88),
            query_id,
            token,
            ..
        } if query_id == "cb-1" && token == "tok-1"
    ));
}
```

- [ ] **Step 2: Run the router tests and verify they fail**

Run:

```bash
cargo test -p spur-bot --test telegram_router -- --nocapture
```

Expected: FAIL because `TelegramInput` does not yet carry `message_thread_id`.

- [ ] **Step 3: Add `message_thread_id` to normalized Telegram inputs**

Update `crates/spur-bot/src/telegram/router.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramInput {
    Text {
        user_id: i64,
        chat_id: i64,
        message_thread_id: Option<i32>,
        text: String,
    },
    Callback {
        user_id: i64,
        chat_id: i64,
        message_thread_id: Option<i32>,
        query_id: String,
        token: String,
    },
}

fn normalize_thread_id(thread_id: Option<i32>) -> Option<i32> {
    match thread_id {
        Some(1) | None => None,
        Some(other) => Some(other),
    }
}

pub fn normalize_update(
    update: &frankenstein::updates::Update,
    operator_user_id: i64,
) -> Option<TelegramInput> {
    match &update.content {
        frankenstein::updates::UpdateContent::Message(message)
            if message.chat.type_field == frankenstein::types::ChatType::Private =>
        {
            let user = message.from.as_ref()?;
            if user.id as i64 != operator_user_id {
                return None;
            }
            Some(TelegramInput::Text {
                user_id: user.id as i64,
                chat_id: message.chat.id,
                message_thread_id: normalize_thread_id(message.message_thread_id),
                text: message.text.clone()?,
            })
        }
        frankenstein::updates::UpdateContent::CallbackQuery(query) => {
            let user = &query.from;
            if user.id as i64 != operator_user_id {
                return None;
            }
            let (chat_id, message_thread_id) = match query.message.as_ref()? {
                frankenstein::types::MaybeInaccessibleMessage::Message(msg) => {
                    (msg.chat.id, normalize_thread_id(msg.message_thread_id))
                }
                frankenstein::types::MaybeInaccessibleMessage::InaccessibleMessage(msg) => {
                    (msg.chat.id, normalize_thread_id(msg.message_thread_id))
                }
            };
            Some(TelegramInput::Callback {
                user_id: user.id as i64,
                chat_id,
                message_thread_id,
                query_id: query.id.clone(),
                token: query.data.clone()?,
            })
        }
        _ => None,
    }
}
```

- [ ] **Step 4: Run the router tests and verify they pass**

Run:

```bash
cargo test -p spur-bot --test telegram_router -- --nocapture
```

Expected: PASS with thread extraction and General normalization.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-bot/src/telegram/router.rs crates/spur-bot/tests/telegram_router.rs
git commit -m "feat(spur-bot): T2 route telegram updates by thread"
```

---

### Task 3: Add topic-capable Telegram client APIs and topic-targeted renders

**Files:**
- Modify: `crates/spur-bot/src/telegram/client.rs`
- Modify: `crates/spur-bot/src/telegram/render.rs`
- Modify: `crates/spur-bot/src/telegram/sender.rs`
- Create: `crates/spur-bot/tests/telegram_client_topics.rs`
- Modify: `crates/spur-bot/tests/telegram_sender.rs`

- [ ] **Step 1: Write the failing client/render tests**

Create `crates/spur-bot/tests/telegram_client_topics.rs`:

```rust
#[test]
fn lobby_targets_omit_message_thread_id() {
    let params = spur_bot::telegram::client::build_send_text_params(42, None, "hello".into());
    let json = serde_json::to_value(params).unwrap();
    assert!(json.get("message_thread_id").is_none());
}

#[test]
fn topic_targets_include_message_thread_id() {
    let params = spur_bot::telegram::client::build_send_text_params(42, Some(77), "hello".into());
    let json = serde_json::to_value(params).unwrap();
    assert_eq!(json.get("message_thread_id").and_then(|v| v.as_i64()), Some(77));
}

#[test]
fn general_topic_id_is_omitted_outbound() {
    let params = spur_bot::telegram::client::build_send_text_params(42, Some(1), "hello".into());
    let json = serde_json::to_value(params).unwrap();
    assert!(json.get("message_thread_id").is_none());
}
```

Add to `crates/spur-bot/tests/telegram_sender.rs`:

```rust
#[tokio::test(start_paused = true)]
async fn sender_coalesces_by_chat_and_thread() {
    let (sender, mut rx) = spur_bot::telegram::sender::TelegramSender::for_test();

    sender.queue_draft(spur_bot::telegram::sender::DraftUpdate {
        chat_id: 42,
        message_thread_id: Some(7),
        draft_id: "draft-a".into(),
        text: "first".into(),
    }).await;
    sender.queue_draft(spur_bot::telegram::sender::DraftUpdate {
        chat_id: 42,
        message_thread_id: Some(8),
        draft_id: "draft-a".into(),
        text: "second".into(),
    }).await;

    tokio::time::advance(std::time::Duration::from_millis(500)).await;
    let first = rx.recv().await.unwrap();
    let second = rx.recv().await.unwrap();

    assert_ne!(first.message_thread_id, second.message_thread_id);
}
```

- [ ] **Step 2: Run the focused tests and verify they fail**

Run:

```bash
cargo test -p spur-bot --test telegram_client_topics -- --nocapture
cargo test -p spur-bot --test telegram_sender -- --nocapture
```

Expected: FAIL because no thread-aware client helpers or draft fields exist yet.

- [ ] **Step 3: Add topic-aware client helper builders and API calls**

Update `crates/spur-bot/src/telegram/client.rs`:

```rust
pub fn normalize_outbound_thread_id(message_thread_id: Option<i32>) -> Option<i32> {
    match message_thread_id {
        Some(1) | None => None,
        Some(other) => Some(other),
    }
}

pub fn build_send_text_params(
    chat_id: i64,
    message_thread_id: Option<i32>,
    text: String,
) -> frankenstein::methods::SendMessageParams {
    let mut builder = frankenstein::methods::SendMessageParams::builder()
        .chat_id(chat_id)
        .text(text);
    if let Some(thread_id) = normalize_outbound_thread_id(message_thread_id) {
        builder = builder.message_thread_id(thread_id);
    }
    builder.build()
}

impl TelegramClient {
    pub async fn get_me(&self) -> anyhow::Result<frankenstein::types::User> {
        Ok(self.inner.get_me().await?.result)
    }

    pub async fn create_forum_topic(
        &self,
        chat_id: i64,
        name: String,
    ) -> anyhow::Result<frankenstein::types::ForumTopic> {
        let params = frankenstein::methods::CreateForumTopicParams::builder()
            .chat_id(chat_id)
            .name(name)
            .build();
        Ok(self.inner.create_forum_topic(&params).await?.result)
    }

    pub async fn send_text(
        &self,
        chat_id: i64,
        message_thread_id: Option<i32>,
        text: String,
    ) -> anyhow::Result<()> {
        self.inner
            .send_message(&build_send_text_params(chat_id, message_thread_id, text))
            .await?;
        Ok(())
    }

    pub async fn send_buttons(
        &self,
        chat_id: i64,
        message_thread_id: Option<i32>,
        text: String,
        buttons: &[crate::runtime::PromptButton],
    ) -> anyhow::Result<()> {
        let row = buttons.iter().map(|button| {
            frankenstein::types::InlineKeyboardButton::builder()
                .text(button.label.clone())
                .callback_data(button.token.clone())
                .build()
        }).collect::<Vec<_>>();
        let markup = frankenstein::types::InlineKeyboardMarkup::builder()
            .inline_keyboard(vec![row])
            .build();

        let mut builder = frankenstein::methods::SendMessageParams::builder()
            .chat_id(chat_id)
            .text(text)
            .reply_markup(frankenstein::types::ReplyMarkup::InlineKeyboardMarkup(markup));
        if let Some(thread_id) = normalize_outbound_thread_id(message_thread_id) {
            builder = builder.message_thread_id(thread_id);
        }
        self.inner.send_message(&builder.build()).await?;
        Ok(())
    }
}
```

- [ ] **Step 4: Thread the optional `message_thread_id` through sender and render**

Update `crates/spur-bot/src/telegram/sender.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftUpdate {
    pub chat_id: i64,
    pub message_thread_id: Option<i32>,
    pub draft_id: String,
    pub text: String,
}
```

Update `crates/spur-bot/src/telegram/render.rs`:

```rust
pub async fn render_batch(
    client: &crate::telegram::client::TelegramClient,
    sender: &crate::telegram::sender::TelegramSender,
    chat_id: i64,
    message_thread_id: Option<i32>,
    renders: Vec<crate::runtime::RuntimeRender>,
) -> anyhow::Result<()> {
    for render in renders {
        match render {
            crate::runtime::RuntimeRender::ServiceMessage { text }
            | crate::runtime::RuntimeRender::FinalAnswer { text } => {
                client.send_text(chat_id, message_thread_id, text).await?;
            }
            crate::runtime::RuntimeRender::WorkingStatus { text } => {
                sender.queue_draft(crate::telegram::sender::DraftUpdate {
                    chat_id,
                    message_thread_id,
                    draft_id: format!("working-{chat_id}-{:?}", message_thread_id),
                    text,
                }).await;
            }
            crate::runtime::RuntimeRender::AnswerCallback { query_id, text } => {
                client.answer_callback(query_id, text).await?;
            }
            crate::runtime::RuntimeRender::ReviewPrompt { text, buttons }
            | crate::runtime::RuntimeRender::PermissionPrompt { text, buttons } => {
                client.send_buttons(chat_id, message_thread_id, text, &buttons).await?;
            }
            crate::runtime::RuntimeRender::FinalizePrompt { .. } => {}
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Run the topic-client and sender tests and verify they pass**

Run:

```bash
cargo test -p spur-bot --test telegram_client_topics -- --nocapture
cargo test -p spur-bot --test telegram_sender -- --nocapture
```

Expected: PASS with lobby omission and topic inclusion coverage.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-bot/src/telegram/client.rs crates/spur-bot/src/telegram/render.rs crates/spur-bot/src/telegram/sender.rs crates/spur-bot/tests/telegram_client_topics.rs crates/spur-bot/tests/telegram_sender.rs
git commit -m "feat(spur-bot): T2 add topic-aware telegram client paths"
```

---

### Task 4: Refactor the runtime into a thread-native session registry

**Files:**
- Modify: `crates/spur-bot/src/runtime.rs`
- Modify: `crates/spur-bot/tests/runtime_flow.rs`

- [ ] **Step 1: Write the failing runtime tests for lobby/topic behavior**

Add to `crates/spur-bot/tests/runtime_flow.rs`:

```rust
#[tokio::test]
async fn lobby_plain_text_is_rejected() {
    let (mut runtime, handle) = test_runtime();
    let renders = runtime.handle_chat_text(&handle, 42, None, "hello").await.unwrap();

    assert!(matches!(
        renders.as_slice(),
        [spur_bot::runtime::RuntimeRender::ServiceMessage { text }]
        if text.contains("/new")
    ));
}

#[tokio::test]
async fn unbound_topic_plain_text_starts_new_session() {
    let (mut runtime, handle) = test_runtime();
    runtime.ensure_topic_record(42, 77, "Session 1".into()).unwrap();

    let renders = runtime.handle_chat_text(&handle, 42, Some(77), "hello").await.unwrap();

    assert!(matches!(
        take_last_command(&handle).await,
        spur_core::InteractiveInput::NewSessionWithMessage { .. }
    ));
    assert!(matches!(
        renders.as_slice(),
        [spur_bot::runtime::RuntimeRender::WorkingStatus { .. }]
    ));
}

#[tokio::test]
async fn restore_pending_topic_queues_resume_then_flushes_message() {
    let (mut runtime, handle) = test_runtime();
    runtime.restore_topic_binding(42, 77, "Session 1".into(), "acp-77".into(), "kimi".into());

    runtime.handle_chat_text(&handle, 42, Some(77), "hello").await.unwrap();
    assert!(matches!(
        take_last_command(&handle).await,
        spur_core::InteractiveInput::ResumeSession { session_id } if session_id == "acp-77"
    ));

    let event = ready_event("session-1", "acp-77", "kimi");
    runtime.handle_spur_event(event).unwrap();
    let flushed = runtime.flush_pending(&handle).await.unwrap();

    assert!(!flushed.is_empty());
    assert!(matches!(
        take_last_command(&handle).await,
        spur_core::InteractiveInput::Message { .. }
    ));
}

#[tokio::test]
async fn topic_resume_archives_previous_binding() {
    let (mut runtime, handle) = test_runtime();
    runtime.activate_topic_binding(42, 77, "Session 1".into(), "acp-old".into(), "kimi".into());

    let renders = runtime
        .handle_chat_text(&handle, 42, Some(77), "/resume acp-new")
        .await
        .unwrap();

    assert!(matches!(
        take_last_command(&handle).await,
        spur_core::InteractiveInput::ResumeSession { session_id } if session_id == "acp-new"
    ));
    assert!(runtime.thread_record(77).unwrap().archived_previous.contains(&"acp-old".to_string()));
    assert!(matches!(renders.as_slice(), [spur_bot::runtime::RuntimeRender::WorkingStatus { .. }]));
}
```

- [ ] **Step 2: Run the runtime tests and verify they fail**

Run:

```bash
cargo test -p spur-bot --test runtime_flow -- --nocapture
```

Expected: FAIL because `handle_chat_text` has no thread parameter and the runtime only supports one global binding.

- [ ] **Step 3: Replace the single-session runtime model with per-thread records**

Refactor `crates/spur-bot/src/runtime.rs` so the public API becomes:

```rust
pub struct ThreadRecord {
    pub topic_name: String,
    pub archived: bool,
    pub binding: BindingState,
    pub acp_session_id: Option<String>,
    pub brain: Option<String>,
    pub live_session: Option<spur_acp::SessionId>,
}

pub struct BotRuntime {
    state_store: BotStateStore,
    persisted: PersistedBotState,
    threads: HashMap<crate::state::ThreadKey, ThreadRecord>,
    prompts: HashMap<String, PendingPrompt>,
    prompt_groups: HashMap<PromptGroup, Vec<String>>,
    permission_reply_txs: HashMap<String, tokio::sync::oneshot::Sender<spur_acp::types::PermissionResponse>>,
    output_buffers: HashMap<spur_acp::SessionId, String>,
    pending_inputs: HashMap<crate::state::ThreadKey, spur_core::InteractiveInput>,
    session_threads: HashMap<spur_acp::SessionId, crate::state::ThreadKey>,
}
```

Update `handle_chat_text` to:

```rust
pub async fn handle_chat_text(
    &mut self,
    handle: &spur_interactive::InteractiveFrontendHandle,
    chat_id: i64,
    message_thread_id: Option<i32>,
    text: &str,
) -> anyhow::Result<Vec<RuntimeRender>> {
    self.persisted.operator_chat_id = Some(chat_id);
    self.state_store.save(&self.persisted)?;

    let key = crate::state::ThreadKey { chat_id, message_thread_id };
    match parse_chat_input(text) {
        ParsedChatInput::PlainText(body) if message_thread_id.is_none() => {
            Ok(vec![RuntimeRender::ServiceMessage {
                text: "Use /new in the lobby to create a topic, or send a message inside an existing topic.".into(),
            }])
        }
        ParsedChatInput::PlainText(body) => {
            let blocks = vec![spur_acp::ContentBlock::Text(spur_acp::TextContent::new(body))];
            let record = self.threads.get_mut(&key).ok_or_else(|| anyhow::anyhow!("unknown topic"))?;
            match &record.binding {
                BindingState::Unbound => {
                    handle.send_command(spur_core::InteractiveInput::NewSessionWithMessage {
                        blocks,
                        interrupt: false,
                    }).await?;
                }
                BindingState::RestorePending { acp_session_id, .. } => {
                    if !self.pending_inputs.contains_key(&key) {
                        handle.send_command(spur_core::InteractiveInput::ResumeSession {
                            session_id: acp_session_id.clone(),
                        }).await?;
                    }
                    self.pending_inputs.insert(
                        key.clone(),
                        spur_core::InteractiveInput::Message { blocks, interrupt: false },
                    );
                    return Ok(vec![RuntimeRender::WorkingStatus {
                        text: "Restoring session…".into(),
                    }]);
                }
                BindingState::Active { .. } => {
                    handle.send_command(spur_core::InteractiveInput::Message {
                        blocks,
                        interrupt: false,
                    }).await?;
                }
                BindingState::ArchivedDetached => {
                    return Ok(vec![RuntimeRender::ServiceMessage {
                        text: "This topic is archived. Use /resume <id> to rebind it.".into(),
                    }]);
                }
            }
            Ok(vec![RuntimeRender::WorkingStatus { text: "Working…".into() }])
        }
        ParsedChatInput::Command(cmd) => self.handle_command(handle, key, cmd).await,
    }
}
```

Then update `handle_command`, `handle_spur_event`, `handle_permission_request`, `handle_callback`, and `flush_pending` so they all resolve the relevant `ThreadKey` and never rely on one global binding.

- [ ] **Step 4: Run the runtime tests and verify they pass**

Run:

```bash
cargo test -p spur-bot --test runtime_flow -- --nocapture
```

Expected: PASS with lobby rejection, topic start, lazy restore, and rebinding coverage.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-bot/src/runtime.rs crates/spur-bot/tests/runtime_flow.rs
git commit -m "feat(spur-bot): T2 make runtime thread-native"
```

---

### Task 5: Add startup topic capability checks and `/new` topic creation

**Files:**
- Modify: `crates/spur-bot/src/telegram/mod.rs`
- Modify: `crates/spur-cli/tests/bot_cli.rs`

- [ ] **Step 1: Write the failing transport tests for topic capability and `/new`**

Add to `crates/spur-cli/tests/bot_cli.rs`:

```rust
#[test]
fn bot_telegram_help_still_exposes_the_command() {
    let mut cmd = assert_cmd::Command::cargo_bin("spur").unwrap();
    cmd.args(["bot", "telegram", "--help"]);
    cmd.assert().success().stdout(predicates::str::contains("bot telegram"));
}
```

Add to `crates/spur-bot/tests/runtime_flow.rs`:

```rust
#[tokio::test]
async fn lobby_new_requires_topic_creation_before_chat() {
    let (mut runtime, handle) = test_runtime();
    let renders = runtime.handle_chat_text(&handle, 42, None, "/new").await.unwrap();

    assert!(matches!(
        renders.as_slice(),
        [spur_bot::runtime::RuntimeRender::CreateTopic { topic_name }]
        if topic_name == "Session 1"
    ));
}
```

- [ ] **Step 2: Run the focused tests and verify they fail**

Run:

```bash
cargo test -p spur-bot --test runtime_flow -- --nocapture
cargo test -p spur-cli --test bot_cli -- --nocapture
```

Expected: FAIL because no `CreateTopic` render intent or startup capability gate exists.

- [ ] **Step 3: Add a topic-creation render intent and startup capability gate**

Update `crates/spur-bot/src/runtime.rs` to add:

```rust
pub enum RuntimeRender {
    ServiceMessage { text: String },
    WorkingStatus { text: String },
    FinalAnswer { text: String },
    ReviewPrompt { text: String, buttons: Vec<PromptButton> },
    PermissionPrompt { text: String, buttons: Vec<PromptButton> },
    AnswerCallback { query_id: String, text: String },
    FinalizePrompt { token: String, text: String },
    CreateTopic { topic_name: String },
}
```

Implement lobby `/new` in `handle_command`:

```rust
BotCommand::New if key.message_thread_id.is_none() => {
    let topic_name = format!("Session {}", self.persisted.next_topic_seq);
    self.persisted.next_topic_seq += 1;
    self.state_store.save(&self.persisted)?;
    Ok(vec![RuntimeRender::CreateTopic { topic_name }])
}
BotCommand::New => Ok(vec![RuntimeRender::ServiceMessage {
    text: "Use /new in the lobby to create a topic.".into(),
}])
```

Update `crates/spur-bot/src/telegram/mod.rs` startup:

```rust
let me = client.get_me().await?;
if !me.has_topics_enabled.unwrap_or(false) {
    anyhow::bail!("telegram bot does not have topics enabled; enable private topics in BotFather before using thread sessions");
}
```

Handle `CreateTopic` inside the main loop by calling `client.create_forum_topic(...)`, inserting a new `Unbound` thread record, and rendering a starter message into that new thread.

- [ ] **Step 4: Run the runtime and bot CLI tests and verify they pass**

Run:

```bash
cargo test -p spur-bot --test runtime_flow -- --nocapture
cargo test -p spur-cli --test bot_cli -- --nocapture
```

Expected: PASS with `/new` behavior and CLI smoke intact.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-bot/src/runtime.rs crates/spur-bot/src/telegram/mod.rs crates/spur-cli/tests/bot_cli.rs
git commit -m "feat(spur-bot): T2 add telegram topic startup and creation"
```

---

### Task 6: Thread-target the transport loop and render dispatch

**Files:**
- Modify: `crates/spur-bot/src/telegram/mod.rs`
- Modify: `crates/spur-bot/tests/telegram_poll_loop.rs`

- [ ] **Step 1: Write the failing transport test for per-thread render dispatch**

Add to `crates/spur-bot/tests/telegram_poll_loop.rs`:

```rust
#[tokio::test]
async fn batch_forward_preserves_thread_identity() {
    let batch = vec![
        spur_bot::telegram::router::TelegramInput::Text {
            user_id: 338086459,
            chat_id: 42,
            message_thread_id: Some(77),
            text: "hello".into(),
        },
    ];

    assert_eq!(
        match &batch[0] {
            spur_bot::telegram::router::TelegramInput::Text { message_thread_id, .. } => *message_thread_id,
            _ => None,
        },
        Some(77)
    );
}
```

- [ ] **Step 2: Run the poll-loop tests and verify they fail or require updates**

Run:

```bash
cargo test -p spur-bot --test telegram_poll_loop -- --nocapture
```

Expected: FAIL or compile error until thread-bearing input batches flow through the transport.

- [ ] **Step 3: Carry thread identity through the Telegram main loop**

Update `crates/spur-bot/src/telegram/mod.rs`:

```rust
match input {
    router::TelegramInput::Text {
        chat_id,
        message_thread_id,
        text,
        ..
    } => {
        let renders = runtime
            .handle_chat_text(&handle, chat_id, message_thread_id, &text)
            .await?;
        let mut all_renders = renders;
        let pending = runtime.flush_pending(&handle).await?;
        all_renders.extend(pending);
        render::render_batch(&client, &sender, chat_id, message_thread_id, all_renders).await?;
    }
    router::TelegramInput::Callback {
        query_id,
        token,
        chat_id,
        message_thread_id,
        ..
    } => {
        let renders = runtime.handle_callback(&handle, &query_id, &token).await?;
        render::render_batch(&client, &sender, chat_id, message_thread_id, renders).await?;
    }
}
```

Also update event and permission paths so the runtime returns or resolves the correct `ThreadKey` for each render batch instead of always using `runtime.bound_chat_id()`.

- [ ] **Step 4: Run the Telegram-focused tests and verify they pass**

Run:

```bash
cargo test -p spur-bot --test telegram_poll_loop -- --nocapture
cargo test -p spur-bot --test telegram_router -- --nocapture
cargo test -p spur-bot --tests
```

Expected: PASS with thread-bearing updates and no regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-bot/src/telegram/mod.rs crates/spur-bot/tests/telegram_poll_loop.rs
git commit -m "feat(spur-bot): T2 dispatch telegram work by thread"
```

---

### Task 7: Run the full targeted acceptance set

**Files:**
- Modify: none

- [ ] **Step 1: Run the focused acceptance commands**

Run:

```bash
cargo test -p spur-bot --test state_store -- --nocapture
cargo test -p spur-bot --test runtime_flow -- --nocapture
cargo test -p spur-bot --test telegram_router -- --nocapture
cargo test -p spur-bot --test telegram_client_topics -- --nocapture
cargo test -p spur-bot --test telegram_sender -- --nocapture
cargo test -p spur-bot --test telegram_poll_loop -- --nocapture
cargo test -p spur-cli --test bot_cli -- --nocapture
cargo test -p spur-bot --tests
cargo build --workspace
cargo clippy -p spur-bot -p spur-cli -- -D warnings
```

Expected:

- all `spur-bot` tests PASS
- `bot_cli` PASS
- workspace build PASS
- clippy clean for `spur-bot` and `spur-cli`

- [ ] **Step 2: Record the known non-goal verification gap**

Do **not** use this as the acceptance gate:

```bash
cargo test --workspace
```

Reason: the repo still has the known unrelated baseline failures in
`crates/spur-cli/tests/auth_cli.rs`.

- [ ] **Step 3: Commit the final integration checkpoint**

```bash
git status
```

Expected: clean working tree after Task 6 commits and acceptance runs.

---

## Spec Coverage Self-Check

- Thread-native runtime keys: covered by Tasks 1, 2, 4, and 6.
- Lobby vs topic command split: covered by Task 4 and Task 5.
- `/new` topic creation with sequential names: covered by Task 5.
- Lazy per-thread restore: covered by Task 4.
- Archived/detached rebinding: covered by Tasks 1 and 4.
- `has_topics_enabled` startup gate: covered by Task 5.
- `message_thread_id = 1` normalization: covered by Tasks 2 and 3.
- Topic-targeted outbound send/draft helpers: covered by Task 3.
- Migration from flat persisted state: covered by Task 1.

No spec requirements are intentionally deferred inside this plan.

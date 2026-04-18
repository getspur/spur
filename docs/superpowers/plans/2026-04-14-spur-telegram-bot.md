# Spur Telegram Bot Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a `spur-telegram` crate that replaces the TUI with a Telegram bot frontend, supporting brain chat (private messages) and threaded executor work (forum topics), using the same channel protocol as `spur-tui`.

**Architecture:** The Telegram bot is a drop-in frontend replacement for `spur-tui`. It subscribes to `broadcast::Receiver<SpurEvent>` for events and sends `InteractiveInput` via `mpsc::Sender` to the orchestrator — the exact same channel contract the TUI uses. Brain chat happens in private Telegram messages; executor threads are auto-created as forum topics in a configured supergroup. Review and permission decisions use Telegram inline keyboards routed back through the orchestrator's `review_dispatcher_loop`.

**Tech Stack:** Rust, teloxide 0.13 (Telegram Bot API), tokio, spur-core, spur-acp, dashmap, serde

---

## File Structure

| File | Responsibility |
|---|---|
| `crates/spur-telegram/Cargo.toml` | Crate dependencies |
| `crates/spur-telegram/src/lib.rs` | Public API: re-exports, `run_telegram` entry point |
| `crates/spur-telegram/src/config.rs` | `TelegramConfig`: bot token, allowed user IDs, group chat ID |
| `crates/spur-telegram/src/bot.rs` | teloxide `Bot` + `Dispatcher` setup, command/message/callback routing |
| `crates/spur-telegram/src/state.rs` | `BotState`: shared state (DashMap for session/topic mappings, pending callbacks) |
| `crates/spur-telegram/src/handlers/mod.rs` | Handler module barrel |
| `crates/spur-telegram/src/handlers/commands.rs` | `/start`, `/brain`, `/brains`, `/sessions`, `/resume`, `/help` |
| `crates/spur-telegram/src/handlers/message.rs` | Text messages → `InteractiveInput::Message` |
| `crates/spur-telegram/src/handlers/callback.rs` | Inline keyboard callbacks → review/permission decisions |
| `crates/spur-telegram/src/renderer.rs` | `SpurEvent` → Telegram API calls (messages, keyboards, topic lifecycle) |
| `crates/spur-telegram/src/formatter.rs` | Markdown formatting, code block splitting, 4096-char pagination |
| `crates/spur-telegram/tests/formatter_test.rs` | Unit tests for message formatting and pagination |
| `crates/spur-telegram/tests/state_test.rs` | Unit tests for state management |
| `crates/spur-telegram/tests/config_test.rs` | Unit tests for config parsing |
| `crates/spur-cli/src/main.rs` | Modified: add `Telegram` subcommand alongside `Watch` |

---

### Task 1: Crate Skeleton and Config

**Files:**
- Create: `crates/spur-telegram/Cargo.toml`
- Create: `crates/spur-telegram/src/lib.rs`
- Create: `crates/spur-telegram/src/config.rs`
- Modify: `Cargo.toml` (workspace)
- Test: `crates/spur-telegram/tests/config_test.rs`

- [ ] **Step 1: Write the config test**

```rust
// crates/spur-telegram/tests/config_test.rs
use spur_telegram::config::TelegramConfig;

#[test]
fn parse_minimal_config() {
    let toml = r#"
        bot_token = "123:ABC"
        allowed_users = [111222333]
    "#;
    let cfg: TelegramConfig = toml::from_str(toml).unwrap();
    assert_eq!(cfg.bot_token, "123:ABC");
    assert_eq!(cfg.allowed_users, vec![111222333u64]);
    assert!(cfg.group_chat_id.is_none());
}

#[test]
fn parse_full_config() {
    let toml = r#"
        bot_token = "123:ABC"
        allowed_users = [111, 222]
        group_chat_id = -1001234567890
    "#;
    let cfg: TelegramConfig = toml::from_str(toml).unwrap();
    assert_eq!(cfg.group_chat_id, Some(-1001234567890i64));
    assert_eq!(cfg.allowed_users.len(), 2);
}

#[test]
fn reject_empty_token() {
    let toml = r#"
        bot_token = ""
        allowed_users = [111]
    "#;
    let cfg: TelegramConfig = toml::from_str(toml).unwrap();
    assert!(cfg.validate().is_err());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Volumes/Projects/spur && cargo test -p spur-telegram --test config_test 2>&1 | tail -5`
Expected: compilation error (crate doesn't exist yet)

- [ ] **Step 3: Create Cargo.toml and add to workspace**

```toml
# crates/spur-telegram/Cargo.toml
[package]
name = "spur-telegram"
description = "Telegram bot frontend for SPUR"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
spur-acp = { workspace = true }
spur-core = { workspace = true }
tokio = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
toml = { workspace = true }
teloxide = { version = "0.13", features = ["macros", "throttle"] }
tokio-util = { version = "0.7", features = ["rt"] }
dashmap = "6"

[dev-dependencies]
tempfile = "3"
```

Add `"crates/spur-telegram"` to the `members` list in the workspace `Cargo.toml`:

```toml
members = [
    "crates/spur-acp",
    "crates/spur-mcp",
    "crates/spur-core",
    "crates/spur-pm",
    "crates/spur-worktree",
    "crates/spur-cost",
    "crates/spur-tui",
    "crates/spur-telegram",
    "crates/spur-cli",
]
```

And add to `[workspace.dependencies]`:

```toml
spur-telegram = { path = "crates/spur-telegram" }
```

- [ ] **Step 4: Implement config.rs**

```rust
// crates/spur-telegram/src/config.rs
use serde::{Deserialize, Serialize};

/// Configuration for the Telegram bot frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    /// Telegram Bot API token (from @BotFather).
    pub bot_token: String,
    /// Telegram user IDs allowed to interact with the bot.
    #[serde(default)]
    pub allowed_users: Vec<u64>,
    /// Optional supergroup chat ID for forum-topic executor threads.
    /// When set, executor work creates forum topics in this group.
    #[serde(default)]
    pub group_chat_id: Option<i64>,
}

impl TelegramConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.bot_token.is_empty(), "bot_token must not be empty");
        Ok(())
    }
}
```

- [ ] **Step 5: Create lib.rs**

```rust
// crates/spur-telegram/src/lib.rs
pub mod config;
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cd /Volumes/Projects/spur && cargo test -p spur-telegram --test config_test -- --nocapture 2>&1 | tail -10`
Expected: 3 tests pass

- [ ] **Step 7: Commit**

```bash
git add crates/spur-telegram/ Cargo.toml
git commit -m "feat(spur-telegram): crate skeleton with TelegramConfig"
```

---

### Task 2: Shared Bot State

**Files:**
- Create: `crates/spur-telegram/src/state.rs`
- Modify: `crates/spur-telegram/src/lib.rs`
- Test: `crates/spur-telegram/tests/state_test.rs`

- [ ] **Step 1: Write the state test**

```rust
// crates/spur-telegram/tests/state_test.rs
use spur_telegram::state::BotState;

#[test]
fn auth_allows_listed_user() {
    let state = BotState::new(vec![111, 222]);
    assert!(state.is_authorized(111));
    assert!(state.is_authorized(222));
    assert!(!state.is_authorized(999));
}

#[test]
fn auth_allows_all_when_empty() {
    let state = BotState::new(vec![]);
    assert!(state.is_authorized(999));
}

#[test]
fn executor_topic_mapping() {
    let state = BotState::new(vec![]);
    state.set_executor_topic("exec-1".into(), 42);
    assert_eq!(state.get_executor_topic("exec-1"), Some(42));
    assert_eq!(state.get_executor_topic("exec-2"), None);
    state.remove_executor_topic("exec-1");
    assert_eq!(state.get_executor_topic("exec-1"), None);
}

#[test]
fn pending_review_store_and_retrieve() {
    let state = BotState::new(vec![]);
    state.set_pending_review("exec-1".into(), 1);
    assert_eq!(state.get_pending_review("exec-1"), Some(1));
    state.remove_pending_review("exec-1");
    assert_eq!(state.get_pending_review("exec-1"), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Volumes/Projects/spur && cargo test -p spur-telegram --test state_test 2>&1 | tail -5`
Expected: compilation error (module doesn't exist)

- [ ] **Step 3: Implement state.rs**

```rust
// crates/spur-telegram/src/state.rs
use dashmap::DashMap;

/// Shared bot state, cheaply cloneable (all fields are Arc-backed).
#[derive(Clone)]
pub struct BotState {
    allowed_users: Vec<u64>,
    /// executor_id → forum topic message_thread_id
    executor_topics: DashMap<String, i32>,
    /// executor_id → attempt_n (for review callback routing)
    pending_reviews: DashMap<String, u32>,
    /// callback_id → permission reply oneshot (stored externally via set/take pattern)
    pending_permissions: DashMap<String, tokio::sync::oneshot::Sender<String>>,
}

impl BotState {
    pub fn new(allowed_users: Vec<u64>) -> Self {
        Self {
            allowed_users,
            executor_topics: DashMap::new(),
            pending_reviews: DashMap::new(),
            pending_permissions: DashMap::new(),
        }
    }

    pub fn is_authorized(&self, user_id: u64) -> bool {
        self.allowed_users.is_empty() || self.allowed_users.contains(&user_id)
    }

    // ── Executor topic mapping ──

    pub fn set_executor_topic(&self, executor_id: String, thread_id: i32) {
        self.executor_topics.insert(executor_id, thread_id);
    }

    pub fn get_executor_topic(&self, executor_id: &str) -> Option<i32> {
        self.executor_topics.get(executor_id).map(|v| *v)
    }

    pub fn remove_executor_topic(&self, executor_id: &str) {
        self.executor_topics.remove(executor_id);
    }

    // ── Pending reviews ──

    pub fn set_pending_review(&self, executor_id: String, attempt_n: u32) {
        self.pending_reviews.insert(executor_id, attempt_n);
    }

    pub fn get_pending_review(&self, executor_id: &str) -> Option<u32> {
        self.pending_reviews.get(executor_id).map(|v| *v)
    }

    pub fn remove_pending_review(&self, executor_id: &str) {
        self.pending_reviews.remove(executor_id);
    }

    // ── Pending permissions ──

    pub fn set_pending_permission(
        &self,
        callback_id: String,
        tx: tokio::sync::oneshot::Sender<String>,
    ) {
        self.pending_permissions.insert(callback_id, tx);
    }

    pub fn take_pending_permission(
        &self,
        callback_id: &str,
    ) -> Option<tokio::sync::oneshot::Sender<String>> {
        self.pending_permissions.remove(callback_id).map(|(_, tx)| tx)
    }
}
```

- [ ] **Step 4: Update lib.rs**

```rust
// crates/spur-telegram/src/lib.rs
pub mod config;
pub mod state;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd /Volumes/Projects/spur && cargo test -p spur-telegram --test state_test -- --nocapture 2>&1 | tail -10`
Expected: 4 tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/spur-telegram/src/state.rs crates/spur-telegram/tests/state_test.rs crates/spur-telegram/src/lib.rs
git commit -m "feat(spur-telegram): BotState with auth, executor-topic, and review mappings"
```

---

### Task 3: Message Formatter

**Files:**
- Create: `crates/spur-telegram/src/formatter.rs`
- Modify: `crates/spur-telegram/src/lib.rs`
- Test: `crates/spur-telegram/tests/formatter_test.rs`

- [ ] **Step 1: Write the formatter tests**

```rust
// crates/spur-telegram/tests/formatter_test.rs
use spur_telegram::formatter;

#[test]
fn short_message_unchanged() {
    let pages = formatter::paginate("Hello world", 4096);
    assert_eq!(pages.len(), 1);
    assert_eq!(pages[0], "Hello world");
}

#[test]
fn long_message_split_at_newline() {
    let line = "x".repeat(100);
    // 50 lines of 100 chars = 5050 chars (with newlines) > 4096
    let text: String = (0..50).map(|_| line.clone()).collect::<Vec<_>>().join("\n");
    let pages = formatter::paginate(&text, 4096);
    assert!(pages.len() > 1);
    for page in &pages {
        assert!(page.len() <= 4096, "page len {} > 4096", page.len());
    }
    // Reassembled content matches original (minus page separators)
    let rejoined: String = pages.join("\n");
    // Should contain all original content
    assert!(rejoined.len() >= text.len());
}

#[test]
fn format_thought_chunk() {
    let text = "Let me think about this...";
    let formatted = formatter::format_thought(text);
    assert!(formatted.contains(text));
    assert!(formatted.starts_with('💭')); // emoji prefix, no markdown
}

#[test]
fn format_tool_call() {
    let formatted = formatter::format_tool_call("Read", r#"{"path": "/tmp/foo"}"#);
    assert!(formatted.contains("Read"));
    assert!(formatted.contains("/tmp/foo"));
}

#[test]
fn format_code_block() {
    let code = "fn main() {}";
    let formatted = formatter::format_code_block(code, Some("rust"));
    assert!(formatted.starts_with("```rust\n"));
    assert!(formatted.ends_with("\n```"));
}

#[test]
fn escape_markdown_v2() {
    let text = "hello_world [test] (foo)";
    let escaped = formatter::escape_md(text);
    assert_eq!(escaped, r"hello\_world \[test\] \(foo\)");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /Volumes/Projects/spur && cargo test -p spur-telegram --test formatter_test 2>&1 | tail -5`
Expected: compilation error

- [ ] **Step 3: Implement formatter.rs**

```rust
// crates/spur-telegram/src/formatter.rs

/// Characters that must be escaped in Telegram MarkdownV2 mode.
const MD_ESCAPE: &[char] = &[
    '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
];

/// Escape text for Telegram MarkdownV2 parse mode.
pub fn escape_md(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 4);
    for ch in text.chars() {
        if MD_ESCAPE.contains(&ch) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Split a long message into pages that fit within Telegram's character limit.
/// Tries to split at newline boundaries to avoid breaking mid-line.
pub fn paginate(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut pages = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            pages.push(remaining.to_string());
            break;
        }

        // Find a newline to split at, searching backwards from max_len
        let split_at = remaining[..max_len]
            .rfind('\n')
            .map(|pos| pos + 1) // include the newline in the current page
            .unwrap_or(max_len); // no newline found, hard split

        pages.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }

    pages
}

/// Format a thought chunk (agent's internal reasoning) for Telegram.
pub fn format_thought(text: &str) -> String {
    format!("💭 {}", text.trim())
}

/// Format a tool call for Telegram display (plain text + code block).
pub fn format_tool_call(tool_name: &str, args_json: &str) -> String {
    // Truncate very long args
    let args_display = if args_json.len() > 500 {
        format!("{}...", &args_json[..500])
    } else {
        args_json.to_string()
    };
    format!("🔧 {}\n```json\n{}\n```", tool_name, args_display)
}

/// Format a code block with optional language tag.
pub fn format_code_block(code: &str, lang: Option<&str>) -> String {
    match lang {
        Some(l) => format!("```{}\n{}\n```", l, code),
        None => format!("```\n{}\n```", code),
    }
}

/// Format an executor review request summary (plain text).
pub fn format_review_summary(summary: &str, diff_files: usize, insertions: usize, deletions: usize) -> String {
    format!(
        "{}\n\n{} files changed, +{} -{}", summary, diff_files, insertions, deletions,
    )
}
```

- [ ] **Step 4: Update lib.rs**

```rust
// crates/spur-telegram/src/lib.rs
pub mod config;
pub mod formatter;
pub mod state;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cd /Volumes/Projects/spur && cargo test -p spur-telegram --test formatter_test -- --nocapture 2>&1 | tail -15`
Expected: 6 tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/spur-telegram/src/formatter.rs crates/spur-telegram/tests/formatter_test.rs crates/spur-telegram/src/lib.rs
git commit -m "feat(spur-telegram): message formatter with pagination, escaping, and rendering"
```

---

### Task 4: Command Handlers

**Files:**
- Create: `crates/spur-telegram/src/handlers/mod.rs`
- Create: `crates/spur-telegram/src/handlers/commands.rs`
- Modify: `crates/spur-telegram/src/lib.rs`

- [ ] **Step 1: Create handlers/mod.rs**

```rust
// crates/spur-telegram/src/handlers/mod.rs
pub mod callback;
pub mod commands;
pub mod message;
```

- [ ] **Step 2: Implement commands.rs**

```rust
// crates/spur-telegram/src/handlers/commands.rs
use teloxide::prelude::*;
use teloxide::utils::command::BotCommands;
use tokio::sync::mpsc;

use crate::state::BotState;

#[derive(BotCommands, Clone)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
pub enum Command {
    #[command(description = "Start the bot and show welcome message")]
    Start,
    #[command(description = "List available brain agents")]
    Brains,
    #[command(description = "List sessions")]
    Sessions,
    #[command(description = "Resume a session by ACP ID")]
    Resume(String),
    #[command(description = "Show help")]
    Help,
}

pub async fn handle_command(
    bot: Bot,
    msg: Message,
    cmd: Command,
    state: BotState,
    input_tx: mpsc::Sender<spur_core::InteractiveInput>,
) -> ResponseResult<()> {
    let user_id = msg.from.as_ref().map(|u| u.id.0).unwrap_or(0);
    if !state.is_authorized(user_id) {
        bot.send_message(msg.chat.id, "Unauthorized.").await?;
        return Ok(());
    }

    match cmd {
        Command::Start => {
            bot.send_message(
                msg.chat.id,
                "SPUR Telegram Bot\n\nSend a message to chat with the brain agent.\n\nCommands:\n/brains - list agents\n/sessions - list sessions\n/resume <id> - resume session\n/help - show help",
            )
            .await?;
        }
        Command::Brains => {
            // The orchestrator doesn't expose a "list brains" command through
            // InteractiveInput. We list known agents from config instead.
            // For now, send a placeholder that the renderer will enrich
            // once the orchestrator emits agent info.
            bot.send_message(msg.chat.id, "Use /sessions to see available sessions. Brain is configured in .spur/config.toml.")
                .await?;
        }
        Command::Sessions => {
            if let Err(e) = input_tx.send(spur_core::InteractiveInput::ListSessions).await {
                tracing::warn!(error = %e, "failed to send ListSessions");
                bot.send_message(msg.chat.id, "Failed to request session list.")
                    .await?;
            }
            // Response comes asynchronously via SpurEvent::SessionsListed,
            // handled in the renderer event loop.
        }
        Command::Resume(session_id) => {
            let session_id = session_id.trim().to_string();
            if session_id.is_empty() {
                bot.send_message(msg.chat.id, "Usage: /resume <session-id>")
                    .await?;
            } else if let Err(e) = input_tx
                .send(spur_core::InteractiveInput::ResumeSession { session_id })
                .await
            {
                tracing::warn!(error = %e, "failed to send ResumeSession");
                bot.send_message(msg.chat.id, "Failed to resume session.")
                    .await?;
            }
        }
        Command::Help => {
            bot.send_message(msg.chat.id, Command::descriptions().to_string())
                .await?;
        }
    }
    Ok(())
}
```

- [ ] **Step 3: Update lib.rs**

```rust
// crates/spur-telegram/src/lib.rs
pub mod config;
pub mod formatter;
pub mod handlers;
pub mod state;
```

- [ ] **Step 4: Verify it compiles**

Run: `cd /Volumes/Projects/spur && cargo check -p spur-telegram 2>&1 | tail -10`
Expected: compiles successfully (possibly with warnings about unused imports)

- [ ] **Step 5: Commit**

```bash
git add crates/spur-telegram/src/handlers/
git commit -m "feat(spur-telegram): command handlers for /start /brains /sessions /resume /help"
```

---

### Task 5: Message Handler (Brain Chat Input)

**Files:**
- Create: `crates/spur-telegram/src/handlers/message.rs`

- [ ] **Step 1: Implement message.rs**

```rust
// crates/spur-telegram/src/handlers/message.rs
use teloxide::prelude::*;
use tokio::sync::mpsc;

use agent_client_protocol::{ContentBlock, TextContent};

use crate::state::BotState;

/// Handle a plain text message from the user (brain chat input).
pub async fn handle_message(
    bot: Bot,
    msg: Message,
    state: BotState,
    input_tx: mpsc::Sender<spur_core::InteractiveInput>,
) -> ResponseResult<()> {
    let user_id = msg.from.as_ref().map(|u| u.id.0).unwrap_or(0);
    if !state.is_authorized(user_id) {
        return Ok(());
    }

    let text = match msg.text() {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return Ok(()), // ignore non-text messages for now
    };

    let input = spur_core::InteractiveInput::Message {
        blocks: vec![ContentBlock::Text(TextContent::new(text))],
        interrupt: false,
    };

    if let Err(e) = input_tx.send(input).await {
        tracing::warn!(error = %e, "failed to send user message to orchestrator");
        bot.send_message(msg.chat.id, "Failed to send message. Is the brain connected?")
            .await?;
    }

    Ok(())
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd /Volumes/Projects/spur && cargo check -p spur-telegram 2>&1 | tail -10`
Expected: compiles successfully

- [ ] **Step 3: Commit**

```bash
git add crates/spur-telegram/src/handlers/message.rs
git commit -m "feat(spur-telegram): message handler converts text to InteractiveInput"
```

---

### Task 6: Callback Handler (Reviews and Permissions)

**Files:**
- Create: `crates/spur-telegram/src/handlers/callback.rs`

- [ ] **Step 1: Implement callback.rs**

```rust
// crates/spur-telegram/src/handlers/callback.rs
use teloxide::prelude::*;
use tokio::sync::mpsc;

use spur_acp::ReviewDecision;

use crate::state::BotState;

/// Handle an inline keyboard callback query.
///
/// Callback data format:
/// - `review:approve:<executor_id>` — approve executor review
/// - `review:reject:<executor_id>` — reject executor review
/// - `review:retry:<executor_id>` — retry executor review
/// - `perm:<callback_id>:<option_id>` — permission decision
pub async fn handle_callback(
    bot: Bot,
    q: CallbackQuery,
    state: BotState,
    dispatch_tx: mpsc::Sender<spur_core::InteractiveInput>,
) -> ResponseResult<()> {
    let user_id = q.from.id.0;
    if !state.is_authorized(user_id) {
        bot.answer_callback_query(&q.id).text("Unauthorized.").await?;
        return Ok(());
    }

    let data = match q.data.as_deref() {
        Some(d) => d,
        None => return Ok(()),
    };

    if let Some(rest) = data.strip_prefix("review:") {
        handle_review_callback(&bot, &q, rest, &state, &dispatch_tx).await?;
    } else if let Some(rest) = data.strip_prefix("perm:") {
        handle_permission_callback(&bot, &q, rest, &state).await?;
    }

    Ok(())
}

async fn handle_review_callback(
    bot: &Bot,
    q: &CallbackQuery,
    data: &str,
    state: &BotState,
    dispatch_tx: &mpsc::Sender<spur_core::InteractiveInput>,
) -> ResponseResult<()> {
    // Format: "<action>:<executor_id>"
    let (action, executor_id) = match data.split_once(':') {
        Some(pair) => pair,
        None => {
            bot.answer_callback_query(&q.id).text("Invalid callback").await?;
            return Ok(());
        }
    };

    let attempt_n = match state.get_pending_review(executor_id) {
        Some(n) => n,
        None => {
            bot.answer_callback_query(&q.id).text("Review expired").await?;
            return Ok(());
        }
    };

    let decision = match action {
        "approve" => ReviewDecision::Approve,
        "reject" => ReviewDecision::Reject {
            reason: "Rejected via Telegram".into(),
        },
        "retry" => ReviewDecision::Retry {
            new_constraints: String::new(),
        },
        _ => {
            bot.answer_callback_query(&q.id).text("Unknown action").await?;
            return Ok(());
        }
    };

    let label = match &decision {
        ReviewDecision::Approve => "Approved",
        ReviewDecision::Reject { .. } => "Rejected",
        ReviewDecision::Retry { .. } => "Retrying",
        ReviewDecision::Modify { .. } => "Modified",
    };

    state.remove_pending_review(executor_id);

    let input = spur_core::InteractiveInput::SubmitReview {
        executor_id: executor_id.to_string(),
        attempt_n,
        decision,
    };

    if let Err(e) = dispatch_tx.send(input).await {
        tracing::warn!(error = %e, "failed to send review decision");
        bot.answer_callback_query(&q.id).text("Failed to submit").await?;
        return Ok(());
    }

    bot.answer_callback_query(&q.id).text(label).await?;

    // Edit the message to show the decision and remove the keyboard
    if let Some(msg) = &q.message {
        if let Some(msg) = msg.regular_message() {
            let text = msg.text().unwrap_or("");
            let updated = format!("{}\n\n--- {} ---", text, label);
            if let Err(e) = bot.edit_message_text(msg.chat.id, msg.id, updated).await {
                tracing::warn!(error = %e, "failed to edit review message");
            }
        }
    }

    Ok(())
}

async fn handle_permission_callback(
    bot: &Bot,
    q: &CallbackQuery,
    data: &str,
    state: &BotState,
) -> ResponseResult<()> {
    // Format: "<callback_id>:<option_id>"
    let (callback_id, option_id) = match data.split_once(':') {
        Some(pair) => pair,
        None => {
            bot.answer_callback_query(&q.id).text("Invalid callback").await?;
            return Ok(());
        }
    };

    if let Some(tx) = state.take_pending_permission(callback_id) {
        let _ = tx.send(option_id.to_string());
        bot.answer_callback_query(&q.id)
            .text(format!("Selected: {}", option_id))
            .await?;
    } else {
        bot.answer_callback_query(&q.id).text("Permission expired").await?;
    }

    Ok(())
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cd /Volumes/Projects/spur && cargo check -p spur-telegram 2>&1 | tail -10`
Expected: compiles successfully

- [ ] **Step 3: Commit**

```bash
git add crates/spur-telegram/src/handlers/callback.rs
git commit -m "feat(spur-telegram): callback handler for review decisions and permissions"
```

---

### Task 7: Event Renderer

**Files:**
- Create: `crates/spur-telegram/src/renderer.rs`
- Modify: `crates/spur-telegram/src/lib.rs`

This is the core module: it consumes `SpurEvent`s from the broadcast channel and translates them into Telegram messages/keyboards.

- [ ] **Step 1: Implement renderer.rs**

```rust
// crates/spur-telegram/src/renderer.rs
use teloxide::prelude::*;
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup, ThreadId};
use tokio::sync::broadcast;

use spur_acp::{SpurEvent, SpurEventBody, SessionUpdate};

use crate::formatter;
use crate::state::BotState;

/// Accumulated text for the current brain response, edited in-place.
struct ResponseAccumulator {
    chat_id: ChatId,
    message_id: Option<MessageId>,
    text: String,
    last_edit: tokio::time::Instant,
}

impl ResponseAccumulator {
    fn new(chat_id: ChatId) -> Self {
        Self {
            chat_id,
            message_id: None,
            text: String::new(),
            last_edit: tokio::time::Instant::now(),
        }
    }

    async fn append(&mut self, bot: &Bot, chunk: &str) {
        self.text.push_str(chunk);

        // Throttle edits to every 2 seconds to avoid rate limits
        let elapsed = self.last_edit.elapsed();
        if elapsed < tokio::time::Duration::from_secs(2) && self.message_id.is_some() {
            return;
        }

        self.flush(bot).await;
    }

    async fn flush(&mut self, bot: &Bot) {
        if self.text.is_empty() {
            return;
        }

        // Telegram limit: truncate for the edit, full text sent on finalize
        let display = if self.text.len() > 4000 {
            format!("{}...", &self.text[..4000])
        } else {
            self.text.clone()
        };

        match self.message_id {
            Some(mid) => {
                if let Err(e) = bot.edit_message_text(self.chat_id, mid, &display).await {
                    tracing::warn!(error = %e, "failed to edit streamed message");
                }
            }
            None => {
                if let Ok(sent) = bot.send_message(self.chat_id, &display).await {
                    self.message_id = Some(sent.id);
                }
            }
        }
        self.last_edit = tokio::time::Instant::now();
    }

    async fn finalize(&mut self, bot: &Bot) {
        if self.text.is_empty() {
            return;
        }

        // Send paginated final response
        let pages = formatter::paginate(&self.text, 4096);
        match self.message_id {
            Some(mid) => {
                // Edit the first message with page 1
                if let Err(e) = bot.edit_message_text(self.chat_id, mid, &pages[0]).await {
                    tracing::warn!(error = %e, "failed to edit final message");
                }
                // Send remaining pages as new messages
                for page in &pages[1..] {
                    if let Err(e) = bot.send_message(self.chat_id, page).await {
                        tracing::warn!(error = %e, "failed to send paginated message");
                    }
                }
            }
            None => {
                for page in &pages {
                    if let Err(e) = bot.send_message(self.chat_id, page).await {
                        tracing::warn!(error = %e, "failed to send paginated message");
                    }
                }
            }
        }
    }

    fn reset(&mut self) {
        self.text.clear();
        self.message_id = None;
    }
}

/// Run the event renderer loop. Consumes SpurEvents and sends Telegram messages.
///
/// This is spawned as a tokio task by `run_telegram`.
pub async fn event_loop(
    bot: Bot,
    chat_id: ChatId,
    group_chat_id: Option<ChatId>,
    mut event_rx: broadcast::Receiver<SpurEvent>,
    state: BotState,
    mut perm_rx: tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let mut accumulator = ResponseAccumulator::new(chat_id);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                accumulator.finalize(&bot).await;
                break;
            }
            event = event_rx.recv() => {
                let event = match event {
                    Ok(e) => e,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(skipped = n, "event receiver lagged");
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };

                handle_event(&bot, chat_id, group_chat_id, &event, &state, &mut accumulator).await;
            }
            Some(perm) = perm_rx.recv() => {
                handle_permission(&bot, chat_id, perm, &state).await;
            }
        }
    }
}

async fn handle_event(
    bot: &Bot,
    chat_id: ChatId,
    group_chat_id: Option<ChatId>,
    event: &SpurEvent,
    state: &BotState,
    accumulator: &mut ResponseAccumulator,
) {
    match &event.body {
        SpurEventBody::BrainSpawned { agent, session } => {
            if let Err(e) = bot
                .send_message(chat_id, format!("Brain connected: {} (session {})", agent, &session.0[..8]))
                .await
            {
                tracing::warn!(error = %e, "failed to send brain spawned notification");
            }
        }

        SpurEventBody::AgentSessionReady { brain, resumed, .. } => {
            let status = if *resumed { "resumed" } else { "new" };
            if let Err(e) = bot
                .send_message(chat_id, format!("Session ready ({} - {})", brain, status))
                .await
            {
                tracing::warn!(error = %e, "failed to send session ready notification");
            }
        }

        SpurEventBody::AgentNotification { notification, .. } => {
            match &notification.update {
                SessionUpdate::AgentMessageChunk(chunk) => {
                    if let Some(blocks) = chunk.content_blocks.as_ref() {
                        for block in blocks {
                            if let Some(text) = extract_text_from_content(block) {
                                if !text.is_empty() {
                                    accumulator.append(bot, &text).await;
                                }
                            }
                        }
                    }
                }
                SessionUpdate::AgentThoughtChunk(chunk) => {
                    // Thoughts are shown with emoji prefix, plain text
                    if let Some(blocks) = chunk.content_blocks.as_ref() {
                        for block in blocks {
                            if let Some(text) = extract_text_from_content(block) {
                                if !text.is_empty() {
                                    if let Err(e) = bot
                                        .send_message(chat_id, format!("💭 {}", text))
                                        .await
                                    {
                                        tracing::warn!(error = %e, "failed to send thought chunk");
                                    }
                                }
                            }
                        }
                    }
                }
                SessionUpdate::ToolCall(tc) => {
                    // Flush accumulated response before showing tool call
                    accumulator.flush(bot).await;
                    let args = tc
                        .raw_input
                        .as_ref()
                        .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                        .unwrap_or_default();
                    let text = format!("Tool: {}\n```json\n{}\n```", tc.title, args);
                    if let Err(e) = bot.send_message(chat_id, text).await {
                        tracing::warn!(error = %e, "failed to send tool call");
                    }
                }
                SessionUpdate::ToolCallUpdate(tcu) => {
                    // Show tool result briefly
                    if let Some(output) = &tcu.fields.raw_output {
                        let text = serde_json::to_string_pretty(output).unwrap_or_default();
                        if !text.is_empty() && text.len() < 2000 {
                            if let Err(e) = bot
                                .send_message(chat_id, format!("```\n{}\n```", text))
                                .await
                            {
                                tracing::warn!(error = %e, "failed to send tool result");
                            }
                        }
                    }
                }
                SessionUpdate::Plan(plan) => {
                    let text: String = plan
                        .entries
                        .iter()
                        .map(|e| {
                            let marker = match &e.status {
                                spur_acp::PlanEntryStatus::Completed => "[x]",
                                spur_acp::PlanEntryStatus::InProgress => "[~]",
                                _ => "[ ]",
                            };
                            format!("{} {}", marker, e.content)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    if let Err(e) = bot.send_message(chat_id, format!("Plan:\n{}", text)).await {
                        tracing::warn!(error = %e, "failed to send plan");
                    }
                }
                _ => {} // UsageUpdate, CurrentModeUpdate, etc. — skip
            }
        }

        SpurEventBody::TurnComplete { .. } => {
            accumulator.finalize(bot).await;
            accumulator.reset();
        }

        SpurEventBody::BrainError { message, .. } => {
            accumulator.finalize(bot).await;
            accumulator.reset();
            if let Err(e) = bot
                .send_message(chat_id, format!("Error: {}", message))
                .await
            {
                tracing::warn!(error = %e, "failed to send brain error");
            }
        }

        SpurEventBody::AuthRequired { message, .. } => {
            if let Err(e) = bot
                .send_message(chat_id, format!("Authentication required: {}", message))
                .await
            {
                tracing::warn!(error = %e, "failed to send auth required");
            }
        }

        // ── Executor lifecycle → forum topics ──

        SpurEventBody::ExecutorSpawned { id, agent, task_spec, .. } => {
            if let Some(group_id) = group_chat_id {
                let title = format!("{}: {}", agent, truncate(task_spec, 60));
                match bot.create_forum_topic(group_id, &title).await {
                    Ok(topic) => {
                        let thread_id = topic.message_thread_id.0;
                        state.set_executor_topic(id.clone(), thread_id);
                        if let Err(e) = bot
                            .send_message(group_id, format!("Task: {}\nAgent: {}", task_spec, agent))
                            .message_thread_id(ThreadId(teloxide::types::MessageId(thread_id)))
                            .await
                        {
                            tracing::warn!(error = %e, "failed to send executor task to forum topic");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to create forum topic for executor");
                        if let Err(e) = bot
                            .send_message(chat_id, format!("Executor started: {} — {}", agent, truncate(task_spec, 80)))
                            .await
                        {
                            tracing::warn!(error = %e, "failed to send executor fallback notification");
                        }
                    }
                }
            } else {
                if let Err(e) = bot
                    .send_message(chat_id, format!("Executor started: {} — {}", agent, truncate(task_spec, 80)))
                    .await
                {
                    tracing::warn!(error = %e, "failed to send executor notification");
                }
            }
        }

        SpurEventBody::ExecutorReviewRequested { id, attempt_n, kind, payload } => {
            state.set_pending_review(id.clone(), *attempt_n);

            let summary = &payload.summary;
            let diff_info = payload.diff_summary.as_ref().map(|d| {
                format!("{} files, +{} -{}", d.files_changed, d.insertions, d.deletions)
            }).unwrap_or_default();

            let text = format!(
                "Review requested ({:?}):\n{}\n{}",
                kind, summary, diff_info,
            );

            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![
                    InlineKeyboardButton::callback("Approve", format!("review:approve:{}", id)),
                    InlineKeyboardButton::callback("Reject", format!("review:reject:{}", id)),
                ],
                vec![
                    InlineKeyboardButton::callback("Retry", format!("review:retry:{}", id)),
                ],
            ]);

            // Send to forum topic if available, otherwise to main chat
            let target_chat = group_chat_id.unwrap_or(chat_id);
            let thread_id = state.get_executor_topic(id);

            let mut req = bot.send_message(target_chat, text).reply_markup(keyboard);
            if let Some(tid) = thread_id {
                req = req.message_thread_id(ThreadId(teloxide::types::MessageId(tid)));
            }
            if let Err(e) = req.await {
                tracing::warn!(error = %e, "failed to send review request");
            }
        }

        SpurEventBody::ExecutorReviewResolved { id, decision } => {
            let label = match decision {
                ReviewDecision::Approve => "Approved",
                ReviewDecision::Reject { .. } => "Rejected",
                ReviewDecision::Retry { .. } => "Retrying",
                ReviewDecision::Modify { .. } => "Modified",
            };

            let target_chat = group_chat_id.unwrap_or(chat_id);
            let thread_id = state.get_executor_topic(id);
            let mut req = bot.send_message(target_chat, format!("Review resolved: {}", label));
            if let Some(tid) = thread_id {
                req = req.message_thread_id(ThreadId(teloxide::types::MessageId(tid)));
            }
            if let Err(e) = req.await {
                tracing::warn!(error = %e, "failed to send review resolved");
            }
        }

        SpurEventBody::ExecutorPhaseChanged { id, phase } => {
            if *phase == spur_acp::LifecycleState::Succeeded || *phase == spur_acp::LifecycleState::Failed {
                // Close forum topic when executor finishes
                if let Some(group_id) = group_chat_id {
                    if let Some(tid) = state.get_executor_topic(id) {
                        let status = if *phase == spur_acp::LifecycleState::Succeeded { "completed" } else { "failed" };
                        if let Err(e) = bot
                            .send_message(group_id, format!("Executor {}", status))
                            .message_thread_id(ThreadId(teloxide::types::MessageId(tid)))
                            .await
                        {
                            tracing::warn!(error = %e, "failed to send executor phase change");
                        }
                        if let Err(e) = bot.close_forum_topic(group_id, ThreadId(teloxide::types::MessageId(tid))).await {
                            tracing::warn!(error = %e, "failed to close forum topic");
                        }
                        state.remove_executor_topic(id);
                    }
                }
            }
        }

        SpurEventBody::SessionsListed { sessions, .. } => {
            if sessions.is_empty() {
                if let Err(e) = bot.send_message(chat_id, "No sessions found.").await {
                    tracing::warn!(error = %e, "failed to send sessions list");
                }
            } else {
                let mut text = String::from("Sessions:\n\n");
                for (i, s) in sessions.iter().enumerate().take(20) {
                    let id_short = if s.id.len() > 8 { &s.id[..8] } else { &s.id };
                    let title = s.title.as_deref().unwrap_or("(untitled)");
                    text.push_str(&format!("{}. `{}` — {}\n", i + 1, id_short, title));
                }
                text.push_str("\nUse /resume <full-session-id> to resume.");
                if let Err(e) = bot.send_message(chat_id, text).await {
                    tracing::warn!(error = %e, "failed to send sessions list");
                }
            }
        }

        SpurEventBody::SessionsListError { message } => {
            if let Err(e) = bot
                .send_message(chat_id, format!("Failed to list sessions: {}", message))
                .await
            {
                tracing::warn!(error = %e, "failed to send sessions list error");
            }
        }

        _ => {} // CostUpdate, PrCreated, etc. — skip for MVP
    }
}

async fn handle_permission(
    bot: &Bot,
    chat_id: ChatId,
    perm: spur_acp::types::PermissionRequest,
    state: &BotState,
) {
    let callback_id = uuid::Uuid::new_v4().to_string()[..8].to_string();

    let description = perm
        .args
        .tool_call
        .as_ref()
        .map(|tc| tc.title.clone())
        .unwrap_or_else(|| "Permission requested".to_string());

    let buttons: Vec<InlineKeyboardButton> = perm
        .args
        .options
        .iter()
        .map(|opt| {
            InlineKeyboardButton::callback(
                &opt.label,
                format!("perm:{}:{}", callback_id, opt.id.0),
            )
        })
        .collect();

    let keyboard = InlineKeyboardMarkup::new(vec![buttons]);

    if let Err(e) = bot
        .send_message(chat_id, format!("Permission: {}", description))
        .reply_markup(keyboard)
        .await
    {
        tracing::warn!(error = %e, "failed to send permission request");
    }

    state.set_pending_permission(callback_id, perm.reply_tx);
}

fn extract_text_from_content(block: &spur_acp::ContentChunk) -> Option<String> {
    // ContentChunk may contain text data
    Some(block.data.clone())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}
```

- [ ] **Step 2: Update lib.rs**

```rust
// crates/spur-telegram/src/lib.rs
pub mod config;
pub mod formatter;
pub mod handlers;
pub mod renderer;
pub mod state;
```

- [ ] **Step 3: Verify it compiles**

Run: `cd /Volumes/Projects/spur && cargo check -p spur-telegram 2>&1 | tail -15`
Expected: compiles (may need minor type adjustments based on actual teloxide/ACP types)

Note: The `ContentChunk`, `ThreadId`, and `ToolCall` types may need adjustment based on the exact teloxide 0.13 and agent-client-protocol 0.10 APIs. Fix any type mismatches reported by the compiler. The key patterns (accumulator, event dispatch, forum topics, inline keyboards) are correct — only the field access paths may need tweaking.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-telegram/src/renderer.rs crates/spur-telegram/src/lib.rs
git commit -m "feat(spur-telegram): event renderer with brain streaming, executor topics, and review keyboards"
```

---

### Task 8: Bot Dispatcher Setup

**Files:**
- Create: `crates/spur-telegram/src/bot.rs`

- [ ] **Step 1: Implement bot.rs**

```rust
// crates/spur-telegram/src/bot.rs
use teloxide::prelude::*;
use teloxide::dispatching::UpdateFilterExt;
use tokio::sync::mpsc;

use crate::handlers::{callback, commands, message};
use crate::state::BotState;

/// Build and run the teloxide dispatcher.
///
/// `input_tx` feeds the orchestrator's `run_interactive()` (non-review inputs).
/// `dispatch_tx` feeds the `review_dispatcher_loop` (SubmitReview only).
pub async fn run_dispatcher(
    bot: Bot,
    state: BotState,
    input_tx: mpsc::Sender<spur_core::InteractiveInput>,
    dispatch_tx: mpsc::Sender<spur_core::InteractiveInput>,
) {
    let handler = Update::filter_message()
        .branch(
            dptree::entry()
                .filter_command::<commands::Command>()
                .endpoint({
                    let state = state.clone();
                    let tx = input_tx.clone();
                    move |bot: Bot, msg: Message, cmd: commands::Command| {
                        let state = state.clone();
                        let tx = tx.clone();
                        async move { commands::handle_command(bot, msg, cmd, state, tx).await }
                    }
                }),
        )
        .branch(dptree::entry().endpoint({
            let state = state.clone();
            let tx = input_tx.clone();
            move |bot: Bot, msg: Message| {
                let state = state.clone();
                let tx = tx.clone();
                async move { message::handle_message(bot, msg, state, tx).await }
            }
        }));

    let callback_handler = Update::filter_callback_query().endpoint({
        let state = state.clone();
        let tx = dispatch_tx.clone();
        move |bot: Bot, q: CallbackQuery| {
            let state = state.clone();
            let tx = tx.clone();
            async move { callback::handle_callback(bot, q, state, tx).await }
        }
    });

    let full_handler = dptree::entry()
        .branch(handler)
        .branch(callback_handler);

    Dispatcher::builder(bot, full_handler)
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
}
```

- [ ] **Step 2: Update lib.rs**

```rust
// crates/spur-telegram/src/lib.rs
pub mod bot;
pub mod config;
pub mod formatter;
pub mod handlers;
pub mod renderer;
pub mod state;
```

- [ ] **Step 3: Verify it compiles**

Run: `cd /Volumes/Projects/spur && cargo check -p spur-telegram 2>&1 | tail -15`
Expected: compiles successfully

- [ ] **Step 4: Commit**

```bash
git add crates/spur-telegram/src/bot.rs crates/spur-telegram/src/lib.rs
git commit -m "feat(spur-telegram): teloxide dispatcher with command, message, and callback routing"
```

---

### Task 9: Entry Point (`run_telegram`) and CLI Integration

**Files:**
- Modify: `crates/spur-telegram/src/lib.rs`
- Modify: `crates/spur-cli/src/main.rs`
- Modify: `crates/spur-cli/Cargo.toml`

This wires everything together — the same channel topology as `Watch` but with the Telegram bot instead of the TUI.

- [ ] **Step 1: Implement run_telegram in lib.rs**

Replace the contents of `crates/spur-telegram/src/lib.rs`:

```rust
// crates/spur-telegram/src/lib.rs
pub mod bot;
pub mod config;
pub mod formatter;
pub mod handlers;
pub mod renderer;
pub mod state;

use teloxide::prelude::*;
use teloxide::adaptors::throttle::Limits;
use teloxide::requests::RequesterExt;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use config::TelegramConfig;
use state::BotState;

/// Run the Telegram bot frontend.
///
/// This is the Telegram equivalent of `spur_tui::run_tui`. It:
/// 1. Creates the teloxide Bot from the config token, wrapped in Throttle
/// 2. Spawns the event renderer loop (SpurEvent → Telegram messages)
/// 3. Runs the teloxide dispatcher (Telegram updates → InteractiveInput)
///
/// The caller (spur-cli) is responsible for:
/// - Creating the Orchestrator and calling `orch.subscribe()` for `event_rx`
/// - Setting up `user_tx`/`dispatch_tx` channels and passing the receivers
///   to `orch.run_interactive()` and `review_dispatcher_loop()`
/// - Passing the `perm_rx` for permission requests
pub async fn run_telegram(
    config: TelegramConfig,
    event_rx: broadcast::Receiver<spur_acp::SpurEvent>,
    user_tx: mpsc::Sender<spur_core::InteractiveInput>,
    dispatch_tx: mpsc::Sender<spur_core::InteractiveInput>,
    perm_rx: tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>,
) -> anyhow::Result<()> {
    config.validate()?;

    // Wrap with Throttle to automatically respect Telegram rate limits
    let bot = Bot::new(&config.bot_token).throttle(Limits::default());
    let state = BotState::new(config.allowed_users.clone());

    let chat_id = ChatId(config.allowed_users.first().copied().unwrap_or(0) as i64);
    let group_chat_id = config.group_chat_id.map(ChatId);

    // CancellationToken coordinates shutdown between dispatcher and renderer
    let cancel = CancellationToken::new();

    // Spawn the event renderer (SpurEvent → Telegram messages)
    let renderer_bot = bot.clone();
    let renderer_state = state.clone();
    let renderer_cancel = cancel.clone();
    tokio::spawn(async move {
        renderer::event_loop(
            renderer_bot,
            chat_id,
            group_chat_id,
            event_rx,
            renderer_state,
            perm_rx,
            renderer_cancel,
        )
        .await;
    });

    // Run the teloxide dispatcher (blocks until Ctrl+C or bot stops)
    bot::run_dispatcher(bot, state, user_tx, dispatch_tx).await;

    // Dispatcher exited — signal renderer to stop
    cancel.cancel();

    Ok(())
}
```

- [ ] **Step 2: Add spur-telegram to spur-cli dependencies**

In `crates/spur-cli/Cargo.toml`, add:

```toml
spur-telegram = { workspace = true }
```

- [ ] **Step 3: Add Telegram subcommand to spur-cli**

In `crates/spur-cli/src/main.rs`, add the `Telegram` variant to the `Commands` enum:

```rust
    /// Launch Telegram bot frontend
    Telegram {
        /// Path to telegram config TOML (default: .spur/telegram.toml)
        #[arg(long, default_value = ".spur/telegram.toml")]
        config: String,
        /// Override the brain agent
        #[arg(long)]
        brain: Option<String>,
    },
```

And add the handler in the `match cli.command` block, right before the `Commands::Watch` arm. This mirrors the Watch command's channel wiring but uses `run_telegram` instead of `run_tui`:

```rust
        Commands::Telegram { config: config_path, brain } => {
            let telegram_config: spur_telegram::config::TelegramConfig = {
                let content = std::fs::read_to_string(&config_path)
                    .with_context(|| format!("Failed to read telegram config: {}", config_path))?;
                toml::from_str(&content)?
            };

            let orch = load_orchestrator(repo_root)?;
            let event_rx = orch.subscribe();
            let review_sink_for_dispatcher = orch.review_sink.clone();

            let (perm_tx, perm_rx) =
                tokio::sync::mpsc::unbounded_channel::<spur_acp::types::PermissionRequest>();

            let (user_tx, user_rx) = tokio::sync::mpsc::channel::<spur_core::InteractiveInput>(32);
            let (dispatch_tx, dispatch_rx) = tokio::sync::mpsc::channel::<spur_core::InteractiveInput>(32);

            tokio::spawn(spur_core::review_dispatcher_loop(dispatch_rx, review_sink_for_dispatcher));

            let mut orch_handle = tokio::spawn(async move {
                if let Err(e) = orch.run_interactive(user_rx, brain, Some(perm_tx)).await {
                    tracing::error!(error = %e, "Interactive session error");
                }
            });

            let telegram_result = spur_telegram::run_telegram(
                telegram_config,
                event_rx,
                user_tx,
                dispatch_tx.clone(),
                perm_rx,
            )
            .await;

            match tokio::time::timeout(std::time::Duration::from_secs(5), &mut orch_handle).await {
                Ok(_) => tracing::info!("orchestrator shut down gracefully"),
                Err(_) => {
                    tracing::warn!("orchestrator shutdown timed out after 5s; aborting");
                    orch_handle.abort();
                    let _ = (&mut orch_handle).await;
                }
            }

            telegram_result?;
            Ok(())
        }
```

- [ ] **Step 4: Verify it compiles**

Run: `cd /Volumes/Projects/spur && cargo check -p spur-cli 2>&1 | tail -15`
Expected: compiles (fix any type issues from teloxide/ACP API mismatches)

- [ ] **Step 5: Commit**

```bash
git add crates/spur-telegram/src/lib.rs crates/spur-cli/src/main.rs crates/spur-cli/Cargo.toml Cargo.toml
git commit -m "feat(spur-cli): add 'spur telegram' subcommand wired to spur-telegram frontend"
```

---

### Task 10: Integration Smoke Test

**Files:**
- Create: `.spur/telegram.toml.example`

- [ ] **Step 1: Create example config**

```toml
# .spur/telegram.toml.example
# Get a bot token from @BotFather on Telegram
bot_token = "YOUR_BOT_TOKEN_HERE"

# Your Telegram user ID (get it from @userinfobot)
allowed_users = [123456789]

# Optional: supergroup chat ID for forum-topic executor threads
# Create a supergroup, enable topics, add the bot as admin with can_manage_topics
# group_chat_id = -1001234567890
```

- [ ] **Step 2: Add .spur/telegram.toml to .gitignore**

Append to `.gitignore`:

```
.spur/telegram.toml
```

This prevents accidental commit of the bot token.

- [ ] **Step 3: Verify full build**

Run: `cd /Volumes/Projects/spur && cargo build -p spur-cli 2>&1 | tail -10`
Expected: successful build

- [ ] **Step 4: Verify tests pass**

Run: `cd /Volumes/Projects/spur && cargo test -p spur-telegram 2>&1 | tail -15`
Expected: all tests pass (config_test, state_test, formatter_test)

- [ ] **Step 5: Verify CLI help shows telegram command**

Run: `cd /Volumes/Projects/spur && cargo run -- --help 2>&1`
Expected: `telegram` appears in the subcommand list

- [ ] **Step 6: Commit**

```bash
git add .spur/telegram.toml.example .gitignore
git commit -m "docs(spur-telegram): add example telegram config, gitignore bot token"
```

---

### Task 11: Fix Compilation Issues

This task exists because Tasks 4-9 contain code written against assumed APIs. The teloxide 0.13 and agent-client-protocol 0.10 APIs may differ in field names, enum variants, or method signatures.

- [ ] **Step 1: Run full cargo check and collect errors**

Run: `cd /Volumes/Projects/spur && cargo check -p spur-telegram 2>&1 | head -100`

- [ ] **Step 2: Fix each compilation error**

Common issues to watch for:
- `ContentChunk` field names (may be `.data` vs `.text` vs `.content`)
- `ThreadId` constructor (teloxide uses `ThreadId(MessageId(i32))` — verify)
- `SessionUpdate` variant fields (check actual `agent-client-protocol` API)
- `PermissionOption` field names (`.label` vs `.description`, `.id` type)
- `ForumTopic` return type from `create_forum_topic` (field name for thread id)
- `InlineKeyboardButton::callback` signature (may take `&str` or `String`)
- `teloxide::types::ParseMode` import path

For each error: read the actual type definition (from docs.rs or cargo doc), fix the code, re-check.

- [ ] **Step 3: Run cargo check until clean**

Run: `cd /Volumes/Projects/spur && cargo check -p spur-cli 2>&1 | tail -5`
Expected: `Finished` with no errors

- [ ] **Step 4: Run all tests**

Run: `cd /Volumes/Projects/spur && cargo test -p spur-telegram 2>&1 | tail -15`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add -A crates/spur-telegram/
git commit -m "fix(spur-telegram): resolve compilation issues against actual API types"
```

---

## Summary

| Task | Deliverable | Tests |
|---|---|---|
| 1 | Crate skeleton + TelegramConfig | 3 config tests |
| 2 | BotState (auth, mappings) | 4 state tests |
| 3 | Message formatter (pagination, escaping) | 6 formatter tests |
| 4 | Command handlers (/start, /sessions, etc.) | compile check |
| 5 | Message handler (brain chat input) | compile check |
| 6 | Callback handler (reviews, permissions) | compile check |
| 7 | Event renderer (SpurEvent → Telegram) | compile check |
| 8 | Bot dispatcher (teloxide routing) | compile check |
| 9 | Entry point + CLI integration | compile check |
| 10 | Integration smoke test | full build + test suite |
| 11 | Fix compilation issues | clean build |

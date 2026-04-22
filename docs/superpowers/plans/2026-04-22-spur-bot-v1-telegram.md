# SPUR Bot Frontend v1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `spur-bot` as a single-operator Telegram DM frontend over the existing SPUR interactive runtime, with sticky current-session routing, restart restore, review prompts, and permission prompts.

**Architecture:** First extract the current `spur watch` interactive bootstrap into a small shared `spur-interactive` host crate so the TUI and bot share one correctness path for event subscription, review dispatch, permission flow, continuation wiring, and shutdown. Then add `spur-bot` with a transport-neutral runtime state machine plus a `frankenstein`-based Telegram adapter, and finally wire a new `spur bot telegram` CLI entry point and config validation.

**Tech Stack:** Rust 2021, `tokio`, `frankenstein` (`client-reqwest`), `spur-core`, `spur-acp`, `spur-license`, `serde`, `toml`, `cargo test`

---

## File Map

### New crates

| File | Responsibility |
|---|---|
| `crates/spur-interactive/Cargo.toml` | Shared host crate dependencies |
| `crates/spur-interactive/src/lib.rs` | Public re-exports for the shared interactive host |
| `crates/spur-interactive/src/host.rs` | `InteractiveFrontendHost`, typed handle, review routing, permission/event stream ownership, shutdown |
| `crates/spur-interactive/tests/host_api.rs` | Host API tests for routing and one-shot stream ownership |
| `crates/spur-bot/Cargo.toml` | Bot crate dependencies |
| `crates/spur-bot/src/lib.rs` | Public exports for runtime and Telegram transport |
| `crates/spur-bot/src/commands.rs` | Bot command parsing for `/new`, `/resume`, `/current`, `/cancel`, and plain text |
| `crates/spur-bot/src/state.rs` | Persisted bot state under `.spur/bot/state.json` and binding-state helpers |
| `crates/spur-bot/src/runtime.rs` | `BotRuntime`, prompt registry, restore flow, event handling, and transport-facing render intents |
| `crates/spur-bot/src/telegram/mod.rs` | Telegram transport entrypoint |
| `crates/spur-bot/src/telegram/config.rs` | Telegram config extraction and validation |
| `crates/spur-bot/src/telegram/client.rs` | Thin `frankenstein` client wrapper, including low-level fallback for `sendMessageDraft` |
| `crates/spur-bot/src/telegram/poll_loop.rs` | Long-poll `getUpdates` loop, webhook cleanup, offset management, retry/backoff |
| `crates/spur-bot/src/telegram/router.rs` | Telegram `Update` normalization into bot runtime inputs |
| `crates/spur-bot/src/telegram/sender.rs` | Outbound request serialization, throttling, draft coalescing |
| `crates/spur-bot/src/telegram/render.rs` | Mapping runtime render intents into Telegram messages, edits, keyboards, and callback acknowledgements |
| `crates/spur-bot/src/telegram/format.rs` | Telegram-safe message splitting and button-label helpers |
| `crates/spur-bot/tests/telegram_config.rs` | Config parsing and validation tests |
| `crates/spur-bot/tests/bot_commands.rs` | Command parser tests |
| `crates/spur-bot/tests/state_store.rs` | Persistent state round-trip tests |
| `crates/spur-bot/tests/runtime_flow.rs` | Session routing, restore, prompt lifecycle, and stale callback tests |
| `crates/spur-bot/tests/telegram_router.rs` | Telegram update filtering and callback parsing tests |
| `crates/spur-bot/tests/telegram_format.rs` | Unicode-safe splitting and label shortening tests |
| `crates/spur-bot/tests/telegram_sender.rs` | Sender throttling and draft coalescing tests |
| `crates/spur-bot/tests/telegram_poll_loop.rs` | Poll-loop offset advancement and retry/backoff tests |
| `crates/spur-cli/tests/bot_cli.rs` | CLI help and config-driven bot command smoke tests |

### Existing files to modify

| File | Responsibility |
|---|---|
| `Cargo.toml` | Add `spur-interactive` and `spur-bot` to workspace members and dependencies |
| `crates/spur-acp/src/config/mod.rs` | Add `[bot.telegram]` config model to `SpurConfig` |
| `crates/spur-cli/src/main.rs` | Add `spur bot telegram`, factor shared interactive bootstrap usage, keep `watch` on the same host |
| `crates/spur-cli/src/commands/config_check.rs` | Validate `[bot.telegram]` settings when enabled |
| `crates/spur-cli/tests/config_check.rs` | Cover valid and invalid bot config cases |

## Implementation Notes

- Keep `spur-tui::UserInput` TUI-local. The shared host API must not depend on `spur-tui`.
- Use `spur_core::InteractiveInput` as the command vocabulary, but keep review submission on a separate typed path.
- Persist only transport-neutral bot state: schema version, bound DM `chat_id`, current ACP session id, current brain.
- Track the live SPUR `SessionId` in memory only so `/cancel` can target the active runtime session without polluting persistent state.
- Treat all review and permission prompt tokens as process-local and memory-only.
- Keep `frankenstein` types inside `crates/spur-bot/src/telegram/`.

---

### Task 1: Create the shared interactive host crate

**Files:**
- Create: `crates/spur-interactive/Cargo.toml`
- Create: `crates/spur-interactive/src/lib.rs`
- Create: `crates/spur-interactive/src/host.rs`
- Test: `crates/spur-interactive/tests/host_api.rs`
- Modify: `Cargo.toml`

- [ ] **Step 1: Write the failing host API tests**

```rust
// crates/spur-interactive/tests/host_api.rs
use spur_core::InteractiveInput;
use spur_interactive::{validate_frontend_command, ReviewSubmission};

#[test]
fn reject_submit_review_on_command_lane() {
    let err = validate_frontend_command(&InteractiveInput::SubmitReview {
        executor_id: "exec-42".into(),
        attempt_n: 2,
        decision: spur_acp::ReviewDecision::Approve,
    })
    .unwrap_err();

    assert!(err.to_string().contains("send_review"));
}

#[test]
fn review_submission_converts_to_submit_review() {
    let input = ReviewSubmission {
        executor_id: "exec-42".into(),
        attempt_n: 2,
        decision: spur_acp::ReviewDecision::Retry {
            new_constraints: String::new(),
        },
    }
    .into_input();

    assert!(matches!(
        input,
        InteractiveInput::SubmitReview {
            executor_id,
            attempt_n: 2,
            decision: spur_acp::ReviewDecision::Retry { new_constraints },
        } if executor_id == "exec-42" && new_constraints.is_empty()
    ));
}
```

- [ ] **Step 2: Run the focused test target and verify it fails**

Run:

```bash
cargo test -p spur-interactive --test host_api -- --nocapture
```

Expected: FAIL because the crate does not exist yet.

- [ ] **Step 3: Add the new crate to the workspace**

Create `crates/spur-interactive/Cargo.toml`:

```toml
[package]
name = "spur-interactive"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
anyhow = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
spur-acp = { workspace = true }
spur-core = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "time"] }
```

Update the workspace root `Cargo.toml`:

```toml
members = [
    "crates/spur-acp",
    "crates/spur-mcp",
    "crates/spur-core",
    "crates/spur-pm",
    "crates/spur-worktree",
    "crates/spur-cost",
    "crates/spur-license",
    "crates/spur-tui",
    "crates/spur-interactive",
    "crates/spur-cli",
]

[workspace.dependencies]
spur-interactive = { path = "crates/spur-interactive" }
```

- [ ] **Step 4: Implement the initial shared host types**

Create `crates/spur-interactive/src/lib.rs`:

```rust
pub mod host;

pub use host::{
    validate_frontend_command, InteractiveFrontendHandle, InteractiveFrontendHost,
    ReviewSubmission,
};
```

Create `crates/spur-interactive/src/host.rs`:

```rust
use anyhow::{bail, Result};
use spur_core::InteractiveInput;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSubmission {
    pub executor_id: String,
    pub attempt_n: u32,
    pub decision: spur_acp::ReviewDecision,
}

impl ReviewSubmission {
    pub fn into_input(self) -> InteractiveInput {
        InteractiveInput::SubmitReview {
            executor_id: self.executor_id,
            attempt_n: self.attempt_n,
            decision: self.decision,
        }
    }
}

pub fn validate_frontend_command(input: &InteractiveInput) -> Result<()> {
    if matches!(input, InteractiveInput::SubmitReview { .. }) {
        bail!("SubmitReview must be routed through send_review");
    }
    Ok(())
}

#[derive(Clone)]
pub struct InteractiveFrontendHandle {
    pub(crate) user_tx: tokio::sync::mpsc::Sender<InteractiveInput>,
    pub(crate) review_tx: tokio::sync::mpsc::Sender<InteractiveInput>,
}

pub struct InteractiveFrontendHost {
    pub(crate) handle: InteractiveFrontendHandle,
    pub(crate) event_rx: Option<tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>>,
    pub(crate) permission_rx:
        Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>>,
    pub(crate) orch_handle: tokio::task::JoinHandle<()>,
}
```

- [ ] **Step 5: Run the tests and verify they pass**

Run:

```bash
cargo test -p spur-interactive --test host_api -- --nocapture
```

Expected: PASS with 2 tests passing.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates/spur-interactive
git commit -m "feat(spur-interactive): B1 add shared host crate skeleton"
```

---

### Task 2: Move the shared interactive bootstrap into `spur-interactive`

**Files:**
- Modify: `crates/spur-interactive/src/host.rs`
- Modify: `crates/spur-cli/src/main.rs`
- Test: `crates/spur-interactive/tests/host_api.rs`

- [ ] **Step 1: Extend the host test with routing and one-shot stream ownership**

Add to `crates/spur-interactive/tests/host_api.rs`:

```rust
#[tokio::test]
async fn send_review_uses_the_review_lane() {
    let (user_tx, mut user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, mut review_rx) = tokio::sync::mpsc::channel(1);
    let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();

    let host = spur_interactive::InteractiveFrontendHost::from_parts_for_test(
        user_tx,
        review_tx,
        event_rx,
        perm_rx,
        tokio::spawn(async {}),
    );
    let handle = host.handle();

    handle
        .send_review(ReviewSubmission {
            executor_id: "exec-7".into(),
            attempt_n: 3,
            decision: spur_acp::ReviewDecision::Retry {
                new_constraints: String::new(),
            },
        })
        .await
        .unwrap();

    assert!(user_rx.try_recv().is_err());
    let input = review_rx.recv().await.unwrap();
    assert!(matches!(
        input,
        InteractiveInput::SubmitReview {
            executor_id,
            attempt_n: 3,
            decision: spur_acp::ReviewDecision::Retry { new_constraints },
        } if executor_id == "exec-7" && new_constraints.is_empty()
    ));
}

#[test]
fn host_streams_can_only_be_taken_once() {
    let (user_tx, _user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, _review_rx) = tokio::sync::mpsc::channel(1);
    let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();

    let mut host = spur_interactive::InteractiveFrontendHost::from_parts_for_test(
        user_tx,
        review_tx,
        event_rx,
        perm_rx,
        tokio::spawn(async {}),
    );

    assert!(host.take_event_stream().is_some());
    assert!(host.take_event_stream().is_none());
    assert!(host.take_permission_stream().is_some());
    assert!(host.take_permission_stream().is_none());
}
```

- [ ] **Step 2: Run the test target and verify it fails**

Run:

```bash
cargo test -p spur-interactive --test host_api -- --nocapture
```

Expected: FAIL because `from_parts_for_test`, `handle`, `send_review`, and `take_*_stream` do not exist yet.

- [ ] **Step 3: Implement the handle API, stream accessors, and orchestrator spawn**

Update `crates/spur-interactive/src/host.rs`:

```rust
impl InteractiveFrontendHandle {
    pub async fn send_command(&self, input: InteractiveInput) -> anyhow::Result<()> {
        validate_frontend_command(&input)?;
        self.user_tx.send(input).await?;
        Ok(())
    }

    pub async fn send_review(&self, review: ReviewSubmission) -> anyhow::Result<()> {
        self.review_tx.send(review.into_input()).await?;
        Ok(())
    }
}

impl InteractiveFrontendHost {
    pub fn from_parts_for_test(
        user_tx: tokio::sync::mpsc::Sender<InteractiveInput>,
        review_tx: tokio::sync::mpsc::Sender<InteractiveInput>,
        event_rx: tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>,
        permission_rx: tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>,
        orch_handle: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self {
            handle: InteractiveFrontendHandle { user_tx, review_tx },
            event_rx: Some(event_rx),
            permission_rx: Some(permission_rx),
            orch_handle,
        }
    }

    pub fn handle(&self) -> InteractiveFrontendHandle {
        self.handle.clone()
    }

    pub fn take_event_stream(
        &mut self,
    ) -> Option<tokio::sync::broadcast::Receiver<spur_acp::SpurEvent>> {
        self.event_rx.take()
    }

    pub fn take_permission_stream(
        &mut self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<spur_acp::types::PermissionRequest>> {
        self.permission_rx.take()
    }

    pub fn spawn(mut orch: spur_core::Orchestrator, brain: Option<String>) -> Self {
        let event_rx = orch.subscribe();
        let review_sink = orch.review_sink.clone();
        let (permission_tx, permission_rx) =
            tokio::sync::mpsc::unbounded_channel::<spur_acp::types::PermissionRequest>();
        let (user_tx, user_rx) = tokio::sync::mpsc::channel::<InteractiveInput>(32);
        let (review_tx, review_rx) = tokio::sync::mpsc::channel::<InteractiveInput>(32);

        tokio::spawn(spur_core::review_dispatcher_loop(review_rx, review_sink));

        let overflow = spur_core::continuation_bridge::new_overflow_buf();
        orch.set_continuation_tx(user_tx.clone(), overflow.clone());

        let orch_handle = tokio::spawn(async move {
            if let Err(error) = orch
                .run_interactive(user_rx, brain, Some(permission_tx), overflow)
                .await
            {
                tracing::error!(%error, "interactive host run_interactive failed");
            }
        });

        Self {
            handle: InteractiveFrontendHandle { user_tx, review_tx },
            event_rx: Some(event_rx),
            permission_rx: Some(permission_rx),
            orch_handle,
        }
    }

    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        self.event_rx.take();
        self.permission_rx.take();
        drop(self.handle);
        let handle = self.orch_handle;
        match tokio::time::timeout(std::time::Duration::from_secs(5), handle).await {
            Ok(_) => Ok(()),
            Err(_) => anyhow::bail!("interactive host shutdown timed out"),
        }
    }
}
```

- [ ] **Step 4: Replace the inline `watch` bootstrap in `spur-cli` with the shared host**

Update the `Commands::Watch` arm in `crates/spur-cli/src/main.rs` so the inline channel setup is removed and replaced by `InteractiveFrontendHost::spawn`:

```rust
let mut host = spur_interactive::InteractiveFrontendHost::spawn(orch, brain.clone());
let host_handle = host.handle();
let event_rx = host.take_event_stream().expect("event stream");
let perm_rx = host.take_permission_stream();

let (tui_tx, mut tui_rx) = tokio::sync::mpsc::channel::<spur_tui::UserInput>(32);
tokio::spawn(async move {
    while let Some(input) = tui_rx.recv().await {
        match input {
            spur_tui::UserInput::SubmitReview {
                executor_id,
                attempt_n,
                decision,
            } => {
                let _ = host_handle
                    .send_review(spur_interactive::ReviewSubmission {
                        executor_id,
                        attempt_n,
                        decision,
                    })
                    .await;
            }
            other => {
                let _ = host_handle.send_command(tui_input_to_interactive(other)).await;
            }
        }
    }
});
```

Add a local helper near `cmd_watch`:

```rust
fn tui_input_to_interactive(input: spur_tui::UserInput) -> spur_core::InteractiveInput {
    match input {
        spur_tui::UserInput::Message {
            blocks, interrupt, ..
        } => spur_core::InteractiveInput::Message { blocks, interrupt },
        spur_tui::UserInput::NewSessionWithMessage { blocks, interrupt } => {
            spur_core::InteractiveInput::NewSessionWithMessage { blocks, interrupt }
        }
        spur_tui::UserInput::ListSessions => spur_core::InteractiveInput::ListSessions,
        spur_tui::UserInput::ResumeSession { session_id } => {
            spur_core::InteractiveInput::ResumeSession { session_id }
        }
        spur_tui::UserInput::SetSessionMode { mode_id } => {
            spur_core::InteractiveInput::SetSessionMode { mode_id }
        }
        spur_tui::UserInput::VendorExec {
            session,
            method,
            params,
        } => spur_core::InteractiveInput::VendorExec {
            session,
            method,
            params,
        },
        spur_tui::UserInput::CancelStream { session } => {
            spur_core::InteractiveInput::CancelStream { session }
        }
        spur_tui::UserInput::RefreshIssues => spur_core::InteractiveInput::RefreshIssues,
        spur_tui::UserInput::GetIssueDetail { id } => {
            spur_core::InteractiveInput::GetIssueDetail { id }
        }
        spur_tui::UserInput::UpdateIssue { id, update } => {
            spur_core::InteractiveInput::UpdateIssue { id, update }
        }
        spur_tui::UserInput::SubmitReview { .. } => {
            unreachable!("review routing is handled before translation")
        }
    }
}
```

- [ ] **Step 5: Run focused tests and a compile-smoke CLI test**

Run:

```bash
cargo test -p spur-interactive --test host_api -- --nocapture
cargo test -p spur-cli --test config_check -- --nocapture
```

Expected: PASS. The second command is the cheap compile-smoke that ensures `spur-cli` still builds after the watch-path refactor.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-interactive crates/spur-cli/src/main.rs
git commit -m "refactor(spur-interactive): B2 extract watch bootstrap host"
```

---

### Task 3: Add `spur-bot` and the Telegram config model

**Files:**
- Create: `crates/spur-bot/Cargo.toml`
- Create: `crates/spur-bot/src/lib.rs`
- Create: `crates/spur-bot/src/commands.rs`
- Create: `crates/spur-bot/src/state.rs`
- Create: `crates/spur-bot/src/runtime.rs`
- Create: `crates/spur-bot/src/telegram/mod.rs`
- Create: `crates/spur-bot/src/telegram/config.rs`
- Test: `crates/spur-bot/tests/telegram_config.rs`
- Modify: `Cargo.toml`
- Modify: `crates/spur-acp/src/config/mod.rs`
- Modify: `crates/spur-cli/src/commands/config_check.rs`
- Modify: `crates/spur-cli/tests/config_check.rs`

- [ ] **Step 1: Write the failing config tests**

Create `crates/spur-bot/tests/telegram_config.rs`:

```rust
use spur_acp::config::SpurConfig;

#[test]
fn parse_single_operator_telegram_config() {
    let cfg: SpurConfig = toml::from_str(
        r#"
[bot.telegram]
enabled = true
bot_token = "123:ABC"
operator_user_id = 424242
poll_timeout_secs = 30
draft_streaming = true
"#,
    )
    .unwrap();

    assert!(cfg.bot.telegram.enabled);
    assert_eq!(cfg.bot.telegram.operator_user_id, Some(424242));
    assert_eq!(cfg.bot.telegram.poll_timeout_secs, 30);
    assert!(cfg.bot.telegram.draft_streaming);
}

#[test]
fn enabled_bot_requires_token_and_operator() {
    let cfg: SpurConfig = toml::from_str(
        r#"
[bot.telegram]
enabled = true
"#,
    )
    .unwrap();

    let err = spur_bot::telegram::config::validate(&cfg.bot.telegram).unwrap_err();
    assert!(err.to_string().contains("bot_token"));
}
```

Add to `crates/spur-cli/tests/config_check.rs`:

```rust
#[test]
fn config_check_fails_when_bot_enabled_without_operator_user() {
    let dir = write_config(
        r#"
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"

[agents.entries.commands]
dispatch = "prompt_text"

[bot.telegram]
enabled = true
bot_token = "123:ABC"
"#,
    );

    let out = Command::new(spur_binary())
        .current_dir(dir.path())
        .args(["config", "check"])
        .output()
        .expect("spawn");

    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("operator_user_id"));
}
```

- [ ] **Step 2: Run the focused tests and verify failure**

Run:

```bash
cargo test -p spur-bot --test telegram_config -- --nocapture
cargo test -p spur-cli --test config_check -- --nocapture
```

Expected: FAIL because `spur-bot` and `[bot.telegram]` do not exist yet.

- [ ] **Step 3: Add the new crate to the workspace**

Create `crates/spur-bot/Cargo.toml`:

```toml
[package]
name = "spur-bot"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
tokio-util = { workspace = true }
tracing = { workspace = true }
uuid = { workspace = true }
toml = { workspace = true }
reqwest = { workspace = true }
agent-client-protocol = { workspace = true }
frankenstein = { version = "0.49", features = ["client-reqwest"] }
spur-acp = { workspace = true }
spur-core = { workspace = true }
spur-interactive = { workspace = true }

[dev-dependencies]
tempfile = "3"
```

Update root `Cargo.toml`:

```toml
members = [
    "crates/spur-acp",
    "crates/spur-mcp",
    "crates/spur-core",
    "crates/spur-pm",
    "crates/spur-worktree",
    "crates/spur-cost",
    "crates/spur-license",
    "crates/spur-tui",
    "crates/spur-interactive",
    "crates/spur-bot",
    "crates/spur-cli",
]

[workspace.dependencies]
spur-bot = { path = "crates/spur-bot" }
```

- [ ] **Step 4: Extend `SpurConfig` and add Telegram config validation**

Update `crates/spur-acp/src/config/mod.rs`:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BotConfig {
    pub telegram: TelegramBotConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TelegramBotConfig {
    pub enabled: bool,
    pub bot_token: Option<String>,
    pub operator_user_id: Option<i64>,
    pub poll_timeout_secs: u64,
    pub draft_streaming: bool,
    pub max_requests_per_second: u32,
}

impl Default for TelegramBotConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: None,
            operator_user_id: None,
            poll_timeout_secs: 30,
            draft_streaming: false,
            max_requests_per_second: 20,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpurConfig {
    #[serde(default)]
    pub brain: BrainConfig,
    #[serde(default)]
    pub agents: AgentsConfig,
    #[serde(default)]
    pub failover: FailoverConfig,
    #[serde(default)]
    pub worktree: WorktreeConfig,
    #[serde(default)]
    pub cost: CostConfig,
    #[serde(default)]
    pub pm: PmConfig,
    #[serde(default)]
    pub bot: BotConfig,
    #[serde(default)]
    pub project: Option<ProjectConfig>,
    #[serde(default)]
    pub delegation: DelegationConfig,
    #[serde(default)]
    pub spur: SpurRuntimeConfig,
}
```

Create `crates/spur-bot/src/lib.rs`:

```rust
pub mod commands;
pub mod runtime;
pub mod state;
pub mod telegram;
```

Create `crates/spur-bot/src/telegram/config.rs`:

```rust
pub fn validate(cfg: &spur_acp::config::TelegramBotConfig) -> anyhow::Result<()> {
    if !cfg.enabled {
        return Ok(());
    }

    anyhow::ensure!(
        cfg.bot_token.as_deref().is_some_and(|s| !s.trim().is_empty()),
        "bot.telegram.bot_token is required when bot.telegram.enabled = true"
    );
    anyhow::ensure!(
        cfg.operator_user_id.is_some(),
        "bot.telegram.operator_user_id is required when bot.telegram.enabled = true"
    );
    anyhow::ensure!(
        cfg.poll_timeout_secs > 0,
        "bot.telegram.poll_timeout_secs must be greater than 0"
    );
    anyhow::ensure!(
        cfg.max_requests_per_second > 0,
        "bot.telegram.max_requests_per_second must be greater than 0"
    );
    Ok(())
}
```

- [ ] **Step 5: Hook bot validation into `spur config check`**

Update `crates/spur-cli/src/commands/config_check.rs`:

```rust
if let Err(error) = spur_bot::telegram::config::validate(&cfg.bot.telegram) {
    eprintln!("\u{2717} {error}");
    fatal_count += 1;
}
```

- [ ] **Step 6: Run the tests and verify they pass**

Run:

```bash
cargo test -p spur-bot --test telegram_config -- --nocapture
cargo test -p spur-cli --test config_check -- --nocapture
```

Expected: PASS. The new config check test should fail before this step and pass after it.

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/spur-acp/src/config/mod.rs crates/spur-bot crates/spur-cli/src/commands/config_check.rs crates/spur-cli/tests/config_check.rs
git commit -m "feat(spur-bot): B3 add bot crate and telegram config"
```

---

### Task 4: Implement command parsing and persistent bot state

**Files:**
- Modify: `crates/spur-bot/src/commands.rs`
- Modify: `crates/spur-bot/src/state.rs`
- Test: `crates/spur-bot/tests/bot_commands.rs`
- Test: `crates/spur-bot/tests/state_store.rs`

- [ ] **Step 1: Write the failing parser and state-store tests**

Create `crates/spur-bot/tests/bot_commands.rs`:

```rust
use spur_bot::commands::{parse_chat_input, BotCommand, ParsedChatInput};

#[test]
fn parse_resume_command() {
    assert_eq!(
        parse_chat_input("/resume acp_123"),
        ParsedChatInput::Command(BotCommand::Resume {
            session_id: "acp_123".into(),
        })
    );
}

#[test]
fn plain_text_stays_plain_text() {
    assert_eq!(
        parse_chat_input("investigate review loop"),
        ParsedChatInput::PlainText("investigate review loop".into())
    );
}
```

Create `crates/spur-bot/tests/state_store.rs`:

```rust
use spur_bot::state::{BotStateStore, PersistedBotState};

#[test]
fn persisted_state_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let store = BotStateStore::new(path.clone());

    let expected = PersistedBotState {
        version: 1,
        operator_chat_id: Some(10_001),
        current_acp_session_id: Some("acp_77".into()),
        current_brain: Some("claude-code".into()),
    };

    store.save(&expected).unwrap();
    let loaded = store.load().unwrap();

    assert_eq!(loaded, expected);
}

#[test]
fn missing_state_file_defaults_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join("missing.json"));

    let loaded = store.load().unwrap();
    assert_eq!(loaded.current_acp_session_id, None);
    assert_eq!(loaded.current_brain, None);
}
```

- [ ] **Step 2: Run the focused tests and verify failure**

Run:

```bash
cargo test -p spur-bot --test bot_commands -- --nocapture
cargo test -p spur-bot --test state_store -- --nocapture
```

Expected: FAIL because the parser and state store are not implemented yet.

- [ ] **Step 3: Implement command parsing**

Update `crates/spur-bot/src/commands.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotCommand {
    Start,
    Help,
    New,
    Sessions,
    Resume { session_id: String },
    Current,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedChatInput {
    Command(BotCommand),
    PlainText(String),
}

pub fn parse_chat_input(raw: &str) -> ParsedChatInput {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("/resume ") {
        return ParsedChatInput::Command(BotCommand::Resume {
            session_id: rest.trim().to_string(),
        });
    }

    match trimmed {
        "/start" => ParsedChatInput::Command(BotCommand::Start),
        "/help" => ParsedChatInput::Command(BotCommand::Help),
        "/new" => ParsedChatInput::Command(BotCommand::New),
        "/sessions" => ParsedChatInput::Command(BotCommand::Sessions),
        "/current" => ParsedChatInput::Command(BotCommand::Current),
        "/cancel" => ParsedChatInput::Command(BotCommand::Cancel),
        _ => ParsedChatInput::PlainText(trimmed.to_string()),
    }
}
```

- [ ] **Step 4: Implement the persisted bot state store**

Update `crates/spur-bot/src/state.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistedBotState {
    pub version: u32,
    pub operator_chat_id: Option<i64>,
    pub current_acp_session_id: Option<String>,
    pub current_brain: Option<String>,
}

impl Default for PersistedBotState {
    fn default() -> Self {
        Self {
            version: 1,
            operator_chat_id: None,
            current_acp_session_id: None,
            current_brain: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingState {
    NoSession,
    RestorePending {
        acp_session_id: String,
        brain: String,
    },
    Active {
        session: spur_acp::SessionId,
        acp_session_id: String,
        brain: String,
    },
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
        Ok(serde_json::from_str(&raw)?)
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

- [ ] **Step 5: Run the tests and verify they pass**

Run:

```bash
cargo test -p spur-bot --test bot_commands -- --nocapture
cargo test -p spur-bot --test state_store -- --nocapture
```

Expected: PASS with 4 tests passing.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-bot/src/commands.rs crates/spur-bot/src/state.rs crates/spur-bot/tests/bot_commands.rs crates/spur-bot/tests/state_store.rs
git commit -m "feat(spur-bot): B4 add command parser and state store"
```

---

### Task 5: Implement the shared bot runtime

**Files:**
- Modify: `crates/spur-bot/src/runtime.rs`
- Test: `crates/spur-bot/tests/runtime_flow.rs`

- [ ] **Step 1: Write the failing runtime flow tests**

Create `crates/spur-bot/tests/runtime_flow.rs`:

```rust
use agent_client_protocol::{
    PermissionOption, PermissionOptionId, PermissionOptionKind, RequestPermissionRequest,
    ToolCallUpdate, ToolCallUpdateFields,
};
use spur_bot::runtime::{BotRuntime, RuntimeRender};
use spur_bot::state::BotStateStore;
use spur_interactive::InteractiveFrontendHost;

fn mk_permission_request(
) -> (
    spur_acp::types::PermissionRequest,
    tokio::sync::oneshot::Receiver<spur_acp::types::PermissionResponse>,
) {
    let tool_call = ToolCallUpdate::new("tool-1", ToolCallUpdateFields::new());
    let args = RequestPermissionRequest::new(
        "session-1",
        tool_call,
        vec![
            PermissionOption::new(
                PermissionOptionId::new("allow_once"),
                "Allow Once",
                PermissionOptionKind::AllowOnce,
            ),
            PermissionOption::new(
                PermissionOptionId::new("deny"),
                "Deny",
                PermissionOptionKind::RejectOnce,
            ),
        ],
    );
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    (
        spur_acp::types::PermissionRequest { args, reply_tx },
        reply_rx,
    )
}

#[tokio::test]
async fn first_plain_message_starts_new_session() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let (user_tx, mut user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, _review_rx) = tokio::sync::mpsc::channel(1);
    let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let host = InteractiveFrontendHost::from_parts_for_test(
        user_tx,
        review_tx,
        event_rx,
        perm_rx,
        tokio::spawn(async {}),
    );
    let handle = host.handle();
    let mut runtime = BotRuntime::new(store);

    let renders = runtime
        .handle_chat_text(&handle, 10_001, "Investigate review loop")
        .await
        .unwrap();

    assert!(matches!(
        user_rx.recv().await.unwrap(),
        spur_core::InteractiveInput::NewSessionWithMessage { .. }
    ));
    assert!(renders.iter().any(|item| matches!(
        item,
        RuntimeRender::WorkingStatus { .. }
    )));
}

#[tokio::test]
async fn agent_session_ready_commits_binding_and_persists() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let mut runtime = BotRuntime::new(store);

    runtime
        .handle_spur_event(spur_acp::SpurEvent::now(
            spur_acp::SpurEventBody::AgentSessionReady {
                session: spur_acp::SessionId("spur_1".into()),
                acp_session_id: "acp_1".into(),
                brain: "claude-code".into(),
                resumed: false,
                cancel_mode: spur_acp::CancelMode::AcpSoft,
            },
        ))
        .unwrap();

    let persisted = runtime.state_store().load().unwrap();
    assert_eq!(persisted.current_acp_session_id.as_deref(), Some("acp_1"));
    assert_eq!(persisted.current_brain.as_deref(), Some("claude-code"));
}

#[tokio::test]
async fn stale_callback_is_reported_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let (user_tx, _user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, _review_rx) = tokio::sync::mpsc::channel(1);
    let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let host = InteractiveFrontendHost::from_parts_for_test(
        user_tx,
        review_tx,
        event_rx,
        perm_rx,
        tokio::spawn(async {}),
    );
    let handle = host.handle();
    let mut runtime = BotRuntime::new(store);

    let renders = runtime
        .handle_callback(&handle, "cbq-stale", "deadbeef")
        .await
        .unwrap();
    assert!(renders.iter().any(|item| matches!(
        item,
        RuntimeRender::AnswerCallback {
            text,
            ..
        } if text.contains("expired")
    )));
}

#[tokio::test]
async fn permission_callback_returns_exact_option_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = BotStateStore::new(dir.path().join(".spur/bot/state.json"));
    let (user_tx, _user_rx) = tokio::sync::mpsc::channel(1);
    let (review_tx, _review_rx) = tokio::sync::mpsc::channel(1);
    let (_event_tx, event_rx) = tokio::sync::broadcast::channel(4);
    let (_perm_tx, perm_rx) = tokio::sync::mpsc::unbounded_channel();
    let host = InteractiveFrontendHost::from_parts_for_test(
        user_tx,
        review_tx,
        event_rx,
        perm_rx,
        tokio::spawn(async {}),
    );
    let handle = host.handle();
    let mut runtime = BotRuntime::new(store);
    let (request, reply_rx) = mk_permission_request();

    let renders = runtime.handle_permission_request(request).unwrap();
    let token = renders
        .iter()
        .find_map(|item| match item {
            RuntimeRender::PermissionPrompt { buttons, .. } => {
                Some(buttons[0].token.clone())
            }
            _ => None,
        })
        .unwrap();

    runtime
        .handle_callback(&handle, "cbq-perm", &token)
        .await
        .unwrap();

    let response = reply_rx.await.unwrap();
    assert_eq!(response.option_id, "allow_once");
}
```

- [ ] **Step 2: Run the focused runtime test and verify failure**

Run:

```bash
cargo test -p spur-bot --test runtime_flow -- --nocapture
```

Expected: FAIL because `BotRuntime`, `RuntimeRender`, and the callback/prompt logic do not exist yet.

- [ ] **Step 3: Implement the runtime state machine and render intents**

Update `crates/spur-bot/src/runtime.rs`:

```rust
use std::collections::HashMap;

use spur_bot::commands::{parse_chat_input, BotCommand, ParsedChatInput};
use spur_bot::state::{BindingState, BotStateStore, PersistedBotState};

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

pub struct BotRuntime {
    state_store: BotStateStore,
    binding: BindingState,
    persisted: PersistedBotState,
    prompts: HashMap<String, PendingPrompt>,
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
}
```

- [ ] **Step 4: Finish event handling, prompt registration, and stale callback behavior**

Extend `crates/spur-bot/src/runtime.rs`:

```rust
impl BotRuntime {
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
                    .map(|s| s.id.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            }]),
            spur_acp::SpurEventBody::ExecutorReviewRequested {
                id,
                attempt_n,
                payload,
                ..
            } => {
                let mut buttons = Vec::new();
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
                    buttons.push(PromptButton {
                        token,
                        label: label.into(),
                    });
                }
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
        for opt in &request.args.options {
            let token = uuid::Uuid::new_v4().simple().to_string();
            self.prompts.insert(
                token.clone(),
                PendingPrompt::Permission {
                    prompt_id: prompt_id.clone(),
                    option_id: opt.option_id.to_string(),
                },
            );
            buttons.push(PromptButton {
                token,
                label: opt.name.to_string(),
            });
        }
        Ok(vec![RuntimeRender::PermissionPrompt {
            text: format!("Permission required for `{}`", request.args.tool_call.id),
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
```

- [ ] **Step 5: Run the runtime tests and verify they pass**

Run:

```bash
cargo test -p spur-bot --test runtime_flow -- --nocapture
```

Expected: PASS with all runtime-flow tests passing.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-bot/src/runtime.rs crates/spur-bot/tests/runtime_flow.rs
git commit -m "feat(spur-bot): B5 implement runtime state machine"
```

---

### Task 6: Implement Telegram formatting and outbound sending

**Files:**
- Modify: `crates/spur-bot/src/telegram/format.rs`
- Modify: `crates/spur-bot/src/telegram/sender.rs`
- Test: `crates/spur-bot/tests/telegram_format.rs`
- Test: `crates/spur-bot/tests/telegram_sender.rs`

- [ ] **Step 1: Write the failing formatting and sender tests**

Create `crates/spur-bot/tests/telegram_format.rs`:

```rust
use spur_bot::telegram::format::{short_button_label, split_for_telegram};

#[test]
fn split_for_telegram_preserves_unicode_scalar_boundaries() {
    let text = "alpha🙂beta🙂gamma".repeat(400);
    let chunks = split_for_telegram(&text, 256);

    assert!(chunks.iter().all(|chunk| chunk.chars().count() <= 256));
    assert_eq!(chunks.concat(), text);
}

#[test]
fn short_button_label_keeps_action_verb() {
    assert_eq!(
        short_button_label("Allow Once", 12),
        "Allow Once"
    );
    assert_eq!(
        short_button_label("Allow Always for This Tool", 16),
        "Allow Always"
    );
}
```

Create `crates/spur-bot/tests/telegram_sender.rs`:

```rust
use spur_bot::telegram::sender::{DraftUpdate, TelegramSender};

#[tokio::test(start_paused = true)]
async fn sender_coalesces_draft_updates_by_draft_id() {
    let (sender, mut rx) = TelegramSender::for_test(20);

    sender
        .queue_draft(DraftUpdate {
            chat_id: 10_001,
            draft_id: "draft-1".into(),
            text: "alpha".into(),
        })
        .await;
    sender
        .queue_draft(DraftUpdate {
            chat_id: 10_001,
            draft_id: "draft-1".into(),
            text: "alpha beta".into(),
        })
        .await;

    tokio::time::advance(std::time::Duration::from_millis(500)).await;

    let sent = rx.recv().await.unwrap();
    assert_eq!(sent.text, "alpha beta");
}
```

- [ ] **Step 2: Run the focused test targets and verify failure**

Run:

```bash
cargo test -p spur-bot --test telegram_format -- --nocapture
cargo test -p spur-bot --test telegram_sender -- --nocapture
```

Expected: FAIL because the formatter and sender helpers do not exist yet.

- [ ] **Step 3: Implement Telegram-safe formatting**

Update `crates/spur-bot/src/telegram/format.rs`:

```rust
pub fn split_for_telegram(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if current.chars().count() == max_chars {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    if chunks.is_empty() {
        vec![String::new()]
    } else {
        chunks
    }
}

pub fn short_button_label(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_string();
    }

    let first_word = label.split_whitespace().next().unwrap_or(label);
    label
        .split_whitespace()
        .scan(String::new(), |acc, part| {
            let candidate = if acc.is_empty() {
                part.to_string()
            } else {
                format!("{acc} {part}")
            };
            if candidate.chars().count() <= max_chars || acc.as_str() == first_word {
                *acc = candidate.clone();
                Some(acc.clone())
            } else {
                None
            }
        })
        .last()
        .unwrap_or_else(|| first_word.to_string())
}
```

- [ ] **Step 4: Implement a throttled Telegram sender**

Update `crates/spur-bot/src/telegram/sender.rs`:

```rust
use std::collections::HashMap;
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftUpdate {
    pub chat_id: i64,
    pub draft_id: String,
    pub text: String,
}

pub struct TelegramSender {
    tx: mpsc::Sender<DraftUpdate>,
}

impl TelegramSender {
    pub fn new(
        client: crate::telegram::client::TelegramClient,
        rate_per_second: u32,
    ) -> Self {
        let (tx, rx) = mpsc::channel(rate_per_second as usize);
        tokio::spawn(async move {
            Self::run_draft_loop(rx, std::time::Duration::from_millis(400), move |update| {
                let client = client.clone();
                tokio::spawn(async move {
                    let _ = client
                        .send_message_draft(update.chat_id, &update.draft_id, &update.text)
                        .await;
                });
            })
            .await;
        });
        Self { tx }
    }

    pub fn for_test(rate_per_second: u32) -> (Self, mpsc::Receiver<DraftUpdate>) {
        let (tx, rx) = mpsc::channel(rate_per_second as usize);
        (Self { tx }, rx)
    }

    pub async fn queue_draft(&self, update: DraftUpdate) {
        let _ = self.tx.send(update).await;
    }

    pub async fn run_draft_loop(
        mut rx: mpsc::Receiver<DraftUpdate>,
        flush_every: std::time::Duration,
        mut flush: impl FnMut(DraftUpdate) + Send + 'static,
    ) {
        let mut pending: HashMap<String, DraftUpdate> = HashMap::new();
        let mut ticker = tokio::time::interval(flush_every);

        loop {
            tokio::select! {
                maybe = rx.recv() => match maybe {
                    Some(update) => { pending.insert(update.draft_id.clone(), update); }
                    None => break,
                },
                _ = ticker.tick() => {
                    for (_id, update) in pending.drain() {
                        flush(update);
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 5: Run the tests and verify they pass**

Run:

```bash
cargo test -p spur-bot --test telegram_format -- --nocapture
cargo test -p spur-bot --test telegram_sender -- --nocapture
```

Expected: PASS with the formatter and sender tests green.

- [ ] **Step 6: Commit**

```bash
git add crates/spur-bot/src/telegram/format.rs crates/spur-bot/src/telegram/sender.rs crates/spur-bot/tests/telegram_format.rs crates/spur-bot/tests/telegram_sender.rs
git commit -m "feat(spur-bot): B6 add telegram formatting and sender"
```

---

### Task 7: Implement the Telegram transport and CLI entry point

**Files:**
- Modify: `crates/spur-bot/src/telegram/mod.rs`
- Modify: `crates/spur-bot/src/telegram/client.rs`
- Modify: `crates/spur-bot/src/telegram/poll_loop.rs`
- Modify: `crates/spur-bot/src/telegram/router.rs`
- Modify: `crates/spur-bot/src/telegram/render.rs`
- Modify: `crates/spur-cli/src/main.rs`
- Test: `crates/spur-bot/tests/telegram_router.rs`
- Test: `crates/spur-bot/tests/telegram_poll_loop.rs`
- Test: `crates/spur-cli/tests/bot_cli.rs`

- [ ] **Step 1: Write the failing transport and CLI smoke tests**

Create `crates/spur-bot/tests/telegram_router.rs`:

```rust
use spur_bot::telegram::router::{normalize_update, TelegramInput};

#[test]
fn router_rejects_non_private_updates() {
    let update = frankenstein::Update {
        content: frankenstein::UpdateContent::Message(frankenstein::Message {
            chat: frankenstein::Chat {
                id: 99,
                r#type: frankenstein::ChatType::Supergroup,
                ..Default::default()
            },
            text: Some("hello".into()),
            ..Default::default()
        }),
        ..Default::default()
    };

    assert!(normalize_update(&update, 424242).is_none());
}

#[test]
fn router_maps_private_command_text() {
    let update = frankenstein::Update {
        content: frankenstein::UpdateContent::Message(frankenstein::Message {
            chat: frankenstein::Chat {
                id: 10_001,
                r#type: frankenstein::ChatType::Private,
                ..Default::default()
            },
            from: Some(frankenstein::User {
                id: 424242,
                is_bot: false,
                first_name: "Kevin".into(),
                ..Default::default()
            }),
            text: Some("/current".into()),
            ..Default::default()
        }),
        ..Default::default()
    };

    assert!(matches!(
        normalize_update(&update, 424242),
        Some(TelegramInput::Text { chat_id: 10_001, text, .. }) if text == "/current"
    ));
}
```

Create `crates/spur-bot/tests/telegram_poll_loop.rs`:

```rust
use spur_bot::telegram::poll_loop::advance_offset;

#[test]
fn offset_advances_only_after_accepted_batch() {
    assert_eq!(advance_offset(100, &[101, 102], true), 103);
    assert_eq!(advance_offset(100, &[101, 102], false), 100);
}
```

Create `crates/spur-cli/tests/bot_cli.rs`:

```rust
use std::process::Command;

fn spur_binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_spur"))
}

#[test]
fn bot_telegram_help_smoke() {
    let out = Command::new(spur_binary())
        .args(["bot", "telegram", "--help"])
        .output()
        .expect("spawn");

    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("Launch the Telegram bot frontend"));
}
```

- [ ] **Step 2: Run the focused tests and verify failure**

Run:

```bash
cargo test -p spur-bot --test telegram_router -- --nocapture
cargo test -p spur-bot --test telegram_poll_loop -- --nocapture
cargo test -p spur-cli --test bot_cli -- --nocapture
```

Expected: FAIL because the Telegram transport and CLI command do not exist yet.

- [ ] **Step 3: Implement the `frankenstein` client wrapper and poll loop**

Update `crates/spur-bot/src/telegram/client.rs`:

```rust
#[derive(Clone)]
pub struct TelegramClient {
    inner: frankenstein::AsyncApi<frankenstein::client_reqwest::Bot>,
}

impl TelegramClient {
    pub fn new(token: &str) -> Self {
        Self {
            inner: frankenstein::AsyncApi::new(frankenstein::client_reqwest::Bot::new(
                token.to_string(),
            )),
        }
    }

    pub async fn delete_webhook(&self) -> anyhow::Result<()> {
        self.inner.delete_webhook(&frankenstein::DeleteWebhookParams::default()).await?;
        Ok(())
    }

    pub async fn get_updates(
        &self,
        offset: i64,
        timeout_secs: u64,
    ) -> anyhow::Result<Vec<frankenstein::Update>> {
        let params = frankenstein::GetUpdatesParams::builder()
            .offset(offset)
            .timeout(timeout_secs as i64)
            .build();
        let response = self.inner.get_updates(&params).await?;
        Ok(response.result)
    }

    pub async fn send_message_draft(
        &self,
        chat_id: i64,
        draft_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        let payload = serde_json::json!({
            "chat_id": chat_id,
            "draft_id": draft_id,
            "text": text,
        });
        self.inner.request("sendMessageDraft", payload).await?;
        Ok(())
    }

    pub async fn send_text(&self, chat_id: i64, text: String) -> anyhow::Result<()> {
        self.inner
            .send_message(
                &frankenstein::SendMessageParams::builder()
                    .chat_id(chat_id)
                    .text(text)
                    .build(),
            )
            .await?;
        Ok(())
    }

    pub async fn answer_callback(&self, query_id: String, text: String) -> anyhow::Result<()> {
        self.inner
            .answer_callback_query(
                &frankenstein::AnswerCallbackQueryParams::builder()
                    .callback_query_id(query_id)
                    .text(text)
                    .build(),
            )
            .await?;
        Ok(())
    }

    pub async fn send_buttons(
        &self,
        chat_id: i64,
        text: String,
        buttons: &[crate::runtime::PromptButton],
    ) -> anyhow::Result<()> {
        let row = buttons
            .iter()
            .map(|button| {
                frankenstein::InlineKeyboardButton::builder()
                    .text(button.label.clone())
                    .callback_data(button.token.clone())
                    .build()
            })
            .collect::<Vec<_>>();
        let markup = frankenstein::InlineKeyboardMarkup::builder()
            .inline_keyboard(vec![row])
            .build();
        self.inner
            .send_message(
                &frankenstein::SendMessageParams::builder()
                    .chat_id(chat_id)
                    .text(text)
                    .reply_markup(markup)
                    .build(),
            )
            .await?;
        Ok(())
    }
}
```

Update `crates/spur-bot/src/telegram/poll_loop.rs`:

```rust
pub fn advance_offset(current: i64, update_ids: &[i64], accepted: bool) -> i64 {
    if !accepted {
        return current;
    }
    update_ids.iter().copied().max().map(|id| id + 1).unwrap_or(current)
}

pub async fn run_poll_loop(
    client: &crate::telegram::client::TelegramClient,
    timeout_secs: u64,
    cancellation: tokio_util::sync::CancellationToken,
    mut on_batch: impl FnMut(Vec<frankenstein::Update>) -> anyhow::Result<()> + Send,
) -> anyhow::Result<()> {
    client.delete_webhook().await?;
    let mut offset = 0_i64;
    let mut backoff = std::time::Duration::from_millis(250);

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            result = client.get_updates(offset, timeout_secs) => {
                match result {
                    Ok(batch) => {
                        let ids = batch.iter().map(|u| u.update_id).collect::<Vec<_>>();
                        let accepted = on_batch(batch).is_ok();
                        offset = advance_offset(offset, &ids, accepted);
                        backoff = std::time::Duration::from_millis(250);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "telegram poll failed");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(std::time::Duration::from_secs(5));
                    }
                }
            }
        }
    }
}
```

- [ ] **Step 4: Implement routing, rendering, and the transport entrypoint**

Update `crates/spur-bot/src/telegram/router.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TelegramInput {
    Text { user_id: i64, chat_id: i64, text: String },
    Callback { user_id: i64, chat_id: i64, query_id: String, token: String },
}

pub fn normalize_update(
    update: &frankenstein::Update,
    operator_user_id: i64,
) -> Option<TelegramInput> {
    match &update.content {
        frankenstein::UpdateContent::Message(message)
            if message.chat.r#type == frankenstein::ChatType::Private =>
        {
            let user = message.from.as_ref()?;
            if user.id != operator_user_id {
                return None;
            }
            Some(TelegramInput::Text {
                user_id: user.id,
                chat_id: message.chat.id,
                text: message.text.clone()?,
            })
        }
        frankenstein::UpdateContent::CallbackQuery(query) => {
            let user = &query.from;
            if user.id != operator_user_id {
                return None;
            }
            Some(TelegramInput::Callback {
                user_id: user.id,
                chat_id: query.message.as_ref()?.chat.id,
                query_id: query.id.clone(),
                token: query.data.clone()?,
            })
        }
        _ => None,
    }
}
```

Update `crates/spur-bot/src/telegram/render.rs` with a narrow renderer:

```rust
pub async fn render_batch(
    client: &crate::telegram::client::TelegramClient,
    sender: &crate::telegram::sender::TelegramSender,
    chat_id: i64,
    renders: Vec<crate::runtime::RuntimeRender>,
) -> anyhow::Result<()> {
    for render in renders {
        match render {
            crate::runtime::RuntimeRender::ServiceMessage { text }
            | crate::runtime::RuntimeRender::FinalAnswer { text } => {
                client.send_text(chat_id, text).await?;
            }
            crate::runtime::RuntimeRender::WorkingStatus { text } => {
                sender
                    .queue_draft(crate::telegram::sender::DraftUpdate {
                        chat_id,
                        draft_id: format!("working-{chat_id}"),
                        text,
                    })
                    .await;
            }
            crate::runtime::RuntimeRender::AnswerCallback { query_id, text } => {
                client.answer_callback(query_id, text).await?;
            }
            crate::runtime::RuntimeRender::ReviewPrompt { text, buttons }
            | crate::runtime::RuntimeRender::PermissionPrompt { text, buttons } => {
                client.send_buttons(chat_id, text, &buttons).await?;
            }
            crate::runtime::RuntimeRender::FinalizePrompt { .. } => {}
        }
    }
    Ok(())
}
```

Update `crates/spur-bot/src/telegram/mod.rs`:

```rust
pub async fn run_telegram_bot(
    cfg: &spur_acp::config::TelegramBotConfig,
    mut host: spur_interactive::InteractiveFrontendHost,
    state_path: std::path::PathBuf,
) -> anyhow::Result<()> {
    let operator_user_id = cfg.operator_user_id.expect("validated");
    let handle = host.handle();
    let mut event_rx = host.take_event_stream().expect("event stream");
    let mut perm_rx = host.take_permission_stream().expect("permission stream");
    let (update_tx, mut update_rx) = tokio::sync::mpsc::channel(64);
    let mut runtime = crate::runtime::BotRuntime::new(crate::state::BotStateStore::new(state_path));
    let client = client::TelegramClient::new(cfg.bot_token.as_deref().expect("validated"));
    let sender = crate::telegram::sender::TelegramSender::new(
        client.clone(),
        cfg.max_requests_per_second,
    );
    let cancellation = tokio_util::sync::CancellationToken::new();

    let poll_cancellation = cancellation.clone();
    tokio::spawn(async move {
        let _ = poll_loop::run_poll_loop(&client, cfg.poll_timeout_secs, poll_cancellation, |batch| {
            for update in batch {
                if let Some(input) = router::normalize_update(&update, operator_user_id) {
                    let _ = update_tx.blocking_send(input);
                }
            }
            Ok(())
        })
        .await;
    });

    loop {
        tokio::select! {
            maybe_update = update_rx.recv() => {
                let Some(input) = maybe_update else { break; };
                let renders = match input {
                    router::TelegramInput::Text { chat_id, text, .. } => {
                        runtime.handle_chat_text(&handle, chat_id, &text).await?
                    }
                    router::TelegramInput::Callback { query_id, token, .. } => {
                        runtime.handle_callback(&handle, &query_id, &token).await?
                    }
                };
                if let Some(chat_id) = runtime.bound_chat_id() {
                    render::render_batch(&client, &sender, chat_id, renders).await?;
                }
            }
            Ok(event) = event_rx.recv() => {
                let renders = runtime.handle_spur_event(event)?;
                if let Some(chat_id) = runtime.bound_chat_id() {
                    render::render_batch(&client, &sender, chat_id, renders).await?;
                }
            }
            Some(request) = perm_rx.recv() => {
                let renders = runtime.handle_permission_request(request)?;
                if let Some(chat_id) = runtime.bound_chat_id() {
                    render::render_batch(&client, &sender, chat_id, renders).await?;
                }
            }
        }
    }

    host.shutdown().await
}
```

- [ ] **Step 5: Add the CLI command and shared startup helper**

Update `crates/spur-cli/src/main.rs`:

```rust
#[derive(Subcommand)]
enum Commands {
    // ...
    Bot {
        #[command(subcommand)]
        command: BotCommands,
    },
    Watch {
        #[arg(long)]
        brain: Option<String>,
        #[arg(long)]
        sessions: bool,
        #[arg(long)]
        dashboard: bool,
    },
}

#[derive(Subcommand)]
enum BotCommands {
    /// Launch the Telegram bot frontend.
    Telegram {
        #[arg(long)]
        brain: Option<String>,
    },
}
```

Add a shared helper near `load_orchestrator`:

```rust
async fn build_interactive_host(
    repo_root: PathBuf,
    config: SpurConfig,
    brain: Option<String>,
) -> Result<spur_interactive::InteractiveFrontendHost> {
    let license = SpurLicense::from_env_or_disabled();
    let pm_service = if license
        .feature_gate()
        .has(spur_license::FeatureKey::PM_INTEGRATION)
    {
        spur_pm::PmService::try_new(
            config.pm.github.as_ref().and_then(|g| g.repo.clone()),
            config.pm.beads.as_ref().is_none_or(|b| b.enabled),
            config.pm.github.as_ref().is_none_or(|g| g.enabled),
            &repo_root,
            None,
        )
        .await
        .unwrap_or(None)
    } else {
        None
    };

    let orch = Orchestrator::new(repo_root, config, Some(license.feature_gate()))?;
    let mut orch = if let Some(pm) = pm_service.map(std::sync::Arc::new) {
        orch.with_pm_service(pm)
    } else {
        orch
    };
    let _license_runtime = orch.spawn_license_runtime(license);
    Ok(spur_interactive::InteractiveFrontendHost::spawn(orch, brain))
}
```

Handle the new command in `main()`:

```rust
Commands::Bot {
    command: BotCommands::Telegram { brain },
} => {
    let config = load_config()?;
    spur_bot::telegram::config::validate(&config.bot.telegram)?;
    let host = build_interactive_host(repo_root.clone(), config.clone(), brain).await?;
    spur_bot::telegram::run_telegram_bot(
        &config.bot.telegram,
        host,
        repo_root.join(".spur").join("bot").join("state.json"),
    )
    .await
}
```

- [ ] **Step 6: Run focused transport tests and full workspace verification**

Run:

```bash
cargo test -p spur-bot --test telegram_router -- --nocapture
cargo test -p spur-bot --test telegram_poll_loop -- --nocapture
cargo test -p spur-cli --test bot_cli -- --nocapture
cargo test -p spur-bot --tests
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

Expected: PASS. If `cargo clippy` flags dead code in the new transport modules, trim the surface before merging instead of adding blanket `allow` attributes.

- [ ] **Step 7: Manual smoke before final merge**

Run this checklist with a real Telegram bot token and one private DM:

```text
1. Start `spur bot telegram` with no `.spur/bot/state.json`.
2. Send a first DM and confirm a new SPUR session starts.
3. Send a second DM and confirm the same session is reused.
4. Run `/sessions` and confirm the response is compact and includes `/resume <id>` guidance.
5. Run `/resume <id>` for another ACP session and confirm the current binding switches.
6. Restart the process and confirm the prior session auto-restores.
7. Trigger an executor review and click Approve, Reject, and Retry in separate attempts.
8. Trigger a permission request and confirm the exact ACP `option_id` is returned.
9. Click a pre-restart button and confirm the callback is answered with an expired message.
10. If `draft_streaming = true`, confirm in-progress assistant text uses one draft and the final answer lands as a durable message.
```

- [ ] **Step 8: Commit**

```bash
git add crates/spur-bot/src/telegram crates/spur-cli/src/main.rs crates/spur-bot/tests/telegram_router.rs crates/spur-bot/tests/telegram_poll_loop.rs crates/spur-cli/tests/bot_cli.rs
git commit -m "feat(spur-bot): B7 wire telegram transport and cli"
```

---

## Spec Coverage Check

- Shared interactive bootstrap reuse: covered by Task 1 and Task 2.
- `spur-bot` crate shape: covered by Task 3 through Task 7.
- Telegram transport via `frankenstein`: covered by Task 6 and Task 7.
- Single sticky current session, `/new`, `/resume`, `/current`, `/cancel`: covered by Task 4 and Task 5.
- Restart restore and `.spur/bot/state.json`: covered by Task 4 and Task 5.
- Review and permission prompt lifecycle with stale callback behavior: covered by Task 5 and Task 7.
- Poll loop, webhook cleanup, offset advancement, and throttled outbound sending: covered by Task 6 and Task 7.
- CLI and config validation: covered by Task 3 and Task 7.

## Placeholder Scan

- No `TODO`, `TBD`, or “similar to Task N” placeholders remain.
- Every task names exact files and test commands.
- Every code-edit step includes concrete code, not a prose placeholder.

## Type Consistency Check

- Shared host types stay consistent across tasks: `InteractiveFrontendHost`, `InteractiveFrontendHandle`, `ReviewSubmission`.
- Bot state/runtime names stay consistent across tasks: `PersistedBotState`, `BindingState`, `BotStateStore`, `BotRuntime`, `RuntimeRender`.
- Telegram transport names stay consistent across tasks: `TelegramClient`, `TelegramSender`, `TelegramInput`, `run_telegram_bot`.

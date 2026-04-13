# Chat Input: Slash Commands and `@` Mentions — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build autocomplete-driven slash-command and `@`-mention support for the chat input in `SessionDetailView`, per the design spec at `docs/superpowers/specs/2026-04-13-chat-input-commands-mentions-design.md`.

**Architecture:** A new `CompletionPopup` component overlays the existing `InputBar` in `SessionDetailView`. A trait-based `CommandRegistry` merges spur-local, standard ACP, and kiro-vendor command sources; a trait-based `MentionRegistry` merges file and directory sources backed by the `ignore` crate. A submit-time router parses the InputBar text + sorted `ProtectedRange`s into a `Vec<ContentBlock>` (text + `ResourceLink`). The `Action::SendMessage` variant and orchestrator send-path are rewired to carry `Vec<ContentBlock>`.

**Tech stack:** Rust 1.88, ratatui 0.29, crossterm 0.28, `agent_client_protocol` 0.10 (re-exported via `spur-acp`), new deps `nucleo-matcher 0.3` + `ignore 0.4`.

---

## File map

Files to **create**:

- `crates/spur-tui/src/components/completion_popup.rs` — overlay list widget
- `crates/spur-tui/src/components/completion_trigger.rs` — detects `/` / `@` prefix + query extraction
- `crates/spur-tui/src/commands/mod.rs` — command registry module root
- `crates/spur-tui/src/commands/entry.rs` — `CommandEntry` / `CommandSource` / `Dispatch` types
- `crates/spur-tui/src/commands/registry.rs` — `CommandRegistry` merging sources
- `crates/spur-tui/src/commands/spur_local.rs` — static spur-local source
- `crates/spur-tui/src/commands/submit_router.rs` — InputBar text + ranges → `Vec<ContentBlock>` + `Dispatch`
- `crates/spur-tui/src/mentions/mod.rs` — mention registry module root
- `crates/spur-tui/src/mentions/entry.rs` — `MentionEntry` / `MentionKind` / `MentionSource` trait
- `crates/spur-tui/src/mentions/registry.rs` — `MentionRegistry` + per-session cache
- `crates/spur-tui/src/mentions/file_source.rs` — file + directory walker using `ignore`
- `crates/spur-tui/tests/input_bar_protected_ranges.rs` — unit tests for atoms
- `crates/spur-tui/tests/command_registry.rs` — unit tests for merger
- `crates/spur-tui/tests/submit_router.rs` — unit tests for routing
- `crates/spur-tui/tests/completion_trigger.rs` — unit tests for trigger detection
- `crates/spur-tui/tests/mention_registry.rs` — unit tests for walker cache + fuzzy
- `crates/spur-tui/tests/session_detail_commands_integration.rs` — end-to-end

Files to **modify**:

- `crates/spur-tui/Cargo.toml` — add `nucleo-matcher`, `ignore` deps
- `crates/spur-tui/src/lib.rs` — declare new modules
- `crates/spur-tui/src/components/mod.rs` — register `completion_popup`, `completion_trigger`
- `crates/spur-tui/src/components/input_bar.rs` — add `ProtectedRange`, `insert_atom`, atom-aware editing
- `crates/spur-tui/src/action.rs` — `Action::SendMessage.text: String` → `blocks: Vec<ContentBlock>`
- `crates/spur-tui/src/app.rs` — update `SendMessage` arm, `UserInput::Message` plumbing, `apply_session_update` to keep full `Vec<AvailableCommand>`
- `crates/spur-tui/src/views/session_detail.rs` — store registries + popup, rewire `handle_key`, update submit path
- `crates/spur-tui/src/views/dashboard.rs` — update the one `Action::SendMessage` call site (line 307)
- `crates/spur-acp/src/lib.rs` — re-export `ResourceLink`, `AvailableCommand`, `AvailableCommandInput`, `UnstructuredCommandInput`, `ExtRequest`, `ExtResponse`, `ExtNotification`
- `crates/spur-acp/src/domain/events.rs` (or wherever `SpurEventBody` lives) — add `AgentExtNotification { session, method, params }` variant (decision resolved in Task 6.1); alternatively route via a new method on the ACP connection adapter
- `crates/spur-acp/src/connection/` (relevant adapter) — hook `ext_notification` inbound; expose `ext_method` outbound
- `crates/spur-core/src/orchestrator.rs` — replace `vec![ContentBlock::Text(TextContent::new(text))]` at orchestrator.rs:553 (and the 256, 731, 1539 call sites if they should also accept blocks) with the `blocks` vec carried on `UserInput::Message`
- `crates/spur-cli/src/main.rs:381` — update destructuring of `UserInput::Message`

---

## Conventions

- **Branch:** implement on the current branch; commit after every task.
- **Test runner:** `cargo test -p spur-tui` for TUI, `cargo test -p spur-acp` for ACP, `cargo test` for full workspace.
- **Lints:** run `cargo clippy --workspace --all-targets -- -D warnings` before each commit. Fix warnings; do not `#[allow]` them.
- **Commits:** one per task. Message format: `feat(spur-tui): Task N — <short summary>` (or `refactor`, `test`, `fix` as appropriate).

---

## Task 1: Replace `available_commands: Vec<String>` with full `Vec<AvailableCommand>`

Preserve description + hint for the popup by keeping the full ACP type.

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs:35` (field declaration) + line 58 (init) + line 322 (read site through `apply_session_update`)
- Modify: `crates/spur-tui/src/app.rs:772-777` (writer)
- Modify: `crates/spur-acp/src/lib.rs:32` (re-export `AvailableCommand`, `AvailableCommandInput`, `UnstructuredCommandInput`)
- Test: `crates/spur-tui/tests/session_update_handling.rs` (extend existing file)

- [ ] **Step 1: re-export the AvailableCommand types from spur-acp.** In `crates/spur-acp/src/lib.rs`, extend the `pub use agent_client_protocol::{ … }` list.

```rust
pub use agent_client_protocol::{
    ContentBlock, ContentChunk, TextContent,
    SessionNotification, SessionUpdate,
    ToolCall as AcpToolCall, ToolCallUpdate as AcpToolCallUpdate,
    ToolCallStatus, ToolKind, ToolCallContent, ToolCallLocation,
    Plan, PlanEntry, PlanEntryStatus, PlanEntryPriority,
    RequestPermissionRequest, PermissionOption, PermissionOptionId,
    PermissionOptionKind, RequestPermissionOutcome, SelectedPermissionOutcome,
    SessionInfo, ListSessionsRequest, ListSessionsResponse, LoadSessionRequest,
    AuthenticateRequest, AuthenticateResponse, AuthMethodId,
    AvailableCommandsUpdate, AvailableCommand, AvailableCommandInput,
    UnstructuredCommandInput,
    CurrentModeUpdate, SessionModeId,
    SetSessionModeRequest, SetSessionModeResponse, UsageUpdate,
};
```

- [ ] **Step 2: write a failing test that asserts hints survive.**

Append to `crates/spur-tui/tests/session_update_handling.rs`:

```rust
#[test]
fn available_commands_update_preserves_hint() {
    use spur_acp::{
        AvailableCommand, AvailableCommandInput, AvailableCommandsUpdate,
        SessionUpdate, UnstructuredCommandInput,
    };
    use spur_tui::views::session_detail::SessionDetailView;

    let mut view = SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".to_string(),
        "brain".to_string(),
    );

    let cmd = AvailableCommand {
        name: "compact".into(),
        description: "compact history".into(),
        input: Some(AvailableCommandInput::Unstructured(
            UnstructuredCommandInput::new("[threshold]"),
        )),
        meta: None,
    };
    let update = SessionUpdate::AvailableCommandsUpdate(AvailableCommandsUpdate {
        available_commands: vec![cmd],
        meta: None,
    });

    spur_tui::app::apply_session_update(&mut view, &update);

    let got = view.available_commands();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].name, "compact");
    assert_eq!(got[0].description, "compact history");
    match got[0].input.as_ref() {
        Some(AvailableCommandInput::Unstructured(u)) => {
            assert_eq!(u.hint, "[threshold]");
        }
        other => panic!("expected Unstructured hint, got {:?}", other),
    }
}
```

- [ ] **Step 3: run the test.**

```
cargo test -p spur-tui --test session_update_handling available_commands_update_preserves_hint
```

Expected: FAIL — `available_commands` returns `Vec<String>` today, so `got[0].name` will not compile.

- [ ] **Step 4: change the field type and accessor.**

In `crates/spur-tui/src/views/session_detail.rs`:

```rust
// at line 35 — replace
pub available_commands: Vec<spur_acp::AvailableCommand>,
// at line 58 (init) — replace
available_commands: Vec::new(),
```

Add a public accessor near the other accessors (after `trace_entry_count`):

```rust
pub fn available_commands(&self) -> &[spur_acp::AvailableCommand] {
    &self.available_commands
}
```

In `crates/spur-tui/src/app.rs` (around line 772-777):

```rust
AvailableCommandsUpdate(u) => {
    state.available_commands = u.available_commands.clone();
}
```

Also confirm the `apply_session_update` function is still `pub(crate)` — promote to `pub` if the test above requires it (the skill prefers crate-public; if promoting, also re-export via `crates/spur-tui/src/lib.rs`).

- [ ] **Step 5: run the test.**

```
cargo test -p spur-tui --test session_update_handling available_commands_update_preserves_hint
```

Expected: PASS.

- [ ] **Step 6: run the full spur-tui suite + clippy.**

```
cargo test -p spur-tui
cargo clippy -p spur-tui --all-targets -- -D warnings
```

Expected: all tests pass, no clippy warnings.

- [ ] **Step 7: commit.**

```
git add crates/spur-acp/src/lib.rs crates/spur-tui/src/views/session_detail.rs \
       crates/spur-tui/src/app.rs crates/spur-tui/src/lib.rs \
       crates/spur-tui/tests/session_update_handling.rs
git commit -m "refactor(spur-tui): store full AvailableCommand to preserve hint"
```

---

## Task 2: Define `CommandEntry`, `CommandSource`, `Dispatch` types

Create the command domain types used by the registry, popup, and router. No behavior yet.

**Files:**
- Create: `crates/spur-tui/src/commands/mod.rs`
- Create: `crates/spur-tui/src/commands/entry.rs`
- Modify: `crates/spur-tui/src/lib.rs` (declare `pub mod commands;`)
- Test: `crates/spur-tui/tests/command_registry.rs`

- [ ] **Step 1: write failing test.**

Create `crates/spur-tui/tests/command_registry.rs`:

```rust
use spur_tui::commands::{CommandEntry, CommandSource, Dispatch};

#[test]
fn command_entry_constructs() {
    let e = CommandEntry {
        name: "help".into(),
        description: "Show spur keybindings".into(),
        hint: None,
        source: CommandSource::Spur,
        dispatch: Dispatch::SpurLocal(spur_tui::action::Action::ShowHelp),
    };
    assert_eq!(e.name, "help");
    assert!(matches!(e.source, CommandSource::Spur));
}

#[test]
fn command_source_agent_carries_handle() {
    let s = CommandSource::Agent { handle: "claude".into() };
    match s {
        CommandSource::Agent { handle } => assert_eq!(handle, "claude"),
        _ => panic!("expected Agent"),
    }
}
```

- [ ] **Step 2: run the test.**

```
cargo test -p spur-tui --test command_registry
```

Expected: FAIL (module does not exist).

- [ ] **Step 3: create the module skeleton.**

`crates/spur-tui/src/commands/mod.rs`:

```rust
pub mod entry;
pub use entry::{CommandEntry, CommandSource, Dispatch};
```

`crates/spur-tui/src/commands/entry.rs`:

```rust
use crate::action::Action;
use serde_json::Value;

/// An entry displayed in the slash-command popup.
#[derive(Debug, Clone)]
pub struct CommandEntry {
    /// Command name without the leading slash (e.g. "help", "compact").
    pub name: String,
    /// Human-readable description shown beside the name.
    pub description: String,
    /// Optional input placeholder from ACP `UnstructuredCommandInput.hint`.
    pub hint: Option<String>,
    /// Where this command came from.
    pub source: CommandSource,
    /// How to execute it on accept/submit.
    pub dispatch: Dispatch,
}

/// Where a `CommandEntry` originates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandSource {
    /// A spur-local command handled by the TUI.
    Spur,
    /// A command advertised by an ACP agent (or its vendor extension).
    /// `handle` is the lowercase agent identifier used for namespacing
    /// (e.g. "claude", "kiro").
    Agent { handle: String },
}

/// How a selected `CommandEntry` should be executed.
#[derive(Debug, Clone)]
pub enum Dispatch {
    /// Fire an `Action` directly, close the popup, do not send a message.
    SpurLocal(Action),
    /// Send the normalized text as a `ContentBlock::Text` to the current agent.
    /// `normalized` is the bare form with leading slash (e.g. "/help").
    PromptText { normalized: String },
    /// Invoke the kiro vendor extension `_kiro.dev/commands/execute`.
    KiroExecute {
        command: String,
        args: Value,
    },
}
```

In `crates/spur-tui/src/lib.rs`, add:

```rust
pub mod commands;
```

(near the other `pub mod` declarations — inspect existing `lib.rs` for style.)

- [ ] **Step 4: run the test.**

```
cargo test -p spur-tui --test command_registry
```

Expected: PASS.

- [ ] **Step 5: clippy + commit.**

```
cargo clippy -p spur-tui --all-targets -- -D warnings
git add crates/spur-tui/src/commands/ crates/spur-tui/src/lib.rs crates/spur-tui/tests/command_registry.rs
git commit -m "feat(spur-tui): add CommandEntry/CommandSource/Dispatch types"
```

---

## Task 3: `SpurLocalSource` static catalog

Define the four v1 spur-local commands (`/help`, `/mode`, `/cost`, `/quit`).

**Files:**
- Create: `crates/spur-tui/src/commands/spur_local.rs`
- Modify: `crates/spur-tui/src/commands/mod.rs`
- Test: `crates/spur-tui/tests/command_registry.rs`

- [ ] **Step 1: failing test.**

Append to `crates/spur-tui/tests/command_registry.rs`:

```rust
#[test]
fn spur_local_source_exposes_v1_set() {
    let entries = spur_tui::commands::SpurLocalSource::entries();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"help"), "missing /help: {:?}", names);
    assert!(names.contains(&"mode"), "missing /mode: {:?}", names);
    assert!(names.contains(&"cost"), "missing /cost: {:?}", names);
    assert!(names.contains(&"quit"), "missing /quit: {:?}", names);

    for e in &entries {
        assert!(matches!(e.source, spur_tui::commands::CommandSource::Spur));
        assert!(matches!(e.dispatch, spur_tui::commands::Dispatch::SpurLocal(_)));
    }
}
```

- [ ] **Step 2: run.** `cargo test -p spur-tui --test command_registry spur_local_source_exposes_v1_set` — FAIL.

- [ ] **Step 3: implement.**

`crates/spur-tui/src/commands/spur_local.rs`:

```rust
use crate::action::Action;
use super::entry::{CommandEntry, CommandSource, Dispatch};

/// Static registry of spur-local slash commands available in every session.
pub struct SpurLocalSource;

impl SpurLocalSource {
    pub fn entries() -> Vec<CommandEntry> {
        vec![
            CommandEntry {
                name: "help".into(),
                description: "Show spur keybindings".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::ShowHelp),
            },
            CommandEntry {
                name: "mode".into(),
                description: "Toggle Claude session mode (plan / default)".into(),
                hint: Some("[plan|default]".into()),
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::TogglePlanMode),
            },
            CommandEntry {
                name: "cost".into(),
                description: "Show current session cost".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::ShowSessionCost),
            },
            CommandEntry {
                name: "quit".into(),
                description: "Quit spur".into(),
                hint: None,
                source: CommandSource::Spur,
                dispatch: Dispatch::SpurLocal(Action::Quit),
            },
        ]
    }
}
```

Because `Action::ShowSessionCost` doesn't exist yet, add it now in `crates/spur-tui/src/action.rs` (keep this atomic with Task 3):

```rust
// Add to the Action enum (alongside ShowHelp/HideHelp):
/// Push a trace entry showing the current session cost.
ShowSessionCost,
```

Then handle it in `app.rs` by matching the arm and pushing a trace entry on the active session_detail. The minimal handler:

```rust
// inside App::process_action match, after the HideHelp arm:
Action::ShowSessionCost => {
    if let Some(ref mut detail) = self.session_detail {
        detail.push_cost_note();
    }
}
```

Add `push_cost_note` on `SessionDetailView`:

```rust
pub fn push_cost_note(&mut self) {
    use crate::components::react_trace::{TraceEntry, TraceKind};
    let msg = format!("Session cost: ${:.2}", self.cost);
    self.react_trace.push(TraceEntry {
        kind: TraceKind::Think,
        text: msg,
        timestamp: Self::now_stamp(),
    });
}
```

Update `crates/spur-tui/src/commands/mod.rs`:

```rust
pub mod entry;
pub mod spur_local;

pub use entry::{CommandEntry, CommandSource, Dispatch};
pub use spur_local::SpurLocalSource;
```

- [ ] **Step 4: run.** `cargo test -p spur-tui --test command_registry` — PASS.

- [ ] **Step 5: full suite + clippy.** `cargo test -p spur-tui && cargo clippy -p spur-tui --all-targets -- -D warnings` — PASS.

- [ ] **Step 6: commit.**

```
git add crates/spur-tui/src/commands/ crates/spur-tui/src/action.rs \
       crates/spur-tui/src/app.rs crates/spur-tui/src/views/session_detail.rs \
       crates/spur-tui/tests/command_registry.rs
git commit -m "feat(spur-tui): SpurLocalSource with /help /mode /cost /quit"
```

---

## Task 4: `CommandRegistry` that merges sources with prefix-on-collision

The registry merges spur-local + agent-advertised commands. Bare names when unique; `/<source>:<name>` canonical form on collision. No fuzzy matching yet (that's Task 8); registry just exposes the merged list and a lookup.

**Files:**
- Create: `crates/spur-tui/src/commands/registry.rs`
- Modify: `crates/spur-tui/src/commands/mod.rs`
- Test: `crates/spur-tui/tests/command_registry.rs`

- [ ] **Step 1: failing test.**

Append to `crates/spur-tui/tests/command_registry.rs`:

```rust
use spur_acp::{AvailableCommand, AvailableCommandInput, UnstructuredCommandInput};
use spur_tui::commands::{CommandRegistry, CommandSource};

fn acp_cmd(name: &str, desc: &str, hint: Option<&str>) -> AvailableCommand {
    AvailableCommand {
        name: name.into(),
        description: desc.into(),
        input: hint.map(|h| AvailableCommandInput::Unstructured(
            UnstructuredCommandInput::new(h),
        )),
        meta: None,
    }
}

#[test]
fn registry_merges_spur_local_and_agent() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands("claude", vec![acp_cmd("compact", "compact", None)]);
    let entries = reg.list();

    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"help"), "spur /help missing: {:?}", names);
    assert!(names.contains(&"compact"), "agent /compact missing: {:?}", names);
}

#[test]
fn registry_marks_collisions_with_source_prefix() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands("claude", vec![acp_cmd("help", "claude help", None)]);

    // /help exists in both spur-local and claude.
    let entries = reg.list();
    let helps: Vec<_> = entries.iter().filter(|e| e.name == "help").collect();
    assert_eq!(helps.len(), 2);

    // canonical_typed_form returns prefixed form on collision:
    let spur_help = helps.iter().find(|e| e.source == CommandSource::Spur).unwrap();
    let claude_help = helps.iter().find(|e|
        matches!(&e.source, CommandSource::Agent { handle } if handle == "claude")
    ).unwrap();
    assert_eq!(reg.canonical_typed_form(spur_help), "/spur:help");
    assert_eq!(reg.canonical_typed_form(claude_help), "/claude:help");
}

#[test]
fn registry_unique_names_use_bare_form() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands("claude", vec![acp_cmd("compact", "", None)]);
    let entries = reg.list();
    let compact = entries.iter().find(|e| e.name == "compact").unwrap();
    assert_eq!(reg.canonical_typed_form(compact), "/compact");
}

#[test]
fn registry_resolve_prefers_explicit_prefix() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands("claude", vec![acp_cmd("help", "", None)]);
    let entry = reg.resolve("/claude:help").expect("match");
    assert!(matches!(&entry.source, CommandSource::Agent { handle } if handle == "claude"));
}

#[test]
fn registry_resolve_bare_ambiguous_prefers_spur() {
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands("claude", vec![acp_cmd("help", "", None)]);
    let entry = reg.resolve("/help").expect("match");
    assert_eq!(entry.source, CommandSource::Spur);
}

#[test]
fn registry_resolve_unknown_returns_none() {
    let reg = CommandRegistry::new();
    assert!(reg.resolve("/does-not-exist").is_none());
    assert!(reg.resolve("hello world").is_none()); // not a slash command
}
```

- [ ] **Step 2: run.** `cargo test -p spur-tui --test command_registry` — FAIL on new tests.

- [ ] **Step 3: implement the registry.**

`crates/spur-tui/src/commands/registry.rs`:

```rust
use spur_acp::{AvailableCommand, AvailableCommandInput};
use super::entry::{CommandEntry, CommandSource, Dispatch};
use super::spur_local::SpurLocalSource;

/// Merges spur-local and agent-advertised slash commands.
///
/// Collision rule: when a name is defined by more than one source, the
/// popup displays each variant separately. The *canonical typed form* of
/// an entry is bare (`/name`) when unique across sources, or prefixed
/// (`/<source>:<name>`) on collision. Resolution at submit time honors
/// explicit prefixes first, then falls back to spur-local-wins for
/// ambiguous bare names.
pub struct CommandRegistry {
    agent_commands: Vec<(String, Vec<AvailableCommand>)>,
    // (handle, list) pairs; order preserved for determinism in tests.
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self { agent_commands: Vec::new() }
    }

    /// Replace the command list for a given agent handle (e.g. "claude", "kiro").
    pub fn set_agent_commands(&mut self, handle: &str, cmds: Vec<AvailableCommand>) {
        if let Some(slot) = self.agent_commands.iter_mut().find(|(h, _)| h == handle) {
            slot.1 = cmds;
        } else {
            self.agent_commands.push((handle.to_string(), cmds));
        }
    }

    /// Full merged list: spur-local + all agents. Order: spur-local first,
    /// then each agent in insertion order.
    pub fn list(&self) -> Vec<CommandEntry> {
        let mut out = SpurLocalSource::entries();
        for (handle, cmds) in &self.agent_commands {
            for c in cmds {
                out.push(agent_entry(handle, c));
            }
        }
        out
    }

    /// The literal text that should be inserted into the InputBar when the
    /// user accepts `entry`. Returns `/name` when unique, `/<source>:<name>`
    /// on collision.
    pub fn canonical_typed_form(&self, entry: &CommandEntry) -> String {
        let colliding = self
            .list()
            .iter()
            .filter(|e| e.name == entry.name)
            .count()
            > 1;
        if colliding {
            match &entry.source {
                CommandSource::Spur => format!("/spur:{}", entry.name),
                CommandSource::Agent { handle } => format!("/{}:{}", handle, entry.name),
            }
        } else {
            format!("/{}", entry.name)
        }
    }

    /// Parse the InputBar text and return the best-matching `CommandEntry`.
    /// `text` is the full InputBar contents (may include trailing args).
    /// Returns `None` if no leading `/<name>` or `/<source>:<name>` matches.
    pub fn resolve(&self, text: &str) -> Option<CommandEntry> {
        let rest = text.strip_prefix('/')?;
        let first_token = rest.split_whitespace().next()?;
        let entries = self.list();
        if let Some((source, name)) = first_token.split_once(':') {
            // Explicit prefix form.
            return entries.into_iter().find(|e| {
                e.name == name
                    && match (&e.source, source) {
                        (CommandSource::Spur, "spur") => true,
                        (CommandSource::Agent { handle }, s) => handle == s,
                        _ => false,
                    }
            });
        }
        // Bare form — prefer spur-local on ambiguity.
        let mut candidates: Vec<_> =
            entries.into_iter().filter(|e| e.name == first_token).collect();
        if candidates.is_empty() {
            return None;
        }
        candidates.sort_by_key(|e| match &e.source {
            CommandSource::Spur => 0,
            CommandSource::Agent { .. } => 1,
        });
        candidates.into_iter().next()
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn agent_entry(handle: &str, c: &AvailableCommand) -> CommandEntry {
    let hint = match &c.input {
        Some(AvailableCommandInput::Unstructured(u)) => Some(u.hint.clone()),
        _ => None,
    };
    CommandEntry {
        name: c.name.clone(),
        description: c.description.clone(),
        hint,
        source: CommandSource::Agent { handle: handle.to_string() },
        dispatch: Dispatch::PromptText {
            normalized: format!("/{}", c.name),
        },
    }
}
```

Update `crates/spur-tui/src/commands/mod.rs`:

```rust
pub mod entry;
pub mod registry;
pub mod spur_local;

pub use entry::{CommandEntry, CommandSource, Dispatch};
pub use registry::CommandRegistry;
pub use spur_local::SpurLocalSource;
```

- [ ] **Step 4: run.** `cargo test -p spur-tui --test command_registry` — PASS.

- [ ] **Step 5: wire the registry onto `SessionDetailView` (replacing the raw `Vec<AvailableCommand>` storage).**

In `crates/spur-tui/src/views/session_detail.rs`:

Replace the `pub available_commands: Vec<AvailableCommand>` field with:

```rust
/// Merged slash-command registry (spur-local + agent-advertised).
pub(crate) command_registry: crate::commands::CommandRegistry,
```

Remove the old `available_commands()` accessor; add:

```rust
pub fn command_registry(&self) -> &crate::commands::CommandRegistry {
    &self.command_registry
}
```

Update init in `SessionDetailView::new`:

```rust
command_registry: crate::commands::CommandRegistry::new(),
```

Update `apply_session_update` in `app.rs`:

```rust
AvailableCommandsUpdate(u) => {
    let agent_handle = state.agent_handle_for_commands();
    state.command_registry.set_agent_commands(&agent_handle, u.available_commands.clone());
}
```

Add a helper on `SessionDetailView` that returns the lowercased agent name for namespacing:

```rust
fn agent_handle_for_commands(&self) -> String {
    self.agent_name.to_lowercase()
}
```

Update the existing test from Task 1 to read from the registry instead:

```rust
// was: view.available_commands()
// now:
let entries = view.command_registry().list();
let compact = entries.iter().find(|e| e.name == "compact").expect("compact present");
assert_eq!(compact.description, "compact history");
assert_eq!(compact.hint.as_deref(), Some("[threshold]"));
```

- [ ] **Step 6: run full spur-tui suite + clippy.** `cargo test -p spur-tui && cargo clippy -p spur-tui --all-targets -- -D warnings` — PASS.

- [ ] **Step 7: commit.**

```
git add crates/spur-tui/src/commands/ crates/spur-tui/src/views/session_detail.rs \
       crates/spur-tui/src/app.rs crates/spur-tui/tests/command_registry.rs \
       crates/spur-tui/tests/session_update_handling.rs
git commit -m "feat(spur-tui): CommandRegistry with prefix-on-collision grammar"
```

---

## Task 5: Add `nucleo-matcher` dep and a fuzzy-ranking helper

The popup filters candidates as the user types. `nucleo-matcher` is helix-editor's fzf-style fuzzy matcher; it's the standard choice for Rust TUIs.

**Files:**
- Modify: `crates/spur-tui/Cargo.toml`
- Create: `crates/spur-tui/src/commands/fuzzy.rs`
- Modify: `crates/spur-tui/src/commands/mod.rs`
- Test: `crates/spur-tui/tests/command_registry.rs`

- [ ] **Step 1: add the dep.**

In `crates/spur-tui/Cargo.toml`:

```toml
[dependencies]
# ... existing ...
nucleo-matcher = "0.3"
```

- [ ] **Step 2: failing test.**

Append to `crates/spur-tui/tests/command_registry.rs`:

```rust
#[test]
fn fuzzy_rank_commands_prefers_prefix_matches() {
    use spur_tui::commands::{fuzzy::rank, CommandRegistry};
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands(
        "claude",
        vec![
            acp_cmd("compact", "", None),
            acp_cmd("config", "", None),
            acp_cmd("doctor", "", None),
        ],
    );
    let entries = reg.list();
    let ranked = rank(&entries, "co");
    // "compact" and "config" should beat "doctor"
    let names: Vec<&str> = ranked.iter().map(|e| e.name.as_str()).collect();
    assert!(names[0] == "compact" || names[0] == "config", "top: {:?}", names);
    assert!(!names.contains(&"doctor") || names.iter().position(|n| *n == "doctor").unwrap() > 1);
}

#[test]
fn fuzzy_rank_empty_query_returns_input_order() {
    use spur_tui::commands::fuzzy::rank;
    use spur_tui::commands::CommandRegistry;
    let reg = CommandRegistry::new();
    let entries = reg.list();
    let ranked = rank(&entries, "");
    assert_eq!(ranked.len(), entries.len());
    assert_eq!(ranked[0].name, entries[0].name);
}
```

- [ ] **Step 3: run.** `cargo test -p spur-tui --test command_registry fuzzy` — FAIL (module missing).

- [ ] **Step 4: implement.**

`crates/spur-tui/src/commands/fuzzy.rs`:

```rust
use nucleo_matcher::{pattern::{CaseMatching, Normalization, Pattern}, Matcher};
use super::entry::CommandEntry;

/// Rank `entries` by fuzzy-match against `query`.
///
/// * Empty query: input order preserved.
/// * Non-empty query: entries with a positive nucleo score are sorted by
///   descending score; unmatched entries are omitted.
pub fn rank(entries: &[CommandEntry], query: &str) -> Vec<CommandEntry> {
    if query.is_empty() {
        return entries.to_vec();
    }
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut scored: Vec<(u32, CommandEntry)> = entries
        .iter()
        .filter_map(|e| {
            let haystack = e.name.clone();
            let score = pattern.score(
                nucleo_matcher::Utf32Str::new(&haystack, &mut Vec::new()),
                &mut matcher,
            )?;
            Some((score, e.clone()))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().map(|(_, e)| e).collect()
}
```

Update `crates/spur-tui/src/commands/mod.rs`:

```rust
pub mod entry;
pub mod fuzzy;
pub mod registry;
pub mod spur_local;

pub use entry::{CommandEntry, CommandSource, Dispatch};
pub use registry::CommandRegistry;
pub use spur_local::SpurLocalSource;
```

- [ ] **Step 5: run.** `cargo test -p spur-tui --test command_registry fuzzy` — PASS. Run full suite + clippy.

- [ ] **Step 6: commit.**

```
git add crates/spur-tui/Cargo.toml crates/spur-tui/src/commands/ \
       crates/spur-tui/tests/command_registry.rs
git commit -m "feat(spur-tui): fuzzy command ranking via nucleo-matcher"
```

---

## Task 6: `CompletionTrigger` — detect `/` / `@` prefix + query

Pure logic module: given `(text: &str, cursor: usize)`, returns whether a popup should be open, the kind, and the active query.

**Files:**
- Create: `crates/spur-tui/src/components/completion_trigger.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`
- Test: `crates/spur-tui/tests/completion_trigger.rs`

- [ ] **Step 1: failing test.**

`crates/spur-tui/tests/completion_trigger.rs`:

```rust
use spur_tui::components::completion_trigger::{detect, Trigger, TriggerKind};

#[test]
fn slash_at_offset_zero_opens_slash_trigger() {
    let t = detect("/he", 3).expect("slash trigger");
    assert_eq!(t.kind, TriggerKind::Slash);
    assert_eq!(t.query, "he");
    assert_eq!(t.prefix_start, 0);
}

#[test]
fn slash_after_whitespace_does_not_trigger_in_v1() {
    assert!(detect("hello /foo", 10).is_none());
}

#[test]
fn at_after_whitespace_opens_mention_trigger() {
    let t = detect("look at @sr", 11).expect("mention trigger");
    assert_eq!(t.kind, TriggerKind::Mention);
    assert_eq!(t.query, "sr");
    assert_eq!(t.prefix_start, 8);
}

#[test]
fn at_at_offset_zero_opens_mention_trigger() {
    let t = detect("@foo", 4).expect("mention trigger");
    assert_eq!(t.kind, TriggerKind::Mention);
    assert_eq!(t.query, "foo");
}

#[test]
fn mention_closes_on_space() {
    assert!(detect("look at @foo bar", 16).is_none());
}

#[test]
fn cursor_before_trigger_means_no_trigger() {
    // cursor at 0, but the "@" is at position 8 — nothing active here.
    assert!(detect("look at @foo", 0).is_none());
}

#[test]
fn empty_query_after_trigger() {
    let t = detect("/", 1).expect("empty-query trigger");
    assert_eq!(t.kind, TriggerKind::Slash);
    assert_eq!(t.query, "");
}
```

- [ ] **Step 2: run.** `cargo test -p spur-tui --test completion_trigger` — FAIL.

- [ ] **Step 3: implement.**

`crates/spur-tui/src/components/completion_trigger.rs`:

```rust
/// The kind of prefix that opened the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    /// Slash-command: `/…`. v1 only fires at byte offset 0.
    Slash,
    /// Resource mention: `@…`. Fires anywhere after whitespace or at offset 0.
    Mention,
}

/// An active popup trigger detected in the InputBar text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// Byte offset of the trigger char (`/` or `@`) in `text`.
    pub prefix_start: usize,
    /// The query between the trigger char and the cursor (no leading char).
    pub query: String,
}

/// Decide whether a popup should be open given `(text, cursor)`.
///
/// Rules (v1):
///   * `/` fires only at byte offset 0.
///   * `@` fires at byte offset 0 OR immediately after ASCII whitespace.
///   * Any whitespace character between the trigger char and the cursor
///     closes the popup.
pub fn detect(text: &str, cursor: usize) -> Option<Trigger> {
    if cursor == 0 || cursor > text.len() {
        return None;
    }
    let before = &text[..cursor];

    // Slash: at offset 0 only.
    if before.starts_with('/') {
        let query = &before[1..];
        if !query.contains(char::is_whitespace) {
            return Some(Trigger {
                kind: TriggerKind::Slash,
                prefix_start: 0,
                query: query.to_string(),
            });
        }
    }

    // Mention: find the last '@' that is preceded by start-of-string or whitespace,
    // then verify no whitespace intervenes between '@' and cursor.
    if let Some(at_pos) = before.rfind('@') {
        let prev_is_boundary = at_pos == 0
            || before[..at_pos]
                .chars()
                .last()
                .map_or(true, |c| c.is_whitespace());
        if prev_is_boundary {
            let query = &before[at_pos + 1..];
            if !query.contains(char::is_whitespace) {
                return Some(Trigger {
                    kind: TriggerKind::Mention,
                    prefix_start: at_pos,
                    query: query.to_string(),
                });
            }
        }
    }

    None
}
```

Update `crates/spur-tui/src/components/mod.rs`:

```rust
pub mod activity_log;
pub mod completion_trigger;
pub mod detail_pane;
pub mod agents_tree;
pub mod help_overlay;
pub mod input_bar;
pub mod line_wrap;
pub mod react_trace;
pub mod review_card;
pub mod status_bar;
// … keep existing constants/functions unchanged
```

- [ ] **Step 4: run.** `cargo test -p spur-tui --test completion_trigger` — PASS.

- [ ] **Step 5: clippy + commit.**

```
cargo clippy -p spur-tui --all-targets -- -D warnings
git add crates/spur-tui/src/components/completion_trigger.rs \
       crates/spur-tui/src/components/mod.rs \
       crates/spur-tui/tests/completion_trigger.rs
git commit -m "feat(spur-tui): CompletionTrigger detection for / and @"
```

---

## Task 7: `CompletionPopup` render widget

Renders a list of rows above the InputBar. No keyboard logic yet — just render + selection state.

**Files:**
- Create: `crates/spur-tui/src/components/completion_popup.rs`
- Modify: `crates/spur-tui/src/components/mod.rs`

- [ ] **Step 1: implement the component.**

`crates/spur-tui/src/components/completion_popup.rs`:

```rust
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState},
    Frame,
};

/// A single row shown in the popup.
#[derive(Debug, Clone)]
pub struct PopupRow {
    /// Left label (e.g. "/help" or "@src/foo.rs").
    pub label: String,
    /// Middle description (may be empty).
    pub description: String,
    /// Right-side tag (e.g. "⟨claude⟩"). Empty string for no tag.
    pub source_tag: String,
}

/// Overlay list widget shown above the InputBar for autocomplete.
pub struct CompletionPopup {
    rows: Vec<PopupRow>,
    state: ListState,
    empty_message: String,
}

impl CompletionPopup {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            state: ListState::default(),
            empty_message: "No matches. Type to refine, Esc to dismiss.".to_string(),
        }
    }

    pub fn set_rows(&mut self, rows: Vec<PopupRow>) {
        self.rows = rows;
        if !self.rows.is_empty() {
            self.state.select(Some(0));
        } else {
            self.state.select(None);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn rows(&self) -> &[PopupRow] {
        &self.rows
    }

    pub fn selected(&self) -> Option<usize> {
        self.state.selected()
    }

    pub fn select_next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let i = self.state.selected().map_or(0, |i| (i + 1) % self.rows.len());
        self.state.select(Some(i));
    }

    pub fn select_prev(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let len = self.rows.len();
        let i = self.state.selected().map_or(0, |i| (i + len - 1) % len);
        self.state.select(Some(i));
    }

    /// Render above `anchor` (typically the InputBar's rect).
    pub fn render(&mut self, frame: &mut Frame, anchor: Rect, container: Rect) {
        let max_rows = self.rows.len().max(1).min(8) as u16;
        let popup_height = max_rows + 2; // +2 for the block border
        let max_label = self.rows.iter().map(|r| r.label.len()).max().unwrap_or(0);
        let max_desc = self.rows.iter().map(|r| r.description.len()).max().unwrap_or(0);
        let max_tag = self.rows.iter().map(|r| r.source_tag.len()).max().unwrap_or(0);
        let desired_width = (max_label + max_desc + max_tag + 8) as u16;
        let popup_width = desired_width.min(container.width / 2).max(30);

        let x = anchor.x.saturating_add(2).min(container.x + container.width.saturating_sub(popup_width));
        let y = anchor.y.saturating_sub(popup_height);
        let popup_area = Rect::new(x, y, popup_width, popup_height);

        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        if self.rows.is_empty() {
            use ratatui::widgets::Paragraph;
            let p = Paragraph::new(Line::from(Span::styled(
                self.empty_message.as_str(),
                Style::default().fg(Color::DarkGray),
            )))
            .block(block);
            frame.render_widget(p, popup_area);
            return;
        }

        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|r| {
                let mut spans = Vec::with_capacity(4);
                spans.push(Span::styled(
                    r.label.clone(),
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                ));
                if !r.description.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        r.description.clone(),
                        Style::default().fg(Color::White),
                    ));
                }
                if !r.source_tag.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        r.source_tag.clone(),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();

        let list = List::new(items)
            .block(block)
            .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

        frame.render_stateful_widget(list, popup_area, &mut self.state);
    }
}

impl Default for CompletionPopup {
    fn default() -> Self {
        Self::new()
    }
}
```

Update `crates/spur-tui/src/components/mod.rs`:

```rust
pub mod completion_popup;
```

- [ ] **Step 2: smoke test via a minimal unit test.**

Append to `crates/spur-tui/tests/completion_trigger.rs` (same file, cheap place):

```rust
#[test]
fn completion_popup_select_cycles() {
    use spur_tui::components::completion_popup::{CompletionPopup, PopupRow};
    let mut p = CompletionPopup::new();
    p.set_rows(vec![
        PopupRow { label: "/help".into(), description: "".into(), source_tag: "⟨spur⟩".into() },
        PopupRow { label: "/compact".into(), description: "".into(), source_tag: "⟨claude⟩".into() },
    ]);
    assert_eq!(p.selected(), Some(0));
    p.select_next();
    assert_eq!(p.selected(), Some(1));
    p.select_next();
    assert_eq!(p.selected(), Some(0));
    p.select_prev();
    assert_eq!(p.selected(), Some(1));
}
```

- [ ] **Step 3: run + clippy.** `cargo test -p spur-tui && cargo clippy -p spur-tui --all-targets -- -D warnings` — PASS.

- [ ] **Step 4: commit.**

```
git add crates/spur-tui/src/components/completion_popup.rs \
       crates/spur-tui/src/components/mod.rs \
       crates/spur-tui/tests/completion_trigger.rs
git commit -m "feat(spur-tui): CompletionPopup overlay list widget"
```

---

## Task 8: Wire the slash-command popup into `SessionDetailView`

Open popup on `/`, filter with `nucleo` against `CommandRegistry`, route keys (Up/Down/Enter/Tab/Esc/Ctrl-C/Backspace-on-empty), insert canonical typed form on accept.

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`
- Modify: `crates/spur-tui/src/components/input_bar.rs` (add `cursor()` accessor)

- [ ] **Step 1: expose cursor on InputBar.**

In `crates/spur-tui/src/components/input_bar.rs`, after `text()`:

```rust
/// Current cursor byte offset in `text`.
pub fn cursor(&self) -> usize {
    self.cursor
}
```

- [ ] **Step 2: add popup to SessionDetailView.**

Because `View::render` takes `&self` (verified: `crates/spur-tui/src/views/mod.rs:19`), wrap the popup in `RefCell` so `render` can mutate `ListState` for selection highlighting.

In `crates/spur-tui/src/views/session_detail.rs`, add fields:

```rust
completion_popup: std::cell::RefCell<crate::components::completion_popup::CompletionPopup>,
active_trigger: Option<crate::components::completion_trigger::Trigger>,
```

Init:

```rust
completion_popup: std::cell::RefCell::new(
    crate::components::completion_popup::CompletionPopup::new(),
),
active_trigger: None,
```

Everywhere the popup is mutated, use `self.completion_popup.borrow_mut()`. Read-only access uses `self.completion_popup.borrow()`.

- [ ] **Step 3: add a `refresh_popup` method that runs after every InputBar edit.**

```rust
fn refresh_popup(&mut self) {
    use crate::components::completion_popup::PopupRow;
    use crate::components::completion_trigger::{detect, TriggerKind};
    use crate::commands::fuzzy;

    let text = self.input_bar.text();
    let cursor = self.input_bar.cursor();
    let trig = detect(text, cursor);
    self.active_trigger = trig.clone();

    match trig {
        Some(t) if t.kind == TriggerKind::Slash => {
            let entries = self.command_registry.list();
            let ranked = fuzzy::rank(&entries, &t.query);
            let rows: Vec<PopupRow> = ranked
                .iter()
                .map(|e| PopupRow {
                    label: self.command_registry.canonical_typed_form(e),
                    description: e.description.clone(),
                    source_tag: match &e.source {
                        crate::commands::CommandSource::Spur => "⟨spur⟩".into(),
                        crate::commands::CommandSource::Agent { handle } => {
                            format!("⟨{}⟩", handle)
                        }
                    },
                })
                .collect();
            self.completion_popup.set_rows(rows);
        }
        _ => {
            // Mention popup wired in Task 14; for now clear rows.
            self.completion_popup.set_rows(Vec::new());
        }
    }
}

fn popup_open(&self) -> bool {
    self.active_trigger.is_some() && !self.completion_popup.is_empty()
}

/// Replace the range [prefix_start..cursor] in the InputBar with `replacement`.
/// Leaves the cursor at the end of the replacement.
fn replace_trigger_token(&mut self, prefix_start: usize, replacement: &str) {
    // InputBar exposes text() but not a range-edit API yet. Clear and rebuild.
    // For this task the popup inserts a slash-command token; mention insert
    // uses insert_atom (Task 13).
    let current = self.input_bar.text().to_string();
    let cursor = self.input_bar.cursor();
    let mut new_text = String::with_capacity(current.len());
    new_text.push_str(&current[..prefix_start]);
    new_text.push_str(replacement);
    new_text.push_str(&current[cursor..]);
    let new_cursor = prefix_start + replacement.len();
    self.input_bar.set_text(new_text, new_cursor);
}
```

- [ ] **Step 4: add `set_text` on InputBar.**

In `crates/spur-tui/src/components/input_bar.rs`:

```rust
/// Replace `text` and cursor wholesale. Panics if `cursor > text.len()` or
/// the cursor is not on a UTF-8 char boundary.
pub fn set_text(&mut self, text: String, cursor: usize) {
    assert!(cursor <= text.len(), "cursor past end");
    assert!(text.is_char_boundary(cursor), "cursor off UTF-8 boundary");
    self.text = text;
    self.cursor = cursor;
}
```

- [ ] **Step 5: rewire `handle_key` to intercept popup keys.**

In `session_detail.rs::handle_key`, **insert a new priority tier between the permission-handling block (line 210) and the editing-key block (line 213)**:

```rust
// Priority 3.5: popup is open — route navigation/accept/dismiss keys.
if self.popup_open() {
    match key.code {
        KeyCode::Up => { self.completion_popup.select_prev(); return None; }
        KeyCode::Down => { self.completion_popup.select_next(); return None; }
        KeyCode::Esc => {
            self.active_trigger = None;
            self.completion_popup.set_rows(Vec::new());
            return None;
        }
        KeyCode::Enter | KeyCode::Tab => {
            return self.accept_completion();
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            self.active_trigger = None;
            self.completion_popup.set_rows(Vec::new());
            return None;
        }
        _ => { /* fall through to editing */ }
    }
}
```

At the end of the `is_editing_key` block (after `input_bar.handle_key`, before the fall-through scroll logic), call `self.refresh_popup();`.

Add the `accept_completion` helper:

```rust
fn accept_completion(&mut self) -> Option<Action> {
    use crate::commands::CommandSource;
    use crate::components::completion_trigger::TriggerKind;

    let trig = self.active_trigger.clone()?;
    let idx = self.completion_popup.selected()?;
    let rows = self.completion_popup.rows().to_vec();
    let row = rows.get(idx)?;

    match trig.kind {
        TriggerKind::Slash => {
            // row.label is the canonical typed form (e.g. "/help" or "/claude:help").
            // Replace [prefix_start..cursor] with label + " ".
            let insertion = format!("{} ", row.label);
            self.replace_trigger_token(trig.prefix_start, &insertion);
            self.active_trigger = None;
            self.completion_popup.set_rows(Vec::new());
            None
        }
        TriggerKind::Mention => {
            // Wired in Task 14.
            self.active_trigger = None;
            self.completion_popup.set_rows(Vec::new());
            None
        }
    }
    // Mark field use of CommandSource to avoid unused warning if it shifts:
    // (compiler will accept; explicit import above covers it)
}
```

- [ ] **Step 6: render the popup overlay in `SessionDetailView::render`.**

After the existing `self.input_bar.render(frame, chunks[2]);` line:

```rust
if self.popup_open() {
    self.completion_popup
        .borrow_mut()
        .render(frame, chunks[2], area);
}
```

The `RefCell` wrapper added in Step 2 allows this from `render(&self, ...)`.

- [ ] **Step 7: manual smoke test.**

Run spur end-to-end against a local agent and verify:
1. Typing `/` opens the popup with `/help`, `/mode`, `/cost`, `/quit` visible (no agent commands yet since none connected).
2. Up/Down navigates.
3. Enter inserts `/help ` and closes the popup.
4. Esc dismisses.
5. Backspace over `/` closes the popup.

If no live agent is available, at minimum run `cargo test -p spur-tui` to ensure the wired code compiles and existing tests pass.

- [ ] **Step 8: clippy + commit.**

```
cargo clippy -p spur-tui --all-targets -- -D warnings
git add crates/spur-tui/
git commit -m "feat(spur-tui): wire slash-command popup into SessionDetailView"
```

---

## Task 9: `Action::SendMessage` carries `Vec<ContentBlock>`

Rewire the action, the `UserInput::Message` channel type, the orchestrator send path, and every call site.

**Files:**
- Modify: `crates/spur-tui/src/action.rs`
- Modify: `crates/spur-tui/src/app.rs` (both `UserInput::Message` enum and the `SendMessage` arm)
- Modify: `crates/spur-tui/src/views/session_detail.rs` (construction)
- Modify: `crates/spur-tui/src/views/dashboard.rs:307`
- Modify: `crates/spur-cli/src/main.rs:381` (destructuring)
- Modify: `crates/spur-core/src/orchestrator.rs:553` (plus 256, 731, 1539 — only rewire 553 in this task; the other sites are internal orchestrator uses and should continue building their own blocks inline)

- [ ] **Step 1: update `Action::SendMessage`.**

In `crates/spur-tui/src/action.rs`:

```rust
SendMessage {
    session: SessionId,
    blocks: Vec<spur_acp::ContentBlock>,
    interrupt: bool,
},
```

- [ ] **Step 2: update `UserInput::Message`.**

In `crates/spur-tui/src/app.rs:23`:

```rust
pub enum UserInput {
    Message {
        session: SessionId,
        blocks: Vec<spur_acp::ContentBlock>,
        interrupt: bool,
    },
    ListSessions,
    ResumeSession { session_id: String },
    SetSessionMode { mode_id: String },
    SubmitReview { executor_id: String, decision: spur_core::ReviewDecision },
}
```

- [ ] **Step 3: update the `Action::SendMessage` arm in `app.rs:334`.**

Replace the body to work with `blocks`:

```rust
Action::SendMessage { session, blocks, interrupt } => {
    if matches!(self.brain_status, BrainStatus::Ready | BrainStatus::Idle | BrainStatus::Error(_)) {
        self.brain_status = BrainStatus::Thinking;
    }

    // Flatten blocks to a preview string for the trace echo.
    let preview = crate::commands::submit_router::blocks_preview(&blocks);

    if let Some(ref mut detail) = self.session_detail {
        detail.push_user_message(&preview);
    } else {
        self.pending_user_messages.push(preview.clone());
    }

    if let Some(ref tx) = self.user_input_tx {
        let _ = tx.try_send(UserInput::Message { session, blocks, interrupt });
    }

    self.sync_brain_status();
}
```

Note: `push_user_message` today takes `&str`. The preview function converts blocks back into a user-readable string for the local trace echo. Define `blocks_preview` in Task 10 (`submit_router.rs`). For this task, add a minimal local helper so this compiles:

```rust
// Temporary helper lives at crate::commands::submit_router::blocks_preview
// and is defined for real in Task 10. Define a stub here:
// (Don't do this — instead merge Task 9 and Task 10 into one commit if easier.)
```

**Implementation tip:** do Tasks 9 and 10 in one commit. They both depend on each other to compile.

- [ ] **Step 4: update `session_detail.rs::handle_key`.**

At line 226-231, change the construction:

```rust
if let Some((text, interrupt)) = self.input_bar.handle_key(key) {
    let blocks = vec![spur_acp::ContentBlock::Text(
        spur_acp::TextContent::new(text),
    )];
    return Some(Action::SendMessage {
        session: self.session_id.clone(),
        blocks,
        interrupt,
    });
}
```

(This gives slash commands and mentions a consistent entry point — the real routing lands in Task 10.)

- [ ] **Step 5: update `dashboard.rs:307`.**

```rust
return Some(Action::SendMessage {
    session: spur_acp::SessionId::new(),
    blocks: vec![spur_acp::ContentBlock::Text(
        spur_acp::TextContent::new(text),
    )],
    interrupt,
});
```

- [ ] **Step 6: update `spur-cli/src/main.rs:381`.**

Find:

```rust
spur_tui::UserInput::Message { text, interrupt, .. } => {
```

Change to:

```rust
spur_tui::UserInput::Message { blocks, interrupt, .. } => {
```

Wherever `text` is used in that arm, replace with a flattened string rebuilt from `blocks` by calling `spur_tui::commands::submit_router::blocks_to_text(&blocks)` (a helper we add in Task 10). If the CLI main doesn't need the text itself and only forwards `blocks`, forward `blocks` directly to the orchestrator.

- [ ] **Step 7: update `orchestrator.rs:553`.**

The orchestrator's `run_interactive` (or similar) currently receives `text` from `UserInput::Message`. Change the signature or the channel payload so it receives `blocks: Vec<ContentBlock>` and forwards directly to `PromptRequest::new`:

```rust
let prompt_request = PromptRequest::new(
    b.acp_session_id.clone(),
    blocks,
);
```

Call sites at 256, 731, 1539 (internal orchestrator uses, not user-text) remain unchanged — they construct their own block vectors for worker prompts etc.

- [ ] **Step 8: run full workspace tests + clippy.**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: compile, all existing tests pass. If `UserInput::Message` is destructured anywhere else we missed, fix and re-run.

- [ ] **Step 9: commit.**

Commit together with Task 10 (see note above) or split cleanly only if `blocks_preview` is stubbed inline.

---

## Task 10: `SubmitRouter` — parse text into blocks + dispatch

Given `(text, protected_ranges, registry)`, produce a `SubmitDecision`:

```rust
pub enum SubmitDecision {
    Send { blocks: Vec<ContentBlock>, interrupt: bool },
    Local { action: Action },
    KiroExecute { command: String, args: Value },
    Empty,
}
```

For Task 10 we cover only the `Send` and `Local` paths (KiroExecute lands in Task 11 when the plumbing exists).

**Files:**
- Create: `crates/spur-tui/src/commands/submit_router.rs`
- Modify: `crates/spur-tui/src/commands/mod.rs`
- Test: `crates/spur-tui/tests/submit_router.rs`

- [ ] **Step 1: failing test.**

`crates/spur-tui/tests/submit_router.rs`:

```rust
use serde_json::json;
use spur_acp::ContentBlock;
use spur_tui::action::Action;
use spur_tui::commands::submit_router::{SubmitDecision, route};
use spur_tui::commands::CommandRegistry;

#[test]
fn plain_text_routes_to_send() {
    let reg = CommandRegistry::new();
    let dec = route("hello world", &[], &reg, false);
    match dec {
        SubmitDecision::Send { blocks, interrupt } => {
            assert_eq!(blocks.len(), 1);
            assert!(matches!(&blocks[0], ContentBlock::Text(t) if t.text == "hello world"));
            assert!(!interrupt);
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn spur_local_slash_dispatches_action() {
    let reg = CommandRegistry::new();
    let dec = route("/help", &[], &reg, false);
    match dec {
        SubmitDecision::Local { action } => {
            assert!(matches!(action, Action::ShowHelp));
        }
        other => panic!("expected Local, got {:?}", other),
    }
}

#[test]
fn agent_slash_becomes_text_block_stripped_of_prefix() {
    use spur_acp::{AvailableCommand, AvailableCommandInput, UnstructuredCommandInput};
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands(
        "claude",
        vec![AvailableCommand {
            name: "compact".into(),
            description: "".into(),
            input: None,
            meta: None,
        }],
    );
    let dec = route("/compact please", &[], &reg, false);
    match dec {
        SubmitDecision::Send { blocks, .. } => {
            assert_eq!(blocks.len(), 1);
            match &blocks[0] {
                ContentBlock::Text(t) => assert_eq!(t.text, "/compact please"),
                other => panic!("expected text, got {:?}", other),
            }
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn explicit_prefix_claude_help_sends_bare_to_claude() {
    use spur_acp::{AvailableCommand};
    let mut reg = CommandRegistry::new();
    reg.set_agent_commands(
        "claude",
        vec![AvailableCommand { name: "help".into(), description: "".into(), input: None, meta: None }],
    );
    let dec = route("/claude:help", &[], &reg, false);
    match dec {
        SubmitDecision::Send { blocks, .. } => {
            match &blocks[0] {
                ContentBlock::Text(t) => assert_eq!(t.text, "/help"),
                other => panic!("got {:?}", other),
            }
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn interrupt_prefix_bang_is_preserved() {
    let reg = CommandRegistry::new();
    let dec = route("!stop now", &[], &reg, true);
    match dec {
        SubmitDecision::Send { interrupt, blocks } => {
            assert!(interrupt);
            match &blocks[0] {
                ContentBlock::Text(t) => assert_eq!(t.text, "!stop now"),
                other => panic!("got {:?}", other),
            }
        }
        other => panic!("expected Send, got {:?}", other),
    }
}

#[test]
fn blocks_preview_roundtrips_text() {
    use spur_tui::commands::submit_router::blocks_preview;
    let blocks = vec![ContentBlock::Text(spur_acp::TextContent::new("hello"))];
    assert_eq!(blocks_preview(&blocks), "hello");
}
```

- [ ] **Step 2: run.** `cargo test -p spur-tui --test submit_router` — FAIL.

- [ ] **Step 3: implement.**

`crates/spur-tui/src/commands/submit_router.rs`:

```rust
use serde_json::Value;
use spur_acp::{ContentBlock, TextContent};
use crate::action::Action;
use crate::components::input_bar::ProtectedRange;
use super::entry::{CommandSource, Dispatch};
use super::registry::CommandRegistry;

/// What the controller should do with an Enter-submitted InputBar.
#[derive(Debug)]
pub enum SubmitDecision {
    /// Send these content blocks to the agent.
    Send { blocks: Vec<ContentBlock>, interrupt: bool },
    /// Fire a local `Action` (e.g. ShowHelp). Do not send.
    Local { action: Action },
    /// Invoke the kiro vendor execute method. Do not send to agent.
    KiroExecute { command: String, args: Value },
    /// Empty input — no-op.
    Empty,
}

/// Compute the decision given the submitted text.
///
/// `ranges` are the protected ranges in the text; an empty slice means
/// no mentions. `interrupt` is `true` when the InputBar detected a leading
/// `!`. For v1 the router ignores mentions when the text is a slash
/// command (the mention atoms still appear in the preview but are not
/// stripped from the forwarded text).
pub fn route(
    text: &str,
    ranges: &[ProtectedRange],
    registry: &CommandRegistry,
    interrupt: bool,
) -> SubmitDecision {
    if text.is_empty() {
        return SubmitDecision::Empty;
    }

    // Slash-command path: only when text starts with '/'.
    if text.starts_with('/') {
        if let Some(entry) = registry.resolve(text) {
            return match entry.dispatch {
                Dispatch::SpurLocal(action) => SubmitDecision::Local { action },
                Dispatch::PromptText { normalized } => {
                    // Rewrite "/<source>:<name> args" → "<normalized> args".
                    let rest = rest_after_first_token(text);
                    let normalized_full = if rest.is_empty() {
                        normalized
                    } else {
                        format!("{} {}", normalized, rest)
                    };
                    SubmitDecision::Send {
                        blocks: vec![ContentBlock::Text(TextContent::new(normalized_full))],
                        interrupt,
                    }
                }
                Dispatch::KiroExecute { command, args: _seed } => {
                    // Fold any trailing args into a raw-text bag per spec (§8).
                    let rest = rest_after_first_token(text);
                    let args = if rest.is_empty() {
                        serde_json::json!({})
                    } else {
                        serde_json::json!({ "args": { "raw": rest } })
                    };
                    SubmitDecision::KiroExecute { command, args }
                }
            };
        }
        // Unknown slash command — fall through to plain text.
    }

    // Mention/plain path: assemble content blocks by walking text + ranges.
    let blocks = assemble_blocks(text, ranges);
    SubmitDecision::Send { blocks, interrupt }
}

fn rest_after_first_token(text: &str) -> String {
    match text.split_once(char::is_whitespace) {
        Some((_, rest)) => rest.trim_start().to_string(),
        None => String::new(),
    }
}

/// Walk `text` + sorted `ranges` interleaved → `[Text, ResourceLink, Text, …]`.
pub fn assemble_blocks(text: &str, ranges: &[ProtectedRange]) -> Vec<ContentBlock> {
    let mut out = Vec::new();
    let mut cursor = 0usize;
    // Ranges are assumed sorted non-overlapping (invariant enforced by InputBar).
    for r in ranges {
        if r.start > cursor {
            out.push(ContentBlock::Text(TextContent::new(&text[cursor..r.start])));
        }
        let link = spur_acp::types::ResourceLink::new(r.name.clone(), r.uri.clone());
        out.push(ContentBlock::ResourceLink(link));
        cursor = r.end;
    }
    if cursor < text.len() {
        out.push(ContentBlock::Text(TextContent::new(&text[cursor..])));
    }
    if out.is_empty() {
        out.push(ContentBlock::Text(TextContent::new(text)));
    }
    out
}

/// Flatten blocks into a human-readable string for the local trace echo.
pub fn blocks_preview(blocks: &[ContentBlock]) -> String {
    let mut s = String::new();
    for b in blocks {
        match b {
            ContentBlock::Text(t) => s.push_str(&t.text),
            ContentBlock::ResourceLink(r) => {
                s.push('@');
                s.push_str(&r.name);
            }
            _ => {}
        }
    }
    s
}

/// Flatten blocks into a plain text string (for CLI path that forwards text).
pub fn blocks_to_text(blocks: &[ContentBlock]) -> String {
    blocks_preview(blocks)
}
```

Note on `ResourceLink`: the spur-acp crate must re-export it. Add to `crates/spur-acp/src/lib.rs` re-export list:

```rust
pub use agent_client_protocol::{
    // … existing …
    ResourceLink,
    ExtRequest, ExtResponse, ExtNotification,
};
```

The path `spur_acp::types::ResourceLink` may also work if `types.rs` already re-exports `*`; prefer `spur_acp::ResourceLink` directly if re-exported at top level. Update the `submit_router.rs` import accordingly.

Stub `ProtectedRange` in `crates/spur-tui/src/components/input_bar.rs` so this compiles now (real implementation in Task 12):

```rust
/// A protected byte range inside `InputBar::text` representing an atomic
/// token (today: a resource mention). Full semantics land in Task 12.
#[derive(Debug, Clone)]
pub struct ProtectedRange {
    pub start: usize,
    pub end: usize,
    pub uri: String,
    pub name: String,
}
```

Add a read-only accessor returning an empty slice until Task 12 populates it:

```rust
/// Sorted, non-overlapping protected ranges. Empty in v1 until mentions land.
pub fn protected_ranges(&self) -> &[ProtectedRange] {
    &self.protected_ranges
}
```

And the field init:

```rust
protected_ranges: Vec::new(),
```

(with the field declared as `protected_ranges: Vec<ProtectedRange>,`).

- [ ] **Step 4: Router wired into `SessionDetailView::handle_key` Enter path.**

Replace the block that constructs `Action::SendMessage` after `input_bar.handle_key` returns `Some((text, interrupt))`:

```rust
if let Some((text, interrupt)) = self.input_bar.handle_key(key) {
    // The InputBar has cleared itself. We parse the submitted text + the
    // ranges captured AT submit time (retrieved BEFORE clear — see below).
    use crate::commands::submit_router::{route, SubmitDecision};
    let dec = route(&text, &self.last_submitted_ranges, &self.command_registry, interrupt);
    self.last_submitted_ranges.clear();
    return match dec {
        SubmitDecision::Empty => None,
        SubmitDecision::Send { blocks, interrupt } => {
            Some(Action::SendMessage { session: self.session_id.clone(), blocks, interrupt })
        }
        SubmitDecision::Local { action } => Some(action),
        SubmitDecision::KiroExecute { command, args } => {
            Some(Action::KiroExecute { session: self.session_id.clone(), command, args })
        }
    };
}
```

Because `InputBar::handle_key` today calls `self.clear()` on Enter before returning, the ranges are lost. Keep the existing return signature `Option<(String, bool)>` (so dashboard.rs still compiles) and add a parallel capture slot that survives the clear. Add to `InputBar`:

```rust
submit_capture: Option<(String, Vec<ProtectedRange>, bool)>,
```

Inside `handle_key`'s `KeyCode::Enter` arm, just before `self.clear()`:

```rust
let ranges = self.protected_ranges.clone();
self.submit_capture = Some((submitted.clone(), ranges, interrupt));
```

And add the accessor:

```rust
pub fn take_submit_capture(&mut self) -> Option<(String, Vec<ProtectedRange>, bool)> {
    self.submit_capture.take()
}
```

Update `clear()` to also reset `submit_capture = None` if desired (not strictly required — the capture is consumed by `take`). Task 12 extends this by populating `ranges` from the real protected-range vector; until then the vector is always empty, so the capture carries `(text, vec![], interrupt)`.

Usage in the view:

```rust
if self.input_bar.handle_key(key).is_some() {
    let Some((text, ranges, interrupt)) = self.input_bar.take_submit_capture() else {
        return None;
    };
    // … route via SubmitRouter below …
}
```

This keeps `dashboard.rs:307` (which still uses `Some((text, interrupt))` tuple-match) working without a signature change.

- [ ] **Step 5: add `Action::KiroExecute`.**

In `crates/spur-tui/src/action.rs`:

```rust
/// Invoke the kiro vendor extension `_kiro.dev/commands/execute`.
KiroExecute {
    session: SessionId,
    command: String,
    args: serde_json::Value,
},
```

Add a stub handler in `app.rs` that logs + pushes a trace entry (real wiring in Task 11):

```rust
Action::KiroExecute { session: _, command, args: _ } => {
    if let Some(ref mut detail) = self.session_detail {
        detail.push_system_note(format!("⟨kiro⟩ /{} queued (handler pending)", command));
    }
}
```

Add `push_system_note` on `SessionDetailView`:

```rust
pub fn push_system_note(&mut self, msg: impl Into<String>) {
    use crate::components::react_trace::{TraceEntry, TraceKind};
    self.react_trace.push(TraceEntry {
        kind: TraceKind::Observe,
        text: msg.into(),
        timestamp: Self::now_stamp(),
    });
}
```

Add field to `SessionDetailView`:

```rust
last_submitted_ranges: Vec<crate::components::input_bar::ProtectedRange>,
```

Init to `Vec::new()`.

- [ ] **Step 6: run.**

```
cargo test -p spur-tui --test submit_router
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all PASS.

- [ ] **Step 7: commit (combined with Task 9).**

```
git add -A
git commit -m "feat(spur-tui): Action::SendMessage carries blocks + SubmitRouter"
```

---

## Task 11: Kiro vendor extension — inbound notification + outbound execute

Wire `_kiro.dev/commands/available` into the `CommandRegistry` and implement outbound `_kiro.dev/commands/execute` so `Action::KiroExecute` actually dispatches over the ACP connection.

**Files:**
- Modify: `crates/spur-acp/src/lib.rs` (re-exports)
- Modify: `crates/spur-acp/src/domain/events.rs` (or wherever `SpurEventBody` lives) — add an `AgentExtNotification` variant
- Modify: `crates/spur-acp/src/connection/*` — the adapter that implements the ACP `Client` trait. Hook `ext_notification` to emit the new event. Expose a method on the connection to call `ext_method`.
- Modify: `crates/spur-tui/src/app.rs` — map inbound `_kiro.dev/commands/available` into `command_registry.set_agent_commands("kiro", ...)`
- Modify: `crates/spur-tui/src/app.rs` — implement `Action::KiroExecute` by calling through the existing `user_input_tx` channel using a new `UserInput::KiroExecute` variant (or a dedicated channel; pick the minimal change)
- Modify: `crates/spur-core/src/orchestrator.rs` — wire `UserInput::KiroExecute` to call `connection.ext_method(...)` and emit result as `BrainNotify`/trace

- [ ] **Step 1: re-export ext types from spur-acp.**

In `crates/spur-acp/src/lib.rs` add `ExtRequest, ExtResponse, ExtNotification, ResourceLink` to the public re-export list (likely already done partially in Task 10; ensure all four).

- [ ] **Step 2: extend `SpurEventBody` or equivalent.**

Inspect `crates/spur-acp/src/domain/events.rs` for the existing `SpurEventBody` enum. Add a new variant:

```rust
/// Vendor-extension notification received from the agent side.
/// Routing by `method` name is the receiver's responsibility.
AgentExtNotification {
    session: SessionId,
    method: String,
    params: serde_json::Value,
},
```

Update any exhaustive matches to handle the new variant (search `match event.body` / `SpurEventBody::`).

- [ ] **Step 3: hook `ext_notification` in the ACP connection adapter.**

Search for the struct that implements `agent_client_protocol::Client` inside `crates/spur-acp/src/connection/`. Override `ext_notification`:

```rust
async fn ext_notification(&self, args: agent_client_protocol::ExtNotification) -> Result<()> {
    let method = args.method.to_string();
    let params: serde_json::Value = serde_json::from_str(args.params.get())?;
    // Extract session id from params if the method is session-scoped.
    // Kiro: _kiro.dev/commands/available has params.sessionId (camelCase).
    let session = params.get("sessionId")
        .and_then(|v| v.as_str())
        .map(|s| SessionId::from(s.to_string()))
        .unwrap_or_default();
    self.emit(SpurEvent::now(SpurEventBody::AgentExtNotification {
        session,
        method,
        params,
    }));
    Ok(())
}
```

- [ ] **Step 4: expose `call_ext` on the connection.**

On the same adapter struct, add:

```rust
pub async fn call_ext(
    &self,
    method: &str,
    params: serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let raw: std::sync::Arc<serde_json::value::RawValue> =
        serde_json::value::to_raw_value(&params)?.into();
    let req = agent_client_protocol::ExtRequest::new(method, raw);
    // Use the underlying JSON-RPC handle. Exact call depends on the adapter;
    // for NativeAcpConnection this is self.inner.ext_method(req).await.
    let resp: agent_client_protocol::ExtResponse = self.inner.ext_method(req).await?;
    let value: serde_json::Value = serde_json::from_str(resp.0.get())?;
    Ok(value)
}
```

(Replace `self.inner.ext_method` with the correct downstream path — inspect how other methods like `prompt` are proxied in the adapter and follow the same pattern.)

- [ ] **Step 5: handle `AgentExtNotification` in the TUI.**

In `crates/spur-tui/src/views/session_detail.rs::handle_spur_event`, add a match arm:

```rust
SpurEventBody::AgentExtNotification { session, method, params } => {
    if session.0 != self.session_id.0 {
        return;
    }
    if method == "_kiro.dev/commands/available" {
        // params shape: { "sessionId": "...", "availableCommands": [ { name, description, ... } ] }
        if let Some(cmds) = params.get("availableCommands").and_then(|v| v.as_array()) {
            let parsed: Vec<spur_acp::AvailableCommand> = cmds.iter()
                .filter_map(|c| serde_json::from_value::<spur_acp::AvailableCommand>(c.clone()).ok())
                .collect();
            self.command_registry.set_agent_commands("kiro", parsed);
        }
    }
}
```

Also set the entry's `dispatch` to `Dispatch::KiroExecute` for kiro-sourced commands. Update `registry::agent_entry` to switch on handle:

```rust
fn agent_entry(handle: &str, c: &AvailableCommand) -> CommandEntry {
    let hint = match &c.input {
        Some(AvailableCommandInput::Unstructured(u)) => Some(u.hint.clone()),
        _ => None,
    };
    let dispatch = if handle == "kiro" {
        Dispatch::KiroExecute { command: c.name.clone(), args: serde_json::json!({}) }
    } else {
        Dispatch::PromptText { normalized: format!("/{}", c.name) }
    };
    CommandEntry {
        name: c.name.clone(),
        description: c.description.clone(),
        hint,
        source: CommandSource::Agent { handle: handle.to_string() },
        dispatch,
    }
}
```

- [ ] **Step 6: add `UserInput::KiroExecute` and plumb through orchestrator.**

In `crates/spur-tui/src/app.rs`:

```rust
pub enum UserInput {
    // … existing …
    KiroExecute {
        session: SessionId,
        command: String,
        args: serde_json::Value,
    },
}
```

In the `Action::KiroExecute` arm:

```rust
Action::KiroExecute { session, command, args } => {
    if let Some(ref tx) = self.user_input_tx {
        let _ = tx.try_send(UserInput::KiroExecute { session, command, args });
    }
}
```

In `orchestrator.rs`, handle `UserInput::KiroExecute`:

```rust
UserInput::KiroExecute { session: _, command, args } => {
    if let Some(ref b) = brain.as_ref() {
        let params = serde_json::json!({
            "sessionId": b.acp_session_id,
            "command": command,
            "args": args,
        });
        match b.connection.call_ext("_kiro.dev/commands/execute", params).await {
            Ok(resp) => {
                self.emit(SpurEvent::now(SpurEventBody::AgentExtNotification {
                    session: b.spur_session_id.clone(),
                    method: "_spur.dev/kiro/execute/response".into(),
                    params: resp,
                }));
            }
            Err(e) => {
                self.emit(SpurEvent::now(SpurEventBody::BrainError {
                    session: b.spur_session_id.clone(),
                    message: format!("⟨kiro⟩ execute failed: {}", e),
                }));
            }
        }
    }
}
```

The synthetic response event is observed by the view and surfaced as a trace entry.

- [ ] **Step 7: write an integration test (if the orchestrator is testable without a live agent, stub the connection).**

Skip if the orchestrator test harness doesn't support connection mocking in v1. Instead add a narrow unit test on the view's `handle_spur_event` to confirm `_kiro.dev/commands/available` populates the registry:

```rust
// crates/spur-tui/tests/session_update_handling.rs (append)
#[test]
fn kiro_available_notification_populates_registry() {
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};
    use spur_tui::views::session_detail::SessionDetailView;

    let sid = SessionId::new();
    let mut view = SessionDetailView::new(sid.clone(), "kiro".into(), "brain".into());
    let params = serde_json::json!({
        "sessionId": sid.0,
        "availableCommands": [
            { "name": "context", "description": "manage context", "input": null }
        ]
    });
    let ev = SpurEvent::now(SpurEventBody::AgentExtNotification {
        session: sid,
        method: "_kiro.dev/commands/available".into(),
        params,
    });
    view.handle_spur_event(&ev);

    let entries = view.command_registry().list();
    assert!(entries.iter().any(|e| e.name == "context"
        && matches!(&e.source, spur_tui::commands::CommandSource::Agent { handle } if handle == "kiro")));
}
```

- [ ] **Step 8: test + clippy + commit.**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git add -A
git commit -m "feat(spur-acp,spur-tui): kiro vendor extension plumbing"
```

---

## Task 12: `InputBar` ProtectedRange semantics + atom editing

Make mentions act as atoms: backspace deletes whole range, arrows skip, typing inside deletes and inserts. 20+ unit tests.

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs`
- Test: `crates/spur-tui/tests/input_bar_protected_ranges.rs`

- [ ] **Step 1: failing test file.**

`crates/spur-tui/tests/input_bar_protected_ranges.rs`:

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::components::input_bar::{InputBar, ProtectedRange};

fn press(bar: &mut InputBar, code: KeyCode) {
    bar.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}
fn type_str(bar: &mut InputBar, s: &str) {
    for c in s.chars() {
        press(bar, KeyCode::Char(c));
    }
}

#[test]
fn insert_atom_creates_range_and_places_cursor_at_end() {
    let mut b = InputBar::new();
    type_str(&mut b, "hi ");
    b.insert_atom("@src/foo.rs", "file:///abs/src/foo.rs".into(), "src/foo.rs".into());
    assert_eq!(b.text(), "hi @src/foo.rs");
    assert_eq!(b.cursor(), b.text().len());
    assert_eq!(b.protected_ranges().len(), 1);
    let r = &b.protected_ranges()[0];
    assert_eq!(&b.text()[r.start..r.end], "@src/foo.rs");
}

#[test]
fn backspace_at_atom_end_deletes_whole_atom() {
    let mut b = InputBar::new();
    type_str(&mut b, "hi ");
    b.insert_atom("@src/foo.rs", "file:///a".into(), "src/foo.rs".into());
    press(&mut b, KeyCode::Backspace);
    assert_eq!(b.text(), "hi ");
    assert_eq!(b.cursor(), 3);
    assert!(b.protected_ranges().is_empty());
}

#[test]
fn backspace_inside_atom_deletes_whole_atom() {
    let mut b = InputBar::new();
    type_str(&mut b, "hi ");
    b.insert_atom("@src/foo.rs", "file:///a".into(), "src/foo.rs".into());
    // Move cursor inside the atom (between '@' and 's').
    press(&mut b, KeyCode::Left);
    press(&mut b, KeyCode::Backspace);
    assert_eq!(b.text(), "hi ");
    assert!(b.protected_ranges().is_empty());
}

#[test]
fn right_arrow_skips_atom_atomically() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " x");
    b.set_text_cursor_for_test(0);
    press(&mut b, KeyCode::Right);
    assert_eq!(b.cursor(), 5); // jumped past "@a.rs" atomically
}

#[test]
fn left_arrow_skips_atom_atomically() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    // Cursor is at end (5). Left should jump to 0.
    press(&mut b, KeyCode::Left);
    assert_eq!(b.cursor(), 0);
}

#[test]
fn typing_inside_atom_deletes_atom_then_inserts() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    // Move inside atom
    press(&mut b, KeyCode::Left);
    press(&mut b, KeyCode::Left);
    // Type 'z'
    press(&mut b, KeyCode::Char('z'));
    assert_eq!(b.text(), "z");
    assert!(b.protected_ranges().is_empty());
    assert_eq!(b.cursor(), 1);
}

#[test]
fn range_shifts_when_text_inserted_before_it() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    // Cursor at end (5). Go to start and insert "xy ".
    press(&mut b, KeyCode::Home);
    type_str(&mut b, "xy ");
    assert_eq!(b.text(), "xy @a.rs");
    let r = &b.protected_ranges()[0];
    assert_eq!(r.start, 3);
    assert_eq!(r.end, 8);
}

#[test]
fn two_atoms_preserve_sort_order() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    type_str(&mut b, " and ");
    b.insert_atom("@b.rs", "file:///b".into(), "b.rs".into());
    let ranges = b.protected_ranges();
    assert_eq!(ranges.len(), 2);
    assert!(ranges[0].start < ranges[1].start);
    assert_eq!(&b.text()[ranges[0].start..ranges[0].end], "@a.rs");
    assert_eq!(&b.text()[ranges[1].start..ranges[1].end], "@b.rs");
}

#[test]
fn clear_removes_ranges() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    b.clear();
    assert_eq!(b.text(), "");
    assert!(b.protected_ranges().is_empty());
}

#[test]
fn forward_delete_at_atom_start_deletes_whole_atom() {
    let mut b = InputBar::new();
    b.insert_atom("@a.rs", "file:///a".into(), "a.rs".into());
    press(&mut b, KeyCode::Home);
    press(&mut b, KeyCode::Delete);
    assert_eq!(b.text(), "");
    assert!(b.protected_ranges().is_empty());
}
```

Add a small test helper on InputBar (behind `#[cfg(test)]` or public with `_for_test` suffix):

```rust
/// Test-only: set the cursor position without asserting anything else.
/// Used by tests that need to position the cursor at a specific byte offset.
#[doc(hidden)]
pub fn set_text_cursor_for_test(&mut self, cursor: usize) {
    assert!(cursor <= self.text.len());
    assert!(self.text.is_char_boundary(cursor));
    self.cursor = cursor;
}
```

- [ ] **Step 2: run.** `cargo test -p spur-tui --test input_bar_protected_ranges` — all FAIL (insert_atom missing; Left/Right/Backspace not atom-aware).

- [ ] **Step 3: implement atom semantics.**

The struct fields were added as stubs in Task 10 (`protected_ranges`, `submit_capture`). Final form:

```rust
pub struct InputBar {
    text: String,
    cursor: usize,
    status: Option<String>,
    protected_ranges: Vec<ProtectedRange>,
    submit_capture: Option<(String, Vec<ProtectedRange>, bool)>,
}
```

Init:

```rust
impl InputBar {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            status: None,
            protected_ranges: Vec::new(),
            submit_capture: None,
        }
    }
}
```

Helper: find range containing a byte position:

```rust
fn range_at(&self, pos: usize) -> Option<usize> {
    self.protected_ranges.iter().position(|r| pos >= r.start && pos < r.end)
}
fn range_ending_at(&self, pos: usize) -> Option<usize> {
    self.protected_ranges.iter().position(|r| r.end == pos)
}
fn range_starting_at(&self, pos: usize) -> Option<usize> {
    self.protected_ranges.iter().position(|r| r.start == pos)
}
```

Shift ranges after an edit at byte position `at` with signed delta:

```rust
fn shift_ranges(&mut self, at: usize, delta: isize) {
    for r in &mut self.protected_ranges {
        if r.start >= at {
            r.start = (r.start as isize + delta) as usize;
            r.end = (r.end as isize + delta) as usize;
        }
    }
}
```

Delete a whole range and shift:

```rust
fn delete_range(&mut self, idx: usize) {
    let r = self.protected_ranges.remove(idx);
    let len = r.end - r.start;
    self.text.drain(r.start..r.end);
    self.cursor = r.start;
    self.shift_ranges(r.start, -(len as isize));
}
```

Rewrite `handle_key`:

```rust
pub fn handle_key(&mut self, key: KeyEvent) -> Option<(String, bool)> {
    match key.code {
        KeyCode::Char(c) => {
            // Typing inside a range → delete the range first.
            if let Some(idx) = self.range_at(self.cursor) {
                self.delete_range(idx);
            }
            self.text.insert(self.cursor, c);
            self.shift_ranges(self.cursor + 1, c.len_utf8() as isize);
            // Actually: shift starts at cursor+c.len_utf8() — use raw delta below.
            // Simpler: shift_ranges(self.cursor, c.len_utf8() as isize) but only
            // for ranges strictly AFTER cursor. Our helper already does that.
            self.cursor += c.len_utf8();
            None
        }
        KeyCode::Backspace => {
            if let Some(idx) = self.range_at(self.cursor) {
                self.delete_range(idx);
            } else if let Some(idx) = self.range_ending_at(self.cursor) {
                self.delete_range(idx);
            } else if self.cursor > 0 {
                let prev = self.prev_char_boundary(self.cursor);
                let delta = -((self.cursor - prev) as isize);
                self.text.drain(prev..self.cursor);
                self.shift_ranges(prev, delta);
                self.cursor = prev;
            }
            None
        }
        KeyCode::Delete => {
            if let Some(idx) = self.range_at(self.cursor) {
                self.delete_range(idx);
            } else if let Some(idx) = self.range_starting_at(self.cursor) {
                self.delete_range(idx);
            } else if self.cursor < self.text.len() {
                let next = self.next_char_boundary(self.cursor);
                let delta = -((next - self.cursor) as isize);
                self.text.drain(self.cursor..next);
                self.shift_ranges(self.cursor, delta);
            }
            None
        }
        KeyCode::Left => {
            if let Some(idx) = self.range_at(self.cursor).or_else(|| self.range_ending_at(self.cursor)) {
                let r = &self.protected_ranges[idx];
                self.cursor = r.start;
            } else if self.cursor > 0 {
                self.cursor = self.prev_char_boundary(self.cursor);
            }
            None
        }
        KeyCode::Right => {
            if let Some(idx) = self.range_at(self.cursor).or_else(|| self.range_starting_at(self.cursor)) {
                let r = &self.protected_ranges[idx];
                self.cursor = r.end;
            } else if self.cursor < self.text.len() {
                self.cursor = self.next_char_boundary(self.cursor);
            }
            None
        }
        KeyCode::Home => { self.cursor = 0; None }
        KeyCode::End => { self.cursor = self.text.len(); None }
        KeyCode::Enter => {
            if self.text.is_empty() { return None; }
            let submitted = self.text.clone();
            let interrupt = submitted.starts_with('!');
            let ranges = self.protected_ranges.clone();
            self.submit_capture = Some((submitted.clone(), ranges, interrupt));
            self.clear();
            Some((submitted, interrupt))
        }
        _ => None,
    }
}
```

Fix the subtle shift bug above: when inserting a char, only ranges with `start >= (old cursor)` should shift. Helper `shift_ranges(at, delta)` currently shifts for `start >= at`. Call `shift_ranges(self.cursor, c.len_utf8() as isize)` BEFORE advancing cursor so ranges at or after the insertion point move. Update accordingly:

```rust
KeyCode::Char(c) => {
    if let Some(idx) = self.range_at(self.cursor) {
        self.delete_range(idx);
    }
    let at = self.cursor;
    self.text.insert(at, c);
    self.shift_ranges(at, c.len_utf8() as isize);
    self.cursor = at + c.len_utf8();
    None
}
```

Add `insert_atom`:

```rust
pub fn insert_atom(&mut self, text: impl AsRef<str>, uri: String, name: String) {
    // Deletes any range the cursor is currently inside.
    if let Some(idx) = self.range_at(self.cursor) {
        self.delete_range(idx);
    }
    let at = self.cursor;
    let s = text.as_ref();
    self.text.insert_str(at, s);
    let end = at + s.len();
    self.shift_ranges(at, s.len() as isize);
    self.protected_ranges.push(ProtectedRange {
        start: at,
        end,
        uri,
        name,
    });
    self.protected_ranges.sort_by_key(|r| r.start);
    self.cursor = end;
}
```

Add `take_submit_capture`:

```rust
pub fn take_submit_capture(&mut self) -> Option<(String, Vec<ProtectedRange>, bool)> {
    self.submit_capture.take()
}
```

Update `clear`:

```rust
pub fn clear(&mut self) {
    self.text.clear();
    self.cursor = 0;
    self.protected_ranges.clear();
}
```

Do NOT change `render` in this task. Atom-styling lands in Task 15. The unit tests here are content-based (`.text()`, `.cursor()`, `.protected_ranges()`) and do not depend on visual styling.

- [ ] **Step 4: run.** `cargo test -p spur-tui --test input_bar_protected_ranges` — PASS. Fix any individual test that fails.

- [ ] **Step 5: full workspace + clippy.**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 6: commit.**

```
git add crates/spur-tui/src/components/input_bar.rs \
       crates/spur-tui/tests/input_bar_protected_ranges.rs
git commit -m "feat(spur-tui): InputBar ProtectedRange atom semantics"
```

---

## Task 13: `MentionSource` trait + file/directory source + registry

Background-prime an index of file + directory paths using `ignore`, cache per session, fuzzy-match with `nucleo`.

**Files:**
- Modify: `crates/spur-tui/Cargo.toml` (add `ignore = "0.4"`)
- Create: `crates/spur-tui/src/mentions/mod.rs`
- Create: `crates/spur-tui/src/mentions/entry.rs`
- Create: `crates/spur-tui/src/mentions/file_source.rs`
- Create: `crates/spur-tui/src/mentions/registry.rs`
- Modify: `crates/spur-tui/src/lib.rs` (`pub mod mentions;`)
- Test: `crates/spur-tui/tests/mention_registry.rs`

- [ ] **Step 1: add dep.**

`crates/spur-tui/Cargo.toml`:

```toml
ignore = "0.4"
```

- [ ] **Step 2: entry + trait.**

`crates/spur-tui/src/mentions/entry.rs`:

```rust
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MentionKind { File, Directory }

#[derive(Debug, Clone)]
pub struct MentionEntry {
    pub kind: MentionKind,
    /// File URI, e.g. "file:///abs/src/foo.rs".
    pub uri: String,
    /// Relative path for display (directories end with '/').
    pub display: String,
}

pub trait MentionSource: Send {
    /// Rebuild the candidate list from scratch. Called by the registry when
    /// the cache is cold or expired.
    fn build(&mut self, cwd: &std::path::Path) -> anyhow::Result<Vec<MentionEntry>>;

    /// Human-readable name (for debugging / future source-tagging).
    fn name(&self) -> &'static str;
}

/// Helper: convert an absolute path under cwd into a `MentionEntry`.
pub fn entry_for_path(cwd: &std::path::Path, abs: &std::path::Path) -> Option<MentionEntry> {
    let rel = abs.strip_prefix(cwd).ok()?;
    let rel_str = rel.to_str()?;
    let kind = if abs.is_dir() { MentionKind::Directory } else { MentionKind::File };
    let display = match kind {
        MentionKind::Directory => format!("{}/", rel_str),
        MentionKind::File => rel_str.to_string(),
    };
    let uri = format!("file://{}", abs.to_str()?);
    Some(MentionEntry { kind, uri, display })
}
```

- [ ] **Step 3: file + dir walker.**

`crates/spur-tui/src/mentions/file_source.rs`:

```rust
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};
use super::entry::{MentionEntry, MentionKind, MentionSource, entry_for_path};

/// Single walker source that emits both files AND directories found under
/// `cwd`, honoring `.gitignore` / `.ignore` / `.rgignore`.
pub struct FileMentionSource;

impl MentionSource for FileMentionSource {
    fn name(&self) -> &'static str { "file" }

    fn build(&mut self, cwd: &Path) -> anyhow::Result<Vec<MentionEntry>> {
        let mut out = Vec::new();
        let walker = WalkBuilder::new(cwd)
            .follow_links(false)
            .hidden(true)
            .git_ignore(true)
            .git_exclude(true)
            .ignore(true)
            .build();
        for dent in walker.flatten() {
            let p = dent.path();
            if p == cwd { continue; }
            if let Some(e) = entry_for_path(cwd, p) {
                out.push(e);
            }
        }
        Ok(out)
    }
}
```

- [ ] **Step 4: registry with cache + fuzzy.**

`crates/spur-tui/src/mentions/registry.rs`:

```rust
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use nucleo_matcher::{pattern::{CaseMatching, Normalization, Pattern}, Matcher};
use spur_acp::SessionId;

use super::entry::{MentionEntry, MentionSource};
use super::file_source::FileMentionSource;

const CACHE_TTL: Duration = Duration::from_secs(60);

struct CachedIndex {
    entries: Vec<MentionEntry>,
    built_at: Instant,
}

pub struct MentionRegistry {
    sources: Vec<Box<dyn MentionSource>>,
    cache: HashMap<String, CachedIndex>, // keyed by session id string
}

impl MentionRegistry {
    pub fn new() -> Self {
        Self {
            sources: vec![Box::new(FileMentionSource)],
            cache: HashMap::new(),
        }
    }

    /// Return up to `limit` entries ranked by fuzzy match against `query`.
    pub fn query(
        &mut self,
        session: &SessionId,
        cwd: &std::path::Path,
        query: &str,
        limit: usize,
    ) -> Vec<MentionEntry> {
        let key = session.0.clone();
        let needs_rebuild = match self.cache.get(&key) {
            Some(c) => c.built_at.elapsed() > CACHE_TTL,
            None => true,
        };
        if needs_rebuild {
            let mut all = Vec::new();
            for s in &mut self.sources {
                if let Ok(mut entries) = s.build(cwd) {
                    all.append(&mut entries);
                }
            }
            self.cache.insert(key.clone(), CachedIndex { entries: all, built_at: Instant::now() });
        }
        let entries = &self.cache[&key].entries;
        if query.is_empty() {
            let mut out: Vec<MentionEntry> = entries.iter().take(limit).cloned().collect();
            // Prefer shorter paths first.
            out.sort_by_key(|e| e.display.len());
            return out.into_iter().take(limit).collect();
        }
        let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
        let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, MentionEntry)> = entries
            .iter()
            .filter_map(|e| {
                let score = pattern.score(
                    nucleo_matcher::Utf32Str::new(&e.display, &mut Vec::new()),
                    &mut matcher,
                )?;
                Some((score, e.clone()))
            })
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.display.len().cmp(&b.1.display.len())));
        scored.into_iter().take(limit).map(|(_, e)| e).collect()
    }
}

impl Default for MentionRegistry {
    fn default() -> Self { Self::new() }
}
```

- [ ] **Step 5: `mod.rs` + crate export.**

`crates/spur-tui/src/mentions/mod.rs`:

```rust
pub mod entry;
pub mod file_source;
pub mod registry;

pub use entry::{MentionEntry, MentionKind, MentionSource};
pub use registry::MentionRegistry;
```

In `crates/spur-tui/src/lib.rs`, add `pub mod mentions;`.

- [ ] **Step 6: integration test.**

`crates/spur-tui/tests/mention_registry.rs`:

```rust
use spur_acp::SessionId;
use spur_tui::mentions::MentionRegistry;

#[test]
fn file_mentions_index_and_fuzzy_match() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/foo.rs"), "// foo").unwrap();
    std::fs::write(root.join("src/bar.rs"), "// bar").unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();

    let mut reg = MentionRegistry::new();
    let sid = SessionId::new();
    let hits = reg.query(&sid, root, "foo", 10);
    assert!(hits.iter().any(|h| h.display.contains("foo.rs")), "{:?}", hits);

    // Empty query yields something (shortest paths first)
    let all = reg.query(&sid, root, "", 10);
    assert!(!all.is_empty());
}
```

Add `tempfile = "3"` to `[dev-dependencies]` in `crates/spur-tui/Cargo.toml`.

- [ ] **Step 7: run.** `cargo test -p spur-tui --test mention_registry` — PASS.

- [ ] **Step 8: clippy + commit.**

```
cargo clippy --workspace --all-targets -- -D warnings
git add crates/spur-tui/Cargo.toml crates/spur-tui/src/mentions/ \
       crates/spur-tui/src/lib.rs crates/spur-tui/tests/mention_registry.rs
git commit -m "feat(spur-tui): MentionRegistry with file/dir indexing via ignore + nucleo"
```

---

## Task 14: Wire mentions into `SessionDetailView` popup + submit path

Hook `@`-triggers into the popup; accept inserts `insert_atom`; Enter submits with interleaved content blocks.

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: add `MentionRegistry` + cwd to `SessionDetailView`.**

```rust
mention_registry: crate::mentions::MentionRegistry,
cwd: std::path::PathBuf,
```

Extend `SessionDetailView::new` to take a `cwd: PathBuf` arg. Update callers — search for `SessionDetailView::new` and pass `std::env::current_dir().unwrap_or_default()` if no session-specific cwd is tracked.

- [ ] **Step 2: extend `refresh_popup` for mentions.**

```rust
match trig {
    Some(t) if t.kind == TriggerKind::Slash => { /* as before */ }
    Some(t) if t.kind == TriggerKind::Mention => {
        let hits = self.mention_registry.query(&self.session_id, &self.cwd, &t.query, 20);
        let rows: Vec<PopupRow> = hits
            .iter()
            .map(|m| {
                let icon = match m.kind {
                    crate::mentions::MentionKind::Directory => "📁",
                    crate::mentions::MentionKind::File => "📄",
                };
                PopupRow {
                    label: format!("{} @{}", icon, m.display),
                    description: String::new(),
                    source_tag: String::new(),
                }
            })
            .collect();
        self.completion_popup.set_rows(rows);
        // Remember the raw hits alongside rows so accept can look up the URI.
        self.active_mention_hits = hits;
    }
    _ => { self.completion_popup.set_rows(Vec::new()); self.active_mention_hits.clear(); }
}
```

Add the parallel store:

```rust
active_mention_hits: Vec<crate::mentions::MentionEntry>,
```

- [ ] **Step 3: extend `accept_completion` for mentions.**

```rust
TriggerKind::Mention => {
    let idx = self.completion_popup.selected()?;
    let hit = self.active_mention_hits.get(idx)?.clone();
    // Replace the `@query` token in InputBar with an atom.
    // First clear the existing token:
    self.replace_trigger_token(trig.prefix_start, "");
    // Then insert the atom at prefix_start.
    let current_cursor = self.input_bar.cursor();
    // replace_trigger_token leaves cursor at prefix_start + replacement.len() = prefix_start.
    assert_eq!(current_cursor, trig.prefix_start);
    let atom = format!("@{}", hit.display);
    self.input_bar.insert_atom(atom, hit.uri, hit.display);
    self.active_trigger = None;
    self.completion_popup.set_rows(Vec::new());
    self.active_mention_hits.clear();
    None
}
```

- [ ] **Step 4: Submit path uses `take_submit_capture`.**

Replace the InputBar-returns-text handler in `handle_key`:

```rust
if let Some(_) = self.input_bar.handle_key(key) {
    // Actual text lives in the capture.
    let Some((text, ranges, interrupt)) = self.input_bar.take_submit_capture() else {
        return None;
    };
    use crate::commands::submit_router::{route, SubmitDecision};
    let dec = route(&text, &ranges, &self.command_registry, interrupt);
    return match dec {
        SubmitDecision::Empty => None,
        SubmitDecision::Send { blocks, interrupt } => {
            Some(Action::SendMessage { session: self.session_id.clone(), blocks, interrupt })
        }
        SubmitDecision::Local { action } => Some(action),
        SubmitDecision::KiroExecute { command, args } => {
            Some(Action::KiroExecute { session: self.session_id.clone(), command, args })
        }
    };
}
```

- [ ] **Step 5: integration test end-to-end.**

`crates/spur-tui/tests/session_detail_commands_integration.rs`:

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_tui::action::Action;
use spur_tui::views::session_detail::SessionDetailView;

fn press(v: &mut SessionDetailView, code: KeyCode) -> Option<Action> {
    use spur_tui::views::View;
    v.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn type_str(v: &mut SessionDetailView, s: &str) {
    for c in s.chars() {
        press(v, KeyCode::Char(c));
    }
}

#[test]
fn plain_text_submit_produces_text_block() {
    let tmp = tempfile::tempdir().unwrap();
    let mut v = SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
    );
    type_str(&mut v, "hello");
    let act = press(&mut v, KeyCode::Enter).expect("action");
    match act {
        Action::SendMessage { blocks, interrupt, .. } => {
            assert!(!interrupt);
            assert_eq!(blocks.len(), 1);
            match &blocks[0] {
                spur_acp::ContentBlock::Text(t) => assert_eq!(t.text, "hello"),
                other => panic!("got {:?}", other),
            }
        }
        other => panic!("expected SendMessage, got {:?}", other),
    }
}

#[test]
fn slash_help_fires_show_help_action() {
    let tmp = tempfile::tempdir().unwrap();
    let mut v = SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
    );
    type_str(&mut v, "/");
    // popup is open; Enter should accept the first row (which is /help from spur-local)
    let _ = press(&mut v, KeyCode::Enter); // accept → inserts "/help " into InputBar
    let act = press(&mut v, KeyCode::Enter); // second Enter → submit
    assert!(matches!(act, Some(Action::ShowHelp)));
}
```

Add `tempfile` to `[dev-dependencies]` if not already done in Task 13.

- [ ] **Step 6: run.**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 7: manual smoke test.**

1. Run spur. Type `@`. Popup shows top-N files + dirs.
2. Type `foo`. Popup filters to files containing "foo".
3. Enter. Mention inserted as atom; backspace deletes whole atom.
4. Submit message with a mention; verify the ReactTrace shows the `@path` echo.

- [ ] **Step 8: commit.**

```
git add crates/spur-tui/src/views/session_detail.rs \
       crates/spur-tui/tests/session_detail_commands_integration.rs
git commit -m "feat(spur-tui): @-mention popup + submit produces ResourceLink blocks"
```

---

## Task 15: Render protected ranges with cyan underline in InputBar

The atom-styled rendering is a correctness + UX polish item isolated here to keep Task 12 focused on edit semantics.

**Files:**
- Modify: `crates/spur-tui/src/components/input_bar.rs` (render function)

- [ ] **Step 1: rewrite `render` to style ranges.**

```rust
pub fn render(&self, frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title(Span::styled(" INSERT ", Style::default().fg(Color::Green)));

    let mut spans = Vec::new();
    if let Some(ref status) = self.status {
        spans.push(Span::styled(format!("{} ", status), Style::default().fg(Color::DarkGray)));
    }
    spans.push(Span::raw("> "));

    // Walk text by ranges, inserting cursor glyph at self.cursor.
    let mut pos = 0usize;
    let atom_style = Style::default().fg(Color::Cyan).add_modifier(Modifier::UNDERLINED);
    let plain = Style::default();
    let mut remaining_ranges = self.protected_ranges.iter().peekable();

    while pos < self.text.len() {
        let next_boundary = remaining_ranges
            .peek()
            .map(|r| if pos < r.start { r.start } else { r.end })
            .unwrap_or(self.text.len());
        let chunk_end = next_boundary.min(self.text.len());

        // Split around cursor if it falls in this chunk.
        let mut chunk_start = pos;
        while chunk_start < chunk_end {
            let split = if self.cursor >= chunk_start && self.cursor < chunk_end {
                self.cursor
            } else {
                chunk_end
            };
            let in_range = remaining_ranges.peek().map_or(false, |r| pos >= r.start && pos < r.end);
            let style = if in_range { atom_style } else { plain };
            if split > chunk_start {
                spans.push(Span::styled(self.text[chunk_start..split].to_string(), style));
            }
            if self.cursor == split && split < self.text.len() {
                spans.push(Span::styled("\u{2588}", Style::default().fg(Color::Green)));
            }
            chunk_start = split.max(chunk_start + 1).min(chunk_end);
            if chunk_start == split && split == self.cursor {
                // avoid infinite loop if cursor exactly at split
                break;
            }
        }
        pos = chunk_end;
        if remaining_ranges.peek().map_or(false, |r| r.end == pos) {
            remaining_ranges.next();
        }
    }

    // Cursor at end-of-text case.
    if self.cursor >= self.text.len() {
        spans.push(Span::styled("\u{2588}", Style::default().fg(Color::Green)));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(paragraph, area);
}
```

If the above walker is too intricate to land cleanly in one pass, the fallback acceptable for v1 is:

```rust
// Fallback: ignore atom styling entirely; keep the original render as-is.
// (Task 15 becomes a no-op and mentions appear in default style in v1.)
```

Mark this fallback explicitly in the commit message if chosen.

- [ ] **Step 2: visual smoke test.** Run spur, insert a mention, confirm the atom text renders with cyan underline styling (or confirm the fallback if chosen).

- [ ] **Step 3: run existing test suite (nothing new to add for this styling-only change).**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 4: commit.**

```
git add crates/spur-tui/src/components/input_bar.rs
git commit -m "feat(spur-tui): render @-mention atoms with cyan underline"
```

---

## Task 16: Final polish, docs, and spec-to-plan traceability check

- [ ] **Step 1:** grep the spec for every §N heading; confirm each is covered by at least one task in this plan. The mapping:
  - §1 scope, §2 prior art — no code; covered by design doc.
  - §3 architecture — Tasks 2, 4, 6, 7, 13.
  - §4 data model — Tasks 2, 12, 13.
  - §5 data flow — Tasks 8, 10, 14.
  - §6 key routing — Task 8, 14.
  - §7 collision grammar — Task 4.
  - §8 kiro vendor — Task 11.
  - §9 ProtectedRange — Task 12.
  - §10 indexing — Task 13.
  - §11 popup UI — Task 7.
  - §12 Action/submit pathway — Tasks 9, 10.
  - §13 error handling — Task 11 (error trace), Task 13 (walk errors via `.flatten()`).
  - §14 testing — every task has tests.
  - §15 phasing — this plan tracks §15 ordering with 16 tasks (some split).
  - §16 open questions — §16 documents deliberate choices; no task required.
  - §17 local command set — Task 3.

- [ ] **Step 2:** re-run `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings` one final time.

- [ ] **Step 3:** update `docs/superpowers/specs/2026-04-13-chat-input-commands-mentions-design.md` `Status` frontmatter from `draft` to `implemented` in a final commit.

```
git commit -am "docs(spec): mark chat input commands+mentions as implemented"
```

---

## Risks and watch-outs

- **`View::render` signature.** Task 8 depends on it taking `&mut self`. If it's `&self`, either change the trait or put the popup behind `RefCell`. Verify before starting Task 8.
- **Orchestrator blocking cwd walk.** Task 13 runs the `ignore::Walk` on the UI thread on first `@`. If this is too slow on large monorepos in practice, move to a background task priming on session open. Deferred by design but measure during Task 14 smoke test.
- **Cursor rendering inside atom spans.** Task 15's walker is intricate. The fallback (no atom styling) is acceptable and does not block any test.
- **Kiro `availableCommands` wire format.** Task 11 assumes kiro's notification params use camelCase `availableCommands`. If kiro uses a different name, adjust the parser. Check an actual kiro ACP log before committing.
- **`ResourceLink` re-export path.** Task 10 uses `spur_acp::types::ResourceLink` or `spur_acp::ResourceLink`; confirm the re-export exists after Task 1 or 10 and update imports accordingly.
- **`SessionDetailView::new` signature change in Task 14.** Adding `cwd: PathBuf` breaks all call sites; grep and fix at the same time.

# SPUR TUI Interactive Loop

**Date:** 2026-04-12
**Status:** Approved
**Scope:** spur-core (orchestrator), spur-tui (app, views, components), spur-cli (main.rs)
**Prerequisite:** TUI Redesign spec (2026-04-12-spur-tui-redesign.md) — already implemented
**PRD Reference:** SPUR_PRD.md sections F1.3 (TUI Dashboard), F1.4 (Ad-hoc Task Execution)

---

## Problem Statement

The current TUI is a passive event viewer. Two root causes prevent interactive orchestration:

1. **Disconnected feedback loop.** The `user_input_tx` channel exists but is never consumed. The orchestrator's `run_adhoc()` sends one prompt, streams the response, and shuts down. No multi-turn conversation.

2. **Ephemeral session state.** `SessionDetailView` is destroyed on `NavigateBack` and recreated fresh on drill-in. Events received while on the Dashboard are lost for the Session Detail view.

## Design Decisions

1. **`spur watch` as primary interactive cockpit.** The TUI starts the orchestrator in-process. Users type tasks directly into the TUI. `spur run` remains for batch/CI use. No IPC daemon (Phase 3).

2. **Single brain session.** One brain at a time, persistent for the TUI lifecycle. The brain delegates to multiple workers concurrently. Multiple brains would undermine SPUR's coordination value by recreating the "multiple terminal tabs" problem inside the TUI.

3. **Session lifecycle = TUI lifecycle.** Brain ACP session stays alive until the user quits `spur watch`. Multi-turn: user sends message, brain responds, user sends another. Full conversation context maintained by the agent.

4. **Approach 1 (Minimal Wiring).** Additive changes only. No View trait refactoring, no model layer extraction. ~270 lines across 7 files.

---

## Architecture

### Interactive Orchestrator Loop

New `Orchestrator::run_interactive()` method. Replaces `run_adhoc()` as the entry point for `spur watch`.

**State machine:**

```
Idle ──msg──> Spawning ──ok──> Connected ──msg──> Streaming
                 |                  |                |  |
                 v                  v                v  | !interrupt
               Error <--------------+----------------'  v
                 |                                  Cancelling ──5s──> break
                 '──next msg──> Spawning (re-spawn)
```

- **Idle:** No brain. Waiting for first user message.
- **Spawning:** `spawn_brain_session()` in progress. Initializes ACP, creates MCP server, emits `BrainSpawned`.
- **Connected:** Brain alive, waiting for user input.
- **Streaming:** Brain producing output from a prompt. User messages queued.
- **Cancelling:** `cancel()` sent, waiting for stream to end (5s timeout).
- **Error:** Connection failed or brain crashed. Next user message triggers re-spawn.

**Two-phase loop:**

```rust
pub async fn run_interactive(
    &mut self,
    mut user_input_rx: mpsc::Receiver<UserInput>,
) -> Result<()> {
    let mut brain: Option<BrainSession> = None;
    let mut pending_messages: VecDeque<String> = VecDeque::new();

    loop {
        // Phase 2: Get next message (from queue or user)
        let text = if let Some(msg) = pending_messages.pop_front() {
            msg
        } else {
            match user_input_rx.recv().await {
                Some(input) => input.text,
                None => break, // TUI closed
            }
        };

        // Lazy-spawn brain on first message (or after crash)
        if brain.is_none() {
            match self.spawn_brain_session().await {
                Ok(b) => brain = Some(b),
                Err(e) => {
                    self.emit(SpurEvent::BrainError {
                        session: SessionId::new(),
                        message: e.to_string(),
                    });
                    continue;
                }
            }
        }
        let b = brain.as_mut().unwrap();

        // Send prompt
        let mut stream = match b.connection.prompt(...).await {
            Ok(s) => s,
            Err(e) => {
                self.emit(SpurEvent::BrainError { ... });
                brain = None; // re-spawn on next message
                continue;
            }
        };

        // Phase 1: Stream output + check for interrupts
        let mut cancel_deadline: Option<Instant> = None;
        loop {
            tokio::select! {
                item = stream.next() => {
                    match item {
                        Some(n) => self.emit(notification_to_spur_event(&n)),
                        None => break, // Turn complete
                    }
                }
                Some(input) = user_input_rx.recv() => {
                    if input.interrupt {
                        let _ = b.connection.cancel(&b.session_id).await;
                        cancel_deadline = Some(Instant::now() + Duration::from_secs(5));
                    }
                    pending_messages.push_back(input.text);
                }
                _ = async { /* cancel timeout */ } => {
                    break; // Force end stream
                }
            }
        }

        self.emit(SpurEvent::TurnComplete {
            session: b.spur_session_id.clone(),
        });
    }

    // Cleanup
    if let Some(mut b) = brain.take() {
        let _ = b.connection.shutdown().await;
    }
    Ok(())
}
```

**Key behaviors:**
- Lazy spawn: brain created on first message, not on TUI start
- Crash recovery: on error, `brain = None`; next message re-spawns fresh
- Message queueing: non-interrupt messages during streaming go to `VecDeque`, concatenated into next prompt
- Cancel timeout: 5s after `cancel()`, force-end the stream for misbehaving agents
- Delegation: handled by existing `handle_delegations()` task (spawned in `spawn_brain_session`)

### Shared Helper: spawn_brain_session()

Extracted from `run_adhoc()` to eliminate duplication. Both methods use it.

```rust
struct BrainSession {
    connection: Box<dyn AgentConnection>,
    session_id: String,           // ACP session ID
    spur_session_id: SessionId,   // SPUR session ID
    mcp_server: Arc<McpCallbackServer>,
    delegation_handle: JoinHandle<()>,
}

async fn spawn_brain_session(&mut self) -> Result<BrainSession> {
    // 1. Resolve brain agent from config
    // 2. Create connection (NativeAcp/Stdio/CliWrap)
    // 3. Initialize ACP
    // 4. Create + start MCP callback server with workers
    // 5. Create ACP session (new_session with cwd + MCP servers)
    // 6. Spawn delegation handler task
    // 7. Emit BrainSpawned
    // 8. Log session start (cost tracker)
}
```

### CLI Wiring: spur watch

```rust
// main.rs watch command:
let mut orchestrator = Orchestrator::new(repo_root, config)?;
let event_rx = orchestrator.subscribe();
let (user_tx, user_rx) = mpsc::channel(32);

// Spawn interactive orchestrator (moves ownership)
let orch_handle = tokio::spawn(async move {
    orchestrator.run_interactive(user_rx).await
});

// Run TUI (blocks main task)
run_tui(event_rx, Some(user_tx)).await?;

// After TUI exits, abort orchestrator
orch_handle.abort();
```

Add `--brain <name>` flag to `spur watch` for brain override (default from config).

---

## TUI Changes

### Dashboard InputBar

Add `InputBar` component to `DashboardView`. Appears at the bottom, above status bar.

```
+-- Agents ----------------------------+
| (agents tree or empty state)         |
+-- Activity --------------------------+
| (activity log)                       |
|                                      |
+--------------------------------------+
| [kiro: ready] > fix the auth bug_   |  <-- InputBar
+--------------------------------------+
| [keybindings]          [$0.00] [SPUR]|  <-- status bar
+--------------------------------------+
```

**Keyboard routing:** Same pattern as SessionDetailView. When InputBar is empty, j/k/g/G/q/v/?/Tab work as navigation. When InputBar has text, all characters go to InputBar.

**On Enter (with text):**
- Emit `Action::SendMessage` with placeholder `SessionId` (orchestrator ignores it in single-brain mode)
- App sets `brain_status = Thinking`

**On Enter (empty, agents panel focused):**
- Emit `Action::NavigateTo(SessionDetail)` — drill into brain session

### SessionDetailView Lifecycle

**Creation:** App creates SessionDetailView when `BrainSpawned` event arrives. Always replaces any existing view (handles brain re-spawn after crash).

```rust
SpurEvent::BrainSpawned { agent, session } => {
    let mut detail = SessionDetailView::new(session, agent, "brain");
    detail.set_brain_status(BrainStatus::Thinking);
    self.session_detail = Some(detail);
    // Auto-navigate from Dashboard
    if matches!(self.current_view, ViewId::Dashboard) {
        self.current_view = ViewId::SessionDetail(session);
    }
}
```

**Preservation:** `NavigateBack` sets `current_view = Dashboard` but does NOT set `session_detail = None`. The view continues receiving and accumulating events while hidden.

**Navigation:** `NavigateTo(SessionDetail)` just switches the view — never recreates. BrainSpawned is the only creator.

**User messages in trace:** When SessionDetailView handles a `SendMessage` action, it immediately adds a `YOU` entry to the ReAct trace (before the orchestrator processes it). Provides instant visual feedback.

### BrainStatus

Tracked in `App`, pushed to both views via `set_brain_status()`.

```rust
pub enum BrainStatus {
    Idle,               // no brain spawned
    Thinking,           // prompt sent, waiting for first output
    Streaming,          // receiving agent output
    Ready,              // turn complete, waiting for input
    Error(String),      // brain crashed/failed
}
```

**InputBar rendering:**

| Status | InputBar display |
|---|---|
| Idle | `> _` |
| Thinking | `[kiro ··· ] > _` |
| Streaming | `[kiro ▸▸▸ ] > _` |
| Ready | `[kiro: ready] > _` |
| Error | `[kiro: error] > _` |

**State transitions:**

| Event | New status |
|---|---|
| `SendMessage` action (when Ready/Idle) | Thinking |
| First `AgentOutput` after Thinking | Streaming |
| `TurnComplete` | Ready |
| `BrainSpawned` | Thinking |
| `BrainError` | Error |

### New SpurEvent Variants

```rust
SpurEvent::TurnComplete {
    session: SessionId,
}

SpurEvent::BrainError {
    session: SessionId,
    message: String,
}
```

- `TurnComplete`: emitted by `run_interactive()` when a brain turn ends (stream returns None). Distinct from `SessionCompleted` (which means the entire session is done).
- `BrainError`: emitted when `prompt()` fails or subprocess dies. Distinct from `AgentOutput(SessionEvent::Error)` which is an error reported BY the agent, not a connection failure.

---

## Change Summary

| File | Change | ~Lines |
|---|---|---|
| `orchestrator.rs` | `run_interactive()` + `spawn_brain_session()` helper (refactored from `run_adhoc`) | 120 |
| `main.rs` | Wire watch command: create channels, spawn orchestrator task, add `--brain` flag | 30 |
| `app.rs` | `BrainStatus` enum, `set_brain_status` on views, auto-create SessionDetailView on BrainSpawned, keep alive on NavigateBack | 40 |
| `dashboard.rs` | Add InputBar, wire keyboard routing and SendMessage | 40 |
| `input_bar.rs` | Optional status label rendering | 15 |
| `session_detail.rs` | `set_brain_status`, add YOU entry on SendMessage | 15 |
| `domain/events.rs` | `TurnComplete`, `BrainError` variants | 10 |
| **Total** | | **~270** |

---

## What This Does NOT Change

- `run_adhoc()` — batch CLI mode unchanged. `spur run` works as before.
- View trait signature — no changes to `fn render(&self, frame, area)`.
- Agent connection layer — NativeAcpConnection, StdioAdapter, CliWrapAdapter unchanged.
- MCP tools — delegation, filesystem tools unchanged.
- Dashboard rendering — agents tree, activity log, status bar unchanged.
- Session Detail rendering — ReAct trace, help overlay unchanged.

## Deferred to Future Phases

- **Phase 2:** Session Model extraction (shared model for 3+ views), command mode (`:brain`, `:cancel`), inline diff viewer, cost detail view
- **Phase 3:** Daemon mode (IPC), attach/detach, session persistence across TUI restarts
- **Phase 4:** Multi-user visibility, team dashboard

---

## Key Design Rationale

1. **Single brain, not multiple.** Multiple brains undermine SPUR's coordination value. One coordinator prevents conflicts and leverages all agents through delegation.

2. **Additive changes only.** No View trait refactoring, no model layer. YAGNI — add the model layer when Phase 2 views (Cost, Issues) require it.

3. **Lazy spawn + crash recovery.** Brain spawned on first message. On crash, connection set to None; next message re-spawns. Zero additional recovery code — the lazy-spawn pattern handles it naturally.

4. **Both views have InputBar.** Dashboard InputBar enables lightweight interaction without drilling in. Session Detail InputBar enables intervention during deep inspection. Same channel, same orchestrator — consistent behavior.

5. **Auto-navigate on BrainSpawned.** User types task on Dashboard, brain spawns, TUI switches to Session Detail showing live output. Smooth transition with no timing issues (event-driven, not action-driven).

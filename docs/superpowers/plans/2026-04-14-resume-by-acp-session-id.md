# Resume by ACP Session Id — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `spur watch` auto-resume the actual prior agent-side conversation by persisting and replaying the agent-authoritative ACP session id (not the ephemeral SPUR id), and pin the native-connection error-propagation fix with a regression test.

**Architecture:** New `SpurEventBody::AgentSessionReady { session, acp_session_id, brain, resumed }` emitted by the orchestrator after every successful brain session establishment (fresh or resumed). The TUI's `SessionMetadataStore` persists `(spur_id → acp_id, brain_name)` per entry and maintains top-level `last_active_acp_session_id` + `last_active_brain` pointers. `spur-cli watch` reads those pointers at startup and sends the ACP id via `UserInput::ResumeSession`.

**Tech Stack:** Rust, tokio, serde, tokio broadcast channel, ratatui (indirectly).

---

## File Structure

- `crates/spur-acp/src/domain/events.rs` — add `AgentSessionReady` variant to `SpurEventBody`.
- `crates/spur-acp/src/connection/native.rs` — already fixed in a prior commit; add a regression test only.
- `crates/spur-acp/tests/load_session_error_propagation.rs` — NEW: regression test for the Fix #1 behavior.
- `crates/spur-core/src/orchestrator.rs` — emit `AgentSessionReady` from `create_brain_session` and `load_brain_session`.
- `crates/spur-tui/src/session_metadata.rs` — extend `SessionEntry` and `SessionMetadata`; add `set_acp_mapping` + `last_active_acp()` accessors.
- `crates/spur-tui/src/app.rs` — handle `AgentSessionReady` in `handle_spur_event`; update top-level pointers.
- `crates/spur-tui/src/views/session_detail.rs` — show a one-line "Resumed from prior conversation" note when `resumed == true`.
- `crates/spur-cli/src/main.rs` — read `last_active_acp_session_id`/`last_active_brain`, pass the ACP id to `ResumeSession`.

---

## Task 1: Regression test for Fix #1 (error propagation on `session/load`)

**Files:**
- Create: `crates/spur-acp/tests/load_session_error_propagation.rs`

**Why:** The hard-coded `Ok(rx)` was silently discarding agent errors. Pin the corrected behavior so a future refactor cannot re-break it.

**Approach:** Use a stub-agent subprocess (tiny Node script) that replies `-32002 Resource not found` to any `session/load` request. Drive `NativeAcpConnection` against it via the registered-agent path and assert `load_session()` returns `Err`.

- [ ] **Step 1: Write the stub-agent script**

Create `crates/spur-acp/tests/fixtures/load_error_stub.mjs` with the following content. It responds to `initialize` with a minimal capabilities payload, and returns `-32002` for any `session/load`:

```javascript
#!/usr/bin/env node
// Stub ACP agent that errors on session/load — used by
// load_session_error_propagation regression test.

process.stdin.setEncoding("utf8");
let buffer = "";

process.stdin.on("data", (chunk) => {
  buffer += chunk;
  let nl;
  while ((nl = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, nl).trim();
    buffer = buffer.slice(nl + 1);
    if (!line) continue;
    let req;
    try { req = JSON.parse(line); } catch { continue; }
    if (req.method === "initialize") {
      process.stdout.write(JSON.stringify({
        jsonrpc: "2.0",
        id: req.id,
        result: {
          protocolVersion: 1,
          agentCapabilities: { loadSession: true, promptCapabilities: {} },
          authMethods: [],
        },
      }) + "\n");
    } else if (req.method === "session/load") {
      process.stdout.write(JSON.stringify({
        jsonrpc: "2.0",
        id: req.id,
        error: {
          code: -32002,
          message: "Resource not found: " + (req.params?.sessionId ?? ""),
          data: { uri: req.params?.sessionId ?? "" },
        },
      }) + "\n");
    }
    // Ignore everything else.
  }
});
```

Make it executable:
```bash
chmod +x crates/spur-acp/tests/fixtures/load_error_stub.mjs
```

- [ ] **Step 2: Write the failing regression test**

Create `crates/spur-acp/tests/load_session_error_propagation.rs`:

```rust
//! Regression test: `NativeAcpConnection::load_session` MUST propagate
//! agent-side errors. A prior bug replied `Ok(rx)` before awaiting the
//! upstream RPC, silently swallowing `-32002 Resource not found` and
//! causing downstream `session/prompt` calls to fire against dead ids.

use agent_client_protocol::ProtocolVersion;
use spur_acp::{
    connection::native::NativeAcpConnection,
    AgentConnection, InitializeRequest, LoadSessionRequest,
};

#[tokio::test(flavor = "multi_thread")]
async fn load_session_propagates_agent_error() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let stub = format!("{manifest_dir}/tests/fixtures/load_error_stub.mjs");

    // NativeAcpConnection::new(agent_name, command, extra_args, permission_tx)
    let mut conn = NativeAcpConnection::new(
        "load-error-stub",
        "node",
        vec![stub],
        None,
    );

    conn.initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await
        .expect("initialize should succeed against stub");

    let req = LoadSessionRequest::new(
        "nonexistent-uuid".to_string(),
        std::env::current_dir().unwrap(),
    );

    let result = conn.load_session(req).await;
    assert!(
        result.is_err(),
        "load_session MUST return Err when agent replies with -32002; \
         got Ok — error propagation regressed"
    );

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.to_lowercase().contains("resource not found")
            || err_msg.to_lowercase().contains("load_session failed"),
        "error message should mention the upstream failure; got: {err_msg}"
    );

    let _ = conn.shutdown().await;
}
```

If any of the imported symbols (`AgentConnection`, `InitializeRequest`, `LoadSessionRequest`, `NativeAcpConnection`) are not re-exported at the crate root, adjust the `use` paths — check `crates/spur-acp/src/lib.rs` for the actual public surface. `agent_client_protocol` is already a dependency of `spur-acp`.

- [ ] **Step 3: Run the test to confirm it passes (Fix #1 is already landed)**

Run: `cargo test -p spur-acp --test load_session_error_propagation -- --nocapture`
Expected: PASS. (If it fails, Fix #1 was reverted — restore it before continuing.)

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/tests/fixtures/load_error_stub.mjs \
        crates/spur-acp/tests/load_session_error_propagation.rs
git commit -m "test(spur-acp): regression test for load_session error propagation"
```

---

## Task 2: Add `AgentSessionReady` event variant

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs`

- [ ] **Step 1: Add the variant**

In `crates/spur-acp/src/domain/events.rs`, after the existing `BrainSpawned` line (97), add:

```rust
    /// Emitted AFTER a brain session is established (fresh or resumed) and
    /// the agent-authoritative ACP session id is known. The TUI persists
    /// the (spur_id → acp_id, brain) mapping so the next `spur watch` run
    /// can resume by the real ACP id.
    ///
    /// - `session`: the SPUR session id (matches the earlier `BrainSpawned`).
    /// - `acp_session_id`: the id the agent assigned (stable across runs
    ///   where the agent supports `session/load`).
    /// - `brain`: the brain agent name that owns this ACP id.
    /// - `resumed`: `true` iff `session/load` succeeded. `false` when the
    ///   path fell back to `new_session` or spawned fresh.
    AgentSessionReady {
        session: SessionId,
        acp_session_id: String,
        brain: String,
        resumed: bool,
    },
```

- [ ] **Step 2: Verify the crate compiles**

Run: `cargo check -p spur-acp`
Expected: compiles clean.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-acp/src/domain/events.rs
git commit -m "feat(spur-acp): add AgentSessionReady event for ACP id discovery"
```

---

## Task 3: Emit `AgentSessionReady` from `create_brain_session`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Locate the successful-spawn site**

Open `crates/spur-core/src/orchestrator.rs`. Find `create_brain_session` (starts around line 984). It constructs a `BrainSession` near the end of the function and returns `Ok(brain_session)`.

- [ ] **Step 2: Add the emit immediately before the function returns**

Right before the final `Ok(brain_session)` in `create_brain_session`, add:

```rust
self.emit(SpurEvent::now(SpurEventBody::AgentSessionReady {
    session: brain_session.spur_session_id.clone(),
    acp_session_id: brain_session.acp_session_id.clone(),
    brain: brain_session.brain_name.clone(),
    resumed: false,
}));
```

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-core`
Expected: compiles clean.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): emit AgentSessionReady from create_brain_session"
```

---

## Task 4: Emit `AgentSessionReady` from `load_brain_session`

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs`

- [ ] **Step 1: Identify the outcome flag and the emit site**

In `load_brain_session` (starts ~line 1088), the match on `connection.load_session(...)` produces `(final_acp_session_id, history_stream)`. The `Ok` arm means `session/load` succeeded (resumed); the `Err` arm means we fell back to `new_session` (not resumed).

- [ ] **Step 2: Track the `resumed` flag**

Modify the match so the outcome is recorded:

```rust
let (final_acp_session_id, history_stream, resumed) = match connection
    .load_session(
        LoadSessionRequest::new(acp_session_id.clone(), self.repo_root.clone())
            .mcp_servers(mcp_servers.clone()),
    )
    .await
{
    Ok(stream) => {
        debug!(brain = %brain_name, "load_session succeeded");
        (acp_session_id, Some(stream), true)
    }
    Err(e) => {
        warn!(brain = %brain_name, error = %e, "load_session failed, falling back to new_session");
        let session_response = connection
            .new_session(self.repo_root.clone(), mcp_servers)
            .await
            .context("Failed to create fallback session after load_session failure")?;
        (session_response.session_id.to_string(), None, false)
    }
};
```

- [ ] **Step 3: Emit the event before constructing `BrainSession`**

Right before the `let brain_session = BrainSession { ... }` block, add:

```rust
self.emit(SpurEvent::now(SpurEventBody::AgentSessionReady {
    session: session_id.clone(),
    acp_session_id: final_acp_session_id.clone(),
    brain: brain_name.clone(),
    resumed,
}));
```

- [ ] **Step 4: Verify compile**

Run: `cargo check -p spur-core`
Expected: compiles clean.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(spur-core): emit AgentSessionReady from load_brain_session"
```

---

## Task 5: Extend `SessionEntry` and `SessionMetadata` schema

**Files:**
- Modify: `crates/spur-tui/src/session_metadata.rs`

- [ ] **Step 1: Add fields to `SessionEntry`**

In `crates/spur-tui/src/session_metadata.rs`, replace the `SessionEntry` struct (lines 17–29) with:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionEntry {
    #[serde(default)]
    pub title_override: Option<String>,
    #[serde(default)]
    pub last_opened_at: String,
    #[serde(default)]
    pub draft: String,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    /// Agent-authoritative ACP session id. `None` for entries written
    /// before this field was introduced (migrated silently via serde default).
    #[serde(default)]
    pub acp_session_id: Option<String>,
    /// Brain agent that owns `acp_session_id`. Used at resume time to
    /// avoid sending an ACP id to a different agent.
    #[serde(default)]
    pub brain_name: Option<String>,
}
```

- [ ] **Step 2: Add fields to `SessionMetadata`**

Replace `SessionMetadata` (lines 31–41) with:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionMetadata {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub last_active_session_id: Option<String>,
    #[serde(default)]
    pub last_active_at: Option<String>,
    /// Mirror of the most recent `AgentSessionReady.acp_session_id`.
    /// Passed to `UserInput::ResumeSession` at next launch.
    #[serde(default)]
    pub last_active_acp_session_id: Option<String>,
    /// Mirror of the most recent `AgentSessionReady.brain`. Used to
    /// skip auto-resume when the launch-time `--brain` override does
    /// not match (avoids sending a claude id to kiro, etc.).
    #[serde(default)]
    pub last_active_brain: Option<String>,
    #[serde(default)]
    pub sessions: BTreeMap<String, SessionEntry>,
}
```

- [ ] **Step 3: Verify compile and existing tests still pass**

Run: `cargo test -p spur-tui session_metadata -- --nocapture`
Expected: existing tests PASS. New fields should be `None` by default and silently present after roundtrip.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/session_metadata.rs
git commit -m "feat(spur-tui): add acp_session_id + brain_name to metadata schema"
```

---

## Task 6: Add `set_acp_mapping` and `last_active_acp()` accessors

**Files:**
- Modify: `crates/spur-tui/src/session_metadata.rs`
- Test: inline in the same file (module `#[cfg(test)]`)

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-tui/src/session_metadata.rs`:

```rust
#[cfg(test)]
mod acp_mapping_tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn set_acp_mapping_populates_entry_and_top_level() {
        let tmp = NamedTempFile::new().unwrap();
        let mut store = SessionMetadataStore::load(tmp.path());

        store.set_acp_mapping("spur-abc", "acp-xyz", "claude-code-acp");

        let entry = store.entry("spur-abc").expect("entry created");
        assert_eq!(entry.acp_session_id.as_deref(), Some("acp-xyz"));
        assert_eq!(entry.brain_name.as_deref(), Some("claude-code-acp"));

        let (acp, brain) = store.last_active_acp().expect("top-level populated");
        assert_eq!(acp, "acp-xyz");
        assert_eq!(brain, "claude-code-acp");
    }

    #[test]
    fn last_active_acp_returns_none_when_absent() {
        let tmp = NamedTempFile::new().unwrap();
        let store = SessionMetadataStore::load(tmp.path());
        assert!(store.last_active_acp().is_none());
    }

    #[test]
    fn roundtrip_preserves_acp_mapping() {
        let tmp = NamedTempFile::new().unwrap();
        {
            let mut store = SessionMetadataStore::load(tmp.path());
            store.set_acp_mapping("spur-1", "acp-1", "brain-a");
            store.save().unwrap();
        }
        let reloaded = SessionMetadataStore::load(tmp.path());
        assert_eq!(
            reloaded.entry("spur-1").and_then(|e| e.acp_session_id.clone()),
            Some("acp-1".into())
        );
        assert_eq!(
            reloaded.metadata().last_active_acp_session_id.as_deref(),
            Some("acp-1")
        );
        assert_eq!(
            reloaded.metadata().last_active_brain.as_deref(),
            Some("brain-a")
        );
    }
}
```

`tempfile` is already a dev-dependency of `spur-tui` — no Cargo.toml change needed.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p spur-tui acp_mapping_tests -- --nocapture`
Expected: FAIL with "no method named `set_acp_mapping`" and "no method named `last_active_acp`".

- [ ] **Step 3: Implement the accessors**

Add to the `impl SessionMetadataStore` block (after `clear_last_active`, before `gc_orphans`):

```rust
/// Persist the `(spur_id → acp_id, brain)` mapping on the per-entry
/// record AND unconditionally promote to the top-level `last_active_*`
/// pointers. See design doc: `AgentSessionReady` is the "newest live
/// target to resume" signal, so mirroring is always correct.
pub fn set_acp_mapping(&mut self, spur_id: &str, acp_id: &str, brain: &str) {
    let entry = self
        .metadata
        .sessions
        .entry(spur_id.to_string())
        .or_default();
    entry.acp_session_id = Some(acp_id.to_string());
    entry.brain_name = Some(brain.to_string());

    self.metadata.last_active_session_id = Some(spur_id.to_string());
    self.metadata.last_active_acp_session_id = Some(acp_id.to_string());
    self.metadata.last_active_brain = Some(brain.to_string());
}

/// Return the top-level `(acp_session_id, brain_name)` pair if both
/// are populated. Used by `spur-cli watch` at startup.
pub fn last_active_acp(&self) -> Option<(String, String)> {
    let acp = self.metadata.last_active_acp_session_id.clone()?;
    let brain = self.metadata.last_active_brain.clone()?;
    Some((acp, brain))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p spur-tui acp_mapping_tests -- --nocapture`
Expected: PASS (3 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/session_metadata.rs crates/spur-tui/Cargo.toml
git commit -m "feat(spur-tui): add set_acp_mapping + last_active_acp accessors"
```

---

## Task 7: Handle `AgentSessionReady` in the TUI App

**Files:**
- Modify: `crates/spur-tui/src/app.rs`

- [ ] **Step 1: Find the event-handling match**

Open `crates/spur-tui/src/app.rs`. Inside `handle_spur_event`, locate the `match &event.body` arm (around line 354 — where `BrainSpawned`, `AgentNotification`, `TurnComplete`, `BrainError` are handled).

- [ ] **Step 2: Add the new arm**

Inside that match, adjacent to `BrainSpawned`, add:

```rust
SpurEventBody::AgentSessionReady {
    session,
    acp_session_id,
    brain,
    resumed: _,
} => {
    self.metadata_store
        .set_acp_mapping(&session.0, acp_session_id, brain);
    if let Err(e) = self.metadata_store.save() {
        tracing::warn!(
            error = %e,
            session = %session.0,
            "failed to persist AgentSessionReady metadata"
        );
    }
}
```

(The `resumed` flag is consumed by `SessionDetailView` in Task 8 via the "Forward to views" block that already runs after this match.)

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-tui`
Expected: compiles clean.

- [ ] **Step 4: Smoke-test metadata is written**

Add a lightweight integration test in `crates/spur-tui/src/app.rs` under `#[cfg(test)]`, or extend an existing test that drives `handle_spur_event`. If no such harness exists, skip to Step 5 — the end-to-end test in Task 10 will cover this.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-tui/src/app.rs
git commit -m "feat(spur-tui): persist ACP mapping on AgentSessionReady"
```

---

## Task 8: Show "Resumed from prior conversation" in `SessionDetailView`

**Files:**
- Modify: `crates/spur-tui/src/views/session_detail.rs`

- [ ] **Step 1: Find the existing event handler**

Open `crates/spur-tui/src/views/session_detail.rs`. Locate the `fn handle_spur_event` method (search for `handle_spur_event`). It already matches several `SpurEventBody` variants.

- [ ] **Step 2: Add handling for `AgentSessionReady`**

Inside the match on `SpurEventBody`, add an arm:

```rust
SpurEventBody::AgentSessionReady {
    session,
    resumed,
    ..
} => {
    if session.0 != self.session_id.0 {
        return;
    }
    if *resumed {
        self.push_system_note(
            "Resumed from prior conversation".to_string(),
        );
    }
}
```

`push_system_note` is defined at `crates/spur-tui/src/views/session_detail.rs:299` (`pub fn push_system_note(&mut self, msg: impl Into<String>)`). No hedging needed — use it directly.

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-tui`
Expected: compiles clean.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/src/views/session_detail.rs
git commit -m "feat(spur-tui): show resume note on AgentSessionReady resumed=true"
```

---

## Task 9: Wire `spur-cli watch` to resume by ACP id

**Files:**
- Modify: `crates/spur-cli/src/main.rs`

- [ ] **Step 1: Locate the current resume block**

Open `crates/spur-cli/src/main.rs` inside `Commands::Watch { brain, sessions, dashboard }`. Find the block (around line 431–455):

```rust
let force_picker = sessions && !dashboard;
let auto_resume_id = if dashboard || sessions {
    None
} else {
    meta.metadata().last_active_session_id.clone()
};

if let Some(sid) = auto_resume_id {
    let resume_tx = tui_tx.clone();
    tokio::spawn(async move {
        let _ = resume_tx
            .send(spur_tui::UserInput::ResumeSession { session_id: sid })
            .await;
    });
}
```

- [ ] **Step 2: Replace with ACP-id-based resume**

Replace the block above with:

```rust
let force_picker = sessions && !dashboard;

// Auto-resume is driven by the ACP session id (the agent-authoritative
// id), not the SPUR in-process id. We also gate on the stored brain
// matching the launch-time `--brain` override to avoid handing a
// claude-owned session id to kiro (and vice versa).
let auto_resume: Option<(String, String)> = if dashboard || sessions {
    None
} else {
    match meta.metadata().last_active_acp() {
        Some((acp, stored_brain)) => match brain.as_deref() {
            Some(requested) if requested != stored_brain => {
                tracing::info!(
                    requested = requested,
                    stored = %stored_brain,
                    "auto-resume skipped: brain override mismatches stored brain"
                );
                None
            }
            _ => Some((acp, stored_brain)),
        },
        None => None,
    }
};

if let Some((acp_id, _stored_brain)) = auto_resume {
    let resume_tx = tui_tx.clone();
    tokio::spawn(async move {
        let _ = resume_tx
            .send(spur_tui::UserInput::ResumeSession { session_id: acp_id })
            .await;
    });
}
```

Note: `meta.metadata().last_active_acp()` accessor does not exist on `&SessionMetadata` — it lives on `SessionMetadataStore`. Use `meta.last_active_acp()` (call on the store, not on `metadata()`).

- [ ] **Step 3: Verify compile**

Run: `cargo check -p spur-cli`
Expected: compiles clean.

- [ ] **Step 4: Manual smoke test**

Delete or back up `.spur/session_metadata.json` to ensure a clean start:
```bash
mv .spur/session_metadata.json .spur/session_metadata.json.bak 2>/dev/null || true
```

Run:
```bash
cargo run -p spur-cli -- watch --brain claude-code-acp
```

Expected behavior:
- Dashboard appears.
- Type `hello` and press Enter. A session spawns and attaches — input is echoed and response streams.
- Quit with `Ctrl+C` after the first TurnComplete.
- Re-run the same command.
- The TUI auto-resumes the prior ACP session: `claude-code-acp` logs in `.spur/logs/` should show a successful `session/load` (no `-32002`), and the conversation history should appear.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-cli/src/main.rs
git commit -m "feat(spur-cli): auto-resume by ACP session id, gated on brain match"
```

---

## Task 10: End-to-end resume path test

**Files:**
- Create: `crates/spur-tui/tests/resume_by_acp_e2e.rs`

**Purpose:** Lock in the full event-to-metadata-to-resume flow. Use the existing test harness pattern for `App` (check `crates/spur-tui/tests/*.rs` for examples of event injection).

- [ ] **Step 1: Survey existing TUI test harness**

Run: `ls crates/spur-tui/tests/`
Identify a test file that constructs an `App`, feeds `SpurEvent`s, and asserts on state. Copy its setup boilerplate.

- [ ] **Step 2: Write the test**

Create `crates/spur-tui/tests/resume_by_acp_e2e.rs`:

```rust
//! End-to-end: AgentSessionReady → metadata updated → re-load store →
//! last_active_acp() returns the ACP id that resume should send.

use spur_acp::{SessionId, SpurEvent, SpurEventBody};
use spur_tui::session_metadata::SessionMetadataStore;
// Use whatever App-construction helper existing tests use. If none
// exists, drive SessionMetadataStore directly — the event handler
// in app.rs is a thin wrapper around set_acp_mapping + save.

#[test]
fn agent_session_ready_persists_acp_mapping_across_reload() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();

    // Simulate what app.rs does on AgentSessionReady.
    {
        let mut store = SessionMetadataStore::load(&path);
        store.set_acp_mapping(
            "spur-session-1",
            "acp-session-xyz",
            "claude-code-acp",
        );
        store.save().unwrap();
    }

    // Simulate a fresh process reading the same file.
    let reloaded = SessionMetadataStore::load(&path);
    let (acp, brain) = reloaded
        .last_active_acp()
        .expect("top-level pointers populated");
    assert_eq!(acp, "acp-session-xyz");
    assert_eq!(brain, "claude-code-acp");

    // Silence unused-import warnings if the helper imports aren't needed.
    let _ = (SessionId::new(), SpurEvent::now(SpurEventBody::TurnComplete {
        session: SessionId::new(),
    }));
}
```

If the existing TUI test harness lets you drive `App::handle_spur_event` directly, prefer that over the store-only shortcut above — that covers the app-layer glue. If not, the store-only test is acceptable because the app handler is a one-liner that delegates to `set_acp_mapping + save`.

- [ ] **Step 3: Run the test**

Run: `cargo test -p spur-tui --test resume_by_acp_e2e -- --nocapture`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-tui/tests/resume_by_acp_e2e.rs
git commit -m "test(spur-tui): e2e resume-by-acp metadata persistence"
```

---

## Task 11: Full workspace verification

- [ ] **Step 1: Build everything**

Run: `cargo build --workspace`
Expected: clean build.

- [ ] **Step 2: Run all tests**

Run: `cargo test --workspace`
Expected: all green, including the new Task 1 regression test and Task 6/10 mapping tests.

- [ ] **Step 3: Clippy pass on modified crates**

Run: `cargo clippy -p spur-acp -p spur-core -p spur-tui -p spur-cli -- -D warnings`
Expected: zero warnings. Fix any that surface in code we touched.

- [ ] **Step 4: Final end-to-end verification**

Delete the current metadata: `rm .spur/session_metadata.json`.
Run `cargo run -p spur-cli -- watch --brain claude-code-acp`. Complete a turn, quit, re-run, confirm the resume note appears ("Resumed from prior conversation") and the conversation history from the first run is visible.

- [ ] **Step 5: Inspect the claude-code-acp log to confirm no `-32002`**

Run: `ls -lt .spur/logs/claude-code-acp-*.log | head -1`, then `cat` the newest file. Expected: no `Resource not found` errors for `session/load`. If present, the ACP id being sent is still wrong — return to Task 9 debugging.

---

## Summary of commits produced

1. `test(spur-acp): regression test for load_session error propagation`
2. `feat(spur-acp): add AgentSessionReady event for ACP id discovery`
3. `feat(spur-core): emit AgentSessionReady from create_brain_session`
4. `feat(spur-core): emit AgentSessionReady from load_brain_session`
5. `feat(spur-tui): add acp_session_id + brain_name to metadata schema`
6. `feat(spur-tui): add set_acp_mapping + last_active_acp accessors`
7. `feat(spur-tui): persist ACP mapping on AgentSessionReady`
8. `feat(spur-tui): show resume note on AgentSessionReady resumed=true`
9. `feat(spur-cli): auto-resume by ACP session id, gated on brain match`
10. `test(spur-tui): e2e resume-by-acp metadata persistence`

Fix #1 (`fix(spur-acp): propagate load_session agent errors`) is already committed on `main` prior to this plan's execution.

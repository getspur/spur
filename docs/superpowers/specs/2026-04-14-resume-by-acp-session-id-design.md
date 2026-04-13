# Resume by ACP Session Id — Design

Date: 2026-04-14
Status: Approved for implementation

## Problem

`cargo run -p spur-cli -- watch --brain claude-code-acp` lands the user in a
state where typing and pressing Enter appears to do nothing. Root cause is
a chain of three bugs in the session-resume path.

### Evidence
From `.spur/logs/claude-code-acp-*.log` across 3 recent runs:
```
session/load  → -32002 Resource not found: <uuid>   (id=1)
session/prompt → -32603 Session not found           (id=2, same uuid)
```
No `session/new` between the two. Every prompt dies on the dead id.

### Three layered bugs

1. **Silent Ok in `LoadSession` handler** (spur-acp/src/connection/native.rs).
   The handler sent `reply.send(Ok(rx))` before awaiting
   `connection.load_session(request)`. ACP-level errors were only logged,
   never propagated. The orchestrator's `load_brain_session` fallback to
   `new_session` never fired. **Fixed in a prior commit** — see the
   regression test work below.
2. **Wrong id flavor in metadata.** `session_metadata.json` stores
   `last_active_session_id` as the SPUR session id
   (`SessionId::new()` minted each spawn). That id is never known to the
   backing agent. On next launch it's sent as `ResumeSession.session_id`
   and treated as the ACP id by `load_brain_session`. Always misses.
3. **Premature `BrainSpawned` emission.** `load_brain_session` emits
   `BrainSpawned` before attempting `load_session`, so the TUI attaches
   to a session whose ACP id may still be up in the air. Superseded by
   the new event below; no longer a bug to fix directly.

## Goal

Make "resume last session" actually resume the prior agent-side conversation,
keeping fresh-spawn semantics unchanged.

## Model

- **SPUR session id** — ephemeral in-process handle, minted per spawn,
  used for event routing and TUI attach. Never stable across runs.
- **ACP session id** — what the backing agent persists. Stable where the
  agent supports `session/load`. The only id that can resume.
- **Resume** = send the ACP id to the agent. Metadata must persist it.

## Design

### Event addition (spur-acp)

New variant in `SpurEventBody`:
```rust
AgentSessionReady {
    session: SessionId,       // SPUR id (same as BrainSpawned)
    acp_session_id: String,   // what the agent knows
    brain: String,            // brain name that owns this id
    resumed: bool,            // true iff session/load succeeded
}
```

Emitted from:
- `create_brain_session`: after `new_session` RPC succeeds, before the
  `BrainSession` is returned. `resumed = false`.
- `load_brain_session`: after the load/new branch completes, carrying
  `final_acp_session_id`. `resumed = load_session returned Ok`.

`BrainSpawned` emission is unchanged — it remains the attach-intent
trigger for the TUI. `AgentSessionReady` arrives shortly after and
supplies the persistent identity.

### Metadata schema bump (spur-tui/session_metadata.rs)

Version `0 → 1`. Shape:
```rust
struct Metadata {
    version: u32,                                      // 1
    last_active_session_id: Option<String>,            // SPUR id (kept for UI)
    last_active_at: Option<String>,
    last_active_acp_session_id: Option<String>,        // NEW
    last_active_brain: Option<String>,                 // NEW
    sessions: HashMap<String, SessionEntry>,
}

struct SessionEntry {
    title_override: Option<String>,
    last_opened_at: String,
    draft: String,
    pinned: bool,
    archived: bool,
    acp_session_id: Option<String>,                    // NEW
    brain_name: Option<String>,                        // NEW
}
```

Read via serde defaults so v0 files deserialize cleanly; `version`
bumps on the next save. Drafts keyed by old SPUR ids are retained but
unreachable (SPUR ids were never stable across runs anyway) — acceptable.

New accessors:
- `set_acp_mapping(spur_id: &str, acp_id: &str, brain: &str)`
- `last_active_acp() -> Option<(String /* acp */, String /* brain */)>`

### TUI event handling (spur-tui/app.rs)

On `SpurEventBody::AgentSessionReady`:
- `metadata_store.set_acp_mapping(session.0, acp_session_id, brain)`.
- If `session.0` equals the current `last_active_session_id`, mirror
  `acp_session_id`/`brain` into the top-level fields.
- Persist.

`TurnComplete` handling unchanged in intent — continues setting
`last_active_session_id` (SPUR) and `last_active_at`, and mirrors
`last_active_acp_session_id` + `last_active_brain` from the matching
entry on the same save.

### Main.rs resume path (spur-cli)

Replace the current block:
```rust
let auto_resume_id = meta.metadata().last_active_session_id.clone();
```
with:
```rust
let auto_resume = meta.metadata().last_active_acp();  // (acp_id, stored_brain)
```

Skip resume if:
- `auto_resume` is `None` (no v1 data yet), OR
- `brain` override is `Some(b)` and `b != stored_brain`.

Otherwise: `UserInput::ResumeSession { session_id: acp_id }`.

### Regression test for Fix #1

In `crates/spur-acp`, add a mock-agent test that returns an error from
`session/load` and asserts `NativeAcpConnection::load_session` returns
`Err`. This pins the behavior that was silently broken.

## Scope boundaries

- No changes to `session_detail.rs`, `react_trace`, or lineage.
- No changes to session picker — it already uses agent-authoritative
  ACP ids via `list_sessions`.
- No draft migration; drafts under old SPUR ids remain in the file but
  are unreachable.
- No file-lock for concurrent `spur watch` processes; pre-existing hazard.

## Edge cases

- **Agent rejects stored ACP id (aged out)**: `load_session` errors
  (now propagated by Fix #1), fallback to `new_session`,
  `AgentSessionReady` fires with `resumed=false` and the new id.
  Metadata overwrites to the fresh id. History is lost but state
  stays consistent.
- **Brain override mismatch**: `--brain kiro` with stored
  `claude-code-acp` id → resume skipped, normal spawn, metadata
  overwrites with kiro's new id on first `AgentSessionReady`.
- **Agents without `load_session` support** (kiro): already falls
  through to `new_session`; `AgentSessionReady` still fires.
- **Simultaneous `spur watch` processes**: race on
  `session_metadata.json`; pre-existing; out of scope.

## Implementation order

Two commits:
1. **Regression test for Fix #1** (~30 LoC): mock agent returning
   `-32002` on `session/load` → assert `Err` propagation.
2. **Resume-by-ACP** (~170 LoC) across five files:
   - `crates/spur-acp/src/domain/events.rs` — add `AgentSessionReady`.
   - `crates/spur-core/src/orchestrator.rs` — emit from
     `create_brain_session` and `load_brain_session`.
   - `crates/spur-tui/src/session_metadata.rs` — schema bump.
   - `crates/spur-tui/src/app.rs` — handle new event.
   - `crates/spur-cli/src/main.rs` — resume by ACP id.

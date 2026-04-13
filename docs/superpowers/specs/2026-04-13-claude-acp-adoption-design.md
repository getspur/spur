# Claude ACP Adoption — Design

**Date:** 2026-04-13
**Status:** Approved for planning
**Owner:** spur-acp / spur-core / spur-tui

## Goal

Adopt the upstream `@agentclientprotocol/claude-agent-acp` TypeScript binary as the
preferred transport for Claude Code integration, replacing `StreamJsonAdapter` for
Claude Code use cases. Reach feature parity with the reference's Claude Code UX
(permission gating, mode/model/command surfaces, usage tracking, session
fork/resume, auth flows) by consuming ACP notifications the binary already emits,
rather than reimplementing the Claude Agent SDK in Rust.

## Non-goals

- Porting any part of `@anthropic-ai/claude-agent-sdk` (70k LoC) to Rust.
- Deleting `StreamJsonAdapter`. It continues to serve non-Claude agents that
  speak Claude-style stream-json.
- Changing the orchestrator's agent-selection model, worktree handling, or
  spur-mcp architecture.
- Supporting Claude Code without Node.js on the host. Claude Code itself
  requires Node; no new runtime dependency is introduced.

## Motivation

Evidence from deep inspection (both codebases plus crate versions):

- The reference `claude-agent-acp` (3,543 LoC) is a thin ACP wrapper over
  `@anthropic-ai/claude-agent-sdk` (70,259 LoC) + `@anthropic-ai/sdk` (14,305
  LoC). The richness lives in the SDK, which has no Rust equivalent.
- Spur's Rust `agent-client-protocol` crate v0.10.4 already exposes the full
  advanced surface: `fork_session`, `resume_session`, `set_session_mode`,
  `set_session_model`, `set_session_config_option`, `close_session`,
  `authenticate` (crate `src/agent.rs:133-216`).
- Spur's `NativeAcpConnection` (`crates/spur-acp/src/connection/native.rs`,
  1,091 LoC) is a complete ACP `Client` implementation with permission, fs,
  and terminal handlers (`native.rs:729-976`). The integration pipe is
  already welded; only the agent profile and a few trait methods are missing.
- `StreamJsonAdapter` parses only three event types and four content blocks
  (`protocol/claude_events.rs:18-71`) — a permanent feature ceiling on
  plan mode, slash commands, usage, compaction, and model switching.

Cost comparison:

| Path | LoC to write | Time | Maintenance |
|---|---|---|---|
| Port TS SDK to Rust | ~88,000 | 4-8 engineer-months | Permanent treadmill |
| Adopt `claude-agent-acp` via NativeAcpConnection | ~1,500 | ~2-3 weeks | Upstream owns it |

## Architecture

### Transport routing (unchanged)

```
config.toml    ──▶  AgentRegistry  ──▶  TransportKind  ──▶  Adapter
                                         ├── Acp         NativeAcpConnection ← Claude Code goes here
                                         ├── StreamJson  StreamJsonAdapter
                                         ├── Stdio       StdioAdapter
                                         └── CliWrap     CliWrapAdapter
```

Claude Code migrates from `TransportKind::StreamJson` to `TransportKind::Acp`.

### Subprocess layout

```
Spur (Rust)
  └─ NativeAcpConnection (dedicated OS thread + LocalSet)
       ├─ spawns: npx --yes @agentclientprotocol/claude-agent-acp@<pinned>
       │    └─ Node.js process
       │         └─ @anthropic-ai/claude-agent-sdk
       │              └─ spawns: claude (Claude Code CLI)
       └─ speaks: ACP ndjson over stdio
```

One Node process per active Claude session. Spawn cost paid once at
`new_session`; all subsequent prompts reuse the same process.

### Permission gating principle (to be documented as project-wide rule)

Every agent gets policy gating. Mechanism depends on transport:

- **Native ACP agents** (Claude via claude-agent-acp, future first-party ACP
  agents): policy gating via ACP `request_permission` RPC, handled in
  `SpurAcpClientDynamic::request_permission` (`native.rs:730`).
- **Non-ACP agents**: policy gating via a spur-mcp permission-prompt tool
  surfaced to the agent's CLI, e.g. Claude's `--permission-prompt-tool`.
  (Not in scope for this spec; called out so future work keeps this
  invariant.)

## Components

### spur-acp

**Config — new agent profile.** Document a ready-to-use `claude-code-acp`
profile:

```toml
[[agents]]
name = "claude-code-acp"
role = "both"
transport = "acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@<pinned-version>"]
capabilities = ["code", "chat"]
```

Version pinning is mandatory. A bare `@latest` would silently pull breaking
upstream changes mid-session.

**`AgentConnection` trait extensions.** Add methods mirroring the ACP advanced
surface. Default implementations return `"not supported by this transport"`
(same pattern used today for `load_session` and `list_sessions` at
`connection/mod.rs:111-128`):

```rust
async fn fork_session(&mut self, req: ForkSessionRequest)
    -> anyhow::Result<ForkSessionResponse>;
async fn resume_session(&mut self, req: ResumeSessionRequest)
    -> anyhow::Result<ResumeSessionResponse>;
async fn close_session(&mut self, req: CloseSessionRequest)
    -> anyhow::Result<CloseSessionResponse>;
async fn set_session_mode(&mut self, req: SetSessionModeRequest)
    -> anyhow::Result<SetSessionModeResponse>;
async fn set_session_model(&mut self, req: SetSessionModelRequest)
    -> anyhow::Result<SetSessionModelResponse>;
async fn set_session_config_option(&mut self, req: SetSessionConfigOptionRequest)
    -> anyhow::Result<SetSessionConfigOptionResponse>;
async fn authenticate(&mut self, req: AuthenticateRequest)
    -> anyhow::Result<AuthenticateResponse>;
```

**`NativeAcpConnection` — implement the new trait methods.** Each method:

1. Adds an `AcpCommand::*` variant (existing pattern, `native.rs:59-89`).
2. Adds a dispatcher arm on the LocalSet thread that calls the SDK's
   corresponding method on `ClientSideConnection`.
3. Forwards the response via `oneshot::Sender`.

Estimated ~50 LoC per method × 7 methods = ~350 LoC.

**Client capability advertisement.** In `NativeAcpConnection::initialize`,
advertise capabilities that activate the reference's full feature surface:

```rust
ClientCapabilities {
    fs: FileSystemCapability { read_text_file: true, write_text_file: true },
    terminal: true,
    auth: AuthCapability { terminal: true, _meta: Some(terminal_auth_meta) },
    _meta: Some(json!({ "terminal-auth": true })),
    ..
}
```

Without this, claude-agent-acp's initialize response omits the matching auth
methods (`acp-agent.ts:326-396`).

**`StreamJsonAdapter` — documentation update only.** Clarify in the file
header that this adapter serves non-Claude stream-json agents. No code
changes.

### spur-core (orchestrator)

**Agent dispatch.** Already routes `TransportKind::Acp` to `NativeAcpConnection`
(`orchestrator.rs:1126,1370`). Zero changes required beyond propagating new
notifications to subscribers.

**Session event bus.** Add forwarded events for the new notification variants
(see spur-tui section). Each is a passthrough; no orchestration logic changes.

### spur-tui

New handlers for `SessionUpdate` variants the reference emits that we
currently ignore:

- `UsageUpdate { used, size, cost }` → status-bar indicator (context %,
  running cost)
- `ModeState` → plan-mode indicator, mode switcher shortcut
- `ModelState` → current-model label, picker
- `AvailableCommands` → slash-command autocomplete in the input area

New interactive surfaces:

- Mode switcher (e.g. `Esc-m` cycles mode) — wires `set_session_mode`
- Model picker — wires `set_session_model`
- Slash-command menu — wires through the existing prompt path
- Auth dialog invoked on `RequestError::authRequired` — launches the
  terminal-auth command advertised by the server's `authMethods`

Estimated ~500-800 LoC across view + state modules.

## Data flow — representative turn

1. User types a prompt in the TUI.
2. `spur-tui` dispatches `SendMessage` to `spur-core`.
3. Orchestrator calls `NativeAcpConnection::prompt` (existing path).
4. `claude-agent-acp` subprocess runs the SDK's `Query`, yielding messages.
5. The subprocess emits ACP `sessionUpdate` notifications:
   - `agent_message_chunk` → TUI transcript
   - `tool_call` / `tool_call_update` → TUI tool-call panels
   - `usage_update` → TUI status bar  (new)
   - `available_commands_update` → TUI autocomplete  (new)
   - `mode_state` / `model_state` → TUI indicators  (new)
6. On a permission-requiring tool, the subprocess RPCs
   `requestPermission` → `SpurAcpClientDynamic::request_permission` →
   `PermissionRequest` channel → TUI dialog → `PermissionResponse` →
   back to subprocess → SDK proceeds.
7. `session_state_changed: idle` ends the turn;
   `PromptResponse { stopReason, usage }` returns to the orchestrator.

## Error handling

- **Subprocess spawn failure** (Node missing, package unresolvable):
  surface as an `AgentHealth::Error` on the connection; the agent
  registry marks the profile unhealthy. Orchestrator returns a clear
  error to the TUI.
- **ACP version mismatch** in handshake: covered by existing
  version-negotiation in `NativeAcpConnection::initialize`. No new code.
- **Subprocess crash mid-session**: `prompt` stream ends; the existing
  `NativeAcpConnection` cleanup path kills the child (`native.rs`
  shutdown handler).
- **Auth required mid-turn**: reference throws `RequestError.authRequired`
  which surfaces as a Rust-side error from the `prompt` call. TUI
  catches and launches the auth dialog.

## Testing

- Unit tests for each new `AgentConnection` trait method's dispatch arm
  (mock the SDK's `ClientSideConnection` responder).
- Integration test: spawn `claude-agent-acp` (or a stub that speaks the
  same ACP subset) and exercise new_session → prompt → permission →
  cancel → close_session end to end.
- Manual UAT checklist: plan mode toggle, model switch mid-session,
  slash command (`/compact`), fork from an existing session, resume
  after restart, auth flow on a fresh machine without credentials.

## Rollout

Behind an agent profile, not a code flag. Users opt in by selecting
`claude-code-acp` in their `config.toml`; existing
`transport = "stream_json"` Claude profiles keep working. Once the new
profile has baked for a release, default examples and docs switch to it
and the stream-json Claude profile is marked deprecated.

## Milestones

The implementation plan (produced next) will sequence work as:

- **M1** — Agent profile + smoke test (config only + manual verification)
- **M2** — Permission round-trip verified end to end
- **M3** — `AgentConnection` trait extensions + dispatcher wiring
  (fork / resume / close / set_mode / set_model / set_config_option /
  authenticate)
- **M4** — TUI handlers for UsageUpdate / ModeState / ModelState /
  AvailableCommands + interactive switchers
- **M5** — Auth flow (authMethods discovery, terminal-auth launcher)
- **M6** — Docs, version pin policy, deprecation note on stream-json
  Claude profile

Each milestone is independently shippable and independently valuable.

## Risks & open questions

- **Version-pin policy.** Manual bumps or an automated check. The spec
  mandates pinning; the mechanism is a planning-level decision.
- **`npx` first-run latency** (package fetch). Options: recommend
  `npm i -g`, bundle the package in a Spur install script, or accept
  the one-time cost. Decide during M1.
- **Client capability wire format.** The Rust ACP crate's
  `ClientCapabilities` struct may not have first-class fields for every
  `_meta` field the reference checks (`terminal-auth`, `gateway`). If
  not, populate via the `_meta` passthrough. Confirm during M1.
- **TUI's current `SessionUpdate` match** may be non-exhaustive; adding
  variants must not regress existing flows. Use a catchall arm with
  debug logging during rollout.

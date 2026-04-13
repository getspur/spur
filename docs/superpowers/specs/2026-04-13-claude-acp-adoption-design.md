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

**Scope of trait extensions in this spec (YAGNI-narrowed).**

Only the methods with a concrete consumer in the milestones below are
implemented now: `set_session_mode` (drives plan-mode toggle) and
`authenticate` (needed to surface `RequestError::authRequired` as a
clear user message even before an in-TUI auth flow exists).

The remaining methods — `fork_session`, `resume_session`,
`close_session`, `set_session_model`, `set_session_config_option` —
are declared in the trait but left as default-unsupported stubs and
implemented in follow-up specs when a TUI consumer requires them.

**`NativeAcpConnection` — implement the two in-scope trait methods.** Each:

1. Adds an `AcpCommand::*` variant (existing pattern, `native.rs:59-89`).
2. Adds a dispatcher arm on the LocalSet thread that calls the SDK's
   corresponding method on `ClientSideConnection`.
3. Forwards the response via `oneshot::Sender`.

Estimated ~50 LoC per method × 2 methods = ~100 LoC in this spec's scope.

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

**Subprocess stderr capture (must-have correction).**
`NativeAcpConnection` currently configures the child process with
`stderr(Stdio::inherit())` (`native.rs:486`). For `claude-agent-acp` the
child emits SDK logs, unhandled-rejection traces, and progress lines to
stderr by design (reference `index.ts:18-21` redirects `console.log` to
stderr). Inherited into Spur's TUI-owning parent process, that output
will corrupt the terminal or disappear silently depending on the
frontend.

Change `NativeAcpConnection` to pipe the child's stderr into a
per-session log file at `.spur/logs/<agent>-<session_id>-acp.log`
(overwriting on session start, appending within a session). Surface the
path in a tracing event at `new_session` time so users can tail it when
debugging.

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

New interactive surfaces (in scope for this spec):

- **Plan-mode toggle** (e.g. `Esc-m`) — wires `set_session_mode` between
  the default mode and `plan`. Mode indicator reflects current state.
- **Auth-required error surfacing** — when the subprocess throws
  `RequestError::authRequired`, the TUI shows a clear, dismissable
  message instructing the user to run `claude /login` and restart the
  session. No interactive login flow in this spec.

Deferred to follow-up specs:

- In-TUI auth dialog that launches `authMethods` terminal-auth commands
- Model picker (`set_session_model`)
- Slash-command autocomplete execution
- Fork-from-session UI
- Resume-by-id UI (the picker exists; wiring to `resume_session` is new)

Estimated ~250-400 LoC across view + state modules for the in-scope
surfaces.

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

Sequenced to kill highest-consequence unknowns first, then deliver
user-visible value, keeping scope tight enough to ship in ~5
engineer-days.

- **M0 — Protocol-compat spike (~0.5d).** Standalone Rust harness (not
  in the Spur tree) that instantiates `NativeAcpConnection`, launches
  `claude-agent-acp` via `npx`, and round-trips `initialize →
  new_session → prompt → set_session_mode → close_session` plus a
  `fork_session` attempt. Records whether each method succeeds, the
  cold-start time, and behavior when offline. Gate: if any core
  round-trip fails, stop and revisit pinned version before proceeding.

- **M1 — Agent profile + smoke + permission + version pin + stderr
  capture (~1-1.5d).**
  1. Add `claude-code-acp` profile to the example config with a pinned
     `claude-agent-acp` version.
  2. Flip `NativeAcpConnection`'s child spawn from `Stdio::inherit()`
     to a per-session log file at
     `.spur/logs/<agent>-<session_id>-acp.log`; emit the path via
     tracing.
  3. Manually exercise a prompt end to end through the TUI.
  4. Trigger a tool-use (e.g. file write) and confirm the permission
     round-trip (claude-agent-acp → `request_permission` → TUI →
     selection → back to subprocess) works out of the box using the
     existing `SpurAcpClientDynamic` impl.

- **M2 — Read-only TUI notifications (~1.5d).** Add handlers for:
  `UsageUpdate` (status-bar: context %, running cost),
  `ModeState` (mode indicator), `AvailableCommands` (rendered as a
  hint list in the input area — display only, no execution wiring
  yet). Use a catchall arm with debug logging for unknown variants to
  avoid regressions.

- **M3 — Narrow trait extensions (~0.5d).** Add `set_session_mode` and
  `authenticate` to `AgentConnection`, with default
  `"not supported by this transport"` implementations. Implement both
  in `NativeAcpConnection` using the established `AcpCommand` +
  LocalSet dispatcher pattern (~100 LoC).

- **M4 — Plan-mode toggle + auth-error surfacing (~1d).** Bind a
  keystroke in the TUI to call `set_session_mode` and cycle between
  default and `plan`. On any `RequestError::authRequired` from session
  creation or prompt, render a clear message telling the user to run
  `claude /login` and restart.

- **M5 — Docs, version-pin policy, deprecation note (~0.5d).** Write a
  short operator guide: how to switch a Claude profile to the new
  transport, how to pin/bump the `claude-agent-acp` version, where to
  find the per-session log. Add a deprecation notice on the Claude
  `stream_json` profile in the default config.

Each milestone is independently shippable. M0 gates the rest.

## Follow-up work (explicitly out of scope)

These are known valuable extensions; each gets its own spec when a
concrete consumer need materializes:

- In-TUI auth flow: discover `authMethods`, launch `terminal-auth`
  commands, observe completion, retry session creation.
- Trait extensions and TUI for `fork_session`, `resume_session`,
  `close_session`, `set_session_model`, `set_session_config_option`.
- Slash-command execution wiring (autocomplete already in M2).
- Model picker UI.
- Observability dashboard: per-session cost aggregation, subprocess
  health metrics.
- Bundled / vendored `claude-agent-acp` install to remove the
  `npx --yes` network requirement.

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

# Kiro vendor-exec fallback — design

**Date:** 2026-04-15
**Status:** Draft — awaiting user review
**Scope:** Option A from the `/clear` error brainstorm.

## Problem

Typing `/clear` (or any other kiro slash command) in the TUI produces:

```
BRAIN ERROR: vendor exec `_kiro.dev/commands/execute` failed:
NativeAcpConnection 'kiro': ext_method failed:
Internal error: "server shut down unexpectedly"
```

After the error, the brain session is dead for the remainder of the TUI run.

## Grounding (live probe)

Driving `kiro-cli 2.0.0 acp` directly over stdio:

| RPC | Result |
|---|---|
| `_kiro.dev/commands/execute` `{command:"/clear"}` | subprocess exits code 0, no response |
| Same for `/compact`, `/usage`, `/help`, `/context`, `/tools`, `/quit`, `/hooks`, `/mcp`, `/todos`, `/knowledge` | subprocess exits code 0, no response |
| `session/prompt` `{prompt:[{type:"text",text:"/clear"}]}` | returns `stopReason: end_turn`, streams `"Conversation cleared"`, emits `_kiro.dev/clear/status`, subprocess stays alive |

Conclusion: kiro's `commands/execute` vendor extension is universally broken in this release. The normal `session/prompt` path already handles slash commands correctly as text.

Additionally, kiro tags some commands with `meta.local=true` in `_kiro.dev/commands/available` (e.g. `/chat save`, `/chat load`). These are meant for the client to handle locally; forwarding them to the server is always wrong.

## Design

Two changes, both small, both independently valuable.

### A1 — Switch kiro's dispatch from `vendor_exec` to `prompt_text`

File: `crates/spur-acp/src/seed_agents.toml`, the `[agents.entries.commands]` block under kiro.

```toml
# Before
[agents.entries.commands]
dispatch = "vendor_exec"
exec_method = "_kiro.dev/commands/execute"
args_template = "raw_rest"

# After
[agents.entries.commands]
dispatch = "prompt_text"
```

Ingest (`[[agents.entries.commands.ingest]]` for `_kiro.dev/commands/available`) stays — the popup still lists kiro commands. Only the **execute** path changes: the TUI will now send the selected command as a regular `ContentBlock::Text` through `session/prompt`, which the probe proved works.

`response` (`_kiro.dev/commands/execute/response`) becomes dead config — remove it too.

No code changes in `crates/spur-tui/src/commands/`. `submit_router` already routes `PromptText` dispatch correctly (submit_router.rs:60-70). `entry_builder::build_entry` already writes `normalized = "/{name}"` for `PromptText` (entry_builder.rs:22-24).

### A2 — Filter `meta.local=true` at ingest

Kiro signals which commands it owns locally vs. which it forwards to the server. `meta.local=true` means "client handles this". Since the TUI doesn't implement kiro's local handlers, these entries are dead weight in the popup.

Files touched:
- `crates/spur-acp/src/types.rs` (or wherever `AvailableCommand.meta` is typed): expose `meta.local: Option<bool>`.
- `crates/spur-tui/src/agents/entry_builder.rs` or the ingest parser in `crates/spur-acp/src/config/` (wherever `item_schema = "acp_available_command"` is parsed into entries): drop any entry where `meta.local == Some(true)`.

A test in `crates/spur-tui/tests/` fixes this contract: given an available-commands payload that mixes `local:true` and `local:false/absent`, only non-local entries surface in the registry.

## Non-goals

- No changes to the `VendorExec` dispatch kind itself. Other agents (or a future fixed kiro) can still use it.
- No respawn logic (that's Option B, separate spec).
- No upstream bug report to kiro in this spec (nice to have, track separately).

## Acceptance criteria

1. `/clear` in the TUI with a connected kiro brain: kiro replies "Conversation cleared" (or equivalent streamed text), session stays alive, no `BrainError` event emitted.
2. Same for `/compact`, `/help`, `/context`, `/usage`, `/tools`, `/hooks`, `/mcp`, `/todos`, `/knowledge`.
3. `/chat save foo.json` (kiro emits with `local:true`) does not appear in the command popup.
4. All existing `cargo test -p spur-tui` and `cargo test -p spur-acp` tests pass.
5. Manual trace: sending `/clear` results in exactly one outgoing `session/prompt` RPC carrying text `/clear`, no `_kiro.dev/commands/execute` on the wire.

## Risk & rollback

- If a future kiro release fixes `commands/execute` and offers better semantics than prompt_text, reverting is a config-only change in `seed_agents.toml`.
- The `meta.local` filter is config-driven (data-flag-based); if kiro changes the flag's meaning, the filter becomes inert rather than broken.

# Using Claude Code with Spur (via `claude-code-acp`)

This is the preferred transport for Claude Code. It runs the upstream
[`@agentclientprotocol/claude-agent-acp`](https://github.com/agentclientprotocol/claude-agent-acp)
binary as a subprocess that speaks ACP, and plugs into Spur's
`NativeAcpConnection`.

## Requirements

- Node.js 20+ on `PATH` (Claude Code itself needs this anyway).
- An authenticated Claude Code install: run `claude /login` once.

## Enabling

Add this profile to `.spur/config.toml`:

```toml
[[agents.entries]]
name = "claude-code-acp"
command = "npx"
args = ["--yes", "@agentclientprotocol/claude-agent-acp@0.26.0"]
transport = "acp"
role = "both"
```

Set as the default brain:

```toml
[brain]
default = "claude-code-acp"
```

## Version pinning

**Do not use `@latest`.** Pin a specific version. To discover current versions:

```bash
npm view @agentclientprotocol/claude-agent-acp versions --json | tail -20
```

Bump the pin in `.spur/config.toml` when you want to adopt upstream changes.
Run a smoke test after each bump (prompt → permission → plan-mode toggle).

## Logs

Each ACP subprocess writes its stderr to:

```
.spur/logs/claude-code-acp-<timestamp>-<pid>-acp.log
```

Tail this when debugging. The Rust side's tracing output is separate and
goes to Spur's configured log sink (see `spur-tui` log-file-sink setup).

## Features enabled by this transport

- Plan-mode toggle (`Alt-m`).
- Live context-% and cost in the status bar.
- Slash-command list (displayed; execution deferred to a follow-up).
- Permission prompts gated through Spur's TUI.
- Clear `authentication required` banner when Claude credentials are missing
  or expired — run `claude /login` in a terminal, then restart the session.

## What's deferred

- In-TUI auth flow — run `claude /login` externally for now. A red banner
  in the TUI tells you when auth is required.
- Model picker.
- `fork_session` / `resume_session` UI.
- Slash-command execution wiring.

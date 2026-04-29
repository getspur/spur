# SPUR

**Issue in, PR out — across every agent.**

SPUR is a Rust-native TUI that orchestrates multiple AI coding agents through the [Agent Client Protocol (ACP)](https://github.com/anthropics/agent-client-protocol). A "brain" agent reasons about your task and delegates work to the best-fit worker agent, while SPUR handles the coordination, review loop, and project management integration.

> ⚠️ **Early stage** — APIs and config format may change.

## How It Works

```
┌─────────────────────┐     ┌──────────────────────────────────┐     ┌─────────────────────┐
│  Project Management │     │           SPUR TUI               │     │   Worker Agents     │
│                     │     │                                  │     │                     │
│  GitHub Issues      │◄───►│  ┌────────────────────────────┐  │     │  Claude Code        │
│  Linear             │     │  │  Brain Agent (orchestrator) │──┼────►│  Codex              │
│  Plane              │     │  └────────────────────────────┘  │     │  Kiro               │
│                     │     │  ┌─────────┬────────┬─────────┐  │     │  Gemini             │
│                     │     │  │ ReAct   │ Agent  │ Activity│  │     │  (any ACP agent)    │
│                     │     │  │ Trace   │Sessions│ Log     │  │     │                     │
│                     │     │  └─────────┴────────┴─────────┘  │     │                     │
└─────────────────────┘     └──────────────────────────────────┘     └─────────────────────┘
                                         ▲
                                         │ ACP (JSON-RPC 2.0 / stdio)
                                         ▼
                                  ┌──────────────┐
                                  │ Git Worktrees │
                                  │ Cost Tracker  │
                                  │ Event Sink    │
                                  └──────────────┘
```

## Features

- **Multi-agent orchestration** — Brain agent delegates tasks to workers via ACP with per-agent routing descriptors (tier, strengths, cost tier)
- **Review loop** — Brain reviews worker output and can approve, reject, modify, or retry with feedback
- **Terminal UI** — Dashboard with ReAct trace viewer, live agent session streams, activity log, and agent tree
- **PM integration** — Fetch issues, create PRs, and update status across GitHub, Linear, and Plane
- **Git worktree isolation** — Each worker operates in its own worktree for safe parallel execution
- **Cost tracking** — Per-session and per-project cost monitoring backed by SQLite
- **Session persistence** — Resume sessions after restarts with crash recovery
- **Mermaid rendering** — Render diagrams inline in the terminal
- **Slash commands & completions** — Input bar with command registry, file mentions, and fuzzy matching
- **TOML config with linting** — Validated configuration with helpful warnings

## Crate Structure

| Crate | Purpose |
|---|---|
| `spur-cli` | Binary entry point and CLI commands |
| `spur-tui` | `ratatui` terminal interface — views, components, input handling |
| `spur-core` | Orchestration engine, review loop, lineage tracking, event pipeline |
| `spur-acp` | ACP client, transports (stdio, native, CLI-wrap), event types, config |
| `spur-mcp` | MCP server exposing delegation tools to the brain agent |
| `spur-pm` | Project management adapters (GitHub, Linear, Plane) |
| `spur-worktree` | Git worktree creation, diffing, merging, and cleanup |
| `spur-cost` | Cost tracking and estimation with SQLite storage |

## Quickstart

### Install

```sh
cargo install spur-cli
```

### Initialize a project

```sh
cd your-project
spur init
```

This creates a `.spur/config.toml` with detected agents and sensible defaults.

### Run

```sh
spur
```

The TUI launches with your configured brain and worker agents. Type a task or pick an issue from your PM integration.

### Check config

```sh
spur config check
```

Validates your configuration and reports warnings.

## Configuration

SPUR is configured via `.spur/config.toml` at your project root. Key sections:

```toml
[brain]
agent = "claude-code-acp"

[agents.entries.claude-code-acp]
role = "brain"
transport = "native"

[agents.entries.codex]
role = "worker"
transport = "stdio"

[pm.github]
repo = "owner/repo"

[worktree]
enabled = true

[cost]
db_path = "~/.spur/cost.db"
```

Each agent entry supports delegation descriptors (`good_for`, `avoid_for`, `tier`, `cost_tier`) that the brain uses for routing decisions. Run `spur config check` to lint your configuration.

## Development

```sh
# Build all crates
cargo build --workspace

# Run the full test suite
cargo test --workspace

# Run tests for a single crate
cargo test -p spur-tui

# Lint
cargo clippy --workspace -- -D warnings

# Format
cargo fmt --all

# Run locally
cargo run -p spur-cli -- --help
```

### Commit format

```
<type>(<scope>): <short imperative>
```

Types: `feat`, `fix`, `test`, `docs`, `refactor`, `chore`. Keep subjects under 72 characters.

## Contributing

1. Fork the repo and create a feature branch
2. Follow the existing code style (`cargo fmt`, `cargo clippy`)
3. Add tests for new functionality — bug fixes follow TDD cadence (failing test first, then fix)
4. Keep changes scoped to one crate when possible
5. Open a PR with a clear description of the problem and solution

## License

[MIT](LICENSE)

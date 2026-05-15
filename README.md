# SPUR

**Issue in, PR out — across every agent, in parallel, with one review surface.**

SPUR is a Rust-native terminal orchestrator for AI coding agents. A "brain" agent reasons about your task and delegates work to one or more "worker" agents — Claude Code, Codex, Gemini, Kimi, OpenCode, or any [ACP](https://github.com/anthropics/agent-client-protocol)-speaking agent. Each worker runs in its own isolated git worktree. SPUR coordinates dispatch, review, retries, cost, and project-management state in one place.

> ⚠️ **Early stage** — APIs and config format may change.

## The Problem

Running multiple AI coding agents today means living with three frustrations:

- **Collision.** Two agents editing the same repo corrupt the working tree.
- **Opacity.** When an agent says "done," there's no unified place to see the diff, approve, reject, or retry.
- **Cost blindness.** Tokens accrue across five vendors with no single ledger.

SPUR removes these by giving each worker its own git worktree, surfacing every diff in a TUI review card, and reading vendor logs into a single DuckDB-backed cost ledger.

## How It Works

```
┌─────────────────────┐     ┌──────────────────────────────────┐     ┌─────────────────────┐
│  Project Management │     │           SPUR TUI               │     │   Worker Agents     │
│                     │     │                                  │     │                     │
│  beads (native)     │◄───►│  ┌────────────────────────────┐  │     │  Claude Code        │
│  GitHub Issues      │     │  │  Brain Agent (orchestrator) │──┼────►│  Codex              │
│                     │     │  └────────────────────────────┘  │     │  Gemini             │
│                     │     │  ┌─────────┬────────┬─────────┐  │     │  Kimi               │
│                     │     │  │Dashboard│Insights│Plan View│  │     │  OpenCode           │
│                     │     │  └─────────┴────────┴─────────┘  │     │  Kiro (partial)     │
└─────────────────────┘     └──────────────────────────────────┘     │  Generic ACP        │
                                         ▲                            └─────────────────────┘
                                         │ ACP (JSON-RPC 2.0 / stdio)
                                         ▼
                          ┌──────────────────────────────────┐
                          │  Git Worktrees per Worker        │
                          │  DuckDB Analytics Engine         │
                          │  Beads Plan Store + Reconciler   │
                          │  MCP Server (delegation tools)   │
                          └──────────────────────────────────┘
```

## Capabilities

- **Parallel delegation, isolated.** `delegate_to_worker`, `delegate_parallel`, and DAG-ordered `submit_plan` dispatch workers concurrently. Each gets its own git worktree on `spur/worker/v2/{agent}/...`, protected by advisory `flock` liveness probes and an orphan-collector. Approved subtask commits are cherry-picked in DAG order onto a staging branch.
- **Review loop with Reflexion retry.** Every worker completion produces a review card — Approve / Reject / Modify / Retry. Retries feed prior attempts back as context (max 3, auto-reject on exhaustion). A dedicated dispatcher decouples decision latency from brain I/O.
- **Heterogeneous agents over ACP.** Per-agent capability negotiation. Slash commands like `/model` and `/effort` are synthesized from each agent's `InitializeResponse`, so cross-agent model switching works on every supported worker.
- **Multi-brain safety.** Plans are locked to a single active brain via a `spur:plan-owner:<id>` label. Tier-1 mutating tools fail on session mismatch. `force_reclaim_plan` breaks deadlocks when a brain wedges.
- **Structured delegation reasoning.** Brains pass a `DelegationPlan` (candidates, rationale, decomposition) with every dispatch. SPUR verifies the chosen agent matches the actual payload — preventing the LLM from lying to its own reviewer.
- **Durable plans.** Plans persist as beads epics, not in memory. The reconciler ticks adaptively, validates ownership, previews overlay conflicts, and supports live mutations (`submit_plan_mutation`: split / replace / amend tasks in-flight) and resume after crash.
- **Cross-vendor cost & insights.** A DuckDB analytics engine reads vendor JSONL/SQLite in place — five live extractors (Claude, Codex, Gemini, OpenCode, Kimi) and a Kiro stub. The Insights view (`Alt+a`) shows 7-day cost sparklines, per-session burn rate, and a `cost_source` provenance gauge.
- **Stable-identity code graph.** Tree-sitter parses Rust / Python / TS / TSX / Markdown into a graph with SHA256 stable symbol IDs and incremental per-file rebuilds. Auto-discovered at the worktree root — no env var setup.
- **Built for orchestration, not chat.** Dashboard with lineage tree, full-screen ReAct trace with inline markdown + mermaid, grouped `@mention` picker (Workers / Files / Issues / Code Graph), `Ctrl+K` universal palette, Plan Inspector (`Alt+P`), live status bar (running count, pending reviews, total cost, model labels).
- **Telegram bot frontend.** `spur-bot` mirrors review cards and session streams into Telegram forum topics for mobile/remote approvals.

## What SPUR is NOT

- Not a chat UI. Every interaction is task-structured.
- Not a single-agent CLI wrapper. SPUR coordinates instances of Claude Code, Codex, etc.; it doesn't replace them.
- Not an IDE. No editor, no LSP, no inline diff gutter.
- Not CI/CD. Plans run locally with local worktrees, not on remote runners.
- Not a universal PM sync. Only beads (native) and GitHub (via `gh`) are implemented today.

## Crate Structure

| Crate | Purpose |
|---|---|
| `spur-cli` | Binary entry point and CLI commands |
| `spur-tui` | `ratatui` terminal interface — views, components, input handling |
| `spur-core` | Orchestration engine, review loop, lineage, event pipeline |
| `spur-acp` | ACP client, transports (stdio / native / cli-wrap), capability negotiation |
| `spur-mcp` | MCP server exposing delegation tools to the brain (delegate, plan, signals) |
| `spur-context` | DuckDB analytics engine + per-agent log extractors |
| `spur-cost` | Pricing registry + SQLite session ledger |
| `spur-graph` | Tree-sitter code graph, stable IDs, incremental rebuild |
| `spur-pm` | Project management adapters (beads, GitHub) |
| `spur-worktree` | Git worktree creation, isolation, flock liveness, cleanup |
| `spur-interactive` | Frontend bridge for non-TUI clients |
| `spur-bot` | Telegram bot frontend |
| `spur-blob-store` | Content-addressed delegation outcome artifacts |
| `spur-license` / `spur-license-admin` | Tier / feature-key registry |

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

Creates a `.spur/config.toml` with detected agents and sensible defaults.

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

SPUR is configured via `.spur/config.toml` at your project root:

```toml
[brain]
agent = "claude-code-acp"

[agents.entries.claude-code-acp]
role = "brain"
transport = "native"

[agents.entries.codex]
role = "worker"
transport = "stdio"
good_for = ["rust", "tests"]
tier = 1

[agents.entries.kimi]
role = "worker"
transport = "stdio"
good_for = ["long-context-exploration", "research"]

[pm.github]
repo = "owner/repo"

[worktree]
enabled = true

[cost]
db_path = "~/.spur/cost.db"
```

Each agent entry supports delegation descriptors (`good_for`, `avoid_for`, `tier`, `cost_tier`) used by the brain for routing. Run `spur config check` to lint your configuration.

## Telemetry and Privacy

SPUR ships anonymous-identifier-based telemetry: Tier 1 crash diagnostics and performance are default ON, Tier 2 usage is default OFF (opt-in).
Disable all telemetry in one line: `SPUR_TELEMETRY=0 spur`.
For persistent disable, run `spur telemetry disable all`.
Collected fields, retention (90 days), deletion-by-ID steps, and pseudonymity details: [docs/PRIVACY.md](docs/PRIVACY.md).

## A Day in the Life

**Parallel refactor.** "Refactor error handling across the codebase." Brain submits a 5-task plan → 5 worktrees, 5 workers run in parallel → review cards appear → approve 4, reject 1 with feedback → rejected worker retries with your note → approved branches cherry-picked to staging. Status bar: total cost $2.40.

**Cost burn check.** Hit `Alt+a` for Insights. Live tab shows 3 active sessions — Codex is on the wrong model. Switch to that session, type `/model gpt-4o`, status bar updates instantly. Breakdown tab shows yesterday $18 (60% Claude, 40% Codex).

**Stuck plan recovery.** Overnight 12-task plan; one task stuck in `Dispatched` (worker died). `Alt+P` Plan Inspector → `force_reclaim_plan` promotes it to `AwaitingReview` → inspect partial diff → reject and split via `submit_plan_mutation` → plan resumes, 11 completed tasks intact.

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

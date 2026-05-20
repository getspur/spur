# SPUR

**The control tower for your CLI coding agents.**

> Issue in, PR out — across every agent, in parallel, with one review surface.

SPUR is a Rust-native terminal layer that sits above the AI coding agents you already run — Claude Code, Codex, Gemini, Kimi, OpenCode, or any [ACP](https://github.com/anthropics/agent-client-protocol)-speaking agent. A "brain" reasons about your task and delegates to one or more "workers." Each worker runs in its own isolated git worktree. SPUR coordinates dispatch, human review, cherry-pick, retries, cross-vendor cost, and project-management state in one place.

> ⚠️ **Early stage** — APIs and config format may change.

## The Problem

Running 2+ AI coding agents today means living with four frustrations:

- **Rate-limit ambush.** Claude Code Max users blow through 5-hour windows in under 90 minutes. *"Paying $200 a month, I hit my weekly in 3 days last week"* (esperent, HN 47626833). When the lockout hits, the in-flight context is gone.
- **Cost opacity.** Tokens accrue across five vendors with no single ledger. *"I'm paying for Max, and when I use the tooling to calculate the spend returned by the API, I can see it's almost $1k!"* (buremba, HN 44598254). Devs only discover the gap by reverse-engineering their own bill.
- **Multi-agent chaos.** *"I run 5–10 Claude Code agents at a time across different repos. Keeping track of which one is waiting for input, which one is working, and which one broke something was chaos. I needed a control tower"* (Beefin, HN 47104424).
- **Worktree merge tax.** DIY tmux + worktrees is the workaround. *"I expected this to become less necessary over time as models got faster, but the opposite has happened"* (nojs, HN 47573483).

SPUR addresses all four: cross-vendor brain-swap when one vendor rate-limits you, a unified live cost ledger across every CLI you pay for, one review surface for parallel workers, and DAG-ordered cherry-pick of approved diffs onto a staging branch.

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

*Ordered by uniqueness — what no single-vendor or cloud peer can match by design.*

- **Unified cost ledger across every vendor.** Five live extractors (Claude, Codex, Gemini, OpenCode, Kimi) feeding a DuckDB analytics engine that reads vendor JSONL/SQLite in place — no ETL. The Insights view (`Alt+a`) shows 7-day cost sparklines, per-session burn rate, and a `cost_source` provenance gauge. Devin, Cosine, Cursor, Aider, and Claude Code each only see their own bill; SPUR sees all of them in one number.
- **Brain-swap across vendors mid-flow.** Hit a Claude rate limit → keep working on Codex → come back to Claude when the window resets. Per-agent capability negotiation; `/model` and `/effort` synthesized from each agent's `InitializeResponse`. Impossible inside any single-vendor tool.
- **Session resume via event replay.** Close the laptop, restart SPUR, the brain picks up exactly where it left off. Not a soft-reconnect — a full replay from an event-sourced log.
- **Local-first durability.** Plans persist as beads epics in SQLite, events as NDJSON, outcomes as content-addressed git blobs. Survives crashes, OS updates, and network outages — resilience cloud-only competitors cannot match without offline sync.
- **Review loop with Reflexion retry.** Every worker completion produces a review card — Approve / Reject / Modify / Retry. Retries feed prior attempts back as context (max 3, auto-reject on exhaustion). A first-class state machine, not a UI convenience.
- **Parallel delegation, isolated.** `delegate_to_worker`, `delegate_parallel`, and DAG-ordered `submit_plan` dispatch workers concurrently. Each gets its own git worktree on `spur/worker/v2/{agent}/...`, protected by advisory `flock` liveness probes and an orphan-collector.
- **Cherry-pick in DAG order.** Approved subtask commits are cherry-picked in topological order onto a staging branch. Collapses the post-worktree merge tax that DIY tmux+worktree users hit every day.
- **Telegram bot on the same state machine.** `spur-bot` mirrors review cards and session streams into Telegram forum topics. Same review lane, same event bus, same approval semantics as the TUI — review on the train, not just at the desk.
- **Native ACP + MCP dual channel.** Structured JSON-RPC, not PTY scraping or prompt injection. Any ACP-speaking agent works out of the box.
- **Durable plan mutations.** `submit_plan_mutation` supports split / replace / amend in-flight; the reconciler ticks adaptively, validates ownership, and previews overlay conflicts.
- **Multi-brain safety.** Plans are locked to a single active brain via a `spur:plan-owner:<id>` label. Tier-1 mutating tools fail on session mismatch. `force_reclaim_plan` breaks deadlocks when a brain wedges.
- **Structured delegation reasoning.** Brains pass a `DelegationPlan` (candidates, rationale, decomposition) with every dispatch. SPUR verifies the chosen agent matches the actual payload — preventing the LLM from lying to its own reviewer.
- **Stable-identity code graph.** Tree-sitter parses Rust / Python / TS / TSX / Markdown into a graph with SHA256 stable symbol IDs and incremental per-file rebuilds. Auto-discovered at the worktree root — no env var setup.
- **Built for orchestration, not chat.** Dashboard with lineage tree, full-screen ReAct trace with inline markdown + mermaid, grouped `@mention` picker (Workers / Files / Issues / Code Graph), `Ctrl+K` universal palette, Plan Inspector (`Alt+P`), live status bar (running count, pending reviews, total cost, model labels).

## What SPUR is NOT

SPUR is built to sit *above* the agents and tools you already use. We don't try to replace them — and several excellent products own jobs SPUR explicitly does not compete on:

- **Not a Devin replacement.** Devin owns "autonomous engineer I can assign tickets to in Slack." SPUR keeps the human in the loop on purpose — review is a state machine, not a slogan. If you want a fully autonomous agent, hire Devin.
- **Not a Cursor competitor.** Cursor owns the AI IDE. SPUR sits *next to* your editor, not inside it. Most SPUR users keep Cursor open in another window.
- **Not an Aider replacement.** Aider owns the simplest free BYO-key single-agent CLI. SPUR's value materializes at fleet size ≥ 2 — "add SPUR above Aider," not "switch from Aider." Aider-as-a-SPUR-worker is on the roadmap.
- **Not a Claude Code replacement.** SPUR uses Claude Code as a worker. We don't try to beat the in-session UX; we add what Claude Code doesn't attempt — cross-session durability, cross-vendor failover, and a cost ledger that spans every CLI.
- **Not autonomous.** Review gate is required by design. If you want a "set and forget" system, this is the wrong tool.

And on the boring scope side:

- Not a chat UI. Every interaction is task-structured.
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
curl -sSL https://getspur.dev/install.sh | sh
```

Installs the signed `spur` binary. Free Community tier — no key required. Pro / Team / Enterprise unlock via a signed license file from your account dashboard.

> Note: install domain is provisional; final URL will be confirmed at launch.

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

## License

SPUR is a proprietary product. The free Community tier is available without a license key under the terms of the EULA at [https://getspur.dev/eula](https://getspur.dev/eula). Pro / Team / Enterprise tiers require a signed license file. See [LICENSE](LICENSE) for terms.

> The source in this repository is not open source. Issues, feedback, and feature requests are welcome via [https://getspur.dev/feedback](https://getspur.dev/feedback) or your team's support channel.

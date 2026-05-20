# Quickstart — SPUR in 5 minutes

One brain, many workers, one review surface. By the end of this page you will have installed SPUR, dispatched a plan, approved a worker's diff, and seen your live cross-vendor cost. No signup. No license key. No second tool to learn.

SPUR's first job is to not break anything you already have. It reads the agents you already use (Claude Code, Codex, Gemini, OpenCode, Kimi) and the worktrees you already wrote scripts to manage. Keep your tmux. Keep your agents. SPUR sits next to them.

---

## 1. Install (30 seconds)

```sh
curl -sSL https://getspur.dev/install.sh | sh
```

What this does:

- Downloads the signed `spur` binary for your platform (macOS / Linux, x86_64 + arm64).
- Verifies the Ed25519 signature against the public key embedded in the installer.
- Drops the binary at `/usr/local/bin/spur` (or `~/.local/bin/spur` if `/usr/local/bin` is not writable). No daemons. No background services. No PATH edits beyond that single binary.

The Community tier is free and runs without a key under SPUR's EULA. You do not need an account to read every section below.

> The install domain is provisional and will be confirmed at launch.

Verify:

```sh
spur --version
```

---

## 2. Configure (60 seconds)

From the root of any git repository:

```sh
cd your-project
spur init
```

`spur init` scans your `$PATH` for installed agents and writes a `.spur/config.toml` with sensible defaults. It does not modify your git config, your shell rc files, or your existing worktrees. If you do not like what it wrote, delete `.spur/` and nothing else changes.

A typical first config looks like this:

```toml
[brain]
agent = "claude-code-acp"

[agents.entries.claude-code-acp]
role  = "brain"
transport = "native"

[agents.entries.codex]
role  = "worker"
transport = "stdio"
good_for = ["rust", "tests"]
tier  = 1

[worktree]
enabled = true

[cost]
db_path = "~/.spur/cost.db"
```

The brain is the agent that reasons about the task and decides what to delegate. Workers run delegated subtasks in isolated git worktrees under `spur/worker/v2/<agent>/...`. If only one agent is installed, SPUR configures it as both brain and worker — fine for a first plan.

Lint the result:

```sh
spur config check
```

---

## 3. Your first plan (2 minutes)

Launch the TUI:

```sh
spur
```

In the prompt at the bottom of the screen, type:

> Refactor error handling across `src/api.rs`, `src/db.rs`, and `src/worker.rs` — convert `unwrap()` calls to `?` with proper error types.

Press Enter. The brain reads the three files, drafts a 3-task plan, and submits it. You will see one row per task in the Lineage panel on the left. Each task spawns a worker in its own worktree. The status bar shows live token spend per session.

When the first worker finishes, a **review card** appears in the right-hand pane:

- **Approve** (`a`) — accept the diff; the commit lands on a staging branch.
- **Reject** (`r`) — send the worker back with a note. Reflexion retry, up to 3 attempts.
- **Modify** (`m`) — open the diff in `$EDITOR` and edit before approving.

Approve the first diff that looks right. That single keystroke is the activation moment — one approved review, one task closed. Repeat for the other two workers as they finish. When all three are approved, the plan is complete: three diffs cherry-picked in DAG order onto a staging branch, ready for you to `git switch` and merge however you normally merge.

If a worker takes longer than you want to watch, close your laptop. The plan is in `beads` (SQLite). The event log is in NDJSON. Reopen `spur` and the brain resumes via event replay — no soft-reconnect, no lost context.

---

## 4. See your cost (30 seconds)

Press `Alt+a` to open Insights.

The **Live** tab shows every active session across every vendor — Claude, Codex, Gemini, OpenCode, Kimi — in one ledger. The **Breakdown** tab shows the same data aggregated for today, this week, this month, split by agent and by repo.

This is the moment most users describe as the killer one. Two engineers in the public VOC corpus described agent spend that was ~5× what their finance teams thought it was. The reason was simple: every vendor reports its own bill in its own place. SPUR reads each vendor's local JSONL or SQLite ledger in place (no ETL, no cloud) and prints the actual aggregate. No peer can match this by design — Cursor sees Cursor, Claude Code sees Claude, Codex sees Codex. SPUR sees all five.

If a session is running on the wrong model, switch to it in the Lineage panel and type `/model gpt-4o` (or any model the agent supports). The status bar updates instantly.

---

## 5. Going further

You now have the loop: dispatch, review, approve, see cost. Everything else is depth.

- **Multi-task plans and DAGs** — `submit_plan` with explicit dependencies. See `docs/plans.md`.
- **Plan mutations** — split, retry, or reclaim stuck tasks mid-flight without losing siblings. See `docs/mutations.md`.
- **Telegram bot** — same review state machine as the TUI, on your phone. `spur bot setup`.
- **Session resume** — close the laptop mid-plan, come back tomorrow, type `spur`. Event replay does the rest.
- **Brain swap mid-flow** — hit a Claude rate limit, keep working on Codex, come back to Claude when the window resets. Set the brain to a second agent in `.spur/config.toml` and dispatch again.
- **Parallel workers, team cost dashboards, RBAC** — these are Pro and Team. The TUI surfaces an upgrade CTA the first time you hit a single-worker ceiling or need session resume across machines. No interruption to your current plan.

The smallest plan first. One brain. One worker. One approved review. If that loop feels good, the rest is additive — and reversible: `rm -rf .spur/` leaves no orphans behind.

# SPUR — One-Pager

**Tagline:** Issue in, PR out — across every agent, in parallel, with one review surface.

## What it is

SPUR is a Rust-native terminal orchestrator for AI coding agents. A "brain" agent reasons about a task and delegates work to one or more "worker" agents — Claude Code, Codex, Gemini, Kimi, OpenCode, or any agent that speaks the Agent Client Protocol (ACP). Each worker runs in its own isolated git worktree. SPUR coordinates dispatch, review, retries, cost, and plan state in one place — in the terminal, with a Telegram bot for review on the go.

Most SPUR users already pay for two or three of these agents and are tired of three things: copy-pasting context between tabs, losing work when Claude rate-limits them mid-flow, and not knowing what they're spending until the bill arrives.

## Three things only SPUR does

1. **A cost ledger that spans every vendor.** Five live extractors (Claude, Codex, Gemini, OpenCode, Kimi) feed a DuckDB engine that reads vendor JSONL/SQLite in place. No vendor can build this — they only see their own bill. We can.

2. **Brain-swap across vendors mid-flow.** Hit your Claude window? Keep working on Codex. Come back to Claude when it resets. Not session-pause-and-resume — full cross-vendor failover with the plan intact.

3. **Durable plans that survive everything.** Plans live in SQLite (beads), events in NDJSON, outcomes in git blobs. Close your laptop, lose your network, take a flight — the brain picks up exactly where it left off via event replay.

## Pricing

| Tier | Monthly | Annual | Lifetime |
|---|---|---|---|
| Community | $0 | $0 | — |
| Pro | $19 / seat | $182 / seat | $290 one-time |
| Team | $49 / seat (min 3) | $470 / seat | — |
| Enterprise | Contact sales | — | — |

Pro is priced deliberately below Claude Code Max ($100/mo) — SPUR is meant to sit *next to* the agents you already pay for, not replace them.

## Install

```
curl -sSL getspur.dev/install.sh | sh
```

Signed Rust binary. No Node, no Python, no Docker. Community tier runs without a license key under our EULA.

## Screenshots available (see `screenshots-list.md`)

1. Insights cost ledger — cross-vendor spend, live
2. Lineage tree — what every executor is doing, collapsible
3. Review card — Approve / Reject / Modify / Retry
4. Plan inspector — DAG view of in-flight tasks
5. Brain-swap moment — Claude → Codex mid-plan
6. Telegram approval — same review state machine, on the phone
7. Install-script terminal recording
8. Pricing page screenshot

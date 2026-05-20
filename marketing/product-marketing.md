# Product Marketing Context — SPUR

*Last updated: 2026-05-20 — V1.1 — auto-draft + self-brainstormed proposals for pricing, goals, language, metrics. All proposed values flagged inline; confirm before launch.*

## Product Overview
**One-liner:** Issue in, PR out — across every agent, in parallel, with one review surface.
**Alternate (from PRD):** One brain, many workers, zero lost context.
**What it does:** SPUR is a Rust-native terminal orchestrator for AI coding agents. A "brain" agent reasons about a task and delegates work to one or more "worker" agents (Claude Code, Codex, Gemini, Kimi, OpenCode, or any ACP-speaking agent). Each worker runs in its own isolated git worktree. SPUR coordinates dispatch, review, retries, cost, and PM state in one place.
**Product category:** AI coding agent orchestrator / multi-agent terminal IDE-companion. "Shelf" customers search from: *Claude Code wrapper*, *multi-agent CLI*, *agent orchestrator*, *Claude Code Max rate-limit fix*.
**Product type:** Open-source developer tool (Rust binary, `cargo install spur-cli` / `curl | sh`) + tiered commercial license (Community / Pro / Team / Enterprise).
**Business model:** Open-core. Community free forever; Pro / Team / Enterprise gated by signed Ed25519 policy documents.

**Proposed pricing** *(self-brainstormed — confirm before launch)*:

| Tier | Monthly | Annual (–20%) | Lifetime |
|---|---|---|---|
| Community | $0 | $0 | — |
| Pro | **$19 / seat / mo** | $182 / seat / yr | **$290 one-time** *(matches `personal_lifetime` alias in `spur-license/src/lib.rs:83`)* |
| Team | **$49 / seat / mo** (min 3 seats) | $470 / seat / yr | — |
| Enterprise | **Contact sales** (est. $25k+/yr floor) | — | — |

Rationale: Pro priced **below** Claude Code Max ($100/mo) so it's an obvious add-on not a substitute. Team 2.5× Pro reflects shared cost dashboard + RBAC + webhooks. Lifetime mirrors the existing `personal_lifetime` plan key already in the license crate. Enterprise floor sized for orgs needing SSO + audit + custom policies.

## Target Audience
**Target companies:** Startups and mid-size eng orgs (10–200 employees) where developers already pay for Claude Code Max + Codex + Kiro/Gemini and feel the vendor sprawl. Also: solo power-users with $200–600/mo personal AI spend.
**Decision-makers:** Bottom-up adoption — individual senior/staff engineers install it. Team tier upsell sells to engineering managers / tech leads. Enterprise sells to platform/devex teams.
**Primary use case:** Run multiple AI coding agents in parallel on the same repo without (a) worktree collisions, (b) losing context to rate limits / closed laptops, (c) flying blind on cost.
**Jobs to be done:**
- "Don't lose my Claude Code session when I hit the 5-hour limit at hour 1."
- "Let me dispatch 5 refactors in parallel and review the diffs in one place, on my phone if needed."
- "Tell me what I'm spending across all five vendors, right now, per session."
**Use cases:**
- Parallel refactor across a large codebase (5-task plan → 5 worktrees → review cards → cherry-pick approved).
- Overnight plan execution with morning review on Telegram.
- Rate-limit failover: start on Claude, resume on Kiro, come back to Claude later.
- Cross-agent model selection (`/model gpt-4o` mid-session on Codex from the same UI).

## Personas

| Persona | Cares about | Challenge | Value we promise |
|---|---|---|---|
| **The Orchestrator** (Sr/Staff eng, tmux/zellij native, $200–600/mo AI spend) | Flow preservation, auditability | Hits Claude rate limits 2–5×/wk, juggles 2–3 terminals | "Start Friday, review Saturday on Telegram, merge Monday — without losing context." |
| **The Team Lead** (EM over 3–10 devs) | Cost visibility, standardization, governance | No view into team agent usage, per-project spend, or review depth | "See pending reviews, per-project costs, and which agents actually deliver merged code." |
| **The Mobile Operator** (dev away from desk) | Approve/reject worker output anywhere | Terminal-only chains you to the desk | Telegram bot with inline review buttons + push. |

## Problems & Pain Points
**Core problem:** Running >1 AI coding agent today means living with **collision** (two agents stomping the same tree), **opacity** (no unified diff/approve/retry surface), and **cost blindness** (tokens accruing across 5 vendors with no single ledger). And Claude Code Max users blow through 5-hour windows in <90 min and lose context.
**Why alternatives fall short:**
- Single-agent CLIs (Claude Code, Codex CLI, Kiro CLI) don't talk to each other — developers copy/paste context between tabs.
- Web orchestrators (Agent Orchestrator-style) live in the browser, not the terminal, and have no native ACP.
- PTY-scraping wrappers can't do structured review or durable plans.
- No tool today treats human review as a state machine — it's always a UI afterthought.
**What it costs them:** Lost work (closed terminal = lost task), repeated context-setting after rate limits, surprise monthly AI bills, manual coordination overhead measured in hours/week.
**Emotional tension:** The dread of hitting a rate limit mid-flow. The frustration of explaining the same context to a third agent. The anxiety of not knowing if you're burning $100 or $1000 today.

## Competitive Landscape

**Direct (multi-agent orchestrators):**
- **ACPX** — Node.js, CLI only. Falls short: no durable plans, no review gate, no session resume.
- **TUICommander** — Rust+Tauri desktop. Falls short: PTY scraping (not native ACP), no plan persistence, no review gate.
- **Ralph** — TypeScript TUI. Falls short: read-only, partial ACP.
- **Agent Orchestrator** — Node.js web dashboard. Falls short: YAML plans only, no review gate, browser-bound.

**Secondary (single-agent CLIs people use today):**
- **Claude Code** — Claude only, no cross-agent, no durable plans, no resume after rate-limit.
- **Codex CLI / Kiro CLI / Gemini CLI** — single-vendor silos; users manually shuttle context between tabs.

**Indirect (DIY alternatives):**
- **tmux + N terminal panes + manual git worktree juggling** — what Orchestrators do today. Falls short: zero durability, no cost view, no review gate.
- **Cloud agent platforms (Devin, Cosine, etc.)** — Falls short: not local, opaque cost, no terminal flow, no BYO-agent.

## Differentiation
**Key differentiators:**
- Rust single binary — `cargo install spur-cli` (no Node, no Python, no Docker).
- Native ACP + MCP dual channel — structured protocol, not PTY scraping.
- Local-first durability — plans in SQLite (beads), events in NDJSON, outcomes in git blobs. Survives crashes, OS updates, network outages.
- Human review as a first-class state machine with timeout / retry / merge gating (not a UI convenience).
- Session resume via event replay — close laptop, restart SPUR, brain picks up exactly where it left off.
- Cross-vendor orchestration with per-agent capability negotiation (`/model`, `/effort` synthesized from each agent's `InitializeResponse`).
- Telegram bot shares the same review lane and event bus as the TUI.
- Cherry-pick of approved subtask commits in DAG order onto a staging branch.
- Reflexion-style retries (prior attempts fed back as context, max 3).
- Multi-brain safety via `spur:plan-owner:<id>` label + tier-1 mutation guards.
**How we do it differently:** SPUR is a *distributed-systems kernel for agent execution disguised as a TUI*. The real product isn't the panels — it's the event-sourced lineage, the durable plan reconciler, and the dual-channel protocol architecture.
**Why that's better:** Closing your laptop doesn't lose context. A worker dying doesn't lose 11 sibling tasks. Hitting a Claude rate limit doesn't kill the plan. Reviewing on your phone uses the same state machine as reviewing in the TUI.
**Why customers choose us:** They're already paying for 3+ agents and want one review surface, one cost ledger, and the ability to walk away from the terminal without losing the loop.

## Objections

| Objection | Response |
|---|---|
| "I just use Claude Code, why do I need this?" | If you're a Max user hitting limits or wanting to try Codex/Gemini for specific tasks, SPUR lets you switch brains mid-session without re-explaining context. If you're happy with one vendor, you don't need SPUR yet. |
| "Another tool to configure?" | One `spur init` auto-detects installed agents and writes sensible defaults. Quickstart is 2 commands. |
| "Why a TUI in 2026?" | Because that's where coding agents already live. SPUR sits *next to* your terminal, not in a browser tab. (And the Telegram bot covers mobile.) |
| "Will it work with my custom agent?" | Any ACP-speaking agent works out of the box. Capability negotiation handles slash commands per-agent. |
| "Open source — what stops me from running everything for free?" | Community tier is genuinely generous (1 brain, 1 worker, full review loop, full cost display, full lineage). Pro/Team gate parallelism, session resume, and team analytics — features that only matter once you've outgrown solo use. |
| "Is human review really required? I want full autonomy." | Yes by design — SPUR explicitly is not a set-and-forget autonomous system. If you want that, look elsewhere. |

**Anti-personas (NOT for):**
- Junior devs using one AI assistant casually.
- Non-technical users who need a GUI.
- Enterprise teams requiring SOC2/HIPAA at launch.
- Users who want a fully autonomous, no-human-in-loop system.

## Switching Dynamics (JTBD Four Forces)
**Push (away from status quo):** Lost work from closed terminal mid-task. Hitting rate limit at hour 1 of 5. Surprise bills. Copy-pasting context between Claude / Codex / Kiro tabs. Two agents corrupting the worktree.
**Pull (toward SPUR):** "Issue in, PR out." Parallel dispatch with one review surface. Telegram review on the go. Live cost in the status bar. Brain swap mid-flow.
**Habit (what keeps them stuck):** tmux muscle memory. "I'll just open another tab." Already paid Claude Max — don't want a second tool.
**Anxiety (what worries them about switching):** "Will this slow me down?" "Will it break my git tree?" "Yet another config file?" "What if SPUR crashes mid-plan?" (Answer: plans are durable in beads — they survive.)

## Customer Language

**Status:** Provisional — drawn from common HN / r/ClaudeAI / r/LocalLLaMA pain patterns. Replace with real verbatim quotes after F2 (customer-research) completes.

**Provisional verbatim — how they describe the problem:**
- "Claude Code just told me I'm out of tokens for 4 more hours."
- "I had to copy-paste my whole architecture into Codex because Claude was rate-limited."
- "Two agents touched the same file and now my branch is broken."
- "I have no idea what I spent on Claude this month."
- "I closed the terminal and lost two hours of agent work."
- "I want to run 5 refactors at once but I only have one Claude Code window."
- "tmux + worktrees is fine until it isn't."

**Provisional verbatim — how they describe SPUR / what they want:**
- "It's like a control plane for my agents."
- "Finally, one place to approve PRs from all of them."
- "I can review on my phone now."
- "Rate-limit-proof Claude Code."
- "Issue in, PR out — that's the whole pitch."

**Language to use** (developer-native, ground in pain):
- "Don't lose context."
- "Issue in, PR out."
- "One review surface."
- "Cherry-pick approved diffs."
- "Worktree per worker."
- "Cost ledger."
- "Brain / worker / review."
- "Rate-limit-proof."

**Words to avoid:**
- "AI-powered" (lowest-common-denominator marketing speak)
- "Synergy", "platform", "ecosystem" (enterprise blandness)
- "Autonomous agents" (we explicitly require human review)
- "Replaces your developer" (we augment, not replace)
- "Vibe coding" (audience is power-users, not casual prompters)
- "Revolutionary" / "next-gen" — show, don't claim

**Glossary:**
| Term | Meaning |
|---|---|
| Brain | The orchestrator agent that reasons about the task and decides what to delegate |
| Worker | A subordinate agent that executes a delegated subtask in its own worktree |
| Review card | The Approve / Reject / Modify / Retry surface for a completed worker attempt |
| Plan | A DAG of tasks dispatched via `submit_plan`, persisted in beads |
| Beads | Local-first SQLite issue tracker SPUR uses as plan store |
| ACP | Agent Client Protocol — JSON-RPC over stdio between SPUR and agents |
| MCP | Model Context Protocol — tool calls from agents back into SPUR |
| Lineage | Collapsible ASCII tree showing what every executor is doing |
| Worktree | Isolated git checkout under `spur/worker/v2/{agent}/...` |

## Brand Voice
**Tone:** Direct, technically precise, no marketing fluff. Confident but grounded — "production-hardened" not "world-changing." Self-aware about being early-stage.
**Style:** Show the code path / state machine / config block. Use diagrams (the README's ASCII flow is on-brand). Resist superlatives; let capability lists speak.
**Personality (5 adjectives):** Rigorous. Pragmatic. Terminal-native. Developer-respectful. Self-aware.

## Proof Points
**Metrics / capabilities to cite (grounded in PRD):**
- ~100K lines of Rust across 13 crates.
- 5 live cost extractors (Claude, Codex, Gemini, OpenCode, Kimi).
- Survives crashes / OS updates / network outages (beads + NDJSON event log).
- Reflexion retry with max 3 attempts.
- Session resume = event replay (not soft-reconnect).
- DuckDB analytics engine reads vendor JSONL/SQLite in place — no ETL.

**Customers / testimonials:** None captured yet (pre-launch). **Launch-blocker:** secure 3–5 named-user quotes from the first 50 Community installs before Phase 4 (Launch). Process: ship a `spur feedback` command + a 2-question post-install survey; mine GitHub issues + Discord for quotables.

**Value themes:**
| Theme | Proof |
|---|---|
| Don't lose context | Event-sourced lineage + session resume via replay |
| Don't lose work | Beads durability — plans survive crash / OS update / network outage |
| Don't lose money | Live per-session cost in status bar; DuckDB cross-vendor analytics |
| Don't lose review discipline | Review gate as state machine, not UI convenience |
| Bring your own agent | Any ACP-speaking agent + per-agent capability negotiation |

## Goals

**Primary business goal:** Become the default terminal companion for any developer running 2+ AI coding agents. Drive Community adoption among Claude Code Max users → convert to Pro on first parallel-worker or session-resume need.

**Conversion funnel (proposed):**

| Stage | Action | Channel |
|---|---|---|
| Awareness | First mention | HN Show / Product Hunt / X / Anthropic DevRel co-marketing |
| Trial | `cargo install spur-cli` + `spur init` | README + curl-pipe install |
| Activation | First successful `submit_plan` with ≥2 reviewed approvals | TUI quickstart + onboarding emails |
| Pro upgrade | Hit single-worker limit OR rate-limit failover OR session-resume need | In-TUI upgrade CTA (`spur-license/src/upgrade_cta.rs` already exists) |
| Team upgrade | EM wants cost dashboard / RBAC across N seats | Sales-led, triggered by 3+ Pro seats on same domain |

**Pro tier conversion action:** *Proposed* checkout URL: `https://getspur.dev/pro` (placeholder — domain + Stripe/Paddle setup is a launch-blocker). Lifetime SKU links to the existing `personal_lifetime` license plan key.

**Target metrics (proposed, 90 days post-launch):**

| Metric | Day 30 | Day 60 | Day 90 |
|---|---|---|---|
| GitHub stars | 1,000 | 3,000 | 7,500 |
| `cargo install` count | 500 | 2,000 | 6,000 |
| Activated installs (≥1 review approved) | 150 | 700 | 2,400 |
| Pro paid conversions | 10 | 50 | 175 |
| Pro MRR (assuming $19 + $290 lifetime mix) | $300 | $1,500 | $5,000 |
| Team deals | 0 | 1 | 4 |
| Telegram bot users | 50 | 300 | 1,000 |

**Current metrics:** None — pre-launch. Telemetry already shipped (`SPUR_TELEMETRY=1`, opt-in Tier-2) — wire up install / activation counters before Phase 4.

**North-star metric:** % of weekly active SPUR sessions where ≥2 different agent vendors are used. Captures the "multi-agent orchestrator" thesis better than installs alone.

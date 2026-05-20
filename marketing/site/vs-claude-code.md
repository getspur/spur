# SPUR vs Claude Code

*Last updated: 2026-05-20. This page is a peers-not-competitors comparison. Claude Code is Anthropic's first-party agentic CLI — the best in-session single-agent coding experience available today, and the source of the entire VOC corpus that motivated SPUR. SPUR is the layer above it. If you came here looking for a "Claude Code replacement," you're on the wrong page; keep reading and we'll explain why.*

---

## TL;DR

**You already use Claude Code. SPUR is the layer above it.**

Claude Code is the best in-session single-agent CLI on the market — first-party model, first-party agent, tightest UX, and the canonical Pro/Max bundle. SPUR doesn't compete with any of that. SPUR runs Claude Code as one worker in a fleet, adds cross-session durability, cross-vendor failover, and a unified cost ledger across every CLI you run.

This page is not a switch pitch. It's a "what to use when" page.

---

## When to use which

A concrete decision matrix, written for the developer who is already paying Anthropic and wants to know whether SPUR adds anything.

| Your situation | Use only Claude Code | Add SPUR on top |
|---|---|---|
| You run **one** Claude Code session at a time | ✅ Yes — SPUR adds nothing here | ❌ Wait until you're juggling ≥ 2 sessions |
| You're inside the Anthropic rate-limit envelope and rarely trip it | ✅ Yes | ❌ Not yet |
| You **trip Pro/Max rate limits** during a sprint and your work stalls | — | ✅ Brain-swap to Codex / Gemini / GLM, then come back |
| You run **5–10 Claude Code agents** across worktrees and forget which is waiting on you | — | ✅ One review queue, one status grid |
| Your only AI bill is the Anthropic one | ✅ Yes | ❌ Cost ledger is overkill |
| You pay **Anthropic + OpenAI + Google + Z.AI** and can't see the total | — | ✅ Unified live ledger across every vendor |
| You closed the laptop and lost two hours of agent context | — | ✅ Plans survive in beads + NDJSON |
| You want to review a diff from your phone while away from the desk | — | ✅ Telegram bot shares the same review lane |
| You want inline diffs, Composer chat, an editor-native experience | ✅ Claude Code (or Cursor) — SPUR is not an editor | ❌ |
| You want an autonomous engineer that owns tickets in Slack | ❌ — that's Devin, not Claude Code and not SPUR | ❌ |

**The honest rule:** if your fleet size is 1 and your bill is one-vendor, stay on Claude Code. SPUR's value materializes at fleet size ≥ 2 or vendor count ≥ 2.

---

## What SPUR adds on top of Claude Code

Three things, and only three things. Each one is something Claude Code does not attempt by design — not a flaw, just a deliberate scope choice.

### 1. Cross-session durability

Claude Code's Task tool and subagents are **in-session and ephemeral** — they return summaries to the orchestrating agent and disappear when the session ends. That's the right design for an in-session coding agent.

SPUR adds a persistence layer on top: every plan, every event, every review-queue entry lives in beads + an NDJSON event log. Close the laptop, OS update, network drop — the plan resumes via event replay. You don't lose the two hours.

### 2. Cross-vendor failover

Claude Code only runs Claude. That's correct — it's Anthropic's first-party agent and the in-session UX is tuned for Anthropic's models.

SPUR's brain layer can swap to Codex, Gemini, GLM, Kimi, or OpenCode mid-flow. The canonical use case: you're three days into the Max weekly window, you hit a rate limit, you fail over to Codex without abandoning the plan, you come back to Claude Code when the window resets. **None of Devin, Cosine, Cursor, Aider, or Claude Code can do this — by design.**

### 3. Unified cost ledger

Anthropic surfaces per-session Claude usage. That covers the Anthropic bill.

SPUR aggregates spend across Claude, Codex, Gemini, OpenCode, and Kimi by reading each vendor's JSONL/SQLite in place — no ETL, no proxy, no API key relinquishing. One number, today's spend, every vendor. This is the one differentiator no single-vendor agent can match, and Claude Code shouldn't try to — it would mean reading competitors' logs.

---

## What we keep using Claude Code for

Inside a SPUR fleet, Claude Code is typically **the worker we reach for first** for a task. Specifically:

- **In-session coding UX.** File-level permission prompts, the streaming diff, the way it negotiates clarifications. Claude Code is the agent everyone is wrapping, including SPUR.
- **Anthropic's models.** Sonnet/Opus consistently top developer satisfaction polls; the in-session prompt scaffolding is tuned for them. SPUR doesn't try to recreate this — it dispatches *into* it.
- **Subagent fan-out within a single task.** Claude Code's Task tool runs up to ~7 in-session subagents for file reads, code searches, and web fetches. That's the right abstraction for sub-tasks of one job. SPUR's worker fleet is the abstraction for *separate* jobs across separate worktrees — a different scope, not a competing one.
- **Pro/Max distribution.** Most SPUR users already pay Anthropic and already have Claude Code installed. SPUR is additive to that, not a replacement for the subscription.

If you're inside a session and choosing the agent to do the actual coding, the answer is usually Claude Code. SPUR's job is everything *between* and *around* sessions.

---

## Side-by-side capability matrix

This matrix is deliberately picked from dimensions that do **not** pit Claude Code and SPUR against each other — they sit at different layers of the stack. We've left out in-session UX, model quality, and editor integration on purpose; Claude Code wins those and a comparison would be a category error.

| Capability | Claude Code (standalone) | Claude Code + SPUR |
|---|---|---|
| In-session coding UX | Best-in-class | Unchanged — SPUR delegates to Claude Code |
| Model selection (in-session) | Anthropic (Sonnet / Opus) | Anthropic (delegated to Claude Code, unchanged) |
| Parallel sub-tasks within one job | Up to ~7 in-session subagents | Up to ~7 in-session subagents (SPUR doesn't touch this) |
| **Parallel jobs across worktrees** | Manual (tmux, your own scripts) | Managed worker fleet with DAG-ordered merge |
| **Rate-limit recovery** | Wait for the window to reset | Brain-swap to Codex / Gemini / GLM, resume on Claude when window reopens |
| **Cross-vendor cost view** | Anthropic only (per-session) | Unified live ledger across Claude, Codex, Gemini, Kimi, OpenCode |
| **Plan durability across crashes / closed laptops** | Session-scoped | Beads + NDJSON event log, resume via replay |
| **Mobile review** | iOS hand-off (read / send prompts) | Telegram bot on the same review state machine as the TUI |
| **Cross-session review queue** | n/a (in-session only) | First-class review lane with timeout / retry / merge gating |
| Distribution | Bundled with Pro $17–20 / Max $100–200 | `cargo install spur-cli`, additive to whatever you already pay |

The whole matrix is read as: *Claude Code owns the row inside one session; SPUR owns the row across many.*

---

## Who's best for who

**Stay on Claude Code alone if:**
- You run one session at a time
- Your weekly spend stays inside the Max envelope
- You don't pay any other model vendor
- You're happy with iOS hand-off as your "away from desk" surface

**Add SPUR if:**
- You run 5–10 CLI agents at a time across worktrees (the canonical wedge persona — Beefin, HN 47104424)
- You've been ambushed by Pro/Max rate limits mid-sprint and want a brain-swap path
- You pay multiple model vendors and can't see one total
- You want a review queue that survives a closed laptop and accepts a "merge" tap from your phone

**Neither of us is for you if:**
- You want an autonomous engineer you assign tickets to in Slack — that's **Devin**.
- You want an AI IDE with inline diffs and Composer-style chat — that's **Cursor**.
- You want the simplest possible single-agent CLI with no orchestration — that's **Aider**.

---

## Open question we owe the reader

Anthropic's roadmap is the obvious watch-list item: cross-session Task durability, a cross-session cost dashboard, or a multi-vendor adapter from Anthropic would each narrow what SPUR adds on top. If Anthropic ships any of those, this page gets shorter — and we'll keep it honest. Today, Claude Code's design is deliberately in-session, single-vendor, and per-session billing; that's the moat shape SPUR is built next to, not against.

---

## CTA

Keep Claude Code. Add SPUR.

- `cargo install spur-cli` — installs the control tower next to whatever agents you already run
- 60-second demo: a Claude Code rate-limit fail-over to Codex without losing the plan
- Docs: how SPUR dispatches into Claude Code as a worker (ACP + slash-command capability negotiation)

---

*Source files: `marketing/competitors/claude-code-standalone.md`, `marketing/competitors/_summary-indirect.md:19,23-26,36`, `marketing/research/themes.md` (themes #1–#3), `marketing/messaging/positioning.md:106-108`, `marketing/product-marketing.md:8`. Verbatim VOC: Beefin (HN 47104424), roxolotl (HN 44598254), gorbypark (HN 44713757).*

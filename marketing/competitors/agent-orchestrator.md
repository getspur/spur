# Agent Orchestrator (Composio AO)

*Profile date: 2026-05-20. All claims cite a primary source (repo, README, or official site). Schema follows the F3 task brief; the canonical SKILL.md was not on disk at profile time (`marketing/marketingskills/` is gitignored per `marketing/README.md:12-14`).*

## Identity

- **Name:** Agent Orchestrator (CLI/package name: `@aoagents/ao`, command `ao`).
- **Vendor:** Composio (composio.dev) — funded company, not a solo project.
- **Official site / parent org:** https://composio.dev (repo `homepage` field).
- **GitHub repo:** https://github.com/ComposioHQ/agent-orchestrator
- **Stars:** 7,141 (GitHub API, 2026-05-20). **The most-starred direct competitor in this set by ≈3×.**
- **Forks / watchers / open issues:** 966 forks / 26 watchers / 870 open issues. (High open-issue count → either heavy real usage or fast-and-loose triage; PR-merged badge in README claims **61 PRs merged**.)
- **License:** MIT.
- **Created:** 2026-02-13. **Last push:** 2026-05-19 (≤1 day stale — actively maintained).
- **Tests:** [README badge](https://github.com/ComposioHQ/agent-orchestrator#readme) advertises "3,288 test cases" — production-grade investment.

## Headline pitch

> "Spawn parallel AI coding agents, each in its own git worktree. Agents autonomously fix CI failures, address review comments, and open PRs — you supervise from one dashboard." — [README banner](https://github.com/ComposioHQ/agent-orchestrator#readme)

## Supported agents

Plugin slot `agent` with built-ins ([README "Plugin Architecture"](https://github.com/ComposioHQ/agent-orchestrator#plugin-architecture)):

- Default: **claude-code**
- Alternatives: **codex, aider, cursor, opencode, kimicode**

Tagline: "**Agent-agnostic** (Claude Code, Codex, Aider) · **Runtime-agnostic** (tmux, ConPTY/process, Docker) · **Tracker-agnostic** (GitHub, Linear)."

## Architecture

Seven-slot plugin architecture, lifecycle in core ([README "Plugin Architecture"](https://github.com/ComposioHQ/agent-orchestrator#plugin-architecture)):

| Slot | Default | Alternatives |
|---|---|---|
| Runtime | tmux (macOS/Linux) / process (Windows) | process, docker |
| Agent | claude-code | codex, aider, cursor, opencode, kimicode |
| Workspace | worktree | clone |
| Tracker | github | linear, gitlab |
| SCM | github | gitlab |
| Notifier | desktop | slack, discord, composio, webhook, openclaw |
| Terminal | iterm2 | web |

- **PTY substrate.** Runtime plugin = tmux (macOS/Linux) or ConPTY process (Windows) — not ACP-native. Agent control happens via terminal multiplexer.
- **Web dashboard.** `ao start` launches `http://localhost:3000` ([README "Quick Start"](https://github.com/ComposioHQ/agent-orchestrator#quick-start)). Browser is the primary UI; CLI is "mostly used by the orchestrator agent."
- **Orchestrator-agent pattern.** Top-level `ao` agent spawns worker agents per-issue; uses `ao` CLI internally to manage sessions. The orchestrator is itself an LLM agent, not a deterministic scheduler.
- **Workspace plugin:** worktree (default) or clone — each agent runs isolated.
- **Reactions** — declarative auto-responses to events in `agent-orchestrator.yaml`:
  ```yaml
  reactions:
    ci-failed:        { auto: true, action: send-to-agent, retries: 2 }
    changes-requested:{ auto: true, action: send-to-agent, escalateAfter: 30m }
    approved-and-green:{ auto: false, action: notify }
  ```
  *CI fails → agent gets logs and fixes. Reviewer requests changes → agent addresses. PR approved with green CI → notify human.*
- **Runtime data** lives at `~/.agent-orchestrator/{hash}-{projectId}/`.
- **Schema-validated config** (`agent-orchestrator.yaml` with `$schema` URL for editor autocomplete).
- **Remote access (macOS):** holds `caffeinate` idle-sleep assertion to keep the dashboard reachable over Tailscale-style remoting.

## Plan persistence model

- **Tracker is pluggable.** GitHub Issues, Linear, GitLab. "Each issue gets its own agent in an isolated git worktree" ([README "How It Works"](https://github.com/ComposioHQ/agent-orchestrator#how-it-works)).
- **No Beads support out of the box** (Beads is in SPUR's substrate and Ralph's). Direct issue-tracker integration is the persistence model — i.e., the issue tracker *is* the plan.
- Per-project config persists in `agent-orchestrator.yaml`; runtime state in `~/.agent-orchestrator/{hash}-{projectId}/`.

## Review / approval model

- **Hybrid: reactions are configurable per-event, default leaning auto.**
  - `ci-failed.auto: true` → automatic.
  - `changes-requested.auto: true` with `escalateAfter: 30m` → automatic with timeout escalation.
  - `approved-and-green.auto: false` → human notify, manual merge (with a "flip to true for auto-merge" comment in the example — explicitly inviting full automation).
- **Notifier plugin slot** (desktop, slack, discord, composio, webhook, openclaw) — output channel for review nudges.
- **No multi-stage proposal-queue review UI** documented in README. The dashboard is supervisory ("you supervise from one dashboard") rather than a strict diff-approval workflow.

## Pricing

- The OSS project is MIT/free. The repo carries no paid tier directly.
- **However:** parent company Composio (composio.dev) is a commercial venture — tool-integrations platform with paid plans. Agent Orchestrator is an *open-source funnel* into Composio's broader stack. Notifier slot includes `composio` as a built-in option, hinting at upsell.
- Underlying agent inference costs (Claude/Codex/etc.) bill to the user.

## Adoption signals

- **GitHub:** **7,141 stars** (≫ all three other competitors), 966 forks, 26 watchers. PR-merged badge: 61 PRs. 870 open issues (very high — heavy real-world usage, or aggressive intake without triage).
- **npm:** `@aoagents/ao` — README links it with `npm version` badge; nightly + stable channels.
- **Test coverage:** 3,288 test cases (advertised).
- **Discord community** linked from README ([discord.gg/UZv7JjxbwG](https://discord.gg/UZv7JjxbwG)).
- **Marketing investment:** dedicated demo video, X promo posts referenced ([demo tweet](https://x.com/agent_wrapper/status/2026329204405723180), ["The Self-Improving AI System That Built Itself" article](https://x.com/agent_wrapper/status/2025986105485733945)).
- **Cross-platform commitment:** macOS, Linux, Windows (with ConPTY runtime plugin for Windows-native operation).

## Top 3 strengths

1. **Highest market visibility & momentum.** 7.1k stars in ≈3 months, venture-backed parent (Composio), dedicated demo content. This is *the* multi-agent orchestrator brand most prospects will Google-find first. SEO and AI-citation share-of-voice already concentrated here.
2. **Clean, extensible plugin spec — 7 slots, all swappable.** "All interfaces defined in `packages/core/src/types.ts`. A plugin implements one interface and exports a `PluginModule`. That's it." This is the architecture pattern other orchestrators (including SPUR) will be benchmarked against.
3. **Reactions DSL is genuinely good.** Declarative `agent-orchestrator.yaml` with per-event `auto` flags + escalation timeouts is the cleanest way anyone in this set has formalized "what should happen when CI fails / when a reviewer comments". Worth borrowing as inspiration.

## Top 3 weaknesses vs SPUR

1. **Auto-merge-by-default culture conflicts with the F2 customer.** README example explicitly invites flipping `approved-and-green.auto` to `true` for auto-merge. The F2 finding — operators want to *queue and decide* — places SPUR as the safer/calmer alternative. AO optimizes for "agent ships PR, you maybe glance"; SPUR optimizes for "agent proposes, you approve". Positioning angle: AO is *fast and loose*; SPUR is *fast and reviewable*.
2. **No native cost ledger across agents.** Notifier plugins fan out events to Slack/Discord/desktop, but there is no aggregated *spend* dashboard in the documented feature set. Cost opacity (`marketing/research/themes.md:23-34`) — the sharpest VOC pain — is unaddressed.
3. **PTY/tmux substrate has the same fragility as Ralph + TUICommander.** Default runtime is tmux; ConPTY on Windows. The orchestrator-agent-driving-an-agent pattern compounds reliability cost (the meta-agent itself can hallucinate). SPUR's deterministic brain/worker dispatch + ACP-native option (where available) reads as the more grown-up runtime.

## Notes for downstream Phase-3 work

- **AO is the most threatening competitor today.** 3× the stars, venture backing, dedicated demo content, broader plugin ecosystem. The vs-AO page is the highest-leverage Phase-3 asset.
- **Don't argue plugin-architecture parity** (AO wins on extensibility breadth — 7 slots, 6 alternatives per slot). Argue *review discipline* and *cost discipline*: AO is built to remove humans from the loop; SPUR is built to make the human's loop fast.
- **Lift the reactions-DSL pattern as inspiration** — SPUR's review-gate config likely benefits from a similar declarative shape (`on: ci-failed → action: queue-for-review` etc.).
- TODO for downstream: confirm whether Composio's paid plans gate any AO features behind a paywall (currently no evidence the OSS repo is feature-limited). Source: https://composio.dev/pricing.
- TODO: pull npm download counts for `@aoagents/ao` to quote scale.

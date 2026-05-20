# TUICommander

*Profile date: 2026-05-20. All claims cite a primary source (repo, README, or official site). Schema follows the F3 task brief; the canonical SKILL.md was not on disk at profile time (`marketing/marketingskills/` is gitignored per `marketing/README.md:12-14`).*

## Identity

- **Name:** TUICommander.
- **Tagline (self-description):** "The IDE that understands AI agents." — [README](https://github.com/sstraus/tuicommander#readme) / [tuicommander.com](https://tuicommander.com).
- **Official site:** https://tuicommander.com (docs at https://tuicommander.com/docs/).
- **GitHub repo:** https://github.com/sstraus/tuicommander
- **Stars:** 66 (GitHub API, 2026-05-20).
- **Forks / watchers:** 11 forks / 1 watcher.
- **License:** Apache-2.0.
- **Created:** 2026-02-17. **Last push:** 2026-05-19 (≤1 day stale — actively maintained).
- **Distribution:** signed/notarized desktop app for macOS, Linux, Windows. [Homebrew tap](https://github.com/sstraus/tuicommander#get-started), shell install scripts, nightly `tip` channel rebuilt on every `main` push.

## Headline pitch

> "**TUICommander is an AI-native IDE** — designed from the ground up for multi-agent development. Agents, code, diffs, PRs, CI status, and usage analytics live in one window. No context switching. No lost threads." — [README "The solution"](https://github.com/sstraus/tuicommander#readme)

## Supported agents

Auto-detects **10 AI coding agents** ([README "Agent observability"](https://github.com/sstraus/tuicommander#readme)):

- Claude Code, Codex CLI, Aider, Gemini CLI, Amp, Cursor Agent, OpenCode, Warp Oz, Droid, Goose.

## Architecture

- **PTY-based observation, not ACP.** Native terminal via `alacritty_terminal` + canvas rendering ([README "Technology Stack"](https://github.com/sstraus/tuicommander#readme)). Agent observability is provider-specific *output-parsing*: "**Rate limit detection** — Provider-specific patterns with countdown timers per session", "**Question detection** — Y/N prompts, numbered options, inquirer-style menus".
- **Tauri v2 + Rust backend, SolidJS UI** — desktop app, not a server. Built with Vite + LightningCSS.
- **Up to 50 concurrent PTY sessions**, split panes, detachable tabs ([README "Terminal features"](https://github.com/sstraus/tuicommander#readme)).
- **Auto-managed git worktrees** — branch click → worktree auto-created; `Cmd+Shift+W` Worktree Manager across all repos with orphan detection and batch ops ([README "Git worktrees, fully managed"](https://github.com/sstraus/tuicommander#readme)).
- **MCP Proxy Hub** — aggregates upstream MCP servers behind one endpoint; auto-configures Claude Code, Cursor, Windsurf, VS Code, Zed, Amp, Gemini.
- **Plugin system** — Obsidian-style with hot reload; 5 capability tiers from read-only watchers to scoped Tauri invoke. SDK: `tuic.activeRepo`, `tuic.toast`, `tuic.onRepoChange`.

## Plan persistence model

- **Session-aware resume** — auto-discovers agent session IDs from disk (Claude Code, Gemini CLI, Codex CLI). Reattaches to existing sessions.
- **Terminal session persistence** — "terminals survive restarts with lazy restore on branch click" ([README "Terminal features"](https://github.com/sstraus/tuicommander#readme)).
- **No first-class issue/task tracker** — no PRD format, no beads-style hierarchy, no DAG. Work units are *terminals* and *worktrees*, not tasks. Tasks are whatever the underlying agent is currently doing.

## Review / approval model

- **No formal review queue or approval gate.** TUICommander observes — it doesn't gate.
- "Question detection" surfaces Y/N prompts via tab indicators + notification sound + keyboard overlay, but the answer goes straight to the agent; no human-in-the-loop *diff approval* before writes hit disk.
- Diff inspection happens *after the fact* via the Git Panel (`Cmd+Shift+D`), side-by-side / unified / scroll-all-files diffs, blame with age heatmap, hunk/line-level restore ([README "See what your agents changed"](https://github.com/sstraus/tuicommander#readme)).
- **CI Auto-Heal** — on CI failure, fetches logs and *automatically injects them into the agent* with no human gate.
- **Built-in AI Chat & autonomous ReAct agent** ([README "Built-in AI Chat & autonomous agent"](https://github.com/sstraus/tuicommander#readme)) — 12 tools including "send input", "edit files", "run commands". Driven by the user, not a multi-stage review pipeline.

## Pricing

- Open source (Apache-2.0). No paid tier surfaced on README or site (no pricing page linked from [tuicommander.com](https://tuicommander.com)).
- Costs are downstream: the underlying agents bill the user directly.

## Adoption signals

- **GitHub:** 66 stars, 11 forks, 1 watcher, 2 open issues. Repo is 3 months old (created 2026-02-17).
- **Distribution sophistication is disproportionate to star count:** signed+notarized macOS builds, Homebrew tap (`sstraus/tap/tuicommander`), nightly channel, multi-platform installers. This is *production-grade packaging at near-zero community traction* — suggests heavy solo investment, narrow current audience.
- Listed in [github.com/topics/agent-orchestration?l=rust](https://github.com/topics/agent-orchestration?l=rust). External writeup at [techloghub.com](https://techloghub.com/open-source/codex-monitor-multi-agent-workspace-orchestrator).
- **No public download counts** surfaced (Homebrew analytics not yet querying meaningfully for a tap this young).

## Top 3 strengths

1. **Closest visual analogue to SPUR's "control tower" UX in a desktop form factor.** Tab status dots (idle/busy/done/unseen/question/error), real-time activity dashboard, 52-week heatmap, per-project usage breakdown — this is exactly the *Cmd-tower picture* the F2 research uncovered (`marketing/research/themes.md:42-48`). If a prospect Googles "control tower for Claude Code", TUICommander screenshots are the most evocative competing image.
2. **Native desktop polish.** Rust + Tauri v2 + SolidJS gives a real-app feel — bundled fonts, Kitty keyboard protocol, on-device Whisper voice dictation, mobile companion PWA over Tailscale or E2E relay. Hard to match in a TUI.
3. **Genuinely AI-native feature set, not afterthoughts.** Rate-limit detection with countdowns, question detection, session-aware resume, CI Auto-Heal, MCP Proxy Hub, inter-agent messaging, Agent Teams as native tabs. Each is a feature SPUR would otherwise need to build.

## Top 3 weaknesses vs SPUR

1. **PTY scraping is structurally fragile.** Rate-limit detection is "provider-specific patterns" — i.e., regex on agent output. Every Claude/Codex/Gemini output-format change is a maintenance event. SPUR's ACP-native plumbing (where available) sidesteps the regex tax. Quote the F2 finding: PTY scrapers are exactly what the next generation of orchestrators is trying to escape (per acpx's positioning).
2. **No human-review state machine.** Diffs are inspected *after* writes; "CI Auto-Heal" auto-injects logs into the agent without a gate. SPUR's review-card / approval-queue model maps onto the F2 "control tower" metaphor more literally — the operator decides what merges. TUICommander assumes you watch the agent; SPUR assumes you *queue and decide*.
3. **Desktop-only — no headless/server mode, no remote multi-tenant.** Mobile PWA is observe-only (questions can be answered, but the engine runs on your Mac). SPUR's daemon architecture lets a team or a CI environment host the orchestrator; TUICommander cannot. Also: single-user license model is invisible (no paid tier) — no obvious monetization for teams, which leaves the high-ARPU segment open.

## Notes for downstream Phase-3 work

- **The vs-TUICommander page is mostly about review-gating and headless deployment.** Don't argue over feature parity on the desktop-IDE surface (TUICommander wins on shiny). Argue that *observing* an agent ≠ *governing* an agent.
- TUICommander's "control tower"-shaped screenshots are excellent reference for what SPUR's own marketing screenshots should *visually* communicate.
- TODO for downstream: capture Homebrew download stats (`brew info --analytics`) and GitHub release download counts (`gh release list`) once they become non-trivial, to quote a real adoption delta.

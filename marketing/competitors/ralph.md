# Ralph TUI

*Profile date: 2026-05-20. All claims cite a primary source (repo, README, or official site). Schema follows the F3 task brief; the canonical SKILL.md was not on disk at profile time (`marketing/marketingskills/` is gitignored per `marketing/README.md:12-14`).*

> **Disambiguation note.** The F3 brief described Ralph as a "TypeScript read-only TUI". Two GitHub projects match the name. The canonical, actively-maintained one is **`subsy/ralph-tui`** (2,327 stars, 231 forks, last push 2026-05-13). A second repo `syntax-syndicate/ralph-ai-tui` exists with the same description but only 2 stars, 0 forks, last push 2026-01-17 — almost certainly a stale fork. This profile covers `subsy/ralph-tui`. **The "read-only" characterization in the brief appears inaccurate** — Ralph *actively executes* agents in an autonomous loop. Flagging this for the brief author to correct in future briefings; carrying forward with the on-disk evidence.

## Identity

- **Name:** Ralph TUI (`ralph-tui` on npm).
- **Official site:** https://ralph-tui.com (repo `homepage` field).
- **GitHub repo:** https://github.com/subsy/ralph-tui
- **Stars:** 2,327 (GitHub API, 2026-05-20).
- **Forks / watchers / open issues:** 231 forks / 13 watchers / 38 open issues.
- **License:** MIT.
- **Created:** 2026-01-11. **Last push:** 2026-05-13 (one week stale — actively maintained, not abandoned).
- **Runtime:** Bun + TypeScript ("Built with Bun" badge on [README](https://github.com/subsy/ralph-tui#readme)).

## Headline pitch

> "**AI Agent Loop Orchestrator** - A terminal UI for orchestrating AI coding agents to work through task lists autonomously. Ralph TUI connects your AI coding assistant … to your task tracker and runs them in an autonomous loop, completing tasks one-by-one with intelligent selection, error handling, and full visibility." — [README](https://github.com/subsy/ralph-tui#readme)

Named after the "Ralph Wiggum" technique — autonomous looping agent execution (referenced by sibling project [mikeyobrien/ralph-orchestrator](https://github.com/mikeyobrien/ralph-orchestrator), found in search results).

## Supported agents

Per [README "Features"](https://github.com/subsy/ralph-tui#features):

- Claude Code, OpenCode, Factory Droid, Cursor CLI, Gemini CLI, Codex, Kiro CLI.

Plugin system: `ralph-tui plugins agents` and `ralph-tui plugins trackers` list available plugins.

## Architecture

- **PTY/CLI subprocess execution** — wraps each agent's CLI binary. Agent adapters "normalize the interfaces of different AI coding tools" (per third-party writeup at [verdent.ai](https://www.verdent.ai/guides/ralph-tui-ai-agent-dashboard) and ralph-tui.com docs page on [Claude Code agent](https://ralph-tui.com/docs/plugins/agents/claude)). The Claude Code plugin specifically supports "subagent tracing — Ralph TUI can show nested tool calls (Read, Write, Task) in real-time" by parsing Claude Code's emitted events.
- **Task-tracker pluggable:** built-in trackers are `prd.json` (simple) and **Beads** (git-backed with dependencies) ([README "Features"](https://github.com/subsy/ralph-tui#features)). Custom trackers via the plugin system. *Note: this is the same Beads format SPUR uses — direct overlap with SPUR's brain/worker plan format.*
- **Loop engine** — 5-step state machine: SELECT TASK → BUILD PROMPT → EXECUTE AGENT → DETECT COMPLETION → NEXT TASK ([README "How It Works"](https://github.com/subsy/ralph-tui#how-it-works)).
- **Sandboxing:** `--sandbox` flag uses `bwrap` on Linux, `sandbox-exec` on macOS.
- **Remote instance management** — control multiple ralph-tui instances on different machines (VPS, CI, dev boxes) from a single TUI; tab UI with `1-9` to switch ([README "Remote Instance Management"](https://github.com/subsy/ralph-tui#remote-instance-management)).
- **Headless mode** — `ralph-tui run --headless` for CI use.

## Plan persistence model

- **Beads integration is first-class.** `--epic`, `--parallel --epic ui-epic --epic backend-epic`, `--epics ui-epic,backend-epic`. "Ralph uses one scheduler, one repo lock, one session branch, one merge queue, and task-scoped worktrees" ([README multi-epic note](https://github.com/subsy/ralph-tui#cli-commands)).
- **prd.json** for simple list-of-tasks model.
- **Session persistence:** `ralph-tui resume` resumes interrupted sessions; pause anytime, survive crashes ([README "Features"](https://github.com/subsy/ralph-tui#features)).
- **Cross-iteration context:** "Automatic progress tracking between tasks."

## Review / approval model

- **Autonomous-by-default**, not review-gated. The loop runs until done or interrupted; Ralph's UX is *watching* + *pausing*, not *queueing diffs for human approval*.
- **TUI controls:** `s` start, `p` pause/resume, `T` toggle subagent tree, `o` cycle right-panel views, `a` agent/model picker, `,` settings, `C` read-only config viewer, `q` quit ([README "TUI Keyboard Shortcuts"](https://github.com/subsy/ralph-tui#tui-keyboard-shortcuts)).
- **No explicit per-change diff approval gate** documented. Sandbox flag mitigates blast radius but doesn't insert a human into the write path.
- **`ralph-tui skills install`** can drop slash commands (`/ralph-tui-prd`, `/ralph-tui-create-json`, `/ralph-tui-create-beads`) into the underlying agent — these are *PRD authoring* skills, not review gates.

## Pricing

- Open source (MIT). npm package `ralph-tui`. No paid tier surfaced on [ralph-tui.com](https://ralph-tui.com) or README.
- Underlying agent costs flow through to the user via Anthropic/OpenAI/etc.

## Adoption signals

- **GitHub:** 2,327 stars, 231 forks (≈9.9% fork-to-star — high engagement), 13 watchers, 38 open issues. Very active issue volume for a 4-month-old project.
- **npm:** package `ralph-tui` v0.6.0 indexed on libraries.io (from search results) — version 0.6.x suggests rapid iteration. Exact download counts not captured.
- **Ecosystem coverage:** Listed on [Terminal Trove](https://terminaltrove.com/ai-coding-agents/ralph-tui/), [LinuxLinks](https://www.linuxlinks.com/ralph-tui-ai-agent-loop-orchestrator/), [Peerlist article](https://peerlist.io/leonardo_zanobi/articles/ralph-tui-ai-agent-orchestration-that-actually-works), [Verdent.ai guide](https://www.verdent.ai/guides/ralph-tui-ai-agent-dashboard) — multi-channel awareness.
- **Docs maturity:** Dedicated `ralph-tui.com` docs site with per-agent pages (e.g., [Claude Code plugin](https://ralph-tui.com/docs/plugins/agents/claude)), getting-started, CLI reference, configuration, troubleshooting.

## Top 3 strengths

1. **Beads integration is shipping today.** Ralph already executes against Beads epics with multi-epic parallel sessions, one scheduler, one merge queue, task-scoped worktrees. This is *exactly* SPUR's spec — but in production with 2.3k stars and a docs site. The most uncomfortable overlap of the four competitors.
2. **Autonomous-loop UX is solved & shipped.** Pause/resume, subagent tracing, headless mode, sandboxing, remote multi-instance control. These are non-trivial pieces of the SPUR worker experience that Ralph has already polished.
3. **Documentation + ecosystem velocity.** Dedicated docs site, per-agent plugin pages, Terminal Trove listing, multiple external writeups in 4 months. SEO surface around "Beads + agent loop" is already partially captured.

## Top 3 weaknesses vs SPUR

1. **No human-review state machine — autonomous-by-default.** Ralph's "full visibility" is observability, not governance. The F2 research (`marketing/research/themes.md:38-48`) shows users *don't* want pure autonomy — they want a *control tower* where things queue and they decide. Ralph optimizes for "agent runs until done"; SPUR optimizes for "agent proposes, human approves" with cost visibility. That's a positioning wedge, not a feature gap.
2. **No unified cost/spend ledger.** Subagent tracing is shown but spend is not aggregated across agents. The F2 cost-opacity finding ("the strongest emotional language in the batch is about *not knowing what you're spending*", `marketing/research/themes.md:84-86`) is the clearest hole. Ralph hasn't built a status-bar live spend or cross-agent cost rollup that the README discloses.
3. **PTY/subprocess substrate is the same fragile tax as TUICommander.** Each agent plugin parses that agent's output; "subagent tracing" is Claude-Code-specific parsing. ACP-native plumbing (the acpx direction) is a long-term threat. SPUR's option to ride ACP where available *and* fall back to PTY scraping otherwise is a structural advantage if framed correctly.

## Notes for downstream Phase-3 work

- **Ralph is the most feature-overlapping competitor we have.** The vs-Ralph page should *not* fight on Beads support (Ralph has it), parallelism (Ralph has it), or sandboxing (Ralph has it). Fight on *review-gating + cost ledger + multi-brain routing*.
- Lift Ralph's docs-site IA as a quality bar — `ralph-tui.com/docs/plugins/agents/claude` is a clean per-agent landing page format SPUR should match in `marketing/site/docs/`.
- TODO for downstream: live npm-download numbers for `ralph-tui` to quote relative scale ("Xk weekly downloads" vs SPUR's at launch).
- TODO: confirm whether `subsy` is a solo maintainer or backed by a company — affects framing of "individual side-project" vs "venture-backed competitor" in vs copy.

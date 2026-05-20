# Claude Code (standalone, no orchestration)

*Profile date: 2026-05-20. INDIRECT competitor — Anthropic's first-party agentic CLI used by itself (no SPUR / acpx / tmux fleet). This profile covers vanilla Claude Code only; multi-CC orchestration is the SPUR JTBD.*

## Identity

- **Name:** Claude Code.
- **Vendor:** Anthropic.
- **Official site:** https://claude.com/product/claude-code (redirected from https://www.anthropic.com/claude-code).
- **Docs:** https://code.claude.com/docs
- **Distribution:** CLI binary + VS Code / JetBrains extensions + web + Slack + iOS handoff.

## Headline pitch

> "Build, debug, and ship from your terminal, IDE, Slack, or the web. Describe what you need, and Claude handles the rest." — [claude.com/product/claude-code](https://claude.com/product/claude-code)

## Agent model

- **Single primary agent** with a **Task / subagent tool** for parallel fan-out: "delegates work to parallel sub-agents — file reads, code searches, web fetches — running up to ~7 agents simultaneously" ([Claude Code docs — Create custom subagents](https://code.claude.com/docs/en/sub-agents); third-party guides like [ClaudeLog "Task/Agent Tools"](https://claudelog.com/mechanics/task-agent-tools/) document the 7-parallel cap).
- **Subagents are sub-tasks of one session**, not peers in a managed fleet. They return summaries to the orchestrating agent and disappear — no persistent worker identity, no cross-session review queue.

## Architecture

- **Local CLI** running in the user's terminal. No backend server, no remote code index ([claude.com/product/claude-code](https://claude.com/product/claude-code)).
- Native integrations: VS Code, JetBrains, Slack, web, iOS hand-off, GitHub/GitLab.
- File-level permission prompts before edits/commands.

## Pricing

From [claude.com/product/claude-code](https://claude.com/product/claude-code) (2026-05-20):

| Tier | Price | Notes |
|------|-------|-------|
| Pro | $17-20 / mo | Includes Claude Code |
| Max | $100-$200 / mo | Higher Claude Code usage allocation |
| API (Console) | Pay-per-token | Standard Anthropic API pricing |

Pricing window is the *exact* source of the F2 #1 pain theme ("paying $200/mo, hit weekly limit in 3 days" — `marketing/research/themes.md:13`).

## Target persona

Anthropic's broad pitch is "developers and engineering teams of all sizes." In practice the high-engagement persona is **the solo IC who has wired Claude Code into terminal + IDE + Slack** — and is *the* canonical SPUR wedge persona (F2 themes #3-#5, `themes.md:37-78`).

## Adoption signals

- Named customers on the product page: **Ramp, Notion, Intercom** ([claude.com/product/claude-code](https://claude.com/product/claude-code)) — reported "significant productivity gains" and "1-2 days saved" per model release.
- No public DAU/MAU disclosed by Anthropic.
- Strong third-party evidence of high engagement: r/ClaudeAI, HN threads (the entire VOC corpus in `marketing/research/voc.md`), `ccusage`, ccmanager, and tooling ecosystem (claudelog.com, claudefa.st, aibuilderclub.com guides). Claude Code is the *agent everyone is wrapping*, including SPUR.

## Top 3 strengths

1. **Best single-agent coding experience available, by general consensus.** Claude Code + Sonnet/Opus tops developer satisfaction polls; Anthropic's investment in the CLI surface is unmatched among model vendors.
2. **First-party agent + first-party model.** Anthropic ships Claude Code, owns the model, owns the API. Latency, model selection, and feature flow are tighter than any third-party wrapper. Subagents and skills are first-class.
3. **Distribution.** Bundled with Pro/Max plans of the dominant LLM. Most Claude subscribers already have it installed; SPUR has to compete with "I'm already here."

## Top 3 reasons a SPUR user would still want SPUR

1. **Claude Code is one fleet member; SPUR runs many.** The 7-parallel subagent cap ([ClaudeLog](https://claudelog.com/mechanics/task-agent-tools/)) is in-session and ephemeral. The F2 wedge user runs **5-10 separate Claude Code sessions across repos** (`themes.md:44-46`) — a multiplex Claude Code does not natively orchestrate. SPUR is the layer above.
2. **Single-vendor by definition.** Claude Code only runs Claude. The moment Pro/Max rate-limits trip (`themes.md:7-19`) the user has no failover. SPUR's brain-swap to Codex / Gemini / GLM directly addresses this without abandoning Claude Code as a worker.
3. **No cross-session control tower or cost ledger.** Anthropic surfaces per-session usage but not "all my agents, what I've spent today, who's stuck." This is theme #2 + #3 of F2 research (`themes.md:23-49`). SPUR is built for exactly that gap.

## Notes for downstream positioning

- **The most important framing.** SPUR is **not** "Claude Code alternative." SPUR is "the orchestrator that makes Claude Code (and Codex, Gemini, GLM…) controllable as a fleet." The vast majority of SPUR users will keep paying Anthropic.
- Don't trash Claude Code in copy. The wedge user *loves* Claude Code — they're frustrated by what it doesn't do *between* sessions, not what it does *in* a session.
- Watch Anthropic's roadmap for: (a) cross-session orchestration, (b) cost dashboards, (c) Task tool durability beyond a session. Any of these narrows the SPUR moat. The current Task tool design (in-session, ephemeral, single-vendor) is the moat-shape we should assume holds for at least 2 more releases.

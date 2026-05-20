# ACPX

*Profile date: 2026-05-20. All claims cite a primary source (repo, README, or official site). Schema follows the field list in the F3 task brief — the `marketing-competitor-profiling` SKILL.md was not on disk at profiling time (marketingskills/ is gitignored per `marketing/README.md:12-14`), so this is the agreed-upon best-effort template.*

## Identity

- **Name:** acpx (`acpx` on npm).
- **Official site:** https://acpx.sh (linked as `homepage` in repo metadata).
- **GitHub repo:** https://github.com/openclaw/acpx
- **Stars:** 2,700 (GitHub API, 2026-05-20).
- **Forks / watchers:** 265 forks / 18 watchers (GitHub API, 2026-05-20).
- **License:** MIT (GitHub API).
- **Created:** 2026-02-17. **Last push:** 2026-05-19 (≤1 day stale — actively maintained).
- **Status (self-declared):** alpha — "CLI/runtime interfaces are likely to change" ([README badge](https://github.com/openclaw/acpx#readme)).

## Headline pitch

> "`acpx` is a headless CLI client for the Agent Client Protocol (ACP), so AI agents and orchestrators can talk to coding agents over a structured protocol instead of PTY scraping." — [README](https://github.com/openclaw/acpx#readme)

Positions itself as *curl for agent sessions*: no UI, no editor, just structured protocol traffic.

## Supported agents

Built-in registry (with escape hatch `--agent <cmd>` for arbitrary ACP servers) — [README "Agent prerequisites"](https://github.com/openclaw/acpx#agent-prerequisites):

- Pi Coding Agent (`acpx pi`)
- OpenClaw ACP bridge (`acpx openclaw`)
- Codex CLI (`acpx codex`)
- Claude Code (`acpx claude`)

Additional adapters auto-download via `npx` on first use; see [agents/README.md](https://github.com/openclaw/acpx/blob/main/agents/README.md).

## Architecture

- **Native ACP** — speaks JSON-RPC over the [Agent Client Protocol](https://agentclientprotocol.com). Explicitly built to *replace* PTY scraping. ([README intro](https://github.com/openclaw/acpx#readme))
- **Stateful sessions per repo**, scoped by cwd; named sessions (`-s backend`, `-s frontend`) for parallel workstreams in the same repo. ([README features](https://github.com/openclaw/acpx#readme))
- **IPC queue model** — separate queue-owner process holds the long-lived ACP connection; clients send prompts via queue IPC. Supports cooperative cancel (`session/cancel`), TTL keep-alive, fire-and-forget (`--no-wait`).
- **Auto-reconnect** — "dead agent processes are detected and sessions are reloaded automatically."
- **Flows runtime** (experimental) — `acpx flow run <file>` executes TypeScript workflow modules with `acp`/`action`/`decision`/`checkpoint`/`compute` step types; persists run state under `~/.acpx/flows/runs/`. ([README "Flows"](https://github.com/openclaw/acpx#flows))

Runtime: Node.js ≥22.12.0, pnpm ≥10.23.0. TypeScript codebase.

## Plan persistence model

- Session state on disk at `~/.acpx/`. Survives invocations and crashes; `sessions show` / `sessions history --limit <n>` for inspection. ([README install & usage examples](https://github.com/openclaw/acpx#install))
- Flows persist run bundles at `~/.acpx/flows/runs/` (per-step status, agent turns, action outputs). A separate "replay viewer" example exists for browser inspection of saved runs ([examples/flows/replay-viewer](https://github.com/openclaw/acpx/blob/main/examples/flows/replay-viewer/README.md), referenced in README).
- No explicit DAG/dependency graph between *issues* — flows are linear TypeScript modules, not a beads-style hierarchical task tracker.

## Review / approval model

Permission policy is per-process, not a multi-step review gate ([README "Global options in practice"](https://github.com/openclaw/acpx#global-options-in-practice)):

- `--approve-all`, `--approve-reads` (default), `--deny-all`
- `--policy '{"escalate":["execute"],"defaultAction":"deny"}'` for fine-grained JSON policy
- `--non-interactive-permissions fail` for non-TTY behavior
- Permission controls + cwd sandboxing on the `fs/*` and `terminal/*` client methods

There is **no built-in human-review queue / approval card UI** — acpx is the protocol layer, not the orchestrator. Reviewing diffs is left to the caller (orchestrator, IDE, or downstream tool).

## Pricing

- Open source (MIT). No paid tier, hosted service, or commercial offering surfaced on the [acpx.sh](https://acpx.sh) site or the GitHub README.
- Costs are downstream: the *underlying* agents (Claude, Codex, etc.) bill the user directly via their own APIs/subscriptions.

## Adoption signals

- **GitHub:** 2.7k stars, 265 forks (high fork-to-star ratio ≈ 9.8% → strong developer engagement). 18 watchers; 9 open issues.
- **npm:** package `acpx` with [npm version + monthly downloads badges](https://www.npmjs.com/package/acpx) on the README (exact download count not captured here — needs a follow-up npm-stat fetch if precise number is required for downstream copy).
- **Ecosystem:** spawned at least one adapter project — `aLittlecrocodile/cursor-acp` markets itself as "ACP adapter for Cursor CLI — lets acpx and other ACP orchestrators control Cursor as a coding agent" (cited in third-party search results) — evidence that "acpx" is being used as a verb in the ecosystem.
- **Third-party writeups:** [casys.ai blog "ACPX Inside Claude Code: Practical Multi-Agent Orchestration"](https://casys.ai/blog/acpx-multi-agent-orchestration).

## Top 3 strengths

1. **Cleanest architectural answer to PTY-scraping pain.** Of the four direct competitors, acpx is the only one that has bet entirely on the Agent Client Protocol. If ACP becomes the de-facto standard (it has Anthropic + OpenClaw backing), every PTY-scraping orchestrator inherits a long-term obsolescence problem.
2. **Composable / Unix-y.** Pipe-friendly: stdin/file prompts, `--format json`, `--no-wait`, structured typed messages instead of ANSI. This is exactly what other orchestrators want as a substrate — making acpx more likely to be *embedded* than competed-with.
3. **Already broad agent coverage via the protocol.** Pi, OpenClaw, Codex, Claude built-in; any ACP server via `--agent`. Adapters auto-download. Lower per-agent integration cost than every PTY-based competitor.

## Top 3 weaknesses vs SPUR

1. **No control-tower UI.** acpx is the protocol plumbing; there is no TUI, no review queue, no status grid, no cost ledger. The F2 customer research finding — "I needed a control tower" (Beefin, `marketing/research/themes.md:46`) — is the surface acpx explicitly *doesn't* provide. SPUR can ship a TUI *on top of* acpx-style ACP rather than against it.
2. **No native cost / rate-limit ledger.** Permission policies, cwd sandboxing, and queue lifecycle are first-class, but there is no aggregated spend view across agents — and cost opacity is the *sharpest* VOC pain (`marketing/research/themes.md:23-34`).
3. **Alpha-stage, single-vendor protocol bet.** Self-declares "anything you build downstream of this might break until it stabilizes." Agents that don't ship ACP servers (most TUIs, IDE-bundled agents) are second-class — acpx has no fallback PTY mode. SPUR's strategic option to be agent-native *and* PTY-tolerant remains an advantage while ACP coverage is incomplete.

## Notes for downstream Phase-3 work

- **Don't position SPUR as anti-acpx.** Position acpx as *infrastructure SPUR can ride*; the brain/worker/review layer is what acpx explicitly disclaims.
- The acpx README skill-install snippet (`npx acpx@latest --skill install acpx --agent codex --scope user`) is a precedent for how an ACP-native tool teaches *other* agents to use it. SPUR has the same opportunity.
- TODO for downstream: pull live npm download counts for `acpx` to quote a precise adoption number ("Xk downloads/month") in the vs-page copy. Source: https://www.npmjs.com/package/acpx.

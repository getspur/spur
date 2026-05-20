# Aider

*Profile date: 2026-05-20. INDIRECT competitor — single-agent local CLI that power-users stitch into multi-agent setups manually. Same schema notes as `devin.md`.*

## Identity

- **Name:** Aider.
- **Official site:** https://aider.chat
- **GitHub:** https://github.com/Aider-AI/aider
- **License:** Apache-2.0.
- **Stars:** ~45,000 (GitHub repo page, 2026-05-20).
- **Forks:** 4.4k. **Watchers:** 249. **Open issues:** 1.2k.
- **Latest release:** v0.86.0 (2025-08-09 — ~9 months stale at profile date; release cadence has slowed).

## Headline pitch

> "AI pair programming in your terminal." — [aider.chat](https://aider.chat)

Positioning is unambiguously **pair-programmer**, not autonomous-engineer: human-in-the-loop, terminal-native, git-integrated.

## Agent model

- **Single CLI session per repo.** One Aider process, one conversation, one set of files in context. Users run *N* Aider sessions in *N* terminals/worktrees to parallelize — but that orchestration is entirely manual.
- BYO LLM key: Claude, OpenAI, DeepSeek, Gemini, local models via litellm. No proprietary model.

## Architecture

- **Local CLI**, Python. No backend. No telemetry server. No cloud sandbox.
- Reads repo, builds a tree-sitter codebase map, sends relevant context + diff prompts to the configured LLM.
- Auto-commits to git after each accepted change.
- IDE integration via watch mode (file-watcher rather than plugin).

## Pricing

- **Free / open source (Apache-2.0).**
- User pays the LLM API directly. No Aider-side subscription.

## Target persona

Terminal-native developers who want **a single AI collaborator with auditable git history** and zero vendor lock-in. The opposite of the Devin "delegate-and-walk-away" buyer.

## Adoption signals

- **45k GitHub stars**, 4.4k forks ([github.com/Aider-AI/aider](https://github.com/Aider-AI/aider)).
- **6.8M PyPI installs**, **15B tokens/week processed**, **88% of recent codebase changes written by Aider itself** ([aider.chat](https://aider.chat)).
- Long-running visible community; widely cited as the reference open-source CLI before Claude Code shipped.
- Caveat: last release Aug 2025 — momentum *may* be cooling against Claude Code, but installed base is enormous.

## Top 3 strengths

1. **Lowest friction + truly free.** `pip install aider-chat`, paste an API key, go. No login, no sandbox, no quota. The 6.8M-install number is the largest in this competitor set.
2. **Best git-discipline of any CLI.** Per-change auto-commits + repo map + diff-mode prompts give the cleanest "what did the AI do today" git log in the category. This is hard to replicate.
3. **Model-agnostic from day one.** BYO key across all major providers. Aider users *already* embody the polyamorous-about-models behavior SPUR's positioning bets on (`themes.md:67-78`).

## Top 3 reasons a SPUR user would still want SPUR

1. **Aider is one agent at a time; SPUR is a fleet.** The F2 wedge persona (`themes.md:43-49`) is running 5-10 agents *in parallel*. Aider users hit that wall and reach for tmux + worktrees + scripts to coordinate — exactly the DIY tax (`themes.md:52-65`) SPUR removes.
2. **No review queue, no status grid, no cost ledger.** Aider intentionally is a single conversation. Users have to context-switch between terminals to know which session is waiting / running / done — Beefin's "control tower" gap, verbatim (`themes.md:46`).
3. **No automatic DAG-ordered merge.** Multi-Aider-worktrees still leave manual cherry-pick and merge-conflict resolution on the developer. SPUR's review-gated merge + DAG ordering is the load-bearing differentiator vs the Aider+worktree DIY stack.

## Notes for downstream positioning

- **Aider users are SPUR's warmest leads.** They've already chosen: terminal, BYO key, git-discipline, polyamorous about models. The pitch is *"keep using Aider — SPUR is the layer above when one Aider session becomes five."*
- A SPUR↔Aider adapter (Aider as a SPUR worker) would be a great F4-F8 distribution play. Aider speaks no protocol like ACP — would need PTY or its `--message`/`--yes-always` flags. Worth scoping.
- Aider release slowdown (no release since Aug 2025) is real; do not bet on it being the leading CLI in 12 months. Claude Code and Codex CLI are eating its share.

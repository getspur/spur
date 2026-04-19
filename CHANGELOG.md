## Unreleased

## v0.4.5 — 2026-04-19

Spur 0.4.5 focuses on getting around faster and trusting what you see. A new universal palette (Ctrl+K) jumps you between sessions, workers, traces, and commands from anywhere. The `@mention` and `/slash` pickers now share one consistent interface. Agent output renders markdown and mermaid diagrams inline, and streaming stays smooth under bursty traffic.

### Added
- **Universal palette (Ctrl+K).** Fuzzy-search and jump to any session, worker, trace entry, or command from anywhere in the TUI. A `[Ctrl+K: go]` badge in the status bar reminds you it's there.
- **Unified completion for `@mention` and `/slash`.** Both now flow through the same picker — same keys, same preview, same ranking. One interface to learn.
- **Rerun recent prompts with Ctrl+R.** Session picker surfaces your recent prompts; pick one to rerun against the current session.
- **Auto-resume landing.** Launching spur drops you straight back on the last session you were working in.
- **Markdown and mermaid in agent output.** Rich content renders inline in the trace view — no more raw source for formatted replies or diagrams.
- **Dashboard view.** New top-level view for at-a-glance status across sessions and workers.
- **Skills installer across adapters.** `.spur/skills/` installs to Cursor, Codex, Kiro, Gemini, and OpenCode in one step, with edits you own preserved on upgrade.
- **Delegation visibility in the timeline.** Delegation plans now appear in the TUI timeline as they happen; `list_available_workers` returns richer descriptors (tier, cost, suitability hints).

### Improved
- **Smoother streaming.** A new scroll anchor and per-frame drain cap keep long, bursty outputs readable instead of jittery.
- **Faster palette and pickers.** Single-pass reranking and cached search patterns.
- **Session metadata persists across restarts.**

### Preview features (opt-in)
- **Brain delegation framework.** A new orchestration model for deciding which worker handles a task. Opt in via your config:
  ```toml
  [brain.delegation]
  framework = "v1"
  ```
  Release builds default to `legacy` for 0.4.5; the v1 framework will become default once it stabilizes.

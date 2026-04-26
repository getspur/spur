## Unreleased

### Changed
- **Peer mailbox reconciler now emits `WorkerPeerMessageAuditFailed`** on
  non-terminal transition errors during startup reconciliation. The
  `WorkerPeerMailboxReconciled.audit_failed_emitted` counter, which was
  always `0` prior to this release, may now report non-zero values when
  the persistent ledger introduces transition failures. Dashboards and
  alerts that filter `audit_failed_emitted == 0` as a "no anomalies"
  signal should switch to alerting on the
  `WorkerPeerMessageAuditFailed { transition_kind: "reconcile_to_delivered" }`
  event directly. (bd-cpf.5b)

### Added
- **`WorkerPeerMailboxReconciled.inflight_already_delivered` counter.**
  Tracks benign idempotent races during startup reconciliation where an
  entry was already in `Delivered` state when the reconciler attempted
  to advance it. Always 0 in Stage-1; becomes non-zero under Stage-2
  crash-loop or concurrent-reconcile scenarios. (bd-cpf.5c)
- **Spur Way skill bundle.** Six bundled skills harden brain-worker-beads
  collaboration: `spur-way` (beads-first invariant), `beads-lifecycle`
  (status state machine), `worker-signals` (`[[spur-signal v1]]` protocol),
  `brain-review-gate` (beads-aware approval checklist), `plan-task-discipline`
  (DAG integrity rules), and `worker-mention-routing` (user `@`-mention
  overrides algorithmic selection). All compile into `spur-core` via
  `include_str!` and render across the seven agent adapters. See
  `docs/superpowers/specs/2026-04-22-superpower-skill-hardening-spur-way.md`.
- **Role-gated skill rendering + Kimi adapter.** Skills now carry a `role`
  field (`brain | worker | both`). Brain-only skills (`brain-review-gate`,
  `brain-delegation`) no longer leak into worker agent directories. Worker
  skills (`test-driven-development`, `systematic-debugging`) are tagged
  explicitly. New `Adapter::Kimi` renders to `.kimi/skills/`, closing the
  gap where Kimi workers accidentally relied on `.claude/skills/` fallback.
  See `docs/superpowers/specs/2026-04-22-multi-agent-skill-embedding-research.md`.

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

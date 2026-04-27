# SPUR Individual Tier Revamp — Design Spec

**Status:** Draft v1.0
**Date:** 2026-04-26
**Owner:** Kevin Truong (PO) + brain (synthesis) + gemini/kimi/codex (review)
**Supersedes:** `docs/architecture-tier-plan.md` (sections 2 + 3 individual portion)
**Related:** `docs/SPUR_PRD.md`, `docs/architecture.md`, `docs/superpowers/specs/2026-04-21-feature-gate-unified-resolver-design.md`, `docs/superpowers/specs/2026-04-18-spur-licensing-architecture.md`

---

## 1. Executive Summary

This spec replaces the existing tier-plan's individual-tier section (Community + Pro) with a **capability-gated, atomic feature-key registry** grounded in the actual 13-crate codebase per `docs/architecture.md`.

**Core decisions:**

1. **All 7 ACP vendor adapters Free.** Cross-vendor orchestration is SPUR's competitive moat; gating vendors at Pro is product self-sabotage.
2. **All 17 bundled skills × 7 render targets Free.** Crippling skills cripples agent quality. Only `skills_pro_custom` (org-internal extensions) is Pro.
3. **Capability gates, not quota gates.** People pay to UNLOCK CAPABILITIES they can't otherwise do. Quotas exist but stay symbolic for the persona.
4. **Honest launch with dated roadmap.** Pro v1 ships ~40 real features today. 4 vapor features are explicitly listed as "v1.1 Q3 2026" with date buffer. Existing Pro buyers get v1.1 unlocks free.
5. **Pricing: $12/mo or $99 lifetime** (lifetime explicitly bound to v1.x).
6. **Team tier deferred** to v2; reserved feature keys documented for forward compatibility.

**Tier philosophy:**
- **Free** is a *daily-driver tier*, not crippleware. The PRD's headline pain (rate-limit fragility) is solved end-to-end (with one-keystroke friction).
- **Pro** is *AFK confidence*: closed-loop automation, mobile remote-control, autonomous review, multi-session epics. The user pays to remove THEMSELVES from the loop.

---

## 2. User-Facing Positioning

### Free — *"Solve rate-limit fragility — free, forever."*
Single-line value prop: *Orchestrate any AI coding agent without losing work to rate limits. Open source core, signed-policy free tier, no time limit.*

### Pro — *"Ship from your couch — closed-loop today, autonomous tomorrow."*

Three pricing options:
- **$12/mo** monthly subscription
- **$99/yr** annual subscription (save $45/yr vs monthly)
- **$99 lifetime** (one-time, bound to v1.x; includes all v1.x roadmap unlocks)

Single-line value prop: *Auto-PR your Linear/GitHub issues, approve from Telegram, run plans across sessions. Lifetime and annual are priced identically — annual is for users who prefer recurring billing for accounting; lifetime is for users who want one-and-done.*

**7-day Pro trial** available via opt-in: `spur upgrade trial`. Anti-abuse via licenseseat NodeLocked binding (one trial per machine fingerprint).

### Team — (v2, deferred)
Reserved value prop: *Govern your team's AI agents — shared lineage, RBAC, audit, multi-operator bot.*

---

## 3. Naming Convention (locked)

```
<crate>_<tier>_<capability>
```

Where:
- `<crate>` ∈ {`acp`, `core`, `mcp`, `tui`, `cli`, `pm`, `cost`, `worktree`, `license`, `bot`, `interactive`, `blob`, `ctx`, `skills`}
- `<tier>` ∈ {`core` (Free baseline), `pro` (Pro upsell), `team` (Team v2-deferred)}
- `<capability>` is a single atomic capability, lowercase snake_case

Each feature key is grep-discoverable in code (`rg "pm_pro_*"` finds every Pro PM gate). Each gate enforcement point in Rust calls `FeatureGate::require(FeatureKey::pm_pro_github_auto)?` — see §6.

---

## 4. Full Feature Key Registry (~110 keys)

Every key carries two markers:
- **Tier:** `[F]` Free, `[P]` Pro, `[T]` Team-deferred
- **Status:** `[v1]` ships in v1 launch, `[v1.1-Q3]` shipped in v1.1 (target Q3 2026), `[v1.x]` reliability fix dependency, `[v2]` Team-deferred

Risk-blocked features cite the relevant Risk # from `architecture.md` §8.

### 4.1 `spur-acp` (Protocol foundation — 11 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `acp_core_transport_stdio` | F | v1 | JSON-RPC over stdio (default ACP transport) |
| `acp_core_transport_socket` | F | v1 | Socket-based transport for sandboxed agents |
| `acp_core_adapter_claude_code` | F | v1 | Claude Code vendor adapter |
| `acp_core_adapter_codex` | F | v1 | Codex vendor adapter |
| `acp_core_adapter_gemini` | F | v1 | Gemini vendor adapter |
| `acp_core_adapter_kiro` | F | v1 | Kiro vendor adapter |
| `acp_core_adapter_cursor` | F | v1 | Cursor vendor adapter |
| `acp_core_adapter_opencode` | F | v1 | OpenCode vendor adapter |
| `acp_core_adapter_kimi` | F | v1 | Kimi vendor adapter |
| `acp_core_session_attach_advisory_lock` | F | v1 | `fs4` single-attach lock per session (`SessionAttachGuard`) |
| `acp_core_session_attach_degraded_nolock` | F | v1 | `fs_unsafe=true` NFS/sshfs fallback path with persistent banner |

### 4.2 `spur-core` (Orchestration engine — 31 keys)

#### Brain & scheduling
| Key | Tier | Status | Description |
|---|---|---|---|
| `core_core_brain_session` | F | v1 | Single brain ACP session (prompt/notification loop) |
| `core_core_brain_scheduler` | F | v1 | Ordered turn delivery to brain (`BrainScheduler`) |
| `core_core_brain_failover_manual_keystroke` | F | v1 | Banner prompt on rate-limit detection: "Switch to Kiro? [y/n]" |
| `core_pro_brain_failover_auto_pool` | P | **v1.1-Q3** | Silent multi-brain pool with health monitoring (Risk #8 fix) |
| `core_core_continuation_bridge` | F | v1 | Detached completion → scheduler projection (`ContinuationBridge`) |

#### Workers & semaphore
| Key | Tier | Status | Description |
|---|---|---|---|
| `core_core_parallel_workers` | F | v1 | Parallel delegation slots (Free cap: 2; Pro cap: 10) |
| `core_core_cancellable_semaphore` | F | v1 | Biased-select cancellable acquire (Risk #23 closure) |
| `core_pro_worker_heartbeat_watchdog` | P | v1 | Opt-in heartbeat-driven worker timeout (default off, Pro-flippable) |

#### Event pipeline
| Key | Tier | Status | Description |
|---|---|---|---|
| `core_core_event_funnel_broadcast` | F | v1 | Monotonic seq stamping + broadcast(4096) channel |
| `core_core_event_sink_ndjson_128mb` | F | v1 | NDJSON file persistence with 128MB rotation (per arch.md, NOT 256MB) |
| `core_core_executor_lineage_projection` | F | v1 | Pure projection from event stream (`ExecutorLineage`) |
| `core_core_notification_pump` | F | v1 | `NotificationPump` event distribution to TUI/bot |
| `core_pro_broadcast_lagged_recovery` | P | **v1.1-Q3** | `Lagged(n)` → NDJSON replay reconstruction (Risk #2/#9 fix) |

#### Review subsystem
**Revised 2026-04-26 (2nd pass) via 3-reviewer triangulation (gemini → kimi → codex).** First revision used `_basic`/`_custom`/`_backoff` suffixes (impl leaks) and folded timeout/retry into `review_sink`. Codex's code reading (`review_sink.rs:34`, `orchestrator.rs:4517`/`4654`, `spur-acp/src/config/mod.rs:246`) showed the sink is router-only; timeout + retry are separable orchestrator branches with distinct config fields. Adopted kimi's 6-key naming (capability nouns, no impl leaks).

| Key | Tier | Status | Description |
|---|---|---|---|
| `core_core_review_sink` | F | v1 | Pipeline routing review cards to frontends; includes built-in manual Approve/Reject/Modify resolution (formerly separate `review_policy_manual` — folded in) |
| `core_core_review_timeout` | F | v1 | Auto-cancel on review timeout (liveness baseline; prevents indefinite hangs) |
| `core_core_review_retry` | F | v1 | Press 'R' to retry a failed review with system-default backoff (UX baseline; non-deterministic agent behavior) |
| `core_pro_review_auto_approve` | P | v1 | Rule-based auto-approve (path globs, change-size limits) — gates the review-bypass branch |
| `core_pro_review_timeout_routing` | P | v1 | Configurable `FallbackAction` routing on timeout (Slack hand-off, alt agent, etc.) — beyond default auto-cancel |
| `core_pro_review_retry_config` | P | v1 | `RetryRequested` resolution with configurable exponential backoff + max-attempts policy (Risk #5 closure) — beyond fixed default backoff |

#### Skills system
**Revised 2026-04-26 (gate-review pass).** Original draft mixed `core_core_skill_*` (2 keys) with `skills_*` (3 keys), violating the "block label matches all contained keys' prefix" invariant and breaking grep-discoverability of the skills boundary. Renamed all 5 keys to share the `skills_*` prefix.

| Key | Tier | Status | Description |
|---|---|---|---|
| `skills_core_registry` | F | v1 | Context-aware loading of bundled skills |
| `skills_core_atomic_installation` | F | v1 | SPUR-MANAGED marker + SHA-256 integrity (Risk #16 mitigation) |
| `skills_core_render_per_vendor` | F | v1 | All 17 bundled skills × 7 render targets (claude/codex/gemini/kiro/cursor/opencode/kimi) |
| `skills_pro_custom` | P | v1 | Register org-internal MCP/agent skills |
| `skills_pro_role_gating` | P | v1 | Per-role skill bundle access control |

#### Peer mailbox
| Key | Tier | Status | Description |
|---|---|---|---|
| `core_pro_peer_mailbox_router` | P | v1 | Inter-worker message scope validation + routing |
| `core_pro_peer_mailbox_ledger` | P | **v1.1** (gated on Risk #22 ledger pruning) | At-least-once delivery state machine |
| `core_pro_peer_mailbox_stranded_recon` | P | **v1.1** | Background reconciler forcibly transitioning orphans to `Undeliverable` |

#### System events
**Revised 2026-04-26 (Wave 4 gate-review pass).** Per gemini findings:
- Removed `core_core_license_event_broadcast` (system wiring required for tier transitions to propagate \u2014 gating it is circular nonsense; the broadcast is intrinsic to the licensing layer).
- Renamed `core_core_ext_notification` \u2192 `core_core_agent_notification` (drops impl-leak `ext_` prefix from `AgentExtNotification` enum name).

Net 4 Free keys (was 5).

| Key | Tier | Status | Description |
|---|---|---|---|
| `core_core_conflict_detection` | F | v1 | `ConflictDetected` event emission + frontend surfacing |
| `core_core_rate_limit_detection` | F | v1 | `RateLimitDetected` event emission for ACP throttling (prevents runaway billing) |
| `core_core_permission_request_detection` | F | v1 | One-shot `PermissionRequest` event detection + surfacing from orchestrator (security baseline; renamed from `_prompt` per gemini gate-review symmetric with `license_event_broadcast` removal: `_prompt` is UI wiring, `_detection` matches sibling `_detection` keys) |
| `core_core_agent_notification` | F | v1 | Agent-originated out-of-band notifications: progress, rate-limit, intent-to-send (advanced peer-payload routing gated separately by `core_pro_peer_mailbox_router`) |

#### Reliability & lifecycle
**Revised 2026-04-26 (Wave 4 gate-review pass).** Per gemini findings symmetric with Task 7 review-key revision: dropped `basic_` prefix (impl leak); moved `plan_orphan_recovery` (Risk #13 safety baseline) and `background_task_tracker` (Risk #6 lifecycle hygiene) from Pro to Free. Free users persisting plans must not lose work to startup-time orphans; Rust async hygiene is not a premium feature. Pro retains `event_replay` as the only true upsell (full lineage rebuild beyond live re-attach).

| Key | Tier | Status | Description |
|---|---|---|---|
| `core_core_session_resume` | F | v1 | Process-restart re-attach to live brain session |
| `core_pro_session_resume_event_replay` | P | **v1.1-Q3** | Full lineage rebuild from NDJSON replay (Risk #9 fix) — Pro upgrade beyond live re-attach |
| `core_core_plan_persistence` | F | v1 | Single in-flight plan survives restart |
| `core_core_plan_orphan_recovery` | F | v1 | `recover_persisted_plans()` startup orphan reclamation (Risk #13 partial) — safety baseline; Free plans must not orphan permanently |
| `core_core_background_task_tracker` | F | v1 | `JoinHandle` + abort-on-Drop tracking (Risk #6 mitigation) — Rust async hygiene baseline |

### 4.3 `spur-mcp` (Brain→SPUR bridge — 14 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `mcp_core_server_dispatch` | F | v1 | MCP server tool dispatch loop |
| `mcp_core_delegate` | F | v1 | `delegate_to_worker`, `delegate_parallel`, `cancel_delegation`, `list_available_workers` (renamed from `_delegate_basic` per gemini gate-review: no `_advanced` Pro counterpart \u2014 advanced delegation behaviors are gated by `pm_pro_*` and `mcp_pro_mutation_executor`) |
| `mcp_core_outcome_fetch` | F | v1 | `fetch_outcome_artifact`, `get_task_diff` |
| `mcp_core_pm` | F | v1 | `get_issue`, `list_issues`, `create_issue`, `update_issue` (acting on `pm_core_*`; renamed from `_pm_basic` per gemini gate-review: orphan `_basic` suffix without Pro counterpart) |
| `mcp_core_pr` | F | v1 | `create_pr` user-initiated via MCP tool (renamed from `_pr_manual`: orphan `_manual` suffix without Pro counterpart) |
| `mcp_core_plan_ephemeral` | F | v1 | `submit_plan` in-memory (any size, lost on restart) |
| `mcp_core_outcome_materializer` | F | v1 | Store→clip→build with truncation ladder (`MERGE_BUDGET = 8192B`) |
| `mcp_pro_plan_durable` | P | v1 | `submit_plan(persist_as_epic=true)`, `execute_epic`, multi-session |
| `mcp_pro_reconciler_journal_notify` | P | v1 | `monitor_journal_appends` 250ms wake mechanism (Risk #19 mitigated) |
| `mcp_pro_signal_watcher_scope_drift` | P | v1 | Autonomous drift detection + mutation proposals |
| `mcp_pro_mutation_executor` | P | v1 | Apply plan mutations (label ops, signal markers) |
| `mcp_pro_graph_tools` | P | v1 | `graph_plan`, `graph_subgraph`, `graph_alerts`, `graph_insights`, `graph_triage` |
| `mcp_pro_review` | P | v1 | `review_task` with auto-merge gating policies (renamed from `_review_advanced`: orphan `_advanced` suffix without Free counterpart) |
| `mcp_pro_custom_tools` | P | v1 | Register org-internal MCP tools |

### 4.4 `spur-tui` (Terminal interface — 10 keys)

**Revised 2026-04-26 (Wave 5 design-review pass).** Per 4-reviewer synthesis (kimi mechanical, codex code-grounded, claude-code consistency, gemini design-smells):
- Renamed `_notification_in_tui_drain` → `_notification_drain` (drop redundant `_in_tui_` infix; crate prefix already says tui — claude-code consistency).
- Moved `tui_pro_telegram_bot_solo` → `bot_pro_telegram_solo` (gate point is at CLI launch / bot subsystem, not TUI — codex code-grounded; deferred to spur-bot Task 19).
- Deferred `tui_pro_trace_source_react` to v1.1 backlog (codex confirmed the palette source is explicitly `// TODO` deferred; aspirational keys do not belong in v1 registry).
- Deferred `tui_pro_custom_keybindings` to v2 backlog (codex confirmed vaporware: only fixed handlers + Vim/Emacs edit mode exist; no configurable keymap subsystem).
- View-per-key granularity preserved against gemini's "umbrella" critique — keeps future tier-move flexibility (e.g. plan_inspector could become Pro later).

| Key | Tier | Status | Description |
|---|---|---|---|
| `tui_core_view_dashboard` | F | v1 | Global activity log, brain/worker status |
| `tui_core_view_session_detail` | F | v1 | Per-session event stream + notification drain |
| `tui_core_view_plan_inspector` | F | v1 | DAG visualization for in-flight plans |
| `tui_core_view_palette_overlay` | F | v1 | Ctrl+K command palette |
| `tui_core_view_issue_browser` | F | v1 | Browse Linear/GitHub/beads issues (Free tier read-only — write actions like status mutation gated separately by `pm_pro_*` keys) |
| `tui_core_view_landing_decision` | F | v1 | Boot routing (`--new`, `--session`, `--sessions`) |
| `tui_core_view_composer` | F | v1 | Multi-line input composer |
| `tui_core_modal_collision_escape` | F | v1 | `kill <pid>` escape-hatch UI for rejected attaches |
| `tui_core_input_paste_as_atom` | F | v1 | LRU-50 multi-line paste atomic placeholders + `ProtectedRange` |
| `tui_core_notification_drain` | F | v1 | 8-event-per-frame display buffer (governs TUI frame responsiveness) |

### 4.5 `spur-cli` (Command surface — 9 keys)

**Revised 2026-04-26 (Wave 5 design-review pass).** Per claude-code consistency: dropped `_command_` infix on every key (cli crate's only public capabilities ARE subcommands; infix carries zero discriminating information — matches spur-mcp precedent where 14 MCP tools are named `mcp_core_pm`/`_pr`/`_delegate`, not `mcp_core_tool_*`). Per codex code-grounded: dropped `_version` (Clap built-in attribute, not a separate dispatch site to gate); deferred `_upgrade_trial` and `_upgrade_pro` to v1.1 backlog (no `Commands::Upgrade` in code yet); deferred `_workflow` to v2 backlog (Phase 3 stub). Per codex: `_license_activate` retained but description updated to reflect actual implementation (`spur auth login --key`).

Gemini's "drop all CLI keys as routing facade" critique rejected: keeping CLI as gate-layer is consistent with the established treatment of MCP tool dispatch (also a routing layer) and provides defense-in-depth before downstream crate enforcement.

| Key | Tier | Status | Description |
|---|---|---|---|
| `cli_core_init` | F | v1 | `spur init` — workspace scaffolding |
| `cli_core_agents` | F | v1 | `spur agents` — vendor configuration & registry |
| `cli_core_sessions` | F | v1 | `spur sessions` — list active/historical |
| `cli_core_run` | F | v1 | `spur run "<task>"` — ad-hoc execution |
| `cli_core_exec` | F | v1 | `spur exec --agent <name>` — direct vendor execution |
| `cli_core_tui` | F | v1 | `spur tui` — open dashboard |
| `cli_core_cost` | F | v1 | `spur cost` — basic cost summary (defense-in-depth gate; primary enforcement at `cost_core_summary`) |
| `cli_core_connect` | F | v1 | `spur connect` — GitHub auth/connect (description was previously "socket bindings"; corrected to match implementation) |
| `cli_core_license_activate` | F | v1 | `spur auth login --key <key>` — write license to `~/.spur/license`, arc_swap reload (canonical form is `auth login`; `license activate` is a planned alias) |

### 4.6 `spur-pm` (Project Management — 5 keys)

**Revised 2026-04-26 (Wave 5 design-review pass).** Major rationalization per codex code-grounded review (capabilities lived in different crates) + claude-code naming consistency:

**Renames (codex + claude-code):**
- `pm_core_pm_read` → `pm_core_browse` (drop awkward duplicate `pm_pm_` segment; description scoped to actually-implemented Beads + GitHub adapters; Linear/Plane stubs are enum-only).
- `pm_core_pr_manual` → `pm_core_pr` (cross-crate parity with just-merged `mcp_core_pr` — same `_manual` orphan-suffix rule applied; both crates intentionally gate the same capability at different surfaces).
- `pm_core_bv_adapter` → `pm_core_beads_graph_adapter` (drop opaque `bv` abbreviation; reframe as capability noun rather than tool name; survives a future tool swap).

**Deferred to v1.1/v2 backlog (codex code-grounded findings — capability lives in another crate or is vaporware):**
- `pm_pro_github_auto` — actual implementation lives in `crates/spur-mcp/src/plan/reconciler.rs`. Roll into a future `mcp_pro_pr_auto` key (deferred to spur-mcp v1.1 follow-up).
- `pm_pro_linear_sync` — vaporware (only `PmSource::Linear` enum value exists; no adapter). v2 backlog.
- `pm_pro_plane_sync` — vaporware (only `PmSource::Plane` enum value exists). v2 backlog.
- `pm_pro_signal_watcher` — duplicates already-merged `mcp_pro_signal_watcher_scope_drift` (real implementation lives in spur-mcp). Drop.
- `pm_pro_auto_merge` — covered by already-merged `mcp_pro_review` (auto-merge gating policies live in MCP review path). Drop.
- `pm_team_webhooks` — vaporware (no receiver implementation in spur-pm). v2 backlog.

**Narrowed scope:** `pm_pro_beads_advanced` retained, but description narrowed from the previous "plan persistence + projection + mutation + signal-watch + auto-merge" omnibus (those live in spur-mcp) to its actual PM-crate boundary: the `PmService::advanced()` activation gate + `BeadsAdvanced` extension surface.

| Key | Tier | Status | Description |
|---|---|---|---|
| `pm_core_beads_basic` | F | v1 | Standard beads CRUD via `br` CLI (paired with `pm_pro_beads_advanced` — `_basic`/`_advanced` is a real Free/Pro capability split, not an orphan tier-flavor adjective) |
| `pm_core_browse` | F | v1 | Browse PM tracker (Beads + GitHub Issues today; Linear/Plane adapters deferred to v2) |
| `pm_core_pr` | F | v1 | User-initiated `create_pr` via `gh` CLI through `PmService::create_pr` |
| `pm_core_beads_graph_adapter` | F | v1 | `bv` (beads-viewer) graph-aware analysis adapter integration |
| `pm_pro_beads_advanced` | P | v1 | `PmService::advanced()` activation + `BeadsAdvanced` extension surface (the Pro PM-side gate; downstream advanced capabilities — plan persistence, signal watching, auto-merge — gate separately at `mcp_pro_*` keys) |

### 4.7 `spur-cost` (Cost tracking — 3 keys)

**Revised 2026-04-27 (Wave 6 L9-Rust+data-engineer first-principles pass).** 6 → 3 keys per 4-reviewer judge synthesis:
- Renamed `cost_core_basic_display` → `cost_core_session_display` (claude-code: drop `_basic_` orphan; codex: scoped to actual `today_summary` ledger).
- Removed `cost_core_ingestion_pipeline` — always-coupled prerequisite to all cost capabilities; cannot be independently gated (a tier with zero ingestion has zero cost features). Codex confirmed code is JSONL-only, not ACP, so the original description was inaccurate anyway.
- Removed `cost_pro_sqlite_wal_mode` — codex ❌ NOT IMPLEMENTED (`init_db` opens SQLite without `PRAGMA journal_mode=WAL`). If implemented, would be a database-correctness baseline (Risk #29) belonging to Free per Wave 4 safety/liveness precedent, not a Pro upsell.
- Deferred `cost_pro_budget_caps` to v1.1 backlog — codex ❌ no spawn/runtime enforcement; tracker is observational only.

| Key | Tier | Status | Description |
|---|---|---|---|
| `cost_core_session_display` | F | v1 | Per-session running cost ledger via `CostTracker::today_summary` (renamed from `_basic_display`: `_basic_` is an orphan tier-flavor adjective per claude-code) |
| `cost_core_pricing_registry` | F | v1 | `PricingRegistry` model pricing lookup table |
| `cost_pro_per_project_tracking` | P | v1 | Per-project aggregation via `CostTracker::by_project` SQL grouping (Free path skips this query branch) |

### 4.8 `spur-context` (DuckDB analytics — 3 keys)

**Revised 2026-04-27 (Wave 6 L9-Rust+data-engineer first-principles pass).** 5 → 3 keys per 4-reviewer judge synthesis:
- Removed `ctx_pro_async_engine` — codex ⚠ no production callers found for `AsyncEngine`. Pure threading infrastructure with no user-visible boundary. Drop, do not defer.
- Deferred `ctx_pro_live_mode` to v1.1 backlog — codex ⚠ APIs exist (`LiveSessionTracker`) but no CLI/user surface; gate has no enforcement point yet.

| Key | Tier | Status | Description |
|---|---|---|---|
| `ctx_pro_duckdb_engine` | P | v1 | `AnalyticsEngine` in-memory DuckDB engine |
| `ctx_pro_daily_report` | P | v1 | Day-bucketed cost/time reports (CLI uses `Reporter::daily_report`) |
| `ctx_pro_weekly_report` | P | v1 | Week-bucketed cost/time reports (engine support ready; CLI gap acceptable) |

### 4.9 `spur-worktree` (Git isolation — 2 keys)

**Revised 2026-04-27 (Wave 6 L9-Rust+data-engineer first-principles pass).** 5 → 2 keys per 4-reviewer judge synthesis:
- Removed `worktree_core_artifact_resolver` — always-on for the system to function (delegation outcomes can't be returned without artifact lookup); not independently gateable.
- Renamed `worktree_pro_cleanup_orphans` → `worktree_core_orphan_cleanup` AND moved Pro→Free (claude-code: verb→noun; codex confirmed code exists at `manager.rs:539` + `worktree_authority.rs:99` so v1.1 status flag was wrong; Wave 4 safety/liveness precedent: garbage collection is a correctness invariant, never a paywall — analogous to Postgres VACUUM, RocksDB compaction).
- Deferred `worktree_pro_git_blob_store` to v1.1 backlog — codex ⚠ orchestrator hardwires GitBlob at `orchestrator.rs:963`; no Free/Pro backend selector exists, so the gate has nothing to enforce.
- Deferred `worktree_pro_custom_policies` to v1.1 backlog — codex ❌ only single cherry-pick path exists; squash/rebase/octopus/naming templates are vaporware.

| Key | Tier | Status | Description |
|---|---|---|---|
| `worktree_core_isolation` | F | v1 | Per-delegation `git worktree` create/destroy via `WorktreeManager::create_worktree` |
| `worktree_core_orphan_cleanup` | F | v1 | Safe global orphan cleanup via `WorktreeAuthority::sweep_once` (Risk #4 mitigation; renamed from `worktree_pro_cleanup_orphans`; tier shifted Pro→Free per safety/liveness precedent — disk-filling worktrees on Free would break daily-driver positioning §2) |

### 4.10 `spur-bot` (Telegram subsystem — 3 keys)

**Revised 2026-04-26 (Wave 5).** Added `bot_pro_telegram_solo` (relocated from spur-tui §4.4); per codex code-grounded review the gate point is `Commands::Bot` (`crates/spur-cli/src/main.rs:591`) / `run_telegram_bot` (`crates/spur-bot/src/telegram/mod.rs:9`), with single-operator filter at `router.rs:25`. Gating in spur-tui would let users bypass via `spur bot ...` CLI invocation.

**Revised 2026-04-27 (Wave 6 L9-Rust+data-engineer first-principles pass).** 7 → 3 keys per 4-reviewer judge synthesis. Core principle: bot sub-keys must be *independently business-toggleable*, not just real boundaries in code. Codex confirmed all 5 sub-keys map to real code, but multiple are tightly-coupled mechanism with no plausible tier-axis:
- Removed `bot_pro_runtime` — always-coupled to telegram_solo (no bot without long-poll loop). Folded under umbrella.
- Removed `bot_pro_runtime_render` — always-coupled to runtime (no telegram bot would ship without markdown rendering; "raw text mode" is degenerate UX, not a tier).
- Removed `bot_pro_callback_validation` — security invariant (analogous to dropped `license_core_ed25519_verify`); never a Pro upsell. Disabling it = exploit (any user could replay any callback).
- Deferred `bot_team_multi_chat` to v2 backlog — codex ❌ no multi-user code (single `operator_user_id` config; router rejects all others).

Retained the 3 keys with plausible business tier axes:

| Key | Tier | Status | Description |
|---|---|---|---|
| `bot_pro_telegram_solo` ★ | P | v1 | Single-operator Telegram remote-control (user-facing umbrella; gate at `Commands::Bot` / `run_telegram_bot`) |
| `bot_pro_thread_registry` | P | v1 | Forum-topic-per-session multiplexing via `PersistedBotState.threads` (real product axis: single-thread vs multi-thread bots are distinguishable customer-visible tiers) |
| `bot_pro_inline_review` | P | v1 | Inline keyboard review buttons (real product axis: passive notify-only bot vs interactive review bot are distinguishable customer-visible tiers) |

### 4.11 `spur-license` (Entitlements — 2 keys)

**Revised 2026-04-27 (Wave 6 L9-Rust+data-engineer first-principles pass).** 6 → 2 keys per 4-reviewer judge synthesis. The original 6-key set conflated *runtime gating dispatch table* (the FeatureKey registry's actual purpose) with *system manifest documentation* (which belongs in this spec, not in `feature_key.rs`).

Removed via **Bootstrap Paradox** principle (gemini + codex aligned): a feature gate cannot meaningfully toggle the gating system that implements it.
- `license_core_facade_entitlement` — IS the gating mechanism (`FeatureGate::has`); cannot gate itself.
- `license_core_policy_resolver` — must run for ANY policy (including one that disables it) to load.
- `license_core_ed25519_verify` — build-time integrity invariant (`build.rs:28`); not a runtime capability.

Removed/renamed per codex code-grounded review:
- Renamed `license_core_provider_heartbeat` → `license_pro_revocation_polling` AND moved Free→Pro — this is a networked Pro capability (LicenseSeat polling for revocations); Free runs offline-only, so the gate is meaningfully tier-axial only at Pro.
- Deferred `license_pro_quota_runtime_downgrade` to v1.1 backlog — codex ⚠ runtime does not visibly propagate license refreshes into `FeatureGate::update_state`; downgrade is not fully enforced.

Retained `license_pro_offline_grace` as Pro v1 — Free has no polling so offline grace is moot/automatic; only meaningfully toggleable for Pro tiers (cached license validity duration during backend unreachability). This is the only non-paradoxical license-system capability worth gating today.

| Key | Tier | Status | Description |
|---|---|---|---|
| `license_pro_revocation_polling` | P | v1 | LicenseSeat backend polling for revocations (renamed from `license_core_provider_heartbeat`; tier shifted Free→Pro: Free runs offline-only with embedded license, polling is a Pro networked capability) |
| `license_pro_offline_grace` | P | v1 | Configurable offline grace period during backend unreachability (Risk #31 mitigation; meaningful only for Pro since only Pro polls) |

**Note on dropped license-meta keys:** the spur-license crate's components (entitlement facade, policy resolver, ed25519 verifier, provider heartbeat) are documented as system invariants in `docs/architecture.md` and the spec body above. They are NOT in the FeatureKey registry because they are always-on integrity infrastructure, not toggleable runtime gates.

### 4.12 `spur-blob-store` (Outcome storage — 1 key)

| Key | Tier | Status | Description |
|---|---|---|---|
| `blob_pro_namespace_deletion` | P | v1 | `DeleteNamespaceReport` bulk cleanup, gated at `spur gc outcomes --namespace` CLI route |

**Wave 7 drops:** `blob_core_memory_backend`, `blob_core_fs_backend`, `blob_pro_measured_backend` are trait-impl variants chosen at construction time, not user-toggleable capabilities. Production wiring is hardwired to `MeasuredOutcomeStore<GitBlobOutcomeStore>` at `crates/spur-core/src/orchestrator.rs:963`; `spur gc outcomes` constructs `GitBlobOutcomeStore` directly, bypassing the wrapper. See §4.16.

### 4.13 `spur-interactive` (Shared host — 0 keys)

Wave 7 dropped all 3 candidate keys (`interactive_core_frontend_host`, `interactive_core_review_lane_mpsc`, `interactive_core_shutdown_orchestrator`). All three describe production invariants of the shared host shared by TUI + Telegram bot: the host itself is structural infrastructure (not tier-gated), the review lane MPSC separation is a production correctness invariant (not a feature toggle), and shutdown orchestration is always-on lifecycle hygiene. None are independently toggleable. See §4.16.

### 4.14 Notifications (cross-crate — 0 keys)

Wave 7 dropped both candidate keys. `notif_core_in_tui` was redundant with already-merged `core_core_notification_pump` + `tui_core_notification_drain` (triple-naming the same path). `notif_pro_external_channels` is greenfield vaporware (no Slack/Discord/email/webhook code exists) and was deferred per the Wave 5/6 vaporware precedent. The entire `notif_*` namespace evaporates from v1; no orphan prefix remains. See §4.16.

---

### 4.15 Registry summary

Note on grouping: `skills_*` prefix keys live in `spur-core` code (under `spur-core/src/skills/`) but are listed in their own row below for grep-discoverability; they're counted in the Skills row, not double-counted under spur-core.

**Revised 2026-04-27 (Wave 8.5 finalization).** Total **v1 registry** keys are **63** post-Wave-8.5. Wave 9 was 2 surgical Pro→Free tier-shifts (with const renames), not consolidations or drops; Wave 8.5 then dropped `acp_core_adapter_gemini` because ACP has no dedicated `AgentKind::Gemini` adapter. Tier composition is now Free 47, Pro v1 15, Pro v1.1 1. Trajectory across all waves: 135 → 123 → 107 → 99 → 64 (Wave 8) → 64 (Wave 9 tier composition only) → 63 (Wave 8.5 ghost adapter drop). Wave 8 amendment to the core principle: *FeatureKey registry is a runtime gate dispatch table — toggleable capabilities AND each key's on/off must compose validly with sibling keys in its family.* Wave 8 identified 15 over-decomposed families where partial enablement breaks tier integrity (compile-coupled APIs, all-or-nothing valid substates, producer/consumer chains where one half is meaningless without the other) and consolidated each family into a single umbrella key. Plus 5 additional drops (ghost adapters, mechanism plumbing) and 5 vaporware deferrals. See **§4.16 Deferred-keys backlog** for full Wave-8/8.5 detail. Counts below show only what's in the v1 Plan A registry post-Wave-8.5.

| Crate / Subsystem | Free keys | Pro keys (v1) | Pro keys (v1.1) | Team-deferred |
|---|---|---|---|---|
| `spur-acp` (transports + 3 implemented adapters + 1 session_attach; -4 ghost adapters dropped, -1 degraded_nolock merged) | 6 | 0 | 0 | 0 |
| `spur-core` (post-Wave-8 consolidations; Wave 9 tier-shifted review_retry_config Pro→Free) | 9 | 3 | 1 | 0 |
| `skills_*` (in spur-core) — Wave 8 quartet→1 | 1 | 1 | 0 | 0 |
| `spur-mcp` (post-Wave-8: delegate absorbs materializer, plan_durable absorbs notify, signal_watcher absorbs mutation_executor; Wave 9 tier-shifted graph_tools Pro→Free) | 7 | 3 | 0 | 0 |
| `spur-tui` (post-Wave-8: dashboard absorbs landing+composer; notification_drain absorbed by core_core_event_pipeline) | 7 | 0 | 0 | 0 |
| `spur-cli` (KEEP_ATOMIC) | 9 | 0 | 0 | 0 |
| `spur-pm` (KEEP_ATOMIC; advanced→basic prereq) | 4 | 1 | 0 | 0 |
| `spur-cost` (KEEP_ATOMIC; pricing_registry prereq) | 2 | 1 | 0 | 0 |
| `spur-context` (post-Wave-8: duckdb_engine absorbs both reports) | 0 | 1 | 0 | 0 |
| `spur-worktree` (KEEP_ATOMIC; cleanup→isolation prereq) | 2 | 0 | 0 | 0 |
| `spur-bot` (post-Wave-8: telegram_solo absorbs thread_registry; inline_review kept separate for security opt-out) | 0 | 2 | 0 | 0 |
| `spur-license` (KEEP_ATOMIC; offline_grace→revocation_polling prereq) | 0 | 2 | 0 | 0 |
| `spur-blob-store` | 0 | 1 | 0 | 0 |
| `spur-interactive` | 0 | 0 | 0 | 0 |
| Notifications (cross-crate) | 0 | 0 | 0 | 0 |
| **Total** | **47** | **15** | **1** | **0** |

**Total v1 atomic feature keys: 63** (47 + 15 + 1 + 0) post-Wave-8.5 — was 64 post-Wave-9, 99 post-Wave-7, 107 post-Wave-6, 123 post-Wave-5, 135 before Wave 5. Wave 8 net reduction of 35 keys reflects: (a) 15 family consolidations (compile-coupled / all-or-nothing substate space) — see §4.16 Wave 8 consolidations table; (b) 4 additional drops (`background_task_tracker` mechanism plumbing + 3 ghost ACP adapters with no `AgentKind` variants); (c) 5 vaporware deferrals to v1.1 backlog (`brain_failover_auto_pool`, `broadcast_lagged_recovery`, `conflict_detection`, `rate_limit_detection`, `mcp_pro_custom_tools` — all confirmed no production code by codex tracing). Wave 8.5 adds one final ghost-adapter drop: `acp_core_adapter_gemini`, because Gemini currently falls under `AgentKind::Generic` and has no dedicated ACP adapter. Pro v1.1 column shrinks from 5 to 1 because all 4 prior v1.1-tagged keys (auto_pool, lagged_recovery, peer_mailbox_ledger, peer_mailbox_stranded_recon) were either deferred to §4.16 or absorbed into umbrella consolidations.

Wave 7 net reduction of 8 keys (preserved historical record): (a) 6 keys dropped as trait-impl variants / production invariants / always-on infrastructure (`blob_core_memory_backend`, `blob_core_fs_backend`, `blob_pro_measured_backend`, `interactive_core_frontend_host`, `interactive_core_review_lane_mpsc`, `interactive_core_shutdown_orchestrator`); (b) 1 key dropped as redundant with already-merged keys (`notif_core_in_tui`); (c) 1 key deferred to §4.16 v1.1 backlog (`notif_pro_external_channels`); (d) 1 key kept with rename (`blob_pro_delete_namespace` → `blob_pro_namespace_deletion`).

The registry naturally lands close to but below the original ~110 target. The continued drop reflects that earlier spec drafts conflated *runtime gate dispatch* (this registry's purpose) with *system manifest documentation* (which lives in this spec body and `docs/architecture.md`).

**Pro v1 launch arsenal: 27 features** organized into 5 ★ headline triggers + 22 supporting depth capabilities.

**Pro v1.1 roadmap: 5 features in v1 registry** (deferred-status flag) + 10 backlog items in §4.16 below = 15 v1.1 candidates total, target Q3 2026.

### 4.16 Deferred-keys backlog (Wave 5 + Wave 6 design-review passes)

The following keys were proposed in earlier drafts but deferred to v1.1 or v2, or dropped entirely, after multi-reviewer judge synthesis (4-reviewer pattern: kimi mechanical / codex code-grounded / claude-code consistency / gemini design-smells; judge synthesizes via L9 first-principles MCTS). Documented here as a single source of truth so future tier-plan or Plan A follow-up authors don't reinvent them.

#### v1.1 backlog (will land when implementation exists)

| Original key | Replacement | Reason | Wave |
|---|---|---|---|
| `tui_pro_telegram_bot_solo` | → `bot_pro_telegram_solo` (now in §4.10) | Gate point belongs in spur-bot crate, not spur-tui (codex code-grounded). | 5 |
| `tui_pro_trace_source_react` | (later) | Palette `TraceSource` is explicitly `// TODO` deferred in `app.rs:430,458,470` — no active wiring. | 5 |
| `cli_core_command_upgrade_trial` | `cli_core_upgrade_trial` (later) | No `Commands::Upgrade` in CLI yet. Lands with trial implementation per §6.2. | 5 |
| `cli_core_command_upgrade_pro` | `cli_core_upgrade_pro` (later) | No Stripe/browser checkout command yet. | 5 |
| `pm_pro_github_auto` | `mcp_pro_pr_auto` (later) | Auto-PR-on-success lives in `crates/spur-mcp/src/plan/reconciler.rs:623`, not spur-pm. | 5 |
| `cost_pro_budget_caps` | (later, retains name) | Tracker is observational only at `crates/spur-cost/src/tracker.rs:33,70`; no spawn/runtime enforcement. Lands with hard budget enforcement (Risk #17). | 6 |
| `ctx_pro_live_mode` | (later, retains name) | Backend APIs exist (`LiveSessionTracker` at `crates/spur-context/src/live.rs:15`) but no CLI/user surface. Lands with live-report CLI command. | 6 |
| `worktree_pro_git_blob_store` | (later, retains name) | Orchestrator hardwires `GitBlobOutcomeStore` at `crates/spur-core/src/orchestrator.rs:963`; no Free/Pro backend selector exists. Lands with backend-routing capability. | 6 |
| `license_pro_quota_runtime_downgrade` | (later, retains name) | Quotas dynamic in `FeatureGate::ArcSwap` but runtime does not propagate license refreshes into `update_state`. Lands with mid-session downgrade enforcement (Risk #32 fix). | 6 |
| `notif_pro_external_channels` | (later, retains name) | Greenfield — no Slack/Discord/email/webhook router or channel-adapter surface exists. Telegram already has its own `bot_pro_*` keys. Lands when a real cross-channel notification subsystem is built. Spec already flagged greenfield at `docs/.../specs/2026-04-26-individual-tier-revamp-design.md:359` (pre-Wave 7). | 7 |

#### v2 backlog (Team or speculative)

| Original key | Reason | Wave |
|---|---|---|
| `tui_pro_custom_keybindings` | Vaporware — no configurable keymap subsystem; only fixed handlers + Vim/Emacs edit mode exist. | 5 |
| `cli_team_command_workflow` | Phase 3 print-only stub (Team-only, was already v2). | 5 |
| `pm_pro_linear_sync` | Vaporware — only `PmSource::Linear` enum value (`types.rs:9`); no adapter. | 5 |
| `pm_pro_plane_sync` | Vaporware — same as Linear (`types.rs:10`); no adapter. | 5 |
| `pm_team_webhooks` | Vaporware — no receiver implementation in spur-pm. | 5 |
| `worktree_pro_custom_policies` | Vaporware — only single cherry-pick path at `crates/spur-worktree/src/manager.rs:402`; no squash/rebase/octopus/naming-template surface. | 6 |
| `bot_team_multi_chat` | Vaporware — single `operator_user_id` config at `crates/spur-acp/src/config/mod.rs:326`; router rejects all other users. Multi-chat requires multi-user RBAC. | 6 |
| `blob_core_memory_backend` | Trait-impl variant chosen at construction time (`MemoryOutcomeStore` is test/dev plumbing at `crates/spur-blob-store/src/memory_store.rs:24`); not a user-toggleable runtime capability. No CLI/UI/config selects it. | 7 |
| `blob_core_fs_backend` | Trait-impl variant; production wiring uses `MeasuredOutcomeStore<GitBlobOutcomeStore>` at `crates/spur-core/src/orchestrator.rs:963`, not `FsOutcomeStore`. No selector exists. | 7 |
| `blob_pro_measured_backend` | Always-on telemetry wrapper hardwired in `Orchestrator::new` at `crates/spur-core/src/orchestrator.rs:963`; `spur gc outcomes` constructs `GitBlobOutcomeStore` directly bypassing the wrapper. Not gateable, not a Pro upsell. | 7 |
| `interactive_core_frontend_host` | Shared infrastructure used by both TUI (`crates/spur-cli/src/main.rs:710`) and Telegram bot (`crates/spur-bot/src/telegram/mod.rs:9`); not tier-gated (Free + Pro both depend on it). API boundary, not a feature toggle. | 7 |
| `interactive_core_review_lane_mpsc` | Production correctness invariant — `SubmitReview` is rejected on the command lane at `crates/spur-interactive/src/host.rs:21`; the lane separation is a sound-architecture requirement, not a tier feature. Toggling it off would break interactive_core_frontend_host's contract. | 7 |
| `interactive_core_shutdown_orchestrator` | Lifecycle hygiene; tightly coupled to `interactive_core_frontend_host` (no host = no shutdown). Always-on safety infrastructure that *must* run. | 7 |
| `notif_core_in_tui` | Redundant with already-merged `core_core_notification_pump` (producer at `crates/spur-core/src/notification_pump.rs:30`) + `tui_core_notification_drain` (consumer at `crates/spur-tui/src/app.rs:2552`); triple-naming the same path with no documented boundary. The `notif_*` namespace's only justification (cross-crate clarity) is undermined when the Free key duplicates per-crate keys. | 7 |

#### Wave 9 — Tier-shifted Pro→Free (Iceberg framework + dual-reviewer synthesis)

Per Wave 9 first-principles + Iceberg framework analysis (gemini strategy/positioning + codex code-grounded dual review with L9-MCTS judge synthesis). Both reviewers converged: 2 keys belong on the Free side of the iceberg to strengthen acquisition without weakening Pro's 5+ headline triggers. Both shifts include `pub const` renames (Pro→Free naming convention). Net to total v1 registry count: 0.

| Original key | Renamed to | Reason | Wave |
|---|---|---|---|
| `mcp_pro_graph_tools` | `mcp_core_graph_tools` | Viral acquisition surface — Plan/dependency graph diagnostics belong above the iceberg waterline as Free demo material. **Codex grounding (narrowed from brain's claim):** output is raw `bv` JSON or Mermaid TEXT at `crates/spur-mcp/src/server.rs:3331-3415`, NOT rendered visual material. Marketing copy: "MCP graph diagnostics / Mermaid text output" — not "Twitter-shareable visual demo." Pro retains `mcp_pro_plan_durable` + `mcp_pro_signal_watcher_scope_drift` as the planning execution-muscle anchors. | 9 |
| `core_pro_review_retry_config` | `core_core_review_retry_config` | Free-tier reliability baseline (Wave 4 safety/liveness precedent: retry resilience is "make Free reliable" not a Pro lever). Composes validly with `core_core_review` umbrella when both are Free — retry only runs when `review_required` is enabled (`crates/spur-core/src/orchestrator.rs:4376-4408`). **Codex grounding (narrowed from brain's claim):** only `max_review_retries` is config; backoff is hard-coded at `crates/spur-core/src/orchestrator.rs:4767-4775`. Marketing copy: "Review retry limit" — not "configurable retry policy." Pro retains `core_pro_review_auto_approve` as the autonomy lever. | 9 |

#### Dropped (capability is not gateable per first-principles analysis)

These keys were considered for v1 but rejected on principle: they describe always-on infrastructure, security baselines, or tightly-coupled mechanism that cannot meaningfully be toggled by a runtime FeatureGate. Documented as system invariants in spec body and `docs/architecture.md` instead.

| Original key | Drop reason | Wave |
|---|---|---|
| `cli_core_command_version` | Clap built-in `#[command(version)]` attribute, not a separate dispatch site to gate. | 5 |
| `pm_pro_signal_watcher` | Duplicates already-merged `mcp_pro_signal_watcher_scope_drift` (Wave 4); real implementation is in spur-mcp. | 5 |
| `pm_pro_auto_merge` | Covered by already-merged `mcp_pro_review` (Wave 4); auto-merge gating policies live in spur-mcp reconciler. | 5 |
| `cost_core_ingestion_pipeline` | Always-coupled prerequisite to all cost capabilities (no ingestion = no `pricing_registry`/`session_display`); not independently gateable. Codex confirmed code is JSONL-only, not ACP, so original description was inaccurate. | 6 |
| `cost_pro_sqlite_wal_mode` | Codex ❌ NOT IMPLEMENTED. If implemented, would be a database-correctness invariant (Risk #29 mitigation) belonging to Free per safety/liveness precedent — never a Pro upsell. | 6 |
| `ctx_pro_async_engine` | Codex ⚠ no production callers found for `AsyncEngine` at `crates/spur-context/src/async_engine.rs:31`. Pure threading infrastructure with no user-visible boundary. | 6 |
| `worktree_core_artifact_resolver` | Always-on for system to function (delegation outcomes can't be returned without artifact lookup); not independently gateable. | 6 |
| `bot_pro_runtime` | Always-coupled to `bot_pro_telegram_solo` (no telegram bot without long-poll loop). Folded under umbrella key. | 6 |
| `bot_pro_runtime_render` | Always-coupled to runtime; "raw text mode" telegram bot is degenerate UX, not a real tier axis. | 6 |
| `bot_pro_callback_validation` | Security invariant (analogous to `license_core_ed25519_verify`); never a Pro upsell. Disabling = exploit (any user could replay any callback). | 6 |
| `license_core_facade_entitlement` | **Bootstrap paradox** — IS the gating mechanism (`FeatureGate::has` at `crates/spur-license/src/gate.rs:40`); cannot meaningfully gate itself. | 6 |
| `license_core_policy_resolver` | **Bootstrap paradox** — must run to load ANY policy (including one that disables it). | 6 |
| `license_core_ed25519_verify` | Build-time integrity invariant (`crates/spur-license/build.rs:28`); not a runtime capability that can be toggled per license. | 6 |
| `core_core_background_task_tracker` | Internal `JoinHandle` ownership; `Drop` aborts background tasks (`crates/spur-core/src/orchestrator.rs:918-923`, `:1048-1128`). Mechanism plumbing, not a user capability. | 8 |
| `acp_core_adapter_cursor` | Ghost adapter — no `AgentKind::Cursor` variant exists in ACP types (`crates/spur-acp/src/types.rs:153-177`); name appears only in skills rendering adapters (`crates/spur-core/src/skills/adapters.rs:17-26`). | 8 |
| `acp_core_adapter_opencode` | Ghost adapter — same reason as cursor. | 8 |
| `acp_core_adapter_kimi` | Ghost adapter — same reason. (Note: `kimi` exists as a SPUR delegation worker, separate namespace from ACP agent adapters.) | 8 |
| `acp_core_adapter_gemini` | Ghost adapter — no dedicated `AgentKind::Gemini` variant exists in ACP types; Gemini is documented as falling under `Generic` until a real adapter is introduced (`docs/spur/acp-meta-conventions.md`). | 8.5 |

#### Wave 8 — Consolidated (multiple keys collapsed into one umbrella)

Per Wave 8 second-order composition analysis (kimi mechanical truth-table + codex code-grounded coupling tracing, L9-MCTS judge synthesis). Wave 8 amendment to the core principle: *FeatureKey registry is a runtime gate dispatch table — toggleable capabilities AND each key's on/off must compose validly with sibling keys in its family.* Each consolidation below has either (a) compile-coupled APIs (constructor requires sibling), (b) all-or-nothing valid substates per truth-table analysis, or (c) producer/consumer chains where one half is meaningless without the other.

| Original keys (collapsed) | Umbrella key | Reason | Wave |
|---|---|---|---|
| `core_core_brain_session` + `core_core_brain_scheduler` + `core_core_continuation_bridge` | `core_core_brain_session` | Scheduler requires active brain session for continuations (`crates/spur-core/src/scheduler.rs:60-198`); bridge only enqueues into orchestrator ingress (`crates/spur-core/src/continuation_bridge.rs:31-94`); session spawn seeds scheduler (`orchestrator.rs:2168-2201`). 1–2 of 8 substates valid. | 8 |
| `core_core_parallel_workers` + `core_core_cancellable_semaphore` | `core_core_parallel_workers` | Semaphore is the parallelism mechanism; cancellation is registered before spawn (`orchestrator.rs:3908-4028`). 2 of 4 substates valid. | 8 |
| `core_core_event_funnel_broadcast` + `core_core_event_sink_ndjson_128mb` + `core_core_executor_lineage_projection` + `core_core_notification_pump` + `core_core_agent_notification` + `tui_core_notification_drain` | `core_core_event_pipeline` | Funnel stamps/broadcasts all events; sink only subscribes to broadcast (`event_sink.rs:28-76`); lineage applied inside funnel (`event_funnel.rs:115-120`); pump emits notifications to event bus; drain consumes from same bus (`app.rs:2500-2575`). All-or-nothing — 2 of 64 substates product-meaningful. | 8 |
| `core_core_review_sink` + `core_core_review_timeout` + `core_core_review_retry` | `core_core_review` | ReviewSink ordering invariant requires register-before-emit; timeout fallback and retry routing both branch off the same review wait loop (`orchestrator.rs:4403-4777`). Timeout/retry without sink = no receiver to wait on. 2 of 8 valid. | 8 |
| `core_pro_review_auto_approve` + `core_pro_review_timeout_routing` | `core_pro_review_auto_approve` | Auto-approve IS a timeout fallback; without timeout routing it never fires (`crates/spur-acp/src/config/mod.rs:261-267`, `orchestrator.rs:4532-4573`). Same Pro review loop config. | 8 |
| `core_pro_peer_mailbox_router` + `core_pro_peer_mailbox_ledger` + `core_pro_peer_mailbox_stranded_recon` | `core_pro_peer_mailbox_router` | Router constructor requires ledger + reconciler — split keys are compile-incoherent (`crates/spur-core/src/peer_mailbox/router.rs:55-68`); orchestrator constructs all three as one bundle (`orchestrator.rs:1070-1100`); guard drop depends on reconciler (`peer_mailbox/guard.rs:30-105`). | 8 |
| `core_core_plan_persistence` + `core_core_plan_orphan_recovery` | `core_core_plan_persistence` | Orphan recovery is a safety baseline — Free plans must not orphan permanently. `(persist=ON, recover=OFF)` produces orphans, only valid as bug. | 8 |
| `skills_core_registry` + `skills_core_atomic_installation` + `skills_core_render_per_vendor` + `skills_pro_role_gating` | `skills_core_registry` | Installer run loop combines registry, per-adapter render, role gating, and install in single code path (`crates/spur-core/src/skills/installer.rs:260-287`). All-or-nothing — crippling skills cripples agent quality. | 8 |
| `ctx_pro_duckdb_engine` + `ctx_pro_daily_report` + `ctx_pro_weekly_report` | `ctx_pro_duckdb_engine` | Reports are wrappers over `AnalyticsEngine` (`crates/spur-context/src/reporter.rs:10-12`, `lib.rs:21-29`). Reports without engine = no data source. | 8 |
| `mcp_core_delegate` + `mcp_core_outcome_materializer` | `mcp_core_delegate` | Materializer is the single producer of persisted outcomes (`crates/spur-mcp/src/outcome_materializer.rs:1-140`); delegate handler couples to materializer in server (`crates/spur-mcp/src/server.rs:2256-2655`). Materializer is back-end mechanism without independent MCP tool surface. | 8 |
| `mcp_pro_plan_durable` + `mcp_pro_reconciler_journal_notify` | `mcp_pro_plan_durable` | Reconciler only spawns with beads + notify enabled (`crates/spur-mcp/src/server.rs:1999-2056`); journal notify is part of durable plan mechanism. | 8 |
| `mcp_pro_signal_watcher_scope_drift` + `mcp_pro_mutation_executor` | `mcp_pro_signal_watcher_scope_drift` | Watcher directly imports + calls `apply_mutation` (`crates/spur-mcp/src/plan/signal_watcher.rs:1-20`, `:152-172`). Compile-coupled; non-composable as separate keys. | 8 |
| `acp_core_session_attach_advisory_lock` + `acp_core_session_attach_degraded_nolock` | `acp_core_session_attach_advisory_lock` | `DegradedNoLock` is *only* an outcome of attempting advisory_lock (`crates/spur-acp/src/session_lock.rs:28-146`). Cannot exist as separate gate — it's a fallback path. | 8 |
| `tui_core_view_dashboard` + `tui_core_view_landing_decision` + `tui_core_view_composer` | `tui_core_view_dashboard` | App owns one view state graph (`crates/spur-tui/src/action.rs:153-160`, `app.rs:171-178`); landing_decision is first-launch UX (one-shot, not a runtime gate); composer is internal input ownership (`session_detail.rs:1038-1148`). | 8 |
| `bot_pro_telegram_solo` + `bot_pro_thread_registry` | `bot_pro_telegram_solo` | Telegram bot requires topics/thread sessions; runtime owns thread/session/executor maps (`crates/spur-bot/src/runtime.rs:89-118`); callbacks carry thread IDs (`router.rs:40-66`). Single-thread is degraded telegram_solo, not a separate axis. | 8 |

#### Wave 8 / 8.5 — Additional drops (non-toggleable / ghost / mechanism plumbing)

(See main Dropped table above for: `core_core_background_task_tracker`, `acp_core_adapter_cursor`/`_opencode`/`_kimi`, and Wave 8.5 `acp_core_adapter_gemini`.)

#### Wave 8 — Additional v1.1 backlog (vaporware confirmed by codex code-grounding)

| Original key | Replacement | Reason | Wave |
|---|---|---|---|
| `core_pro_brain_failover_auto_pool` | (later, retains name OR umbrella) | No alternate brain pool exists; reconnect just escalates same brain (`crates/spur-core/src/orchestrator.rs:3384-3550`). Lands when auto-respawn pool is built (Risk #8 fix). | 8 |
| `core_pro_broadcast_lagged_recovery` | (later, retains name) | Only warn/drop on lag; no recovery logic (`crates/spur-tui/src/app.rs:2500-2575`). Lands with real lag-recovery implementation (Risk #2/#9 fix). | 8 |
| `core_core_conflict_detection` | (later, retains name) | Event variant exists (`crates/spur-acp/src/domain/events.rs:595-600`) and TUI render case (`dashboard.rs:1751`); no production emitters. Lands when conflict-emission code ships. | 8 |
| `core_core_rate_limit_detection` | (later, retains name) | Same pattern — event variant + TUI render case but no emitters (`dashboard.rs:1777`). Lands with rate-limit detection emission. | 8 |
| `mcp_pro_custom_tools` | (later, retains name) | No dynamic custom tool registry in `tools_list` (`crates/spur-mcp/src/tools.rs:671-699`). Lands when dynamic tool registration ships. | 8 |

---

## 5. PolicyDocument JSON

Issued by `spur-policy-2026-04` Ed25519 key. Compile-time check via `build.rs` per existing `spur-license` infrastructure.

```json
{
  "schema_version": 2,
  "issued_at": "2026-04-26T00:00:00Z",
  "expires_at": "2027-04-26T00:00:00Z",
  "policy_version": "v1.0-individual",
  "tier_policies": {
    "community": {
      "features": [
        "acp_core_transport_stdio", "acp_core_transport_socket",
        "acp_core_adapter_claude_code", "acp_core_adapter_codex",
        "acp_core_adapter_gemini", "acp_core_adapter_kiro",
        "acp_core_adapter_cursor", "acp_core_adapter_opencode",
        "acp_core_adapter_kimi",
        "acp_core_session_attach_advisory_lock",
        "acp_core_session_attach_degraded_nolock",

        "core_core_brain_session", "core_core_brain_scheduler",
        "core_core_brain_failover_manual_keystroke",
        "core_core_continuation_bridge",
        "core_core_parallel_workers", "core_core_cancellable_semaphore",
        "core_core_event_funnel_broadcast",
        "core_core_event_sink_ndjson_128mb",
        "core_core_executor_lineage_projection",
        "core_core_notification_pump",
        "core_core_review_sink",
        "core_core_review_timeout",
        "core_core_review_retry",
        "skills_core_registry", "skills_core_atomic_installation",
        "skills_core_render_per_vendor",
        "core_core_conflict_detection", "core_core_rate_limit_detection",
        "core_core_permission_request_detection", "core_core_agent_notification",
        "core_core_session_resume", "core_core_plan_persistence",
        "core_core_plan_orphan_recovery", "core_core_background_task_tracker",

        "mcp_core_server_dispatch", "mcp_core_delegate",
        "mcp_core_outcome_fetch", "mcp_core_pm",
        "mcp_core_pr", "mcp_core_plan_ephemeral",
        "mcp_core_outcome_materializer",

        "tui_core_view_dashboard", "tui_core_view_session_detail",
        "tui_core_view_plan_inspector", "tui_core_view_palette_overlay",
        "tui_core_view_issue_browser", "tui_core_view_landing_decision",
        "tui_core_view_composer", "tui_core_modal_collision_escape",
        "tui_core_input_paste_as_atom", "tui_core_notification_in_tui_drain",

        "cli_core_command_init", "cli_core_command_agents",
        "cli_core_command_sessions", "cli_core_command_run",
        "cli_core_command_exec", "cli_core_command_tui",
        "cli_core_command_cost", "cli_core_command_connect",
        "cli_core_command_version",
        "cli_core_command_upgrade_trial",
        "cli_core_command_upgrade_pro",
        "cli_core_command_license_activate",

        "pm_core_beads_basic", "pm_core_pm_read",
        "pm_core_pr_manual", "pm_core_bv_adapter",

        "cost_core_basic_display", "cost_core_pricing_registry",
        "cost_core_ingestion_pipeline",

        "worktree_core_isolation", "worktree_core_artifact_resolver",

        "license_core_facade_entitlement", "license_core_policy_resolver",
        "license_core_ed25519_verify", "license_core_provider_heartbeat"
      ],
      "_wave7_dropped_from_above": "spur-blob-store backend trait-impl variants (3), spur-interactive shared host invariants (3), notif_core_in_tui (redundant) — see §4.16",
      "quotas": {
        "max_concurrent_workers": 2,
        "event_retention_mb": 128,
        "brain_failover_chain_depth": 1,
        "max_team_members": 1
      },
      "metadata": {
        "label": "Community",
        "tagline": "Solve rate-limit fragility — free, forever.",
        "description": "Daily-driver tier with cross-vendor orchestration, full TUI, beads PM, manual review, and one-keystroke rate-limit recovery."
      }
    },
    "pro": {
      "features": [
        "@inherit:community",

        "core_pro_worker_heartbeat_watchdog",
        "core_pro_review_auto_approve",
        "core_pro_review_timeout_routing",
        "core_pro_review_retry_config",
        "core_pro_peer_mailbox_router",

        "skills_pro_custom", "skills_pro_role_gating",

        "mcp_pro_plan_durable", "mcp_pro_reconciler_journal_notify",
        "mcp_pro_signal_watcher_scope_drift", "mcp_pro_mutation_executor",
        "mcp_pro_graph_tools", "mcp_pro_review",
        "mcp_pro_custom_tools",

        "tui_pro_telegram_bot_solo", "tui_pro_custom_keybindings",

        "pm_pro_beads_advanced", "pm_pro_github_auto",
        "pm_pro_linear_sync", "pm_pro_plane_sync",
        "pm_pro_signal_watcher", "pm_pro_auto_merge",

        "cost_pro_per_project_tracking", "cost_pro_sqlite_wal_mode",

        "ctx_pro_duckdb_engine", "ctx_pro_async_engine",
        "ctx_pro_live_mode", "ctx_pro_daily_report", "ctx_pro_weekly_report",

        "worktree_pro_git_blob_store", "worktree_pro_custom_policies",

        "bot_pro_runtime", "bot_pro_thread_registry",
        "bot_pro_runtime_render", "bot_pro_callback_validation",
        "bot_pro_inline_review",

        "license_pro_offline_grace",

        "blob_pro_namespace_deletion"
      ],
      "_wave7_dropped_from_pro": "blob_pro_measured_backend (always-on telemetry — see §4.16); blob_pro_delete_namespace renamed to blob_pro_namespace_deletion",
      "v1_1_q3_roadmap": [
        "core_pro_brain_failover_auto_pool",
        "core_pro_broadcast_lagged_recovery",
        "core_pro_session_resume_event_replay",
        "core_pro_peer_mailbox_ledger",
        "core_pro_peer_mailbox_stranded_recon",
        "tui_pro_trace_source_react",
        "cost_pro_budget_caps",
        "worktree_pro_cleanup_orphans",
        "license_pro_quota_runtime_downgrade"
      ],
      "_wave7_dropped_from_v1_1": "notif_pro_external_channels deferred to §4.16 v1.1 backlog (greenfield, no impl)",
      "quotas": {
        "max_concurrent_workers": 10,
        "event_retention_gb": 10,
        "brain_failover_chain_depth": 3,
        "max_team_members": 1
      },
      "metadata": {
        "label": "Pro",
        "tagline": "Ship from your couch — closed-loop today, autonomous tomorrow.",
        "description": "AFK confidence: auto-PR, Telegram remote-control, autonomous review, multi-session epics. Annual and lifetime are priced identically; lifetime includes all v1.x roadmap unlocks.",
        "pricing": {
          "monthly_usd": 12,
          "annual_usd": 99,
          "lifetime_usd": 99,
          "lifetime_scope": "v1.x",
          "trial_days": 7,
          "trial_opt_in_command": "spur upgrade trial",
          "trial_anti_abuse": "licenseseat NodeLocked binding (one trial per machine fingerprint)"
        }
      }
    },
    "team": {
      "deferred_to": "v2",
      "reserved_features": [
        "cli_team_command_workflow",
        "pm_team_webhooks",
        "bot_team_multi_chat"
      ],
      "metadata": {
        "label": "Team",
        "status": "deferred",
        "description": "Reserved for v2 — multi-user RBAC, shared lineage, audit logs, SSO, team-wide bot."
      }
    }
  },
  "flags": {
    "enable_telemetry": {
      "enabled": false,
      "description": "Opt-in anonymous usage stats."
    },
    "enable_v1_1_preview": {
      "enabled": true,
      "tier_filter": ["pro"],
      "subject_filter": ["lifetime", "annual"],
      "description": "Default-on for lifetime and annual subscribers (early-adopter reward). Monthly subscribers can opt-in via 'spur config set enable_v1_1_preview true'."
    }
  }
}
```

**Notes:**
- `@inherit:community` is a directive the policy resolver expands at load time — Pro is a strict superset of Community.
- `v1_1_q3_roadmap` is a separate field from `features`. Resolver MUST NOT activate roadmap features just because they're listed; they require code shipping AND the `enable_v1_1_preview` flag (during preview window) or default-on at v1.1 GA.
- Quotas use existing `MaxConcurrentWorkers`, `EventRetentionBytes` keys per arch.md §8 Risk #32.

---

## 6. Implementation: Gate Enforcement Points

Each tier-gated feature must be enforced at its first use-site. Per `2026-04-21-feature-gate-unified-resolver-design.md`, all checks go through `FeatureGate::require(FeatureKey::*)`.

### High-priority enforcement sites (v1 launch blockers)

| Feature key | File | Approximate line | Gate type |
|---|---|---|---|
| `core_core_parallel_workers` (cap 2/10) | `spur-core/src/orchestrator.rs` | ~3450 (semaphore construction) | Quota check |
| `pm_pro_github_auto` | `spur-pm/src/github_adapter.rs` | auto-PR-on-success path | Capability check |
| `pm_pro_linear_sync` | `spur-pm/src/service.rs` | bidirectional sync entry point | Capability check |
| `pm_pro_plane_sync` | `spur-pm/src/service.rs` | bidirectional sync entry point | Capability check |
| `tui_pro_telegram_bot_solo` | `spur-cli/src/main.rs` | `bot telegram` command dispatch | Capability check |
| `core_pro_review_auto_approve` | `spur-core/src/orchestrator.rs` | review-bypass / rule-based auto-approve path near `review_required` handling | Capability check |
| `core_pro_review_timeout_routing` | `spur-core/src/orchestrator.rs` | timeout `tokio::select!` arm (~line 4517) | Capability check on non-default timeout fallback/routing |
| `core_pro_review_retry_config` | `spur-core/src/orchestrator.rs` | `ReviewDecision::Retry` arm (~line 4654) — max attempts, backoff policy | Capability check on custom retry config |
| `mcp_pro_plan_durable` | `spur-mcp/src/server.rs:323` (active_plans insert) | `submit_plan(persist_as_epic=true)` | Capability check |
| `mcp_pro_signal_watcher_scope_drift` | `spur-mcp/src/signal_watcher.rs` | mutation proposal emit | Capability check |
| `mcp_pro_graph_tools` | `spur-mcp/src/server.rs` | each `graph_*` tool dispatch | Capability check |
| `mcp_pro_custom_tools` | `spur-mcp/src/server.rs` | external tool registration | Capability check |
| `core_pro_peer_mailbox_router` | `spur-core/src/orchestrator.rs:861` | `attach_peer_mailbox()` | Capability check |
| `pm_pro_beads_advanced` | `spur-pm/src/service.rs:200` | `advanced()` accessor | Capability check |
| `ctx_pro_duckdb_engine` | `spur-context/src/engine.rs` | `AnalyticsEngine::new` | Capability check |
| `worktree_pro_custom_policies` | `spur-worktree/src/manager.rs` | merge-strategy selection | Capability check |
| `worktree_pro_git_blob_store` | `spur-worktree/src/blob_store.rs` | `GitBlobOutcomeStore::new` | Capability check |
| `cost_pro_per_project_tracking` | `spur-cost/src/tracker.rs` | per-project aggregation | Capability check |
| `skills_pro_custom` | `spur-core/src/skills/registry.rs` | custom skill registration | Capability check |
| `core_core_brain_failover_manual_keystroke` | `spur-core/src/orchestrator.rs` | rate-limit detection handler | Behavior switch (Free → manual prompt; Pro → silent if `auto_pool` shipped) |

### Quota enforcement

| Quota key | Source | Enforcement site |
|---|---|---|
| `max_concurrent_workers` | PolicyDocument | `Orchestrator::new` semaphore size |
| `event_retention_mb` (Free 128MB) / `event_retention_gb` (Pro 10GB) | PolicyDocument | `EventSink` rotation threshold |
| `brain_failover_chain_depth` | PolicyDocument | failover chain length cap |
| `max_team_members` | PolicyDocument | (v2 enforcement; v1 Free=1, Pro=1) |

### Risk #32 mitigation

Currently feature-gates evaluate at startup (point-in-time snapshot). For v1, this is acceptable. For `license_pro_quota_runtime_downgrade` (v1.1), refactor to use-site evaluation via `arc_swap` reload. Existing infrastructure supports this.

---

## 6.2 Pro Trial via licenseseat (no new state machine)

The `spur upgrade trial` command issues a regular Pro license with a 7-day expiry using EXISTING licenseseat infrastructure. No new license state machine, no new tier, no new feature key family.

### Mechanism (uses existing primitives)

| Primitive | Source | Purpose |
|---|---|---|
| `LicenseState.expires_at: Option<DateTime<Utc>>` | `spur-license/src/lib.rs:105` | Already exists — time-bound license expiry |
| `LicenseStatus::Active → Expired` transition | `spur-license/src/licenseseat.rs` | Already wired via `spawn_sdk_event_bridge` |
| `BindingMode::NodeLocked` (machine fingerprint) | `spur-license/src/lib.rs:53` | Already exists — prevents trial farming |
| `CommunityProvider` fallback on expiry | `spur-license/src/community.rs` | Already wired — auto-downgrades Pro→Free at expiry |
| `broadcast(LicenseEvent)` → all subscribers | `spur-core` event bus | Existing — TUI badge updates in real-time |

### `spur upgrade trial` flow

1. User runs `spur upgrade trial` (gated by `cli_core_command_upgrade_trial` — Free)
2. CLI calls SPUR backend (or licenseseat self-issue API) with current machine fingerprint
3. Backend issues license: `plan=pro, expires_at=now+7d, binding=NodeLocked, subject_kind=User`
4. License key cached locally; `arc_swap` reload makes Pro features immediately available
5. TUI shows persistent badge: "Pro trial — N days remaining" (countdown)
6. At `expires_at`, licenseseat fires `EventKind::Expired` → `CommunityProvider` takes over
7. TUI shows: "Trial ended — upgrade to keep Pro features [spur upgrade pro]"

### Anti-abuse

- **Machine binding:** licenseseat's `NodeLocked` binding fingerprints the machine. Same machine cannot claim a second trial.
- **Backend dedup:** SPUR backend rejects duplicate machine fingerprints across all trial requests.
- **No re-trial after expiry:** License reactivation requires paid Pro purchase; trial credit is one-time per machine.
- **No CI bypass:** `subject_kind=Ci` and `binding_mode=FloatingCi` not allowed for trial issuance (forces real machine binding).

### v1 Pro feature exposure during trial

All Pro v1 features active (the user experiences the full Pro bundle for 7 days). v1.1-Q3 roadmap features are NOT exposed during trial — they don't exist yet, and the trial period will end before v1.1 ships.

### Implementation effort

- ~1 day backend (issue trial license endpoint + dedup logic)
- ~1 day CLI command (`spur upgrade trial` + UX wiring)
- ~0.5 day TUI badge (countdown + expiry banner)
- ~0.5 day testing
- **Total: 2-3 engineer-days, blocking nothing else**

---

## 7. Tier-Blocking Risk Dependencies

Features marked `[v1.1-Q3]` in §4 cannot ship until specific architecture risks are closed. Dependencies:

| v1.1 Feature | Blocked by Risk # | Risk description | Effort estimate |
|---|---|---|---|
| `core_pro_brain_failover_auto_pool` | #8 | "Brain failover exists but is best-effort; no auto-respawn" | 2-3 weeks |
| `core_pro_session_resume_event_replay` | #9 | "EventSink writes NDJSON but no code reads it back for replay" | 2 weeks |
| `core_pro_broadcast_lagged_recovery` | #2, #9 | Lagged subscribers permanently drop events | 1 week (after #9) |
| `cost_pro_budget_caps` | #17 | "spur-cost is purely observational... never compares to a limit" | 1-2 weeks |
| `core_pro_peer_mailbox_ledger` (GA flip) | #22 | "Peer mailbox unbounded ledger" | 1 week |
| `core_pro_peer_mailbox_stranded_recon` (GA flip) | #22 (transitive) | Same root cause | (bundled with #22) |
| `worktree_pro_cleanup_orphans` | #4 | "cleanup_orphans() exists but has zero call sites and is unsafe" | 2 weeks (refactor required) |
| `license_pro_quota_runtime_downgrade` | #32 | "Feature gates evaluated once at startup" | 1 week |
| `notif_pro_external_channels` | (greenfield) | Deferred to §4.16 v1.1 backlog (Wave 7); no Slack/Discord/webhook code exists | 4-6 weeks |
| `tui_pro_trace_source_react` | (placeholder) | `TraceSource` exists but not wired into palette | 1 week |

**Total v1.1-Q3 effort estimate: ~14-20 engineer-weeks.** Q3 2026 (target month: July 2026 internal, August buffer) is achievable from May 1 start.

---

## 8. Migration from Existing tier-plan.md

`docs/architecture-tier-plan.md` becomes legacy. Replacement strategy:

### 8.1 Files to delete
- None (preserve `architecture-tier-plan.md` as historical record; this spec supersedes individual-tier portions)

### 8.2 Feature key renames (existing → new)

| Old key | New key | Reason |
|---|---|---|
| `brain_session` | `core_core_brain_session` | Naming convention |
| `single_worker` | (removed; subsumed by `core_core_parallel_workers` with quota=2) | Quota-only distinction |
| `parallel_workers` | `core_core_parallel_workers` | Naming convention |
| `event_persistence` | `core_core_event_sink_ndjson_128mb` | Atomic + correct size |
| `extended_retention` | (removed; subsumed by quota lift) | Quota-only |
| `session_resume` | `core_core_session_resume` (Free) + `core_pro_session_resume_event_replay` (Pro v1.1) | Split |
| `manual_review` | (folded into `core_core_review_sink` per gemini gate-review 2026-04-26) | Subsumed |
| `auto_review_policies` | `core_pro_review_policy_auto_approve` + custom `timeout_fallback`/`retry_backoff` (basics moved to Free) | Split + re-tiered |
| `tui_dashboard` | `tui_core_view_dashboard` | Naming convention |
| `tui_session_detail` | `tui_core_view_session_detail` (now Free) | Re-tiered |
| `basic_lineage` | `core_core_executor_lineage_projection` | Naming |
| `shared_lineage` | (deferred to Team v2) | Out of scope |
| `worktree_isolation` | `worktree_core_isolation` | Naming |
| `custom_worktree_policies` | `worktree_pro_custom_policies` | Naming |
| `basic_cost_display` | `cost_core_basic_display` | Naming |
| `advanced_cost_analytics` | `cost_pro_per_project_tracking` + `ctx_pro_*` | Split (DuckDB → ctx) |
| `pm_integration` | (split into 9 keys per §4.6) | Major decomposition |
| `mcp_standard_tools` | `mcp_core_*` (5 keys) | Split |
| `custom_mcp_tools` | `mcp_pro_custom_tools` | Naming |
| `basic_notifications` | (subsumed by `core_core_notification_pump` + `tui_core_notification_drain`; Wave 7 dropped `notif_core_in_tui` as redundant) | Subsumed |
| `custom_notifications` | `notif_pro_external_channels` (deferred to §4.16 v1.1 backlog) | Vaporware → backlog |
| `local_config` | `cli_core_command_*` (subsumed) | Removed |

### 8.3 New keys (no equivalent in old plan)

All `acp_core_adapter_*` (7), all `core_core_skill_*` + `skills_core_render_per_vendor`, all `core_core_event_*`, `core_core_continuation_bridge`, `core_core_notification_pump`, all `bot_pro_*` (3), all `ctx_pro_*` (3), `blob_pro_namespace_deletion` (1), all `license_pro_*` (2), `acp_core_session_attach_*` (2), `tui_core_modal_collision_escape`, `tui_core_input_paste_as_atom`, etc. (Wave 7 dropped all 3 `interactive_core_*`, all 3 `blob_core_*` backend variants, `blob_pro_measured_backend`, and the entire `notif_*` namespace per §4.16.)

### 8.4 Re-tiered features (key changes)

- `pm_integration` (Team-only in old) → `pm_pro_*` family (Pro). **User-requested correction.**
- `peer_mailbox` (not in old plan) → `core_pro_peer_mailbox_*` (Pro after Risk #22). **User-requested confirmation.**
- `basic_session_resume` (would have moved to Pro per gemini's "ephemeral/durable" framing) → kept Free. **User correction.**
- `worktree_isolation` (gemini wanted Pro) → kept Free. **Safety axis decision.**
- `brain_failover` (kimi wanted full Pro) → split into manual (Free) + auto (Pro v1.1-Q3). **MCTS resolution.**

### 8.5 Implementation checklist (sequenced)

**Phase 1: Registry & Policy (Week 1)**
- [ ] Replace `feature_key.rs` enum with 121 atomic keys per §4
- [ ] Group keys by crate prefix for grep-discoverability
- [ ] Add quota key constants
- [ ] Rewrite `default_policy.json` with §5 PolicyDocument
- [ ] Re-sign with `spur-policy-2026-04` Ed25519 key
- [ ] Verify `build.rs` compile-time check passes
- [ ] Add `@inherit:community` resolver expansion

**Phase 2: Gate Enforcement (Weeks 2-3)**
- [ ] Add `FeatureGate::require()` calls at each enforcement site per §6
- [ ] Wire `pm_pro_github_auto` gate into `GitHubAdapter` auto-PR path
- [ ] Wire `pm_pro_linear_sync` + `pm_pro_plane_sync` gates
- [ ] Wire `tui_pro_telegram_bot_solo` gate at CLI dispatch
- [ ] Wire `mcp_pro_*` gates into MCP server tool dispatch
- [ ] Wire `core_pro_review_policy_*` gates in ReviewSink
- [ ] Wire `core_pro_peer_mailbox_*` gates (default off until Risk #22)
- [ ] Wire `pm_pro_beads_advanced` gate in service.rs:200
- [ ] Wire `ctx_pro_*` gates in spur-context
- [ ] Wire `worktree_pro_*` gates in WorktreeManager

**Phase 3: Quota Enforcement (Week 3)**
- [ ] `Orchestrator::new` semaphore size from `max_concurrent_workers`
- [ ] `EventSink` rotation from `event_retention_mb`/`event_retention_gb`
- [ ] Brain failover chain depth from `brain_failover_chain_depth`

**Phase 4: UI Affordances (Week 4)**
- [ ] TUI badges for Pro features ([Pro] tag)
- [ ] Capability-tease modals when Free user attempts Pro action
- [ ] `core_core_brain_failover_manual_keystroke` banner UX
- [ ] Pro upgrade flow: `spur upgrade pro` → browser → license activation

**Phase 5: Marketing & Docs (Week 4)**
- [ ] Rewrite `community-tier.md` with new feature list
- [ ] Write `pro-tier.md` with conversion triggers + roadmap
- [ ] Comparison table (§9 of this spec)
- [ ] Roadmap page on website with Q3 dates
- [ ] Pricing page: $12/mo, $99 lifetime (v1.x bound)

**Phase 6: v1.1 Roadmap Track (Months 2-4 in parallel)**
- [ ] Risk #8 fix → `core_pro_brain_failover_auto_pool`
- [ ] Risk #9 fix → `core_pro_session_resume_event_replay` + `core_pro_broadcast_lagged_recovery`
- [ ] Risk #17 fix → `cost_pro_budget_caps`
- [ ] Risk #22 fix → flip `core_pro_peer_mailbox_*` to GA
- [ ] Risk #4 fix → `worktree_pro_cleanup_orphans`
- [ ] Risk #32 fix → `license_pro_quota_runtime_downgrade`
- [ ] Greenfield → `notif_pro_external_channels` (Wave 7 deferred to §4.16; lands when Slack/Discord/email/webhook subsystem exists)
- [ ] Wiring → `tui_pro_trace_source_react`

---

## 9. Marketing Copy

### 9.1 Comparison Table (for website pricing page)

| Capability | Free | Pro |
|---|---|---|
| **Cross-vendor orchestration** | 4 ACP agents (Claude/Kiro/Codex/Gemini) — Wave-8 dropped 3 ghost adapters | Same |
| **Parallel workers** | 2 concurrent | 10 concurrent |
| **Rate-limit recovery** | One-keystroke manual switch | Silent automatic *(deferred to v1.1 — Risk #8)* |
| **Worktree isolation + orphan cleanup** | ✓ | ✓ |
| **Review gate (sink+timeout+retry)** | ✓ (with retry-limit config — Wave 9 shift) | Same + auto-approve policy |
| **Auto-approve on review timeout** | ✗ | ✓ ★ |
| **MCP graph diagnostics** | ✓ (raw JSON / Mermaid text — Wave 9 shift) | Same |
| **Closed-loop PR creation** | Manual | **Automatic on success via reconciler** ★ |
| **Telegram remote-control** | ✗ | **Single-operator bot + inline review** ★ |
| **Multi-session durable plans** | Single-plan ephemeral | **Durable, cross-session + signal watcher** ★ |
| **Peer-mailbox multi-agent routing** | ✗ | ✓ *(default-off; opt-in)* |
| **Custom skills / role gating** | Bundled skills only | **Custom skill overrides** ★ |
| **DuckDB analytics engine** | ✗ | ✓ *(experimental flag today)* |
| **Per-project cost tracking** | Per-session only | All-time per-project rollup *(billing-export shipping later)* |
| **Budget caps** | Soft warning | **Hard caps** *(deferred to v1.1 — Risk #17)* |
| **Session resume** | Process-restart re-attach | **Full event-log replay** *(v1.1)* |
| **External notifications** | (TUI/bot path; deferred external channels v1.1) | **Slack/Discord/webhook** *(deferred to v1.1)* |
| **Bundled skills** | All bundled + atomic-installation + render-per-vendor (Wave-8 umbrella) | Same + custom org skills + role gating |
| **License resilience** | n/a | Revocation polling + offline grace period |
| **Monthly subscription** | n/a | $12/mo |
| **Annual subscription** | n/a | $99/yr (save $45) |
| **Lifetime license** | n/a | $99 (v1.x, includes Q3 roadmap) |
| **Free Pro trial** | n/a | 7 days, opt-in via `spur upgrade trial` |

★ = headline upgrade trigger
*(coming Q3)* = v1.1-Q3 roadmap, free for existing Pro subscribers

### 9.2 Free tier copy (community-tier.md headline section)

> **SPUR Free is the daily-driver tier — not a trial.**
>
> Solve the #1 problem with AI coding agents (rate-limit fragility) without paying anything. Switch between Claude, Kiro, Codex, Gemini, Cursor, OpenCode, and Kimi with one keystroke. Run 2 workers in parallel. Browse your Linear/GitHub issues. Create PRs manually when you're ready. Worktree-isolated, locally-orchestrated, signed-policy free tier with no expiration date.
>
> When you want SPUR to *do this for you* — auto-create PRs, run while you're at lunch, take phone approvals via Telegram — that's Pro.

### 9.3 Pro tier copy (pro-tier.md headline section)

> **SPUR Pro: $12/mo, $99/yr, or $99 lifetime.**
> *Ship from your couch — closed-loop today, autonomous tomorrow.*
>
> Try it free for 7 days: `spur upgrade trial` — no card required, one trial per machine.
>
> **What you get today (v1):**
> - Auto-create GitHub PRs the moment delegation succeeds
> - Bidirectional Linear and Plane sync (status, comments, agent logs)
> - Telegram remote-control: approve/reject/modify from your phone
> - Auto-approve policies: SPUR runs while you're AFK
> - Multi-session durable plans: epics survive restarts and span days
> - 10 parallel workers, 10 GB event retention, 3-deep failover chain
> - DuckDB analytics: daily/weekly cost reports
> - Custom MCP tools, custom skills, custom worktree policies
> - All depth features: peer mailbox, signal watcher, graph tools, advanced review
>
> **Coming Q3 2026 (free for existing subscribers):**
> - Silent automatic brain failover (no [y/n] prompts)
> - Full session resume from event-log replay
> - Hard budget caps with circuit breakers
> - Slack / Discord / webhook notifications
>
> **Lifetime license is bound to v1.x.** When v2 ships, your v1.x lifetime continues to receive security and reliability updates indefinitely. v2 will have separate pricing.

### 9.4 Roadmap page (website)

> **SPUR v1.1 — Target: Q3 2026**
>
> Pro subscribers get these unlocks free as they ship:
> - **Silent multi-brain failover** (Risk #8 closure)
> - **Session replay from event log** (Risk #9 closure)
> - **Budget caps with circuit breakers** (Risk #17 closure)
> - **External notification channels** (Slack/Discord/webhook)
> - **Peer mailbox GA** (Risk #22 closure — already opt-in available)
> - **Worktree orphan cleanup** (Risk #4 closure)
> - **Mid-session quota enforcement** (Risk #32 closure)
> - **TraceSource palette wiring**
>
> Status: 14-20 engineer-weeks remaining. Internal target: July 2026. External commitment: Q3 2026 (August buffer).

### 9.5 Iceberg framework analysis (Wave 9 dual-reviewer synthesis)

The Free vs Pro stack is best evaluated through the Iceberg framework: features have visible above-water value (drives acquisition + conversion) AND invisible below-water value (drives retention + LTV). Wave 9 (gemini strategy + codex code-grounded dual review with L9-MCTS judge synthesis) produced this calibrated view.

**Free above-water (acquisition iceberg, ~9 ★ hooks):**
1. Cross-vendor orchestration (4 implemented adapters: Claude / Kiro / Codex / Gemini)
2. Full TUI dashboard + 5 functional views (dashboard, session_detail, plan_inspector, palette_overlay, issue_browser)
3. One-keystroke manual rate-limit recovery
4. Plan persistence + session resume + review with retry config
5. Beads PM (basic + browse + PR + graph_adapter + MCP graph diagnostics)
6. Per-session cost display + pricing registry
7. Multi-worker parallel (2 concurrent quota)
8. 9 CLI commands (init, agents, sessions, run, exec, tui, cost, connect, license_activate)
9. Worktree isolation + orphan cleanup

**Free below-water (retention/lock-in iceberg, NOT advertised):**
- Beads PM data lock-in (`.beads/` directory; switching cost compounds with usage)
- Skills muscle memory (custom prompts learned via skills_core_registry)
- TUI keybinding sunk cost
- Plan + session history locked in local SQLite
- Per-token cost transparency trust (Free users see full pricing detail; Pro adds aggregation only)

**Pro above-water (conversion triggers, 5+ ★ headlines):**
1. ★ **Remote Control** — telegram_solo + inline_review (production code; AFK ship-from-couch)
2. ★ **Multi-Agent Coordination** — peer_mailbox_router + signal_watcher_scope_drift + plan_durable (router default-off + in-memory today; durable plans production-ready)
3. ★ **Review Control Plane** — auto_approve (timeout fallback / permission fast-path; NOT autonomous review judgement) + worker_heartbeat_watchdog (default-off until heartbeat emitters ship) + mcp_pro_review (manual approve/reject control surface)
4. ★ **Cost Insights** — per_project_tracking (all-time rollup; billing-export shipping later) + duckdb_engine (experimental flag; CLI by-agent today)
5. ★ **Extensibility** — skills_pro_custom + pm_pro_beads_advanced

**Pro below-water (LTV anchors):**
- Lifetime $99 = "ownership flip" psychology
- v1.x roadmap commitment (lifetime users get all v1.1-Q3 features as they ship)
- Annual = lifetime parity ($99) signals "this product will keep growing"
- Sunk cost from extended Free → Pro psychological bridge

**Persona model (4 archetypes; B2D-realistic conversion baseline 2-5%):**

| # | Persona | TAM share | Primary trigger | 90-day conversion baseline |
|---|---|---|---|---|
| **P1** | Solo Indie Developer | ~50% | telegram_solo (AFK trigger) | 2-4% |
| **P2** | Agency / Multi-Client Freelancer | ~15% | per_project_tracking + inline_review | 6-12% (real billing pain shortens window) |
| **P4** | Senior Engineer @ BigCo (personal license) | ~15% | mcp_pro_review (control surface) + lifetime $99 | 2-3%/yr (slow but high LTV via lifetime) |
| **P5** | ML / AI Researcher (cost-obsessed) | ~5% | per_project_tracking + duckdb_engine | 5-8% |

*Persona P3 ("Team 2-dev") was removed in Wave 9 dual-review synthesis: it conflated `peer_mailbox_router` (inter-AGENT message routing — see `crates/spur-acp/src/config/mod.rs:372-375`) with inter-HUMAN team collaboration. SPUR has no human team UX in v1; multi-user RBAC is v2. Team-tier pricing is therefore deferred to v2 per spec §4.16 (`bot_team_multi_chat`).*

**Wave 9 strategic decisions (dual-reviewer convergence):**

1. **2 surgical tier-shifts Pro→Free** (per Wave 9 §4.16 entries):
   - `mcp_pro_graph_tools` → `mcp_core_graph_tools` (acquisition surface)
   - `core_pro_review_retry_config` → `core_core_review_retry_config` (Free reliability baseline)

2. **Marketing groupings (5 Pro headline categories above)** — codex grounded each name in actual code reality. NOT in Wave 9 marketing copy: any phrase implying autonomous-judgement review (mcp_pro_review is manual control plane), Twitter-shareable visual (graph_tools is text output), or team collaboration (peer_mailbox is inter-agent routing).

3. **Below-water amplifiers explicitly DEFERRED to Plan D/E** (not Wave 9 scope):
   - 7-day Pro trial (per spec §6.2; CLI auth has no trial mechanism today — `crates/spur-cli/src/commands/auth.rs:15-40`)
   - Capability-tease modals (TUI has modal primitive at `crates/spur-tui/src/components/collision_modal.rs:11-99` but no locked-Pro tease pattern)
   - Skills marketplace publish/discover (fully greenfield; only local bundled+overrides today)

4. **Pricing held at $12/mo + $99 annual + $99 lifetime.** Team tier pricing deferred to v2.

**Risk acknowledgements (codex top-3):**
- *Registry tier shifts ≠ runtime enforcement.* Wave-9 const renames update the registry only; actual `FeatureGate::has` use is sparse today (`crates/spur-license/src/gate.rs:40-66`). Plan B is the policy/enforcement work.
- *Persona copy must match implementation completeness.* Avoid marketing surfaces that are engine-only or control-plane-only (cost billing export, autonomous review judgement, DuckDB project reports — all need narrowing).
- *Trial + tease modals + marketplace are not ready as conversion amplifiers.* Plan D scope.

---

## 10. Success Metrics

### 10.1 Conversion targets

| Metric | Day 30 | Month 3 | Year 1 |
|---|---|---|---|
| Free → Pro conversion | 5% | 8% | 12% |
| Lifetime / Monthly mix | 30/70 | 40/60 | 50/50 |
| Pro Day-7 retention | 90% | 92% | 94% |
| Pro Month-3 retention | 75% | 80% | 82% |

### 10.2 Trigger thresholds (auto-tightening logic)

If Free → Pro conversion is **below 3% at Month 3**, tighten Free quotas:
- `max_concurrent_workers`: 2 → 1
- `event_retention_mb`: 128 → 64
- `brain_failover_chain_depth`: 1 → 0 (manual recovery requires explicit user action)

If Free → Pro conversion is **above 15% at Month 3**, consider loosening:
- `max_concurrent_workers`: 2 → 3 (still below Pro's 10)
- This signals Pro pull is too strong; we may be losing potential Free amplifiers

### 10.3 Health metrics (weekly review)

- Pro upgrade attempt funnel: where users drop off in the upgrade flow
- v1.1 roadmap velocity: weekly progress on Risk #8/9/17/22/32 closures
- NPS for Pro subscribers (target > 50 by Month 3)
- Refund rate (target < 5% lifetime, < 8% monthly)
- Top 3 reasons for Pro cancellation (segment by lifetime vs monthly)

### 10.4 Anti-metrics (do NOT optimize for)

- Total Pro feature count (depth ≠ value if features go unused)
- Marketing site traffic (vanity)
- GitHub stars (vanity unless tied to install conversion)

---

## 11. Open Decisions Logged

| Decision | Resolved | Rationale |
|---|---|---|
| spur-pm in Pro tier? | Yes (split into core/pro per §4.6) | User request + first-principles closed-loop value |
| basic_session_resume in Free? | Yes | Reliability table stakes (user correction) |
| peer_mailbox in Pro? | Yes (after Risk #22) | User confirmation |
| All 7 vendor adapters Free? | Yes | Cross-vendor moat preservation |
| All skills Free except custom? | Yes | Crippling skills cripples product |
| 7-day Pro trial? | No | Kimi's churn argument; replaced by capability-tease affordances |
| Lifetime price? | $99 (revised from $129) | Fewer "shipped today" headline features warrants accessible entry |
| Pro Annual middle tier? | $99/yr (priced same as lifetime) | Recurring billing for accounting; lifetime for one-and-done |
| `enable_v1_1_preview` default-on? | Yes for lifetime + annual subscribers | Early-adopter reward; monthly users opt-in |
| 7-day Pro trial via `spur upgrade trial`? | Yes (opt-in only, NOT auto-baked) | Uses existing licenseseat `expires_at` infrastructure; NodeLocked binding prevents farming |
| Vapor features? | Listed as v1.1-Q3 roadmap with date buffer; free for existing Pro | Industry-standard playbook (Stripe/Vercel/Snowflake) |
| Worktree isolation in Free? | Yes | Safety axis (rejected gemini's revenue framing) |
| `parallel_workers` Free cap? | 2 | Splits gemini (2) and kimi (1); demonstrates orchestration without unlimited |
| `event_persistence` Free? | 128 MB (corrected from 256 MB) | Matches arch.md actual rotation size |

### Resolved (this session)

| Decision | Resolution |
|---|---|
| Pro Annual middle tier? | **Yes — $99/yr** (priced identically to lifetime; positioning is "recurring billing for accounting" vs "one-and-done") |
| `enable_v1_1_preview` default-on for lifetime? | **Yes** — extended to lifetime AND annual subscribers as early-adopter reward; monthly subscribers can opt-in via config |
| 7-day Pro trial via `spur upgrade trial`? | **Yes — opt-in only (NOT auto-baked)**; uses existing licenseseat `expires_at` infrastructure with NodeLocked binding for anti-abuse |

---

## 12. Glossary

| Term | Definition |
|---|---|
| **Atomic feature key** | Single capability gated by a single `FeatureGate::require()` call |
| **Capability gate** | Tier check based on whether a feature is available (binary) |
| **Quota gate** | Tier check based on a numeric limit (e.g., max_concurrent_workers) |
| **PolicyDocument** | Signed JSON document mapping tiers → feature keys + quotas |
| **★ headline trigger** | A Pro feature that is a primary purchase intent driver |
| **v1.1-Q3** | Roadmap status: shipped in v1.1 release, target Q3 2026 |
| **Capability tease** | Visible-but-locked Pro feature affordance in TUI (replaces time-trial) |
| **Lifetime scope** | Lifetime license is bound to a major version (v1.x); v2 will reprice |
| **MCTS** | Monte Carlo Tree Search; used in this design for tier decision rollouts |

---

*End of spec. Implementation plan to be authored separately via `superpowers:writing-plans` skill after user review.*

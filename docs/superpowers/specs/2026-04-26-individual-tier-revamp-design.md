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
**Revised 2026-04-26 per gemini gate-review findings.** Original draft over-gated baseline safety/UX behaviors. Manual review is intrinsic to `review_sink`; basic timeout fallback (auto-cancel) and basic retry (press 'R') are universal liveness features. Only configurable customization (rule-based auto-approve, custom fallback routing, custom retry policy with backoff) is Pro.

| Key | Tier | Status | Description |
|---|---|---|---|
| `core_core_review_sink` | F | v1 | Pipeline routing review cards to frontends; includes built-in manual Approve/Reject/Modify resolution (formerly separate `review_policy_manual` — folded in) |
| `core_core_review_policy_timeout_fallback_basic` | F | v1 | Auto-cancel on review timeout (liveness baseline; prevents indefinite hangs) |
| `core_core_review_policy_retry_basic` | F | v1 | Press 'R' to retry a failed review (UX baseline; non-deterministic agent behavior) |
| `core_pro_review_policy_auto_approve` | P | v1 | Configurable auto-approve rules (path globs, change-size limits) |
| `core_pro_review_policy_timeout_fallback_custom` | P | v1 | Configurable `FallbackAction` routing on timeout (Slack hand-off, alt agent, etc.) |
| `core_pro_review_policy_retry_backoff` | P | v1 | `RetryRequested` resolution with configurable exponential backoff + max-attempts policy (Risk #5 closure) |

#### Skills system
| Key | Tier | Status | Description |
|---|---|---|---|
| `core_core_skill_registry` | F | v1 | Context-aware loading of bundled skills |
| `core_core_skill_atomic_installation` | F | v1 | SPUR-MANAGED marker + SHA-256 integrity (Risk #16 mitigation) |
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
| Key | Tier | Status | Description |
|---|---|---|---|
| `core_core_conflict_detection` | F | v1 | `ConflictDetected` event emission + frontend surfacing |
| `core_core_rate_limit_detection` | F | v1 | `RateLimitDetected` event emission for ACP throttling |
| `core_core_license_event_broadcast` | F | v1 | `LicenseUpdated` subscription via `broadcast(LicenseEvent)` |
| `core_core_permission_request_prompt` | F | v1 | One-shot `PermissionRequest` modals from orchestrator |
| `core_core_ext_notification` | F | v1 | `AgentExtNotification` boundary injection (incl. `_spur/peer_message` routing) |

#### Reliability & lifecycle
| Key | Tier | Status | Description |
|---|---|---|---|
| `core_core_basic_session_resume` | F | v1 | Process-restart re-attach to live brain session |
| `core_pro_session_resume_event_replay` | P | **v1.1-Q3** | Full lineage rebuild from NDJSON replay (Risk #9 fix) |
| `core_core_basic_plan_persistence` | F | v1 | Single in-flight plan survives restart |
| `core_pro_plan_orphan_recovery` | P | v1 | `recover_persisted_plans()` startup orphan reclamation (Risk #13 partial) |
| `core_pro_background_task_tracker` | P | v1 | `JoinHandle` + abort-on-Drop tracking (Risk #6 mitigation) |

### 4.3 `spur-mcp` (Brain→SPUR bridge — 14 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `mcp_core_server_dispatch` | F | v1 | MCP server tool dispatch loop |
| `mcp_core_delegate_basic` | F | v1 | `delegate_to_worker`, `delegate_parallel`, `cancel_delegation`, `list_available_workers` |
| `mcp_core_outcome_fetch` | F | v1 | `fetch_outcome_artifact`, `get_task_diff` |
| `mcp_core_pm_basic` | F | v1 | `get_issue`, `list_issues`, `create_issue`, `update_issue` (acting on `pm_core_*`) |
| `mcp_core_pr_manual` | F | v1 | `create_pr` user-initiated via MCP tool |
| `mcp_core_plan_ephemeral` | F | v1 | `submit_plan` in-memory (any size, lost on restart) |
| `mcp_core_outcome_materializer` | F | v1 | Store→clip→build with truncation ladder (`MERGE_BUDGET = 8192B`) |
| `mcp_pro_plan_durable` | P | v1 | `submit_plan(persist_as_epic=true)`, `execute_epic`, multi-session |
| `mcp_pro_reconciler_journal_notify` | P | v1 | `monitor_journal_appends` 250ms wake mechanism (Risk #19 mitigated) |
| `mcp_pro_signal_watcher_scope_drift` | P | v1 | Autonomous drift detection + mutation proposals |
| `mcp_pro_mutation_executor` | P | v1 | Apply plan mutations (label ops, signal markers) |
| `mcp_pro_graph_tools` | P | v1 | `graph_plan`, `graph_subgraph`, `graph_alerts`, `graph_insights`, `graph_triage` |
| `mcp_pro_review_advanced` | P | v1 | `review_task` with auto-merge gating policies |
| `mcp_pro_custom_tools` | P | v1 | Register org-internal MCP tools |

### 4.4 `spur-tui` (Terminal interface — 13 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `tui_core_view_dashboard` | F | v1 | Global activity log, brain/worker status |
| `tui_core_view_session_detail` | F | v1 | Per-session event stream + notification drain |
| `tui_core_view_plan_inspector` | F | v1 | DAG visualization for in-flight plans |
| `tui_core_view_palette_overlay` | F | v1 | Ctrl+K command palette |
| `tui_core_view_issue_browser` | F | v1 | Browse Linear/GitHub/beads issues (read-only on Free) |
| `tui_core_view_landing_decision` | F | v1 | Boot routing (`--new`, `--session`, `--sessions`, `--force-attach`) |
| `tui_core_view_composer` | F | v1 | Multi-line input composer |
| `tui_core_modal_collision_escape` | F | v1 | `kill <pid>` escape hatch for rejected attaches |
| `tui_core_input_paste_as_atom` | F | v1 | LRU-50 multi-line paste atomic placeholders + `ProtectedRange` |
| `tui_core_notification_in_tui_drain` | F | v1 | 8-event-per-frame display buffer |
| `tui_pro_telegram_bot_solo` ★ | P | v1 | Single-operator Telegram remote-control (full bot subsystem — see §4.10) |
| `tui_pro_trace_source_react` | P | **v1.1** | `TraceSource`/`ReactTrace` palette wiring (placeholder exists) |
| `tui_pro_custom_keybindings` | P | v1 | User-configurable keybinding overlay |

### 4.5 `spur-cli` (Command surface — 10 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `cli_core_command_init` | F | v1 | `spur init` — workspace scaffolding |
| `cli_core_command_agents` | F | v1 | `spur agents` — vendor configuration & registry |
| `cli_core_command_sessions` | F | v1 | `spur sessions` — list active/historical |
| `cli_core_command_run` | F | v1 | `spur run "<task>"` — ad-hoc execution |
| `cli_core_command_exec` | F | v1 | `spur exec --agent <name>` — direct vendor execution |
| `cli_core_command_tui` | F | v1 | `spur tui` — open dashboard |
| `cli_core_command_cost` | F | v1 | `spur cost` — basic cost summary |
| `cli_core_command_connect` | F | v1 | `spur connect` — socket bindings |
| `cli_core_command_version` | F | v1 | `spur version` |
| `cli_core_command_upgrade_trial` | F | v1 | `spur upgrade trial` — opt-in 7-day Pro trial (anti-abuse via licenseseat NodeLocked binding) |
| `cli_core_command_upgrade_pro` | F | v1 | `spur upgrade pro` — opens browser to Stripe checkout (monthly/annual/lifetime) |
| `cli_core_command_license_activate` | F | v1 | `spur license activate <key>` — write license to `~/.spur/license`, arc_swap reload |
| `cli_team_command_workflow` | T | **v2** | `spur workflow run/validate <file>` (TOML workflow engine — Team only) |

### 4.6 `spur-pm` (Project Management — 9 keys + 1 Team-deferred)

| Key | Tier | Status | Description |
|---|---|---|---|
| `pm_core_beads_basic` | F | v1 | Standard beads CRUD via `br` CLI |
| `pm_core_pm_read` | F | v1 | Browse Linear/GitHub/Plane/beads (read-only, no risk) |
| `pm_core_pr_manual` | F | v1 | User-initiated `create_pr` via `gh` CLI |
| `pm_core_bv_adapter` | F | v1 | bv adapter integration |
| `pm_pro_beads_advanced` | P | v1 | Plan persistence + projection + mutation execution + signal-watch + auto-merge |
| `pm_pro_github_auto` ★ | P | v1 | Auto-create PR on delegation success |
| `pm_pro_linear_sync` ★ | P | v1 | Bidirectional Linear status sync (comments, agent activity log, status mapping) |
| `pm_pro_plane_sync` | P | v1 | Same for Plane (REST + MCP) |
| `pm_pro_signal_watcher` | P | v1 | Beads-advanced signal proposal + scope drift detection |
| `pm_pro_auto_merge` | P | v1 | Beads-advanced auto-merge gating |
| `pm_team_webhooks` | T | **v2** | Bidirectional webhook receivers |

### 4.7 `spur-cost` (Cost tracking — 6 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `cost_core_basic_display` | F | v1 | Per-session running cost in TUI + soft daily warning |
| `cost_core_pricing_registry` | F | v1 | Model pricing table |
| `cost_core_ingestion_pipeline` | F | v1 | `TokenEvent` ingestion from ACP notifications |
| `cost_pro_per_project_tracking` | P | v1 | Per-project aggregation (separate from per-session) |
| `cost_pro_sqlite_wal_mode` | P | v1 | WAL mode + `busy_timeout` for concurrent reader/writer (Risk #29 mitigation) |
| `cost_pro_budget_caps` | P | **v1.1-Q3** | Hard budget enforcement at spawn-time + runtime (Risk #17 fix) |

### 4.8 `spur-context` (DuckDB analytics — 5 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `ctx_pro_duckdb_engine` | P | v1 | `AnalyticsEngine` in-memory DuckDB |
| `ctx_pro_async_engine` | P | v1 | `AsyncAnalyticsEngine` `spawn_blocking` wrapper (Risk #30 hardening pending) |
| `ctx_pro_live_mode` | P | v1 | Real-time analytics |
| `ctx_pro_daily_report` | P | v1 | Day-bucketed cost/time reports |
| `ctx_pro_weekly_report` | P | v1 | Week-bucketed cost/time reports |

### 4.9 `spur-worktree` (Git isolation — 5 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `worktree_core_isolation` | F | v1 | Auto-create/cleanup `git worktree` per delegation (safety axis — kept Free) |
| `worktree_core_artifact_resolver` | F | v1 | Delegation result artifact lookup |
| `worktree_pro_git_blob_store` | P | v1 | `GitBlobOutcomeStore` production backend (`refs/spur/outcomes/...`) |
| `worktree_pro_custom_policies` | P | v1 | Merge strategies (squash/rebase/octopus), naming templates |
| `worktree_pro_cleanup_orphans` | P | **v1.1** | Safe global orphan cleanup (Risk #4 fix required) |

### 4.10 `spur-bot` (Telegram subsystem — 5 keys + 1 Team-deferred)

`tui_pro_telegram_bot_solo` ★ (in §4.4) is the user-facing bundle. Underneath it gates these atomic capabilities:

| Key | Tier | Status | Description |
|---|---|---|---|
| `bot_pro_runtime` | P | v1 | `BotRuntime` long-poll connection |
| `bot_pro_thread_registry` | P | v1 | Forum-topic-per-session multiplexing |
| `bot_pro_runtime_render` | P | v1 | `SpurEvent` → Markdown state machine |
| `bot_pro_callback_validation` | P | v1 | `live_session` callback expiry validation (Risk #12 closure) |
| `bot_pro_inline_review` | P | v1 | Inline keyboard review buttons (approve/reject/modify/retry) |
| `bot_team_multi_chat` | T | **v2** | Team-wide multi-chat with RBAC |

### 4.11 `spur-license` (Entitlements — 6 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `license_core_facade_entitlement` | F | v1 | Point-in-time `FeatureGate` + `arc_swap` entitlement check |
| `license_core_policy_resolver` | F | v1 | Signed PolicyDocument parsing |
| `license_core_ed25519_verify` | F | v1 | Ed25519 signature verification |
| `license_core_provider_heartbeat` | F | v1 | Background polling for revocations |
| `license_pro_offline_grace` | P | v1 | Configurable offline grace period during network failure (Risk #31 mitigation) |
| `license_pro_quota_runtime_downgrade` | P | **v1.1** | Mid-session downgrade enforcement (Risk #32 fix) |

### 4.12 `spur-blob-store` (Outcome storage — 4 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `blob_core_memory_backend` | F | v1 | `MemoryOutcomeStore` (tests) |
| `blob_core_fs_backend` | F | v1 | `FsOutcomeStore` local filesystem |
| `blob_pro_measured_backend` | P | v1 | `MeasuredOutcomeStore` instrumentation wrapper |
| `blob_pro_delete_namespace` | P | v1 | `DeleteNamespaceReport` bulk cleanup |

### 4.13 `spur-interactive` (Shared host — 3 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `interactive_core_frontend_host` | F | v1 | Unified TUI/bot trait host |
| `interactive_core_review_lane_mpsc` | F | v1 | Dedicated review lane preventing head-of-line blocking |
| `interactive_core_shutdown_orchestrator` | F | v1 | Multi-subsystem abort propagation |

### 4.14 Notifications (cross-crate, kept here for clarity — 2 keys)

| Key | Tier | Status | Description |
|---|---|---|---|
| `notif_core_in_tui` | F | v1 | In-TUI notification pump (existing) |
| `notif_pro_external_channels` | P | **v1.1-Q3** | Slack/Discord/email/webhook routing (greenfield — does not exist today) |

---

### 4.15 Registry summary

Note on grouping: `skills_*` prefix keys live in `spur-core` code (under `spur-core/src/skills/`) but are listed in their own row below for grep-discoverability; they're counted in the Skills row, not double-counted under spur-core.

| Crate / Subsystem | Free keys | Pro keys (v1) | Pro keys (v1.1) | Team-deferred |
|---|---|---|---|---|
| `spur-acp` | 11 | 0 | 0 | 0 |
| `spur-core` | 19 | 7 | 5 | 0 |
| `skills_*` (in spur-core) | 3 | 2 | 0 | 0 |
| `spur-mcp` | 7 | 7 | 0 | 0 |
| `spur-tui` | 10 | 2 | 1 | 0 |
| `spur-cli` | 12 | 0 | 0 | 1 |
| `spur-pm` | 4 | 6 | 0 | 1 |
| `spur-cost` | 3 | 2 | 1 | 0 |
| `spur-context` | 0 | 5 | 0 | 0 |
| `spur-worktree` | 2 | 2 | 1 | 0 |
| `spur-bot` | 0 | 5 | 0 | 1 |
| `spur-license` | 4 | 1 | 1 | 0 |
| `spur-blob-store` | 2 | 2 | 0 | 0 |
| `spur-interactive` | 3 | 0 | 0 | 0 |
| Notifications (cross-crate) | 1 | 0 | 1 | 0 |
| **Total** | **81** | **41** | **10** | **3** |

**Total atomic feature keys: 135** (well above the ~110 target; matches the user's intuition that 45 was insufficient).

**Pro v1 launch arsenal: 41 features** organized into 5 ★ headline triggers + 36 supporting depth capabilities.

**Pro v1.1 roadmap: 10 features** (target Q3 2026 for the 8 risk-blocked items + greenfield notifications + TraceSource wiring; 2 peer-mailbox features ship as soon as Risk #22 ledger pruning lands, which may be earlier than Q3).

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
        "core_core_review_policy_timeout_fallback_basic",
        "core_core_review_policy_retry_basic",
        "core_core_skill_registry", "core_core_skill_atomic_installation",
        "skills_core_render_per_vendor",
        "core_core_conflict_detection", "core_core_rate_limit_detection",
        "core_core_license_event_broadcast",
        "core_core_permission_request_prompt", "core_core_ext_notification",
        "core_core_basic_session_resume", "core_core_basic_plan_persistence",

        "mcp_core_server_dispatch", "mcp_core_delegate_basic",
        "mcp_core_outcome_fetch", "mcp_core_pm_basic",
        "mcp_core_pr_manual", "mcp_core_plan_ephemeral",
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
        "license_core_ed25519_verify", "license_core_provider_heartbeat",

        "blob_core_memory_backend", "blob_core_fs_backend",

        "interactive_core_frontend_host",
        "interactive_core_review_lane_mpsc",
        "interactive_core_shutdown_orchestrator",

        "notif_core_in_tui"
      ],
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
        "core_pro_review_policy_auto_approve",
        "core_pro_review_policy_timeout_fallback_custom",
        "core_pro_review_policy_retry_backoff",
        "core_pro_peer_mailbox_router",
        "core_pro_plan_orphan_recovery",
        "core_pro_background_task_tracker",

        "skills_pro_custom", "skills_pro_role_gating",

        "mcp_pro_plan_durable", "mcp_pro_reconciler_journal_notify",
        "mcp_pro_signal_watcher_scope_drift", "mcp_pro_mutation_executor",
        "mcp_pro_graph_tools", "mcp_pro_review_advanced",
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

        "blob_pro_measured_backend", "blob_pro_delete_namespace"
      ],
      "v1_1_q3_roadmap": [
        "core_pro_brain_failover_auto_pool",
        "core_pro_broadcast_lagged_recovery",
        "core_pro_session_resume_event_replay",
        "core_pro_peer_mailbox_ledger",
        "core_pro_peer_mailbox_stranded_recon",
        "tui_pro_trace_source_react",
        "cost_pro_budget_caps",
        "worktree_pro_cleanup_orphans",
        "license_pro_quota_runtime_downgrade",
        "notif_pro_external_channels"
      ],
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
| `core_pro_review_policy_auto_approve` | `spur-core/src/review_sink.rs` | auto-approve resolution | Capability check |
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
| `notif_pro_external_channels` | (greenfield) | No Slack/Discord/webhook code exists | 4-6 weeks |
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
| `session_resume` | `core_core_basic_session_resume` (Free) + `core_pro_session_resume_event_replay` (Pro v1.1) | Split |
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
| `basic_notifications` | `notif_core_in_tui` | Naming |
| `custom_notifications` | `notif_pro_external_channels` (v1.1) | Re-dated |
| `local_config` | `cli_core_command_*` (subsumed) | Removed |

### 8.3 New keys (no equivalent in old plan)

All `acp_core_adapter_*` (7), all `core_core_skill_*` + `skills_core_render_per_vendor`, all `core_core_event_*`, `core_core_continuation_bridge`, `core_core_notification_pump`, all `bot_pro_*` (5), all `ctx_pro_*` (5), all `blob_*` (4), all `license_*` (6), all `interactive_core_*` (3), `acp_core_session_attach_*` (2), `tui_core_modal_collision_escape`, `tui_core_input_paste_as_atom`, etc.

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
- [ ] Greenfield → `notif_pro_external_channels`
- [ ] Wiring → `tui_pro_trace_source_react`

---

## 9. Marketing Copy

### 9.1 Comparison Table (for website pricing page)

| Capability | Free | Pro |
|---|---|---|
| **Cross-vendor orchestration** | All 7 ACP agents (Claude/Kiro/Codex/Gemini/Cursor/OpenCode/Kimi) | Same |
| **Parallel workers** | 2 concurrent | 10 concurrent |
| **Rate-limit recovery** | One-keystroke manual switch | Silent automatic *(coming Q3)* |
| **Worktree isolation** | ✓ | ✓ |
| **Manual review gate** | ✓ | ✓ |
| **Auto-approve policies** | ✗ | ✓ |
| **Closed-loop PR creation** | Manual | **Automatic on success** ★ |
| **Linear / Plane sync** | Read-only | **Bidirectional** ★ |
| **Telegram remote-control** | ✗ | **Single-operator bot** ★ |
| **Multi-session epic plans** | Single-plan ephemeral | **Durable, cross-session** ★ |
| **Custom MCP tools / skills** | ✗ | ✓ |
| **DuckDB analytics + reports** | ✗ | ✓ |
| **Per-project cost tracking** | Per-session only | Per-project + per-day |
| **Budget caps** | Soft warning | **Hard caps** *(coming Q3)* |
| **Session resume** | Process-restart re-attach | **Full event-log replay** *(coming Q3)* |
| **External notifications** | In-TUI only | **Slack/Discord/webhook** *(coming Q3)* |
| **Bundled skills** | All 17 × 7 vendors | Same + custom org skills |
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
> When you want SPUR to *do this for you* — auto-create PRs, sync Linear status, run while you're at lunch, take phone approvals — that's Pro.

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

# SPUR — Product Requirements Document

**Version:** 2.2  
**Date:** July 16, 2026  
**Author:** Product Owner, SPUR TUI  
**Status:** Grounded (v2.0 base + v2.1 TUI surfaces; **v2.2 FeatureGate / Community-first policy** reconciled against `crates/spur-license` `FeatureGate`, `FeatureKey` registry (63 keys), and signed `resources/default_policy.json` policy_version `2026-07-03`)

**v2.2 changelog:** Replaced legacy tier marketing language (`single_worker`, `tui_session_detail` as Pro-only, old policy feature names) with the live Community-first entitlement model. Cross-mapped every product surface to `FeatureKey` + signed policy. Documented that **quotas** (not missing feature keys) limit Community concurrency.

**v2.1 changelog:** Documented post–April 2026 TUI surfaces — Plan Browser, Loop Browser, Explore Browser, Agent Config Browser, Insights.

---

## 1. Executive Summary

### Product Vision

SPUR is a Rust-native orchestration layer for AI coding agents. It sits between project management tools and agent CLIs, providing a terminal user interface (TUI), a Telegram bot frontend, and a headless orchestration engine that manages brain–worker delegation, durable plan execution, human-in-the-loop review, and cost tracking. It does not replace Claude Code, Kiro, Codex, or Gemini. It makes them composable, recoverable, and observable.

**One-liner:** *"One brain, many workers, zero lost context."*

**Conversion one-liner (marketing):** *"Issue in, PR out — across every agent, in parallel, with one review surface."*

### What Exists Today (Grounded)

The workspace is ~500k lines of Rust across 22 crates under `crates/` (plus `xtask/`). The following are **production-hardened** (not aspirational):

- **Dual-channel protocol architecture**: ACP (SPUR → Agent, JSON-RPC/stdio) and MCP (Agent → SPUR, tool calls)
- **Durable plan reconciler**: Plans survive process restarts in a local SQLite beads store; GitHub is used only for PR creation
- **Review gate state machine**: Human approval/deny/modify/retry with timeout, auto-merge gating, and cancellation
- **Session attach lock**: `fs4` advisory locks prevent split-brain multi-window attachment to the same brain session
- **Continuation bridge**: Delegation outcomes are clipped to a bounded envelope and fed back to the brain scheduler as ordered turns
- **Peer mailbox**: Structured worker-to-worker messaging with ledgered delivery and stranded-message reconciliation (Stage-1, opt-in)
- **Tiered license gating**: `FeatureGate` + signed Ed25519 policy (`default_policy.json`). **Community is the daily-driver baseline** (48 feature keys); Pro adds ~14 upsell keys via `@inherit:community`
- **Event-sourced lineage**: Pure projections from an NDJSON event stream; session resume is replay
- **Outcome materialization**: Content-addressed blob storage (memory, FS, git) with measured instrumentation
- **Paste-as-atom**: Multi-line paste safety in the TUI composer (`tui_core_input_paste_as_atom` — Community)
- **Multi-view TUI control tower**: Dashboard, Session Detail, Plan Inspector, Issue Browser, palette, collision modal are **Community feature keys**. Plan Browser / Loop Browser / Explore / Agent Config / Session Picker ship in the TUI without separate `FeatureKey` rows (ungated views). Insights requires Pro `ctx_pro_duckdb_engine` + `analytics` build feature
- **Explore pool**: Ecosystem skills and agent personas browsable/adoptable from the TUI (`/explore`) and CLI (`spur explore …`); skills baseline is `skills_core_registry` (Community)

### Problem Statement (Still Valid)

1. **Rate limit fragility.** Claude Code Max users exhaust 5-hour session windows in under 90 minutes. When limits hit, context is lost.
2. **Single-vendor lock-in.** Claude Agent Teams, Codex CLI, and Kiro do not talk to each other. Developers manually copy context between terminal tabs.
3. **No durable execution.** If a terminal window closes mid-task, the task is gone. There is no resume, no audit trail, no review gate.
4. **Cost opacity.** Developers see API bills days later. There is no per-session, per-task, or per-plan spend visibility at the moment of delegation.

### Solution

SPUR is the **nervous system**, not the brain. It provides:

```
PM Tool (beads/GitHub) ←→ SPUR Orchestrator ←→ Brain Agent (Claude/Kiro/Codex)
                                 ↓
                         Worker Agents (in git worktrees)
                                 ↓
                     Human Review Gate → Merge / Retry / Reject
```

---

## 2. Product–Architecture Mapping

This section maps **existing technical capability** (what the code does) to **product-facing value** (what the user experiences). This is the core of the v2.x revamp: no aspirational features, no forward-dating.

| Technical Capability | Product Feature | User Value | Maturity |
|---|---|---|---|
| ACP client + session lock (`spur-acp`) | Secure brain session attach | No split-brain when two terminal windows open the same project | Production |
| BrainScheduler + ContinuationBridge (`spur-core`) | Ordered turn execution, session resume | Brain picks up exactly where it left off after restart or failover | Production |
| EventFunnel + broadcast (`spur-core`) | Real-time TUI + Telegram bot sync | Every event appears in both frontends simultaneously | Production |
| ExecutorLineage projection (`spur-core`) | Collapsible ASCII agent tree with phase spinners | Visual map of what every worker is doing, how long, how much it cost | Production |
| ReviewSink + state machine (`spur-core`) | `[A]pprove [D]eny [M]odify [R]etry` review cards | Human gate before any code merges; timeout + retry policies | Production |
| OutcomeStore + materializer (`spur-blob-store`, `spur-mcp`) | Diffs, PR links, and artifacts in the Detail pane | Full delegation output is persisted and browseable, not lost to scrollback | Production |
| PeerMailboxRouter + Ledger (`spur-core`) | Worker-to-worker messaging | Workers can coordinate without round-tripping through the brain | Stage-1 (opt-in) |
| `spur-pm` beads adapter (`br` CLI boundary) | Local-first plan persistence | Plans survive crashes; no cloud dependency for core workflow | Production |
| `spur-pm` GitHub adapter (`gh` CLI) | PR creation from delegation outcomes | One-key "create PR" after review approval | Production |
| `spur-cost` SQLite + PricingRegistry | Per-executor, per-session, per-project cost display | See spend as it happens, not on next month's bill | Production |
| `spur-context` DuckDB analytics | Daily / weekly cost reports via SQL + optional Insights TUI | Aggregated spend analysis without ETL | Production (SQL); Insights UI maturing |
| `spur-license` `FeatureGate` + Ed25519 policy | Wait-free `has(FeatureKey)` / `quota(QuotaKey)` over `ArcSwap` snapshot | Community offline-by-default; Pro/Team via signed policy + JWT | Production |
| `spur-bot` Telegram runtime | Mobile review (`bot_pro_telegram_solo`, `bot_pro_inline_review`) | Step away from the terminal without losing the loop | Production (**Pro** feature keys) |
| WorktreeManager (`spur-worktree`) | True filesystem isolation per worker | Parallel workers cannot clobber each other's files | Production (Community) |
| PlanProjectionStore + Plan Browser (`spur-core`, `spur-tui`) | Browse, claim, start/resume sprint plans | Durable plans are inventory, not only drill-down | Production (TUI view unregistered; plan durability Pro = `mcp_pro_plan_durable`) |
| Loop runtime + Loop Browser (`spur-core`, `spur-tui`) | Pause/resume/retire recurring loops | Ongoing agent ops beyond one-shot plans | Production (TUI view unregistered) |
| Explore pool (`spur-core` explore + `spur-tui` ExploreBrowser) | Browse/adopt skills and agent personas | Extensibility without leaving the TUI | Production (Community `skills_core_registry`; custom skills Pro) |
| Agent config browser (`spur-tui`) | Inspect/edit registered agent settings | Multi-agent setup without hand-editing TOML only | Production (TUI view unregistered) |

### Second-Order Insight: What This Mapping Reveals

**First-order thinking:** "SPUR is a TUI that runs multiple agents."

**Second-order thinking:** "SPUR is a distributed-systems kernel for agent execution disguised as a TUI." The real differentiators are not the panels or keybindings. They are:

1. **Session resume as replay** — The event-sourced lineage makes SPUR the only agent tool where closing your laptop does not lose context. This matters more than any dashboard widget.
2. **Review gate as durable state** — Most tools treat human review as a UI convenience. SPUR treats it as a first-class state machine with timeout, retry, and merge gating. This turns "human-in-the-loop" from a slogan into an execution guarantee.
3. **Local-first plan durability** — Beads (SQLite via `br` CLI) means plans survive SPUR crashes, OS updates, and network outages. This is resilience that cloud-only competitors cannot match without offline sync.
4. **Dual-channel autonomy** — ACP gives the brain freedom to stream; MCP gives SPUR control over delegation and PM ops. Neither channel is a workaround for the other. This architectural choice is why SPUR can support any ACP-speaking agent without per-agent hacks.
5. **Control-tower density (v2.1)** — Plan Browser, Loop Browser, and Explore Browser turn the TUI from “chat + tree” into ops surfaces for inventory, recurrence, and ecosystem adoption. Market the kernel first; demo these screens as proof of operational depth.

---

## 3. Target Users

### Primary Persona: "The Orchestrator"

- **Role:** Senior/Staff Engineer or Tech Lead at a startup or mid-size company (10–200 employees)
- **Current tooling:** Claude Code Max ($100–200/mo), Kiro CLI, Codex CLI, Gemini CLI
- **Monthly AI spend:** $200–600 across tools
- **Pain frequency:** Hits rate limits 2–5× per week, manually juggles 2–3 terminal tabs
- **Technical profile:** tmux/zellij, git worktrees, comfortable with JSON-RPC and TOML config
- **Decision driver:** Flow preservation and auditability, not cost alone
- **SPUR value prop:** "I can start a session Friday, review worker output Saturday on Telegram, and merge Monday without losing context."

### Secondary Persona: "The Team Lead"

- **Role:** Engineering Manager overseeing 3–10 developers
- **Pain:** No visibility into which agents the team uses, per-project spend, or review queue depth
- **Decision driver:** Cost visibility, standardization, governance
- **SPUR value prop:** "I can see pending reviews, per-project costs, and which agents are actually delivering code that merges."

### Tertiary Persona: "The Mobile Operator"

- **Role:** Developer who steps away from desk but needs to approve/reject worker output
- **Pain:** Terminal-only workflows chain you to your desk
- **SPUR value prop:** Telegram bot with inline review buttons and session status push notifications

### Anti-Persona (Still Not For)

- Junior developers using a single AI assistant casually
- Non-technical users who need a GUI
- Enterprise teams requiring SOC2/HIPAA compliance at launch
- Users who want a "set and forget" fully autonomous system (SPUR requires human review by design)

---

## 4. Tier-Structured Product Definition

SPUR's commercial model is **feature-gated tiers** enforced by a signed policy document and a wait-free runtime gate.

### 4.0 Source of truth (do not invent tier names)

| Layer | Path | Role |
|---|---|---|
| **Feature registry** | `crates/spur-license/src/policy/feature_key.rs` | 63 typed `FeatureKey` consts (`<crate>_<tier>_<capability>` naming). Parse via `FeatureKey::from_known`; unknown strings are dropped. |
| **Signed policy** | `crates/spur-license/resources/default_policy.json` | Canonical grant lists + quotas per tier. `policy_version: 2026-07-03`. Pro/Team/Enterprise use `@inherit:community`. |
| **Runtime gate** | `crates/spur-license/src/gate.rs` → `FeatureGate` | `has(FeatureKey)`, `quota(QuotaKey)`, `tier()`, `update_state`. Default construction = **Community offline snapshot**. |
| **Default install** | `CommunityProvider` + `FeatureGate::new(PolicyResolver::embedded())` | No license key required for Community. |
| **Upgrade CTA** | `required_tier_for(FeatureKey)` | Walks Community → Pro → Team → Enterprise until the key appears in policy. |

**Product rule (Wave 9 / Community-friendly):** Give away the **complete solo daily driver** on Community. Monetize **remote control, multi-agent depth, durable plan control-plane extras, DuckDB insights, custom skills** on Pro. Do **not** gate Session Detail, Plan Inspector, Issue Browser, session resume, or basic MCP/PM behind Pro.

**Naming caveat:** Some keys still contain `_pro_` in the string (e.g. `pm_pro_beads_advanced`) but are **granted on Community** by policy. The **policy list is authoritative**; key name is historical registry taxonomy, not the paywall.

**Quota vs feature:** Community grants `core_core_parallel_workers` (the capability exists) but sets `max_concurrent_workers: 1`. Concurrency is a **quota**, not a missing feature key. Pro raises workers to 10 and retention/failover chain accordingly.

### 4.1 Community (Free) — daily-driver baseline

**Policy metadata:** `label: Free` · *“Daily-driver baseline; covers solo-dev complete workflow.”*

**48 feature keys** in signed policy (all Community-granted):

#### Product map (what users get free)

| Product surface | `FeatureKey`(s) | Notes |
|---|---|---|
| Brain session + manual failover | `core_core_brain_session`, `core_core_brain_failover_manual_keystroke` | |
| Event pipeline / lineage | `core_core_event_pipeline` | Funnel + sink + lineage umbrella |
| Review gate (manual A/D/M/R + retry config) | `core_core_review`, `core_core_review_retry_config` | Auto-approve is Pro |
| Session resume (attach) | `core_core_session_resume` | Event-replay depth is Pro/v1.1 |
| Plan persistence | `core_core_plan_persistence` | Ephemeral MCP plan tools free; durable plan MCP is Pro |
| Parallel workers **capability** | `core_core_parallel_workers` | **Quota: 1 concurrent worker** |
| Permission prompts | `core_core_permission_request_detection` | |
| ACP transports + adapters | `acp_core_transport_*`, `acp_core_adapter_{claude_code,codex,kiro}`, `acp_core_session_attach_advisory_lock` | |
| Worktree isolation + orphan cleanup | `worktree_core_isolation`, `worktree_core_orphan_cleanup` | |
| MCP core tools | `mcp_core_server_dispatch`, `mcp_core_delegate`, `mcp_core_outcome_fetch`, `mcp_core_pm`, `mcp_core_pr`, `mcp_core_plan_ephemeral`, `mcp_core_graph_tools` | Graph tools are Community (viral surface) |
| PM / beads / browse / PR | `pm_core_*`, **`pm_pro_beads_advanced`** | Advanced beads granted free despite name |
| Cost display | `cost_core_session_display`, `cost_core_pricing_registry` | Observational only |
| Skills registry | `skills_core_registry` | Custom skills Pro |
| **TUI views (gated keys)** | `tui_core_view_dashboard`, `tui_core_view_session_detail`, `tui_core_view_plan_inspector`, `tui_core_view_issue_browser`, `tui_core_view_palette_overlay`, `tui_core_modal_collision_escape`, `tui_core_input_paste_as_atom` | Full control-tower UI on Free |
| CLI surface | `cli_core_{init,agents,sessions,run,exec,tui,cost,connect,license_activate}` | Entire CLI core free |

#### Community quotas (signed policy)

| Quota | Community value |
|---|---|
| `max_concurrent_workers` | **1** |
| `event_retention_bytes` | 128 MiB |
| `brain_failover_chain_depth` | 1 |

#### TUI views without a dedicated `FeatureKey` (currently unregistered)

These ship in `spur-tui` and are **not** listed in the 63-key registry. They are effectively Community-available whenever `cli_core_tui` + core runtime are on:

- Session Picker, Plan Browser (`/sprints`), Loop Browser, Explore Browser (`/explore`), Agent Config Browser (`/configure`), Mermaid overlay (`markdown` feature), Insights placeholder when `analytics` off

**Second-order product insight:** Community is intentionally **product-complete for solo use**. Revenue should not depend on hiding the TUI. Upsell is depth (Telegram, durable plan control plane, DuckDB, auto-review, peer mailbox, concurrency quota), not “you can open Session Detail.”

### 4.2 Pro — upsell on Community inherit

**Policy:** `@inherit:community` + **14 Pro-only keys**. Metadata: *“Adds remote control, multi-agent coordination, review control plane, cost insights, extensibility.”*

| Upsell | `FeatureKey` | Product story |
|---|---|---|
| Telegram bot + inline review | `bot_pro_telegram_solo`, `bot_pro_inline_review` | Mobile Operator persona |
| Peer mailbox | `core_pro_peer_mailbox_router` | Worker↔worker (Stage-1) |
| Auto-approve review | `core_pro_review_auto_approve` | Less manual gate friction |
| Worker heartbeat watchdog | `core_pro_worker_heartbeat_watchdog` | Hang detection |
| Durable plan MCP | `mcp_pro_plan_durable` | Reconciler / journal path |
| MCP review + scope-drift signals | `mcp_pro_review`, `mcp_pro_signal_watcher_scope_drift` | Control-plane recovery |
| Per-project cost tracking | `cost_pro_per_project_tracking` | Team Lead spend lens |
| DuckDB analytics engine | `ctx_pro_duckdb_engine` | Insights TUI + SQL reports |
| Custom skills | `skills_pro_custom` | Beyond bundled registry |
| Blob namespace deletion | `blob_pro_namespace_deletion` | Ops hygiene |
| License offline grace / revocation | `license_pro_offline_grace`, `license_pro_revocation_polling` | Paid-plan hygiene |

#### Pro quotas

| Quota | Pro value |
|---|---|
| `max_concurrent_workers` | **10** |
| `event_retention_bytes` | 1 GiB |
| `brain_failover_chain_depth` | 3 |

#### v1.1 roadmap key (not in live Pro grant list yet)

- `core_pro_session_resume_event_replay` — listed under `v1_1_q3_roadmap.pro` in policy JSON; deeper resume fidelity remains a Pro story.

**Second-order product insight:** Pro sells **scale + remote + analytics + control-plane depth**, not the basic TUI. Concurrent workers 1→10 is the clearest quantitative upsell next to Telegram and DuckDB.

### 4.3 Team

**Live policy reality:** Team currently **inherits Community + the same 14 Pro keys** (inlined; resolver does not yet `@inherit:pro`). Quotas: workers 10, retention 10 GiB, failover depth 3, plus seat floor intent.

| Feature | Status vs old PRD |
|---|---|
| Issue Browser / PM browse | **Already Community** (`tui_core_view_issue_browser`, `pm_core_browse`) — not a Team-only gate |
| Shared review queue UI | Soft product concept; no distinct Team-only `FeatureKey` yet |
| Team cost dashboard | Requires Pro `ctx_pro_duckdb_engine` + Insights GA; not a separate Team key |
| RBAC / PM webhooks | Still planned — no registry keys |

**Second-order product insight:** Do not market Team as “unlock Issue Browser.” Market Team as **seats, retention, and future coordination entitlements** once distinct keys ship. Until then Team is commercially a Pro-feature set with higher retention quota.

### 4.4 Enterprise

**Live policy reality:** Same feature list as Pro/Team (placeholder). Quotas unlimited for workers / retention / failover.

| Feature | Status |
|---|---|
| SSO/SAML | 🚧 Planned — no `FeatureKey` yet |
| Audit logs | 🚧 Planned — no `FeatureKey` yet |
| Custom policies | 🚧 Partial — signed policy + license badge today |
| Custom MCP tools | 🚧 Deferred (Wave-8 dropped `mcp_pro_custom_tools`) |
| SLA guarantee | 🚧 Planned — commercial, not a runtime key |

### 4.5 FeatureGate × product surface cheat sheet

| User-visible capability | Community? | What actually limits it |
|---|---|---|
| Dashboard + Session Detail + Plan Inspector + Issue Browser | **Yes** | Feature keys on Community |
| Session resume / attach lock / collision modal | **Yes** | Community keys |
| Paste-as-atom composer | **Yes** | Community key |
| 1 concurrent worker | **Yes** | **Quota = 1** (feature key present) |
| 10 concurrent workers | Pro | Quota |
| Telegram review | **Pro** | `bot_pro_*` |
| Durable plan reconciler MCP | **Pro** | `mcp_pro_plan_durable` |
| Auto-approve | **Pro** | `core_pro_review_auto_approve` |
| DuckDB / Insights analytics | **Pro** | `ctx_pro_duckdb_engine` + build feature |
| Peer mailbox | **Pro** | `core_pro_peer_mailbox_router` (opt-in runtime) |
| Custom skills | **Pro** | `skills_pro_custom` |
| Explore / Plan Browser / Loop Browser / Agent Config | **Effectively free** | No dedicated `FeatureKey` (ungated views) |

---

## 5. Feature Specifications (Grounded)

### 5.1 Views (What the User Sees)

Navigation is driven by `ViewId` in `crates/spur-tui`:

`Dashboard` · `SessionDetail` · `SessionPicker` · `PlanInspector` · `PlanBrowser` · `LoopBrowser` · `IssueBrowser` · `ExploreBrowser` · `AgentConfigBrowser` · `Insights` · `MermaidOverlay` (feature-gated)

#### Dashboard (`tui_core_view_dashboard` — Community)

| Element | Description |
|---|---|
| Agents tree | Collapsible ASCII tree of executor lineage. `j/k/↑/↓` navigate, `Enter` focuses, `c` toggles collapse, `z` toggles zoom. |
| Activity log | Scrolling system event log (5,000-entry cap). Color-coded by kind. Follow mode with manual override. |
| Detail pane | Tabbed detail for focused executor: Stream, Artifacts, Attempts, Task, Review. |
| Input bar / Composer | Multi-line chat with Emacs/Vim modes, `@`-mentions, `/` slash commands, protected ranges, paste-as-atom (`tui_core_input_paste_as_atom`). |
| Status bar | Context hints, issue count, running count, pending review count, total cost, elapsed time, license badge, flag summary. |
| Workers panel | Collapsible inline panel showing active worker delegations. `Alt+D` toggles. Concurrent count limited by **quota** (Community = 1). |
| Empty state | Setup nudge when no agents are configured; rotating example prompts when agents exist. |

#### Session Detail (`tui_core_view_session_detail` — Community)

| Element | Description |
|---|---|
| ReAct trace | Streaming trace: `UserMessage`, `AgentMessage`, `Think`, `Observe`, `Act`, `Delegate`, `Permission`. Coalesces consecutive entries. |
| Markdown streaming | Live markdown with GFM tables, mermaid fence extraction, 50 ms debounced flush, 64 KB safety cap. |
| Mermaid viewer | `Alt+V` opens full-screen mermaid overlay. `[`/`]` cycles diagrams. Requires `markdown` compile-time feature. |
| Input integrations | `@`-mention picker (files, dirs, workers), `/` slash-command picker, `Ctrl+R` history picker. |
| Permission prompts | Inline `[y] allow [n] deny [a] always allow` with countdown timers. |
| Draft persistence | 500 ms debounced save to `SessionMetadataStore`. Force flush at intent boundaries. |
| Workers panel | Inline active workers; `Alt+D` collapse. |
| Load / cancel UX | `LoadState` pipeline for resume attach; Esc cancel-with-confirm while stream is in flight; `fs_unsafe` banner when lock is unenforceable. |
| Cost / context | SPUR estimate + optional agent-reported session cost; context used/size from usage updates. |

#### Session Picker (ungated view — Community runtime)

| Element | Description |
|---|---|
| Session list | Searchable list with pinned ⭐, archived, brain name, relative time, short ID. Virtual `[+ Start new session]` row. |
| Preselect | `--session <id>` preselects and auto-dispatches resume. Bare `spur tui` preselects last but requires Enter. |
| Search/filter | `/` focuses search bar. Real-time filtering. |
| Pin / archive / rename | `p` pin/unpin, `d` archive, `R` rename. |
| Collision modal | On `SessionAttachRejected`: holder PID, TTY, workdir, `kill <pid>` escape hatch (`tui_core_modal_collision_escape`). |
| Draft-loss safety | Confirm-switch banner when unsent draft exists. |
| Gate note | Resume capability is Community (`core_core_session_resume`). No separate picker `FeatureKey`. |

#### Plan Inspector (`tui_core_view_plan_inspector` — Community)

| Element | Description |
|---|---|
| Plan header | Plan ID, status, progress gauge, awaiting review count, failed count, next action. |
| Stage board | Kanban-style columns per stage. Tasks show status badge, worker link, blocked indicator, dependency hints, retry count. |
| Task detail | Identity, execution (agent, attempt, branch), dependencies, output (summary, diff, mutation ID, superseded-by). |
| Responsive layout | Side-by-side when width ≥ 90, stacked when narrower. |
| Gate note | Durable plan **MCP** (`mcp_pro_plan_durable`) is Pro; the inspector view key is Community. |

#### Plan Browser (`/sprints` — ungated view)

| Element | Description |
|---|---|
| Plan list | Active and historical sprint plans with sort (`S`) and filter (`f`). |
| Detail peek | Summary / plan body / work-item peeks (`p` / `o`). |
| Claim / start | `c` claim (with force-reclaim confirm when needed); `s` start or resume owned plans. |
| Cross-nav | `L` jumps toward loops; refresh via `r`. |
| Technical basis | `spur-tui` PlanBrowserView + plan projection events from `spur-core`. |

#### Loop Browser (ungated view)

| Element | Description |
|---|---|
| Loop list | Recurring loop rows with sort (`S`) and filter (`f`). |
| Inspect | Enter opens detail; `o` opens related issue when present. |
| Lifecycle | `p` pause/resume; `x` retire; `r` refresh. |
| Product value | Surfaces **ongoing agent ops** beyond one-shot plan execution. |
| Technical basis | `spur-tui` LoopBrowserView + loop events from the orchestrator. |

#### Issue Browser (`tui_core_view_issue_browser` — Community, not Team-only)

| Element | Description |
|---|---|
| Issues list | ID, Priority (P0–P4), Type, Status, Assignee, Title. |
| Issue detail | Full body, metadata, labels, URL. |
| Status updates | `o/w/b/d` — open/in_progress/blocked/closed. |
| Work on issue | `W` constructs prompt from issue and sends to brain. |
| Gate note | Also backed by `pm_core_browse` / beads keys — all Community. |

#### Explore Browser (`/explore` — ungated view; `skills_core_registry` Community)

| Element | Description |
|---|---|
| Tabs | Skills · Agents. |
| Stages | Browse → Gate (confirm) → Manage (status / materializations). |
| Catalog | Bundled + layered store catalog; filter, star, adopt/apply. |
| CLI parity | `spur explore sync\|list\|add\|remove\|status` manages the same pool. |
| Product value | Ecosystem extensibility without leaving the control tower. |
| Upsell | Custom skills installation depth: `skills_pro_custom` (Pro). |

#### Agent Config Browser (`/configure` — ungated view)

| Element | Description |
|---|---|
| Agent list | Registered agents with field navigation and optional preselect by name. |
| Edit draft | In-TUI draft edit + validation errors before apply. |
| Product value | Multi-agent setup without only hand-editing config files. |
| Technical basis | `spur-tui` AgentConfigBrowserView. |

#### Insights (`ctx_pro_duckdb_engine` Pro + `analytics` build feature)

| Element | Description |
|---|---|
| Tabs | Overview · Timeline · Breakdown · Live. |
| Dimensions | Granularity (daily/weekly/monthly) and dimension (agent/model/project). |
| Backend | Async DuckDB engine from `spur-context` (Pro feature key). |
| Build gate | When `analytics` is off, view renders “Analytics unavailable in this build.” |
| Maturity | **Maturing** — first UI over Pro analytics entitlement; not a separate Team key. |

#### Mermaid Overlay (`markdown` feature)

Full-screen diagram viewer opened from Session Detail (`Alt+V`). Cycles diagrams with `[`/`]`. Requires `markdown` compile-time feature.

### 5.2 Composer & Input System

| Feature | Description |
|---|---|
| Multi-line editing | Emacs (default) and Vim (Normal/Insert/Visual/Operator) modes. |
| Protected ranges | Atomic `@mention` and paste-reference tokens. Cursor-skipped, deleted as units. |
| Paste-as-atom | Multi-line pastes become placeholder tokens (`[Paste #N · M lines]`). Side table capped at 50 entries (LRU). Expands on submit. |
| `@`-mentions | Fuzzy picker for files, directories, workers. `MentionRegistry` with 60 s TTL, nucleo scoring, +25 % worker score boost. Explore-pool agents surface in worker queries when available. |
| `/` slash commands | Spur-local (`/clear`, `/mode`, `/sessions`, `/cost`, `/quit`, `/vim`, `/issues`, `/sprints`, `/explore`, `/configure`, `/theme`, `/notebook`, …) merged with agent-advertised commands. |
| History picker | `Ctrl+R` fuzzy search over global input history (cap 100). |
| Activity spinner | Per-frame spinner driven by `ActivityKind`. |
| Soft wrap | Unicode-width aware word-boundary wrapping. `TAB_WIDTH = 4`. |

### 5.3 Review System

| Feature | Gate | Description |
|---|---|---|
| Review card | Community (`core_core_review`) | Renders review kind, summary, diff stats, PR URL, error. Action hints: `[A]pprove [D]eny [M]odify [R]etry`. |
| Retry config | Community (`core_core_review_retry_config`) | Free reliability baseline (Wave 9 tier-shift). |
| Inline executor cards | Community runtime | Live executor status inline in brain trace at delegate call sites. |
| Review tab | Community (DetailPane on free views) | Dedicated tab with full context and keyboard shortcuts. |
| Auto-approve | **Pro** (`core_pro_review_auto_approve`) | Policy-driven skip of manual gate when conditions match. |
| MCP review / scope-drift | **Pro** (`mcp_pro_review`, `mcp_pro_signal_watcher_scope_drift`) | Control-plane recovery tools. |

### 5.4 Cost & Telemetry

| Feature | Gate | Description |
|---|---|---|
| Basic cost badge | Community (`cost_core_session_display`, `cost_core_pricing_registry`) | Total cost and elapsed time in status bar. Per-executor cost in lineage tree. |
| Per-project tracking | **Pro** (`cost_pro_per_project_tracking`) | Project-scoped spend (TUI may show upgrade modal when denied). |
| Insights TUI | **Pro** (`ctx_pro_duckdb_engine`) + `analytics` build | Overview / Timeline / Breakdown / Live over DuckDB. |
| Daily/weekly SQL reports | **Pro** engine | `spur-context` DuckDB; not Community-default. |

**Honest product note:** `spur-cost` is currently **observational, not enforceable**. It records start/end events and computes cost, but the orchestrator spawns sessions without any budget check. This is a known gap (Architecture Risk #17). The product positioning must be "cost visibility," not "cost governance," until enforcement lands.

---

## 6. Architecture at a Glance

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  Frontends                                                                  │
│  ┌─────────────────────────────────────┐  ┌─────────────────────────────┐  │
│  │ TUI (ratatui + crossterm)           │  │ Telegram Bot (spur-bot)     │  │
│  │ Dashboard · SessionDetail/Picker    │  │ Forum-topic sessions        │  │
│  │ PlanInspector · PlanBrowser         │  │ Inline review buttons       │  │
│  │ LoopBrowser · IssueBrowser          │  │                             │  │
│  │ ExploreBrowser · AgentConfig        │  │                             │  │
│  │ Insights* · Mermaid* · Palette      │  │                             │  │
│  └─────────────────────────────────────┘  └─────────────────────────────┘  │
│  * feature-gated (analytics / markdown)                                     │
├─────────────────────────────────────────────────────────────────────────────┤
│  Shared Host (spur-interactive)                                             │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │ Channel wiring · Review lane · Shutdown orchestration                 │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
├─────────────────────────────────────────────────────────────────────────────┤
│  Orchestration (spur-core)                                                  │
│  ┌──────────────┬──────────────┬──────────────┬──────────────┬────────────┐ │
│  │ Orchestrator │ BrainSched   │ EventFunnel  │ ExecutorLine │ PeerMailbox│ │
│  │              │ Continuation │ ReviewSink   │ age          │ Router/Led │ │
│  │              │ Bridge       │              │              │ ger        │ │
│  │ PlanProjection · Loops · Explore pool · Agent profiles                │ │
│  └──────────────┴──────────────┴──────────────┴──────────────┴────────────┘ │
├─────────────────────────────────────────────────────────────────────────────┤
│  MCP Server (spur-mcp)                                                      │
│  ┌─────────────────────────────┐  ┌─────────────────────────────────────┐   │
│  │ Tool dispatch               │  │ Reconciler · PlanProjectionStore    │   │
│  │ OutcomeMaterializer         │  │ SignalWatcher · MutationExecutor    │   │
│  └─────────────────────────────┘  └─────────────────────────────────────┘   │
├─────────────────────────────────────────────────────────────────────────────┤
│  Protocol (spur-acp)                                                        │
│  ┌───────────────────────────────────────────────────────────────────────┐  │
│  │ AgentConnection · SpurEvent · SessionAttachGuard · OutcomeKey         │  │
│  └───────────────────────────────────────────────────────────────────────┘  │
├─────────────────────────────────────────────────────────────────────────────┤
│  Support Services                                                           │
│  ┌──────────┬──────────┬──────────┬──────────┬──────────┬────────────────┐  │
│  │ spur-pm  │ spur-cost│ spur-work│ spur-lic │ spur-blob│ spur-context   │  │
│  │ beads    │ SQLite   │ tree     │ ense     │ -store   │ DuckDB         │  │
│  │ GitHub   │ Pricing  │ GitBlob  │ Ed25519  │ Memory/  │ analytics      │  │
│  │          │ Registry │ Outcome  │ policy   │ FS/Git   │                │  │
│  │          │          │ Store    │          │          │                │  │
│  └──────────┴──────────┴──────────┴──────────┴──────────┴────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Data Flow (Simplified)

1. User sends message via TUI or Telegram → `InteractiveFrontendHost` → `Orchestrator`
2. `Orchestrator` prompts brain via ACP → brain reasons and calls MCP tools
3. MCP tools (e.g., `delegate_to_worker`) → `Orchestrator` spawns worker in git worktree
4. Worker streams notifications → `EventFunnel` → broadcast to TUI, bot, EventSink, Lineage
5. Worker completes → `OutcomeStore` persists full result → `OutcomeMaterializer` clips to `BrainContinuation`
6. `BrainScheduler` feeds continuation back to brain as next ordered turn
7. Human reviews output → approve → merge worktree → create PR (GitHub) → update beads issue

---

## 7. Risk-Informed Product Decisions

The architecture audit identified 41 risks (11 fixed, 2 mitigated, 28 open). Product decisions must account for these, not ignore them.

### 7.1 Open Risks with Product Impact

| Risk # | Technical Risk | Product Impact | User-Facing Mitigation Today |
|---|---|---|---|
| #2 | Broadcast `Lagged` drops events when subscribers are slow | TUI may miss status updates on burst | 8-event drain cap; no replay yet. Users on very active sessions may see stale state. |
| #4 | Worktree orphaning on unclean shutdown | Disk space leaks | None. Users may need to manually clean `../.worktrees/` occasionally. |
| #6 | Worker JoinHandles not tracked for shutdown abort | Worker tasks may leak on cancellation | Cancellation token registry exists; visual feedback in TUI is immediate. |
| #7 | Orchestrator is a God Object (~8,650 lines) | Slower feature delivery, higher regression risk | Aggressive testing (SIT/UAT harness) before merge. |
| #8 | No outer worker timeout (hang = indefinite) | Worker can run forever, burning tokens | Heartbeat watchdog exists (default off until emitter ships). Users can cancel manually. |
| #17 | Cost tracking has no budget enforcement | Users can overspend accidentally | Honest product positioning: "visibility," not "governance." Document the gap. |
| #22 | Peer mailbox unbounded ledger growth | Memory growth in long sessions | Peer mailbox is default-off. Enable only for short, peer-heavy sessions. |
| #24 | NDJSON rotation death spiral on disk full | Silent event loss | Monitor disk space; no in-product alert yet. |
| #26 | TUI `LoadState` deadlock on `BrainConnectFailed` | Infinite spinner, no error surfacing | Document: if spinner persists >30 s, press `Esc` and retry attach. |
| #29 | SQLite `SQLITE_BUSY` when querying cost during session | `spur cost` CLI returns opaque errors | Document: query cost when no session is active, or use `spur-context` DuckDB reports. |
| #32 | License downgrade mid-session does not shrink limits | Revoked Pro license keeps Pro concurrency until restart | Acceptable edge case; policy heartbeat checks on restart. |
| #41 | `fs_unsafe` on NFS/sshfs allows multi-instance attach | Two TUI windows on different hosts can attach to same session | Persistent banner warns user; no secondary coordination yet. |

### 7.2 Second-Order Product Strategy

**Decision: Position SPUR as "resilient first, smart second."**

First-order temptation: Market the "smart router" (ML-based task classification) and "unified billing" from the old Phase 4.

Second-order analysis:
- ML-based routing requires training data we do not have at our scale.
- Unified billing requires vendor partnerships that do not exist.
- Both distract from the core value proposition: **durable execution with human review gates**.

**What to market instead:**
1. **Session immortality** — Close your laptop, restart SPUR, resume exactly where you left off. No other agent tool does this.
2. **Worker isolation + review** — Every worker runs in a git worktree. Every output hits a human review gate before merge. This is safety that autonomous agent tools skip.
3. **Terminal-native, zero web UI** — No browser tabs, no cloud login, no SaaS downtime. Everything is local SQLite, local git, local TUI.
4. **Ops surfaces (v2.1, secondary in demos)** — Plan inventory, recurring loops, and Explore adoption prove the control tower is deeper than a chat pane. Lead with resilience; close with these screens.

**Decision: Tier boundaries follow the signed policy (Community-first), not legacy marketing tables.**

- **Community:** Complete solo daily driver — full core TUI views, session resume, review, MCP core, PM browse, graph tools. Concurrency **quota = 1**. Explore / plan / loop browsers unregistered → free in practice.
- **Pro:** Telegram, durable plan MCP, auto-approve, peer mailbox, DuckDB/Insights, custom skills, higher worker quota (10), retention/failover depth.
- **Team:** Today ≈ Pro feature set + higher retention; distinct Team keys still placeholder. Do not sell “Issue Browser” as Team-only.
- **Enterprise:** Unlimited quotas + future compliance keys. Sells trust when keys exist.

**Decision: Be honest about gaps.**

- Cost enforcement is not built. Say so. Do not promise "budget alerts" as a shipping feature.
- Event loss under burst is possible. Document the drain cap.
- Peer mailbox is experimental. Gate it behind config, not marketing.
- Insights is Pro + build-feature and maturing — not full Team cost dashboard GA.
- Key names with `_pro_` may still be Community-granted — always check policy, not string prefix.

Transparency builds trust with the "Orchestrator" persona, who can read Rust and will verify claims against the repo.

---

## 8. CLI Reference

### Global Commands

```
spur init                          Initialize SPUR in current directory
spur tui                           Open TUI dashboard
spur tui --new                     Empty dashboard; no session resume
spur tui --session <id>            Attach to specific session (explicit consent)
spur tui --sessions                Open session picker
spur bot telegram                  Start Telegram bot frontend
spur agents                        List registered agents
spur sessions                      List active sessions
spur cost                          Show cost summary (observational)
spur cost --week                   Weekly breakdown
spur cost --by agent               Per-agent breakdown
spur cost --by project             Per-project breakdown
spur explore sync|list|add|remove|status
                                   Manage ecosystem skills/agents pool
spur workflow run <file>           Execute a workflow definition
spur workflow validate <file>      Validate a TOML workflow
spur version                       Show version info
```

### Configuration Hierarchy

```
1. CLI flags (highest priority)
2. Environment variables (SPUR_BRAIN, SPUR_AGENTS, etc.)
3. Project config: .spur/config.toml (per-repo)
4. User config: ~/.spur/config.toml (global)
5. Defaults (lowest priority)
```

---

## 9. Competitive Landscape (Updated)

| Tool | Language | Interface | Native ACP | Cross-Agent | Durable Plans | Human Review Gate | Session Resume |
|---|---|---|---|---|---|---|---|
| **SPUR** | **Rust** | **TUI + Bot** | **Yes** | **Yes** | **Yes (beads)** | **Yes (state machine)** | **Yes (event replay)** |
| ACPX | Node.js | CLI only | Yes | Yes | No | No | No |
| TUICommander | Rust+Tauri | Desktop | No (PTY) | Detection | No | No | No |
| Ralph | TypeScript | TUI (read-only) | Partial | Yes | No | No | No |
| Agent Orchestrator | Node.js | Web dashboard | No | Yes | YAML | No | No |
| Claude Code | Built-in | Terminal | No | Claude only | No | No | No |
| Kiro CLI | Rust | Terminal | Yes | No | No | No | No |
| Codex CLI | TypeScript | Terminal | Partial | No | No | No | No |

### SPUR's Unique Position (Grounded)

1. **Rust single binary** — Signed install via `curl | sh` (proprietary distribution). No Node.js, no Python, no Docker required for the control plane.
2. **Native ACP + MCP dual channel** — Structured protocol support, not PTY scraping or prompt injection.
3. **Local-first durability** — Plans in SQLite (beads), events in NDJSON, outcomes in git blobs. Survives crashes and outages.
4. **Human review as execution gate** — Not a UI afterthought; a state machine that blocks merge until approved.
5. **Session resume via event replay** — Close the terminal, restart SPUR, resume the exact same brain session.
6. **Cross-vendor agent orchestration** — Claude, Kiro, Codex, Gemini, custom ACP agents.
7. **Telegram bot sharing the TUI correctness path** — Same review lane, same event bus, same state machine.
8. **Control-tower ops surfaces (v2.1)** — Plan Browser, Loop Browser, and Explore pool: inventory, recurrence, and ecosystem adoption in one TUI.

---

## 10. Metrics & Success Criteria (Revised)

### v0.5 — Foundation (Current)

- [x] ACP session manager with attach lock and collision handling
- [x] TUI dashboard with agents tree, activity log, composer
- [x] Brain scheduler with continuation bridge
- [x] Review gate state machine (approve/deny/modify/retry)
- [x] Outcome storage and materialization
- [x] Cost tracking (observational)
- [x] Telegram bot frontend
- [x] License tier system with signed policies
- [x] Durable plan reconciler in beads

### v0.6 — Resilience (Next)

- [ ] Broadcast `Lagged` recovery or backpressure (Risk #2)
- [ ] Worker heartbeat emitter + enable watchdog by default (Risk #23 follow-up)
- [ ] Cost budget enforcement: per-session caps, per-plan ceilings (Risk #17)
- [ ] TUI state-machine closure for all terminal brain events (Risk #26)
- [ ] Worktree orphan cleanup on startup (Risk #4)
- [ ] Orchestrator decomposition: BrainSessionManager, DelegationDispatcher actors

### v0.6.x — TUI surface + Community-first policy (largely landed)

- [x] Plan Browser (list / claim / start) — ungated view
- [x] Loop Browser (list / pause / resume / retire) — ungated view
- [x] Explore Browser + `spur explore` CLI
- [x] Agent Config Browser (`/configure`)
- [x] Community-first signed policy (`2026-07-03`): Session Detail / Plan Inspector / Issue Browser / resume / graph tools free
- [x] Community concurrency as **quota = 1** (not missing parallel-workers key)
- [x] Insights skeleton (`ctx_pro_duckdb_engine` Pro + `analytics` build)
- [ ] Register FeatureKeys for Plan Browser / Loop Browser / Explore if product wants explicit tier control
- [ ] Insights GA polish under Pro analytics entitlement
- [ ] Distinct Team-only keys (today Team ≈ Pro features)
- [ ] `TraceSource` wired into command palette

### v0.7 — Scale

- [ ] Team-specific entitlements beyond Pro (seats / shared queue / RBAC keys)
- [ ] RBAC enforcement (Team tier)
- [ ] PM webhook real-time sync (Team tier)
- [ ] Peer mailbox default-on with ledger pruning (Risk #22)

### v1.0 — Commercial Hardening

- [ ] Audit log viewer (Enterprise)
- [ ] SSO/SAML flow (Enterprise)
- [ ] SLA monitoring and status indicators (Enterprise)
- [ ] Zero open Critical/High risks
- [ ] SIT/UAT harness coverage for all interactive surfaces

---

## 11. Roadmap vs. Reality Check

| Old PRD Phase | What Was Promised | What Exists | Honest Assessment |
|---|---|---|---|
| Phase 1: Foundation | Agent discovery, basic TUI, ad-hoc tasks | TUI multi-view control tower, sessions, composer, review gate, attach lock, Explore + agent config | **Over-delivered on TUI.** Agent discovery via Explore/config browsers closes much of the old gap. |
| Phase 2: Workflow Engine | TOML workflows, rate limit failover, cost tracking | Durable plan reconciler (MCP-based, not TOML), Plan/Loop browsers, cost tracking (observational), brain failover | **Workflow engine is MCP-native, not TOML-native.** TOML workflows are a legacy concept; real users submit plans via MCP. Cost tracking lacks enforcement. |
| Phase 3: PM Integration | Linear, Plane, GitHub | Beads-primary, GitHub-satellite. Linear/Plane not implemented. Issue Browser ships for Team. | **Correct call.** Local-first beads is more resilient than SaaS PM APIs. GitHub PR creation is sufficient. |
| Phase 4: Commercial | Team dashboard, smart router, unified billing | Tier system exists, feature gates exist, DuckDB + Insights (partial). Smart router and unified billing do not. | **Smart router was speculative.** Unified billing requires vendor partnerships. Tier gating is the real commercial foundation. Insights is the first UI path, not GA team dashboard. |

---

## 12. Open Questions → Decisions

| Question | Decision | Rationale |
|---|---|---|
| **Brain default:** Claude or Kiro? | **User-configured, no default.** `spur init` detects installed agents and lists them. The user picks. Kiro has native ACP; Claude has deeper reasoning. Both are valid. | Removing default avoids privileging one vendor and respects user existing habits. |
| **Workflow sharing registry?** | **Defer.** The real workflow system is plan submission via MCP. Explore pool + git-based community content replace a central registry for v1. | Registry adds operational cost without proven demand. Git-based sharing + Explore is sufficient for v1. |
| **MCP server exposing SPUR tools?** | **Already exists.** `spur-mcp` is the MCP server. Brain agents already call `delegate_to_worker`, `submit_plan`, etc. | This was an open question in v1.0; it is answered in v2.0. |
| **Local model support (Ollama)?** | **Defer.** No ACP-compatible local model CLI exists at quality threshold. When one does, `spur-acp` will support it automatically via the adapter pattern. | Supporting Ollama via raw stdio would require a non-ACP adapter, violating protocol-first principle. |
| **Notification channels beyond Telegram?** | **Telegram only for v1.** Slack/Discord can be added via the same `spur-interactive` host pattern. Telegram was chosen for its forum-topic threading, which maps cleanly to session isolation. | Adding more channels is easy; maintaining them is not. Prove Telegram usage first. |
| **Cost tracking: visibility or governance?** | **Visibility today, governance in v0.6.** Be honest that enforcement is not built. Do not market budget caps until Risk #17 is closed. | Premature governance promises create liability. Observational tracking is still valuable. |
| **Should Explore / Loops lead homepage copy?** | **No — secondary demo beat.** Lead with session resume, isolation, and review. Use Explore/Loops as depth proof after the core story lands. | Orchestrator persona buys durability first; ecosystem and recurrence convert power users. |

---

## 13. Glossary

| Term | Definition |
|---|---|
| **ACP** | Agent Client Protocol — JSON-RPC 2.0 over stdio for SPUR → Agent communication |
| **MCP** | Model Context Protocol — JSON-RPC 2.0 over stdio for Agent → SPUR tool calls |
| **Brain agent** | Primary reasoning agent (Claude Code, Kiro, Codex) that decomposes tasks and delegates |
| **Worker agent** | Agent that receives delegated subtasks and executes in an isolated git worktree |
| **Beads** | Local-first SQLite issue store accessed via `br` CLI; SPUR's durable plan backend |
| **Delegation** | Core unit of work: brain requests → SPUR dispatches → worker executes → review gate → merge/discard |
| **Review gate** | Human-in-the-loop state machine: AwaitingReview → Approved / Rejected / Modified / RetryRequested |
| **OutcomeStore** | Content-addressed storage for delegation results (memory, FS, or git blob backends) |
| **Continuation bridge** | Mechanism that clips delegation outcomes to a bounded envelope and feeds them back to the brain scheduler |
| **Peer mailbox** | Structured worker-to-worker messaging with ledgered delivery and reconciliation |
| **Session attach lock** | `fs4` advisory lock preventing split-brain attachment to the same ACP session from multiple processes |
| **EventFunnel** | Singleton guaranteeing monotonic sequence numbers for all SpurEvents |
| **Lineage projection** | Pure function of event stream producing the executor tree shown in the TUI |
| **Plan Browser** | TUI inventory of sprint plans: list, filter, claim, start/resume (`PlanBrowserView`, `/sprints`) |
| **Plan Inspector** | TUI drill-down for one plan: stage board + task detail (`PlanInspectorView`) |
| **Loop** | Recurring orchestrated work unit with pause/resume/retire lifecycle, browsable in Loop Browser |
| **Explore pool** | Layered catalog of ecosystem skills and agent personas adoptable into a project (`ExploreBrowserView`, `spur explore`) |
| **Insights** | Feature-gated analytics TUI over DuckDB (`InsightsView`; Overview / Timeline / Breakdown / Live) |
| **ViewId** | Enum of navigable TUI surfaces in `crates/spur-tui` |
| **FeatureKey** | Typed entitlement string in `spur-license` (63-key Wave-9 registry); policy decides which tier grants it |
| **FeatureGate** | Wait-free runtime checker (`has` / `quota` / `tier`) over an `ArcSwap` entitlement snapshot |
| **Quota** | Numeric limit separate from feature presence (e.g. Community has parallel-workers key but `max_concurrent_workers = 1`) |
| **@inherit:community** | Policy directive: higher tiers include the full Community feature set before adding upsell keys |

---

*SPUR — drive your agents into coordinated action.*

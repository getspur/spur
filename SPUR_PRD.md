# SPUR — Product Requirements Document

**Version:** 2.0  
**Date:** April 26, 2026  
**Author:** Product Owner, SPUR TUI  
**Status:** Grounded (reconciled against `bd-arch.21`, `bd-arch.23`, and `c75e4586`)

---

## 1. Executive Summary

### Product Vision

SPUR is a Rust-native orchestration layer for AI coding agents. It sits between project management tools and agent CLIs, providing a terminal user interface (TUI), a Telegram bot frontend, and a headless orchestration engine that manages brain–worker delegation, durable plan execution, human-in-the-loop review, and cost tracking. It does not replace Claude Code, Kiro, Codex, or Gemini. It makes them composable, recoverable, and observable.

**One-liner:** *"One brain, many workers, zero lost context."*

### What Exists Today (Grounded)

The codebase is ~100 k lines of Rust across 13 crates. The following are **production-hardened** (not aspirational):

- **Dual-channel protocol architecture**: ACP (SPUR → Agent, JSON-RPC/stdio) and MCP (Agent → SPUR, tool calls)
- **Durable plan reconciler**: Plans survive process restarts in a local SQLite beads store; GitHub is used only for PR creation
- **Review gate state machine**: Human approval/deny/modify/retry with timeout, auto-merge gating, and cancellation
- **Session attach lock**: `fs4` advisory locks prevent split-brain multi-window attachment to the same brain session
- **Continuation bridge**: Delegation outcomes are clipped to a bounded envelope and fed back to the brain scheduler as ordered turns
- **Peer mailbox**: Structured worker-to-worker messaging with ledgered delivery and stranded-message reconciliation (Stage-1, opt-in)
- **Tiered license gating**: Community / Pro / Team / Enterprise policy features with signed Ed25519 policy documents
- **Event-sourced lineage**: Pure projections from an NDJSON event stream; session resume is replay
- **Outcome materialization**: Content-addressed blob storage (memory, FS, git) with measured instrumentation
- **Paste-as-atom**: Multi-line paste safety in the TUI composer

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

This section maps **existing technical capability** (what the code does) to **product-facing value** (what the user experiences). This is the core of the v2.0 revamp: no aspirational features, no forward-dating.

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
| `spur-context` DuckDB analytics | Daily / weekly cost reports via SQL | Aggregated spend analysis without ETL | Production |
| `spur-license` Ed25519 policy + `arc_swap` gates | Community / Pro / Team / Enterprise tiers | Free forever for individuals; pay for team features | Production |
| `spur-bot` Telegram runtime | Mobile/tablet push notifications and review | Step away from the terminal without losing the loop | Production |
| WorktreeManager (`spur-worktree`) | True filesystem isolation per worker | Parallel workers cannot clobber each other's files | Production |

### Second-Order Insight: What This Mapping Reveals

**First-order thinking:** "SPUR is a TUI that runs multiple agents."

**Second-order thinking:** "SPUR is a distributed-systems kernel for agent execution disguised as a TUI." The real differentiators are not the panels or keybindings. They are:

1. **Session resume as replay** — The event-sourced lineage makes SPUR the only agent tool where closing your laptop does not lose context. This matters more than any dashboard widget.
2. **Review gate as durable state** — Most tools treat human review as a UI convenience. SPUR treats it as a first-class state machine with timeout, retry, and merge gating. This turns "human-in-the-loop" from a slogan into an execution guarantee.
3. **Local-first plan durability** — Beads (SQLite via `br` CLI) means plans survive SPUR crashes, OS updates, and network outages. This is resilience that cloud-only competitors cannot match without offline sync.
4. **Dual-channel autonomy** — ACP gives the brain freedom to stream; MCP gives SPUR control over delegation and PM ops. Neither channel is a workaround for the other. This architectural choice is why SPUR can support any ACP-speaking agent without per-agent hacks.

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

SPUR's commercial model is **feature-gated tiers** enforced by signed policy documents. The TUI renders most UI regardless of tier; runtime enforcement is progressive. This section replaces the old phase-based roadmap with what actually exists.

### 4.1 Community (Free)

**Policy features:** `brain_session`, `single_worker`, `worktree_isolation`, `manual_review`, `event_persistence`, `basic_lineage`, `tui_dashboard`, `basic_cost_display`, `basic_notifications`, `local_config`, `mcp_standard_tools`

| Feature | What It Does | Technical Basis |
|---|---|---|
| Brain session | One active brain (Claude/Kiro/Codex) with ReAct trace | `spur-acp` session manager + `spur-core` scheduler |
| Single worker | One worker delegation at a time | Semaphore count = 1 |
| Worktree isolation | Each worker gets a git worktree | `spur-worktree` |
| Manual review | `[A/D/M/R]` review cards in TUI | `spur-core` ReviewSink |
| Basic lineage | Collapsible ASCII tree with phase + cost | `spur-core` ExecutorLineage projection |
| TUI dashboard | Agents tree + activity log + input bar + status bar | `spur-tui` DashboardView |
| Basic cost display | Total cost and elapsed time in status bar | `spur-cost` CostTracker |
| Event persistence | NDJSON event log, 128 MB rotation | `spur-core` EventSink |
| Session metadata | Draft persistence, input history (100 entries) | `spur-tui` SessionMetadataStore |
| MCP standard tools | `delegate_to_worker`, `create_pr`, `get_issue`, `submit_plan` | `spur-mcp` tool catalog |

**Second-order product insight:** Community tier is intentionally generous. The goal is to make SPUR the default terminal companion for any developer using Claude Code or Kiro. Revenue comes from teams that outgrow single-worker, single-brain limits.

### 4.2 Pro

**Policy features (adds to Community):** `parallel_workers`, `auto_review_policies`, `session_resume`, `advanced_cost_analytics`, `custom_worktree_policies`, `custom_notifications`, `extended_retention`, `tui_session_detail`

| Feature | What It Does | Technical Basis |
|---|---|---|
| Parallel workers | Multiple simultaneous worker delegations | Semaphore count > 1 + workers panel |
| Session resume | Attach to existing sessions after restart/close | Session metadata + event replay + attach lock |
| Session picker | Searchable list with pin/archive/rename/copy ID | `spur-tui` SessionPickerView |
| Session detail | Full-screen brain chat with Stream/Artifacts/Attempts/Task/Review tabs | `spur-tui` SessionDetailView + DetailPane |
| Advanced cost analytics | Per-attempt cost breakdown in Attempts tab | `spur-cost` per-executor ingestion |
| Auto-review policies | Hints for auto-approve conditions (partial) | `spur-core` review scoring placeholders |
| Collision modal | Handle `SessionAttachRejected` with holder PID info | `spur-acp` `fs4` lock + `spur-tui` CollisionModal |

**Second-order product insight:** Pro tier sells **resilience and parallelism**. Session resume is the killer feature for Claude Code Max users who hit rate limits — they can switch to Kiro as brain, then resume the original Claude session later. Parallel workers turn SPUR from a sequential tool into a true orchestrator.

### 4.3 Team

**Policy features (adds to Pro):** `pm_integration`, `shared_lineage`, `team_cost_dashboard`, `centralized_config`, `rbac`, `shared_review_queue`, `pm_webhooks`

| Feature | What It Does | Technical Basis |
|---|---|---|
| PM integration | Issue browser with status updates | `spur-pm` BeadsAdapter + IssueBrowserView |
| Shared review queue | Status-bar badge for pending reviews across team | `spur-core` ReviewSink (lineage is already shared via broadcast) |
| Team cost dashboard | *(Planned)* Aggregated team spend view | `spur-context` DuckDB analytics engine is ready; UI pending |
| RBAC | *(Planned)* Role-based access control | Policy entitlement exists; enforcement pending |
| PM webhooks | *(Planned)* Real-time issue sync via webhooks | Policy entitlement exists; implementation pending |

**Second-order product insight:** Team tier is currently **entitlement-heavy, UI-light**. The DuckDB analytics engine (`spur-context`) can already produce daily/weekly reports. The product gap is visualization, not data. This is a deliberate choice: we ship data infrastructure first, then UI, so that early Team customers get SQL-level access immediately.

### 4.4 Enterprise

**Policy features (adds to Team):** `sso_saml`, `audit_logs`, `custom_policies`, `custom_mcp_tools`, `dedicated_support`, `sla_guarantee`

| Feature | Status |
|---|---|
| SSO/SAML | 🚧 Planned — policy entitlement only |
| Audit logs | 🚧 Planned — policy entitlement only |
| Custom policies | 🚧 Partial — license badge + flag summary visible |
| Custom MCP tools | 🚧 Partial — generic tool rendering covers most cases |
| SLA guarantee | 🚧 Planned — policy entitlement only |

**Second-order product insight:** Enterprise tier is currently a **sales enablement** tier. The policy system and Ed25519 signing infrastructure exist, so custom policies can be issued today. The UI for audit logs and SSO is not yet built, but the license system can gate it the moment it ships.

---

## 5. Feature Specifications (Grounded)

### 5.1 Views (What the User Sees)

#### Dashboard (`tui_dashboard` — Community)

| Element | Description |
|---|---|
| Agents tree | Collapsible ASCII tree of executor lineage. `j/k/↑/↓` navigate, `Enter` focuses, `c` toggles collapse, `z` toggles zoom. |
| Activity log | Scrolling system event log (5,000-entry cap). Color-coded by kind. Follow mode with manual override. |
| Detail pane | Tabbed detail for focused executor: Stream, Artifacts, Attempts, Task, Review. *(Pro: full tabs; Community: basic)* |
| Input bar / Composer | Multi-line chat with Emacs/Vim modes, `@`-mentions, `/` slash commands, protected ranges, paste-as-atom. |
| Status bar | Context hints, issue count, running count, pending review count, total cost, elapsed time, license badge, flag summary. |
| Workers panel | *(Pro)* Collapsible inline panel showing active worker delegations. `Alt+D` toggles. |

#### Session Detail (`brain_session` + `tui_session_detail` — Pro)

| Element | Description |
|---|---|
| ReAct trace | Streaming trace: `UserMessage`, `AgentMessage`, `Think`, `Observe`, `Act`, `Delegate`, `Permission`. Coalesces consecutive entries. |
| Markdown streaming | Live markdown with GFM tables, mermaid fence extraction, 50 ms debounced flush, 64 KB safety cap. |
| Mermaid viewer | `Alt+V` opens full-screen mermaid overlay. `[`/`]` cycles diagrams. Requires `markdown` compile-time feature. |
| Input integrations | `@`-mention picker (files, dirs, workers), `/` slash-command picker, `Ctrl+R` history picker. |
| Permission prompts | Inline `[y] allow [n] deny [a] always allow` with countdown timers. |
| Draft persistence | 500 ms debounced save to `SessionMetadataStore`. Force flush at intent boundaries. |

#### Session Picker (`session_resume` — Pro)

| Element | Description |
|---|---|
| Session list | Searchable list with pinned ⭐, archived, brain name, relative time, short ID. Virtual `[+ Start new session]` row. |
| Preselect | `--session <id>` preselects and auto-dispatches resume. Bare `spur tui` preselects last but requires Enter. |
| Search/filter | `/` focuses search bar. Real-time filtering. |
| Pin / archive / rename | `p` pin/unpin, `d` archive, `R` rename. |
| Collision modal | On `SessionAttachRejected`: holder PID, TTY, workdir, `kill <pid>` escape hatch. |
| Draft-loss safety | Confirm-switch banner when unsent draft exists. |

#### Plan Inspector (`advanced_cost_analytics` + `custom_worktree_policies` — Pro)

| Element | Description |
|---|---|
| Plan header | Plan ID, status, progress gauge, awaiting review count, failed count, next action. |
| Stage board | Kanban-style columns per stage. Tasks show status badge, worker link, blocked indicator, dependency hints, retry count. |
| Task detail | Identity, execution (agent, attempt, branch), dependencies, output (summary, diff, mutation ID, superseded-by). |
| Responsive layout | Side-by-side when width ≥ 90, stacked when narrower. |

#### Issue Browser (`pm_integration` — Team)

| Element | Description |
|---|---|
| Issues list | ID, Priority (P0–P4), Type, Status, Assignee, Title. |
| Issue detail | Full body, metadata, labels, URL. |
| Status updates | `o/w/b/d` — open/in_progress/blocked/closed. |
| Work on issue | `W` constructs prompt from issue and sends to brain. |

### 5.2 Composer & Input System

| Feature | Description |
|---|---|
| Multi-line editing | Emacs (default) and Vim (Normal/Insert/Visual/Operator) modes. |
| Protected ranges | Atomic `@mention` and paste-reference tokens. Cursor-skipped, deleted as units. |
| Paste-as-atom | Multi-line pastes become placeholder tokens (`[Paste #N · M lines]`). Side table capped at 50 entries (LRU). Expands on submit. |
| `@`-mentions | Fuzzy picker for files, directories, workers. `MentionRegistry` with 60 s TTL, nucleo scoring, +25 % worker score boost. |
| `/` slash commands | Spur-local (`/clear`, `/mode`, `/sessions`, `/cost`, `/quit`, `/vim`, `/issues`) merged with agent-advertised commands. |
| History picker | `Ctrl+R` fuzzy search over global input history (cap 100). |
| Activity spinner | Per-frame spinner driven by `ActivityKind`. |
| Soft wrap | Unicode-width aware word-boundary wrapping. `TAB_WIDTH = 4`. |

### 5.3 Review System

| Feature | Tier | Description |
|---|---|---|
| Review card | Community | Renders review kind, summary, diff stats, PR URL, error. Action hints: `[A]pprove [D]eny [M]odify [R]etry`. |
| Inline executor cards | Pro | Live executor status inline in brain trace at delegate call sites. Phase-aware density. |
| Review tab | Pro | Dedicated tab in DetailPane with full context and keyboard shortcuts. |
| Shared review queue | Team | Queue badges in status bar for team-wide pending reviews. |

### 5.4 Cost & Telemetry

| Feature | Tier | Description |
|---|---|---|
| Basic cost badge | Community | Total cost and elapsed time in status bar. Per-executor cost in lineage tree. |
| Per-session cost | Pro | Cost tracking per attempt in DetailPane Attempts tab. |
| Team cost aggregates | Team | *(Planned)* Team-wide cost dashboard. |
| Daily/weekly reports | Pro/Team | `spur-context` DuckDB analytics produces reports via SQL. |

**Honest product note:** `spur-cost` is currently **observational, not enforceable**. It records start/end events and computes cost, but the orchestrator spawns sessions without any budget check. This is a known gap (Architecture Risk #17). The product positioning must be "cost visibility," not "cost governance," until enforcement lands.

---

## 6. Architecture at a Glance

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  Frontends                                                                  │
│  ┌─────────────────────────────┐  ┌─────────────────────────────────────┐   │
│  │ TUI (ratatui + crossterm)   │  │ Telegram Bot (spur-bot)             │   │
│  │ Dashboard · SessionDetail   │  │ Forum-topic sessions                │   │
│  │ PlanInspector · Palette     │  │ Inline review buttons               │   │
│  └─────────────────────────────┘  └─────────────────────────────────────┘   │
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

**Decision: Tier boundaries should reflect operational scale, not feature whims.**

- **Community:** Individual developer, one worker, manual review. Enough to fall in love with the workflow.
- **Pro:** Individual power user or small team. Parallel workers, session resume, cost analytics. Sells resilience.
- **Team:** Engineering team with shared project state. PM integration, shared review queue, team cost aggregation. Sells coordination.
- **Enterprise:** Compliance and custom policy. SSO, audit logs, SLA. Sells trust.

**Decision: Be honest about gaps.**

- Cost enforcement is not built. Say so. Do not promise "budget alerts" as a shipping feature.
- Event loss under burst is possible. Document the drain cap.
- Peer mailbox is experimental. Gate it behind config, not marketing.

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

1. **Rust single binary** — `cargo install spur-cli` or `curl | sh`. No Node.js, no Python, no Docker.
2. **Native ACP + MCP dual channel** — Structured protocol support, not PTY scraping or prompt injection.
3. **Local-first durability** — Plans in SQLite (beads), events in NDJSON, outcomes in git blobs. Survives crashes and outages.
4. **Human review as execution gate** — Not a UI afterthought; a state machine that blocks merge until approved.
5. **Session resume via event replay** — Close the terminal, restart SPUR, resume the exact same brain session.
6. **Cross-vendor agent orchestration** — Claude, Kiro, Codex, Gemini, custom ACP agents.
7. **Telegram bot sharing the TUI correctness path** — Same review lane, same event bus, same state machine.

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

### v0.7 — Scale

- [ ] Team cost dashboard UI (Team tier entitlement → implementation)
- [ ] RBAC enforcement (Team tier)
- [ ] PM webhook real-time sync (Team tier)
- [ ] Peer mailbox default-on with ledger pruning (Risk #22)
- [ ] `TraceSource` wired into command palette

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
| Phase 1: Foundation | Agent discovery, basic TUI, ad-hoc tasks | TUI, sessions, composer, review gate, attach lock | **Over-delivered on TUI; under-delivered on agent discovery.** Discovery is less important than session management. |
| Phase 2: Workflow Engine | TOML workflows, rate limit failover, cost tracking | Durable plan reconciler (MCP-based, not TOML), cost tracking (observational), brain failover | **Workflow engine is MCP-native, not TOML-native.** TOML workflows are a legacy concept; real users submit plans via MCP. Cost tracking lacks enforcement. |
| Phase 3: PM Integration | Linear, Plane, GitHub | Beads-primary, GitHub-satellite. Linear/Plane not implemented. | **Correct call.** Local-first beads is more resilient than SaaS PM APIs. GitHub PR creation is sufficient. |
| Phase 4: Commercial | Team dashboard, smart router, unified billing | Tier system exists, feature gates exist, DuckDB analytics exists. Smart router and unified billing do not. | **Smart router was speculative.** Unified billing requires vendor partnerships. Tier gating is the real commercial foundation. |

---

## 12. Open Questions → Decisions

| Question | Decision | Rationale |
|---|---|---|
| **Brain default:** Claude or Kiro? | **User-configured, no default.** `spur init` detects installed agents and lists them. The user picks. Kiro has native ACP; Claude has deeper reasoning. Both are valid. | Removing default avoids privileging one vendor and respects user existing habits. |
| **Workflow sharing registry?** | **Defer.** The real workflow system is plan submission via MCP. Community plan templates can be git repos, not a registry. | Registry adds operational cost without proven demand. Git-based sharing is sufficient for v1. |
| **MCP server exposing SPUR tools?** | **Already exists.** `spur-mcp` is the MCP server. Brain agents already call `delegate_to_worker`, `submit_plan`, etc. | This was an open question in v1.0; it is answered in v2.0. |
| **Local model support (Ollama)?** | **Defer.** No ACP-compatible local model CLI exists at quality threshold. When one does, `spur-acp` will support it automatically via the adapter pattern. | Supporting Ollama via raw stdio would require a non-ACP adapter, violating protocol-first principle. |
| **Notification channels beyond Telegram?** | **Telegram only for v1.** Slack/Discord can be added via the same `spur-interactive` host pattern. Telegram was chosen for its forum-topic threading, which maps cleanly to session isolation. | Adding more channels is easy; maintaining them is not. Prove Telegram usage first. |
| **Cost tracking: visibility or governance?** | **Visibility today, governance in v0.6.** Be honest that enforcement is not built. Do not market budget caps until Risk #17 is closed. | Premature governance promises create liability. Observational tracking is still valuable. |

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

---

*SPUR — drive your agents into coordinated action.*

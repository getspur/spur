# SPUR: Business-Technical Document

> **Version:** 2026-04-30  
> **Classification:** Internal — Product & Engineering Alignment  
> **Source of Truth:** `docs/architecture.md`, `docs/onboarding/community-tier.md`, `docs/superpowers/specs/2026-04-19-community-default-onboarding-design.md`, `crates/spur-license/resources/default_policy.json`

---

## 1. Executive Summary

**SPUR** is a Rust-native, multi-agent orchestration platform that transforms AI-assisted development from a fragmented, single-vendor experience into a **disciplined, multi-agent pipeline** with human-in-the-loop governance. It operates under the principle: *"Issue in, PR out — across every agent."*

| Attribute | Value |
|---|---|
| **Language** | Rust (~110,000 LOC across 13 crates) |
| **Architecture** | Event-driven with event sourcing, dual-channel (ACP + MCP) |
| **Primary Interface** | Terminal UI (`ratatui`), CLI, Telegram Bot |
| **Target Users** | Senior/staff engineers, team leads, AI-heavy engineering teams |
| **Business Model** | Open-core freemium: Community (free) → Pro → Team → Enterprise |
| **Core Value Prop** | Orchestrate 20+ AI agents safely, with cost visibility, git isolation, and review gates |

---

## 2. The Problem SPUR Solves

### 2.1 Pain Points in AI-Assisted Development

The rise of AI coding agents has created a new category of operational pain for engineering teams:

| Pain Point | Description | Impact |
|---|---|---|
| **Agent Sprawl** | Developers juggle 2–5 agent CLIs (Claude Code, Codex, Kiro, Gemini, OpenCode) in separate terminal tabs | Context fragmentation, copy-paste errors, cognitive load |
| **Vendor Lock-in & Rate Limit Fragility** | When Claude Code throttles, all work stops. No automatic failover to alternative agents exists | Downtime during critical tasks, single-point-of-failure |
| **Zero Cost Visibility** | Teams spending $3,000+/month on AI tools have no cross-agent cost tracking | Budget overruns, unaccounted spend, no ROI data |
| **Safety Fears** | Running agents directly on the main branch risks repo contamination | Developers hesitate to use agents for fear of "messing up the repo" |
| **No Native Multi-Agent Delegation** | Claude Code subagents are Claude-only. No mechanism to delegate security tasks to Kiro and refactoring to Codex based on task fit | Suboptimal agent-task matching, lower success rates |
| **Lost Session State** | Agent sessions die with the terminal. No resume, no lineage, no audit trail | Repeated context rebuilding, loss of intermediate reasoning |

### 2.2 The Market Context

- **55% of developers** now use AI agents weekly
- **73% of engineering teams** use AI coding tools daily
- Average senior engineer spends **$200–600/month** on AI coding subscriptions
- Teams running multiple agents lack a **unified control plane**

### 2.3 SPUR's Thesis

> *"Discipline cannot be prompted; it must be compiled."*

SPUR enforces **macro-discipline** (state machines, DAGs, review gates, git isolation) in compiled Rust, while letting LLMs handle **micro-discipline** (tactical coding) within constrained, isolated worker sessions. The result: agents can run autonomously without risking the main codebase, and humans retain control at every critical junction.

---

## 3. The User Journey: What Users Can Do With SPUR

### 3.1 Day 1 — Zero-Friction Onboarding (Community Tier)

```bash
cargo install spur-cli    # or curl | sh
cd your-project
spur init                 # creates .spur/config.toml
spur                      # launches TUI
```

| Step | Experience | Technical Enabler |
|---|---|---|
| **1. Install** | Single binary, no daemon, no Docker | `spur-cli` is a static Rust binary |
| **2. Init** | `spur init` scaffolds `.spur/` with config, skills, and event dirs | `spur-cli/src/commands/init.rs` |
| **3. First Run** | No license key required. Community tier activates silently. Inline paste prompt for Pro upgrade | `CommunityProvider` + `PolicyResolver` with embedded signed policy |
| **4. Landing** | Empty dashboard or session picker preselected on last session | `LandingDecision` — no implicit attach |
| **5. Task Input** | Type a task, press Enter. Brain spawns. View switches to Session Detail | `InteractiveFrontendHost` → `Orchestrator::run_interactive` |
| **6. Live Observation** | Markdown streams live. Status bar shows BrainStatus (Idle/Thinking/Streaming/Ready) + model + cost | `SessionDetailView` with `AgentCaps`-driven status bar |
| **7. Delegation** | Brain may delegate to worker agents. Inline trace entries appear; workers panel shows active executors | MCP `delegate_to_worker` → `DelegationDispatcher` |
| **8. Review Gate** | Worker output pauses at review gate. User presses `a` (approve), `d` (reject), `m` (modify), `R` (retry) | `ReviewSink` + `ReviewHandle` typestate |
| **9. Merge** | Approved worktrees auto-merge. PR can be created via MCP tool | `WorktreeManager::merge` + `create_pr` tool |
| **10. Cost Check** | `spur cost` shows per-session and cumulative spend | `spur-cost` SQLite + `PricingRegistry` |

**Community tier feature set** (from `default_policy.json`):
- 1 concurrent worker (`max_concurrent_workers: 1`)
- Full TUI (dashboard, session detail, plan inspector, issue browser, palette)
- Brain session orchestration with manual review gate
- Session resume from lineage replay
- Worktree isolation per delegation
- Basic cost display and local PM browse
- MCP delegate / PR-creation tools
- 128 MB event retention

### 3.2 Power User — Parallel Execution (Pro Tier)

```bash
spur auth login --key <YOUR-KEY>   # activate Pro
spur                               # 5 concurrent workers, auto-review policies
```

| Capability | Pro Addition |
|---|---|
| **Parallel Workers** | Up to 5 concurrent delegations within one orchestrator |
| **Auto-Review** | Configurable policies (auto-approve safe refactorings, auto-reject test failures) |
| **Per-Project Cost Analytics** | DuckDB-powered trends, CSV export, burn-rate forecasting |
| **Custom Worktree Policies** | Merge strategies, naming conventions, orphan cleanup rules |
| **Telegram Remote Review** | Approve/reject from phone via inline buttons |
| **Extended Retention** | 1 GB event logs |

### 3.3 Team Collaboration (Team Tier)

```bash
# Team lead configures shared beads project
spur config set pm.backend beads
spur config set team.id <TEAM-ID>
```

| Capability | Team Addition |
|---|---|
| **PM Integration** | GitHub/Linear/Plane issue sync — the stickiest feature |
| **Shared Lineage** | Team-wide session visibility and shared review queue |
| **10 Concurrent Workers** | Per seat |
| **Centralized Config + RBAC** | Role-based access control |
| **Team Cost Dashboard** | Cross-member spend visibility |
| **10 GB Retention** | Shared event storage |

### 3.4 Telegram Bot — Mobile Workflow

```bash
spur bot telegram --start   # binds to operator chat
```

- Forum-topic sessions map 1:1 to SPUR brain sessions
- Receive streaming updates, approve/reject via inline buttons
- Send follow-up prompts from phone
- Full review lane without opening terminal

### 3.5 Plan-Driven Automation

```bash
# Brain submits a plan via MCP tool
# Plan is persisted to beads as an epic with child tasks
# Reconciler dispatches ready tasks in DAG order
# Signals (scope drift, blockers) trigger re-planning
```

| Stage | User Action | System Action |
|---|---|---|
| Plan Submit | Brain calls `submit_plan` | Persist to beads epic; label with `spur:plan-id` |
| Task Dispatch | — | Reconciler dispatches ready tasks respecting dependencies |
| Worker Execution | Monitor TUI dashboard | Delegation lifecycle: worktree → spawn → run → review |
| Signal Handling | — | `SignalWatcher` detects scope drift → proposes task split |
| Completion | Review final PR | Auto-merge on approval; audit trail in beads comments |

---

## 4. Technical Architecture

### 4.1 Component Layers (13 Crates)

```
┌─────────────────────────────────────────────────────────────┐
│  ENTRY POINTS                                               │
│  spur-cli          Binary, arg parsing, bootstrap           │
├─────────────────────────────────────────────────────────────┤
│  PRESENTATION                                               │
│  spur-tui          ratatui terminal UI                      │
│  spur-bot          Telegram Bot frontend                    │
├─────────────────────────────────────────────────────────────┤
│  ORCHESTRATION                                              │
│  spur-core         Event pipeline, review loop, lineage     │
│  spur-mcp          MCP server, plan reconciler, persist     │
├─────────────────────────────────────────────────────────────┤
│  PROTOCOL                                                   │
│  spur-acp          ACP client, transports, domain types     │
├─────────────────────────────────────────────────────────────┤
│  SUPPORT SERVICES                                           │
│  spur-pm           Beads (local) + GitHub (satellite)       │
│  spur-cost         SQLite cost tracking + pricing           │
│  spur-worktree     Git worktree lifecycle                   │
│  spur-license      Feature gates, signed policy, Ed25519    │
│  spur-blob-store   Content-addressed outcome storage        │
│  spur-context      DuckDB analytics engine                  │
├─────────────────────────────────────────────────────────────┤
│  SHARED HOST                                                │
│  spur-interactive  Channel wiring, review lane, shutdown    │
└─────────────────────────────────────────────────────────────┘
```

### 4.2 Dual-Channel Architecture

SPUR uses a **dual-channel** design that gives the brain agent autonomy while SPUR retains execution control:

| Channel | Direction | Protocol | Purpose |
|---|---|---|---|
| **ACP** | SPUR → Agent | JSON-RPC over stdio | Session management, prompts, notifications |
| **MCP** | Agent → SPUR | JSON-RPC over HTTP | Tool calls: delegate, create PR, get issue, submit plan |

```
User → TUI → Host → Orchestrator → ACP → Brain Agent
                                      ↑
                                      └ MCP ← Brain calls tools
                                              ↓
                                        MCP Server → Orchestrator → Workers
```

### 4.3 Event Sourcing & Lineage Projection

All state changes flow through the **EventFunnel** — a singleton that stamps monotonic sequence numbers:

```
Emitters → mpsc → Stamp(seq, occurred_at) → broadcast(4096) → Subscribers
                                                        ├── TUI App
                                                        ├── Bot Runtime
                                                        ├── ExecutorLineage
                                                        ├── EventSink (NDJSON)
                                                        ├── PlanProjectionStore
                                                        └── PeerMailbox
```

**Key invariant:** `ExecutorLineage` rebuilds state purely by applying `SpurEvent`s in order. No `SystemTime::now()` inside `apply`. Session resume is deterministic replay.

### 4.4 Delegation Lifecycle

A delegation is the atomic unit of work. It traverses a state machine with human review gates:

```
Requested → SemaphoreWait → WorktreeCreated → WorkerSpawned → WorkerRunning
                                                                   │
                    ┌──────────────────────────────────────────────┤
                    ▼                                              ▼
              ReviewGate ←── WorkerDone                      WorkerFailed
                    │                                              │
        ┌─────┬─────┼─────┐                                       │
        ▼     ▼     ▼     ▼                                       ▼
      Approved Rejected Modified RetryRequested              Cancelled
        │       │       │            │                            │
        └───────┴───────┴────────────┘                            │
                    │                                               │
              MergeWorktree → OutcomePersist → ContinuationBuilt  DiscardWorktree
                    │                                               │
                    └──────────── Completed ────────────────────────┘
```

**Safety mechanisms:**
- **Git worktree isolation** — each worker runs in `refs/heads/spur/worker/v2/<agent>/<session>/<worker>`
- **Review gate** — human approval required before merge (configurable auto-approve for Pro)
- **Cancellation token** — registered before spawn; races with execution in `tokio::select!`
- **Semaphore release** — RAII `SemaphorePermit` guarantees release on every exit path
- **WorktreeAuthority** — lease-aware GC via `fs4` advisory locks; startup + periodic sweep

### 4.5 Plan Reconciler (Durable Execution)

The reconciler provides **durable plan execution** — plans survive process restarts:

1. Brain submits plan → persisted to beads as epic with child tasks
2. Reconciler dispatches ready tasks respecting DAG dependencies
3. Worker signals (scope drift, blockers) trigger re-planning
4. Audit trail written as beads comments with `[[spur-audit v1]]` sentinels
5. Plan completion triggers auto-PR creation

**Key types:** `PlanProjectionStore`, `MutationExecutor`, `SignalWatcher`, `Reconciler`

### 4.6 Peer Mailbox (Inter-Worker Messaging)

Workers communicate via `_spur/peer_message` ext notifications:

- **Router** — accepts/rejects based on scope check + ledger check
- **Ledger** — `InMemoryLedger` tracks `Pending → Injected → Delivered → Consumed|Rejected`
- **Guard** — typestate: inject → ack → finalize
- **Reconciler** — recovers stranded messages after timeout

Production-gated behind `SpurConfig::peer_mailbox_enabled` (default `false`).

### 4.7 Licensing & Feature Gating

SPUR uses a **typed, signed, embedded `PolicyDocument`** as the source of truth for tier entitlements:

```
Embedded JSON (default_policy.json) ──Ed25519 verified──→ PolicyDocument
                                                              ├── tier_policies (G1: entitlements)
                                                              └── flags (G2: runtime toggles)
                                                                   ├── enabled
                                                                   ├── rollout_percent
                                                                   └── tier_filter
```

**Gating contract — FLOOR ∧ GATE:**
```rust
pub fn feature_enabled(license: &SpurLicense, flags: &FlagEvaluator, key: FeatureKey) -> bool {
    license.has_entitlement(key.as_str())      // FLOOR: tier includes this?
        && flags.is_enabled(key.as_str(), ...) // GATE: rollout open to this user?
}
```

**Tiers:**

| Tier | Price | Workers | Key Differentiator |
|---|---|---|---|
| Community | Free | 1 | Full daily-driver workflow, no time limit |
| Pro | $12/mo or $99 lifetime | 5 | Parallel workers, auto-review, cost analytics, Telegram |
| Team | $29–39/seat/mo (min 3) | 10/seat | PM integration, shared lineage, team dashboard |
| Enterprise | Custom annual | Custom | SSO/SAML, audit logs, on-premise, SLA |

---

## 5. Data Model & Storage Architecture

### 5.1 Operational Data (SQLite)

| Database | Purpose | Schema |
|---|---|---|
| `cost.db` | Token usage and spend tracking | `SessionRecord`, `DelegationRecord`, `TokenSummary`, `CostSummary` |
| `beads.db` | Local issue tracker | Issues, comments, labels, dependencies |

### 5.2 Analytical Data (DuckDB)

| Database | Purpose | Schema |
|---|---|---|
| `context.db` | Cross-agent analytics | `DailyReport`, `WeeklyReport`, `BurnRate`, `ModelBreakdown` |

Reads agent JSONL logs in-place via `read_json_auto()` — no ETL pipeline.

### 5.3 Event Logs (NDJSON)

| Location | Purpose | Rotation |
|---|---|---|
| `.spur/events/` | Replayable event stream | 128 MB per file, 8 MB default cap with GC |

### 5.4 Outcome Storage (Git Blobs)

| Backend | Use Case | Keying |
|---|---|---|
| `GitBlobOutcomeStore` | Production | Content-addressed: `refs/spur/outcomes/...` |
| `FsOutcomeStore` | Local dev | Filesystem |
| `MemoryOutcomeStore` | Tests | In-memory |

**Store-then-clip pattern:** Full `DelegationResult` stored; `OutcomeMaterializer` clips to `MERGE_BUDGET` (8 KB) before building `BrainContinuation`.

---

## 6. Integration Ecosystem

### 6.1 Agent Adapters (ACP-Compatible)

| Agent | Transport | Native ACP | Cost Model | Best For |
|---|---|---|---|---|
| **Kiro CLI** | Native ACP | ✓ | Own credits | Spec-driven tasks |
| **Gemini CLI** | stdio | — | Free (1K req/day) | Long context, budget work |
| **OpenCode** | stdio | — | BYOK | Large user base (6.5M) |
| **Claude Code** | cli-wrap | — | $200/mo Max | Complex reasoning |
| **Codex CLI** | stdio | — | OpenAI API | Fast targeted edits |
| **Goose** | stdio | — | BYOK | General purpose |
| **Kimi CLI** | stdio | — | BYOK | Chinese market |
| **Cursor** | stdio | — | Subscription | IDE integration |

### 6.2 Project Management

| Backend | Role | Integration |
|---|---|---|
| **Beads** (`br` CLI) | Primary | Local SQLite; issue CRUD, dependency graphs, plan persistence |
| **GitHub** (`gh` CLI) | Satellite | PR creation, issue sync |
| **Linear / Plane** | Planned | Roadmap item |

### 6.3 Communication

| Channel | Crate | Library |
|---|---|---|
| Terminal TUI | `spur-tui` | `ratatui` |
| Telegram Bot | `spur-bot` | `frankenstein` |
| MCP Tools | `spur-mcp` | `rmcp` (streamable HTTP) |

---

## 7. Security & Safety Model

### 7.1 Git Isolation

- Every worker operates in a **dedicated git worktree**
- Main branch is never touched by workers
- Merges only occur after human review or auto-approve policy
- `WorktreeAuthority` prevents orphan accumulation via lease-aware GC

### 7.2 Session Single-Attach Lock

- `fs4` advisory lockfile prevents split-brain multi-window attachment
- Kernel-auto-released on process exit
- NFS/sshfs degrades gracefully to `fs_unsafe` with persistent banner

### 7.3 Policy Signature Verification

- Embedded `default_policy.json` is Ed25519-signed
- Verified at **compile time** (`build.rs`) and **runtime**
- Runtime overlay (`~/.spur/policy-overlay.json`) is also signature-verified; fail-closed on tamper

### 7.4 Cost Governance (Observational → Active)

- **Current:** `spur-cost` is passive logging — tracks, reports, but does not enforce
- **Planned (v1.0):** Cost circuit breaker with `max_spend_per_session` and `max_spend_per_plan`

---

## 8. Business Model & Pricing Rationale

### 8.1 Why Open-Core?

SPUR's Community tier is **not a time-limited trial** — it is a permanent free tier with a complete daily-driver workflow. This design choice is deliberate:

| Decision | Rationale |
|---|---|
| **Cost tracking is free** | Prevents bill shock and negative word-of-mouth. Users trust SPUR with their spend data. |
| **PM integration is Team-only** | Prevents painful reconfiguration when upgrading from individual to team. Creates stickiness at Team tier. |
| **No time-based trial** | Users upgrade when they hit friction (need parallel workers), not when a timer expires. Higher conversion quality. |
| **Lifetime deal for Pro** | $99 one-time captures price-sensitive power users; subscription captures ongoing team value. |

### 8.2 Conversion Funnel

```
Install (cargo install) → Community default (no key) → Demo key taste (DEMO-SPUR-2026-Q2)
    → Hit 1-worker limit → Upgrade to Pro ($12/mo or $99 lifetime)
    → Need team PM integration → Upgrade to Team ($29-39/seat)
    → Need SSO/audit → Enterprise (custom)
```

### 8.3 Revenue Targets

| Milestone | Target | Timeline |
|---|---|---|
| Pro launch | $2K MRR | Month 6 |
| Team launch | $4.8K MRR | Month 9–12 |
| v2.0 | $100K ARR | Month 18+ |

---

## 9. Development & Operational Practices

### 9.1 Build System

- **Custom wrapper:** `scripts/spur-cargo` (sccache worktree sync for cross-worktree cache reuse)
- **Tests:** `cargo test --workspace` — unit + integration across all crates
- **Linting:** `cargo clippy --workspace -- -D warnings`

### 9.2 Risk Management (MCTS Framework)

SPUR uses a **Monte Carlo Tree Search** risk framework grounded in first-principles axioms:

| Axiom | Statement |
|---|---|
| R1 | Resource Finiteness — every buffer, queue, and cache has a bound |
| R2 | Failure Inevitability — disks fill, networks partition, processes crash |
| R3 | Observability Requires Explicitness — silent catch-alls are deliberate blindness |
| R4 | Synchronization Requires Consensus — no exclusive access without visible coordination |
| R5 | Backpressure Propagates or Drops — no third option when producer outpaces consumer |
| R6 | State Machines Must Be Closed — every state needs defined transitions for every input |
| R7 | Time is a Resource — every await consumes unbounded time unless bounded |

**Current Systemic Health Score:** 0.20 / 1.0 — highest priority: observability bridge fixes (eliminate silent catch-alls, NDJSON replay on `Lagged`).

### 9.3 Plan-Driven Workflow

Non-trivial work flows: **Spec → Plan → Implementation**

- Specs live in `docs/superpowers/specs/`
- Plans live in `docs/superpowers/plans/`
- Invariants are numbered and tested (e.g., invariant #9: FLOOR ∧ GATE gating contract)

### 9.4 Signal Conventions

Workers emit structured signals (e.g., `scope_drift`, `cost_spike`) as sentinel-fenced JSON inside beads comments. The brain parses these to re-plan:

```
[[spur-signal v1]]
{
  "signal_id": "<uuid>",
  "kind": "scope_drift",
  "severity": 0.82,
  "reason": "auth refactor pulls in 4 new subsystems",
  "estimated_subtasks": 3
}
```

---

## 10. Roadmap

### Near-Term (v0.4.x — Months 4–6)
- [x] Community default onboarding (no-key first run)
- [x] Day-1 feature flags (`FlagEvaluator`, policy overlay)
- [x] Peer mailbox production wire-up
- [x] Session picker recall revamp
- [ ] Telegram gateway GA
- [ ] Linear integration
- [ ] Plan inspector DAG UI
- [ ] Cost circuit breaker (budget enforcement)

### Medium-Term (v1.0 — Months 9–12)
- [ ] Team dashboard + smart routing
- [ ] Team tier launch ($4.8K MRR target)
- [ ] Worker heartbeat watchdog (default-on)
- [ ] NDJSON live `Lagged` recovery
- [ ] Orchestrator decomposition (BrainSessionManager, DelegationDispatcher actors)
- [ ] OpenFeature migration (if SaaS flag management adopted)

### Long-Term (v2.0 — Month 18+)
- [ ] **Agent Performance Report** — "J.D. Power of AI coding agents"
- [ ] Enterprise: self-hosted, SAML, audit logs
- [ ] $100K ARR target
- [ ] Industry benchmark authority

---

## 11. Key Metrics & Success Criteria

| Metric | Target | Measurement |
|---|---|---|
| Time-to-first-delegation | < 2 min | Telemetry from `spur init` to first `DelegationRequested` |
| Community → Pro conversion | > 8% | License state transitions |
| Pro → Team conversion | > 15% | Team invite acceptance rate |
| Session resume success | > 95% | `SessionAttachGuard` acquire rate |
| Worker merge success | > 90% | `DelegationCompleted(Success)` / total |
| Cost tracking accuracy | ±5% | Compare `spur cost` to vendor invoices |
| Systemic Health Score | > 0.70 | MCTS evaluation monthly |

---

## 12. Conclusion

SPUR is a **production-grade multi-agent orchestration system** that solves the fragmentation, safety, and visibility problems of AI-assisted development. By combining:

1. **Event-sourced architecture** for deterministic state replay
2. **Git worktree isolation** for safe parallel execution
3. **Human-in-the-loop review gates** for quality control
4. **Dual-channel ACP+MCP** for brain autonomy + SPUR control
5. **Signed policy documents** for clean tier-based feature gating
6. **DuckDB analytics** for cross-agent cost visibility

SPUR enables engineering teams to move from *"one agent, one terminal, one vendor"* to *"issue in, PR out — across every agent."*

---

*Document generated from codebase analysis on 2026-04-30. Source of truth for licensing: `crates/spur-license/resources/default_policy.json`. Source of truth for architecture: `docs/architecture.md`.*

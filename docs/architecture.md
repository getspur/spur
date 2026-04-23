# SPUR Architecture

> Reviewed 2026-04-23. Covers all 11 workspace crates, ~75k lines of Rust.

## 1. Component Architecture

Eleven crates with strict layering. No dependency cycles.

```mermaid
graph TB
    subgraph "Entry Points"
        CLI["spur-cli<br/><i>Binary, arg parsing,<br/>bootstrap</i>"]
    end

    subgraph "Presentation"
        TUI["spur-tui<br/><i>ratatui terminal UI<br/>Dashboard · SessionDetail · Picker<br/>PlanInspector · Palette · Composer</i>"]
        BOT["spur-bot<br/><i>Telegram Bot frontend<br/>Forum-topic sessions<br/>Thread registry · Runtime state machine</i>"]
    end

    subgraph "Orchestration"
        CORE["spur-core<br/><i>Orchestrator engine<br/>Event pipeline · Review loop<br/>Lineage projection · Skills system<br/>Brain scheduler · Continuation bridge</i>"]
        MCP["spur-mcp<br/><i>MCP server<br/>Brain→SPUR tool bridge<br/>Persisted plan reconciler</i>"]
    end

    subgraph "Protocol"
        ACP["spur-acp<br/><i>ACP client · Transports<br/>Config · Domain types<br/>Agent adapters</i>"]
    end

    subgraph "Support Services"
        PM["spur-pm<br/><i>Beads (local-first) · GitHub (PR satellite)<br/>Issue/PR adapters via br/bv CLI</i>"]
        COST["spur-cost<br/><i>SQLite cost tracking<br/>Per-session · per-project</i>"]
        WT["spur-worktree<br/><i>Git worktree lifecycle<br/>Create · diff · merge · cleanup</i>"]
        LICENSE["spur-license<br/><i>License facade<br/>Feature gates · Quotas<br/>Signed policy · Ed25519</i>"]
    end

    subgraph "Shared Host"
        INTERACTIVE["spur-interactive<br/><i>Shared frontend host<br/>Channel wiring · Review lane<br/>Shutdown orchestration</i>"]
    end

    CLI --> CORE
    CLI --> TUI
    CLI --> BOT
    CLI --> ACP
    TUI --> CORE
    TUI --> ACP
    TUI --> LICENSE
    BOT --> INTERACTIVE
    CORE --> ACP
    CORE --> PM
    CORE --> COST
    CORE --> WT
    CORE --> LICENSE
    MCP --> ACP
    MCP --> CORE
    MCP --> PM
    MCP --> COST
    INTERACTIVE --> CORE
    PM --> ACP
    COST --> ACP
```

### Crate Responsibilities

| Crate | Lines | Role | Key Types |
|---|---|---|---|
| `spur-acp` | ~7.5k | Protocol foundation | `AgentConnection`, `SpurEvent`, `SpurEventBody`, `DelegationStatus`, `SpurConfig` |
| `spur-core` | ~11k | Orchestration engine | `Orchestrator`, `BrainScheduler`, `ContinuationBridge`, `EventFunnel`, `ReviewSink`, `ExecutorLineage`, `SkillRegistry` |
| `spur-mcp` | ~14.8k | Brain→SPUR bridge + durable plans | MCP `Server`, `Reconciler`, `PlanProjectionStore`, `MutationExecutor` |
| `spur-tui` | ~31k | Terminal interface | `App`, `DashboardView`, `SessionDetailView`, `PlanInspectorView`, `PaletteOverlay` |
| `spur-cli` | ~2k | Binary entry point | CLI args, `tui` command, `bot telegram`, `profile` |
| `spur-pm` | ~2.9k | PM integration — **beads-primary, GitHub-satellite** | `PmService`, `BeadsAdapter` (shells to `br`), `GitHubAdapter` (shells to `gh`), `BeadsAdvanced`, `BvAdapter` |
| `spur-cost` | ~0.5k | Cost tracking | `CostTracker`, SQLite schema |
| `spur-worktree` | ~1k | Git isolation | `WorktreeManager`, `MergeResult`, `ArtifactResolver` |
| `spur-license` | ~2.6k | Licensing & feature gates | `SpurLicense`, `FeatureGate`, `PolicyResolver`, `LicenseProvider` |
| `spur-bot` | ~1.9k | Telegram frontend | `BotRuntime`, `ThreadRegistry`, `TelegramClient`, `RuntimeRender` |
| `spur-interactive` | ~0.1k | Shared host glue | `InteractiveFrontendHost`, `InteractiveFrontendHandle`, `ReviewSubmission` |

---

## 2. Data Flow

The system uses a **dual-channel architecture**: ACP (SPUR→Agent) and MCP (Agent→SPUR). Events flow outward via broadcast; commands flow inward via mpsc. A new **interactive host layer** lets both TUI and Telegram share one correctness path.

```mermaid
flowchart LR
    subgraph USER["User"]
        KB[Keyboard Input]
        TG[Telegram Chat]
    end

    subgraph TUI_LAYER["spur-tui"]
        APP[App<br/>Event Loop]
        DASH[Dashboard]
        DETAIL[SessionDetail]
        LINEAGE_VIEW[Lineage<br/>Projection]
        PALETTE[Palette<br/>Ctrl+K]
        PLAN_INSP[PlanInspector]
    end

    subgraph BOT_LAYER["spur-bot"]
        POLL[Poll Loop]
        ROUTER[Update Router]
        RUNTIME[BotRuntime]
        SENDER[TelegramSender]
    end

    subgraph HOST_LAYER["spur-interactive"]
        HOST[InteractiveFrontendHost]
        HANDLE[InteractiveFrontendHandle]
        REV_LANE[Review Lane]
    end

    subgraph CORE_LAYER["spur-core"]
        ORCH[Orchestrator]
        SCHED[BrainScheduler]
        FUNNEL[EventFunnel<br/><i>seq stamping</i>]
        SINK[EventSink<br/><i>NDJSON persistence</i>]
        REVIEW[ReviewSink]
        LINEAGE[ExecutorLineage<br/><i>event-sourced state</i>]
        NPUMP[NotificationPump]
        BRIDGE[ContinuationBridge]
        SKILLS[SkillRegistry]
    end

    subgraph MCP_LAYER["spur-mcp"]
        MCPS[MCP Server<br/><i>Tool dispatch</i>]
        RECON[Reconciler<br/><i>Durable plans</i>]
        SIGW[SignalWatcher]
    end

    subgraph AGENTS["External Agents"]
        BRAIN["Brain Agent<br/><i>Claude Code / Kiro / Codex</i>"]
        W1["Worker 1<br/><i>in worktree</i>"]
        W2["Worker 2<br/><i>in worktree</i>"]
    end

    subgraph SERVICES["Support"]
        PM_SVC[spur-pm<br/>Beads local SQLite via br CLI<br/>GitHub API for PRs only]
        COST_SVC[spur-cost<br/>SQLite]
        WT_SVC[spur-worktree<br/>Git]
        LIC_SVC[spur-license<br/>FeatureGate]
    end

    %% User → TUI → Host → Orchestrator (commands)
    KB -->|"keypress"| APP
    APP -->|"mpsc(UserInput)"| HOST
    HOST -->|"mpsc(InteractiveInput)"| ORCH
    TG -->|"update"| POLL
    POLL -->|"batch"| ROUTER
    ROUTER -->|"TelegramInput"| RUNTIME
    RUNTIME -->|"send_command"| HANDLE
    HANDLE -->|"mpsc(InteractiveInput)"| ORCH
    HANDLE -->|"mpsc(SubmitReview)"| REV_LANE
    REV_LANE -->|"forward"| REVIEW

    %% Orchestrator → Brain (ACP)
    ORCH -->|"ACP: prompt()"| BRAIN
    BRAIN -->|"ACP: notifications"| ORCH
    ORCH -->|"skills"| SKILLS

    %% Brain → MCP → Orchestrator (tool calls)
    BRAIN -->|"MCP: delegate_to_worker"| MCPS
    BRAIN -->|"MCP: create_pr / get_issue"| MCPS
    BRAIN -->|"MCP: submit_plan / execute_epic"| MCPS
    MCPS -->|"DelegationRequest"| ORCH
    MCPS -->|"persist dispatch"| RECON
    RECON -->|"dispatch wake"| ORCH
    SIGW -->|"mutation proposal"| RECON

    %% Orchestrator → Workers (ACP)
    ORCH -->|"ACP: new_session + prompt"| W1
    ORCH -->|"ACP: new_session + prompt"| W2
    W1 -->|"ACP: notifications"| ORCH
    W2 -->|"ACP: notifications"| ORCH

    %% Event pipeline (broadcast)
    ORCH -->|"emit(body)"| FUNNEL
    FUNNEL -->|"broadcast"| APP
    FUNNEL -->|"broadcast"| RUNTIME
    FUNNEL -->|"broadcast"| SINK
    FUNNEL -->|"broadcast"| LINEAGE

    %% TUI rendering
    LINEAGE --> LINEAGE_VIEW
    LINEAGE_VIEW --> DASH
    LINEAGE_VIEW --> DETAIL
    LINEAGE_VIEW --> PLAN_INSP

    %% Review loop
    ORCH --> REVIEW
    REVIEW -->|"review card"| APP
    REVIEW -->|"inline buttons"| RUNTIME
    APP -->|"SubmitReview"| HOST
    RUNTIME -->|"callback → decision"| HANDLE

    %% Support services
    ORCH --> WT_SVC
    ORCH --> COST_SVC
    ORCH --> LIC_SVC
    MCPS -->|"plan CRUD + audit"| PM_SVC
    MCPS -->|"PR creation only"| PM_SVC

    %% Continuation bridge
    ORCH -.->|"detached completion"| BRIDGE
    BRIDGE -->|"Continuation → scheduler"| SCHED
    SCHED -->|"ordered turns"| ORCH

    style BRAIN fill:#e94560,stroke:#e94560,color:#fff
    style W1 fill:#533483,stroke:#533483,color:#fff
    style W2 fill:#533483,stroke:#533483,color:#fff
    style FUNNEL fill:#0f3460,stroke:#0f3460,color:#fff
    style HOST fill:#1a1a2e,stroke:#e94560,color:#fff
    style RECON fill:#533483,stroke:#533483,color:#fff
```

### Channel Summary

| Channel | Type | Direction | Purpose |
|---|---|---|---|
| `mpsc(UserInput)` | Tokio mpsc | TUI → Host | User commands, palette dispatch |
| `mpsc(InteractiveInput)` | Tokio mpsc | Host → Orchestrator | Unified command vocabulary (message, resume, cancel) |
| `mpsc(SubmitReview)` | Tokio mpsc | Host → ReviewSink | Dedicated review lane (no head-of-line blocking) |
| `broadcast(SpurEvent)` | Tokio broadcast | Orchestrator → All | Event fan-out (TUI, bot, sink, lineage) |
| `mpsc(PermissionRequest)` | Tokio mpsc | Orchestrator → Frontend | One-shot permission prompts |
| ACP (JSON-RPC/stdio) | Agent Client Protocol | SPUR ↔ Agents | Session management, prompts, notifications |
| MCP (JSON-RPC/stdio) | Model Context Protocol | Brain → SPUR | Delegation, PM ops, plan submission |
| `mpsc(DelegationRequest)` | Tokio mpsc | MCP Server → Orchestrator | Delegation dispatch |
| `oneshot(DelegationResult)` | Tokio oneshot | Orchestrator → MCP Server | Delegation response |
| `broadcast(LicenseEvent)` | Tokio broadcast | License runtime → All | License state changes |

---

## 3. Delegation Lifecycle

A delegation is the core unit of work. It flows through a state machine with review gates, retry loops, and cancellation.

```mermaid
stateDiagram-v2
    [*] --> Requested: Brain calls delegate_to_worker

    Requested --> SemaphoreWait: DelegationRequested event
    SemaphoreWait --> WorktreeCreated: Semaphore acquired

    WorktreeCreated --> WorkerSpawned: Git worktree ready
    WorkerSpawned --> WorkerRunning: ACP new_session + prompt

    WorkerRunning --> WorkerDone: Worker completes
    WorkerRunning --> WorkerFailed: Worker error / timeout
    WorkerRunning --> Cancelled: User / brain calls cancel_delegation

    WorkerDone --> ReviewGate: Review required
    WorkerDone --> AutoApproved: No review needed
    WorkerFailed --> ReviewGate: Failure review

    state ReviewGate {
        [*] --> AwaitingReview: ExecutorReviewRequested
        AwaitingReview --> Approved: User presses 'a'
        AwaitingReview --> Rejected: User presses 'd'
        AwaitingReview --> Modified: User presses 'm'
        AwaitingReview --> RetryRequested: User presses 'R'
        AwaitingReview --> TimedOut: Review timeout
    }

    AutoApproved --> MergeWorktree
    Approved --> MergeWorktree
    Modified --> MergeWorktree

    Rejected --> DiscardWorktree
    TimedOut --> FallbackAction: Configurable fallback
    Cancelled --> DiscardWorktree

    RetryRequested --> WorkerSpawned: New attempt\n(bump attempt_n,\nfresh session,\nretry context)

    MergeWorktree --> Completed: DelegationCompleted(Success)
    DiscardWorktree --> Completed: DelegationCompleted(Rejected)
    FallbackAction --> Completed: DelegationCompleted(TimedOut)
    WorkerFailed --> Completed: DelegationCompleted(Failed)
    Cancelled --> Completed: DelegationCompleted(Cancelled)
```

### Key Invariants

- Every delegation emits exactly one `DelegationCompleted` event (via `finalize`)
- Retry loops produce new `Attempt` entries under the same `ExecutorId`
- Stream buffer is cleared on retry start
- Worktree is cleaned up on every terminal path (merge, preserve, or discard)
- Semaphore is released on every exit path (RAII via `SemaphorePermit`)
- Cancellation token is registered before spawn and races with worker execution (`INV-6`)
- ReviewHandle typestate enforces register-before-emit (`INV-4`)

---

## 4. Event Bus Topology

All state changes flow through the EventFunnel — a singleton that guarantees monotonic sequence numbers.

```mermaid
flowchart TB
    subgraph EMITTERS["Event Emitters"]
        O_BRAIN["Brain lifecycle<br/><i>Spawned · Error · Reconnect · Retired</i>"]
        O_DELEG["Delegation lifecycle<br/><i>Requested · Completed · Cancelled</i>"]
        O_WORKER["Worker lifecycle<br/><i>Spawned · Notification · Progress</i>"]
        O_REVIEW["Review lifecycle<br/><i>Requested · Resolved · Cancelled</i>"]
        O_PLAN["Plan lifecycle<br/><i>Snapshot · Completed · ReadyToMerge</i>"]
        O_SYSTEM["System events<br/><i>Cost · Conflict · RateLimit · License</i>"]
    end

    subgraph FUNNEL["EventFunnel (singleton)"]
        MPSC["mpsc::unbounded"]
        STAMP["Stamp seq + occurred_at"]
        BC["broadcast::channel(512)"]
    end

    subgraph SUBSCRIBERS["Subscribers"]
        S_TUI["TUI App<br/><i>NotificationDrain<br/>≤8 events/frame</i>"]
        S_BOT["Bot Runtime<br/><i>SpurEvent → RuntimeRender</i>"]
        S_LINEAGE["ExecutorLineage<br/><i>Pure projection<br/>HashMap&lt;ExecutorId, Node&gt;</i>"]
        S_SINK["EventSink<br/><i>NDJSON files<br/>128MB rotation</i>"]
        S_DASH["Dashboard<br/><i>Activity log entries</i>"]
        S_PLAN["PlanProjectionStore<br/><i>Cache + snapshot</i>"]
    end

    O_BRAIN --> MPSC
    O_DELEG --> MPSC
    O_WORKER --> MPSC
    O_REVIEW --> MPSC
    O_PLAN --> MPSC
    O_SYSTEM --> MPSC

    MPSC --> STAMP
    STAMP --> BC

    BC --> S_TUI
    BC --> S_BOT
    BC --> S_LINEAGE
    BC --> S_SINK
    BC --> S_PLAN

    S_TUI --> S_DASH
    S_LINEAGE -->|"read by"| S_TUI
    S_PLAN -->|"read by"| S_TUI

    style STAMP fill:#e94560,stroke:#e94560,color:#fff
    style BC fill:#0f3460,stroke:#0f3460,color:#fff
```

### SpurEventBody Variants (~30+)

| Category | Variants |
|---|---|
| Brain lifecycle | `BrainSpawned`, `BrainError`, `BrainFailover`, `BrainReconnecting`, `BrainReconnected`, `BrainReconnectFailed`, `BrainRetired` |
| Session lifecycle | `SessionCompleted`, `TurnComplete`, `AgentNotification` |
| Delegation | `DelegationRequested`, `DelegationCompleted` |
| Worker | `WorkerSpawned`, `WorkerNotification`, `WorkerProgress`, `WorkerFileTouched`, `WorkerHeartbeat` |
| Executor state | `ExecutorPhaseChanged`, `ExecutorRetryStarted`, `ExecutorArtifact` |
| Review | `ExecutorReviewRequested`, `ExecutorReviewResolved`, `ExecutorReviewCancelled` |
| Plan | `PlanSnapshotUpdated`, `PlanCompleted`, `PlanReadyToMerge` |
| System | `CostUpdate`, `ConflictDetected`, `RateLimitDetected`, `LicenseUpdated` |
| PM | `IssueReceived`, `PrCreated`, `IssueUpdated` |

---

## 5. Architectural Assessment

### Strengths

- **Event sourcing** — lineage projection is a pure function of the event stream; session resume is replay
- **Dual-channel (ACP+MCP)** — brain has autonomy (calls tools) while SPUR controls execution
- **Clean crate layering** — no dependency cycles, spur-acp is the foundation, spur-worktree is a leaf
- **Review gate as first-class state machine** — human-in-the-loop with timeout, retry, fallback, cancellation
- **Worktree isolation** — true filesystem isolation for parallel workers
- **Shared interactive host** — `spur-interactive` eliminates bootstrap duplication between TUI and Telegram bot
- **Skills system** — 17 bundled skills render per-agent (Claude Code, Codex, Gemini, Kiro, Cursor, OpenCode, Kimi) with role gating and atomic SPUR-MANAGED installation
- **Durable plan reconciler** — persisted plans survive process restarts in local beads (SQLite via `br` CLI); beads comments serve as the audit log. GitHub is used only for PR creation.
- **`br` CLI boundary** — `spur-pm` shells to the external `br` binary for all database operations, creating an anti-corruption layer between SPUR's orchestration semantics and the beads schema evolution
- **License feature gates** — wait-free `arc_swap` entitlement checks with signed policy documents and Ed25519 verification

### Known Risks

| Risk | Severity | Status | Location |
|---|---|---|---|
| Stranded executors — early-exit paths bypass `finalize()` | ~~Critical~~ | **Fixed** — `DelegationGuard` RAII emits `DelegationCompleted(Failed)` on Drop | `orchestrator.rs` |
| broadcast::channel drops events when subscribers are slow | ~~High~~ | **Mitigated** — drain cap raised 8→64 (1920 events/sec at 30fps) | `app.rs` |
| Silent `_ => {}` catch-alls on SpurEventBody matches | ~~High~~ | **Fixed** — explicit arms for all variants; reconnect events now update brain status | `app.rs` |
| Worktree orphaning on unclean shutdown | ~~High~~ | **Fixed** — `cleanup_orphans()` discovers and removes stale `spur/` worktrees on startup | `manager.rs` |
| No backoff on delegation retry loops | ~~High~~ | **Fixed** — exponential backoff 1s→30s cap between retries | `orchestrator.rs` |
| Worker JoinHandles not tracked for shutdown abort | ~~Medium~~ | **Fixed** — `TaskTracker` replaces fire-and-forget `tokio::spawn` for collector JoinHandles | `orchestrator.rs` |
| Orchestrator is a 4400-line God Object | **High** | **Partially addressed** — BrainScheduler, ContinuationBridge, EventFunnel, EventSink, ReviewSink, RetryLoop, LicenseRuntime, PlanProjectionStore, and Lineage modules extracted. Remaining: delegation dispatch and review coordination still inline. | `orchestrator.rs` |
| Single-process — no fault isolation between brain and workers | Low (v0.1) | **Open** — worker panic could take down orchestrator | Architecture-level |
| Broadcast Lagged recovery not implemented | Low | **Open** — Lagged events are logged but lineage is not rebuilt from NDJSON. Runtime state (lineage, sessions, pending continuations) is ephemeral; only plan structural state is durable in beads. | `app.rs` event loop |
| `delegate_async` MCP tool drops `delegation_plan` from schema | ~~Low~~ | **Fixed** — schema now matches `delegate_to_worker`; `delegate_async` deprecated and removed | `tools.rs` |
| No MCP tool to cancel a running delegation | ~~Low~~ | **Fixed** — `cancel_delegation` tool sends sentinel; orchestrator handler wired | `server.rs`, `tools.rs`, `orchestrator.rs` |
| Telegram bot callback expiry after rebind | **Medium** | **Fixed** — prompt tokens capture `live_session` at creation; stale callbacks return "expired" | `runtime.rs` |
| Persisted plan halt on startup (orphan recovery gap) | **High** | **Fixed** — `recover_persisted_plans()` compensates dispatch orphans on startup | `plan/reconciler.rs` |
| Reconciler wake path lost on restart | **High** | **Fixed** — journal append monitor + default wake path restore startup dispatch | `plan/reconciler.rs` |
| Lazy history loading causes stale session metadata | **Medium** | **Fixed** — lazy loading gated by exact pending target; superseded resumes evicted | `runtime.rs` |
| Skill installer overwrites user-edited files | **Medium** | **Mitigated** — SPUR-MANAGED marker + SHA-256 hash detect user edits; `Skip(UserEdited)` outcome | `skills/installer.rs` |
| Cost tracking has no budget enforcement | **High** | **Open** — `spur-cost` logs to SQLite but no consumer throttles behavior on budget exceed. Runaway retry loops or autonomous mutations can burn API budget unchecked. | `spur-cost/src/lib.rs` |
| beads-advanced features tied to beads backend | **Medium** | **Open** — Plan projection, mutation execution, signal watching, and auto-merge require `BeadsAdvanced` which returns `None` for GitHub-only backends. Reconciler degrades to basic dispatch without beads. | `spur-pm/src/service.rs` |
| Reconciler journal wake file not yet active | **Low** | **Open** — `monitor_journal_appends()` polls `.beads/journal` every 250ms, but the file is not produced by `br` today. Falls back to base-interval polling (3s→30s). | `plan/reconciler.rs` |

### Remaining Work (prioritized)

1. **Orchestrator decomposition** — Split the remaining inline delegation dispatch and review coordination into actors:
   ```
   Orchestrator (coordinator)
     ├── BrainSessionManager (spawn, reconnect, failover, retire)
     ├── DelegationDispatcher (semaphore, worker spawn, worktree, cancellation)
     ├── ReviewCoordinator (sink, timeout, retry, auto-merge)
     └── EventPipeline (funnel, sink, broadcast)
   ```

2. **Lagged event recovery** — When `broadcast::Receiver` returns `Lagged(n)`, replay from the NDJSON event log to rebuild the lineage projection from scratch.

3. **MCP signal automation** — SignalWatcher currently proposes `ScopeDrift` splits. Next: integrate `MutationScorer` heuristics, add `CostSpike` and `QualityRegression` signal kinds.

4. **Bot multi-chat support** — Current `spur-bot` binds one operator chat. Future: team-wide bot with RBAC and per-topic permission gates.

5. **Trace source in palette** — `TraceSource` placeholder exists; wire `ReactTrace` entries into palette search.

6. **Runtime state durability** — Separate ephemeral runtime state (lineage, sessions, continuations) from durable plan state. Add checkpoint/restore for orchestrator runtime state, or document the explicit ephemeral boundary.

7. **Cost governance** — Convert `spur-cost` from passive logging to an active budget gate: per-session caps, per-plan ceilings, circuit breakers for anomalous spend.

### Decomposition Recommendation (for v1.0+)

The orchestrator should be split into actors:
```
Orchestrator (coordinator)
  ├── BrainSessionManager (spawn, reconnect, failover, retire, scheduler)
  ├── DelegationDispatcher (semaphore, worker spawn, worktree, cancel token registry)
  ├── ReviewCoordinator (sink, timeout, retry, auto-merge gating)
  ├── EventPipeline (funnel, sink, broadcast)
  └── PlanRuntime (projection store, reconciler bridge, mutation executor)
```

Each actor communicates via typed message channels, enabling independent testing and future distribution.

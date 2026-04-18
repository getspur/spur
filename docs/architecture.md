# SPUR Architecture

> Reviewed 2026-04-16. Covers all 8 workspace crates, ~280k lines of Rust.

## 1. Component Architecture

Eight crates with strict layering. No dependency cycles.

```mermaid
graph TB
    subgraph "Entry Points"
        CLI["spur-cli<br/><i>Binary, arg parsing,<br/>bootstrap</i>"]
    end

    subgraph "Presentation"
        TUI["spur-tui<br/><i>ratatui terminal UI<br/>Dashboard · SessionDetail · Picker</i>"]
    end

    subgraph "Orchestration"
        CORE["spur-core<br/><i>Orchestrator engine<br/>Event pipeline · Review loop<br/>Lineage projection</i>"]
        MCP["spur-mcp<br/><i>MCP server<br/>Brain→SPUR tool bridge</i>"]
    end

    subgraph "Protocol"
        ACP["spur-acp<br/><i>ACP client · Transports<br/>Config · Domain types<br/>Agent adapters</i>"]
    end

    subgraph "Support Services"
        PM["spur-pm<br/><i>GitHub · Linear · Plane<br/>Issue/PR adapters</i>"]
        COST["spur-cost<br/><i>SQLite cost tracking<br/>Per-session · per-project</i>"]
        WT["spur-worktree<br/><i>Git worktree lifecycle<br/>Create · diff · merge · cleanup</i>"]
    end

    CLI --> CORE
    CLI --> TUI
    CLI --> ACP
    TUI --> CORE
    TUI --> ACP
    CORE --> ACP
    CORE --> PM
    CORE --> COST
    CORE --> WT
    MCP --> ACP
    MCP --> CORE
    MCP --> PM
    MCP --> COST
    PM --> ACP
    COST --> ACP

    style ACP fill:#1a1a2e,stroke:#e94560,color:#fff
    style CORE fill:#1a1a2e,stroke:#0f3460,color:#fff
    style TUI fill:#1a1a2e,stroke:#16213e,color:#fff
    style MCP fill:#1a1a2e,stroke:#533483,color:#fff
    style CLI fill:#1a1a2e,stroke:#e94560,color:#fff
    style PM fill:#1a1a2e,stroke:#0f3460,color:#fff
    style COST fill:#1a1a2e,stroke:#0f3460,color:#fff
    style WT fill:#1a1a2e,stroke:#0f3460,color:#fff
```

### Crate Responsibilities

| Crate | Lines | Role | Key Types |
|---|---|---|---|
| `spur-acp` | ~15k | Protocol foundation | `AgentConnection`, `SpurEvent`, `SpurEventBody`, `DelegationStatus`, `SpurConfig` |
| `spur-core` | ~12k | Orchestration engine | `Orchestrator`, `ExecutorLineage`, `EventFunnel`, `ReviewSink` |
| `spur-mcp` | ~3k | Brain→SPUR bridge | MCP `Server`, tool definitions |
| `spur-tui` | ~18k | Terminal interface | `App`, `DashboardView`, `SessionDetailView` |
| `spur-cli` | ~4k | Binary entry point | CLI args, bootstrap |
| `spur-pm` | ~2k | PM integration | `PmAdapter` trait, `GitHubAdapter` |
| `spur-cost` | ~2k | Cost tracking | `CostTracker`, SQLite schema |
| `spur-worktree` | ~1.5k | Git isolation | `WorktreeManager`, `MergeResult` |

---

## 2. Data Flow

The system uses a **dual-channel architecture**: ACP (SPUR→Agent) and MCP (Agent→SPUR). Events flow outward via broadcast; commands flow inward via mpsc.

```mermaid
flowchart LR
    subgraph USER["User"]
        KB[Keyboard Input]
    end

    subgraph TUI_LAYER["spur-tui"]
        APP[App<br/>Event Loop]
        DASH[Dashboard]
        DETAIL[SessionDetail]
        LINEAGE_VIEW[Lineage<br/>Projection]
    end

    subgraph CORE_LAYER["spur-core"]
        ORCH[Orchestrator]
        FUNNEL[EventFunnel<br/><i>seq stamping</i>]
        SINK[EventSink<br/><i>NDJSON persistence</i>]
        REVIEW[ReviewSink]
        LINEAGE[ExecutorLineage<br/><i>event-sourced state</i>]
        NPUMP[NotificationPump]
    end

    subgraph MCP_LAYER["spur-mcp"]
        MCPS[MCP Server<br/><i>Tool dispatch</i>]
    end

    subgraph AGENTS["External Agents"]
        BRAIN["Brain Agent<br/><i>Claude Code / Kiro</i>"]
        W1["Worker 1<br/><i>in worktree</i>"]
        W2["Worker 2<br/><i>in worktree</i>"]
    end

    subgraph SERVICES["Support"]
        PM_SVC[spur-pm<br/>GitHub API]
        COST_SVC[spur-cost<br/>SQLite]
        WT_SVC[spur-worktree<br/>Git]
    end

    %% User → TUI → Orchestrator (commands)
    KB -->|"keypress"| APP
    APP -->|"mpsc(UserInput)"| ORCH

    %% Orchestrator → Brain (ACP)
    ORCH -->|"ACP: prompt()"| BRAIN
    BRAIN -->|"ACP: notifications"| ORCH

    %% Brain → MCP → Orchestrator (tool calls)
    BRAIN -->|"MCP: delegate_to_worker"| MCPS
    BRAIN -->|"MCP: create_pr / get_issue"| MCPS
    MCPS -->|"DelegationRequest"| ORCH

    %% Orchestrator → Workers (ACP)
    ORCH -->|"ACP: new_session + prompt"| W1
    ORCH -->|"ACP: new_session + prompt"| W2
    W1 -->|"ACP: notifications"| ORCH
    W2 -->|"ACP: notifications"| ORCH

    %% Event pipeline (broadcast)
    ORCH -->|"emit(body)"| FUNNEL
    FUNNEL -->|"broadcast"| APP
    FUNNEL -->|"broadcast"| SINK
    FUNNEL -->|"broadcast"| LINEAGE

    %% TUI rendering
    LINEAGE --> LINEAGE_VIEW
    LINEAGE_VIEW --> DASH
    LINEAGE_VIEW --> DETAIL

    %% Review loop
    ORCH --> REVIEW
    REVIEW -->|"review card"| APP
    APP -->|"SubmitReview"| ORCH

    %% Support services
    ORCH --> WT_SVC
    ORCH --> COST_SVC
    MCPS --> PM_SVC

    style BRAIN fill:#e94560,stroke:#e94560,color:#fff
    style W1 fill:#533483,stroke:#533483,color:#fff
    style W2 fill:#533483,stroke:#533483,color:#fff
    style FUNNEL fill:#0f3460,stroke:#0f3460,color:#fff
```

### Channel Summary

| Channel | Type | Direction | Purpose |
|---|---|---|---|
| `mpsc(UserInput)` | Tokio mpsc | TUI → Orchestrator | User commands, review decisions |
| `broadcast(SpurEvent)` | Tokio broadcast | Orchestrator → All | Event fan-out (TUI, sink, lineage) |
| ACP (JSON-RPC/stdio) | Agent Client Protocol | SPUR ↔ Agents | Session management, prompts, notifications |
| MCP (JSON-RPC/stdio) | Model Context Protocol | Brain → SPUR | Delegation requests, PM operations |
| `mpsc(DelegationRequest)` | Tokio mpsc | MCP Server → Orchestrator | Delegation dispatch |
| `oneshot(DelegationResult)` | Tokio oneshot | Orchestrator → MCP Server | Delegation response |

---

## 3. Delegation Lifecycle

A delegation is the core unit of work. It flows through a state machine with review gates and retry loops.

```mermaid
stateDiagram-v2
    [*] --> Requested: Brain calls delegate_to_worker

    Requested --> SemaphoreWait: DelegationRequested event
    SemaphoreWait --> WorktreeCreated: Semaphore acquired

    WorktreeCreated --> WorkerSpawned: Git worktree ready
    WorkerSpawned --> WorkerRunning: ACP new_session + prompt

    WorkerRunning --> WorkerDone: Worker completes
    WorkerRunning --> WorkerFailed: Worker error / timeout

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

    RetryRequested --> WorkerSpawned: New attempt\n(bump attempt_n,\nfresh session,\nretry context)

    MergeWorktree --> Completed: DelegationCompleted(Success)
    DiscardWorktree --> Completed: DelegationCompleted(Rejected)
    FallbackAction --> Completed: DelegationCompleted(TimedOut)
    WorkerFailed --> Completed: DelegationCompleted(Failed)

    Completed --> [*]: Result returned to brain via MCP
```

### Key Invariants

- Every delegation emits exactly one `DelegationCompleted` event (via `finalize`)
- Retry loops produce new `Attempt` entries under the same `ExecutorId`
- Stream buffer is cleared on retry start
- Worktree is cleaned up on every terminal path (merge, preserve, or discard)
- Semaphore is released on every exit path (RAII via `SemaphorePermit`)

---

## 4. Event Bus Topology

All state changes flow through the EventFunnel — a singleton that guarantees monotonic sequence numbers.

```mermaid
flowchart TB
    subgraph EMITTERS["Event Emitters"]
        O_BRAIN["Brain lifecycle<br/><i>Spawned · Error · Reconnect</i>"]
        O_DELEG["Delegation lifecycle<br/><i>Requested · Completed</i>"]
        O_WORKER["Worker lifecycle<br/><i>Spawned · Notification · Progress</i>"]
        O_REVIEW["Review lifecycle<br/><i>Requested · Resolved · Cancelled</i>"]
        O_SYSTEM["System events<br/><i>Cost · Conflict · PM</i>"]
    end

    subgraph FUNNEL["EventFunnel (singleton)"]
        MPSC["mpsc::unbounded"]
        STAMP["Stamp seq + occurred_at"]
        BC["broadcast::channel(512)"]
    end

    subgraph SUBSCRIBERS["Subscribers"]
        S_TUI["TUI App<br/><i>NotificationDrain<br/>≤8 events/frame</i>"]
        S_LINEAGE["ExecutorLineage<br/><i>Pure projection<br/>HashMap&lt;ExecutorId, Node&gt;</i>"]
        S_SINK["EventSink<br/><i>NDJSON files<br/>128MB rotation</i>"]
        S_DASH["Dashboard<br/><i>Activity log entries</i>"]
    end

    O_BRAIN --> MPSC
    O_DELEG --> MPSC
    O_WORKER --> MPSC
    O_REVIEW --> MPSC
    O_SYSTEM --> MPSC

    MPSC --> STAMP
    STAMP --> BC

    BC --> S_TUI
    BC --> S_LINEAGE
    BC --> S_SINK

    S_TUI --> S_DASH
    S_LINEAGE -->|"read by"| S_TUI

    style STAMP fill:#e94560,stroke:#e94560,color:#fff
    style BC fill:#0f3460,stroke:#0f3460,color:#fff
```

### SpurEventBody Variants (~25)

| Category | Variants |
|---|---|
| Brain lifecycle | `BrainSpawned`, `BrainError`, `BrainFailover`, `BrainReconnecting`, `BrainReconnected`, `BrainReconnectFailed` |
| Session lifecycle | `SessionCompleted`, `TurnComplete`, `AgentNotification` |
| Delegation | `DelegationRequested`, `DelegationCompleted` |
| Worker | `WorkerSpawned`, `WorkerNotification`, `WorkerProgress`, `WorkerFileTouched`, `WorkerHeartbeat` |
| Executor state | `ExecutorPhaseChanged`, `ExecutorRetryStarted`, `ExecutorArtifact` |
| Review | `ExecutorReviewRequested`, `ExecutorReviewResolved`, `ExecutorReviewCancelled` |
| System | `CostUpdate`, `ConflictDetected`, `RateLimitDetected` |
| PM | `IssueReceived`, `PrCreated`, `IssueUpdated` |

---

## 5. Architectural Assessment

### Strengths

- **Event sourcing** — lineage projection is a pure function of the event stream; session resume is replay
- **Dual-channel (ACP+MCP)** — brain has autonomy (calls tools) while SPUR controls execution
- **Clean crate layering** — no dependency cycles, spur-acp is the foundation, spur-worktree is a leaf
- **Review gate as first-class state machine** — human-in-the-loop with timeout, retry, fallback
- **Worktree isolation** — true filesystem isolation for parallel workers

### Known Risks

| Risk | Severity | Status | Location |
|---|---|---|---|
| Stranded executors — early-exit paths bypass `finalize()` | ~~Critical~~ | **Fixed** — `DelegationGuard` RAII emits `DelegationCompleted(Failed)` on Drop | `orchestrator.rs` |
| broadcast::channel drops events when subscribers are slow | ~~High~~ | **Mitigated** — drain cap raised 8→64 (1920 events/sec at 30fps) | `app.rs` |
| Silent `_ => {}` catch-alls on SpurEventBody matches | ~~High~~ | **Fixed** — explicit arms for all variants; reconnect events now update brain status | `app.rs` |
| Worktree orphaning on unclean shutdown | ~~High~~ | **Fixed** — `cleanup_orphans()` discovers and removes stale `spur/` worktrees on startup | `manager.rs` |
| No backoff on delegation retry loops | ~~High~~ | **Fixed** — exponential backoff 1s→30s cap between retries | `orchestrator.rs` |
| Worker JoinHandles not tracked for shutdown abort | **Medium** | **Open** — fire-and-forget `tokio::spawn` for ext-notification pumps; stale events emitted after executor is done | `orchestrator.rs:1300, 3123` |
| Orchestrator is a 4400-line God Object | **High** | **Open** — single struct owns brain lifecycle, delegation dispatch, review coordination, worktree management, cost tracking | `orchestrator.rs` |
| Single-process — no fault isolation between brain and workers | Low (v0.1) | **Open** — worker panic could take down orchestrator | Architecture-level |
| Broadcast Lagged recovery not implemented | Low | **Open** — Lagged events are logged but lineage is not rebuilt from NDJSON | `app.rs` event loop |
| `delegate_async` MCP tool drops `delegation_plan` from schema | Low | **Fixed** — schema now matches `delegate_to_worker` | `tools.rs` |
| No MCP tool to cancel a running delegation | Low | **Fixed** — `cancel_delegation` tool sends sentinel; orchestrator handler pending | `server.rs`, `tools.rs` |

### Remaining Work (prioritized)

1. **TaskTracker for JoinHandle management** — Replace fire-and-forget `tokio::spawn` with `tokio_util::task::TaskTracker`. Requires adding `tokio-util` to `spur-core/Cargo.toml` and refactoring ~7 spawn sites. Provides graceful shutdown with 5s drain window. Also prevents stale ext-notification events after executor completion.

2. **Orchestrator decomposition** — Split the 4400-line God Object into actors:
   ```
   Orchestrator (coordinator)
     ├── BrainSessionManager (spawn, reconnect, failover)
     ├── DelegationDispatcher (semaphore, worker spawn, worktree)
     ├── ReviewCoordinator (sink, timeout, retry)
     └── EventPipeline (funnel, sink, broadcast)
   ```

3. **Lagged event recovery** — When `broadcast::Receiver` returns `Lagged(n)`, replay from the NDJSON event log to rebuild the lineage projection from scratch.

4. **MCP tool gaps** — ~~Add `cancel_delegation` tool; add `delegation_plan` to `delegate_async` schema~~ **Done** — also added TTL eviction for `completed_delegations` and `TaskTracker` for collector JoinHandles. Remaining: orchestrator-side `__cancel_delegation` handler in `spur-core`.

### Decomposition Recommendation (for v1.0+)

The orchestrator should be split into actors:
```
Orchestrator (coordinator)
  ├── BrainSessionManager (spawn, reconnect, failover)
  ├── DelegationDispatcher (semaphore, worker spawn, worktree)
  ├── ReviewCoordinator (sink, timeout, retry)
  └── EventPipeline (funnel, sink, broadcast)
```

Each actor communicates via typed message channels, enabling independent testing and future distribution.

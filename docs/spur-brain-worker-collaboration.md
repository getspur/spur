# SPUR Brain-Worker Collaboration: End-to-End User Journey

## Table of Contents

1. [Architecture Overview](#architecture-overview)
2. [Journey 1: Ad-Hoc Task (spur run)](#journey-1-ad-hoc-task)
3. [Journey 2: Interactive Session (spur watch)](#journey-2-interactive-session)
4. [Journey 3: Plan-Based Delegation (submit_plan)](#journey-3-plan-based-delegation)
5. [Journey 4: Failure Recovery](#journey-4-failure-recovery)
6. [TUI Wireframes](#tui-wireframes)
7. [Tool Surface Map](#tool-surface-map)

---

## Architecture Overview

```mermaid
graph TB
    subgraph User Layer
        CLI["spur CLI"]
        TUI["TUI Dashboard"]
    end

    subgraph Orchestrator Layer
        ORCH["Orchestrator"]
        FUNNEL["Event Funnel<br/>(monotonic seq)"]
        REVIEW["Review Sink"]
    end

    subgraph MCP Layer
        MCP["MCP Callback Server<br/>(HTTP JSON-RPC)"]
        PLAN["Plan Executor<br/>(DAG Scheduler)"]
    end

    subgraph Agent Layer
        BRAIN["Brain Agent<br/>(claude-code / kiro / codex)"]
        W1["Worker 1<br/>(worktree A)"]
        W2["Worker 2<br/>(worktree B)"]
        W3["Worker N<br/>(worktree N)"]
    end

    subgraph PM Layer
        BEADS["BeadsAdapter<br/>(br CLI)"]
        BV["BvAdapter<br/>(bv CLI)"]
        GH["GitHubAdapter<br/>(gh CLI)"]
    end

    CLI --> ORCH
    TUI --> ORCH
    ORCH --> FUNNEL
    FUNNEL --> TUI
    ORCH --> MCP
    MCP --> BRAIN
    BRAIN -->|"MCP tool calls"| MCP
    MCP -->|"DelegationRequest"| ORCH
    MCP --> PLAN
    PLAN -->|"DelegationRequest"| ORCH
    ORCH -->|"spawn in worktree"| W1
    ORCH -->|"spawn in worktree"| W2
    ORCH -->|"spawn in worktree"| W3
    ORCH --> REVIEW
    MCP --> BEADS
    MCP --> BV
    MCP --> GH
```

### Separation of Concerns

```mermaid
graph LR
    subgraph "Brain = PLANNER"
        B1["Understands task"]
        B2["Decomposes into subtasks"]
        B3["Assigns agents"]
        B4["Reviews results"]
    end

    subgraph "Orchestrator = SCHEDULER"
        O1["Enforces dependency order"]
        O2["Manages concurrency<br/>(semaphore)"]
        O3["Creates worktrees"]
        O4["Routes review gate"]
    end

    subgraph "Worker = EXECUTOR"
        W1["Receives focused task"]
        W2["Codes in isolated worktree"]
        W3["Returns diff + summary"]
    end

    B1 --> B2 --> B3 --> O1
    O1 --> O2 --> O3 --> W1
    W1 --> W2 --> W3 --> O4
    O4 --> B4
```

---

## Journey 1: Ad-Hoc Task

**Command:** `spur run "implement JWT auth"`

```mermaid
sequenceDiagram
    participant User
    participant CLI as spur CLI
    participant Orch as Orchestrator
    participant MCP as MCP Server
    participant Brain
    participant PM as PM Service
    participant BV as bv (graph)
    participant Worker

    User->>CLI: spur run "implement JWT auth"
    CLI->>Orch: run_adhoc(task, opts)

    par Parallel startup
        Orch->>PM: list_issues(status: open)
        PM-->>Orch: issues[]
    and
        Orch->>BV: bv --robot-triage
        BV-->>Orch: TriageReport
    end

    Orch->>Orch: emit IssuesLoaded + GraphAlertsSummary
    Orch->>Orch: build_brain_prompt(graph_summary + task)

    Orch->>MCP: start HTTP server
    Orch->>Brain: initialize + new_session + prompt

    loop Brain reasoning
        Brain->>MCP: graph_triage()
        MCP->>BV: bv --robot-triage
        BV-->>MCP: raw JSON
        MCP-->>Brain: triage report

        Brain->>MCP: delegate_to_worker(agent, task)
        MCP->>Orch: DelegationRequest via channel
        Orch->>Worker: spawn in worktree
        Worker-->>Orch: DelegationResult
        Orch-->>MCP: result via oneshot
        MCP-->>Brain: result JSON
    end

    Brain-->>Orch: turn complete
    Orch->>CLI: RunResult
    CLI->>User: session_id, cost, success
```

---

## Journey 2: Interactive Session

**Command:** `spur watch`

```mermaid
sequenceDiagram
    participant User
    participant TUI
    participant Orch as Orchestrator
    participant Brain
    participant MCP as MCP Server
    participant Worker

    User->>TUI: spur watch
    TUI->>Orch: run_interactive()

    Orch->>Orch: refresh_pm_state() [parallel issues + alerts]
    Orch->>Orch: emit startup guidance (if br/bv missing)

    Note over TUI: Splash screen: "Type a task below"

    User->>TUI: types "add user registration"
    TUI->>Orch: InteractiveInput::Message

    Orch->>Brain: lazy-spawn + prompt
    Brain->>MCP: graph_plan()
    MCP-->>Brain: execution tracks

    Brain->>MCP: submit_plan(tasks with deps)
    MCP->>MCP: validate DAG (cycles, dangling)
    MCP-->>Brain: plan_id

    Note over MCP: Plan executor runs in background

    loop Plan execution (automatic)
        MCP->>Orch: DelegationRequest (ready tasks)
        Orch->>Worker: spawn in worktree
        Orch-->>TUI: DelegationRequested event
        Worker-->>Orch: result
        Orch-->>TUI: DelegationCompleted event
        Orch-->>MCP: result via oneshot
        MCP->>MCP: unblock dependent tasks
    end

    Brain->>MCP: get_plan_status(plan_id)
    MCP-->>Brain: {status: completed, tasks: [...]}

    Brain-->>TUI: streaming response with review
    TUI-->>User: shows completion

    User->>TUI: types next message...
```

---

## Journey 3: Plan-Based Delegation

**The deterministic workflow: brain plans, orchestrator schedules, workers execute.**

```mermaid
flowchart TD
    START([Brain receives task]) --> TRIAGE

    subgraph "Phase 1: Orientation"
        TRIAGE["graph_triage()<br/>Project health + recommendations"]
        PLAN["graph_plan()<br/>Dependency-ordered tracks"]
        TRIAGE --> PLAN
    end

    subgraph "Phase 2: Plan Construction"
        ENRICH["For each track item:<br/>1. Read issue detail<br/>2. Choose agent<br/>3. Write CONTEXT/GOAL/CONSTRAINTS"]
        PLAN --> ENRICH
        SUBMIT["submit_plan(tasks=[<br/>  {id:A, agent:codex, deps:[]},<br/>  {id:B, agent:claude, deps:[]},<br/>  {id:C, agent:claude, deps:[A,B]}<br/>])"]
        ENRICH --> SUBMIT
    end

    subgraph "Phase 3: Automatic Execution"
        VALIDATE{"validate_plan()<br/>cycles? dangling?"}
        SUBMIT --> VALIDATE
        VALIDATE -->|invalid| ERROR[Return error to brain]
        VALIDATE -->|valid| READY["Compute ready set<br/>(tasks with deps satisfied)"]

        READY --> DISPATCH["Dispatch ready tasks<br/>in parallel via delegation_tx"]
        DISPATCH --> WAIT["JoinSet: await<br/>next completion"]
        WAIT --> UPDATE["Update task status<br/>Completed / Failed"]
        UPDATE --> UNBLOCK{"More tasks<br/>unblocked?"}
        UNBLOCK -->|yes| READY
        UNBLOCK -->|no, in-flight| WAIT
        UNBLOCK -->|no, done| FINISH
        FINISH["Mark unreachable tasks<br/>(blocked by failed dep)"]
    end

    subgraph "Phase 4: Review"
        POLL["get_plan_status(plan_id)<br/>→ per-task status + diff_summary"]
        FINISH --> POLL
        POLL --> REVIEW["Brain reviews results"]
        REVIEW --> PR["create_pr() or<br/>further work"]
    end
```

### Plan Execution Timeline (Diamond Dependency)

```mermaid
gantt
    title Plan: A,B independent → C depends on A+B → D depends on C
    dateFormat X
    axisFormat %s

    section Track 1 (parallel)
    Task A (codex)      :a, 0, 5
    Task B (claude)     :b, 0, 8

    section Track 2 (after A+B)
    Task C (claude)     :c, after b, 6

    section Track 3 (after C)
    Task D (claude)     :d, after c, 4

    section Milestones
    A completes         :milestone, 5, 0
    B completes         :milestone, 8, 0
    C ready (unblocked) :milestone, 8, 0
    Plan complete       :milestone, 18, 0
```

---

## Journey 4: Failure Recovery

```mermaid
stateDiagram-v2
    [*] --> Pending: Plan submitted

    Pending --> Ready: All deps completed
    Ready --> Dispatched: Sent to worker
    Dispatched --> Completed: Worker success
    Dispatched --> Failed: Worker error

    Pending --> Failed: Blocked by failed dep

    Completed --> [*]
    Failed --> [*]

    note right of Failed
        Brain sees failure via
        get_plan_status() and
        can retry with
        delegate_to_worker()
        or submit a new plan
    end note
```

### Failure Propagation Example

```mermaid
flowchart LR
    A["Task A<br/>✅ Completed"] --> C
    B["Task B<br/>❌ Failed:<br/>'test error'"] --> C
    C["Task C<br/>❌ Failed:<br/>'Blocked by<br/>failed dependency'"] --> D
    D["Task D<br/>❌ Failed:<br/>'Blocked by<br/>failed dependency'"]

    style A fill:#2d5a2d,color:#fff
    style B fill:#5a2d2d,color:#fff
    style C fill:#5a2d2d,color:#fff
    style D fill:#5a2d2d,color:#fff
```

**get_plan_status response:**
```json
{
  "plan_id": "abc-123",
  "status": "partial",
  "progress": "1/4 completed, 0 running, 0 pending, 3 failed",
  "tasks": [
    {"task_id": "A", "agent": "codex",  "status": "completed", "summary": "JWT module done"},
    {"task_id": "B", "agent": "claude", "status": "failed",    "error": "test compilation error"},
    {"task_id": "C", "agent": "claude", "status": "failed",    "error": "Blocked by failed dependency"},
    {"task_id": "D", "agent": "claude", "status": "failed",    "error": "Blocked by failed dependency"}
  ]
}
```

---

## TUI Wireframes

### State 1: Startup — Splash Screen

```
┌──────────────────────────────────────────────────────────────────────┐
│                                                                      │
│                                                                      │
│                              SPUR                                    │
│                                                                      │
│                    Multi-agent orchestrator                           │
│                                                                      │
│                   Type a task below to start                         │
│                                                                      │
│                   Press [s] to browse sessions                       │
│                                                                      │
│                                                                      │
├──────────────────────────────────────────────────────────────────────┤
│ > _                                                                  │
├──────────────────────────────────────────────────────────────────────┤
│ [i]nput [Enter]focus [s]essions [?]help [q]uit                       │
│                          12 issues · 2 alerts · 0 running · $0.00   │
└──────────────────────────────────────────────────────────────────────┘
```

### State 2: Brain Thinking — Tool Calls Visible

```
┌─ Lineage ───────────────────────────────────────────────────────────┐
│  ▼ brain:claude-code [sess-a1b2]                                    │
│    ├─ ⟳ Running (12s)                                               │
│    │   ├─ graph_triage ✓                                            │
│    │   ├─ graph_plan ✓                                              │
│    │   └─ submit_plan → plan-xyz (4 tasks)                          │
├─ Issues (12 open) ──────────────────────────────────────────────────┤
│  ◆ ISS-1  P0  Implement JWT auth module                            │
│  ◆ ISS-2  P1  Add users table migration                            │
│  ◆ ISS-3  P1  Wire up login endpoint                    blocked    │
│  ◆ ISS-4  P2  Add e2e auth tests                        blocked    │
├─ Activity ──────────────────────────────────────────────────────────┤
│  11:02:31 [brain] Analyzing project graph...                        │
│  11:02:32 [brain] Graph triage: 12 open, 8 actionable, 2 blocked   │
│  11:02:33 [brain] Execution plan: 3 tracks, 4 tasks                │
│  11:02:34 [brain] Submitting plan with dependency ordering...       │
│  11:02:34 [spur]  Plan submitted: 4 tasks (plan-xyz)               │
├──────────────────────────────────────────────────────────────────────┤
│ > _                                                                  │
├──────────────────────────────────────────────────────────────────────┤
│ [i]nput [r]eview [?]help    12 issues · 2 alerts · 1 running · $0.12│
└──────────────────────────────────────────────────────────────────────┘
```

### State 3: Plan Executing — Workers in Parallel

```
┌─ Lineage ───────────────────────────────────────────────────────────┐
│  ▼ brain:claude-code [sess-a1b2]                                    │
│    ├─ ✓ Completed turn (submitted plan)                             │
│    ├─ ▶ worker:codex [ISS-1] ⟳ Running (45s)                       │
│    │   └─ worktree: .worktrees/deleg-001                            │
│    ├─ ▶ worker:claude-code [ISS-2] ⟳ Running (42s)                 │
│    │   └─ worktree: .worktrees/deleg-002                            │
│    ├─ ◻ worker:claude-code [ISS-3] ⏳ Pending (blocked by 1,2)     │
│    └─ ◻ worker:claude-code [ISS-4] ⏳ Pending (blocked by 3)       │
├─ Issues (12 open) ──────────────────────────────────────────────────┤
│  ◆ ISS-1  P0  Implement JWT auth module              in_progress   │
│  ◆ ISS-2  P1  Add users table migration              in_progress   │
│  ◆ ISS-3  P1  Wire up login endpoint                    blocked    │
│  ◆ ISS-4  P2  Add e2e auth tests                        blocked    │
├─ Activity ──────────────────────────────────────────────────────────┤
│  11:02:35 [deleg] Dispatched ISS-1 → codex (worktree deleg-001)    │
│  11:02:35 [deleg] Dispatched ISS-2 → claude-code (deleg-002)       │
│  11:02:36 [deleg] Claimed ISS-1 → status: in_progress              │
│  11:02:36 [deleg] Claimed ISS-2 → status: in_progress              │
│  11:03:15 [codex] ISS-1: Implementing JWT signing and verification │
│  11:03:18 [claude] ISS-2: Creating users migration with bcrypt...  │
├──────────────────────────────────────────────────────────────────────┤
│ > _                                                                  │
├──────────────────────────────────────────────────────────────────────┤
│ [i]nput [r]eview [?]help    12 issues · 2 alerts · 2 running · $0.45│
└──────────────────────────────────────────────────────────────────────┘
```

### State 4: Dependency Unblocking — ISS-3 Starts After ISS-1+2

```
┌─ Lineage ───────────────────────────────────────────────────────────┐
│  ▼ brain:claude-code [sess-a1b2]                                    │
│    ├─ ✓ Completed turn                                              │
│    ├─ ✓ worker:codex [ISS-1] ✓ Completed (2m 10s) +3/-0 1 file    │
│    ├─ ✓ worker:claude-code [ISS-2] ✓ Completed (2m 30s) +42/-0    │
│    ├─ ▶ worker:claude-code [ISS-3] ⟳ Running (15s)                 │
│    │   └─ worktree: .worktrees/deleg-003                            │
│    └─ ◻ worker:claude-code [ISS-4] ⏳ Pending (blocked by 3)       │
├─ Issues (12 open) ──────────────────────────────────────────────────┤
│  ◆ ISS-1  P0  Implement JWT auth module                  open      │
│  ◆ ISS-2  P1  Add users table migration                  open      │
│  ◆ ISS-3  P1  Wire up login endpoint                 in_progress   │
│  ◆ ISS-4  P2  Add e2e auth tests                        blocked    │
├─ Activity ──────────────────────────────────────────────────────────┤
│  11:04:45 [deleg] ISS-1 completed: JWT implementation done         │
│  11:05:05 [deleg] ISS-2 completed: Users table + bcrypt migration  │
│  11:05:06 [spur]  ISS-3 unblocked (deps ISS-1, ISS-2 satisfied)   │
│  11:05:06 [deleg] Dispatched ISS-3 → claude-code (deleg-003)       │
│  11:05:07 [deleg] Claimed ISS-3 → status: in_progress              │
│  11:05:20 [claude] ISS-3: Wiring JWT middleware into login route...│
├──────────────────────────────────────────────────────────────────────┤
│ > _                                                                  │
├──────────────────────────────────────────────────────────────────────┤
│ [i]nput [r]eview [?]help    12 issues · 0 alerts · 1 running · $1.23│
└──────────────────────────────────────────────────────────────────────┘
```

### State 5: Plan Complete — Brain Reviews

```
┌─ Lineage ───────────────────────────────────────────────────────────┐
│  ▼ brain:claude-code [sess-a1b2]                                    │
│    ├─ ⟳ Running (reviewing plan results...)                         │
│    ├─ ✓ worker:codex [ISS-1] ✓ Completed +3/-0 1 file             │
│    ├─ ✓ worker:claude-code [ISS-2] ✓ Completed +42/-0 3 files     │
│    ├─ ✓ worker:claude-code [ISS-3] ✓ Completed +28/-2 2 files     │
│    └─ ✓ worker:claude-code [ISS-4] ✓ Completed +65/-0 4 files     │
├─ Issues (12 open) ──────────────────────────────────────────────────┤
│  ◆ ISS-1  P0  Implement JWT auth module                  open      │
│  ◆ ISS-2  P1  Add users table migration                  open      │
│  ◆ ISS-3  P1  Wire up login endpoint                     open      │
│  ◆ ISS-4  P2  Add e2e auth tests                         open      │
├─ Activity ──────────────────────────────────────────────────────────┤
│  11:08:12 [deleg] ISS-4 completed: 65 lines of e2e tests added    │
│  11:08:12 [spur]  Plan plan-xyz complete: 4/4 tasks succeeded      │
│  11:08:13 [brain] Reviewing all worker results...                  │
│  11:08:15 [brain] All 4 tasks completed successfully.              │
│  11:08:16 [brain] Creating pull request...                         │
│  11:08:18 [brain] PR created: github.com/org/repo/pull/42          │
├──────────────────────────────────────────────────────────────────────┤
│ > _                                                                  │
├──────────────────────────────────────────────────────────────────────┤
│ [i]nput [r]eview [?]help    12 issues · 0 alerts · 0 running · $2.15│
└──────────────────────────────────────────────────────────────────────┘
```

### State 6: Partial Failure — Brain Sees Failed Tasks

```
┌─ Lineage ───────────────────────────────────────────────────────────┐
│  ▼ brain:claude-code [sess-a1b2]                                    │
│    ├─ ⟳ Running (handling failure...)                               │
│    ├─ ✓ worker:codex [ISS-1] ✓ Completed +3/-0 1 file             │
│    ├─ ✗ worker:claude-code [ISS-2] ✗ Failed: "test compile error" │
│    ├─ ✗ [ISS-3] Blocked by failed dependency (ISS-2)               │
│    └─ ✗ [ISS-4] Blocked by failed dependency (ISS-3)               │
├─ Issues (12 open) ──────────────────────────────────────────────────┤
│  ◆ ISS-1  P0  Implement JWT auth module                  open      │
│  ◆ ISS-2  P1  Add users table migration                  open      │
│  ◆ ISS-3  P1  Wire up login endpoint                    blocked    │
│  ◆ ISS-4  P2  Add e2e auth tests                        blocked    │
├─ Activity ──────────────────────────────────────────────────────────┤
│  11:06:30 [deleg] ISS-2 FAILED: test compile error                 │
│  11:06:30 [spur]  Plan plan-xyz: ISS-3 blocked by failed dep      │
│  11:06:30 [spur]  Plan plan-xyz: ISS-4 blocked by failed dep      │
│  11:06:31 [brain] ISS-2 failed. Retrying with constraints...      │
│  11:06:32 [brain] delegate_to_worker(claude-code, "Fix ISS-2...")  │
│  11:06:33 [deleg] Dispatched ISS-2-retry → claude-code (deleg-005)│
├──────────────────────────────────────────────────────────────────────┤
│ > _                                                                  │
├──────────────────────────────────────────────────────────────────────┤
│ [i]nput [r]eview [?]help    12 issues · 1 alerts · 1 running · $1.80│
└──────────────────────────────────────────────────────────────────────┘
```

### State 7: Missing Dependencies — Startup Guidance

```
┌──────────────────────────────────────────────────────────────────────┐
│                                                                      │
│                              SPUR                                    │
│                                                                      │
│                    Multi-agent orchestrator                           │
│                                                                      │
│                   Type a task below to start                         │
│                                                                      │
│                   Press [s] to browse sessions                       │
│                                                                      │
├─ Issues ────────────────────────────────────────────────────────────┤
├──────────────────────────────────────────────────────────────────────┤
│ > _                                                                  │
├──────────────────────────────────────────────────────────────────────┤
│ [i]nput [Enter]focus [s]essions [?]help    8 issues · 0 running     │
└──────────────────────────────────────────────────────────────────────┘
```

---

## Tool Surface Map

### By Category

```
┌─────────────────────────────────────────────────────────────────────┐
│                        MCP Tool Surface (20 tools)                  │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  PLAN-BASED DELEGATION (deterministic)                              │
│  ┌─────────────────┐    ┌──────────────────┐                       │
│  │  submit_plan    │───▶│ get_plan_status   │                       │
│  │  DAG of tasks   │    │ poll progress     │                       │
│  └─────────────────┘    └──────────────────┘                       │
│                                                                     │
│  AD-HOC DELEGATION (brain-driven)                                   │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────┐  │
│  │delegate_to_worker│  │delegate_parallel │  │ delegate_async   │  │
│  │ blocking, single │  │ blocking, batch  │  │ non-blocking     │  │
│  └──────────────────┘  └──────────────────┘  └────────┬─────────┘  │
│                                                        │            │
│  ┌──────────────────┐  ┌──────────────────┐  ┌────────▼─────────┐  │
│  │cancel_delegation │  │check_deleg_status│  │ wait_delegation  │  │
│  └──────────────────┘  └──────────────────┘  └──────────────────┘  │
│                                                                     │
│  GRAPH ANALYSIS (bv)              PROJECT MANAGEMENT (br/gh)        │
│  ┌───────────────┐               ┌──────────────┐                  │
│  │ graph_triage  │──orientation  │ list_issues   │──CRUD           │
│  │ graph_plan    │──tracks       │ get_issue     │                  │
│  │ graph_insights│──deep metrics │ update_issue  │                  │
│  │ graph_alerts  │──monitoring   │ create_pr     │                  │
│  │ graph_subgraph│──deps for ID  └──────────────┘                  │
│  └───────────────┘                                                  │
│                                                                     │
│  UTILITY                                                            │
│  ┌────────────────────┐  ┌────────────────┐  ┌─────────────────┐   │
│  │list_available_     │  │report_progress │  │get_session_cost │   │
│  │       workers      │  │ fire-and-forget│  │                 │   │
│  └────────────────────┘  └────────────────┘  └─────────────────┘   │
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

### Decision Flow: Which Delegation Tool?

```mermaid
flowchart TD
    START{How complex<br/>is the work?}

    START -->|"Single task,<br/>want result now"| DW["delegate_to_worker<br/>(blocking, 90s timeout)"]
    START -->|"Multiple independent<br/>tasks, no deps"| DP["delegate_parallel<br/>(blocking, all at once)"]
    START -->|"Tasks with<br/>dependency ordering"| SP["submit_plan<br/>(DAG executor)"]
    START -->|"Fire and forget,<br/>check later"| DA["delegate_async<br/>+ wait_delegation"]

    SP -->|"Monitor"| GPS["get_plan_status<br/>(non-blocking poll)"]
    DA -->|"Poll"| CDS["check_delegation_status"]
    DW -->|"Timed out?"| CDS
```

### Optimal Brain Workflow

```mermaid
flowchart TD
    A["1. graph_triage()<br/>Orientation: health, recommendations"] --> B
    B["2. graph_plan()<br/>Dependency-ordered execution tracks"] --> C
    C["3. For each issue in plan:<br/>get_issue(id) → read details"] --> D
    D["4. Enrich tasks:<br/>assign agent + write<br/>CONTEXT/GOAL/CONSTRAINTS"] --> E
    E["5. submit_plan(tasks)<br/>→ plan_id"] --> F
    F["6. get_plan_status(plan_id)<br/>Poll until complete"] --> G
    G{"All succeeded?"}
    G -->|yes| H["7. create_pr()"]
    G -->|partial| I["8. Review failures<br/>delegate_to_worker() for retries"]
    I --> F

    style A fill:#1a3a5c,color:#fff
    style B fill:#1a3a5c,color:#fff
    style E fill:#2d5a2d,color:#fff
    style H fill:#2d5a2d,color:#fff
```

---

## Data Flow: Event Bus

```mermaid
flowchart LR
    subgraph Sources
        ORCH["Orchestrator"]
        PLAN["Plan Executor"]
        DELEG["Delegation Handler"]
    end

    subgraph "Event Funnel (monotonic seq)"
        FUNNEL["mpsc → stamp seq + timestamp → broadcast"]
    end

    subgraph Consumers
        TUI["TUI Dashboard"]
        SINK["JSONL Sink<br/>(.spur/events/)"]
    end

    ORCH -->|"BrainSpawned<br/>IssuesLoaded<br/>GraphAlertsSummary<br/>TurnComplete"| FUNNEL
    PLAN -->|"(via delegation pipeline)<br/>DelegationRequested<br/>DelegationDispatched"| FUNNEL
    DELEG -->|"DelegationCompleted<br/>IssueUpdated<br/>ExecutorPhaseChanged"| FUNNEL

    FUNNEL --> TUI
    FUNNEL --> SINK
```

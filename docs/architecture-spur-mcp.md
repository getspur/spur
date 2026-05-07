# spur-mcp — Detailed Architecture

> Reviewed 2026-04-16. Covers `crates/spur-mcp/src/` (~3k lines, 3 files).

## 1. Role in SPUR

SPUR uses a **dual-channel architecture** to communicate with the brain agent:

| Channel | Direction | Protocol | Purpose |
|---|---|---|---|
| ACP | SPUR → Brain | JSON-RPC / stdio | Session management, prompts, notifications |
| MCP | Brain → SPUR | JSON-RPC / HTTP | Tool calls: delegation, PM ops, observability |

`spur-mcp` implements the **inbound** MCP channel. It is the brain's only mechanism to cause side effects in SPUR — every delegation, every PR creation, every progress report flows through this crate.

```mermaid
graph LR
    subgraph SPUR["SPUR Process"]
        ORCH["spur-core<br/>Orchestrator"]
        MCP["spur-mcp<br/>McpCallbackServer"]
        PM["spur-pm"]
        COST["spur-cost"]
        WT["spur-worktree"]
    end

    BRAIN["Brain Agent<br/>(Claude Code / Kiro)"]

    ORCH -->|"ACP: prompt(), notifications"| BRAIN
    BRAIN -->|"MCP: tools/call (HTTP POST)"| MCP
    MCP -->|"mpsc‹DelegationRequest›"| ORCH
    ORCH -->|"oneshot‹DelegationResult›"| MCP
    ORCH --> PM
    ORCH --> COST
    ORCH --> WT

    style MCP fill:#533483,stroke:#533483,color:#fff
    style BRAIN fill:#e94560,stroke:#e94560,color:#fff
```

**First-principles justification**: An LLM brain can only act through tool calls. ACP gives SPUR control over the brain's session. MCP gives the brain control over SPUR's capabilities. The two channels form a bidirectional control plane where neither side is fully passive.

---

## 2. Internal Layer Architecture

`spur-mcp` is 3 files with 6 logical layers:

```mermaid
graph TB
    subgraph server.rs["server.rs"]
        L1["Layer 1: Transport<br/><i>TCP listener · HTTP/1.1 parser<br/>127.0.0.1:0 · 1 MiB body cap</i>"]
        L2["Layer 2: JSON-RPC Protocol<br/><i>Request/Response/Error types<br/>MCP methods: initialize, tools/list, tools/call</i>"]
        L4["Layer 4: Tool Dispatch<br/><i>handle_tool_call() → per-tool handlers<br/>Pattern match on tool name string</i>"]
        L5["Layer 5: Delegation Lifecycle<br/><i>active_delegations (HashSet)<br/>completed_delegations (HashMap)<br/>spawn_result_collector()</i>"]
    end

    subgraph tools.rs["tools.rs"]
        L3["Layer 3: Tool Registry<br/><i>11 static ToolDefinitions<br/>JSON Schema inputSchema<br/>tools_list()</i>"]
        L6["Layer 6: Orchestrator Bridge<br/><i>DelegationRequest + oneshot<br/>DelegationChannel (mpsc receiver)<br/>brain_session_id stamping</i>"]
    end

    L1 --> L2
    L2 --> L4
    L4 --> L5
    L4 -.->|"tools/list"| L3
    L5 --> L6
    L6 -->|"mpsc send"| ORCH["Orchestrator<br/>(spur-core)"]

    style L1 fill:#1a1a2e,stroke:#e94560,color:#fff
    style L2 fill:#1a1a2e,stroke:#e94560,color:#fff
    style L3 fill:#1a1a2e,stroke:#533483,color:#fff
    style L4 fill:#1a1a2e,stroke:#0f3460,color:#fff
    style L5 fill:#1a1a2e,stroke:#0f3460,color:#fff
    style L6 fill:#1a1a2e,stroke:#533483,color:#fff
```

### Layer Details

| Layer | File | Responsibility | Key Design Decision |
|---|---|---|---|
| **Transport** | `server.rs` | TCP accept loop, HTTP parsing | Hand-rolled HTTP — avoids axum/hyper dependency for a localhost-only, single-client server |
| **JSON-RPC** | `server.rs` | MCP protocol framing | Standard error codes (-32700, -32601, -32602, -32603) |
| **Tool Registry** | `tools.rs` | Static tool definitions | Compile-time fixed; no dynamic registration |
| **Tool Dispatch** | `server.rs` | Route tool name → handler | String match; each handler validates its own params |
| **Delegation Lifecycle** | `server.rs` | Async state for in-flight delegations | Background collector pattern decouples delegation lifetime from HTTP request lifetime |
| **Orchestrator Bridge** | `tools.rs` | Channel types + request construction | Oneshot-per-request eliminates response routing bugs |

---

## 3. Tool Taxonomy

11 tools in 4 categories, all exposed via `tools/list`:

```mermaid
graph TB
    subgraph DELEGATION["Delegation (core)"]
        D1["delegate_to_worker<br/><i>Blocking, 90s timeout<br/>Falls back to polling</i>"]
        D2["delegate_parallel<br/><i>Fan-out N tasks<br/>Batch 90s timeout</i>"]
        D3["delegate_async<br/><i>Fire-and-forget<br/>Returns delegation_id</i>"]
        D4["wait_delegation<br/><i>Block on async result<br/>90s timeout</i>"]
        D5["check_delegation_status<br/><i>Non-blocking poll<br/>Returns result or 'running'</i>"]
        D6["cancel_delegation<br/><i>Request abort<br/>Non-blocking, poll to confirm</i>"]
    end

    subgraph DISCOVERY["Discovery"]
        W1["list_available_workers<br/><i>Returns WorkerInfo[]<br/>tier, good_for, cost_tier</i>"]
    end

    subgraph PM["PM Passthrough"]
        P1["get_issue<br/><i>GitHub / Linear / Plane</i>"]
        P2["update_issue<br/><i>Status + comment</i>"]
        P3["create_pr<br/><i>Title, body, branch</i>"]
    end

    subgraph OBS["Observability"]
        O1["report_progress<br/><i>Fire-and-forget<br/>No response payload</i>"]
        O2["get_session_cost<br/><i>Accumulated USD estimate</i>"]
    end

    style DELEGATION fill:#1a1a2e,stroke:#e94560,color:#fff
    style DISCOVERY fill:#1a1a2e,stroke:#0f3460,color:#fff
    style PM fill:#1a1a2e,stroke:#533483,color:#fff
    style OBS fill:#1a1a2e,stroke:#16213e,color:#fff
```

### Delegation Patterns

The brain has three strategies for dispatching work:

| Pattern | Tool(s) | Blocking? | Use Case |
|---|---|---|---|
| **Synchronous** | `delegate_to_worker` | Yes (90s cap) | Single task, brain waits for result |
| **Async + Poll** | `delegate_async` → `check_delegation_status` / `wait_delegation` | No (initial), then optional block | Long-running tasks, brain does other work meanwhile |
| **Parallel** | `delegate_parallel` | Yes (90s cap) | Independent subtasks, brain waits for all |

### Plan-level base (br-osl)

`submit_plan` accepts an optional `base: BaseTarget` parameter. When omitted (or set to `{"kind":"repo_main"}`), the plan engine snapshots the brain working tree HEAD into `spur/brain-snapshot-*` (legacy default — convenient for "extend my desk" workflows).

To dispatch a plan against an explicit ref instead, pass:

```json
{ "tasks": [...], "base": { "kind": "branch", "name": "<branch>" } }
```

or

```json
{ "tasks": [...], "base": { "kind": "commit", "oid": "<oid>" } }
```

In these explicit cases:
- The brain working tree is **not** touched (no stash, no `index.lock` contention).
- A `spur/brain-snapshot-*` ref is created pointing at the resolved OID, decoupling the plan's base from any later movement of the source branch.
- `merge_plan` cherry-picks worker branches onto this snapshot ref exactly as before.
- The reconciler still emits `WithOverlay { base: Branch{<snapshot ref>}, overlays: [<approved deps>] }` for every dispatch.
- The `PlanSubmit` audit sentinel records the operator-supplied `BaseTarget` in `explicit_base` for forensics.

Use case: stacking phased plans. Phase N+1 specifies `base: { kind: "branch", name: "spur/plan-merge-<phase-N-id>" }` so its workers see Phase N's approved-but-unmerged work as their foundation.

Out of scope (not in this implementation): plan-level `WithOverlay`, per-task `base` overrides. File a follow-up issue if either becomes necessary.

### Sentinel Agent Convention

PM and internal operations reuse the `DelegationRequest` channel with magic agent name prefixes:

| Sentinel Agent | Routed To | Blocking? |
|---|---|---|
| `__pm_get_issue` | `spur-pm` adapter | Yes (oneshot) |
| `__pm_update_issue` | `spur-pm` adapter | Yes (oneshot) |
| `__pm_create_pr` | `spur-pm` adapter | Yes (oneshot) |
| `__progress` | Orchestrator event emit | Fire-and-forget |
| `__session_cost` | `spur-cost` tracker | Yes (oneshot) |

**Design rationale**: One channel, one message type. The orchestrator pattern-matches on the `__` prefix in `execute_delegation`. Avoids N separate channel types at the cost of slightly magical strings.

---

## 4. Request Lifecycle — Blocking Delegation

The most common path: brain calls `delegate_to_worker`, blocks up to 90s, gets result or falls back to polling.

```mermaid
sequenceDiagram
    participant Brain as Brain Agent
    participant HTTP as Transport (TCP)
    participant RPC as JSON-RPC Dispatch
    participant DLM as Delegation Lifecycle Mgr
    participant CH as mpsc Channel
    participant ORCH as Orchestrator
    participant Worker as Worker Agent

    Brain->>HTTP: POST / (JSON-RPC tools/call)
    HTTP->>RPC: JsonRpcRequest{method: "tools/call"}
    RPC->>RPC: Extract tool name + arguments
    RPC->>DLM: handle_delegate_to_worker(args)

    DLM->>DLM: Generate UUID request_id
    DLM->>DLM: Create oneshot (tx, rx)
    DLM->>DLM: Build DelegationRequest{id, agent, task, respond_to: tx}
    DLM->>CH: delegation_tx.send(request)
    DLM->>DLM: Insert request_id → active_delegations
    DLM->>DLM: spawn_result_collector(request_id, rx)

    CH->>ORCH: request_rx.recv()
    ORCH->>ORCH: Acquire semaphore permit
    ORCH->>ORCH: Create worktree
    ORCH->>Worker: ACP: new_session + prompt
    Worker-->>ORCH: ACP: notifications (streaming)
    Worker->>ORCH: ACP: session complete

    alt Review required
        ORCH->>ORCH: ReviewGate (await user decision)
    end

    ORCH->>DLM: oneshot tx.send(DelegationResult)
    Note over DLM: spawn_result_collector moves<br/>result to completed_delegations

    loop Every 250ms (up to 90s)
        DLM->>DLM: Check completed_delegations.remove(id)
    end

    alt Result arrived within 90s
        DLM->>RPC: JsonRpcResponse::success(result)
        RPC->>HTTP: HTTP 200 + JSON body
        HTTP->>Brain: Response
    else Timeout exceeded
        DLM->>RPC: JsonRpcResponse::success({delegation_id})
        RPC->>HTTP: HTTP 200 + polling instructions
        HTTP->>Brain: "Call check_delegation_status"
    end
```

---

## 5. Request Lifecycle — Async Delegation + Polling

Brain fires `delegate_async`, immediately gets a `delegation_id`, then polls later.

```mermaid
sequenceDiagram
    participant Brain as Brain Agent
    participant MCP as McpCallbackServer
    participant ORCH as Orchestrator
    participant Worker as Worker Agent

    Brain->>MCP: delegate_async{agent, task}
    MCP->>MCP: Generate UUID, create oneshot
    MCP->>ORCH: mpsc send(DelegationRequest)
    MCP->>MCP: Insert → active_delegations
    MCP->>MCP: spawn_result_collector(id, rx)
    MCP->>Brain: {delegation_id: "abc-123"}

    Note over Brain: Brain continues other work...

    ORCH->>Worker: ACP: spawn + prompt
    Worker-->>ORCH: (working...)

    Brain->>MCP: check_delegation_status{delegation_id: "abc-123"}
    MCP->>MCP: Check completed_delegations → not found
    MCP->>MCP: Check active_delegations → found
    MCP->>Brain: {status: "running"}

    Worker->>ORCH: Complete
    ORCH->>MCP: oneshot send(DelegationResult)
    Note over MCP: Collector moves result<br/>active → completed

    Brain->>MCP: check_delegation_status{delegation_id: "abc-123"}
    MCP->>MCP: completed_delegations.remove("abc-123")
    MCP->>Brain: DelegationResult{status, diff, summary, cost}
```

---

## 6. Delegation State Machine (within MCP Server)

Each delegation tracked by the MCP server transitions through these states:

```mermaid
stateDiagram-v2
    [*] --> Dispatched: DelegationRequest sent via mpsc

    Dispatched --> Active: request_id inserted into<br/>active_delegations HashSet

    Active --> Collecting: spawn_result_collector<br/>awaiting oneshot

    Collecting --> Completed: oneshot received result<br/>→ moved to completed_delegations HashMap<br/>→ removed from active_delegations

    Collecting --> Failed: oneshot channel closed<br/>(orchestrator disconnected)<br/>→ synthetic Failed result

    Completed --> Consumed: Brain polls via<br/>check_delegation_status or<br/>wait_delegation or<br/>delegate_to_worker 250ms loop

    Consumed --> [*]: Result returned, entry removed

    note right of Active
        Brain can query state:
        • active → {status: "running"}
        • completed → result returned
        • neither → "Unknown delegation"
    end note
```

### State Storage

| State | Storage | Lookup |
|---|---|---|
| Active | `Arc<Mutex<HashSet<String>>>` | O(1) contains check |
| Completed | `Arc<Mutex<HashMap<String, DelegationResult>>>` | O(1) remove (returns value) |
| Consumed | Removed from both maps | N/A |

---

## 7. Concurrency Model

```mermaid
graph TB
    subgraph SERVER["McpCallbackServer (Arc‹Self›)"]
        LISTENER["TCP Accept Loop<br/><i>tokio::spawn per connection</i>"]
        H1["Handler Task 1<br/><i>delegate_to_worker</i>"]
        H2["Handler Task 2<br/><i>delegate_async</i>"]
        H3["Handler Task 3<br/><i>check_delegation_status</i>"]
        C1["Collector Task A<br/><i>await oneshot</i>"]
        C2["Collector Task B<br/><i>await oneshot</i>"]
    end

    subgraph SHARED["Shared State (Arc‹Mutex›)"]
        AD["active_delegations<br/>HashSet‹String›"]
        CD["completed_delegations<br/>HashMap‹String, DelegationResult›"]
    end

    subgraph CHANNEL["Channel to Orchestrator"]
        TX["delegation_tx<br/>mpsc::Sender (cloneable)"]
    end

    LISTENER --> H1
    LISTENER --> H2
    LISTENER --> H3
    H1 -->|"lock + insert"| AD
    H2 -->|"lock + insert"| AD
    H3 -->|"lock + check"| AD
    H3 -->|"lock + remove"| CD
    H1 -->|"lock + remove (poll loop)"| CD
    C1 -->|"lock + remove"| AD
    C1 -->|"lock + insert"| CD
    C2 -->|"lock + remove"| AD
    C2 -->|"lock + insert"| CD
    H1 --> TX
    H2 --> TX

    style AD fill:#0f3460,stroke:#0f3460,color:#fff
    style CD fill:#0f3460,stroke:#0f3460,color:#fff
    style TX fill:#e94560,stroke:#e94560,color:#fff
```

**Key properties**:
- `McpCallbackServer` is wrapped in `Arc<Self>` — shared across all connection tasks
- `delegation_tx` is `mpsc::Sender` (cheaply cloneable) — no contention on sends
- `active_delegations` and `completed_delegations` use `tokio::sync::Mutex` — async-aware, no deadlock risk from `.await` inside critical section
- Each `spawn_result_collector` is a fire-and-forget tokio task — no JoinHandle tracked (known gap)
- The 250ms polling loop in `delegate_to_worker` acquires the mutex briefly each iteration

---

## 8. Cross-Crate Interface Map

```mermaid
graph LR
    subgraph spur_mcp["spur-mcp"]
        MCS["McpCallbackServer"]
        WI["WorkerInfo"]
        DR["DelegationRequest"]
        DC["DelegationChannel"]
        TD["ToolDefinition"]
    end

    subgraph spur_acp["spur-acp (dependency)"]
        SID["SessionId"]
        DRES["DelegationResult"]
        DSTAT["DelegationStatus"]
        DPLAN["DelegationPlan"]
        ACFG["AgentConfig"]
        TIER["Tier"]
    end

    subgraph spur_core["spur-core (consumer)"]
        ORCH["Orchestrator::handle_delegations()"]
        BWI["build_worker_info() call site"]
    end

    DR -->|"contains"| SID
    DR -->|"contains"| DPLAN
    DR -->|"oneshot‹T›"| DRES
    DRES -->|"contains"| DSTAT
    MCS -->|"produces"| DR
    DC -->|"wraps mpsc::Receiver‹DR›"| DR
    ORCH -->|"consumes"| DC
    BWI -->|"reads"| ACFG
    BWI -->|"produces"| WI
    MCS -->|"holds Vec‹›"| WI
    MCS -->|"returns via tools/list"| TD

    style spur_mcp fill:#1a1a2e,stroke:#533483,color:#fff
    style spur_acp fill:#1a1a2e,stroke:#e94560,color:#fff
    style spur_core fill:#1a1a2e,stroke:#0f3460,color:#fff
```

### Type Ownership

| Type | Defined In | Used By |
|---|---|---|
| `McpCallbackServer` | `spur-mcp/server.rs` | `spur-core` (creates + starts) |
| `DelegationRequest` | `spur-mcp/tools.rs` | `spur-core` (receives via channel) |
| `DelegationChannel` | `spur-mcp/tools.rs` | `spur-core` (owns receiver end) |
| `WorkerInfo` | `spur-mcp/server.rs` | `spur-mcp` (returned by list_available_workers) |
| `ToolDefinition` | `spur-mcp/tools.rs` | `spur-mcp` (returned by tools/list) |
| `build_worker_info()` | `spur-mcp/server.rs` | `spur-core` (called at startup) |
| `DelegationResult` | `spur-acp` | `spur-mcp` (received via oneshot) |
| `DelegationStatus` | `spur-acp` | `spur-mcp` (in result payloads) |
| `DelegationPlan` | `spur-acp` | `spur-mcp` (passed through from brain) |
| `SessionId` | `spur-acp` | `spur-mcp` (stamped on every request) |

---

## 9. File Map

| File | Lines | Responsibility |
|---|---|---|
| `lib.rs` | ~10 | Module declarations, public re-exports |
| `tools.rs` | ~300 | `ToolDefinition` struct, 11 tool schema definitions, `DelegationRequest`/`DelegationChannel` types |
| `server.rs` | ~700 | `McpCallbackServer`, JSON-RPC types, HTTP transport, tool dispatch, delegation lifecycle, `WorkerInfo`, `build_worker_info()` |

---

## 10. Known Gaps & Future Work

| Issue | Severity | Status | Description |
|---|---|---|---|
| `delegate_async` missing `delegation_plan` in schema | Low | **Fixed** | Schema now matches `delegate_to_worker` — full `delegation_plan` property with candidates, decomposition, chosen, rationale |
| No `cancel_delegation` tool | Low | **Fixed** | New tool sends `__cancel_delegation` sentinel via mpsc and awaits orchestrator response; orchestrator-side handler pending (forward-compatible — returns "not yet wired" until implemented) |
| No TTL on `completed_delegations` | Low | **Fixed** | Values stored as `(DelegationResult, Instant)` tuples; lazy eviction (10-min TTL) in `check_delegation_status` (combined lock) and `wait_delegation` |
| `spawn_result_collector` JoinHandles untracked | Low | **Fixed** | `tokio_util::task::TaskTracker` replaces fire-and-forget `tokio::spawn`; `shutdown()` method closes tracker and awaits all collectors |
| `shutdown()` not called by orchestrator | Low | **Open** | `BrainSession` stores `mcp_handle: JoinHandle` but not `Arc<McpCallbackServer>` — orchestrator aborts the accept loop but never drains the TaskTracker. Collectors still complete via oneshot resolution. Not a regression; graceful drain requires storing the Arc in BrainSession (spur-core change). |
| `__cancel_delegation` blocked by semaphore | Low | **Open** | Cancel sentinel goes through `handle_delegations` → semaphore.acquire(). If all permits are held, cancel blocks until a delegation completes. Fix: skip semaphore for `__` sentinels in orchestrator (spur-core change). |
| Hand-rolled HTTP has no keep-alive | Low | **Open** — intentional | Each tool call opens a new TCP connection; acceptable for localhost single-client |
| No authentication on HTTP listener | Low | **Open** — intentional | Localhost binding is the only security boundary; any local process can call tools |

### Remaining Work (prioritized)

1. **Orchestrator-side `__cancel_delegation` handler** — `spur-core` needs to track delegation `JoinHandle`s by request ID and abort on cancel sentinel. The MCP tool is wired and forward-compatible.
2. **Skip semaphore for `__` sentinels** — In `handle_delegations`, check `agent.starts_with("__")` before `semaphore.acquire()` and bypass the permit. Prevents cancel/PM/cost requests from blocking behind running delegations.
3. **Store `Arc<McpCallbackServer>` in `BrainSession`** — Call `mcp_server.shutdown().await` before aborting `mcp_handle` for graceful TaskTracker drain.

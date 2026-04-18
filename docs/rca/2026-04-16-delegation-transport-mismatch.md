# Root Cause Analysis: Lost Worker Delegation Results (2026-04-16)

## Incident Summary

Brain agent (kiro) delegated review tasks to two workers (claude-code-acp, codex) via SPUR's MCP `delegate_to_worker` tool. Both workers ran successfully and completed with `Success` status. **The brain never received the results.** Both MCP tool calls were killed at exactly 120 seconds by the brain agent's MCP client HTTP timeout, while the workers needed 6+ minutes to complete.

---

## The Smoking Gun: 120-Second Timeout

```
claude-code-acp:
  T+0s     Tool call started (seq=3288)
  T+2s     Worker spawned (seq=3289)
  T+120s   Tool call FAILED — brain MCP client timeout (seq=3454)
  T+364s   Worker completed Success (seq=4276)
           ↑ Result LOST — brain already moved on

codex:
  T+0s     Tool call started (seq=3496)
  T+2s     Worker spawned (seq=3498)
  T+120s   Tool call FAILED — brain MCP client timeout (seq=3945)
  T+302s   Worker completed Success (seq=6412)
           ↑ Result LOST — brain already moved on
```

Both tool calls failed at **exactly 120.0 seconds**. No user messages, no cancellations, no interruptions occurred between start and failure. The brain immediately said: *"The worker agents aren't reachable right now (the ACP transport is down)"* — misinterpreting the timeout as unreachability.

---

## Sequence Diagram: How the Result Gets Lost

```mermaid
sequenceDiagram
    participant Brain as Brain (kiro-cli)
    participant MCP as MCP Callback Server
    participant Orch as Orchestrator
    participant Worker as Worker Agent

    Brain->>MCP: HTTP POST tools/call<br/>delegate_to_worker("claude-code-acp")
    activate MCP
    MCP->>Orch: DelegationRequest via mpsc
    activate Orch
    Orch->>Worker: spawn stdio child process
    activate Worker

    Note over MCP: blocks on oneshot rx.await<br/>(no timeout)

    Note over Brain,MCP: ⏱ 120 seconds pass...

    Brain--xMCP: HTTP client timeout (120s)<br/>TCP connection closed
    deactivate MCP

    Note over Brain: Reports tool_call_update<br/>status=failed, NO rawOutput
    Note over Brain: "worker isn't reachable"<br/>moves on to other work

    Note over Worker: Still running...<br/>163→584 notifications

    Worker->>Orch: Work complete (Success)
    deactivate Worker
    Orch->>MCP: oneshot tx.send(DelegationResult)
    deactivate Orch

    Note over MCP: rx.await resolves with Ok(result)<br/>Tries to write HTTP response...<br/>TCP connection already closed<br/>❌ RESULT DROPPED
```

---

## Interaction Diagram: Full Incident Timeline

```mermaid
sequenceDiagram
    participant User
    participant Brain as Brain (kiro)
    participant MCP as SPUR MCP Server
    participant Orch as Orchestrator
    participant CC as claude-code-acp
    participant CX as codex

    User->>Brain: "delegate to codex-acp for double review"

    rect rgb(255, 230, 230)
        Note right of Brain: Attempt 1: wrong agent name
        Brain->>MCP: delegate_to_worker("codex-acp")
        MCP->>Orch: DelegationRequest
        Orch-->>MCP: Failed: "codex-acp not found"
        MCP-->>Brain: error response
    end

    Brain->>MCP: list_available_workers
    MCP-->>Brain: [claude-code-acp, codex, kiro, gemini]

    rect rgb(255, 245, 230)
        Note right of Brain: Attempt 2: claude-code-acp (120s timeout)
        Brain->>MCP: delegate_to_worker("claude-code-acp")
        MCP->>Orch: DelegationRequest
        Orch->>CC: spawn worker
        activate CC
        Note over MCP: blocking on oneshot...
        Note over Brain,MCP: ⏱ T+120s
        Brain--xMCP: HTTP timeout
        Note over Brain: status=failed
    end

    Brain->>Brain: "claude-code-acp isn't reachable"

    rect rgb(255, 245, 230)
        Note right of Brain: Attempt 3: codex (120s timeout)
        Brain->>MCP: delegate_to_worker("codex")
        MCP->>Orch: DelegationRequest
        Orch->>CX: spawn worker
        activate CX
        Note over MCP: blocking on oneshot...
        Note over Brain,MCP: ⏱ T+120s
        Brain--xMCP: HTTP timeout
        Note over Brain: status=failed
    end

    Brain->>Brain: "workers aren't reachable,<br/>doing review myself"
    Brain->>Brain: grep, read, sequentialthinking<br/>(independent review)

    Note over CC: T+364s: work complete
    CC->>Orch: Success + artifacts
    deactivate CC
    Orch->>MCP: oneshot result (DROPPED)

    Note over CX: T+302s: work complete
    CX->>Orch: Success + artifacts
    deactivate CX
    Orch->>MCP: oneshot result (DROPPED)

    Brain->>User: "Double Review Result:<br/>Worker agents unreachable.<br/>Ran review independently."
```

---

## Interaction Diagram: The Blocking Architecture (Current)

```mermaid
flowchart TD
    subgraph Brain["Brain Agent (kiro-cli)"]
        BT[Tool Call: delegate_to_worker]
        BH[HTTP Client<br/>⏱ 120s timeout]
    end

    subgraph MCP["MCP Callback Server"]
        HD[handle_delegate_to_worker]
        RX["oneshot rx.await<br/>⏱ NO timeout"]
    end

    subgraph Orch["Orchestrator"]
        ED[execute_delegation]
        TX[oneshot tx.send]
    end

    subgraph Worker["Worker Agent"]
        WR[Running 5-10 min]
    end

    BT -->|"HTTP POST"| BH
    BH -->|"TCP connection"| HD
    HD --> RX
    HD -->|"mpsc send"| ED
    ED -->|"spawn"| WR
    WR -->|"complete"| TX
    TX -.->|"result"| RX

    BH -.->|"❌ timeout at 120s<br/>closes TCP"| HD

    style BH fill:#ff6b6b,color:#fff
    style RX fill:#ffa94d,color:#fff
```

---

## Why `wait_delegation` Has the Same Vulnerability

```mermaid
sequenceDiagram
    participant Brain
    participant MCP
    participant Orch
    participant Worker

    Brain->>MCP: delegate_async("codex")
    MCP->>Orch: DelegationRequest
    MCP-->>Brain: {"delegation_id": "abc-123"}
    Note over Brain: ✅ Returns immediately<br/>No timeout risk

    Orch->>Worker: spawn
    activate Worker

    Brain->>MCP: wait_delegation("abc-123")
    activate MCP
    Note over MCP: removes rx from pending_delegations<br/>blocks on rx.await

    Note over Brain,MCP: ⏱ 120s timeout
    Brain--xMCP: HTTP timeout
    deactivate MCP
    Note over Brain: status=failed
    Note over MCP: rx DROPPED (was removed from map)<br/>❌ Cannot retry

    Worker->>Orch: Success
    deactivate Worker
    Orch->>MCP: oneshot result → rx already dropped<br/>❌ RESULT LOST
```

The `wait_delegation` handler removes the oneshot receiver from `pending_delegations` before awaiting it. If the HTTP client times out, the receiver is dropped and the brain cannot retry. **Same result loss as `delegate_to_worker`.**

---

## Evidence from Event Log

Source: `.spur/events/46013-1776320086418-0.ndjson` (6711 events)

### claude-code-acp delegation

| Seq | Time | Event | Detail |
|---|---|---|---|
| 3286 | 13:39:57 | Brain calls `delegate_to_worker` | agent=claude-code-acp |
| 3287 | 13:40:08 | DelegationRequested | orchestrator received |
| 3288 | 13:40:08 | tool_call started | title="@spur-mcp/delegate_to_worker" |
| 3289 | 13:40:10 | WorkerSpawned | claude-code-acp in worktree |
| 3290 | 13:40:10 | DelegationDispatched | executor linked |
| — | — | *163 WorkerNotifications* | worker actively running |
| **3454** | **13:42:08** | **tool_call_update status=failed** | **120.0s — NO rawOutput** |
| 3468 | 13:42:15 | Brain text | *"The claude-code-acp worker isn't reachable"* |
| — | — | *421 more WorkerNotifications* | worker still running |
| 4276 | 13:46:12 | DelegationCompleted | **status=Success** (result lost) |

### codex delegation

| Seq | Time | Event | Detail |
|---|---|---|---|
| 3476 | 13:42:15 | Brain calls `delegate_to_worker` | agent=codex |
| 3495 | 13:42:24 | DelegationRequested | orchestrator received |
| 3496 | 13:42:24 | tool_call started | title="@spur-mcp/delegate_to_worker" |
| 3498 | 13:42:26 | WorkerSpawned | codex in worktree |
| **3945** | **13:44:24** | **tool_call_update status=failed** | **120.0s — NO rawOutput** |
| — | — | *2310 more WorkerNotifications* | worker still running |
| 6412 | 13:47:26 | DelegationCompleted | **status=Success** (result lost) |

---

## Cross-Evaluation: Challenges and Verdicts

| # | Challenge | Verdict |
|---|---|---|
| 1 | Could the MCP server have crashed? | **No.** Brain called other MCP tools (report_progress, grep) successfully after the "failures." Server was alive. |
| 2 | Could a user message have interrupted? | **No.** Zero user messages or cancellation events between tool_call start and failure. |
| 3 | Is 120s a TCP keepalive timeout? | **No.** macOS TCP keepalive is 7200s. The 120.0s precision on both calls points to a hardcoded HTTP client timeout. |
| 4 | Is the timeout in SPUR's code? | **No.** SPUR's MCP server has no timeout on `rx.await`. The timeout is in the brain agent's MCP HTTP client (kiro-cli). |
| 5 | Does `delegate_async` + `wait_delegation` fix it? | **No.** `wait_delegation` also blocks on `rx.await` and is subject to the same 120s timeout. Worse: it removes the rx from the map before awaiting, making retry impossible. |
| 6 | Could the brain have received results later? | **No.** The TCP connection was closed at T+120s. When the oneshot resolves at T+300-364s, the MCP server tries to write to a dead connection. Result is dropped. |

---

## Root Cause

```mermaid
flowchart LR
    A["Brain MCP client<br/>120s HTTP timeout"] -->|kills| B["MCP tool call<br/>(delegate_to_worker)"]
    B -->|"status=failed<br/>no rawOutput"| C["Brain moves on"]
    
    D["Worker runs 5-10 min"] -->|completes| E["Orchestrator sends<br/>result via oneshot"]
    E -->|"TCP closed"| F["❌ Result dropped"]
    
    C --> G["Brain does review<br/>independently"]
    F --> H["6+ min of worker<br/>compute wasted"]

    style A fill:#ff6b6b,color:#fff
    style F fill:#ff6b6b,color:#fff
    style H fill:#ff6b6b,color:#fff
```

**The MCP `delegate_to_worker` tool is a synchronous blocking call with no timeout awareness.** The brain's MCP client enforces a 120-second HTTP request timeout. Worker execution typically takes 5-10 minutes. The timeout fires before the worker completes, killing the tool call and losing the result.

The `wait_delegation` tool has the same vulnerability, making the `delegate_async` + `wait_delegation` pattern equally broken for long-running workers.

---

## Fix Options (Re-ranked After Cross-Evaluation)

### Fix 1 (Recommended): Non-blocking poll with `check_delegation_status`

```mermaid
sequenceDiagram
    participant Brain
    participant MCP
    participant Orch
    participant Worker

    Brain->>MCP: delegate_async("codex")
    MCP->>Orch: DelegationRequest
    Orch->>Worker: spawn
    activate Worker
    MCP-->>Brain: {"delegation_id": "abc-123"}
    Note over Brain: ✅ Immediate return

    Brain->>Brain: Do other work...

    Brain->>MCP: check_delegation_status("abc-123")
    MCP-->>Brain: {"status": "running"}
    Note over Brain: ✅ Immediate return

    Brain->>Brain: Do more work...

    Worker->>Orch: Success + artifacts
    deactivate Worker
    Orch->>MCP: oneshot → stored in results map

    Brain->>MCP: check_delegation_status("abc-123")
    MCP-->>Brain: {"status": "completed", "result": {...}}
    Note over Brain: ✅ Got the result!
```

New `check_delegation_status` tool returns immediately with current status. No blocking, no timeout risk. Brain stays productive between polls.

**Implementation**: spawn a background task per delegation that awaits the oneshot and stores the result in `Arc<Mutex<HashMap<String, DelegationResult>>>`. The poll tool reads from this map.

| Property | Value |
|---|---|
| Scope | spur-mcp (new tool + background task) |
| Effort | Low-Medium |
| Blast radius | None — additive, no existing API changes |
| Brain prompt change | Use delegate_async → poll loop instead of blocking delegate_to_worker |

### Fix 2: Server-side timeout with graceful fallback

MCP server wraps `rx.await` in `tokio::time::timeout(90s)`. On timeout, returns `{"status": "running", "delegation_id": "..."}` instead of blocking forever. Requires changing oneshot to watch channel so the receiver survives across retries.

| Property | Value |
|---|---|
| Scope | spur-mcp + spur-core (channel type change) |
| Effort | Medium |
| Blast radius | Low — changes internal channel type |

### Fix 3: Upgrade to MCP Streamable HTTP with SSE

MCP server sends SSE progress events during the blocking wait, keeping the brain's HTTP client alive. Best UX but requires rewriting the HTTP handler.

| Property | Value |
|---|---|
| Scope | spur-mcp (major HTTP handler rewrite) |
| Effort | High |
| Blast radius | Medium — changes transport protocol |

### Fix 4: Increase brain's HTTP timeout (Workaround)

Configure the brain agent's MCP client to use a longer timeout (e.g., 30 minutes). Doesn't fix the architectural issue — brain is still blocked and unproductive during the wait.

| Property | Value |
|---|---|
| Scope | Brain agent config (external to SPUR) |
| Effort | Low |
| Blast radius | None |

---

## Corrected Assessment

Previous RCA versions incorrectly attributed failures to:
- ~~Transport mismatch between Kiro CLI and SPUR~~ → The Kiro CLI transport errors were from separate manual attempts, unrelated to the brain's MCP calls
- ~~UX gap (silent blocking)~~ → The brain's tool calls were actively killed by the 120s timeout, not just silent
- ~~`delegate_async` + `wait_delegation` as a fix~~ → `wait_delegation` has the same blocking/timeout vulnerability

The real issue: **a 120-second HTTP client timeout kills 5-minute blocking MCP tool calls, losing completed worker results with no recovery path.**

---

## Implementation: Fix Applied

### Design: Spawn-First-Then-Poll

The core insight: `tokio::time::timeout` and `tokio::select!` both consume the oneshot receiver on timeout, dropping it and losing the result. The fix avoids this by **never holding the oneshot in the HTTP handler** — instead, a background task owns the receiver and writes to a shared results map.

```mermaid
sequenceDiagram
    participant Brain
    participant MCP as MCP Server
    participant BG as Background Collector
    participant Orch as Orchestrator
    participant Worker

    Brain->>MCP: delegate_to_worker("codex")
    MCP->>Orch: DelegationRequest (oneshot tx)
    Orch->>Worker: spawn

    MCP->>BG: spawn_result_collector(rx)
    activate BG
    Note over BG: owns the oneshot rx<br/>awaits in background

    loop Poll completed_delegations (250ms)
        MCP->>MCP: check map
        Note over MCP: not found → continue
    end

    Note over MCP: 90s deadline reached

    MCP-->>Brain: "still running, delegation_id=abc-123"
    Note over Brain: ✅ Response before 120s timeout

    Worker->>Orch: Success
    Orch->>BG: oneshot tx.send(result)
    BG->>BG: store in completed_delegations
    deactivate BG

    Brain->>MCP: check_delegation_status("abc-123")
    MCP->>MCP: found in completed_delegations
    MCP-->>Brain: {"status": "Success", ...}
    Note over Brain: ✅ Got the result!
```

### Changes (2 files, spur-mcp only)

**server.rs:**
- `pending_delegations` → `active_delegations: Arc<Mutex<HashSet<String>>>` + `completed_delegations: Arc<Mutex<HashMap<String, DelegationResult>>>`
- `DELEGATION_BLOCK_TIMEOUT = 90s` constant
- `spawn_result_collector()` — background task that awaits oneshot, stores result
- `handle_delegate_to_worker` — spawns collector, polls completed map with 90s deadline
- `handle_delegate_async` — spawns collector instead of storing raw rx
- `handle_wait_delegation` — polls completed map with 90s deadline (was: blocking rx.await)
- `handle_delegate_parallel` — spawns collectors for all, polls with batch 90s deadline
- `handle_check_delegation_status` — new, instant poll of completed map
- Routing added in `handle_tool_call`

**tools.rs:**
- `check_delegation_status_def()` — new tool definition
- Updated `delegate_to_worker_def` description (mentions 90s fallback)
- Updated `wait_delegation_def` description (mentions 90s fallback)
- Added to `tools_list()`

### Verification
- `cargo build --workspace` — clean
- `cargo clippy -p spur-mcp --no-deps` — zero warnings
- `cargo test -p spur-mcp` — 2/2 pass
- Zero cross-crate changes, zero downstream breakage

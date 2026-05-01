# Curated Worker MCP Subset — Design Spec

**Status**: Brainstorm complete; ready for plan-writing
**Date**: 2026-05-02
**Beads**: bd-14cq (parent); follow-up to bd-wjvs
**Approach**: B (fixed worker catalog, MVP with documented gaps)

---

## 1. Motivation

`spur-mcp` is currently the brain's inbound control channel. Workers receive `vec![]` for `mcp_servers` at `crates/spur-core/src/orchestrator.rs:6571-6577`, with the explicit comment `// Workers get no MCP servers (per spec).` Brain sessions receive the full catalog at `crates/spur-core/src/orchestrator.rs:2161`.

The dual-gate code review pilot (bd-wjvs) and similar workflows need workers to **post results back to beads, emit signals, and read context** without round-tripping through the brain. This spec changes the spec: workers may opt in to a curated MCP tool subset by connecting to a new dedicated **Spur Worker MCP Server**.

The brain MCP server, its 11-tool catalog, and all existing brain workflows remain unchanged.

---

## 2. Tool subset

The Worker MCP server exposes **8 tools**, grouped by category:

| Category | Tools |
|---|---|
| **Read** | `get_issue`, `list_issues`, `get_task_diff`, `get_plan_status`, `fetch_outcome_artifact` |
| **Write** | `update_issue` (open scope: any issue ID) |
| **Signal** | `report_signal` |
| **Progress** *(experimental, separately gated)* | `report_progress` |

All other tools (`delegate_*`, `submit_plan`, `merge_plan`, `execute_epic`, `cancel_delegation`, `review_task`, `create_pr`, `create_issue`, `add_dependency`, graph tools, `get_reconciler_status`) remain brain-only and are not exposed on the Worker MCP server.

**Why exclusions matter:**

- `delegate_*` and `cancel_delegation` would let workers spawn or kill sub-workers — a worker pyramid is out of scope.
- `submit_plan`, `merge_plan`, `execute_epic` are plan-lifecycle operations that must remain brain-mediated.
- `review_task` would let a coder worker self-approve its own output, breaking the review-gate invariant.
- `create_pr` is a terminal orchestrator action, not a worker affordance.

---

## 3. Architecture

### 3.1 Topology

A new `WorkerMcpServer` is started **lazily, per `BrainSession`**, the first time the orchestrator dispatches a worker with `enable_worker_mcp = true`. Bound to `127.0.0.1:0` (random ephemeral port). One server is shared by every worker in that brain session; per-request scoping isolates them.

```
BrainSession
├── McpCallbackServer (brain) — port A, 11 tools  [unchanged]
└── WorkerMcpServer  (workers) — port B, 8 tools  [new, lazy]
```

### 3.2 Lifecycle

**Lifecycle is bound to the count of active `enable_worker_mcp=true` delegations, NOT to request idle-time.** This is critical: a worker may spend minutes on local work (build, test, file edits) between MCP calls. Tying lifecycle to request idle would tear down the server mid-task and bind a new ephemeral port on next call, breaking the worker's stale URL.

| Hook | Where | Behavior |
|---|---|---|
| Start | First `enable_worker_mcp=true` delegation begins | `WorkerMcpServer::start()` binds `127.0.0.1:0`, returns `Arc<WorkerMcpServer>` (see §3.2.1). Active-delegation count = 1. |
| Increment | Subsequent `enable_worker_mcp=true` delegation begins | Reuse existing `Arc<WorkerMcpServer>`. Count += 1. |
| Decrement | Worker completes (success or failure) | Count -= 1. If count > 0, server stays up. |
| Stop | Count reaches 0 | Server enters shutdown: drain in-flight requests with 5s timeout (mirrors `shutdown_mcp_server` at `orchestrator.rs:960`), release port, drain audit retry queue, remove Arc from orchestrator's map. |
| Cancel-shutdown | New delegation arrives during shutdown | Atomic check-and-set on a "shutting_down" flag; if a new dispatch wins the race, shutdown is cancelled and the server resumes. Otherwise, the new delegation triggers a fresh `start()`. |
| Forced stop | `Orchestrator::retire_brain_session` | Same as count→0 path, regardless of count. |

`BrainSession` itself is a passive struct (`orchestrator.rs:801`); all lifecycle hooks live on `Orchestrator` methods.

#### 3.2.1 Server handle and storage

```rust
// crates/spur-mcp/src/worker_server.rs
pub struct WorkerMcpServer {
    addr: SocketAddr,
    hmac_key: [u8; 32],
    brain_session_id: BrainSessionId,
    active_delegations: dashmap::DashMap<DelegationId, WorkerSessionState>,
    audit_flush_tx: mpsc::UnboundedSender<FlushMessage>,
    shutdown: tokio_util::sync::CancellationToken,
}

impl WorkerMcpServer {
    /// Bind a new listener and spawn the accept loop. Returns the Arc immediately
    /// (the server is ready to accept connections before this returns).
    pub async fn start(
        brain_session_id: BrainSessionId,
        pm_service: Arc<PmService>,
        feature_gate: Arc<FeatureGate>,
        funnel: FunnelHandle,
    ) -> Result<Arc<Self>, BindError>;

    pub fn url(&self) -> String;  // http://127.0.0.1:PORT/mcp?token=<token>

    pub fn issue_token(&self, delegation_id: &DelegationId, ttl: Duration) -> String;

    pub async fn shutdown(self: Arc<Self>);  // 5s drain, force-abort on timeout
}
```

The orchestrator stores running servers in:

```rust
// crates/spur-core/src/orchestrator.rs
struct Orchestrator {
    // ... existing fields ...
    worker_mcp_servers: Arc<DashMap<BrainSessionId, Arc<WorkerMcpServer>>>,
}
```

Lazy init via `dashmap::Entry::or_try_insert_with` ensures atomic single-start under concurrent dispatch.

### 3.3 Endpoint discovery

Worker subprocess receives the URL **only** via the ACP `session/new` payload's `mcpServers` field. The orchestrator passes:

```rust
let mcp_servers = if ctx.enable_worker_mcp.unwrap_or(false) {
    vec![McpServer::Http(McpServerHttp::new(
        "spur-worker-mcp",
        &format!("{base_url}?token={token}")
    ))]
} else {
    vec![]
};
```

at `orchestrator.rs:6571-6577`. **No env var. No argv.** This matters because env vars leak to all child subprocesses (cargo, git, npm, `printenv` in build scripts) and argv is visible via `ps aux` to all local users. The ACP transport delivers `session/new` over stdin to the worker subprocess only.

### 3.4 Authentication

Per-delegation **bearer token** in the URL: `http://127.0.0.1:PORT/mcp?token=<token>`. The token is a compact signed message — payload + MAC, joined with `.` — modeled on JWT structure but without the unnecessary `alg` header (algorithm is fixed):

```
token := base64url(payload_json) || "." || base64url(HMAC-SHA256(key, base64url(payload_json)))
payload_json := {"d":"<delegation_id>","b":"<brain_session_id>","e":<unix_seconds_expiry>}
```

The HMAC key is 32 random bytes generated once per orchestrator process (held in `Arc<WorkerMcpServer>`). Default token lifetime is `delegation_timeout + 5min grace`, or `1h` if the delegation has no timeout.

**Validation happens in HTTP middleware**, BEFORE the JSON-RPC dispatcher sees the request:

1. Extract `token` from URL query string. Decode + verify HMAC. Reject with `-32600` if invalid.
2. Decode payload; reject with `-32600` if expired (with hardcoded 30s clock-skew tolerance).
3. **Attach `(delegation_id, brain_session_id)` to a request-scoped `WorkerCallContext`.** Handlers receive this context as a parameter — they do **NOT** read `delegation_id` from the JSON-RPC payload, because most tool schemas (`update_issue`, `get_issue`, `list_issues`, `report_signal`) do not have a `delegation_id` field. The token is the sole source of truth for caller identity.

This middleware-extracted-context approach uniformly attributes every call regardless of tool schema. It also closes the spoofing vector: a worker cannot impersonate another worker's `delegation_id` because the token is signed over the canonical identity, not the payload.

Failures emit a `WorkerMcp{subkind: auth-denied}` sentinel (see §5).

The token mitigates three problems at once:
- Local port-scan attackers cannot call tools (no token).
- Workers cannot spoof each other's identity (token signature binds it).
- `update_issue`'s open scope is bounded by the token requirement (only authenticated workers can call it).

### 3.5 `enable_worker_mcp` default

**`enable_worker_mcp` defaults to `false`.** Workers receive `mcp_servers = vec![]` unless the brain explicitly opts in per dispatch. This preserves the historical "Workers get no MCP servers (per spec)" contract for every existing caller and makes the new capability opt-in.

`enable_worker_progress` (the gate for the experimental `report_progress` tool) also defaults to `false`.

### 3.6 Schema additions

```rust
// crates/spur-mcp/src/tool_schemas.rs
pub struct DelegateToWorkerInput {
    // ... existing fields ...
    pub enable_worker_mcp: Option<bool>,        // default false
    pub enable_worker_progress: Option<bool>,   // default false
}

// Same fields added to DelegateParallelTaskInput.
```

### 3.7 `report_progress` dual-gating

Filter at **both** `tools/list` (worker discovery) **and** `tools/call` (dispatch). When `enable_worker_progress=false`:

- `tools/list` response omits `report_progress`.
- `tools/call` with `name="report_progress"` returns JSON-RPC error `-32601 Method not found`. (Belt-and-suspenders against a hardcoded SDK bypass.)

### 3.8 Concurrency

Per-delegation state lives in `dashmap::DashMap<DelegationId, WorkerSession>`. Coarse mutexes only for low-frequency operations (server start/stop, token-key access). The shared listener uses task-per-connection (mirrors brain server).

---

## 4. Data flow — example: `update_issue`

```
Worker (kimi)
  │  tools/call {name:"update_issue", params:{id, comment, ...}}
  │  Authorization: Bearer <token>
  ▼
WorkerMcpServer HTTP middleware
  │  validate token signature + expiry + delegation_id match
  ▼
Dispatch wrapper
  │  emit synchronous spur-audit v1 sentinel on worker's own issue_id
  ▼
PmService::update_issue (shared with brain server)
  │
  ▼
Beads
```

Failure path at any step → JSON-RPC error returned to worker; worker handles it (typically via `report_signal` or task abort).

---

## 5. Audit trail

### 5.1 New sentinel variant

```rust
// crates/spur-mcp/src/plan/audit_sentinel.rs
pub enum AuditSentinelKind {
    // ... existing variants ...
    WorkerMcp {
        delegation_id: String,
        subkind: WorkerMcpSubkind,
        tool_name: Option<String>,
        target_issue_id: Option<String>,
        error: Option<String>,
    },
}

#[serde(rename_all = "kebab-case")]
pub enum WorkerMcpSubkind {
    Call,             // generic per-call audit (write tools)
    AuthDenied,       // token validation failed
    ScopeViolation,   // delegation_id mismatch
    UpstreamFailure,  // PmService unavailable
    FlushFailed,      // read-audit retry exhausted
    PmDegraded,       // PmService unavailable >5min, audit data degrading
}
```

Encoded per `audit_sentinel.rs:200` `encode_comment`: `[[spur-audit v1]]\n{JSON}`.

### 5.2 Audit policy by tool category

| Category | Tools | Audit behavior |
|---|---|---|
| **Write** | `update_issue` | Synchronous `WorkerMcp{subkind: Call}` sentinel per call on worker's own `issue_id`. |
| **Signal** | `report_signal` | No new audit (existing `AuditSentinelKind::Signal` covers it). |
| **Progress** | `report_progress` | No audit (it IS the channel; fire-and-forget). |
| **Read** | 5 read tools | Aggregated per-delegation summary at delegation end (or every 60s for long-running). One sentinel summarizes N reads. |

### 5.3 Read-audit retry queue (fail-soft)

Read-audit summaries are written via `PmService` with **exponential backoff** (30s base, 5min cap), executed by a **dedicated background flusher task** spawned at server start. The flusher owns a `mpsc::UnboundedReceiver<FlushMessage>`; producers (the worker exit hook + the buffer wrapper's `Drop`) send `FlushMessage` records on the corresponding sender.

**Async `Drop` is not used**; Rust does not support it. The synchronous `Drop` impl on the buffer wrapper performs only a non-blocking `try_send` on the unbounded channel:

```rust
impl Drop for ReadAuditBuffer {
    fn drop(&mut self) {
        let entries = std::mem::take(&mut self.entries);
        if !entries.is_empty() {
            let _ = self.flush_tx.send(FlushMessage {
                delegation_id: self.delegation_id.clone(),
                entries,
            });
        }
    }
}
```

The orchestrator's worker exit hook also explicitly calls `worker_server.flush_delegation(delegation_id)` synchronously before allowing the worker session to drop — this drains the active buffer entry from the `DashMap` and pushes it to the flusher channel. Relying on `Arc<DashMap>` deallocation alone does NOT trigger flush; explicit `.remove(delegation_id)` is required.

If `PmService` remains unavailable for >5min, the flusher emits one `WorkerMcp{subkind: PmDegraded}` event via funnel for TUI visibility. **This is fail-soft, not fail-closed**: audit data degrades when PM is unreachable; we accept the gap rather than block worker reads. (True fail-closed — blocking worker reads when PM is down — is a phase-2 option.)

---

## 6. Error handling

| Scenario | Detected at | Behavior |
|---|---|---|
| `WorkerMcpServer` fails to bind port | Orchestrator lazy-init | Return `DelegationDispatchError::WorkerMcpUnavailable` (new variant; uses code `-32002`). Worker NOT spawned. Brain sees error in `delegate_to_worker` result. |
| Missing/malformed/expired token | HTTP middleware | HTTP 401 + JSON-RPC error `-32600`. Emit `WorkerMcp{subkind: AuthDenied}` sentinel (so brain detects spoof attempts). |
| Token valid but `delegation_id` mismatch | Dispatch wrapper | JSON-RPC error `-32602`. Emit `WorkerMcp{subkind: ScopeViolation}`. |
| `PmService` unavailable for write tool | Tool handler | JSON-RPC error to worker. Emit `WorkerMcp{subkind: UpstreamFailure}`. No silent success. |
| `PmService` unavailable for read-audit flush | Audit retry task | Backoff retry per §5.3; degrade after 5min. |
| Worker process crashes mid-call | Worker exit hook | Explicit `.remove(delegation_id)`; buffer wrapper `Drop` attempts final flush. |
| Brain session retired with requests in flight | `retire_brain_session` | Server enters draining; rejects new with `-32001 SessionRetiring` (consistent with `server.rs:241`); waits 5s; force-aborts. Workers may observe EOF/broken-pipe rather than the JSON-RPC error during force-abort — expected race, not a bug. |
| SDK does not honor `mcpServers` in ACP payload | Worker startup | Graceful degradation: worker has no MCP tools; original work still proceeds. Brain detects via per-SDK matrix test. |
| JSON-RPC batched request | HTTP middleware | Reject with `-32600 Invalid Request`. Batched-request support is phase-2. |

---

## 7. Observability

- **Per-delegation summary event**: `SpurEventBody::WorkerMcpDelegationSummary { delegation_id, calls_total, calls_by_tool, p99_latency_ms, errors }` emitted at delegation end (and every 60s for long-running). One event per delegation, not per call — avoids overflowing the broadcast channel (`orchestrator.rs:1705` capacity 4096) and TUI drain cap (`app.rs:4301` `DRAIN_CAP_PER_FRAME=8`).
- **Tracing logs only for MVP** — no metrics counters until phase-2 metrics infrastructure exists.
- Existing per-delegation audit trail captures all writes; aggregated read summaries capture reads (see §5.2).

---

## 8. Backwards compatibility

Guarantees:

- `McpCallbackServer` (brain) at `crates/spur-mcp/src/server.rs:371` — unchanged.
- `tools_list()` (brain catalog) at `crates/spur-mcp/src/tools.rs:816` — unchanged.
- Brain dispatch site at `orchestrator.rs:2161` — unchanged.
- Worker dispatch site at `orchestrator.rs:6571-6577` — adds conditional, but `enable_worker_mcp` defaults to `false`, so existing callers see identical behavior (`vec![]`).
- `DelegateToWorkerInput` / `DelegateParallelTaskInput` schemas — additive optional fields only; existing serialized calls still deserialize.

---

## 9. Implementation surface (estimate)

### 9.1 Handler signature contract

Existing handlers in `crates/spur-mcp/src/server.rs` return three different types:
- `handle_get_issue`, `handle_update_issue`, `handle_get_plan_status`, `handle_fetch_outcome_artifact` → `JsonRpcResponse`
- `handle_get_task_diff` → `Result<String, String>`
- `handle_report_signal` → `anyhow::Result<Value>` (and is **already freestanding** at `server.rs:1198`)

The refactor introduces a unified contract for freestanding handlers consumed by **both** `McpCallbackServer` and `WorkerMcpServer`:

```rust
// crates/spur-mcp/src/handlers.rs (new module)
#[derive(Debug, thiserror::Error)]
pub enum McpHandlerError { ... }  // unifies the existing inconsistent error types

pub async fn get_issue(
    pm: &PmService,
    feature_gate: &FeatureGate,
    ctx: &WorkerCallContext,  // delegation_id, brain_session_id from token middleware
    args: GetIssueArgs,
) -> Result<Value, McpHandlerError>;
// (same shape for the other 7 tools)
```

Each server's dispatcher calls the freestanding handler and converts `Result<Value, McpHandlerError>` to `JsonRpcResponse`. The `WorkerCallContext` parameter is supplied by middleware (token decoding) on `WorkerMcpServer`; on `McpCallbackServer` it is constructed from the brain session.

### 9.2 File-level estimate

| File | Change | LOC |
|---|---|---|
| `crates/spur-mcp/src/worker_server.rs` | NEW: `WorkerMcpServer` struct, HTTP listener, token middleware, dispatch wrapper, active-delegation tracking, background audit flusher task, lazy start/atomic shutdown-cancel, drain-with-timeout | ~800 |
| `crates/spur-mcp/src/handlers.rs` | NEW: freestanding handler functions per §9.1, `McpHandlerError` enum, `WorkerCallContext` struct | ~120 |
| `crates/spur-mcp/src/server.rs` | Refactor: 5 method handlers (`handle_get_issue`, `handle_update_issue`, `handle_get_task_diff`, `handle_get_plan_status`, `handle_fetch_outcome_artifact`) become thin wrappers over the freestanding versions in `handlers.rs`. Note: `handle_report_signal` is already freestanding (`server.rs:1198`) — only its signature is re-aligned to the new contract. `handle_report_progress` does NOT exist yet — added to `handlers.rs` as net-new code. The `handle_get_task_diff` extraction is the heaviest (~210 LOC method body + dependency plumbing for `pm_service`, `feature_gate`, `repo_root`, `load_or_project_plan`). | ~350 |
| `crates/spur-mcp/src/tools.rs` | Add `worker_tools_list()` returning the 8-tool subset | ~40 |
| `crates/spur-mcp/src/plan/audit_sentinel.rs` | Add `WorkerMcp` variant + `WorkerMcpSubkind` enum (kebab-case via existing serde rename) | ~30 |
| `crates/spur-mcp/src/tool_schemas.rs` | Add `enable_worker_mcp` and `enable_worker_progress` fields to `DelegateToWorkerInput` and `DelegateParallelTaskInput` | ~10 |
| `crates/spur-mcp/src/token.rs` | NEW: HMAC token gen + validation per §3.4 wire format | ~120 |
| `crates/spur-mcp/src/lib.rs` | Register new modules | ~5 |
| `crates/spur-core/src/orchestrator.rs` | Lazy `WorkerMcpServer` start via `DashMap::or_try_insert_with`; per-server `worker_mcp_servers` map field; active-delegation count tracking; conditional `mcp_servers` injection at `:6571-6577`; `Orchestrator::retire_brain_session` shutdown integration | ~200 |
| `crates/spur-acp/src/domain/delegation.rs` | New `DelegationDispatchError::WorkerMcpUnavailable` variant (existing enum at `server.rs:235`) — actually lives where `DelegationDispatchError` is defined; cited file may need adjustment when implementing | ~10 |
| `crates/spur-acp/src/domain/events.rs` | Add `SpurEventBody::WorkerMcpDelegationSummary` variant | ~15 |

**Estimate: ~1,700 LOC** (medium-large diff — bumped 2× from the original ~750 estimate after final reviewers correctly flagged the underestimate, particularly the 5-handler `server.rs` refactor and the missing `handlers.rs` shared module).

---

## 10. Testing strategy

| Layer | Tests |
|---|---|
| **Unit** | Token gen/validation (HMAC over `(delegation_id, brain_session_id, expiry)`); audit-buffer `Drop` final-flush; dispatch dual-gate (asserts `report_progress` rejected even when bypassing `tools/list`); `WorkerMcp` sentinel encoding round-trip. |
| **Integration** | Orchestrator → `WorkerMcpServer` → mock worker round-trip; `enable_worker_mcp=false` produces empty `mcpServers` (preserves historical contract); cross-delegation spoof rejected; `update_issue` from worker writes both audit sentinel AND payload. |
| **SDK matrix** | Smoke test for each of 7 SDKs (kimi, gemini, codex, claude-code, opencode, claude-code-sj, kiro): dispatch worker with `enable_worker_mcp=true`, verify the worker CAN call `get_issue` and CANNOT call a non-existent tool. Per-SDK gated CI job. |
| **Security** | Spoofed `delegation_id` rejected; expired token rejected; cross-`brain_session_id` `fetch_outcome_artifact` returns `Unauthorized` (existing `OutcomeKey` scoping at `server.rs:2997-3001`); token NOT present in argv or env at worker subprocess (assertion test on subprocess `Command` builder). |
| **Concurrency** | Stress test — N=8 concurrent workers, mixed read/write tool calls. Asserts: no deadlock, p99 < 500ms, audit buffer integrity preserved, server idle-stop fires after last worker. |
| **Failure injection** | PM failure during read-flush → assert backoff retry; assert `PmDegraded` sentinel fires after 5min. Orchestrator restart mid-worker → worker gets clean error (or EOF), not hang. SDK that ignores 401 and retries → server does not exhaust connection pool (basic rate limit). |

---

## 11. Threat model

| # | Worst case | Mitigation |
|---|---|---|
| 1 | Local process port-scans, finds worker MCP port, calls `update_issue` to deface beads | Bearer token in URL; server rejects requests without valid token. |
| 2 | Worker B spoofs Worker A's `delegation_id` in JSON-RPC payload | Token binds to a specific `delegation_id`; dispatch wrapper rejects mismatch with `ScopeViolation` audit. |
| 3 | Compromised worker spams `update_issue` | Audit sentinel per call makes spam traceable; rate-limiting at server layer (basic) — full quota is phase-2. |
| 4 | Worker tries to read another plan's diffs via `fetch_outcome_artifact` | Existing `OutcomeKey` scopes by `brain_session_id` (`server.rs:2997-3001`); cross-session reads return `Unauthorized`. |
| 5 | SDK leaks token via `printenv` or `ps aux` | Token only in ACP payload (over stdin); never in argv or env. Assertion test enforces this. |

---

## 12. Documented gaps (deferred to phase 2)

These are intentionally out of MVP scope. Each becomes its own bd-* ticket after this spec is approved.

1. **HMAC key rotation across orchestrator restart.** MVP: ephemeral key per orchestrator process; restart invalidates all in-flight tokens — workers must retry to get fresh tokens. Phase-2: key versioning with grace period.
2. **JSON-RPC batched request hardening.** MVP: rejects batches with `-32600`. Phase-2: per-payload validation in batches.
3. **Clock-skew tolerance.** MVP: hardcoded 30s tolerance window. Phase-2: configurable + monotonic vs wall-clock semantics.
4. **Metrics infrastructure.** MVP: `tracing` logs only. Phase-2: Prometheus/OTel counters and histograms (`worker_mcp_requests_total`, `worker_mcp_auth_denied_total`, latency histograms).
5. **HTTP keep-alive on worker server.** MVP: connection-per-request (mirrors brain server per `architecture-spur-mcp.md` §10). Phase-2: keep-alive with stale-token cache invalidation.
6. **Per-delegation rate-limiting / quota.** MVP: basic global rate limit. Phase-2: per-delegation token bucket.
7. **Worker roles (Reviewer / Auditor / Coder / Doc-writer).** MVP: single fixed catalog. Phase-2: role-based catalogs as proposed in Approach C of the brainstorm.

---

## 13. Open questions resolved during brainstorm

- Tool subset: 8 tools, `update_issue` open scope, `report_progress` experimentally gated. ✅
- Topology: shared `WorkerMcpServer` per BrainSession (not per-worker). ✅
- Endpoint discovery: ACP `mcpServers` payload only (env var rejected). ✅
- Authorization: per-delegation bearer token in URL (catalog-trust alone was insufficient). ✅
- Audit trail: write-synchronous; read-aggregated per delegation. ✅
- Backwards compat: `enable_worker_mcp` defaults to `false`. ✅
- Roles: deferred to phase 2 (single fixed catalog for MVP). ✅

---

## 14. References

- `docs/architecture-spur-mcp.md` (current architecture)
- `crates/spur-core/src/orchestrator.rs:6571-6577` (worker dispatch site — modified)
- `crates/spur-core/src/orchestrator.rs:2161` (brain dispatch site — unchanged)
- `crates/spur-mcp/src/server.rs:371` (`McpCallbackServer` — unchanged)
- `crates/spur-mcp/src/server.rs:1198` (`handle_report_signal` — already freestanding; signature re-aligned to new contract)
- `crates/spur-mcp/src/tools.rs:816` (brain `tools_list()` — unchanged)
- `crates/spur-mcp/src/plan/audit_sentinel.rs:71` (`AuditSentinelKind` definition — extended with `WorkerMcp` variant)
- `crates/spur-mcp/src/plan/audit_sentinel.rs:200` (`encode_comment` — sentinel emission helper)
- bd-wjvs (parent investigation)
- bd-14cq (this brainstorm — see comments for full review trail)

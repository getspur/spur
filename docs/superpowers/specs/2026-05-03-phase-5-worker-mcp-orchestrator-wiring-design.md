# Phase 5 — Worker MCP Orchestrator Wiring Design

**Status:** Approved (brainstormed 2026-05-03 via MCTS multi-round evaluation)
**Parent spec:** `docs/superpowers/specs/2026-05-02-curated-worker-mcp-subset-design.md`
**Phase 4 reference:** plan `955aab7b-2410-49e8-8187-c62976c629df`, merged to main as commit `9330d885`
**Beads:** follow-on to bd-u38o (Phase 4 epic)

---

## 1. Goal

Wire the `WorkerMcpServer` (built and merged in Phase 4) into `crates/spur-core/src/orchestrator.rs` so that workers dispatched with `enable_worker_mcp=true` actually receive a per-delegation HMAC-token-authenticated MCP URL. Until Phase 5 lands, the Phase 4 server has no callers — it is dead code.

Phase 5 also plumbs the `enable_worker_mcp` / `enable_worker_progress` flags from the brain's `delegate_to_worker` MCP call all the way to the orchestrator's worker dispatch site (these flags are parsed in Phase 4's `tool_schemas.rs` but not threaded through `DelegationRequest`).

---

## 2. Non-Goals

- **No process-wide singleton refactor.** A multi-tenant `WorkerMcpServer` (one port per orchestrator, brain isolation via URL path) is genuinely better architecture but requires modifying Phase 4's frozen surface. Future ticket.
- **No fix to the existing brain MCP shutdown latency.** `shutdown_mcp_server` (`orchestrator.rs:961`) already blocks `retire_brain_session` for up to 5s via `MCP_SHUTDOWN_TIMEOUT`. Phase 5 must NOT compound this — but fixing the existing case is a separate Phase 4 follow-up.
- **No outcome enum tightening.** `complete_delegation(&str, &str)` takes the outcome as `String` today. Replacing with a typed enum is a Phase 4 follow-up tracked separately.
- **No TUI/dashboard renderer for `WorkerMcpDelegationSummary`.** Event emission lands here; rendering is bd-2947.
- **No SDK matrix CI job.** Per-SDK smoke tests are Phase 6 in the parent plan.

---

## 3. Architecture

### 3.1 Diverging from the parent spec — eager start, not lazy

The parent spec (line 46 of `2026-05-02-curated-worker-mcp-subset-design.md`) prescribed lazy start (per-Orchestrator `DashMap` keyed by `BrainSessionId`, server constructed on first `enable_worker_mcp=true` delegation). After 8 rounds of MCTS-style evaluation against the actual current code, Phase 5 instead **eagerly starts a `WorkerMcpServer` per `BrainSession`**, stored as an `Option<Arc<WorkerMcpServer>>` field on the existing `BrainSession` struct.

**Justification for the divergence:**

| Concern | Lazy verdict | Eager verdict |
|---|---|---|
| Race when two delegations arrive simultaneously | Needs `tokio::sync::OnceCell` per entry (PARking_lot mutex can't be held across await) | Zero race surface — brain bootstrap is single-threaded per session |
| `WorkerMcpServer::start` bind failure | Returns `WorkerMcpUnavailable` per delegation | Stored as `None`, returns `WorkerMcpUnavailable` per delegation (same observable contract) |
| Resource cost when `enable_worker_mcp=false` (the common case today) | Zero | 1 ephemeral port + 1 idle async task per brain |
| Cleanup on `retire_brain_session` | Map-entry removal + shutdown | Field `take()` + shutdown |
| Cleanup on brain crash (retire skipped) | Map entry leaks until orchestrator exit | Same — leaks until orchestrator drops the BrainSession Arc |
| Lines of new code | ~150 (map field + ensure helper + race test) | ~80 (field + 5 call sites) |
| Symmetry with existing brain MCP lifecycle | Asymmetric (brain MCP is eager) | Symmetric |

The lazy approach was justified by hypothetical thousand-brain-deployment scale that does not match current single-developer concurrent usage. Eager wins on simplicity, race elimination, and lifecycle symmetry.

### 3.2 Component diagram

```
BrainSession (orchestrator.rs:802)
├── mcp_server: Option<Arc<McpCallbackServer>>     ← brain MCP (existing)
├── worker_mcp_server: Option<Arc<WorkerMcpServer>>  ← NEW (Phase 5)
└── ...
```

`WorkerMcpServer` constructed in `create_brain_session` and `load_brain_session` immediately after `mcp_server.start().await` returns the brain MCP URL.

### 3.3 Construction sequencing

`WorkerMcpDeps` requires `plan_resolver: Arc<dyn PlanResolver>` and `reconciler_outcomes: Arc<Mutex<OutcomeStore>>`. Both are owned by `McpCallbackServer` (the brain MCP):

- `McpCallbackServer` already implements `PlanResolver` (`server.rs:5860`) — Phase 5 takes `Arc::clone(&mcp_server) as Arc<dyn PlanResolver>`.
- `reconciler_outcomes` is constructed inside `McpCallbackServer::new` (`server.rs:1647`) — Phase 5 needs a new accessor `McpCallbackServer::reconciler_outcomes_arc(&self) -> Arc<Mutex<OutcomeStore>>` to share the same `Arc`.

This forces brain MCP to construct first, worker MCP second. Both happen in the same brain bootstrap critical section, no race.

### 3.4 Bind-failure tolerance

```rust
let worker_mcp_server = match WorkerMcpServer::start(brain_session_id.to_string(), deps).await {
    Ok(server) => Some(server),
    Err(bind_err) => {
        tracing::warn!(
            brain_session_id = %brain_session_id,
            error = %bind_err,
            "WorkerMcpServer bind failed; enable_worker_mcp delegations will return WorkerMcpUnavailable"
        );
        None
    }
};
```

Brain bootstrap continues with `worker_mcp_server = None`. Per-delegation failure path is exactly the parent spec's contract: brain receives `DelegationDispatchError::WorkerMcpUnavailable` with JSON-RPC code `-32002`.

### 3.5 Two-level delegation lifecycle

`register_delegation` and `complete_delegation` (Phase 4 T22/T23) MUST pair 1:1 per delegation_id. The orchestrator has a two-level loop:

- **Outer level** (`execute_delegation`): per-delegation. The `request_id` (= `DelegationRequest.id`) is stable across retries. Owns register/complete pairing.
- **Inner level** (`run_one_worker_attempt`): per-attempt. Each attempt may construct a fresh worker session and worktree.

Phase 5 splits the work accordingly:

| Concern | Where |
|---|---|
| `register_delegation(req_id, ctx)` — once per delegation | `execute_delegation` entry, BEFORE the attempt loop |
| `issue_token(req_id, TTL)` — once per attempt (allows TTL refresh on long retries) | `run_one_worker_attempt` at `:6825-6830` dispatch site |
| URL construction `{server.url()}?token={token}` — per attempt | Same dispatch site |
| `complete_delegation(req_id, outcome)` — once per delegation | `execute_delegation` terminal exit, AFTER the attempt loop |

This guarantees register/complete pair 1:1 even across retry attempts, while letting each attempt issue a fresh short-lived token.

#### 3.5.1 Per-attempt URL construction at `:6825-6830`

The actual worker dispatch site is **`orchestrator.rs:6825-6830`** (inside `run_one_worker_attempt`), NOT the `:6571-6577` cited in the parent spec. Current code:

```rust
// Workers get no MCP servers (per spec).
let session_response = crate::skip_perm::new_session_with_bypass(
    &mut *connection,
    ctx.agent_config,
    worktree_info.path.clone(),
    vec![],
).await
```

Phase 5 replaces the `vec![]` with:

```rust
let mcp_servers = if ctx.enable_worker_mcp {
    match ctx.worker_mcp_server.as_ref() {
        Some(server) => {
            let token = server
                .issue_token(ctx.request_id, TOKEN_TTL)
                .map_err(|e| AttemptSetupError::WorkerMcpTokenIssuance(e.to_string()))?;
            vec![McpServer::Http(McpServerHttp::new(
                "spur-worker-mcp",
                &format!("{}?token={}", server.url(), token),
            ))]
        }
        None => return Err(AttemptSetupError::WorkerMcpUnavailable),
    }
} else {
    vec![]  // Preserves historical contract for default-false callers.
};
```

`TOKEN_TTL = Duration::from_secs(3600)` (1 hour — covers long-running worker tasks).

`WorkerAttemptCtx` (`orchestrator.rs:6635`) gains two new borrow fields threaded down from `execute_delegation`:
- `enable_worker_mcp: bool`
- `worker_mcp_server: Option<&Arc<WorkerMcpServer>>`

`AttemptSetupError` (existing enum) gains two new variants: `WorkerMcpUnavailable` and `WorkerMcpTokenIssuance(String)`. Both are mapped to `DelegationDispatchError::WorkerMcpUnavailable` (JSON-RPC code `-32002`) at the `execute_delegation` boundary. Token issuance failure is bucketed into the same external error because the brain's mitigation is identical: retry without `enable_worker_mcp`.

### 3.6 Per-delegation register/complete in `execute_delegation`

```rust
// At execute_delegation entry, BEFORE the attempt loop:
if delegation_request.enable_worker_mcp {
    if let Some(server) = brain_session.worker_mcp_server.as_ref() {
        server.register_delegation(
            delegation_request.id.to_string(),
            DelegationContext {
                enable_worker_progress: delegation_request.enable_worker_progress,
            },
        );
    }
    // If server is None, T27's per-attempt path will return WorkerMcpUnavailable.
}

// Run attempt loop... (existing code)

// At execute_delegation terminal exit, AFTER the attempt loop, BEFORE returning:
if delegation_request.enable_worker_mcp {
    if let Some(server) = brain_session.worker_mcp_server.as_ref() {
        let outcome_str = match &final_status {
            DelegationStatus::Approved | DelegationStatus::AwaitingReview => "success",
            _ => "error",
        };
        server.complete_delegation(&delegation_request.id.to_string(), outcome_str);
    }
}
```

`complete_delegation`:
- Decrements `active_delegations: AtomicU32` (T22)
- Drops `DelegationDispatchGuard` → fires `WorkerMcpDelegationSummary` event with tool_calls / audits_emitted / duration_ms / outcome
- Removes per-delegation entries from `delegations`, `read_audit_buffers`, `delegation_guards` maps (T20, T22, T23) — prevents unbounded leak across long-lived brain sessions

The parent spec's outcome enum (`String`) is preserved as-is for Phase 5 — tightening to typed enum is a Phase 4 follow-up.

**Failure-path requirement:** the `complete_delegation` call MUST execute on every terminal exit of `execute_delegation`, including panics and early-return error paths. Implementation must use either (a) a `scopeguard`-style RAII drop guard that owns the call, or (b) explicit `complete_delegation` calls on every terminal branch verified by TDD test enumerating all `DelegationStatus` variants. Choice deferred to plan-writing phase.

### 3.7 Detached shutdown in `retire_brain_session`

The existing `shutdown_mcp_server` (`orchestrator.rs:961`) blocks `retire_brain_session` for up to 5s via `MCP_SHUTDOWN_TIMEOUT`. Phase 5 must NOT add another awaited 5s.

```rust
// In retire_brain_session, AFTER existing shutdown_mcp_server call:
if let Some(server) = brain_session.worker_mcp_server.take() {
    tokio::spawn(async move {
        let outcome = server.shutdown(Duration::from_secs(5)).await;
        if !outcome.drained {
            tracing::warn!(
                active = outcome.active_at_deadline,
                "worker MCP background drain deadline elapsed"
            );
        }
    });
}
```

**Properties:**

| Scenario | Behavior |
|---|---|
| Graceful brain session retire (TUI close session) | Returns instantly. Background drain has 5s for in-flight HTTP + audit flush. |
| Whole-spur process exit | `tokio::spawn`'d task is killed by runtime drop. Audit loss on hard exit accepted (identical to runtime drop of any other tokio task). |
| In-flight worker HTTP call when shutdown fires | `WorkerMcpServer::shutdown` (Phase 4 T24) signals via biased select on `shutdown_token`; listener closes; client sees connection drop within ms. |
| Active read-audit buffers | `ReadAuditBuffer::Drop` (Phase 4 T20) sends final entries via channel; background flusher (Phase 4 T21) drains within deadline. |

### 3.8 Flag plumbing — `enable_worker_mcp` and `enable_worker_progress`

Phase 4 added these fields to `tool_schemas::DelegateInput` and `DelegateParallelInput` (`tool_schemas.rs:29,52`) but did NOT thread them through:

```
DelegateInput  →  [DROP]  →  DelegationRequest  →  Orchestrator
                  ↑
            Phase 4 stops here
```

Phase 5 adds the missing plumbing:

1. `DelegationRequest` (`tools.rs:131`) gains two fields:
   ```rust
   pub enable_worker_mcp: bool,
   pub enable_worker_progress: bool,
   ```
   Both default `false` to preserve historical behavior for any caller that doesn't set them.
2. `server.rs` `delegate_to_worker_impl` and `delegate_parallel_impl` handlers populate these fields from `DelegateInput`.
3. Orchestrator dispatch site (`:6825-6830`) reads `delegation_request.enable_worker_mcp`.

### 3.9 New error variant — `DelegationDispatchError::WorkerMcpUnavailable`

Per parent spec §3 failure modes table. Lives in `crates/spur-acp/src/domain/delegation.rs`. JSON-RPC code: `-32002`. Returned to brain via the `delegate_to_worker` response when the brain opts in but the per-brain `WorkerMcpServer` failed to bind during bootstrap.

---

## 4. Tasks (T25–T30, sequential)

| Task | Title | Touches |
|---|---|---|
| T25 | Plumb `enable_worker_mcp` / `enable_worker_progress` through `DelegationRequest`. Add `WorkerMcpUnavailable` variant. | `tool_schemas.rs`, `tools.rs`, `server.rs` (handlers), `delegation.rs` |
| T26 | Add `worker_mcp_server: Option<Arc<WorkerMcpServer>>` to `BrainSession`. Add `reconciler_outcomes_arc()` accessor on `McpCallbackServer`. Construct in both `create_brain_session` and `load_brain_session` paths after brain MCP `.start().await`. Tolerate bind failure → warn + None. | `orchestrator.rs:802` (struct), `:3995` and `:4256` (construction sites), `server.rs` (accessor) |
| T27 | `register_delegation` at `execute_delegation` entry + `complete_delegation` at every terminal exit (1:1 pairing across all `DelegationStatus` branches). | `orchestrator.rs` `execute_delegation` |
| T28 | Conditional `mcp_servers` injection at `:6825-6830` with per-attempt `issue_token`. Thread `enable_worker_mcp` + `worker_mcp_server` borrow into `WorkerAttemptCtx`. New `AttemptSetupError::WorkerMcpUnavailable` and `WorkerMcpTokenIssuance` variants. | `orchestrator.rs:6635` (ctx struct), `:6825-6830` (dispatch site), `run_one_worker_attempt` |
| T29 | Detached `worker_mcp_server.take().shutdown(5s)` via `tokio::spawn` in `retire_brain_session`, before the existing `shutdown_mcp_server` call. Returns instantly. | `orchestrator.rs:1031` |
| T30 | End-to-end smoke test: in-process mock worker via `reqwest` POSTs to the server URL with the issued token, asserts `update_issue` lands an audit sentinel AND `WorkerMcpDelegationSummary` event fires. | `crates/spur-core/tests/e2e_worker_mcp.rs` (NEW) |

Sequential because T26 depends on T25 (struct fields), T27 on T26 (server field exists), T28 on T26+T27 (server field + register_delegation), T29 on T26 (field exists to take), T30 on all of T25-T29.

---

## 5. Test Matrix

Per task, failing-test-first (TDD). Beyond unit tests embedded in each task:

- **T25**: serde round-trip — flag set in `DelegateInput` JSON, asserted on `DelegationRequest` after handler.
- **T26**: bind-failure path — inject a port-exhaustion mock for `WorkerMcpServer::start`, assert brain bootstrap continues and `worker_mcp_server == None`.
- **T27**: register/complete pairing — assert `complete_delegation` called exactly once per delegation across all `DelegationStatus` branches (Approved, AwaitingReview, Rejected, Failed, Cancelled, TimedOut). Plus panic safety test if RAII guard chosen.
- **T28**: three dispatch branches — (a) `enable_worker_mcp=false` → `vec![]`; (b) `enable_worker_mcp=true` + `Some(server)` → URL contains `?token=`; (c) `enable_worker_mcp=true` + `None` → `WorkerMcpUnavailable` propagates as setup error.
- **T29**: detachment assertion — `retire_brain_session` returns within 50ms even when worker MCP has 10 active delegations; warn fires after 5s in background.
- **T30**: full path — orchestrator dispatches mock worker → worker POSTs → audit sentinel lands in beads via `PmService` → `WorkerMcpDelegationSummary` event observed via funnel.

---

## 6. Risks & Mitigations

| Risk | Mitigation |
|---|---|
| Brain bootstrap latency increases by `WorkerMcpServer::start` time (~ms-scale port bind) | Acceptable — same order as existing brain MCP `.start()`. If pathological in CI, time it and gate with metric. |
| Eager start binds a port for sessions that never opt in | Bounded — 1 port + 1 idle task per brain. At current usage (handful of concurrent brains), negligible. Z' singleton refactor is the long-term answer. |
| Detached shutdown means audit data can be lost on `kill -9` | Identical to Phase 4's existing semantics. Documented in `WorkerMcpServer::shutdown` doc comments. |
| `complete_delegation` not called on all error paths in `execute_delegation` | TDD test asserts pairing across all 6 terminal status branches. Code review checklist item. |
| Token TTL of 1h shorter than worker task duration | Configurable later. Sufficient for current worker timeouts (~30min p99). |

---

## 7. Open Questions

None blocking. Logged for tracking:

- **Q1** — Should `MCP_SHUTDOWN_TIMEOUT=5s` for the brain MCP also be detached? (Phase 4 follow-up #9, separate ticket.)
- **Q2** — Should `Z'` singleton refactor be sequenced before scaling beyond ~50 concurrent brains? (Future planning, not Phase 5.)

---

## 8. Acceptance Criteria

Phase 5 is done when:

1. Brain calling `delegate_to_worker(enable_worker_mcp=true)` results in the worker process receiving an MCP URL with a valid HMAC token.
2. Worker can call any of the 8 curated tools and the call hits the orchestrator's `PmService` / plan state correctly.
3. Audit sentinels (`WorkerWrite` for writes, `ReadAggregate` for reads) land in beads as comments on the relevant issues.
4. `WorkerMcpDelegationSummary` event fires once per completed delegation with non-zero `tool_calls` if any tools were called.
5. Brain calling `delegate_to_worker` without `enable_worker_mcp` (default) results in `mcp_servers = vec![]` to the worker — historical contract preserved.
6. `retire_brain_session` returns within 50ms regardless of worker MCP active delegation count.
7. End-to-end smoke test (`crates/spur-core/tests/e2e_worker_mcp.rs`) passes in CI.

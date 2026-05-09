# Orchestrator.rs Modularization Proposal

**Source:** `crates/spur-core/src/orchestrator.rs` (12,999 lines)
**Status:** Super-gold file — critical path, high churn, untenable at ~13 kLOC
**Goal:** Decompose into thematic modules without changing behavior

---

## 1. Current Anatomy

| Region | Lines | % | What Lives There |
|--------|-------|---|------------------|
| Free functions + types (pre-impl) | 1–2,800 | 22 % | Codex discovery, BaseSpec helpers, PM plan loading, `InteractiveInput`, `BrainSession`, `Orchestrator` struct, MCP shutdown helpers |
| `impl Orchestrator` — session mgmt | 2,800–6,000 | 25 % | `run_adhoc`, `run_interactive` (1,229-line god method), `create_brain_session`, `load_brain_session`, `retire_active_brain`, reconnect, connection bootstrap |
| `impl Orchestrator` — delegation | 6,000–10,000 | 31 % | `handle_delegations`, `execute_delegation`, `run_one_worker_attempt`, retry/review gate, worktree cleanup, diff/artifact handling |
| Tail free functions + tests | 10,000–12,999 | 22 % | `review_dispatcher_loop`, peer-mailbox drains, 18 test modules |

**Key smell:** `run_interactive` is ~1,229 lines and couples scheduler logic, connection state, PM command handlers, prompt dispatch, stream draining, cancel deadlines, and continuation merging.

---

## 2. Proposed Module Layout

Convert `orchestrator.rs` → `orchestrator/mod.rs` + submodules.

```
crates/spur-core/src/orchestrator/
├── mod.rs                 # Orchestrator struct def, constructor, Drop, public API surface
├── types.rs               # RunOpts, RunResult, ActiveConnection, BrainSession, error enums
├── input.rs               # InteractiveInput enum + parsing/dispatch helpers
├── session.rs             # Brain session lifecycle (create, load, retire, reconnect)
├── interactive_loop.rs    # run_interactive god-method decomposition
├── connection.rs          # connect_brain, create_connection, attach guards, session listing
├── prompt.rs              # build_brain_prompt v1/legacy, render_header, render_workers_block
├── delegation/
│   ├── mod.rs             # handle_delegations (dispatch loop)
│   ├── execute.rs         # execute_delegation + retry loop + review gate
│   ├── worker_attempt.rs  # run_one_worker_attempt (worktree → prompt → diff → artifact)
│   ├── cleanup.rs         # worktree preserve/commit/remove predicates & apply_worktree_cleanup
│   ├── diff_artifact.rs   # build_diff_summary, decide_artifact_handling, sha256_hex_for_outcome
│   └── finalize.rs        # finalize, flush_then_emit_completed, flush_worker_mcp_audits
├── plan_ops.rs            # load_plan_summaries, canonicalize, lifecycle_from_plan, etc.
├── pm_bridge.rs           # refresh_pm_state, handle_get_issue_graph, issue_to_detail_event
├── worker_mcp.rs          # WorkerMcpFetcher, cache_or_start, build_worker_mcp_servers_with
├── codex_discovery.rs     # list_codex_sessions_from_disk_root, parse_codex_rollout_header, etc.
├── review.rs              # review_dispatcher_loop, cleanup_cancelled_review, apply_decision_to_candidate
├── util.rs                # format_error_chain, truncate_summary, shellexpand_tilde, binary_on_path
└── tests.rs               # All 18+ test modules (or split by theme)
```

---

## 3. Module Contents & Extraction Logic

### 3.1 `types.rs` — (~350 lines extracted)
**What moves:**
- `RunOpts`, `RunResult`
- `ActiveConnection`, `BrainSession` + its `impl` block (including `for_test`)
- `LoadBrainSessionError`, `ReconnectError`
- `InteractiveInput` (or keep in `input.rs` — see §3.3)
- `FaultInjectionHooks`

**Why:** These are pure data definitions with minimal dependencies on the rest of the orchestrator. `BrainSession` is the primary state container; isolating it makes the session module's contract explicit.

---

### 3.2 `session.rs` — (~1,800 lines extracted)
**What moves:**
- `create_brain_session` (~248 lines)
- `load_brain_session` (~342 lines)
- `retire_active_brain` (~140 lines)
- `shutdown_active_brain` (~25 lines)
- `try_reconnect_brain` (~74 lines)
- `reconnect_with_events` (~76 lines)
- `spawn_brain_session` (~20 lines)
- `acquire_attach_guard_for_*` family (~76 lines total)

**Free functions that follow:**
- `abort_mcp_handle`, `cleanup_mcp_on_err`, `shutdown_mcp_server`, `retire_brain_session`
- `RetirableMcpServer` trait + impl for `McpCallbackServer`

**Why:** These methods form a coherent lifecycle graph: `spawn` → `create`/`load` → `retire` → `reconnect`. They share the same state transitions (`BrainSession` ↔ `ActiveConnection`). Extracting them turns `Orchestrator` from a god-object into a coordinator that delegates to a `SessionManager`-like abstraction.

> **Refactoring note:** `retire_active_brain` takes `&mut self` only for `self.funnel`, `self.self_held`, `self.registry`, and `self.worker_mcp_servers`. After extraction it can become a standalone async method that accepts these deps explicitly.

---

### 3.3 `input.rs` — (~120 lines extracted)
**What moves:**
- `InteractiveInput` enum definition (~105 lines)
- `strip_bang_prefix`

**Why:** `InteractiveInput` is the TUI → orchestrator protocol. It is referenced by `run_interactive`, the scheduler, and the review dispatcher. Keeping it in its own module makes the ingress contract visible and versionable.

---

### 3.4 `interactive_loop.rs` — (~1,200 lines extracted)
**What moves:**
- `run_interactive` (~1,229 lines)

**Internal decomposition (within the module):**
The method can be broken into private helpers:
- `dispatch_idle_input()` — handles all non-prompt `InteractiveInput` variants
- `dispatch_prompt_turn()` — scheduler → prompt → stream → turn-complete
- `lazy_spawn_brain()` — brain init from `agent_connection` or cold start

**Why:** `run_interactive` is the single largest method in the entire crate. Extracting it into its own module forces the decomposition into testable sub-routines. The module will depend on `session.rs` (for create/load/retire) and `delegation/mod.rs` (for `handle_delegations` spawn).

---

### 3.5 `connection.rs` — (~300 lines extracted)
**What moves:**
- `connect_brain`
- `selected_brain_name`
- `create_connection`
- `build_connection_from_transport`
- `list_sessions_from_rpc`
- `list_sessions_from_disk`
- `read_session_history_from_disk`
- `init_agents`, `check_agents`

**Why:** Connection bootstrapping is orthogonal to session *management* (which deals with MCP servers, delegation handlers, and notification pumps). Isolating transport creation makes it easier to add new transports or mock connections in tests.

---

### 3.6 `prompt.rs` — (~150 lines extracted)
**What moves:**
- `build_brain_prompt`, `build_brain_prompt_legacy`, `build_brain_prompt_v1`
- `render_header`, `render_workers_block`, `append_issue_and_task`
- `log_prompt_once`
- `enforce_log_cap`

**Why:** Prompt construction is pure string building with no async I/O. Isolating it enables unit testing of prompt content without booting the orchestrator.

---

### 3.7 `delegation/mod.rs` — (~350 lines extracted)
**What moves:**
- `handle_delegations` (~344 lines)
- `DelegationGuard` (internal RAII struct)

**Why:** This is the async dispatch loop that receives `DelegationRequest`s and spawns tasks. Keeping the loop separate from the per-delegation execution logic mirrors the producer/consumer split.

---

### 3.8 `delegation/execute.rs` — (~750 lines extracted)
**What moves:**
- `execute_delegation` (~727 lines)
- `maybe_spawn_dispatch_lease_heartbeat`
- Retry-history types: `RetryAttempt`, `render_retry_context`, `apply_bloat_cap`

**Why:** The retry loop + review gate is a self-contained state machine (Attempt → Review → Retry|Terminal). Extracting it makes the bounded-retry contract testable in isolation.

---

### 3.9 `delegation/worker_attempt.rs` — (~600 lines extracted)
**What moves:**
- `run_one_worker_attempt` (~591 lines)
- `WorkerAttemptOutcome` struct
- `AttemptSetupError` struct
- `candidate_set_for_target`

**Why:** This is the "worker runtime": snapshot → worktree → overlay → prompt → stream → diff → artifact. It is fully self-contained (creates its own `WorktreeManager` and `AgentRegistry`).

---

### 3.10 `delegation/cleanup.rs` — (~100 lines extracted)
**What moves:**
- `should_preserve_worktree`
- `should_commit_worker_diff`
- `apply_worktree_cleanup`

**Why:** Post-gate worktree decisions are pure predicates + one async cleanup routine. Isolating them clarifies the "what happens to the worktree after review?" policy.

---

### 3.11 `delegation/diff_artifact.rs` — (~150 lines extracted)
**What moves:**
- `build_diff_summary`
- `decide_artifact_handling`
- `sha256_hex_for_outcome`
- `truncate_summary`, `truncate_summary_env_default`, `summary_cap_bytes`

**Why:** Diff and artifact handling are worker-output post-processing. They depend on `git` and the outcome store, not on the orchestrator's session state.

---

### 3.12 `delegation/finalize.rs` — (~120 lines extracted)
**What moves:**
- `finalize`
- `flush_then_emit_completed`
- `flush_worker_mcp_audits`
- `emit_flush_failed_audit_sentinel`
- `outcome_for_status`

**Why:** Terminal event emission and MCP audit flushing form a linear pipeline that should be invoked consistently from both the happy path and cancellation paths.

---

### 3.13 `plan_ops.rs` — (~250 lines extracted)
**What moves:**
- `PlanSummaryCandidate`, `PlanSummaryLoad`
- `load_plan_summaries`, `annotate_plan_summary_canonical_epics`
- `canonicalize_plan_summary_candidates`, `duplicate_plan_warning`
- `count_plan_children`, `lifecycle_from_plan`
- `plan_id_from_labels`, `plan_owner_state_from_labels`
- `owner_state_text`, `issue_body_preview`
- `parse_label_value`, `compact_label_component`
- Plan-related constants: `PLAN_COMPLETE_LABEL`, `PLAN_PENDING_LABEL`, etc.

**Why:** Plan summary loading is a self-contained deduplication pipeline. It only needs `PmService` and emits events — no orchestrator state.

---

### 3.14 `pm_bridge.rs` — (~150 lines extracted)
**What moves:**
- `refresh_pm_state`
- `to_summary_event`, `issue_to_detail_event`
- `graph_node_to_event`, `graph_edge_to_event`, `dependency_graph_to_event_parts`
- `handle_get_issue_graph`
- `emit_alerts_from_report`, `build_graph_prompt_summary`
- `IssueGraphPm` trait + impl

**Why:** These are thin adapters between `spur_pm` types and `spur_acp` event types. They form the PM → event-bus bridge.

---

### 3.15 `worker_mcp.rs` — (~120 lines extracted)
**What moves:**
- `WorkerMcpFetcher`
- `build_worker_mcp_servers_with`
- `cache_or_start`

**Why:** Worker MCP server lifecycle is orthogonal to brain session lifecycle. The generic `cache_or_start` helper is reusable.

---

### 3.16 `codex_discovery.rs` — (~120 lines extracted)
**What moves:**
- `filter_sessions_for_repo`
- `list_codex_sessions_from_disk_root`
- `codex_rollout_paths`
- `sorted_child_dirs`, `sorted_rollout_files`
- `parse_codex_rollout_header`, `parse_codex_session_header`
- `json_string_field`

**Why:** Codex-specific filesystem scraping is a standalone concern. It has no deps on the orchestrator beyond `SessionInfo` and `Path`.

---

### 3.17 `review.rs` — (~80 lines extracted)
**What moves:**
- `review_dispatcher_loop`
- `cleanup_cancelled_review`
- `apply_decision_to_candidate`

**Why:** The review dispatcher is a standalone async loop that only needs `ReviewSink` and `InteractiveInput`.

---

### 3.18 `util.rs` — (~80 lines extracted)
**What moves:**
- `format_error_chain`
- `reconnect_failure_event`
- `is_connection_death`
- `startup_beads_warning`, `render_beads_startup_warning`
- `binary_on_path`
- `shellexpand_tilde`, `dirs_home`
- `cancel_mode_for`, `arm_cancel_deadline`

**Why:** Pure utility functions with no domain-specific state.

---

## 4. Remaining in `mod.rs`

After extraction, `orchestrator/mod.rs` should contain:

```rust
// ── Public API types (re-exports) ─────────────────────────────────────
pub use types::{ActiveConnection, BrainSession, RunOpts, RunResult};
pub use input::InteractiveInput;

// ── Orchestrator struct (still the central coordinator) ───────────────
pub struct Orchestrator { ... }  // ~20 fields

// ── Constructor & Drop ────────────────────────────────────────────────
impl Orchestrator {
    pub fn new(...) -> Result<Self>
    pub fn with_pm_service(self, ...) -> Self
    pub fn set_continuation_tx(...)
    pub fn subscribe(&self) -> broadcast::Receiver<SpurEvent>
    pub fn cancellation_control(&self) -> CancellationControl
    pub fn spawn_license_runtime(&self, ...) -> JoinHandle<()>
    // ... other thin setters
}

impl Drop for Orchestrator { ... }

// ── High-level entry points (delegate to submodules) ──────────────────
impl Orchestrator {
    pub async fn run_adhoc(&mut self, ...) -> Result<RunResult>
    pub async fn run_interactive(mut self, ...) -> Result<()>
    pub async fn exec_direct(&mut self, ...) -> Result<RunResult>
}

// ── Thin forwarding methods ───────────────────────────────────────────
// e.g., emit(), session_config_options(), spur_agent_caps(), etc.
```

**Target size for mod.rs:** ~400–500 lines (vs. today's 13,000).

---

## 5. Dependency Graph (proposed)

```
                 ┌─────────────┐
                 │   mod.rs    │
                 │ (coordinator)│
                 └──────┬──────┘
        ┌───────────────┼───────────────┐
        ▼               ▼               ▼
   ┌─────────┐   ┌────────────┐   ┌─────────────┐
   │ types   │   │  session   │   │ interactive_loop
   └────┬────┘   └─────┬──────┘   └──────┬──────┘
        │              │                  │
        └──────────────┼──────────────────┘
                       ▼
              ┌─────────────────┐
              │   connection    │
              └────────┬────────┘
                       │
        ┌──────────────┼──────────────┐
        ▼              ▼              ▼
   ┌─────────┐  ┌──────────┐  ┌───────────┐
   │ prompt  │  │ pm_bridge│  │ delegation/
   └─────────┘  └──────────┘  └─────┬─────┘
                                    │
                    ┌───────────────┼───────────────┐
                    ▼               ▼               ▼
              ┌─────────┐    ┌──────────┐    ┌──────────┐
              │ worker_ │    │  plan_ops│    │  review  │
              │ mcp     │    └──────────┘    └──────────┘
              └─────────┘
```

**No circular deps expected.** All extracted modules depend on `types.rs` (for `BrainSession`, `RunResult`, etc.) and external crates (`spur_acp`, `spur_pm`, `spur_mcp`). `mod.rs` depends on all submodules for its public API.

---

## 6. Migration Strategy

### Phase 1 — Extract zero-risk modules (no `impl Orchestrator` methods)
1. `types.rs` — move structs/enums
2. `input.rs` — move `InteractiveInput`
3. `codex_discovery.rs` — move disk-scraping functions
4. `util.rs` — move pure helpers
5. `pm_bridge.rs` — move event-mapping functions
6. `plan_ops.rs` — move plan summary loading
7. `review.rs` — move review dispatcher loop
8. `worker_mcp.rs` — move cache helper + fetcher

**Risk:** Low. These are mostly free functions and data types.

### Phase 2 — Extract self-contained `impl Orchestrator` method groups
9. `prompt.rs` — `build_brain_prompt*` family (no mutable state)
10. `connection.rs` — `connect_brain`, `create_connection`, `list_sessions_*`

**Risk:** Low-medium. Methods still take `&self` / `&mut self`, but only read `registry`, `config`, `repo_root`.

### Phase 3 — Extract the big ones
11. `delegation/` directory — `handle_delegations`, `execute_delegation`, `run_one_worker_attempt`, cleanup, finalize, diff/artifact
12. `session.rs` — `create_brain_session`, `load_brain_session`, `retire_active_brain`, reconnect family

**Risk:** Medium. These methods touch many orchestrator fields. Refactor to pass deps explicitly where possible.

### Phase 4 — Decompose `run_interactive`
13. `interactive_loop.rs` — extract the god method and decompose into private helpers

**Risk:** Medium-high. This is the most coupled code. Extensive testing required.

### Phase 5 — Tests
14. Move test modules to `tests.rs` or co-locate with their subject modules.

---

## 7. Success Metrics

| Metric | Before | After (target) |
|--------|--------|----------------|
| `orchestrator.rs` / `mod.rs` lines | 12,999 | < 500 |
| Largest single method (`run_interactive`) | 1,229 | < 200 (after decomposition) |
| Largest single file | 12,999 | < 1,500 (e.g. `delegation/execute.rs`) |
| Test modules co-located | 0 (all inline) | 100 % in `tests.rs` or submodule `#[cfg(test)]` |
| Public API breakage | N/A | Zero — all re-exports preserved |

---

## 8. Open Questions

1. **Should `delegation/` be a directory or flat `delegation.rs`?**
   - Recommendation: Directory. The 5 sub-modules (execute, worker_attempt, cleanup, diff_artifact, finalize) are independently testable.

2. **Should tests move to a separate `tests/` integration directory?**
   - Recommendation: No. Keep unit tests co-located as `#[cfg(test)]` modules inside each submodule. Integration tests already live in `crates/spur-core/tests/`.

3. **Should `InteractiveInput` stay in `types.rs` or get its own `input.rs`?**
   - Recommendation: `input.rs`. It is the ingress protocol and will likely grow as the TUI adds commands.

4. **How to handle the `test_support` module?**
   - It currently exposes test-only helpers (~254 lines). Keep it in `mod.rs` behind `#[cfg(any(test, feature = "test-support"))]` or move to `tests/support.rs`.

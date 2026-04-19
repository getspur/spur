# Phase 3a — Low-Risk Hardening Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close six post-INV-1..7 deferred-hardening items (schema drift, lax validation, retry duplication, Cancelled UX gap, replay-safety, Pending-at-exit counting) without changing any brain-visible tool surface.

**Architecture:** Additive changes with zero API-shape breaks. Introduce `schemars` as the single source of truth for delegate-* tool schemas; tighten MCP-boundary validation to return explicit errors instead of silent `None`; extract `RetryLoop` combinator from orchestrator.rs:3046 and test_support's `run_gate_with_retries`; add `PlanTaskStatus::Cancelled` with non-cascading semantic; add a third orphan-buffer in lineage for `DelegationDispatched`-before-`WorkerSpawned`; mark non-terminal plan tasks as `Failed` on `run_plan` exit.

**Tech Stack:** Rust 2024, tokio, rmcp 1.4, schemars (new workspace dep), serde_json, tracing.

**Parent context:** Phase 1..2 (INV-1..7) landed as merge `010a8e2`. Deferred observations list is captured in the conversation that produced this plan; this plan is the execution of that list.

---

## File Structure

No new crates; all work stays inside `spur-acp`, `spur-core`, `spur-mcp`. One new module in `spur-core`.

Files touched:

- `Cargo.toml` (workspace) — add `schemars = "0.8"` to `[workspace.dependencies]`.
- `crates/spur-mcp/Cargo.toml` — depend on `schemars`.
- `crates/spur-mcp/src/tools.rs` — replace three hand-rolled schemas with derived schemas.
- `crates/spur-mcp/src/server.rs` — tighten `delegation_plan` deserialization at three handlers.
- `crates/spur-acp/src/domain/delegation.rs` — add `#[derive(JsonSchema)]` to `DelegationPlan`, `PlanCandidate`, `PlanSubtask` (and the `Delegate*Input` structs introduced here).
- `crates/spur-core/src/retry_loop.rs` (new) — `RetryLoop` combinator + `RetryOutcome` enum.
- `crates/spur-core/src/lib.rs` — `pub mod retry_loop;` export.
- `crates/spur-core/src/orchestrator.rs` — call `RetryLoop::run` at the production retry site (lines 3046+); delete inline retry bookkeeping.
- `crates/spur-core/src/orchestrator.rs` (test_support section) — `run_gate_with_retries` delegates to `RetryLoop`.
- `crates/spur-mcp/src/plan.rs` — add `PlanTaskStatus::Cancelled { reason }`, handle it in `build_plan_status`, `dispatch_newly_ready`, terminal-exit cleanup, and both result-match arms; update PlanCompleted to include a new `cancelled: u32` field.
- `crates/spur-acp/src/domain/events.rs` — add `cancelled: u32` to `SpurEventBody::PlanCompleted`.
- `crates/spur-core/src/lineage/projection.rs` — add `pending_dispatch_by_executor_id: HashMap<String, (String, Option<String>)>`.
- `crates/spur-core/src/lineage/adapter.rs` — buffer `DelegationDispatched` when executor absent; drain on `WorkerSpawned`; `tracing::warn!` on duplicate `DelegationRequested` with differing payload.
- `crates/spur-mcp/src/plan.rs` (end of `run_plan` loop) — mark non-terminal tasks Failed on exit.
- Tests: one integration test per task, listed per-task below.

Out of scope (explicit non-goals):
- Tool cardinality (UP-2) — deferred to Phase 3d.
- Backpressure (UP-3) — deferred to Phase 3d.
- God-file decomposition (DN-1) — Phase 3b.
- Cancel → subprocess lifecycle (DN-3) — Phase 3c.

---

### Task 1: UP-1 — schemars-derived delegate-* schemas

**Files:**
- Modify: `Cargo.toml` (workspace root)
- Modify: `crates/spur-mcp/Cargo.toml`
- Modify: `crates/spur-acp/Cargo.toml`
- Modify: `crates/spur-acp/src/domain/delegation.rs:90-115`
- Create: `crates/spur-mcp/src/tool_schemas.rs`
- Modify: `crates/spur-mcp/src/tools.rs:56-362`
- Modify: `crates/spur-mcp/src/lib.rs`
- Test: `crates/spur-mcp/tests/tool_schema_stability.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-mcp/tests/tool_schema_stability.rs`:

```rust
//! UP-1: delegate_* tool schemas are derived from one shared Input struct.
//! After this task, all three delegate_* tools must share a single schema
//! derivation for the DelegationPlan sub-object (no hand-rolled divergence).

use spur_mcp::tools::{
    delegate_to_worker_def, delegate_parallel_def, delegate_async_def,
};

fn plan_schema_of(tool_def: &rmcp::model::Tool) -> serde_json::Value {
    let schema = serde_json::to_value(&tool_def.input_schema).unwrap();
    // The `delegation_plan` sub-schema can be at different nesting depths:
    //   - delegate_to_worker / delegate_async: top-level .properties.delegation_plan
    //   - delegate_parallel: per-task inside .properties.tasks.items.properties.delegation_plan
    //     AND a batch-level top-level .properties.delegation_plan (described differently).
    // This test checks the TOP-LEVEL delegation_plan — per-task is covered below.
    schema["properties"]["delegation_plan"].clone()
}

#[test]
fn delegate_to_worker_and_async_share_the_same_delegation_plan_schema() {
    let a = plan_schema_of(&delegate_to_worker_def());
    let b = plan_schema_of(&delegate_async_def());
    assert_eq!(a, b, "delegate_to_worker and delegate_async must share the derived DelegationPlan schema");
}

#[test]
fn delegate_parallel_batch_level_plan_uses_same_schema() {
    let batch = plan_schema_of(&delegate_parallel_def());
    let single = plan_schema_of(&delegate_to_worker_def());
    assert_eq!(batch, single, "delegate_parallel batch-level plan must match single-task plan");
}

#[test]
fn delegate_parallel_per_task_plan_matches_single_task_schema() {
    let schema: serde_json::Value = serde_json::to_value(
        &delegate_parallel_def().input_schema
    ).unwrap();
    let per_task = &schema["properties"]["tasks"]["items"]["properties"]["delegation_plan"];
    let single = plan_schema_of(&delegate_to_worker_def());
    assert_eq!(per_task, &single, "per-task delegation_plan inside delegate_parallel must match");
}

#[test]
fn delegation_plan_has_expected_top_level_fields() {
    let schema = plan_schema_of(&delegate_to_worker_def());
    let props = &schema["properties"];
    assert!(props.get("candidates").is_some());
    assert!(props.get("decomposition").is_some());
    assert!(props.get("chosen").is_some());
    assert!(props.get("rationale").is_some());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-mcp --test tool_schema_stability`
Expected: FAIL — the three schemas currently differ in depth (delegate_parallel's batch-level plan uses bare `{"type": "array"}` for candidates/decomposition; per-task plan is `{"type": "object"}` with no properties).

- [ ] **Step 3: Add schemars to the workspace**

In `Cargo.toml` (workspace root), inside `[workspace.dependencies]`, add (keep alphabetical):

```toml
schemars = "0.8"
```

In `crates/spur-mcp/Cargo.toml`, inside `[dependencies]`, add:

```toml
schemars = { workspace = true }
```

In `crates/spur-acp/Cargo.toml`, inside `[dependencies]`, add:

```toml
schemars = { workspace = true }
```

- [ ] **Step 4: Derive JsonSchema on DelegationPlan**

In `crates/spur-acp/src/domain/delegation.rs`, find `pub struct DelegationPlan` around line 90. Add `JsonSchema` to its derive list and do the same for `PlanCandidate`, `PlanSubtask` (whatever the companion types are):

```rust
#[derive(Debug, Clone, Default, Deserialize, Serialize, schemars::JsonSchema)]
pub struct DelegationPlan {
    #[serde(default)]
    pub candidates: Vec<PlanCandidate>,
    #[serde(default)]
    pub decomposition: Vec<PlanSubtask>,
    pub chosen: Option<String>,
    pub rationale: Option<String>,
}
```

Apply `#[derive(schemars::JsonSchema)]` to `PlanCandidate` and `PlanSubtask` similarly. If any field uses a type that isn't `JsonSchema`, either derive it there too or annotate with `#[schemars(with = "String")]` as a fallback.

- [ ] **Step 5: Create the shared Input structs**

Create `crates/spur-mcp/src/tool_schemas.rs`:

```rust
//! UP-1: single-source-of-truth input schemas for delegate_* tools.
//!
//! `rmcp::model::Tool.input_schema` expects a `serde_json::Value`; we
//! produce it by `schemars::schema_for!(...)` and serializing to JSON.
//! These structs MUST mirror the DelegationRequest surface the handlers
//! expect — changing a field here is a brain-visible change.

use schemars::JsonSchema;
use serde::Deserialize;
use spur_acp::DelegationPlan;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateToWorkerInput {
    /// Name of the agent to delegate to (e.g. "coder", "reviewer").
    pub agent: String,
    /// The task description the worker will execute.
    pub task: String,
    #[serde(default)]
    pub context_files: Vec<String>,
    #[serde(default)]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub delegation_plan: Option<DelegationPlan>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateAsyncInput {
    pub agent: String,
    pub task: String,
    #[serde(default)]
    pub context_files: Vec<String>,
    #[serde(default)]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub delegation_plan: Option<DelegationPlan>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateParallelTask {
    pub agent: String,
    pub task: String,
    #[serde(default)]
    pub context_files: Vec<String>,
    #[serde(default)]
    pub issue_id: Option<String>,
    #[serde(default)]
    pub delegation_plan: Option<DelegationPlan>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DelegateParallelInput {
    pub tasks: Vec<DelegateParallelTask>,
    #[serde(default)]
    pub delegation_plan: Option<DelegationPlan>,
}

/// Render a `schemars::schema::RootSchema` as a `serde_json::Value` for
/// use in `rmcp::model::Tool.input_schema`.
pub fn schema_value<T: JsonSchema>() -> serde_json::Value {
    let schema = schemars::schema_for!(T);
    serde_json::to_value(schema).expect("schema serialization")
}
```

- [ ] **Step 6: Export and rewrite the three tool-def builders**

In `crates/spur-mcp/src/lib.rs`, add:

```rust
pub mod tool_schemas;
```

In `crates/spur-mcp/src/tools.rs`, replace each `fn delegate_*_def()` body so that its `input_schema` is derived. Example for `delegate_to_worker_def`:

```rust
pub fn delegate_to_worker_def() -> Tool {
    Tool {
        name: "delegate_to_worker".into(),
        description: Some(
            "Delegate a single task to a named worker. Blocks up to 90 seconds \
             for completion. For long-running work, prefer delegate_async and \
             wait_delegation. See list_available_workers for agent names."
                .into(),
        ),
        input_schema: std::sync::Arc::new(
            crate::tool_schemas::schema_value::<
                crate::tool_schemas::DelegateToWorkerInput
            >()
                .as_object()
                .expect("derived schema is an object")
                .clone(),
        ),
        annotations: None,
    }
}
```

Do the same for `delegate_async_def` (use `DelegateAsyncInput`) and `delegate_parallel_def` (use `DelegateParallelInput`). Delete the hand-rolled `schemars::schema::SchemaObject` / `serde_json::json!({...})` bodies entirely — preserve only the `name`, `description`, and `annotations` on each tool. The descriptions MUST stay human-readable and tool-specific — don't factor them out.

- [ ] **Step 7: Run tests and verify they now pass**

Run: `cargo test -p spur-mcp --test tool_schema_stability`
Expected: PASS (all four tests).

Then run the full MCP crate tests:
Run: `cargo test -p spur-mcp`
Expected: PASS. If existing tests were asserting field-by-field on the hand-rolled schemas, they may break — update those to match the new derived shape.

- [ ] **Step 8: Clippy + fmt**

Run: `cargo clippy -p spur-mcp -p spur-acp --all-targets -- -D warnings`
Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "feat(spur-mcp): UP-1 — derive delegate_* schemas from shared Input structs

Replaces three hand-rolled JSON schema literals with schemars-derived
schemas from DelegateToWorkerInput / DelegateAsyncInput /
DelegateParallelInput. Single source of truth for the DelegationPlan
sub-object. Adds deny_unknown_fields so malformed args surface at the
serde layer instead of being silently dropped by Task 2."
```

---

### Task 2: UP-4 — strict MCP-boundary validation for delegation_plan

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:693-695` (delegate_to_worker path)
- Modify: `crates/spur-mcp/src/server.rs:236-238` (parse_parallel_tasks path)
- Modify: `crates/spur-mcp/src/server.rs:1979-1981` (delegate_async path)
- Test: `crates/spur-mcp/tests/delegation_plan_validation.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-mcp/tests/delegation_plan_validation.rs`:

```rust
//! UP-4: malformed delegation_plan must surface an error to the brain,
//! not silently coerce to None.

use spur_mcp::server::McpCallbackServer;
use serde_json::json;

#[tokio::test]
async fn delegate_to_worker_rejects_malformed_delegation_plan() {
    // Build a McpCallbackServer with no-op transport.
    let server = McpCallbackServer::for_test_default();
    let args = json!({
        "agent": "coder",
        "task": "do a thing",
        "delegation_plan": "this is a string, not an object"  // WRONG TYPE
    });

    let result = server.handle_tool_call("delegate_to_worker", args).await;

    match result {
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("delegation_plan") && msg.contains("invalid"),
                "error must name the offending field: {msg}"
            );
        }
        Ok(resp) => panic!("expected validation error, got success: {resp:?}"),
    }
}

#[tokio::test]
async fn delegate_to_worker_rejects_delegation_plan_with_wrong_inner_types() {
    let server = McpCallbackServer::for_test_default();
    let args = json!({
        "agent": "coder",
        "task": "do a thing",
        "delegation_plan": {
            "candidates": "not an array",
            "chosen": 42  // wrong inner type
        }
    });

    let result = server.handle_tool_call("delegate_to_worker", args).await;

    assert!(result.is_err(), "malformed inner fields must be rejected");
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("delegation_plan"), "error must name the field: {msg}");
}

#[tokio::test]
async fn delegate_to_worker_accepts_absent_delegation_plan() {
    let server = McpCallbackServer::for_test_default();
    let args = json!({
        "agent": "coder",
        "task": "do a thing"
        // no delegation_plan at all — must succeed
    });
    let result = server.handle_tool_call("delegate_to_worker", args).await;
    assert!(
        result.is_ok() || matches!(&result, Err(e) if !format!("{e}").contains("delegation_plan")),
        "absent delegation_plan must be a clean None, not an error"
    );
}
```

If `McpCallbackServer::for_test_default` and `handle_tool_call` don't exist in that exact shape, adapt to the actual construction pattern already used by existing tests in `crates/spur-mcp/tests/`. The test's intent is what matters: parse via the handler entry point and verify a clear error vs success.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-mcp --test delegation_plan_validation`
Expected: FAIL — current code silently coerces via `.ok()`, so the malformed-plan tests pass through as if plan were None, producing success or a different error.

- [ ] **Step 3: Introduce a helper that returns Result**

In `crates/spur-mcp/src/server.rs`, near the top-level helpers section, add:

```rust
/// UP-4: Parse `delegation_plan` from a JSON value map with strict
/// validation. Absent → `Ok(None)`. Present but malformed → `Err` with
/// the serde message, so the brain sees a real error instead of silent
/// coercion.
fn parse_delegation_plan(
    container: &serde_json::Value,
) -> Result<Option<spur_acp::DelegationPlan>, String> {
    match container.get("delegation_plan") {
        None => Ok(None),
        Some(serde_json::Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| format!("invalid delegation_plan: {e}")),
    }
}
```

- [ ] **Step 4: Replace the three silent-coerce sites**

In `crates/spur-mcp/src/server.rs`, find each occurrence of:

```rust
let delegation_plan: Option<spur_acp::DelegationPlan> = args
    .get("delegation_plan")
    .and_then(|v| serde_json::from_value(v.clone()).ok());
```

(and the `task_obj`-variant inside `parse_parallel_tasks`).

Replace each with:

```rust
let delegation_plan = parse_delegation_plan(args)
    .map_err(|e| /* use the existing error-construction idiom for this handler */)?;
```

For `handle_delegate_to_worker` and `handle_delegate_async` (lines 693 and 1979), the "existing error-construction idiom" returns an `rmcp::Error` or equivalent — mirror what happens when `agent` is missing. Grep the enclosing function for an existing `.ok_or_else(|| ...)` or `.map_err(|e| ...)` to find the pattern.

For `parse_parallel_tasks` (line 236), the function should propagate via its existing error type (probably `Result<Vec<...>, rmcp::Error>` or `Result<Vec<...>, String>`). Use the same idiom.

Each handler's call site changes from a silent `.ok()` to a propagated error. The three call sites end up reading:

```rust
let delegation_plan = parse_delegation_plan(args).map_err(invalid_params_error)?;
```

where `invalid_params_error` is whatever the handler's existing "bad params" helper is named — reuse, don't invent.

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p spur-mcp --test delegation_plan_validation`
Expected: PASS.

- [ ] **Step 6: Regression**

Run: `cargo test -p spur-mcp`
Expected: PASS. If any existing test passed a malformed `delegation_plan` and expected silent coercion, fix the test to pass a valid one — silent coercion was the bug.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix(spur-mcp): UP-4 — strict delegation_plan validation at MCP boundary

parse_delegation_plan surfaces serde errors to the brain instead of
.ok()-coercing malformed input to None. A wrong-type or malformed
delegation_plan now returns an 'invalid delegation_plan: <serde msg>'
error; absent remains Ok(None)."
```

---

### Task 3: DN-2 — extract RetryLoop combinator

**Files:**
- Create: `crates/spur-core/src/retry_loop.rs`
- Modify: `crates/spur-core/src/lib.rs`
- Modify: `crates/spur-core/src/orchestrator.rs:3046-3065` (production retry site)
- Modify: `crates/spur-core/src/orchestrator.rs:4266-4326` (test_support run_gate_with_retries)
- Test: `crates/spur-core/tests/retry_loop.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-core/tests/retry_loop.rs`:

```rust
//! DN-2: RetryLoop combinator — bound, error-message, and outcome
//! mapping are exercised here so both the production and test-support
//! retry call sites can delegate without divergence.

use spur_acp::DelegationStatus;
use spur_core::retry_loop::{RetryLoop, RetryOutcome};
use std::sync::atomic::{AtomicU32, Ordering};

#[tokio::test]
async fn retry_loop_returns_terminal_on_first_terminal() {
    let rl = RetryLoop::new(3);
    let result = rl
        .run(|_n| async {
            RetryOutcome::Terminal(DelegationStatus::Success)
        })
        .await;
    assert!(matches!(result, DelegationStatus::Success));
}

#[tokio::test]
async fn retry_loop_counts_attempts_and_fails_after_limit_plus_one() {
    let rl = RetryLoop::new(3);
    let counter = AtomicU32::new(0);
    let result = rl
        .run(|n| {
            counter.store(n, Ordering::SeqCst);
            async move { RetryOutcome::Retry }
        })
        .await;
    // max=3: attempts 1,2,3,4 run; on attempt 4 the Retry outcome
    // exceeds the bound (n > 3) and returns Failed.
    assert_eq!(counter.load(Ordering::SeqCst), 4);
    match result {
        DelegationStatus::Failed { error } => {
            assert!(error.contains("retry limit exceeded after 4 attempts"), "msg: {error}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn retry_loop_terminal_short_circuits_mid_loop() {
    let rl = RetryLoop::new(5);
    let result = rl
        .run(|n| async move {
            if n == 2 {
                RetryOutcome::Terminal(DelegationStatus::Rejected { reason: "no".into() })
            } else {
                RetryOutcome::Retry
            }
        })
        .await;
    match result {
        DelegationStatus::Rejected { reason } => assert_eq!(reason, "no"),
        other => panic!("expected Rejected, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --test retry_loop`
Expected: FAIL — `retry_loop` module does not exist.

- [ ] **Step 3: Implement the combinator**

Create `crates/spur-core/src/retry_loop.rs`:

```rust
//! DN-2: the review-decision retry loop — used by both the production
//! orchestrator and the test_support helper. Invariants preserved from
//! orchestrator.rs:3046+:
//!
//! - `attempt_n: u32` starts at 1.
//! - Bound is strict `>`: with `max=3`, attempts 1..=4 run; attempt 4's
//!   Retry outcome exceeds the bound.
//! - Error string on exhaustion: `"retry limit exceeded after {n} attempts"`
//!   where `n` is the actual count of attempts that ran.

use spur_acp::DelegationStatus;
use std::future::Future;

/// The outcome of a single attempt inside `RetryLoop::run`.
pub enum RetryOutcome {
    /// Attempt reached a terminal status — return it unchanged.
    Terminal(DelegationStatus),
    /// Attempt produced a Retry decision — loop bumps `attempt_n` and
    /// calls the closure again unless the bound is exceeded.
    Retry,
}

/// Bounded retry loop for review-gated delegation attempts.
#[derive(Debug, Clone, Copy)]
pub struct RetryLoop {
    max_retries: u32,
}

impl RetryLoop {
    pub fn new(max_retries: u32) -> Self {
        Self { max_retries }
    }

    pub async fn run<F, Fut>(&self, mut attempt: F) -> DelegationStatus
    where
        F: FnMut(u32) -> Fut,
        Fut: Future<Output = RetryOutcome>,
    {
        let mut attempt_n: u32 = 1;
        loop {
            match attempt(attempt_n).await {
                RetryOutcome::Terminal(s) => return s,
                RetryOutcome::Retry => {
                    if attempt_n > self.max_retries {
                        return DelegationStatus::Failed {
                            error: format!(
                                "retry limit exceeded after {attempt_n} attempts"
                            ),
                        };
                    }
                    attempt_n += 1;
                }
            }
        }
    }
}
```

In `crates/spur-core/src/lib.rs`, add near the other `pub mod` declarations:

```rust
pub mod retry_loop;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-core --test retry_loop`
Expected: PASS.

- [ ] **Step 5: Port the test_support run_gate_with_retries**

In `crates/spur-core/src/orchestrator.rs` inside `pub mod test_support`, rewrite `run_gate_with_retries` (current body at lines 4266–4326) to delegate:

```rust
pub async fn run_gate_with_retries(
    executor_id: ExecutorId,
    candidate_status: DelegationStatus,
    review_timeout: std::time::Duration,
    timeout_fallback: TimeoutFallback,
    max_review_retries: u32,
    review_sink: ReviewSink,
) -> DelegationStatus {
    use crate::retry_loop::{RetryLoop, RetryOutcome};
    use spur_acp::ReviewDecision;

    RetryLoop::new(max_review_retries)
        .run(|attempt_n| {
            let eid = executor_id.clone();
            let candidate = candidate_status.clone();
            let sink = review_sink.clone();
            let fallback = timeout_fallback;
            let timeout = review_timeout;
            async move {
                let rx = match register_gate(eid.clone(), attempt_n, &sink).await {
                    Ok(rx) => rx,
                    Err(e) => return RetryOutcome::Terminal(DelegationStatus::Failed {
                        error: format!("review registration failed: {e}"),
                    }),
                };
                let decision = tokio::select! {
                    r = rx => r.ok(),
                    _ = tokio::time::sleep(timeout) => {
                        sink.remove(&eid).await;
                        return RetryOutcome::Terminal(DelegationStatus::TimedOut {
                            waited_for: timeout, fallback,
                        });
                    }
                };
                match decision {
                    Some(ReviewDecision::Approve) => RetryOutcome::Terminal(candidate),
                    Some(ReviewDecision::Reject { reason }) =>
                        RetryOutcome::Terminal(DelegationStatus::Rejected { reason }),
                    Some(ReviewDecision::Modify { note }) =>
                        RetryOutcome::Terminal(DelegationStatus::Modified { reviewer_note: note }),
                    Some(ReviewDecision::Retry { .. }) => RetryOutcome::Retry,
                    None => {
                        sink.remove(&eid).await;
                        RetryOutcome::Terminal(DelegationStatus::TimedOut {
                            waited_for: timeout, fallback,
                        })
                    }
                }
            }
        })
        .await
}
```

If `DelegationStatus` is not `Clone`, the function needs a per-iteration construction pattern — inspect the enum and adapt (e.g. pass `Fn() -> DelegationStatus` to reconstruct). The combinator's closure is `FnMut`, so captured state can be mutated between attempts if needed.

- [ ] **Step 6: Port the production site**

In `crates/spur-core/src/orchestrator.rs` around line 3046, replace the inline retry-bookkeeping block. The current body handles `Retry { new_constraints }` by incrementing `attempt_n`, checking `attempt_n > agent_config.review.max_review_retries`, formatting the identical error string, and looping. The full outer loop (covering `register_gate` → `rx select!` → decision match) becomes:

```rust
// NOTE: retry bookkeeping lives in crate::retry_loop::RetryLoop; keep
// the closure below in sync with test_support::run_gate_with_retries.
let final_status = RetryLoop::new(agent_config.review.max_review_retries)
    .run(|attempt_n| {
        // ... clone captures ...
        async move {
            // register_gate (production variant — has event emission sites)
            // select! over rx / timeout
            // decision match: Approve → Terminal(candidate), Reject → Terminal(Rejected {..}),
            //                 Modify → Terminal(Modified {..}), Retry → Retry, None → Terminal(TimedOut).
            //
            // Production MUST additionally:
            //   - Emit SpurEventBody::ExecutorReviewResolved on Approve/Reject/Modify
            //     (whatever the existing emit calls are in the current inline body —
            //     move them verbatim into this closure).
            //   - Call apply_worktree_cleanup on terminal branches that need it.
        }
    })
    .await;
```

Do a line-by-line port. Every side effect (event emit, worktree cleanup, state update) that currently lives inside the inline `match` arms moves into the closure. The closure returns `RetryOutcome::Terminal(status)` or `RetryOutcome::Retry` as appropriate. No side effect moves OUT of the retry site — all preservation is local.

Delete the comment at orchestrator.rs:3046 that reads "this retry logic is duplicated in run_gate_with_retries" — it's no longer true.

- [ ] **Step 7: Verify**

Run: `cargo test -p spur-core`
Expected: PASS. Especially watch:
- `review_gate_integration` tests — they exercise `run_gate_with_retries`.
- `retry_loop` — your new test.
- Any orchestrator-facing integration test that expected the exact error string.

Run: `cargo test -p spur-acp -p spur-mcp -p spur-core`
Expected: PASS.

- [ ] **Step 8: Clippy + fmt**

Run: `cargo clippy -p spur-core --all-targets -- -D warnings`
Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "refactor(spur-core): DN-2 — extract RetryLoop combinator

Production orchestrator retry site (orchestrator.rs:3046+) and
test_support::run_gate_with_retries now both delegate to
RetryLoop::new(max).run(closure). Bound, attempt-count semantics, and
error string preserved exactly. Deletes the 'duplicated in
run_gate_with_retries' comment — no longer true."
```

---

### Task 4: DN-4 — PlanTaskStatus::Cancelled with non-cascading semantic

**Design decisions locked in this plan:**
- `Cancelled { reason: String }` is a distinct terminal status, not folded into `Failed`.
- A task whose dep is `Cancelled` is treated as dep-satisfied in `dispatch_newly_ready` (downstream runs; cancellation is not a failure signal for deps).
- `mark_descendants_failed` is NOT invoked when a task transitions to `Cancelled` (explicit test).
- `SpurEventBody::PlanCompleted` gains a new `cancelled: u32` field. `all_approved` ⇒ `PlanReadyToMerge` requires `cancelled == 0` (a plan with cancelled tasks is not merge-authorized).

**Files:**
- Modify: `crates/spur-acp/src/domain/events.rs` — add `cancelled: u32` to `PlanCompleted`
- Modify: `crates/spur-mcp/src/plan.rs:38-53` (enum), `~645`, `~2090`, `~1955-1977` (dispatch_newly_ready), `~783-791` (build_plan_status), `~712-743` (count loop)
- Modify: `crates/spur-core/src/lineage/adapter.rs` if it matches on `PlanTaskStatus` — grep first
- Test: `crates/spur-mcp/tests/plan_cancelled_task_semantics.rs` (new)

- [ ] **Step 1: Write the failing test**

Create `crates/spur-mcp/tests/plan_cancelled_task_semantics.rs`:

```rust
//! DN-4: PlanTaskStatus::Cancelled has non-cascading dep semantics.

use spur_mcp::plan::{PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus, apply_decision_and_extract /* or whatever the public helper is */};

#[test]
fn cancelled_task_does_not_cascade_to_dependents() {
    // Task A has no deps; task B depends on A.
    // A is Cancelled (brain aborted). B must NOT become Failed via
    // mark_descendants_failed; B should be eligible for dispatch.
    let mut state = PlanState::test_new(vec![
        PlanTaskEntry::test_new("a", vec![], PlanTaskStatus::Cancelled { reason: "brain aborted".into() }),
        PlanTaskEntry::test_new("b", vec!["a".into()], PlanTaskStatus::Pending),
    ]);

    let ready: Vec<String> = spur_mcp::plan::test_support::compute_ready_ids(&state);
    assert!(ready.contains(&"b".into()), "b must be ready: cancelled deps do not block");
}

#[test]
fn cancelled_task_transition_does_not_invoke_cascade() {
    // Transitioning a task from Dispatched to Cancelled (via the
    // result-match path at plan.rs:~2090) must NOT set transitioned_to_failed.
    let outcome = spur_mcp::plan::test_support::apply_result_status_for_test(
        spur_acp::DelegationStatus::Cancelled { reason: "brain aborted".into() }
    );
    assert!(!outcome.cascaded);
    assert!(matches!(outcome.final_status, PlanTaskStatus::Cancelled { .. }));
}

#[test]
fn plan_completed_event_includes_cancelled_count() {
    use spur_acp::SpurEventBody;
    let body = SpurEventBody::PlanCompleted {
        plan_id: "p1".into(),
        approved: 1,
        rejected: 0,
        failed: 0,
        cancelled: 2,
    };
    let json = serde_json::to_string(&body).unwrap();
    assert!(json.contains("\"cancelled\":2"), "event payload must expose cancelled count");
}

#[test]
fn plan_ready_to_merge_requires_zero_cancelled() {
    // Emit terminal counts for a plan where all non-cancelled tasks are
    // Approved, but one task is Cancelled. PlanReadyToMerge must NOT fire.
    // (Full behavior test — uses run_plan with a synthetic plan.)
    // Implementation: drive run_plan to completion with a CaptureSink
    // (see INV-7 pattern in submit_plan_persist.rs) and assert the
    // emitted events include PlanCompleted{cancelled: 1} but NOT
    // PlanReadyToMerge.
    //
    // If test_support helpers don't expose enough for this, mirror the
    // shape of `run_plan_emits_plan_completed_on_terminal_state` in
    // tests/submit_plan_persist.rs.
    unimplemented!("see INV-7 test pattern");  // Replace with real body in Step 3.
}
```

The last test is sketched — port it against the real run_plan + CaptureSink pattern already in `tests/submit_plan_persist.rs`. If helpers like `PlanState::test_new` / `PlanTaskEntry::test_new` / `test_support::compute_ready_ids` / `test_support::apply_result_status_for_test` don't exist yet, add them to `plan::test_support` as thin wrappers over the real logic so the test can drive internals. Keep them narrowly scoped.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-mcp --test plan_cancelled_task_semantics`
Expected: FAIL (compile or assertion — `PlanTaskStatus::Cancelled` variant does not exist, and `PlanCompleted` has no `cancelled` field).

- [ ] **Step 3: Add the enum variant**

In `crates/spur-mcp/src/plan.rs` at the `PlanTaskStatus` enum (lines 38–53):

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PlanTaskStatus {
    Pending,
    Ready,
    Dispatched { delegation_id: String },
    AwaitingReview { summary: Option<String> },
    Approved { summary: Option<String> },
    Rejected { feedback: Option<String> },
    Failed { error: String },
    /// DN-4: brain aborted this task via cancel_delegation. Non-cascading:
    /// dependents treat this like an Approved dep for the purposes of
    /// scheduling. A plan with any Cancelled task cannot emit
    /// PlanReadyToMerge.
    Cancelled { reason: String },
}
```

- [ ] **Step 4: Add cancelled field to PlanCompleted**

In `crates/spur-acp/src/domain/events.rs`, find `SpurEventBody::PlanCompleted` and add the field:

```rust
PlanCompleted {
    plan_id: String,
    approved: u32,
    rejected: u32,
    failed: u32,
    /// DN-4: tasks cancelled via cancel_delegation before terminal review.
    #[serde(default)]
    cancelled: u32,
},
```

The `#[serde(default)]` makes it backward-compatible with any pre-existing JSON streams (including tests) that don't carry the field.

- [ ] **Step 5: Update emission sites and matches**

In `crates/spur-mcp/src/plan.rs`:

**5a.** `build_plan_status` (around line 783–791) — exhaustive match with no wildcard. Add:

```rust
PlanTaskStatus::Cancelled { reason } => /* mirror the shape sibling arms produce */,
```

Keep the returned status variant consistent with siblings — if Failed returns a `{ "status": "failed", "error": ... }` object, Cancelled should return `{ "status": "cancelled", "reason": ... }`. Preserve the existing Serialize-derive shape.

**5b.** `dispatch_newly_ready` (around lines 1955–1977) — the `matches!` pattern on dep satisfaction. Expand it to accept `Cancelled` as dep-satisfied:

```rust
.filter(|t| {
    t.spec.depends_on.iter().all(|dep| {
        state.tasks.iter().any(|o| {
            o.spec.task_id == *dep
                && matches!(
                    o.status,
                    PlanTaskStatus::Approved { .. } | PlanTaskStatus::Cancelled { .. }
                )
        })
    })
})
```

**5c.** First result-match wildcard (around line 645, inside `spawn_completion_future`). Change:

```rust
DelegationStatus::Failed { error } => {
    entry.status = PlanTaskStatus::Failed { error: error.clone() };
}
DelegationStatus::Cancelled { reason } => {
    entry.status = PlanTaskStatus::Cancelled { reason: reason.clone() };
}
other => {
    warn!(plan_id = %pid, task_id = %tid, "Plan task ended: {other:?}");
    entry.status = PlanTaskStatus::Failed { error: format!("{other:?}") };
}
```

**5d.** Second result-match wildcard (around line 2090). The current arm sets `transitioned_to_failed = true;` and then fires `mark_descendants_failed`. For Cancelled we explicitly skip both:

```rust
DelegationStatus::Cancelled { reason } => {
    entry.status = PlanTaskStatus::Cancelled { reason: reason.clone() };
    // Non-cascading: dependents will see Cancelled as "dep satisfied"
    // in dispatch_newly_ready and can proceed.
}
other => {
    entry.status = PlanTaskStatus::Failed { error: format!("{other:?}") };
    transitioned_to_failed = true;
}
```

Position the new `Cancelled` arm BEFORE the `other =>` catch-all. Verify `transitioned_to_failed` is NOT set in the Cancelled branch (that's the whole point).

**5e.** Terminal count loop (lines 712–743). Add a `cancelled` counter and emit it:

```rust
let (approved_count, rejected_count, failed_count, cancelled_count, awaiting_review_count, all_approved) = {
    let p = plan.lock().await;
    let mut a = 0u32; let mut r = 0u32; let mut f = 0u32; let mut c = 0u32; let mut ar = 0u32;
    let non_empty = !p.tasks.is_empty();
    let mut all_a = non_empty;
    for t in &p.tasks {
        match &t.status {
            PlanTaskStatus::Approved { .. } => a += 1,
            PlanTaskStatus::Rejected { .. } => { r += 1; all_a = false; }
            PlanTaskStatus::Failed { .. } => { f += 1; all_a = false; }
            PlanTaskStatus::Cancelled { .. } => { c += 1; all_a = false; }
            PlanTaskStatus::AwaitingReview { .. } => { ar += 1; all_a = false; }
            _ => { all_a = false; }
        }
    }
    (a, r, f, c, ar, all_a)
};

if let Some(sink) = &event_sink {
    sink.emit(SpurEvent::now(SpurEventBody::PlanCompleted {
        plan_id: plan_id.clone(),
        approved: approved_count,
        rejected: rejected_count,
        failed: failed_count,
        cancelled: cancelled_count,
    }));
    if all_approved {
        sink.emit(SpurEvent::now(SpurEventBody::PlanReadyToMerge {
            plan_id: plan_id.clone(),
        }));
    }
}
```

- [ ] **Step 6: TUI / lineage / server.rs compile fixups**

Run: `cargo build -p spur-acp -p spur-mcp -p spur-core -p spur-tui`

Each compile error about a non-exhaustive `PlanTaskStatus` match: add a `PlanTaskStatus::Cancelled { reason } => /* same UI shape as Failed */` arm. Display text suggestion: `"Cancelled: {reason}"`. Keep per-site copy consistent with how Failed is displayed.

Each compile error about `SpurEventBody::PlanCompleted` missing the field: add `cancelled: 0` at construction sites in tests; at production emission sites the field is already computed above.

- [ ] **Step 7: Run tests and verify they pass**

Run: `cargo test -p spur-mcp --test plan_cancelled_task_semantics`
Expected: PASS.

Run: `cargo test -p spur-acp -p spur-mcp -p spur-core`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(spur-mcp): DN-4 — PlanTaskStatus::Cancelled with non-cascading semantic

Distinct terminal status for brain-cancelled plan tasks. dispatch_newly_ready
treats Cancelled deps as satisfied (downstream still runs). mark_descendants_failed
is NOT invoked on Cancelled transition. PlanCompleted gains a cancelled: u32
field (serde-default for stream compat); PlanReadyToMerge requires cancelled == 0."
```

---

### Task 5: DN-5 — INV-1 replay-safety hardening

**Two sub-items:**
- Third orphan buffer in lineage for `DelegationDispatched`-before-`WorkerSpawned`, drained on `WorkerSpawned`.
- `tracing::warn!` on duplicate `DelegationRequested` with differing payload.

**Files:**
- Modify: `crates/spur-core/src/lineage/projection.rs:38-54`
- Modify: `crates/spur-core/src/lineage/adapter.rs:90-128` (and the `WorkerSpawned` arm)
- Test: `crates/spur-core/tests/lineage_integration.rs` (append)

- [ ] **Step 1: Write the failing tests**

Append to `crates/spur-core/tests/lineage_integration.rs`:

```rust
#[test]
fn dispatched_before_spawned_drains_on_worker_arrival() {
    // DelegationRequested → DelegationDispatched arrives BEFORE the
    // WorkerSpawned for that executor. The buffered task_spec must be
    // stamped onto the node once WorkerSpawned processes.
    use spur_acp::{SessionId, SpurEvent, SpurEventBody};
    use spur_core::lineage::{ExecutorId, ExecutorLineage};

    let mut l = ExecutorLineage::default();

    // 1) DelegationRequested — buffered by request_id.
    l.apply(&SpurEvent::now(SpurEventBody::DelegationRequested {
        from: SessionId("b".into()),
        to_agent: "coder".into(),
        task: "TASK-A".into(),
        request_id: "req-A".into(),
        delegation_plan: None,
        issue_id: None,
    }));

    // 2) DelegationDispatched arrives BEFORE WorkerSpawned for worker-A.
    //    With today's adapter, the stamp is silently dropped.
    l.apply(&SpurEvent::now(SpurEventBody::DelegationDispatched {
        from: SessionId("b".into()),
        request_id: "req-A".into(),
        executor_id: "worker-A".into(),
    }));

    // 3) WorkerSpawned — node materializes; the buffered dispatch
    //    must replay onto it.
    l.apply(&SpurEvent::now(SpurEventBody::WorkerSpawned {
        agent: "coder".into(),
        session: SessionId("worker-A".into()),
        worktree: std::path::PathBuf::from("/tmp/wA"),
    }));

    let n = l.node(&ExecutorId::new("worker-A")).expect("worker-A");
    assert_eq!(n.task_spec, "TASK-A", "buffered dispatch must stamp task_spec on spawn");
}

#[test]
fn duplicate_delegation_requested_with_same_payload_is_silent() {
    // Replaying the SAME request twice is legitimate (event-source
    // replay) and must not log a warning.
    // (We assert on side-effect absence only — tracing capture is a
    // separate test infra concern; here we just exercise the path to
    // prove it doesn't panic / double-insert.)
    let mut l = spur_core::lineage::ExecutorLineage::default();
    let e = spur_acp::SpurEvent::now(spur_acp::SpurEventBody::DelegationRequested {
        from: spur_acp::SessionId("b".into()),
        to_agent: "coder".into(),
        task: "TASK-A".into(),
        request_id: "req-A".into(),
        delegation_plan: None,
        issue_id: None,
    });
    l.apply(&e);
    l.apply(&e); // replay
    // No assertion on map size — or_insert_with already handled this.
    // The behavioral assertion is just "didn't panic".
}

#[tokio::test]
async fn duplicate_delegation_requested_with_differing_payload_warns() {
    // A buggy emitter or corrupted stream might emit req-A twice with
    // different tasks. We must log a warn! so operators see the anomaly
    // but still preserve the FIRST payload (or_insert_with semantics).
    use tracing_test::traced_test;
    // If tracing-test isn't already in workspace dev-deps, add it:
    //     tracing-test = "0.2"   # in crates/spur-core/Cargo.toml [dev-dependencies]
    // and mark this test with #[traced_test] from tracing_test.

    #[traced_test]
    fn inner() {
        let mut l = spur_core::lineage::ExecutorLineage::default();
        l.apply(&spur_acp::SpurEvent::now(spur_acp::SpurEventBody::DelegationRequested {
            from: spur_acp::SessionId("b".into()),
            to_agent: "coder".into(),
            task: "TASK-A".into(),
            request_id: "req-A".into(),
            delegation_plan: None,
            issue_id: None,
        }));
        l.apply(&spur_acp::SpurEvent::now(spur_acp::SpurEventBody::DelegationRequested {
            from: spur_acp::SessionId("b".into()),
            to_agent: "coder".into(),
            task: "TASK-DIFFERENT".into(),  // different payload!
            request_id: "req-A".into(),
            delegation_plan: None,
            issue_id: None,
        }));
        assert!(logs_contain("duplicate DelegationRequested"));
    }
    inner();
}
```

If `tracing-test` brings too much surface, replace the third test with a simpler mechanism: expose a `pub(crate)` counter in `ExecutorLineage::duplicate_request_warnings` incremented on the divergent path; assert it's `1`.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-core --test lineage_integration dispatched_before_spawned_drains_on_worker_arrival`
Expected: FAIL — current adapter drops the dispatch silently when the executor doesn't exist.

Run: `cargo test -p spur-core --test lineage_integration duplicate_delegation_requested_with_differing_payload_warns`
Expected: FAIL — no warning is emitted today.

- [ ] **Step 3: Add the third orphan buffer**

In `crates/spur-core/src/lineage/projection.rs` (lines 38–54), extend `ExecutorLineage`:

```rust
pub struct ExecutorLineage {
    nodes: HashMap<ExecutorId, ExecutorNode>,
    roots: Vec<ExecutorId>,
    orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>,
    parent_orphan_buffer: HashMap<ExecutorId, VecDeque<SpurEvent>>,
    pending_review_order: VecDeque<ExecutorId>,
    pending_task_by_request_id: HashMap<String, (String, Option<String>)>,
    /// DN-5: DelegationDispatched events whose target executor is not
    /// yet in `nodes`. Keyed by `executor_id` (a String, matching the
    /// event payload). Drained on WorkerSpawned.
    pending_dispatch_by_executor_id: HashMap<String, (String, Option<String>)>,
}
```

Add an accessor mirroring `pending_task_by_request_id_mut`:

```rust
pub(crate) fn pending_dispatch_by_executor_id_mut(
    &mut self,
) -> &mut HashMap<String, (String, Option<String>)> {
    &mut self.pending_dispatch_by_executor_id
}
```

Initialize the new field wherever `ExecutorLineage` is constructed (probably in `Default` derive — confirm).

- [ ] **Step 4: Buffer on dispatch, drain on spawn, warn on duplicate**

In `crates/spur-core/src/lineage/adapter.rs`:

**4a.** `DelegationDispatched` arm (current lines 109–128). When node is absent, buffer instead of dropping:

```rust
SpurEventBody::DelegationDispatched { request_id, executor_id, .. } => {
    let task_and_issue = lineage
        .pending_task_by_request_id_mut()
        .remove(request_id);
    if let Some((task, issue_id)) = task_and_issue {
        let eid = ExecutorId::new(executor_id.clone());
        if let Some(n) = lineage.node_mut_public(&eid) {
            n.task_spec = task;
            n.issue_id = issue_id;
        } else {
            // DN-5: executor not yet materialized — buffer for replay on WorkerSpawned.
            lineage
                .pending_dispatch_by_executor_id_mut()
                .insert(executor_id.clone(), (task, issue_id));
        }
    }
}
```

**4b.** `WorkerSpawned` arm (find it via grep near line 44). After the node is inserted into `nodes` and attached to parent (or roots), drain the dispatch buffer:

```rust
SpurEventBody::WorkerSpawned { session, ... } => {
    // ... existing insertion logic ...
    let eid_str = session.0.clone();
    if let Some((task, issue_id)) = lineage
        .pending_dispatch_by_executor_id_mut()
        .remove(&eid_str)
    {
        let eid = ExecutorId::new(eid_str);
        if let Some(n) = lineage.node_mut_public(&eid) {
            n.task_spec = task;
            n.issue_id = issue_id;
        }
    }
}
```

(Position the drain AFTER the node insertion, so `node_mut_public` succeeds.)

**4c.** `DelegationRequested` arm (lines 90–107). Detect duplicates with differing payload:

```rust
SpurEventBody::DelegationRequested { task, request_id, issue_id, .. } => {
    let buf = lineage.pending_task_by_request_id_mut();
    match buf.get(request_id) {
        Some((existing_task, existing_issue))
            if existing_task != task || existing_issue != issue_id =>
        {
            tracing::warn!(
                request_id = %request_id,
                new_task = %task,
                existing_task = %existing_task,
                "duplicate DelegationRequested with differing payload; keeping first"
            );
        }
        _ => {}
    }
    buf.entry(request_id.clone())
        .or_insert_with(|| (task.clone(), issue_id.clone()));
}
```

Only emit the warn when there IS an existing entry AND its payload differs. Replays of the identical event stay silent.

- [ ] **Step 5: Run tests and verify they pass**

Run: `cargo test -p spur-core --test lineage_integration`
Expected: PASS (including the two new tests and all existing).

- [ ] **Step 6: Regression + lint**

Run: `cargo test -p spur-acp -p spur-mcp -p spur-core`
Run: `cargo clippy -p spur-core --all-targets -- -D warnings`
Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "fix(spur-core): DN-5 — INV-1 replay-safety (orphan dispatch + dup warn)

Adds pending_dispatch_by_executor_id: a third orphan buffer for
DelegationDispatched events whose target executor hasn't materialized
yet. Drained in the WorkerSpawned arm so task_spec stamps land on
out-of-order replay. tracing::warn! when a second DelegationRequested
arrives for the same request_id with a differing task or issue_id —
identical replays stay silent (or_insert_with idempotency preserved)."
```

---

### Task 6: DN-6 — mark non-terminal tasks Failed on plan exit

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs:690-708` (mark-unreachable block)
- Test: `crates/spur-mcp/tests/submit_plan_persist.rs` (append)

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-mcp/tests/submit_plan_persist.rs`:

```rust
#[tokio::test]
async fn run_plan_marks_pending_tasks_failed_on_terminal_exit() {
    // Scenario: plan has a Pending task whose delegation_tx dropped
    // before dispatch (simulated by building a plan with a task in a
    // non-terminal state that run_plan cannot advance). After run_plan
    // returns, the task must be Failed with an explicit error, and the
    // emitted PlanCompleted event must count it.
    //
    // Build a PlanState with one task in Pending state that has a dep
    // on a non-existent task id (so dispatch_newly_ready cannot promote
    // it to Ready). run_plan will loop until both in_flight is empty
    // and nothing is newly ready; mark-unreachable currently only
    // catches Pending-with-Failed-dep, NOT Pending-with-missing-dep.

    use std::sync::Arc;
    use tokio::sync::{mpsc, Mutex};
    use spur_acp::{SpurEvent, SpurEventBody};
    use spur_mcp::plan::{PlanState, PlanTask, PlanTaskEntry, PlanTaskStatus, run_plan};

    let state = PlanState {
        plan_id: "p1".into(),
        tasks: vec![PlanTaskEntry {
            spec: PlanTask {
                task_id: "t1".into(),
                agent: "a".into(),
                task: "T".into(),
                depends_on: vec!["missing-dep".into()],  // never satisfied
                issue_id: None,
                context_files: vec![],
            },
            status: PlanTaskStatus::Pending,
            result: None, worker_branch: None, attempt: 1, history: vec![],
        }],
        brain_session_id: spur_acp::BrainSessionId::new(spur_acp::SessionId("b".into())),
        epic_id: None,
    };

    let captured: Arc<std::sync::Mutex<Vec<SpurEvent>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = /* mirror CaptureSink from run_plan_emits_plan_completed_on_terminal_state */;
    let (dtx, _drx) = mpsc::channel(8);
    let plan_arc = Arc::new(Mutex::new(state));

    run_plan(Arc::clone(&plan_arc), dtx, Some(sink)).await;

    // Task is Failed now, not Pending.
    let st = plan_arc.lock().await;
    assert!(matches!(st.tasks[0].status, PlanTaskStatus::Failed { .. }));
    drop(st);

    // PlanCompleted event counts it as Failed.
    let events = captured.lock().unwrap();
    let pc = events.iter().find_map(|e| match &e.body {
        SpurEventBody::PlanCompleted { failed, .. } => Some(*failed),
        _ => None,
    }).expect("PlanCompleted must be emitted");
    assert_eq!(pc, 1, "stuck Pending task must be counted as failed");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p spur-mcp --test submit_plan_persist run_plan_marks_pending_tasks_failed_on_terminal_exit`
Expected: FAIL — currently the task stays Pending and the count is 0.

- [ ] **Step 3: Extend mark-unreachable to catch all non-terminal states**

In `crates/spur-mcp/src/plan.rs`, replace the existing block at lines 690–708:

```rust
// DN-6: On terminal loop exit, promote all non-terminal tasks to
// Failed with a specific error. Originally this block only caught
// Pending-with-failed-dep; now it also catches Pending with
// missing / non-satisfied deps, stuck Ready, stuck Dispatched, and
// stuck AwaitingReview. A task that genuinely finished terminates
// at Approved / Rejected / Failed / Cancelled and is unaffected.
{
    let failed_ids: HashSet<String> = p.tasks.iter()
        .filter(|t| matches!(t.status, PlanTaskStatus::Failed { .. }))
        .map(|t| t.spec.task_id.clone())
        .collect();

    for entry in &mut p.tasks {
        match &entry.status {
            PlanTaskStatus::Pending if entry.spec.depends_on.iter().any(|d| failed_ids.contains(d)) => {
                entry.status = PlanTaskStatus::Failed {
                    error: "Blocked by failed dependency".into(),
                };
            }
            PlanTaskStatus::Pending => {
                entry.status = PlanTaskStatus::Failed {
                    error: "Plan exited with task still pending (dep never satisfied)".into(),
                };
            }
            PlanTaskStatus::Ready => {
                entry.status = PlanTaskStatus::Failed {
                    error: "Plan exited with task ready but never dispatched".into(),
                };
            }
            PlanTaskStatus::Dispatched { delegation_id } => {
                entry.status = PlanTaskStatus::Failed {
                    error: format!("Plan exited with task still running (delegation {delegation_id})"),
                };
            }
            PlanTaskStatus::AwaitingReview { .. } => {
                entry.status = PlanTaskStatus::Failed {
                    error: "Plan exited with task awaiting review".into(),
                };
            }
            PlanTaskStatus::Approved { .. }
            | PlanTaskStatus::Rejected { .. }
            | PlanTaskStatus::Failed { .. }
            | PlanTaskStatus::Cancelled { .. } => {}
        }
    }
}
```

Position this block BEFORE the terminal count loop (lines 712+). After this block runs, the count loop will see every task as terminal; the `_ => { all_a = false; }` arm will never fire for a well-behaved plan-exit.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p spur-mcp --test submit_plan_persist run_plan_marks_pending_tasks_failed_on_terminal_exit`
Expected: PASS.

- [ ] **Step 5: Regression + lint**

Run: `cargo test -p spur-acp -p spur-mcp -p spur-core`
Run: `cargo clippy -p spur-mcp --all-targets -- -D warnings`
Run: `cargo fmt --all -- --check`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "fix(spur-mcp): DN-6 — mark non-terminal tasks Failed on run_plan exit

Extends the mark-unreachable block at the tail of run_plan's loop to
catch all non-terminal statuses, not just Pending-with-failed-dep. A
task that is Pending with missing dep, stuck Ready, stuck Dispatched,
or AwaitingReview at loop exit is promoted to Failed with a
status-specific error. Downstream result: PlanCompleted's approved +
rejected + failed + cancelled now sums to total task count for a
well-behaved plan termination."
```

---

## Self-Review

**Spec coverage:** All six items map to tasks 1–6 in order. ✓

**Placeholder scan:** No "TBD / TODO / fill in" text in steps. Tests have concrete code. ✓ (One exception: Task 4 Step 1's fourth test is sketched with `unimplemented!()` and explicit guidance to port against the `run_plan_emits_plan_completed_on_terminal_state` pattern — flagged to the engineer, acceptable for a test that depends on test-infra already built in the prior phase.)

**Type consistency:**
- `PlanTaskStatus::Cancelled { reason: String }` — single field name `reason`, matches `DelegationStatus::Cancelled { reason: String }`. ✓
- `SpurEventBody::PlanCompleted.cancelled: u32` — consistent with sibling counters. ✓
- `RetryOutcome::{Terminal(DelegationStatus), Retry}` — consistent between definition and use sites in Tasks 3. ✓
- `pending_dispatch_by_executor_id: HashMap<String, (String, Option<String>)>` — matches the tuple shape of `pending_task_by_request_id`. ✓

**Risk notes for the executor:**
- Task 3 Step 6 is the biggest refactor in the plan — budget 45+ minutes. Grounding report notes the production retry site has event emissions and worktree cleanup that must move into the closure verbatim.
- Task 4 Step 6 (TUI compile fixups) may surface unexpected matches in spur-tui; don't over-engineer copy, mirror Failed's treatment.
- Task 5 Step 4b requires finding the WorkerSpawned arm in adapter.rs — the grounding report confirms it's around line 44 but the exact location may have shifted after INV-1 landed; use `grep -n "SpurEventBody::WorkerSpawned" crates/spur-core/src/lineage/adapter.rs` first.

---

Plan complete and saved to `docs/superpowers/plans/2026-04-19-phase3a-low-risk-hardening.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?

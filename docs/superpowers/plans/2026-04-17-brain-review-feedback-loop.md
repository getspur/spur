# Brain-Driven Review Feedback Loop — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the brain agent MCP tools to review worker output (diffs), approve/reject tasks, and sync beads status — closing the feedback loop in plan execution.

**Architecture:** Two new MCP tools (`get_task_diff`, `review_task`) backed by three new `PlanTaskStatus` states (`AwaitingReview`, `Approved`, `Rejected`). A new `detach_worktree()` method preserves approved branches (removes worktree dir, keeps git branch). The orchestrator's `apply_worktree_cleanup` becomes a three-way dispatch. `DelegationResult` gains a `worker_branch` field.

**Tech Stack:** Rust, tokio, serde_json, spur MCP server (JSON-RPC)

**Spec:** `docs/superpowers/specs/2026-04-17-brain-review-feedback-loop-design.md`

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/spur-worktree/src/manager.rs` | Modify | Add `detach_worktree()` method |
| `crates/spur-acp/src/domain/delegation.rs` | Modify | Add `worker_branch` field to `DelegationResult` |
| `crates/spur-core/src/orchestrator.rs` | Modify | Three-way cleanup, thread branch name to result |
| `crates/spur-mcp/src/plan.rs` | Modify | New states, AwaitingReview transition, `review_task()`, enriched status |
| `crates/spur-mcp/src/tools.rs` | Modify | `get_task_diff_def()` + `review_task_def()` tool definitions |
| `crates/spur-mcp/src/server.rs` | Modify | `handle_get_task_diff()` + `handle_review_task()` handlers |

---

### Task 1: Add `detach_worktree()` to WorktreeManager

**Files:**
- Modify: `crates/spur-worktree/src/manager.rs:287` (after `remove_worktree`)

- [ ] **Step 1: Add `detach_worktree` method**

Insert after `remove_worktree` (after line 309 in `manager.rs`):

```rust
    /// Remove the worktree directory but keep the branch alive for future merge.
    /// Returns the preserved branch name.
    pub async fn detach_worktree(&mut self, session_id: &SessionId) -> Result<String> {
        let session_str = session_id.to_string();
        let info = self
            .active
            .remove(&session_str)
            .ok_or_else(|| anyhow!("no active worktree for session {session_str}"))?;

        let path_str = info
            .path
            .to_str()
            .ok_or_else(|| anyhow!("worktree path is not valid UTF-8"))?
            .to_string();

        self.run_git(&["worktree", "remove", &path_str, "--force"], None)
            .await
            .with_context(|| format!("failed to detach worktree at {path_str}"))?;

        // Branch intentionally NOT deleted — preserved for brain review + merge.
        debug!(branch = %info.branch, "detached worktree, branch preserved");
        Ok(info.branch)
    }
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p spur-worktree 2>&1 | tail -3`
Expected: `Finished` with 0 errors

- [ ] **Step 3: Commit**

```bash
git add crates/spur-worktree/src/manager.rs
git commit -m "feat(worktree): add detach_worktree() — remove dir, keep branch"
```

---

### Task 2: Add `worker_branch` to `DelegationResult`

**Files:**
- Modify: `crates/spur-acp/src/domain/delegation.rs:62-74`

- [ ] **Step 1: Add the field**

In the `DelegationResult` struct, add after `estimated_cost_usd`:

```rust
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_branch: Option<String>,
```

- [ ] **Step 2: Fix all construction sites**

Search for `DelegationResult {` across the codebase. Every construction site needs `worker_branch: None` added. Key locations:

In `crates/spur-core/src/orchestrator.rs` — the `finalize` function (line ~3224):
```rust
    DelegationResult {
        status: final_status,
        diff,
        diff_summary,
        summary,
        estimated_cost_usd: total_cost,
        worker_branch: None, // NEW
    }
```

In the early-return `DelegationResult` stubs (lines ~2553, ~2572) and `DelegationGuard::drop` (line ~3191), add `worker_branch: None`.

- [ ] **Step 3: Verify it compiles**

Run: `cargo build 2>&1 | tail -3`
Expected: `Finished` with 0 errors, 0 warnings

- [ ] **Step 4: Commit**

```bash
git add crates/spur-acp/src/domain/delegation.rs crates/spur-core/src/orchestrator.rs
git commit -m "feat(acp): add worker_branch field to DelegationResult"
```

---

### Task 3: Three-way cleanup in orchestrator

**Files:**
- Modify: `crates/spur-core/src/orchestrator.rs:3137-3163` (`apply_worktree_cleanup`)
- Modify: `crates/spur-core/src/orchestrator.rs:3208-3228` (`finalize`)

- [ ] **Step 1: Change `apply_worktree_cleanup` to return `Option<String>`**

Replace the function (lines 3137–3163) with:

```rust
async fn apply_worktree_cleanup(
    worktrees: &mut WorktreeManager,
    worker_session: &SessionId,
    final_status: &DelegationStatus,
    diff: &Option<String>,
    agent: &str,
    worktree_path: &std::path::Path,
) -> Option<String> {
    if should_commit_worker_diff(final_status) && diff.is_some() {
        if let Err(e) = worktrees
            .commit_worker_changes(worker_session, &format!("spur: worker {} output", agent))
            .await
        {
            tracing::warn!(error = %e, "failed to commit worker diff");
        }
    }

    if should_preserve_worktree(final_status) {
        tracing::info!(
            worktree = %worktree_path.display(),
            status = ?final_status,
            "preserving worktree for review inspection"
        );
        None
    } else if should_commit_worker_diff(final_status) {
        // Approved work: remove worktree dir but keep branch for merge.
        match worktrees.detach_worktree(worker_session).await {
            Ok(branch) => Some(branch),
            Err(e) => {
                tracing::warn!(error = %e, "detach_worktree failed, falling back to full remove");
                let _ = worktrees.remove_worktree(worker_session).await;
                None
            }
        }
    } else {
        let _ = worktrees.remove_worktree(worker_session).await;
        None
    }
}
```

- [ ] **Step 2: Update all callers to capture the return value**

Every call to `apply_worktree_cleanup(...)` currently ignores the return. Change each to:
```rust
let preserved_branch = apply_worktree_cleanup(
    &mut worktrees,
    &outcome.worker_session,
    &final_status,
    &outcome.diff,
    &ctx.agent,
    &outcome.worktree_path,
).await;
```

There are ~10 call sites (lines ~2665, ~2709, ~2785, ~2817, ~2848, ~2879, ~2925, ~3028). All need the `let preserved_branch =` prefix. Only the call sites in the Approve/Modified/no-review paths will get `Some(branch)` — the rest get `None`.

- [ ] **Step 3: Thread branch name into `finalize`**

Update `finalize` signature to accept `worker_branch: Option<String>`:

```rust
fn finalize(
    funnel: &crate::event_funnel::FunnelHandle,
    worker_session: SessionId,
    final_status: DelegationStatus,
    diff: Option<String>,
    diff_summary: Option<spur_acp::DiffSummary>,
    summary: Option<String>,
    total_cost: f64,
    worker_branch: Option<String>,
) -> DelegationResult {
    funnel.emit(SpurEventBody::DelegationCompleted {
        worker_session,
        status: final_status.clone(),
    });
    DelegationResult {
        status: final_status,
        diff,
        diff_summary,
        summary,
        estimated_cost_usd: total_cost,
        worker_branch,
    }
}
```

Update all `finalize(...)` call sites to pass `preserved_branch` as the last argument. For call sites that don't have a `preserved_branch` variable (early returns, guard drops), pass `None`.

- [ ] **Step 4: Verify it compiles**

Run: `cargo build 2>&1 | tail -3`
Expected: `Finished` with 0 errors

- [ ] **Step 5: Commit**

```bash
git add crates/spur-core/src/orchestrator.rs
git commit -m "feat(orchestrator): three-way cleanup — detach approved branches"
```

---

### Task 4: New PlanTaskStatus states and AwaitingReview transition

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs:38-63` (PlanTaskStatus, PlanTaskEntry)
- Modify: `crates/spur-mcp/src/plan.rs:239-282` (completion handler in run_plan)

- [ ] **Step 1: Add new status variants and `worker_branch` field**

Replace `PlanTaskStatus` enum (lines 38–54):

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum PlanTaskStatus {
    Pending,
    Ready,
    Dispatched {
        delegation_id: String,
    },
    AwaitingReview {
        summary: Option<String>,
    },
    Approved {
        summary: Option<String>,
    },
    Rejected {
        feedback: Option<String>,
    },
    Failed {
        error: String,
    },
}
```

Add `worker_branch` to `PlanTaskEntry` (lines 57–63):

```rust
pub struct PlanTaskEntry {
    pub spec: PlanTask,
    pub status: PlanTaskStatus,
    pub result: Option<DelegationResult>,
    pub worker_branch: Option<String>,
}
```

- [ ] **Step 2: Update the completion handler in `run_plan`**

In `run_plan()` where `DelegationResult` is received (lines ~239–282), change the `Success` arm from `Completed { summary }` to `AwaitingReview { summary }`:

```rust
match &result.status {
    DelegationStatus::Success | DelegationStatus::Modified { .. } => {
        entry.status = PlanTaskStatus::AwaitingReview {
            summary: result.summary.clone(),
        };
        entry.worker_branch = result.worker_branch.clone();
    }
    DelegationStatus::Failed { error } => {
        entry.status = PlanTaskStatus::Failed {
            error: error.clone(),
        };
    }
    other => {
        entry.status = PlanTaskStatus::Failed {
            error: format!("{other:?}"),
        };
    }
}
entry.result = Some(result);
```

- [ ] **Step 3: Update `is_terminal` helper** (if one exists)

Search for any helper that checks if a task is terminal (for the JoinSet loop exit condition). Update to treat `AwaitingReview` as terminal for Phase 1. The existing code likely checks for `Completed` — replace with `AwaitingReview`.

- [ ] **Step 4: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -5`
Expected: `Finished` with 0 errors (may have warnings about unused `Completed`)

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/plan.rs
git commit -m "feat(plan): AwaitingReview/Approved/Rejected states + worker_branch"
```

---

### Task 5: Enriched `build_plan_status` with counts

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs:354-443` (`build_plan_status`)

- [ ] **Step 1: Rewrite `build_plan_status` to include counts and derived status**

Replace the function body to produce the enriched response:

```rust
pub fn build_plan_status(plan_id: &str, state: &PlanState) -> serde_json::Value {
    let mut counts = serde_json::Map::new();
    let mut pending = 0u32;
    let mut ready = 0u32;
    let mut dispatched = 0u32;
    let mut awaiting_review = 0u32;
    let mut approved = 0u32;
    let mut rejected = 0u32;
    let mut failed = 0u32;

    let mut tasks = Vec::new();

    for (id, entry) in &state.tasks {
        let mut task_obj = serde_json::Map::new();
        task_obj.insert("task_id".into(), json!(id));
        task_obj.insert("agent".into(), json!(entry.spec.agent));

        match &entry.status {
            PlanTaskStatus::Pending => { pending += 1; task_obj.insert("status".into(), json!("pending")); }
            PlanTaskStatus::Ready => { ready += 1; task_obj.insert("status".into(), json!("ready")); }
            PlanTaskStatus::Dispatched { delegation_id } => {
                dispatched += 1;
                task_obj.insert("status".into(), json!("dispatched"));
                task_obj.insert("delegation_id".into(), json!(delegation_id));
            }
            PlanTaskStatus::AwaitingReview { summary } => {
                awaiting_review += 1;
                task_obj.insert("status".into(), json!("awaiting_review"));
                if let Some(s) = summary { task_obj.insert("summary".into(), json!(s)); }
                if let Some(ref r) = entry.result {
                    if let Some(ref ds) = r.diff_summary {
                        task_obj.insert("diff_summary".into(), serde_json::to_value(ds).unwrap_or_default());
                    }
                }
                if let Some(ref b) = entry.worker_branch {
                    task_obj.insert("worker_branch".into(), json!(b));
                }
            }
            PlanTaskStatus::Approved { summary } => {
                approved += 1;
                task_obj.insert("status".into(), json!("approved"));
                if let Some(s) = summary { task_obj.insert("summary".into(), json!(s)); }
                if let Some(ref r) = entry.result {
                    if let Some(ref ds) = r.diff_summary {
                        task_obj.insert("diff_summary".into(), serde_json::to_value(ds).unwrap_or_default());
                    }
                }
                if let Some(ref b) = entry.worker_branch {
                    task_obj.insert("worker_branch".into(), json!(b));
                }
            }
            PlanTaskStatus::Rejected { feedback } => {
                rejected += 1;
                task_obj.insert("status".into(), json!("rejected"));
                if let Some(f) = feedback { task_obj.insert("feedback".into(), json!(f)); }
            }
            PlanTaskStatus::Failed { error } => {
                failed += 1;
                task_obj.insert("status".into(), json!("failed"));
                task_obj.insert("error".into(), json!(error));
            }
        }
        tasks.push(serde_json::Value::Object(task_obj));
    }

    let total = state.tasks.len() as u32;
    let all_workers_done = dispatched == 0 && ready == 0 && pending == 0;
    let ready_to_merge = approved == total && total > 0;

    let overall = if dispatched > 0 || ready > 0 || pending > 0 {
        "running"
    } else if awaiting_review > 0 {
        "awaiting_review"
    } else if approved == total && total > 0 {
        "approved"
    } else if failed == total && total > 0 {
        "failed"
    } else if rejected > 0 {
        "has_rejections"
    } else if failed > 0 && (approved > 0 || rejected > 0) {
        "partial"
    } else {
        "unknown"
    };

    json!({
        "plan_id": plan_id,
        "status": overall,
        "progress": format!("{}/{} completed, {} running, {} pending, {} failed",
            approved + rejected, total, dispatched, pending + ready, failed),
        "counts": {
            "total": total,
            "pending": pending,
            "ready": ready,
            "dispatched": dispatched,
            "awaiting_review": awaiting_review,
            "approved": approved,
            "rejected": rejected,
            "failed": failed
        },
        "all_workers_done": all_workers_done,
        "ready_to_merge": ready_to_merge,
        "tasks": tasks
    })
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -3`
Expected: `Finished` with 0 errors

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/src/plan.rs
git commit -m "feat(plan): enriched build_plan_status with counts + derived status"
```

---

### Task 6: `review_task()` function in plan.rs

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs` (add function after `build_plan_status`)

- [ ] **Step 1: Add `review_task` function**

```rust
/// Apply a brain review decision to a plan task.
/// Returns the updated plan status JSON (same shape as build_plan_status)
/// plus a `warnings` array for non-fatal side-effect failures.
pub async fn review_task(
    plan_id: &str,
    task_id: &str,
    decision: &str,
    feedback: Option<&str>,
    state: &mut PlanState,
    pm: Option<&spur_pm::PmService>,
) -> Result<serde_json::Value, String> {
    let entry = state
        .tasks
        .get_mut(task_id)
        .ok_or_else(|| format!("unknown task '{task_id}' in plan '{plan_id}'"))?;

    // Validate current state.
    let summary = match &entry.status {
        PlanTaskStatus::AwaitingReview { summary } => summary.clone(),
        other => {
            return Err(format!(
                "task '{task_id}' is not awaiting review (current: {other:?})"
            ));
        }
    };

    let mut warnings = Vec::<String>::new();

    match decision {
        "approve" => {
            entry.status = PlanTaskStatus::Approved { summary };
            // Sync beads: mark done + add comment.
            if let Some(pm) = pm {
                if let Some(issue_id) = entry.spec.issue_id.as_deref() {
                    let comment = format!(
                        "Brain approved: {}",
                        feedback.unwrap_or("meets acceptance criteria")
                    );
                    let update = spur_pm::types::IssueUpdate {
                        status: Some("done".to_string()),
                        comment: Some(comment),
                        ..Default::default()
                    };
                    if let Err(e) = pm.update_issue(issue_id, update).await {
                        warnings.push(format!("beads update failed: {e}"));
                    }
                }
            }
        }
        "reject" => {
            entry.status = PlanTaskStatus::Rejected {
                feedback: feedback.map(|s| s.to_string()),
            };
            // Sync beads: reopen + add comment.
            if let Some(pm) = pm {
                if let Some(issue_id) = entry.spec.issue_id.as_deref() {
                    let comment = format!(
                        "Brain rejected: {}",
                        feedback.unwrap_or("does not meet requirements")
                    );
                    let update = spur_pm::types::IssueUpdate {
                        status: Some("open".to_string()),
                        comment: Some(comment),
                        ..Default::default()
                    };
                    if let Err(e) = pm.update_issue(issue_id, update).await {
                        warnings.push(format!("beads update failed: {e}"));
                    }
                }
            }
        }
        _ => return Err(format!("invalid decision '{decision}': must be 'approve' or 'reject'")),
    }

    let mut result = build_plan_status(plan_id, state);
    if let Some(obj) = result.as_object_mut() {
        obj.insert("task_id".into(), json!(task_id));
        obj.insert("decision".into(), json!(decision));
        obj.insert("warnings".into(), json!(warnings));
    }
    Ok(result)
}
```

- [ ] **Step 2: Add necessary imports at top of plan.rs**

Ensure these are imported:
```rust
use serde_json::json;
```
(likely already present — verify)

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -3`
Expected: `Finished` with 0 errors

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/plan.rs
git commit -m "feat(plan): review_task() — approve/reject with beads sync"
```

---

### Task 7: MCP tool definitions in tools.rs

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs` (add two `_def()` functions + register in `tools_list`)

- [ ] **Step 1: Add `get_task_diff_def()` function**

Add before `tools_list()`:

```rust
pub fn get_task_diff_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_task_diff".to_string(),
        description: "Get the full unified diff for a plan task. Use after get_plan_status shows \
            tasks in awaiting_review, approved, rejected, or failed state. Returns the complete \
            diff, worker branch name, task description, and summary for brain code review."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The plan_id returned by submit_plan"
                },
                "task_id": {
                    "type": "string",
                    "description": "The task_id to inspect"
                }
            },
            "required": ["plan_id", "task_id"]
        }),
    }
}
```

- [ ] **Step 2: Add `review_task_def()` function**

```rust
pub fn review_task_def() -> ToolDefinition {
    ToolDefinition {
        name: "review_task".to_string(),
        description: "Submit a review decision for a plan task that is awaiting review. \
            Use get_task_diff first to read the diff, then call this to approve or reject. \
            On approve: beads issue marked done. On reject: beads issue reopened. \
            Returns updated plan status with counts and ready_to_merge flag."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The plan_id returned by submit_plan"
                },
                "task_id": {
                    "type": "string",
                    "description": "The task_id to review"
                },
                "decision": {
                    "type": "string",
                    "enum": ["approve", "reject"],
                    "description": "Review verdict"
                },
                "feedback": {
                    "type": "string",
                    "description": "Review notes (required for reject, optional for approve)"
                }
            },
            "required": ["plan_id", "task_id", "decision"]
        }),
    }
}
```

- [ ] **Step 3: Register both in `tools_list()`**

Add to the vector in `tools_list()` (lines ~672–697), after the `get_plan_status` entry:

```rust
        get_task_diff_def(),
        review_task_def(),
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build -p spur-mcp 2>&1 | tail -3`
Expected: `Finished` with 0 errors

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/tools.rs
git commit -m "feat(mcp): get_task_diff + review_task tool definitions"
```

---

### Task 8: MCP server handlers in server.rs

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` (add handlers + dispatch arms)

- [ ] **Step 1: Add `handle_get_task_diff` method**

Add to `impl McpCallbackServer`:

```rust
    async fn handle_get_task_diff(&self, args: &serde_json::Value) -> Result<String, String> {
        let plan_id = args["plan_id"]
            .as_str()
            .ok_or("missing plan_id")?
            .to_string();
        let task_id = args["task_id"]
            .as_str()
            .ok_or("missing task_id")?
            .to_string();

        // Clone Arc to release outer lock quickly.
        let plan_arc = {
            let plans = self.active_plans.lock().await;
            plans
                .get(&plan_id)
                .cloned()
                .ok_or_else(|| format!("unknown plan '{plan_id}'"))?
        };

        let state = plan_arc.lock().await;
        let entry = state
            .tasks
            .get(&task_id)
            .ok_or_else(|| format!("unknown task '{task_id}' in plan '{plan_id}'"))?;

        // Validate state — diff only available after worker finishes.
        match &entry.status {
            crate::plan::PlanTaskStatus::Pending
            | crate::plan::PlanTaskStatus::Ready => {
                return Err(format!("task '{task_id}' has not been dispatched yet"));
            }
            crate::plan::PlanTaskStatus::Dispatched { .. } => {
                return Err(format!("task '{task_id}' is still running — diff not available yet"));
            }
            _ => {} // AwaitingReview, Approved, Rejected, Failed — all have diffs
        }

        let mut resp = serde_json::Map::new();
        resp.insert("task_id".into(), json!(task_id));
        resp.insert("agent".into(), json!(entry.spec.agent));
        resp.insert("task_description".into(), json!(entry.spec.task));

        // Status as string.
        let status_str = match &entry.status {
            crate::plan::PlanTaskStatus::AwaitingReview { .. } => "awaiting_review",
            crate::plan::PlanTaskStatus::Approved { .. } => "approved",
            crate::plan::PlanTaskStatus::Rejected { .. } => "rejected",
            crate::plan::PlanTaskStatus::Failed { .. } => "failed",
            _ => "unknown",
        };
        resp.insert("status".into(), json!(status_str));

        if let Some(ref branch) = entry.worker_branch {
            resp.insert("worker_branch".into(), json!(branch));
        }

        if let Some(ref result) = entry.result {
            if let Some(ref diff) = result.diff {
                resp.insert("diff".into(), json!(diff));
            }
            if let Some(ref ds) = result.diff_summary {
                resp.insert("diff_summary".into(), serde_json::to_value(ds).unwrap_or_default());
            }
            if let Some(ref s) = result.summary {
                resp.insert("summary".into(), json!(s));
            }
        }

        let text = serde_json::to_string_pretty(&serde_json::Value::Object(resp))
            .map_err(|e| e.to_string())?;
        Ok(text)
    }
```

- [ ] **Step 2: Add `handle_review_task` method**

```rust
    async fn handle_review_task(&self, args: &serde_json::Value) -> Result<String, String> {
        let plan_id = args["plan_id"]
            .as_str()
            .ok_or("missing plan_id")?
            .to_string();
        let task_id = args["task_id"]
            .as_str()
            .ok_or("missing task_id")?
            .to_string();
        let decision = args["decision"]
            .as_str()
            .ok_or("missing decision")?;
        let feedback = args["feedback"].as_str();

        // Clone Arc to release outer lock quickly.
        let plan_arc = {
            let plans = self.active_plans.lock().await;
            plans
                .get(&plan_id)
                .cloned()
                .ok_or_else(|| format!("unknown plan '{plan_id}'"))?
        };

        let pm = self.pm.as_ref();

        let mut state = plan_arc.lock().await;
        let result = crate::plan::review_task(
            &plan_id, &task_id, decision, feedback, &mut state, pm,
        )
        .await?;

        let text = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
        Ok(text)
    }
```

- [ ] **Step 3: Add dispatch arms in `handle_tool_call`**

In the `match tool_name` block (lines ~391–417), add:

```rust
"get_task_diff" => match self.handle_get_task_diff(&args).await {
    Ok(text) => tool_result_text(id, &text),
    Err(e) => tool_result_error(id, &e),
},
"review_task" => match self.handle_review_task(&args).await {
    Ok(text) => tool_result_text(id, &text),
    Err(e) => tool_result_error(id, &e),
},
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build 2>&1 | tail -3`
Expected: `Finished` with 0 errors

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "feat(mcp): handle_get_task_diff + handle_review_task handlers"
```

---

### Task 9: Update existing plan tests

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs` (existing test module)

- [ ] **Step 1: Update test assertions for new status names**

The existing 8 tests in plan.rs use `PlanTaskStatus::Completed` and `PlanTaskStatus::Failed`. Update all `Completed { summary }` references to `AwaitingReview { summary }`. The test validation logic and test names remain the same — only the expected status variant changes.

Search for `Completed` in the test module and replace with `AwaitingReview`.

- [ ] **Step 2: Run tests**

Run: `cargo test -p spur-mcp 2>&1 | tail -10`
Expected: 8 tests pass, 0 failures

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/src/plan.rs
git commit -m "test(plan): update tests for AwaitingReview status"
```

---

### Task 10: Full build + integration smoke test

**Files:** None (verification only)

- [ ] **Step 1: Full workspace build**

Run: `cargo build 2>&1 | tail -5`
Expected: `Finished` with 0 errors, 0 warnings

- [ ] **Step 2: Run all tests**

Run: `cargo test 2>&1 | tail -15`
Expected: All test suites pass, 0 failures

- [ ] **Step 3: Verify tool registration**

Run: `cargo build && echo "Build OK"`
Then visually confirm `tools_list()` returns 24 tools (was 22) by checking the vector length in tools.rs.

- [ ] **Step 4: Final commit if any fixups**

```bash
git add -A
git commit -m "chore: fixups from integration verification"
```

(Skip if no changes needed.)

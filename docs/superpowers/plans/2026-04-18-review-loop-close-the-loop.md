# Review Loop — Close-the-Loop Bundle Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close four holes in the brain → worker → review loop: (1) `request_changes` never wrote to beads, (2) beads rejected `-s done` on approve causing silent drift, (3) `request_changes` at `MAX_ATTEMPTS` returned `Err` and left plans in limbo, (4) `get_task_diff` returned empty for a committed branch (unknown cause — spike only).

**Architecture:** Three surgical in-place edits to `crates/spur-mcp/src/plan.rs` and `crates/spur-pm`. No new enum variants, no new events, no new MCP tools, no TUI changes. Reuses existing `PlanTaskStatus::Rejected`, `PlanTaskReviewed` event, and rejection cascade. Plus one investigation spike delivering an RCA doc.

**Tech Stack:** Rust 2021, tokio 1.x, async-trait, anyhow, serde, beads (`br`) CLI via `tokio::process::Command`.

**Spec:** `docs/superpowers/specs/2026-04-18-review-loop-close-the-loop-design.md`

---

## File map

### Files modified

| File | Tasks | Purpose |
|---|---|---|
| `crates/spur-pm/src/beads.rs` | 1 | `BeadsAdapter` gets `closed_status: String` field |
| `crates/spur-pm/src/service.rs` | 1 | `PmService::try_new` adds `closed_status: Option<String>` param; `closed_status()` accessor |
| `crates/spur-cli/src/main.rs` | 1 | Caller passes `None` to `PmService::try_new` |
| `crates/spur-mcp/src/plan.rs` | 1, 2, 3 | Approve uses `pm.closed_status()` · `request_changes` writes beads comment after dispatch · auto-reject at MAX |
| `docs/rca/2026-04-18-get-task-diff-empty.md` | 4 | New RCA (spike deliverable) |

### Files NOT touched (guard rails)

- `crates/spur-acp/src/domain/events.rs` — no new event variants
- `crates/spur-tui/src/views/dashboard.rs` — no new status labels
- `crates/spur-mcp/src/tools.rs` — no new MCP tools
- `crates/spur-mcp/src/server.rs` — no new handlers

---

## Task 1: Configurable closed-status for PmService

**Goal:** `PmService` carries a configured "closed" status string (default `"closed"`) that the approve branch of `review_task` uses instead of hardcoded `"done"`. Fixes `beads update failed: Invalid status: done`.

**Files:**
- Modify: `crates/spur-pm/src/beads.rs` (struct `BeadsAdapter`, `connect`)
- Modify: `crates/spur-pm/src/service.rs` (`PmService::try_new` signature, new `closed_status` accessor)
- Modify: `crates/spur-cli/src/main.rs:446-451` (pass `None`)
- Modify: `crates/spur-mcp/src/plan.rs:985` (use `pm.closed_status()`)
- Test: inline in `crates/spur-pm/src/service.rs` (new `#[cfg(test)]` block if none exists) and `crates/spur-mcp/src/plan.rs` existing `mod tests`.

### Step 1.1: Read current shape of BeadsAdapter::connect

- [ ] **Read** `crates/spur-pm/src/beads.rs` lines 1-240 to locate:
  - The `BeadsAdapter` struct definition (fields)
  - The `BeadsAdapter::connect(repo_root: &Path)` function — it's an `async fn` returning `anyhow::Result<Self>`.

Expected: `BeadsAdapter` has fields like `cwd: PathBuf`, and possibly a lockfile handle. Note the exact field layout for the edit below.

### Step 1.2: Write the failing test for `closed_status()` default

- [ ] Append to `crates/spur-pm/src/service.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_status_defaults_to_closed_when_none() {
        // Pure: no I/O. Constructs only the closed-status field.
        // We test via the accessor on a constructed adapter-less stand-in:
        // since PmService::try_new is async and requires a repo, we instead
        // call the internal resolver directly.
        assert_eq!(super::resolve_closed_status(None), "closed");
        assert_eq!(super::resolve_closed_status(Some("resolved".to_string())), "resolved");
    }
}
```

### Step 1.3: Run test to verify it fails

Run: `cargo test -p spur-pm --lib closed_status_defaults_to_closed_when_none`
Expected: FAIL with `error[E0425]: cannot find function \`resolve_closed_status\``.

### Step 1.4: Add `resolve_closed_status` helper + `PmService.closed_status` field

- [ ] In `crates/spur-pm/src/service.rs`, add this helper above `impl PmService`:

```rust
/// Resolve the beads "closed" status string. Default is `"closed"` — the
/// value the default beads config accepts. Override via the argument for
/// projects whose beads config uses a different vocabulary (e.g., `"done"`,
/// `"resolved"`).
pub(crate) fn resolve_closed_status(override_value: Option<String>) -> String {
    override_value.unwrap_or_else(|| "closed".to_string())
}
```

- [ ] Add a `closed_status: String` field to `pub struct PmService`:

```rust
pub struct PmService {
    inner: PmBackendInner,
    bv: Option<BvAdapter>,
    closed_status: String,
}
```

- [ ] Add the accessor in `impl PmService`:

```rust
/// Returns the status string used to mark an issue as closed/done in the
/// configured PM backend. Default `"closed"` unless overridden at
/// construction.
pub fn closed_status(&self) -> &str {
    &self.closed_status
}
```

### Step 1.5: Run the helper test to verify it passes

Run: `cargo test -p spur-pm --lib closed_status_defaults_to_closed_when_none`
Expected: PASS.

### Step 1.6: Add `closed_status` param to `PmService::try_new`

- [ ] Change the signature of `try_new` in `crates/spur-pm/src/service.rs`:

```rust
pub async fn try_new(
    github_repo: Option<String>,
    beads_enabled: bool,
    github_enabled: bool,
    repo_root: &Path,
    closed_status: Option<String>,
) -> anyhow::Result<Option<Self>> {
```

- [ ] Thread the resolved value into each `return Ok(Some(Self { ... }))` by computing it once at the top:

```rust
pub async fn try_new(
    github_repo: Option<String>,
    beads_enabled: bool,
    github_enabled: bool,
    repo_root: &Path,
    closed_status: Option<String>,
) -> anyhow::Result<Option<Self>> {
    let resolved_closed = resolve_closed_status(closed_status);
    let beads_dir = repo_root.join(".beads");

    if beads_dir.is_dir() && beads_enabled {
        let beads = BeadsAdapter::connect(repo_root).await?;
        let bv = match BvAdapter::connect(repo_root).await {
            Ok(bv) => Some(bv),
            Err(e) => {
                tracing::info!("bv unavailable (graph analysis disabled): {e}");
                None
            }
        };
        let github = if github_enabled {
            Self::try_github(github_repo, repo_root).await
        } else {
            None
        };
        return Ok(Some(Self {
            inner: PmBackendInner::Beads { beads, github },
            bv,
            closed_status: resolved_closed,
        }));
    }

    if github_enabled {
        if let Some(gh) = Self::try_github(github_repo, repo_root).await {
            return Ok(Some(Self {
                inner: PmBackendInner::GitHub { adapter: gh },
                bv: None,
                closed_status: resolved_closed,
            }));
        }
    }

    Ok(None)
}
```

Note: Only the beads adapter actually uses `closed_status` — the GitHub path is still stored with the resolved value for symmetry (cheap, future-proof). If the adapter trait grows a `close_issue` method later, this field is already in place.

### Step 1.7: Fix the caller in spur-cli

- [ ] Edit `crates/spur-cli/src/main.rs:446-451`:

```rust
let pm_service = spur_pm::PmService::try_new(
    config.pm.github.as_ref().and_then(|g| g.repo.clone()),
    config.pm.beads.as_ref().map_or(true, |b| b.enabled),
    config.pm.github.as_ref().map_or(true, |g| g.enabled),
    &repo_root,
    None,
)
.await
.unwrap_or_else(|e| {
    tracing::warn!("PM service initialization failed: {e}");
    None
});
```

### Step 1.8: Swap the hardcoded `"done"` in plan.rs:985

- [ ] Edit `crates/spur-mcp/src/plan.rs:977-992`. Replace the block:

```rust
            // Beads sync (non-blocking).
            if let Some(pm) = pm {
                if let Some(ref id) = issue_id {
                    let comment = format!(
                        "Brain approved: {}",
                        feedback.unwrap_or("meets acceptance criteria")
                    );
                    let update = spur_pm::IssueUpdate {
                        status: Some("done".to_string()),
                        comment: Some(comment),
                        ..Default::default()
                    };
                    if let Err(e) = pm.update_issue(id, update).await {
                        warnings.push(format!("beads update failed: {e}"));
                    }
                }
            }
```

with:

```rust
            // Beads sync (non-blocking). Uses the configured closed-status
            // string — default `"closed"`; override via PmService::try_new.
            if let Some(pm) = pm {
                if let Some(ref id) = issue_id {
                    let comment = format!(
                        "Brain approved: {}",
                        feedback.unwrap_or("meets acceptance criteria")
                    );
                    let update = spur_pm::IssueUpdate {
                        status: Some(pm.closed_status().to_string()),
                        comment: Some(comment),
                        ..Default::default()
                    };
                    if let Err(e) = pm.update_issue(id, update).await {
                        warnings.push(format!("beads update failed: {e}"));
                    }
                }
            }
```

### Step 1.9: Build + run the whole workspace test

Run: `cargo build -p spur-pm -p spur-mcp -p spur-cli`
Expected: builds clean.

Run: `cargo test -p spur-pm --lib`
Expected: PASS (including `closed_status_defaults_to_closed_when_none`).

Run: `cargo test -p spur-mcp --lib`
Expected: PASS (no review_task tests yet exercise this path, but nothing should regress).

### Step 1.10: Commit

```bash
git add crates/spur-pm/src/service.rs crates/spur-cli/src/main.rs crates/spur-mcp/src/plan.rs
git commit -m "$(cat <<'EOF'
feat(pm): configurable closed-status for PmService

PmService::try_new accepts closed_status: Option<String>, default
"closed". Approve branch of review_task uses pm.closed_status() instead
of hardcoded "done", removing the `Invalid status: done` warning.

Closes T1 of close-the-loop-v2.
EOF
)"
```

---

## Task 2: `request_changes` writes beads comment after successful dispatch

**Goal:** Every `request_changes` decision emits a durable beads comment so human auditors see the full review thread. Write happens AFTER successful `try_send` so the audit trail never lies about a dispatch that didn't happen.

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs` (add helper `format_request_changes_comment`, invoke beads write in request_changes branch)
- Test: `crates/spur-mcp/src/plan.rs` existing `mod tests`.

### Step 2.1: Write the failing test for the comment-formatter helper

- [ ] Append to `mod tests` in `crates/spur-mcp/src/plan.rs` (near the existing `enriched_task_*` tests):

```rust
#[test]
fn format_request_changes_comment_includes_attempt_feedback_branch() {
    let c = super::format_request_changes_comment(
        "please rename `foo` to `bar`",
        2,
        super::MAX_ATTEMPTS,
        Some("spur/worker-bd-1mh-1"),
    );
    assert!(c.contains("Brain requested changes"));
    assert!(c.contains("attempt 2/3"));
    assert!(c.contains("please rename `foo` to `bar`"));
    assert!(c.contains("spur/worker-bd-1mh-1"));
}

#[test]
fn format_request_changes_comment_no_branch() {
    let c = super::format_request_changes_comment(
        "add a null check",
        1,
        super::MAX_ATTEMPTS,
        None,
    );
    assert!(c.contains("attempt 1/3"));
    assert!(c.contains("add a null check"));
    assert!(c.contains("(no branch yet)"));
}
```

### Step 2.2: Run test to verify it fails

Run: `cargo test -p spur-mcp --lib format_request_changes_comment`
Expected: FAIL with `cannot find function \`format_request_changes_comment\``.

### Step 2.3: Add the pure helper

- [ ] In `crates/spur-mcp/src/plan.rs`, add this helper above `pub async fn review_task` (around line 920):

```rust
/// Format the beads comment body for a `request_changes` review decision.
/// Pure — no I/O. Written to the PM backend after the new worker attempt
/// has been successfully dispatched.
pub(crate) fn format_request_changes_comment(
    feedback: &str,
    attempt: u32,
    max_attempts: u32,
    worker_branch: Option<&str>,
) -> String {
    let branch_line = worker_branch.unwrap_or("(no branch yet)");
    format!(
        "Brain requested changes (attempt {attempt}/{max_attempts}):\n{feedback}\n\nWorker branch: {branch_line}"
    )
}
```

### Step 2.4: Run test to verify it passes

Run: `cargo test -p spur-mcp --lib format_request_changes_comment`
Expected: PASS (both tests).

### Step 2.5: Wire the helper into `request_changes` AFTER `try_send`

- [ ] Edit `crates/spur-mcp/src/plan.rs` — inside the `"request_changes"` match arm, after `new_dispatches.push(...)` at line 1142 and before the match arm closes at line 1143, insert:

```rust
            // Best-effort audit write to beads. Runs AFTER try_send has
            // succeeded and state mutation is committed — if this fails the
            // dispatch still happened, which is what the state machine
            // already reflects. Pulls the worker_branch from
            // entry.history.last() (the just-superseded attempt) rather
            // than entry.worker_branch (cleared at line ~1125 above).
            //
            // NOTE: entry is borrowed mutably at this point from earlier in
            // the request_changes arm. We release the borrow before the
            // await by cloning out the fields we need.
            let issue_id_for_audit = entry.spec.issue_id.clone();
            let superseded_branch: Option<String> = entry.history
                .last()
                .and_then(|h| h.worker_branch.clone());
            if let (Some(pm), Some(id)) = (pm, issue_id_for_audit.as_ref()) {
                let comment = format_request_changes_comment(
                    fb,
                    new_attempt,
                    MAX_ATTEMPTS,
                    superseded_branch.as_deref(),
                );
                let update = spur_pm::IssueUpdate {
                    comment: Some(comment),
                    ..Default::default()
                };
                if let Err(e) = pm.update_issue(id, update).await {
                    warnings.push(format!("beads comment failed: {e}"));
                }
            }
```

**Why `entry.history.last()`, not `entry.worker_branch`:** the existing request_changes code clears `entry.worker_branch` at around line 1125 (the attempt being superseded is archived into `entry.history`). Reading `entry.worker_branch` here would yield `None` for every call. The archived branch lives in `entry.history.last()`.

**Why clone before the await:** `entry` is borrowed mutably from the code above in this match arm. Holding a mutable borrow across `.await` on `pm.update_issue(...)` is forbidden by the borrow checker. Cloning `issue_id` and `worker_branch` out of `entry` releases the borrow before the await.

### Step 2.6: Build

Run: `cargo build -p spur-mcp`
Expected: builds clean. If borrow checker still complains, the most likely cause is that `entry` is named in an outer `let` binding that lives too long — drop the binding explicitly with `let _ = entry;` or restructure to an `{ ... }` block that scopes the mutable borrow tightly.

### Step 2.7: Run tests

Run: `cargo test -p spur-mcp --lib`
Expected: PASS — all existing tests plus the two new `format_request_changes_comment_*` tests.

### Step 2.8: Commit

```bash
git add crates/spur-mcp/src/plan.rs
git commit -m "$(cat <<'EOF'
feat(mcp): request_changes writes beads comment after successful dispatch

After try_send succeeds and state mutates, write a best-effort beads
comment carrying the feedback, attempt N/MAX, and the just-superseded
worker branch (from entry.history.last()). Failure is a warning, not
an error — the signal path is already committed.

Closes T2 of close-the-loop-v2.
EOF
)"
```

---

## Task 3: Auto-reject `request_changes` at MAX_ATTEMPTS

**Goal:** When `request_changes` is called on a task already at `MAX_ATTEMPTS`, transition to `Rejected { feedback: "retries exhausted (N/MAX): <fb>" }` instead of returning `Err`. Plans always terminate.

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs:1042-1057` (the MAX guard at the top of the `request_changes` branch)
- Test: `crates/spur-mcp/src/plan.rs` existing `mod tests`.

### Step 3.1: Write the failing test

- [ ] Append to `mod tests` in `crates/spur-mcp/src/plan.rs`:

```rust
#[tokio::test]
async fn request_changes_at_max_attempts_auto_rejects() {
    use super::*;
    // Construct a PlanState with one task already at MAX_ATTEMPTS and
    // status AwaitingReview.
    let task_spec = task("T1", &[]);
    let entry = PlanTaskEntry {
        spec: task_spec,
        status: PlanTaskStatus::AwaitingReview { summary: Some("wip".into()) },
        result: None,
        worker_branch: None,
        attempt: MAX_ATTEMPTS,
        history: vec![
            AttemptRecord {
                attempt: 1,
                worker_branch: Some("spur/worker-1".into()),
                diff_summary: None,
                summary: None,
                feedback: "fix this".into(),
            },
            AttemptRecord {
                attempt: 2,
                worker_branch: Some("spur/worker-2".into()),
                diff_summary: None,
                summary: None,
                feedback: "fix that".into(),
            },
        ],
    };
    let mut state = PlanState {
        plan_id: "p1".into(),
        tasks: vec![entry],
        brain_session_id: agent_client_protocol::SessionId::new(std::sync::Arc::<str>::from("s1")),
    };

    let resp = review_task(
        "p1",
        "T1",
        "request_changes",
        Some("please try the other approach"),
        &mut state,
        None, // no pm
        None, // no sink
        None, None, None, // no delegation channel/tracker/arc
    )
    .await
    .expect("should Ok, not Err — MAX reached = auto-reject");

    // Status flipped to Rejected, not left AwaitingReview.
    let entry = &state.tasks[0];
    assert!(
        matches!(entry.status, PlanTaskStatus::Rejected { .. }),
        "expected Rejected at MAX_ATTEMPTS, got {:?}",
        entry.status
    );
    if let PlanTaskStatus::Rejected { feedback: Some(ref fb) } = entry.status {
        assert!(fb.contains("retries exhausted"));
        assert!(fb.contains("3/3"));
        assert!(fb.contains("please try the other approach"));
    } else {
        panic!("expected Rejected with feedback");
    }

    // Response carries decision=reject and a warning about auto-reject.
    let obj = resp.as_object().expect("resp is object");
    assert_eq!(obj.get("decision").and_then(|v| v.as_str()), Some("reject"));
    let warnings = obj.get("warnings").and_then(|v| v.as_array()).expect("warnings array");
    assert!(
        warnings.iter().any(|w| w.as_str().map_or(false, |s| s.contains("auto-rejected") && s.contains("MAX_ATTEMPTS"))),
        "expected auto-reject warning, got {warnings:?}"
    );

    // Overall status is terminal.
    let overall = obj.get("status").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        is_terminal_plan_status(overall),
        "expected terminal overall status, got {overall:?}"
    );
}
```

### Step 3.2: Run test to verify it fails

Run: `cargo test -p spur-mcp --lib request_changes_at_max_attempts_auto_rejects`
Expected: FAIL — currently `review_task` returns `Err("task is at max attempts (3); approve, reject, or leave as-is")` at plan.rs:1053-1057, so `.expect("should Ok...")` panics.

### Step 3.3: Replace the Err with auto-reject

- [ ] Edit `crates/spur-mcp/src/plan.rs:1047-1057`. Replace:

```rust
            // Validate attempt < MAX_ATTEMPTS.
            let entry = state
                .tasks
                .iter_mut()
                .find(|t| t.spec.task_id == task_id)
                .unwrap();
            if entry.attempt >= MAX_ATTEMPTS {
                return Err(format!(
                    "task is at max attempts ({MAX_ATTEMPTS}); approve, reject, or leave as-is"
                ));
            }
```

with:

```rust
            // At MAX_ATTEMPTS, instead of erroring and leaving the task in
            // AwaitingReview limbo, auto-transition to Rejected with an
            // exhaustion-prefixed feedback. Reuses the existing rejection
            // cascade and PlanTaskReviewed event. Downstream consumers
            // (is_terminal_plan_status, TUI, retry_plan_task sentinel)
            // all already handle Rejected correctly.
            {
                let entry = state
                    .tasks
                    .iter_mut()
                    .find(|t| t.spec.task_id == task_id)
                    .unwrap();
                if entry.attempt >= MAX_ATTEMPTS {
                    let exhausted_fb = format!(
                        "retries exhausted ({N}/{MAX}): {fb}",
                        N = entry.attempt,
                        MAX = MAX_ATTEMPTS,
                        fb = fb,
                    );
                    let issue_id = entry.spec.issue_id.clone();
                    entry.status = PlanTaskStatus::Rejected {
                        feedback: Some(exhausted_fb.clone()),
                    };
                    warnings.push(format!(
                        "auto-rejected: MAX_ATTEMPTS ({MAX_ATTEMPTS}) reached"
                    ));

                    // Rejection cascade: mark transitively-dependent tasks Failed.
                    mark_descendants_failed(task_id, state, &mut warnings);

                    // Best-effort beads comment with the retries-exhausted prefix.
                    if let Some(pm) = pm {
                        if let Some(ref id) = issue_id {
                            let comment = format!(
                                "Brain rejected (retries exhausted {N}/{MAX}): {fb}",
                                N = MAX_ATTEMPTS,
                                MAX = MAX_ATTEMPTS,
                                fb = fb,
                            );
                            let update = spur_pm::IssueUpdate {
                                comment: Some(comment),
                                ..Default::default()
                            };
                            if let Err(e) = pm.update_issue(id, update).await {
                                warnings.push(format!("beads comment failed: {e}"));
                            }
                        }
                    }

                    // Jump over the normal request_changes dispatch path —
                    // fall through to response-building + event emit. Since
                    // we already mutated state, we need to compute task_name
                    // and build the response ourselves. Use a labeled break
                    // out of the outer match arm.
                    // Rust can't `break` out of a match arm without a label;
                    // use an inner block returning early to response code.
                }
            }
```

**Important:** This won't compile yet — we've mutated state but still need to reach the response-building code at plan.rs:1151-1205. The cleanest refactor is to extract the response-building and event-emit into early return from the function. See Step 3.4.

### Step 3.4: Restructure request_changes to early-return after auto-reject

- [ ] Revise Step 3.3's insertion so the auto-reject path computes its own response and returns `Ok(...)` BEFORE the normal `request_changes` flow continues. Final shape of the `"request_changes"` match arm starting at line 1042:

```rust
        "request_changes" => {
            let fb = feedback.ok_or_else(|| {
                "request_changes requires feedback".to_string()
            })?;

            // Check for MAX_ATTEMPTS — if reached, auto-reject instead of
            // erroring. This guarantees plan termination.
            {
                let entry = state
                    .tasks
                    .iter_mut()
                    .find(|t| t.spec.task_id == task_id)
                    .unwrap();
                if entry.attempt >= MAX_ATTEMPTS {
                    let exhausted_fb = format!(
                        "retries exhausted ({}/{}): {}",
                        entry.attempt, MAX_ATTEMPTS, fb
                    );
                    let issue_id = entry.spec.issue_id.clone();
                    let attempt_at_reject = entry.attempt;
                    entry.status = PlanTaskStatus::Rejected {
                        feedback: Some(exhausted_fb.clone()),
                    };
                    warnings.push(format!(
                        "auto-rejected: MAX_ATTEMPTS ({MAX_ATTEMPTS}) reached"
                    ));

                    // Rejection cascade.
                    mark_descendants_failed(task_id, state, &mut warnings);

                    // Best-effort beads comment.
                    if let Some(pm) = pm {
                        if let Some(ref id) = issue_id {
                            let comment = format!(
                                "Brain rejected (retries exhausted {}/{}): {}",
                                MAX_ATTEMPTS, MAX_ATTEMPTS, fb
                            );
                            let update = spur_pm::IssueUpdate {
                                comment: Some(comment),
                                ..Default::default()
                            };
                            if let Err(e) = pm.update_issue(id, update).await {
                                warnings.push(format!("beads comment failed: {e}"));
                            }
                        }
                    }

                    // Build response and early-return with decision=reject.
                    let task_name = state
                        .tasks
                        .iter()
                        .find(|t| t.spec.task_id == task_id)
                        .map(|t| display_name(&t.spec.task))
                        .unwrap_or_default();
                    let mut resp = build_plan_status(plan_id, state);
                    if let serde_json::Value::Object(ref mut m) = resp {
                        m.insert("task_id".into(), serde_json::json!(task_id));
                        m.insert("task_name".into(), serde_json::json!(task_name));
                        m.insert("decision".into(), serde_json::json!("reject"));
                        m.insert("warnings".into(), serde_json::json!(warnings));
                    }
                    if let Some(sink) = sink {
                        sink.emit(spur_acp::SpurEventBody::PlanTaskReviewed {
                            plan_id: plan_id.to_string(),
                            task_id: task_id.to_string(),
                            task_name: Some(task_name),
                            decision: "reject".to_string(),
                            feedback: Some(exhausted_fb),
                            attempt: attempt_at_reject,
                            max_attempts: MAX_ATTEMPTS,
                        });
                    }
                    return Ok(resp);
                }
            }

            // --- normal request_changes path (unchanged from Phase 2) ---

            let (tx, tracker, arc) = match (delegation_tx, task_tracker, plan_arc.clone()) {
                (Some(a), Some(b), Some(c)) => (a, b, c),
                _ => {
                    return Err(
                        "request_changes requires orchestrator channel (internal error)"
                            .to_string(),
                    );
                }
            };

            // ... (rest of existing request_changes body from line 1068 onward, unchanged)
```

- [ ] Leave the existing body from line 1068 (`// Capture the attempt being superseded...`) through the `new_dispatches.push(...)` at line 1142 UNCHANGED.

- [ ] Task 2's beads-comment write block (added in Step 2.5-2.6) also stays unchanged — it sits after `new_dispatches.push` and before the match arm closes.

### Step 3.5: Build

Run: `cargo build -p spur-mcp`
Expected: builds clean. If borrow checker complains, see Step 2.7's note about splitting mut borrow from the pm call.

### Step 3.6: Run the new test to verify it passes

Run: `cargo test -p spur-mcp --lib request_changes_at_max_attempts_auto_rejects`
Expected: PASS.

### Step 3.7: Run full test suite to check no regressions

Run: `cargo test -p spur-mcp --lib`
Expected: PASS. Pay attention to any Phase 2 test that asserted an `Err` on MAX — if present, either the test was wrong (limbo bug) and should be updated, or the test asserted a sentinel we now need to preserve.

Run: `cargo test -p spur-pm --lib`
Expected: PASS.

### Step 3.8: Commit

```bash
git add crates/spur-mcp/src/plan.rs
git commit -m "$(cat <<'EOF'
feat(mcp): auto-reject request_changes at MAX_ATTEMPTS

Instead of returning Err and leaving the task in AwaitingReview limbo,
transition to Rejected with feedback prefixed "retries exhausted
(N/MAX): <fb>". Reuses existing rejection cascade + PlanTaskReviewed
event. Distinguishable from a merit-based reject by the beads comment
prefix "Brain rejected (retries exhausted N/MAX): ...".

Plans now always reach a terminal state when MAX_ATTEMPTS is hit.

Closes T3 of close-the-loop-v2.
EOF
)"
```

---

## Task 4: Spike — RCA for `get_task_diff` empty-result

**Goal:** Produce a reproduction + root-cause + recommendation doc for why `get_task_diff(bd-1mh.2)` returned empty despite commit `95e8b73` existing on the worker branch. No code change in this task.

**Files:**
- Create: `docs/rca/2026-04-18-get-task-diff-empty.md`

### Step 4.1: Read the existing RCA template

- [ ] Read `docs/rca/2026-04-16-delegation-transport-mismatch.md` end-to-end to learn the shape: title, date, severity, observed behavior, reproduction, root cause, timeline, remediation.

### Step 4.2: Locate `get_task_diff` implementation

- [ ] Run: `grep -rn "fn get_task_diff\|handle_get_task_diff\|get_task_diff_def" crates/spur-mcp/src/`

Note which file defines it, what it returns for empty-diff cases (`Ok(None)` vs `Ok(String::new())` vs `Err(...)`), and how it computes the diff (base branch? fetch? local commits only?).

### Step 4.3: Attempt reproduction

- [ ] Identify the plan log: `.spur/events/` contains ndjson event logs from the bd-1mh run. Grep for `"bd-1mh.2"` and `"get_task_diff"` to find the exact request/response pair.

- [ ] Check whether the worker branch is still in git: `git branch -a | grep bd-1mh`. If it is, reproduce the diff call by invoking `get_task_diff` directly (via MCP server if running, or via a targeted integration test).

- [ ] Compare to `git show 95e8b73 --stat` and `git diff main..<worker-branch> --stat`. Note which matches what `get_task_diff` returned.

### Step 4.4: Identify root cause

- [ ] Possible causes to investigate:
  - **Base branch wrong:** diff computed against a branch that already contains 95e8b73 (no delta).
  - **Fetch missing:** orchestrator's git worktree hasn't fetched the worker's commits (if worker uses a separate worktree).
  - **Empty-success path:** a code path in `get_task_diff` returns `Ok(String::new())` or `Ok(None)` on a git-exit-status-0 with empty stdout, silently hiding a real failure.
  - **Wrong subject SHA:** the tool reads `entry.result.commit_sha` which wasn't populated.
  - **Timing:** diff was fetched before worker pushed; stale cache.

- [ ] Determine the actual cause from the logs + code inspection. Write it up.

### Step 4.5: Propose fix options

- [ ] Write one paragraph per viable fix option, with:
  - Code location
  - Proposed change
  - Estimated LOC
  - Risk (regression surface)

### Step 4.6: Write and commit the RCA

- [ ] Create `docs/rca/2026-04-18-get-task-diff-empty.md` with this outline:

```markdown
# RCA: `get_task_diff` empty result for bd-1mh.2

**Date:** 2026-04-18
**Severity:** medium — brain reviews blind; no user-visible crash.
**Status:** investigation only; fix deferred to follow-up spec if warranted.

## Observed

During bd-1mh epic execution on 2026-04-17, `get_task_diff(bd-1mh.2)`
returned [exact response shape] despite commit `95e8b73` existing on
worker branch [branch name]. Brain proceeded to review via direct
`git show 95e8b73` inspection instead.

## Reproduction

[steps from Step 4.3]

## Root cause

[from Step 4.4]

## Options considered

### Option A: ...
### Option B: ...

## Recommendation

[one of: "file follow-up issue and fix in next spec", "transient, no
action needed", "design change required — bump to a new spec"].

## Follow-up

[link to any beads issue filed].
```

- [ ] Commit:

```bash
git add docs/rca/2026-04-18-get-task-diff-empty.md
git commit -m "$(cat <<'EOF'
docs(rca): get_task_diff empty-result for bd-1mh.2

Spike deliverable for close-the-loop-v2 T4. No code change.

EOF
)"
```

---

## Final verification

### Step F.1: Full workspace build

- [ ] Run: `cargo build --workspace`
Expected: clean.

### Step F.2: Full workspace test

- [ ] Run: `cargo test --workspace --lib`
Expected: all PASS. New tests:
- `spur-pm::service::tests::closed_status_defaults_to_closed_when_none`
- `spur-mcp::plan::tests::format_request_changes_comment_includes_attempt_feedback_branch`
- `spur-mcp::plan::tests::format_request_changes_comment_no_branch`
- `spur-mcp::plan::tests::request_changes_at_max_attempts_auto_rejects`

### Step F.3: Spec success-criteria checklist

Verify each criterion from the spec:

- [ ] **"Running the `bd-1mh` replay end-to-end produces no `beads update failed: Invalid status` warnings."** — Task 1 closes this. The default `"closed"` value is what the beads config accepts.
- [ ] **"`br show <issue_id>` shows a comment for every brain review decision..."** — Task 2 closes the `request_changes` gap; Task 3 adds the retries-exhausted prefix; approve/reject already had comments.
- [ ] **"A plan with a task that hits `request_changes` at MAX_ATTEMPTS terminates..."** — Task 3 verified by `request_changes_at_max_attempts_auto_rejects`; overall becomes `has_rejections` which `is_terminal_plan_status` returns true for.
- [ ] **"`docs/rca/2026-04-18-get-task-diff-empty.md` exists..."** — Task 4.

### Step F.4: Guard-rail scan (re-state spec non-goals)

Run: `grep -rn "PlanTaskStatus::Exhausted\|PlanTaskStatus::Stalled\|mark_stalled\|PlanTaskExhausted\|PlanTaskStalled" crates/`
Expected: **zero matches** (outside spec/plan docs). Any match means scope crept.

Run: `git diff main..HEAD --stat`
Expected: ~95 LOC of production changes (per spec estimate), plus test code, plus 1 RCA doc. If far over, investigate.

---

## Rollback plan

Each task is a single commit. Rollback = `git revert <sha>` per task. Tasks 1, 2, 3 are independently revertable: Task 1 leaves hardcoded `"done"` (the known-bad state), Task 2 loses beads comments on request_changes, Task 3 restores the MAX_ATTEMPTS limbo bug. Task 4 is a doc — trivially revertable.

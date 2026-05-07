# br-xh7: G-strict overlay sibling-conflict prediction + auto-staging-branch fallback

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the parallel-sibling overlay-conflict failure class by (1) auto-serializing tasks with overlapping `context_files` at plan submit time, (2) running a pre-dispatch overlay dry-run in the reconciler as defense-in-depth, and (3) exposing a `plan_truncate_and_restart` MCP tool for mid-plan recovery when prevention fails.

**Architecture:** Three layered changes to spur-mcp's plan engine, dispatched serially to avoid the very failure mode they fix.

- **Task 1 (auto-serialize-siblings):** Extend `crate::plan::validate_plan` flow with a post-validation pass that injects synthetic `depends_on` edges between unrelated tasks whose `context_files` overlap. `submit_plan` reports the injected edges in its response so the brain can audit.
- **Task 2 (predispatch-dry-run):** Extract `preview_task_base_impl` from the MCP handler into a callable helper. Call it from `tick_once` between overlay-spec computation and `persist_dispatch_intent`. On predicted conflict, transition the task to `BlockedOnSetupConflict` without spawning a worker.
- **Task 3 (truncate-and-restart-tool):** New `plan_truncate_and_restart` MCP tool. Creates `spur/plan-staging/{plan_id}` by cherry-picking approved tips in DAG order; supersedes remaining tasks in the original plan; submits a new plan with `BaseSpec::Branch(...)` for each.

**Tech Stack:** Rust, tokio, serde, anyhow. spur-mcp crate (`crates/spur-mcp/`), spur-worktree crate (`crates/spur-worktree/`). Existing test conventions: `#[cfg(test)] mod tests` blocks colocated with implementation, `cargo test -p spur-mcp` to run.

**Sequencing:** Strictly serial (Task 1 → Task 2 → Task 3). Task 2 reuses helpers Task 1 may add for context_files overlap detection. Task 3 reuses cherry-pick infrastructure already exercised by `preview_task_base_impl` via the helper extracted in Task 2. Parallelizing these would re-create the failure mode the plan is fixing — both Task 1 and Task 3 touch `crates/spur-mcp/src/server.rs` (submit_plan + new tool dispatch arm) and `crates/spur-mcp/src/tools.rs` (tool definitions).

**Out of scope:**
- Option C (auto per-plan integration branch) — spec-rejected (`docs/superpowers/specs/2026-05-01-bd-1dwm-design.md:47-58`); the manual `plan_truncate_and_restart` (Task 3) is the explicit alternative.
- Region-level (diff-aware) conflict prediction at submit time — file-level granularity is sufficient per the brainstorming decision; brain may accept conservative serialization.
- Removal/relaxation of synthetic edges via a brain-controlled override flag — defer to a follow-up issue once we have data on false-positive frequency.

---

## Task 1: auto-serialize-siblings — file-overlap-driven dependency injection in submit_plan

**Files:**
- Modify: `crates/spur-mcp/src/plan/mod.rs:1064-1099` (validation block — add `auto_serialize_overlaps` function and call site).
- Modify: `crates/spur-mcp/src/plan/mod.rs:4039-4218` (test module — add overlap detection tests).
- Modify: `crates/spur-mcp/src/server.rs:4882-4895` (`submit_plan` handler — call new pass after `validate_plan`).
- Modify: `crates/spur-mcp/src/server.rs:5069-5099` (`submit_plan` response shape — include `auto_serialized` list).
- Test: `crates/spur-mcp/src/plan/mod.rs` (in-file `mod tests`).

### Sub-task 1A: define overlap detector + synthetic-edge injector

- [ ] **Step 1A.1: Write the failing test for `find_sibling_overlaps`**

Append to `crates/spur-mcp/src/plan/mod.rs` inside `mod tests` (after the existing `three_node_cycle_rejected` test, near line 4218):

```rust
fn task_with_files(id: &str, deps: &[&str], files: &[&str]) -> PlanTask {
    PlanTask {
        task_id: id.into(),
        agent: "test-agent".into(),
        task: "test task".into(),
        depends_on: deps.iter().map(|s| s.to_string()).collect(),
        issue_id: None,
        context_files: files.iter().map(|s| s.to_string()).collect(),
    }
}

#[test]
fn find_sibling_overlaps_detects_unrelated_pair_sharing_file() {
    // Two tasks at the same DAG level, neither depends on the other,
    // both touch orchestrator.rs. Expect one overlap entry.
    let tasks = vec![
        task_with_files("A", &[], &["crates/spur-core/src/orchestrator.rs"]),
        task_with_files("B", &[], &["crates/spur-core/src/orchestrator.rs"]),
    ];
    let overlaps = super::find_sibling_overlaps(&tasks);
    assert_eq!(overlaps.len(), 1);
    let entry = &overlaps[0];
    // The injected edge is deterministic: lexicographically lower id
    // becomes the dep of the higher id (so synthetic edges form an order
    // independent of input task order).
    assert_eq!(entry.from, "A");
    assert_eq!(entry.to, "B");
    assert_eq!(entry.shared_files, vec!["crates/spur-core/src/orchestrator.rs"]);
}

#[test]
fn find_sibling_overlaps_skips_pairs_with_existing_transitive_dep() {
    // A → B → C. A and C share a file but A is already a transitive dep of C.
    let tasks = vec![
        task_with_files("A", &[], &["shared.rs"]),
        task_with_files("B", &["A"], &[]),
        task_with_files("C", &["B"], &["shared.rs"]),
    ];
    let overlaps = super::find_sibling_overlaps(&tasks);
    assert!(
        overlaps.is_empty(),
        "expected no overlaps, got {:?}",
        overlaps
    );
}

#[test]
fn find_sibling_overlaps_skips_disjoint_files() {
    let tasks = vec![
        task_with_files("A", &[], &["foo.rs"]),
        task_with_files("B", &[], &["bar.rs"]),
    ];
    assert!(super::find_sibling_overlaps(&tasks).is_empty());
}

#[test]
fn find_sibling_overlaps_handles_empty_context_files() {
    // A task with no context_files cannot overlap with anything.
    let tasks = vec![
        task_with_files("A", &[], &[]),
        task_with_files("B", &[], &["foo.rs"]),
    ];
    assert!(super::find_sibling_overlaps(&tasks).is_empty());
}

#[test]
fn find_sibling_overlaps_diamond_dag_three_siblings() {
    // The br-77i incident pattern: A depends on root, B/C/D are parallel
    // siblings all touching orchestrator.rs.
    let tasks = vec![
        task_with_files("root", &[], &["root.rs"]),
        task_with_files("B", &["root"], &["orch.rs"]),
        task_with_files("C", &["root"], &["orch.rs"]),
        task_with_files("D", &["root"], &["orch.rs"]),
        task_with_files("sink", &["B", "C", "D"], &[]),
    ];
    let overlaps = super::find_sibling_overlaps(&tasks);
    // Three unordered pairs (B,C), (B,D), (C,D) → three synthetic edges.
    assert_eq!(overlaps.len(), 3);
    let pairs: std::collections::HashSet<(&str, &str)> = overlaps
        .iter()
        .map(|o| (o.from.as_str(), o.to.as_str()))
        .collect();
    assert!(pairs.contains(&("B", "C")));
    assert!(pairs.contains(&("B", "D")));
    assert!(pairs.contains(&("C", "D")));
}
```

- [ ] **Step 1A.2: Run tests to verify FAIL**

Run: `cargo test -p spur-mcp --lib find_sibling_overlaps`
Expected: All 5 tests fail to compile — `find_sibling_overlaps` not defined.

- [ ] **Step 1A.3: Implement `SiblingOverlap` struct and `find_sibling_overlaps`**

In `crates/spur-mcp/src/plan/mod.rs`, immediately after `validate_plan` (at line 1099), add:

```rust
/// A single auto-injected dependency edge. Returned by `find_sibling_overlaps`
/// and surfaced in the `submit_plan` response so the brain can audit which
/// tasks were serialized.
#[derive(Debug, Clone, Serialize)]
pub struct SiblingOverlap {
    /// Task that must complete first (lex-lower task_id of the pair).
    pub from: String,
    /// Task that gets the synthetic `depends_on: from` edge (lex-higher).
    pub to: String,
    /// The intersection of `context_files` that triggered the synthetic edge.
    pub shared_files: Vec<String>,
}

/// Detect pairs of tasks where:
///   1. Neither is a transitive ancestor of the other (i.e., they could
///      currently dispatch in parallel), AND
///   2. Their `context_files` sets intersect.
/// For each such pair, produce one `SiblingOverlap` with `from` = lex-lower
/// task_id, `to` = lex-higher task_id. Determinism matters: callers will
/// inject `depends_on` edges based on this output, and the synthetic graph
/// must not depend on input ordering.
pub fn find_sibling_overlaps(tasks: &[PlanTask]) -> Vec<SiblingOverlap> {
    // Build adjacency (forward edges: dep → dependent) and reachability.
    let id_to_idx: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.task_id.as_str(), i))
        .collect();
    let n = tasks.len();
    // Transitive closure via DFS from each node.
    let mut reachable: Vec<HashSet<usize>> = vec![HashSet::new(); n];
    for (i, t) in tasks.iter().enumerate() {
        let mut stack: Vec<usize> = t
            .depends_on
            .iter()
            .filter_map(|d| id_to_idx.get(d.as_str()).copied())
            .collect();
        while let Some(node) = stack.pop() {
            if reachable[i].insert(node) {
                for dep in &tasks[node].depends_on {
                    if let Some(&dep_idx) = id_to_idx.get(dep.as_str()) {
                        if !reachable[i].contains(&dep_idx) {
                            stack.push(dep_idx);
                        }
                    }
                }
            }
        }
    }

    let related = |a: usize, b: usize| reachable[a].contains(&b) || reachable[b].contains(&a);

    let mut overlaps = Vec::new();
    for i in 0..n {
        for j in (i + 1)..n {
            if related(i, j) {
                continue;
            }
            let files_i: HashSet<&str> =
                tasks[i].context_files.iter().map(String::as_str).collect();
            let shared: Vec<String> = tasks[j]
                .context_files
                .iter()
                .filter(|f| files_i.contains(f.as_str()))
                .cloned()
                .collect();
            if shared.is_empty() {
                continue;
            }
            // Determinism: order pair by lex task_id.
            let (from, to) = if tasks[i].task_id <= tasks[j].task_id {
                (&tasks[i].task_id, &tasks[j].task_id)
            } else {
                (&tasks[j].task_id, &tasks[i].task_id)
            };
            let mut shared_sorted = shared;
            shared_sorted.sort();
            overlaps.push(SiblingOverlap {
                from: from.clone(),
                to: to.clone(),
                shared_files: shared_sorted,
            });
        }
    }
    // Sort by (from, to) for stable output.
    overlaps.sort_by(|a, b| a.from.cmp(&b.from).then_with(|| a.to.cmp(&b.to)));
    overlaps
}
```

- [ ] **Step 1A.4: Run tests to verify PASS**

Run: `cargo test -p spur-mcp --lib find_sibling_overlaps`
Expected: All 5 tests pass.

- [ ] **Step 1A.5: Commit**

```bash
git add crates/spur-mcp/src/plan/mod.rs
git commit -m "feat(spur-mcp): add find_sibling_overlaps for plan-time conflict detection (br-xh7 task 1A)"
```

### Sub-task 1B: apply synthetic edges to PlanTask list

- [ ] **Step 1B.1: Write the failing test for `apply_sibling_overlaps`**

Append to the same `mod tests`:

```rust
#[test]
fn apply_sibling_overlaps_injects_edges_and_preserves_originals() {
    let mut tasks = vec![
        task_with_files("A", &[], &["shared.rs"]),
        task_with_files("B", &["A"], &["other.rs"]),
        task_with_files("C", &[], &["shared.rs"]),
    ];
    let overlaps = super::find_sibling_overlaps(&tasks);
    assert_eq!(overlaps.len(), 1);
    super::apply_sibling_overlaps(&mut tasks, &overlaps);

    let c = tasks.iter().find(|t| t.task_id == "C").unwrap();
    assert!(
        c.depends_on.iter().any(|d| d == "A"),
        "expected synthetic edge A→C, got {:?}",
        c.depends_on
    );
    let b = tasks.iter().find(|t| t.task_id == "B").unwrap();
    assert_eq!(
        b.depends_on,
        vec!["A".to_string()],
        "B's original deps must be preserved"
    );
}

#[test]
fn apply_sibling_overlaps_idempotent_on_existing_edge() {
    // If somehow the synthetic edge is already present, we must not duplicate.
    let mut tasks = vec![
        task_with_files("A", &[], &["shared.rs"]),
        task_with_files("B", &["A"], &["shared.rs"]),
    ];
    // After find_sibling_overlaps: A and B are related (B depends on A), so
    // no overlap is emitted. Direct invocation simulates a stale synthetic
    // edge slipping through.
    let synthetic = vec![super::SiblingOverlap {
        from: "A".into(),
        to: "B".into(),
        shared_files: vec!["shared.rs".into()],
    }];
    super::apply_sibling_overlaps(&mut tasks, &synthetic);
    let b = tasks.iter().find(|t| t.task_id == "B").unwrap();
    assert_eq!(b.depends_on, vec!["A".to_string()]);
}

#[test]
fn apply_sibling_overlaps_keeps_validate_plan_passing() {
    // After injecting synthetic edges, the resulting plan must still pass
    // validate_plan (no cycles introduced — synthetic edges go lex-lower→higher,
    // and original DAG was acyclic).
    let mut tasks = vec![
        task_with_files("A", &[], &["x.rs"]),
        task_with_files("B", &[], &["x.rs"]),
        task_with_files("C", &[], &["x.rs"]),
    ];
    let overlaps = super::find_sibling_overlaps(&tasks);
    super::apply_sibling_overlaps(&mut tasks, &overlaps);
    super::validate_plan(&tasks).expect("post-injection plan must validate");
}
```

- [ ] **Step 1B.2: Run tests to verify FAIL**

Run: `cargo test -p spur-mcp --lib apply_sibling_overlaps`
Expected: 3 tests fail to compile — `apply_sibling_overlaps` not defined.

- [ ] **Step 1B.3: Implement `apply_sibling_overlaps`**

Below `find_sibling_overlaps` in `crates/spur-mcp/src/plan/mod.rs`:

```rust
/// Mutates `tasks` in place: for each `SiblingOverlap`, append `from` to the
/// `depends_on` of the task with id `to` (unless already present).
pub fn apply_sibling_overlaps(tasks: &mut [PlanTask], overlaps: &[SiblingOverlap]) {
    if overlaps.is_empty() {
        return;
    }
    let id_to_idx: HashMap<String, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.task_id.clone(), i))
        .collect();
    for o in overlaps {
        let Some(&idx) = id_to_idx.get(&o.to) else {
            continue;
        };
        if !tasks[idx].depends_on.iter().any(|d| d == &o.from) {
            tasks[idx].depends_on.push(o.from.clone());
        }
    }
}
```

- [ ] **Step 1B.4: Run tests to verify PASS**

Run: `cargo test -p spur-mcp --lib apply_sibling_overlaps`
Expected: All 3 tests pass.

- [ ] **Step 1B.5: Commit**

```bash
git add crates/spur-mcp/src/plan/mod.rs
git commit -m "feat(spur-mcp): add apply_sibling_overlaps to inject synthetic deps (br-xh7 task 1B)"
```

### Sub-task 1C: wire into submit_plan and surface in response

- [ ] **Step 1C.1: Write the failing integration test for submit_plan**

Open `crates/spur-mcp/src/server.rs` and locate the existing submit_plan tests. If none exist for this concern, add a new test at the bottom of the file's existing `#[cfg(test)] mod tests` block. (If no `mod tests` exists in `server.rs`, add a unit test in `crates/spur-mcp/src/plan/mod.rs` that calls a new helper `submit_plan_normalize_tasks(&mut Vec<PlanTask>) -> Vec<SiblingOverlap>` instead — see Step 1C.3.)

Concrete test (add to `crates/spur-mcp/src/plan/mod.rs` `mod tests`):

```rust
#[test]
fn submit_plan_normalize_tasks_returns_injected_overlaps() {
    // Diamond-DAG sibling-file-overlap (the br-77i incident pattern).
    let mut tasks = vec![
        task_with_files("root", &[], &["root.rs"]),
        task_with_files("X", &["root"], &["orch.rs"]),
        task_with_files("Y", &["root"], &["orch.rs"]),
        task_with_files("sink", &["X", "Y"], &[]),
    ];
    let overlaps = super::submit_plan_normalize_tasks(&mut tasks)
        .expect("normalize should succeed for valid input");
    assert_eq!(overlaps.len(), 1);
    assert_eq!(overlaps[0].from, "X");
    assert_eq!(overlaps[0].to, "Y");
    let y = tasks.iter().find(|t| t.task_id == "Y").unwrap();
    assert!(
        y.depends_on.iter().any(|d| d == "X"),
        "Y must now depend on X"
    );
}

#[test]
fn submit_plan_normalize_tasks_propagates_validate_errors() {
    let mut tasks = vec![task_with_files("A", &["A"], &[])]; // self-cycle
    let err = super::submit_plan_normalize_tasks(&mut tasks).unwrap_err();
    assert!(err.contains("Cycle"));
}
```

- [ ] **Step 1C.2: Run test to verify FAIL**

Run: `cargo test -p spur-mcp --lib submit_plan_normalize_tasks`
Expected: 2 tests fail to compile — `submit_plan_normalize_tasks` not defined.

- [ ] **Step 1C.3: Implement `submit_plan_normalize_tasks`**

In `crates/spur-mcp/src/plan/mod.rs`, immediately after `apply_sibling_overlaps`:

```rust
/// Submit-time normalization pipeline: validates the plan, computes sibling
/// overlaps, applies synthetic edges, and re-validates (defense-in-depth
/// against any future logic that could introduce cycles). Returns the list
/// of injected overlaps so `submit_plan` can surface them in its response.
pub fn submit_plan_normalize_tasks(
    tasks: &mut Vec<PlanTask>,
) -> Result<Vec<SiblingOverlap>, String> {
    validate_plan(tasks)?;
    let overlaps = find_sibling_overlaps(tasks);
    apply_sibling_overlaps(tasks, &overlaps);
    // Re-validate after mutation. Synthetic edges should never introduce a
    // cycle (lex-ordered pairs are acyclic by construction), but a future
    // refactor could break this — fail loudly if it does.
    validate_plan(tasks).map_err(|e| {
        format!(
            "auto-serialize-siblings produced an invalid plan (this is a bug): {e}"
        )
    })?;
    Ok(overlaps)
}
```

- [ ] **Step 1C.4: Run test to verify PASS**

Run: `cargo test -p spur-mcp --lib submit_plan_normalize_tasks`
Expected: 2 tests pass.

- [ ] **Step 1C.5: Wire into the `submit_plan` handler**

In `crates/spur-mcp/src/server.rs` around line 4882-4895, replace:

```rust
let tasks: Vec<crate::plan::PlanTask> = match tasks_val
    .into_iter()
    .map(serde_json::from_value)
    .collect::<Result<Vec<_>, _>>()
{
    Ok(t) => t,
    Err(e) => {
        return JsonRpcResponse::invalid_params(id, format!("Invalid task format: {e}"))
    }
};

if let Err(e) = crate::plan::validate_plan(&tasks) {
    return JsonRpcResponse::invalid_params(id, e);
}
```

with:

```rust
let mut tasks: Vec<crate::plan::PlanTask> = match tasks_val
    .into_iter()
    .map(serde_json::from_value)
    .collect::<Result<Vec<_>, _>>()
{
    Ok(t) => t,
    Err(e) => {
        return JsonRpcResponse::invalid_params(id, format!("Invalid task format: {e}"))
    }
};

let auto_serialized = match crate::plan::submit_plan_normalize_tasks(&mut tasks) {
    Ok(overlaps) => overlaps,
    Err(e) => return JsonRpcResponse::invalid_params(id, e),
};
```

- [ ] **Step 1C.6: Surface `auto_serialized` in the response**

In `crates/spur-mcp/src/server.rs` around line 5090-5099, replace:

```rust
JsonRpcResponse::success(
    id,
    json!({
        "continuation_will_fire": true,
        "content": [{
            "type": "text",
            "text": response_text
        }]
    }),
)
```

with:

```rust
let response_text = if auto_serialized.is_empty() {
    response_text
} else {
    let edges: Vec<String> = auto_serialized
        .iter()
        .map(|o| {
            format!(
                "  {} → {} (shared: {})",
                o.from,
                o.to,
                o.shared_files.join(", ")
            )
        })
        .collect();
    format!(
        "{response_text}\n\nAuto-serialized {} sibling pair(s) with overlapping context_files:\n{}",
        auto_serialized.len(),
        edges.join("\n")
    )
};

JsonRpcResponse::success(
    id,
    json!({
        "continuation_will_fire": true,
        "auto_serialized": auto_serialized,
        "content": [{
            "type": "text",
            "text": response_text
        }]
    }),
)
```

- [ ] **Step 1C.7: Build to verify both call sites compile**

Run: `cargo build -p spur-mcp`
Expected: Clean build.

- [ ] **Step 1C.8: Run the full crate test suite**

Run: `cargo test -p spur-mcp`
Expected: All tests pass (including the existing `validate_plan` tests, which are unaffected — synthetic edges are appended to `depends_on`, never replace).

- [ ] **Step 1C.9: Commit**

```bash
git add crates/spur-mcp/src/plan/mod.rs crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): submit_plan auto-serializes sibling tasks with overlapping context_files (br-xh7 task 1C)"
```

### Sub-task 1D: end-to-end regression test for the br-77i scenario

- [ ] **Step 1D.1: Write the regression test**

Append to `crates/spur-mcp/src/plan/mod.rs` `mod tests`:

```rust
#[test]
fn br_77i_diamond_dag_orchestrator_rs_serializes_three_siblings() {
    // Reproduces the bd-14cq Wave-1+Wave-2 DAG that triggered br-77i:
    //   orch-server-field (root) →
    //     orch-shutdown-retire | orch-flush-on-exit | orch-inject-mcp-url →
    //       e2e-integration-test (sink)
    // All three Wave-2 tasks touched orchestrator.rs.
    let mut tasks = vec![
        task_with_files("orch-server-field", &[], &["crates/spur-core/src/orchestrator.rs"]),
        task_with_files(
            "orch-shutdown-retire",
            &["orch-server-field"],
            &["crates/spur-core/src/orchestrator.rs"],
        ),
        task_with_files(
            "orch-flush-on-exit",
            &["orch-server-field"],
            &["crates/spur-core/src/orchestrator.rs"],
        ),
        task_with_files(
            "orch-inject-mcp-url",
            &["orch-server-field"],
            &["crates/spur-core/src/orchestrator.rs"],
        ),
        task_with_files(
            "e2e-integration-test",
            &["orch-shutdown-retire", "orch-flush-on-exit", "orch-inject-mcp-url"],
            &[],
        ),
    ];
    let overlaps = super::submit_plan_normalize_tasks(&mut tasks).unwrap();

    // Three pairs among Wave-2 siblings: (flush, inject), (flush, shutdown),
    // (inject, shutdown). orch-server-field overlaps with all three Wave-2
    // tasks but is their declared parent → not flagged.
    assert_eq!(overlaps.len(), 3, "got {:?}", overlaps);

    // Verify Wave-2 tasks are now linearly ordered: lex-min depends on
    // nothing extra; lex-mid depends on lex-min; lex-max depends on the
    // other two.
    let flush = tasks.iter().find(|t| t.task_id == "orch-flush-on-exit").unwrap();
    let inject = tasks.iter().find(|t| t.task_id == "orch-inject-mcp-url").unwrap();
    let shutdown = tasks.iter().find(|t| t.task_id == "orch-shutdown-retire").unwrap();
    // Lex order: orch-flush-on-exit < orch-inject-mcp-url < orch-shutdown-retire.
    assert_eq!(flush.depends_on, vec!["orch-server-field"]);
    assert!(inject.depends_on.contains(&"orch-flush-on-exit".to_string()));
    assert!(shutdown.depends_on.contains(&"orch-flush-on-exit".to_string()));
    assert!(shutdown.depends_on.contains(&"orch-inject-mcp-url".to_string()));
}
```

- [ ] **Step 1D.2: Run regression test**

Run: `cargo test -p spur-mcp --lib br_77i_diamond_dag_orchestrator_rs_serializes_three_siblings`
Expected: Pass.

- [ ] **Step 1D.3: Commit**

```bash
git add crates/spur-mcp/src/plan/mod.rs
git commit -m "test(spur-mcp): regression test for br-77i diamond-DAG sibling-overlap (br-xh7 task 1D)"
```

---

## Task 2: predispatch-dry-run — pre-dispatch overlay simulation in the reconciler

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:5753-5917` (extract `preview_task_base_impl` body into a callable helper).
- Create: `crates/spur-mcp/src/plan/preview.rs` (new module owning the helper; keeps `server.rs` from growing).
- Modify: `crates/spur-mcp/src/plan/mod.rs:1-22` (add `pub mod preview;`).
- Modify: `crates/spur-mcp/src/server.rs:5919-5948` (`handle_preview_task_base` calls extracted helper).
- Modify: `crates/spur-mcp/src/plan/reconciler.rs:739-915` (`tick_once` invokes helper after base-spec compute, before persist).
- Test: `crates/spur-mcp/src/plan/preview.rs` (in-file `mod tests` for helper).
- Test: `crates/spur-mcp/src/plan/reconciler.rs` (existing test module — add reconciler-level test).

### Sub-task 2A: extract `preview_task_base_impl` into `plan::preview`

- [ ] **Step 2A.1: Create the new module file**

Create `crates/spur-mcp/src/plan/preview.rs`:

```rust
//! Overlay dry-run helper. Computes the predicted post-overlay HEAD for a plan
//! task by creating a throwaway worktree, applying the approved-dep overlay
//! closure, and either capturing the HEAD oid or surfacing an `OverlayConflict`.
//!
//! Used by:
//!   - the `preview_task_base` MCP tool (read-only brain inspection), and
//!   - the reconciler's pre-dispatch check (transitions to BlockedOnSetupConflict
//!     before spawning a worker on a predicted conflict — see br-xh7 task 2).

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Mutex;

use spur_worktree::{WorktreeError, WorktreeManager};

use crate::tool_schemas::{PreviewConflict, PreviewTaskBaseOutput};
use crate::tools::OverlayCommit;

/// Compute the overlay preview for `task_id` in `plan_state`. The throwaway
/// worktree and branch are always cleaned up before returning.
///
/// `repo_root` is the workspace root used to materialize the preview worktree
/// under `.spur/worktrees/preview/<uuid>`.
///
/// Returns:
///   - `Ok(PreviewTaskBaseOutput)` on success — `predicted_base_oid` is `Some`
///     when overlays apply cleanly, `None` plus `conflict` populated on
///     `OverlayConflict`.
///   - `Err(_)` for any other error (git invocation failure, missing dep oid,
///     worktree creation failure).
pub async fn preview_overlay(
    plan_state: &Arc<Mutex<crate::plan::PlanState>>,
    plan_id: &str,
    task_id: &str,
    repo_root: &Path,
) -> anyhow::Result<PreviewTaskBaseOutput> {
    let (base_ref, overlay_sources) = {
        let state = plan_state.lock().await;
        if !state.tasks.iter().any(|e| e.spec.task_id == task_id) {
            anyhow::bail!("Unknown task_id '{task_id}' in plan '{plan_id}'");
        }
        let overlay_sources = state
            .approved_dep_closure(task_id)
            .into_iter()
            .filter_map(|dep| {
                let dep_id = dep.spec.task_id.as_str();
                let base_oid = dep.dispatched_base_oid.clone()?;
                let worker_branch = dep.worker_branch.as_ref().cloned()?;
                tracing::trace!(
                    plan_id,
                    task_id,
                    dep_task_id = dep_id,
                    "preview_overlay: queued overlay source"
                );
                Some((dep.spec.task_id.clone(), base_oid, worker_branch))
            })
            .collect::<Vec<_>>();
        let base_ref = state
            .base_snapshot_branch
            .clone()
            .or_else(|| state.base_snapshot_oid.clone())
            .unwrap_or_else(|| "HEAD".to_string());
        (base_ref, overlay_sources)
    };

    let mut overlays = Vec::with_capacity(overlay_sources.len());
    for (source_task_id, base_oid, worker_branch) in overlay_sources {
        let tip_oid = crate::server::run_git_capture(
            repo_root,
            None,
            &["rev-parse", "--verify", worker_branch.as_str()],
        )
        .await
        .with_context(|| {
            format!(
                "failed to resolve worker branch '{worker_branch}' for dep {source_task_id}"
            )
        })?;
        overlays.push(OverlayCommit {
            source_task_id,
            base_oid,
            tip_oid,
        });
    }

    let preview_id = uuid::Uuid::new_v4().simple().to_string();
    let throwaway_path = repo_root.join(".spur/worktrees/preview").join(&preview_id);
    if let Some(parent) = throwaway_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("failed to create preview worktree parent {}", parent.display())
        })?;
    }
    let throwaway_branch = format!("spur/preview-{preview_id}");
    let manager = WorktreeManager::new(repo_root.to_path_buf());

    if let Err(error) = manager
        .create_worktree_at(&throwaway_path, &throwaway_branch, &base_ref)
        .await
    {
        let _ = manager.remove_worktree_at(&throwaway_path).await;
        let _ = manager.delete_branch(&throwaway_branch).await;
        return Err(error.into());
    }

    let overlay_args = overlays
        .iter()
        .map(|o| (o.source_task_id.clone(), o.base_oid.clone(), o.tip_oid.clone()))
        .collect::<Vec<_>>();

    let result = match manager.apply_overlays(&throwaway_path, &overlay_args).await {
        Ok(()) => match manager.resolve_head(&throwaway_path).await {
            Ok(head) => Ok(PreviewTaskBaseOutput {
                overlays,
                predicted_base_oid: Some(head),
                conflict: None,
            }),
            Err(error) => Err(anyhow::Error::from(error).context("failed to resolve preview HEAD")),
        },
        Err(WorktreeError::OverlayConflict { source_task_id, files }) => Ok(PreviewTaskBaseOutput {
            overlays,
            predicted_base_oid: None,
            conflict: Some(PreviewConflict { dep_task_id: source_task_id, files }),
        }),
        Err(other) => Err(anyhow::anyhow!("preview overlay failed: {other}")),
    };

    if let Err(e) = manager.remove_worktree_at(&throwaway_path).await {
        tracing::warn!(plan_id, task_id, path = %throwaway_path.display(), "preview cleanup: remove_worktree_at failed: {e}");
    }
    if let Err(e) = manager.delete_branch(&throwaway_branch).await {
        tracing::warn!(plan_id, task_id, branch = %throwaway_branch, "preview cleanup: delete_branch failed: {e}");
    }
    result
}
```

- [ ] **Step 2A.2: Register the module**

In `crates/spur-mcp/src/plan/mod.rs:8-22`, add `pub mod preview;` to the module declarations (alphabetical: between `outcomes` and `projector`).

- [ ] **Step 2A.3: Re-route `preview_task_base_impl` through the helper**

In `crates/spur-mcp/src/server.rs:5753-5917`, replace the entire body of `preview_task_base_impl` with:

```rust
async fn preview_task_base_impl(
    &self,
    input: crate::tool_schemas::PreviewTaskBaseInput,
) -> anyhow::Result<crate::tool_schemas::PreviewTaskBaseOutput> {
    let repo_root = self
        .repo_root
        .as_ref()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Repository root not configured"))?;
    let plan_arc = self
        .load_or_project_plan(&input.plan_id)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    crate::plan::preview::preview_overlay(&plan_arc, &input.plan_id, &input.task_id, &repo_root)
        .await
}
```

If `run_git_capture` is `pub(crate)` or private, change its visibility to `pub(crate)` so `plan::preview` can call it. If it lives outside `crate::server`, adjust the import path in `preview.rs` accordingly.

- [ ] **Step 2A.4: Build to verify**

Run: `cargo build -p spur-mcp`
Expected: Clean build. If `run_git_capture` visibility error, mark it `pub(crate)` and rebuild.

- [ ] **Step 2A.5: Run existing preview_task_base tests**

Run: `cargo test -p spur-mcp preview_task_base`
Expected: All existing preview_task_base tests still pass — refactor is behavior-preserving.

- [ ] **Step 2A.6: Commit refactor**

```bash
git add crates/spur-mcp/src/plan/preview.rs crates/spur-mcp/src/plan/mod.rs crates/spur-mcp/src/server.rs
git commit -m "refactor(spur-mcp): extract preview_overlay helper from MCP handler (br-xh7 task 2A)"
```

### Sub-task 2B: pre-dispatch dry-run hook in `tick_once`

- [ ] **Step 2B.1: Write the failing reconciler test**

Locate the existing reconciler test module in `crates/spur-mcp/src/plan/reconciler.rs`. Add this test (placement: alongside other `tick_once` tests):

```rust
#[tokio::test]
async fn tick_once_predicts_overlay_conflict_and_blocks_without_dispatch() {
    // GIVEN a plan where the next Ready task's overlay closure has a
    // simulated conflict, WHEN tick_once runs, THEN the task transitions to
    // BlockedOnSetupConflict and no DelegationRequest is sent.
    //
    // Use a stub `preview_overlay` that returns Conflict; assert that the
    // dispatch channel never receives a request and the task status is
    // updated accordingly.
    //
    // Implementation note: this test will use the existing reconciler test
    // harness pattern (search for `fn build_test_reconciler` or similar in
    // this file). If no harness exists with a way to inject a preview stub,
    // add a `predispatch_preview` strategy field on `Reconciler` (with a
    // default of "real") and override it to "always_conflict" for the test.

    // ... follow the established harness pattern; pseudocode:
    // let (reconciler, dispatch_rx) = build_test_reconciler_with_preview(
    //     PreviewStrategy::AlwaysConflict { dep_task_id: "X".into(), files: vec!["a.rs".into()] },
    // );
    // submit a 2-task plan: X (approved), Y (depends_on X, ready).
    // reconciler.tick_once().await.unwrap();
    // assert!(dispatch_rx.try_recv().is_err()); // no dispatch
    // let plan_state = reconciler.snapshot(plan_id).await;
    // let y = plan_state.task("Y");
    // assert!(matches!(y.status, PlanTaskStatus::BlockedOnSetupConflict { .. }));
}
```

If the existing test harness does not support a preview stub, the implementation step below adds an opt-in `predispatch_preview_strategy` field to the reconciler config; replace the pseudocode with a concrete invocation.

- [ ] **Step 2B.2: Run test to verify FAIL**

Run: `cargo test -p spur-mcp tick_once_predicts_overlay_conflict_and_blocks_without_dispatch`
Expected: Compile or runtime fail — preview stub plumbing not in place; or the dispatch channel receives a request.

- [ ] **Step 2B.3: Add a preview-strategy hook to the reconciler**

In `crates/spur-mcp/src/plan/reconciler.rs`, find the `Reconciler` config/builder. Add:

```rust
/// Strategy for the pre-dispatch overlay dry-run. Production uses `Real`;
/// tests inject `AlwaysConflict { dep_task_id, files }` or `AlwaysClean`.
#[derive(Debug, Clone)]
pub enum PreviewStrategy {
    Real,
    AlwaysClean,
    AlwaysConflict { dep_task_id: String, files: Vec<String> },
}

impl Default for PreviewStrategy {
    fn default() -> Self {
        Self::Real
    }
}
```

Wire it into the existing `ReconcilerConfig` (or equivalent struct in this file) as a new field `pub predispatch_preview: PreviewStrategy`, defaulted to `Real`.

- [ ] **Step 2B.4: Add the pre-dispatch check in `tick_once`**

In `crates/spur-mcp/src/plan/reconciler.rs`, between line 849 (the closing `}` of the `match plan_dispatch_base_spec(...)` Ok arm — `Ok(base_spec) => base_spec`) and line 850 (`if let Err(error) = crate::plan::persist_dispatch_intent(...)`), insert:

```rust
// br-xh7 task 2: pre-dispatch overlay dry-run. If the closure of approved
// dep overlays predicts a conflict, transition the task to
// BlockedOnSetupConflict without spawning a worker.
let preview_outcome = match &self.config.predispatch_preview {
    crate::plan::reconciler::PreviewStrategy::AlwaysClean => None,
    crate::plan::reconciler::PreviewStrategy::AlwaysConflict { dep_task_id, files } => {
        Some(crate::tool_schemas::PreviewConflict {
            dep_task_id: dep_task_id.clone(),
            files: files.clone(),
        })
    }
    crate::plan::reconciler::PreviewStrategy::Real => {
        let plan_arc = match self.active_plan_handle(plan_id).await {
            Some(arc) => arc,
            None => {
                tracing::warn!(%plan_id, task_id = %task.spec.task_id, "predispatch preview: no active plan handle; skipping check");
                None
            }
        };
        match plan_arc {
            Some(arc) => match crate::plan::preview::preview_overlay(
                &arc,
                plan_id,
                &task.spec.task_id,
                &self.config.repo_root,
            )
            .await
            {
                Ok(out) => out.conflict,
                Err(e) => {
                    tracing::warn!(%plan_id, task_id = %task.spec.task_id, "predispatch preview: helper errored, falling through to live dispatch: {e}");
                    None
                }
            },
            None => None,
        }
    }
};
if let Some(conflict) = preview_outcome {
    tracing::info!(
        %plan_id,
        task_id = %task.spec.task_id,
        dep_task_id = %conflict.dep_task_id,
        files = ?conflict.files,
        "predispatch preview: overlay conflict predicted; blocking without worker spawn"
    );
    self.transition_to_blocked_on_setup_conflict(
        plan_id,
        &task.spec.task_id,
        &conflict.dep_task_id,
        &conflict.files,
    )
    .await;
    self.record_skipped(
        Some(plan_id),
        &task.spec.task_id,
        SkipReason::PredispatchOverlayConflict {
            dep_task_id: conflict.dep_task_id.clone(),
            files: conflict.files.clone(),
        },
    )
    .await;
    continue;
}
```

If `active_plan_handle`, `transition_to_blocked_on_setup_conflict`, or `SkipReason::PredispatchOverlayConflict` do not yet exist, add them:

- `active_plan_handle(plan_id) -> Option<Arc<Mutex<PlanState>>>`: lookup in the existing active-plans registry (mirror what `load_or_project_plan` does in `server.rs`). If you cannot easily reach the registry from the reconciler, fall back to `project_plan_from_beads(plan_id)` and wrap in `Arc::new(Mutex::new(...))`.
- `transition_to_blocked_on_setup_conflict(plan_id, task_id, dep_task_id, files)`: mirror the existing in-flight transition at `plan/mod.rs:2420-2431` — set `entry.status = PlanTaskStatus::BlockedOnSetupConflict { dep_task_id, files }` in the active plan state and emit the corresponding audit/projector update used elsewhere.
- `SkipReason::PredispatchOverlayConflict { dep_task_id, files }`: add as a new variant on the existing `SkipReason` enum in this file.

- [ ] **Step 2B.5: Update the test in 2B.1 to use real plumbing**

Replace the pseudocode placeholder in the test with concrete invocations now that `PreviewStrategy::AlwaysConflict` and `transition_to_blocked_on_setup_conflict` exist.

- [ ] **Step 2B.6: Run test to verify PASS**

Run: `cargo test -p spur-mcp tick_once_predicts_overlay_conflict_and_blocks_without_dispatch`
Expected: Pass.

- [ ] **Step 2B.7: Add the clean-path test**

Add to the same test module:

```rust
#[tokio::test]
async fn tick_once_with_clean_preview_dispatches_normally() {
    // PreviewStrategy::AlwaysClean — verify that a Ready task still
    // dispatches when the dry-run reports no conflict.
    // ... use the same harness with PreviewStrategy::AlwaysClean.
    // assert dispatch_rx.recv() returns a DelegationRequest.
}
```

- [ ] **Step 2B.8: Run clean-path test**

Run: `cargo test -p spur-mcp tick_once_with_clean_preview_dispatches_normally`
Expected: Pass.

- [ ] **Step 2B.9: Run the full crate test suite**

Run: `cargo test -p spur-mcp`
Expected: All tests pass.

- [ ] **Step 2B.10: Commit**

```bash
git add crates/spur-mcp/src/plan/reconciler.rs
git commit -m "feat(spur-mcp): pre-dispatch overlay dry-run blocks on predicted conflict (br-xh7 task 2B)"
```

---

## Task 3: truncate-and-restart-tool — `plan_truncate_and_restart` MCP tool

**Files:**
- Modify: `crates/spur-mcp/src/tool_schemas.rs:60-84` (add input/output types).
- Modify: `crates/spur-mcp/src/tools.rs:931-1090` (add tool def, register in `tools_list`).
- Modify: `crates/spur-mcp/src/server.rs:2984-2989` (add dispatch arm).
- Modify: `crates/spur-mcp/src/server.rs:5919` (add handler near `handle_preview_task_base`).
- Create: `crates/spur-mcp/src/plan/staging.rs` (new module with the staging-branch builder + new-plan generator).
- Modify: `crates/spur-mcp/src/plan/mod.rs:8-22` (register new module).
- Test: `crates/spur-mcp/src/plan/staging.rs` (in-file `mod tests`).

### Sub-task 3A: input/output types and tool registration

- [ ] **Step 3A.1: Add input/output types**

Append to `crates/spur-mcp/src/tool_schemas.rs:84` (after `PreviewConflict`):

```rust
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlanTruncateAndRestartInput {
    pub plan_id: String,
    /// Task that is currently blocked. All non-terminal tasks (including this
    /// one) will be marked Superseded in the original plan and re-dispatched
    /// in a new plan rooted at the staging branch.
    pub blocked_task_id: String,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct PlanTruncateAndRestartOutput {
    /// New branch containing approved tips cherry-picked in DAG order.
    pub staging_branch: String,
    /// Original-plan task IDs that were marked Superseded.
    pub superseded_task_ids: Vec<String>,
    /// New plan ID rooted at `staging_branch`.
    pub new_plan_id: String,
    /// If cherry-pick collided, the dep that conflicted and the files; in
    /// this case `staging_branch` and `new_plan_id` are still populated with
    /// best-effort partial values, but the brain must resolve manually.
    pub conflict: Option<StagingConflict>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct StagingConflict {
    pub dep_task_id: String,
    pub files: Vec<String>,
}
```

- [ ] **Step 3A.2: Add tool definition**

In `crates/spur-mcp/src/tools.rs` after `preview_task_base_def` (line 937):

```rust
fn plan_truncate_and_restart_def() -> ToolDefinition {
    ToolDefinition {
        name: "plan_truncate_and_restart".into(),
        description: "Recovery tool for plans blocked by overlay conflicts. \
            Cherry-picks approved task tips in DAG order onto a fresh \
            `spur/plan-staging/{plan_id}` branch, marks remaining tasks \
            Superseded in the original plan, and submits a new plan whose \
            tasks dispatch against the staging branch. Use after \
            `BlockedOnSetupConflict` when the conflict is across approved \
            siblings (i.e. cannot be unwound by re-dispatching a single \
            upstream task)."
            .into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::PlanTruncateAndRestartInput>(),
    }
}
```

- [ ] **Step 3A.3: Register in `tools_list`**

In `crates/spur-mcp/src/tools.rs` `tools_list()` (around line 1085 where `preview_task_base_def()` is currently included), add `plan_truncate_and_restart_def(),` immediately after `preview_task_base_def(),`.

- [ ] **Step 3A.4: Add dispatch arm**

In `crates/spur-mcp/src/server.rs` `tools/call` match (around line 2989, where `"preview_task_base"` is currently handled), add:

```rust
"plan_truncate_and_restart" => self.handle_plan_truncate_and_restart(id, arguments).await,
```

- [ ] **Step 3A.5: Build to verify wiring**

Run: `cargo build -p spur-mcp`
Expected: Build fails with "no method named `handle_plan_truncate_and_restart`" — that's expected; we wire the handler in 3B.

- [ ] **Step 3A.6: Stub the handler so the build passes**

In `crates/spur-mcp/src/server.rs` after `handle_preview_task_base` (around line 5948), add:

```rust
async fn handle_plan_truncate_and_restart(&self, id: Value, args: Value) -> JsonRpcResponse {
    let _input: crate::tool_schemas::PlanTruncateAndRestartInput =
        match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => return JsonRpcResponse::invalid_params(id, error.to_string()),
        };
    JsonRpcResponse::internal_error(id, "plan_truncate_and_restart: not yet implemented".to_string())
}
```

- [ ] **Step 3A.7: Build**

Run: `cargo build -p spur-mcp`
Expected: Clean build.

- [ ] **Step 3A.8: Commit wiring scaffold**

```bash
git add crates/spur-mcp/src/tool_schemas.rs crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): scaffold plan_truncate_and_restart MCP tool (br-xh7 task 3A)"
```

### Sub-task 3B: staging-branch builder

- [ ] **Step 3B.1: Create the staging module skeleton**

Create `crates/spur-mcp/src/plan/staging.rs`:

```rust
//! Plan-recovery staging: build a `spur/plan-staging/{plan_id}` branch by
//! cherry-picking approved task tips in DAG order, supersede remaining tasks
//! in the original plan, and shape a new plan rooted at the staging branch.
//!
//! See br-xh7 task 3 for context.

use std::path::Path;

use spur_worktree::{WorktreeError, WorktreeManager};

use crate::plan::{PlanState, PlanTask, PlanTaskStatus};

/// Result of attempting to build the staging branch.
#[derive(Debug)]
pub struct StagingBuild {
    pub branch: String,
    pub merged_task_ids: Vec<String>,
    pub conflict: Option<crate::tool_schemas::StagingConflict>,
}

/// Walk approved tasks in DAG order; cherry-pick each task's
/// `[dispatched_base_oid..worker_branch tip]` onto a fresh staging branch
/// rooted at `plan.base_snapshot_branch` (or `base_snapshot_oid` / `HEAD`).
///
/// On the first conflict, leaves the staging branch in a clean state at the
/// last successfully-applied tip and returns `Some(StagingConflict)` in the
/// result so the caller can route to brain review.
pub async fn build_staging_branch(
    plan: &PlanState,
    repo_root: &Path,
) -> anyhow::Result<StagingBuild> {
    let branch_name = format!("spur/plan-staging/{}", plan.plan_id);
    let base_ref = plan
        .base_snapshot_branch
        .clone()
        .or_else(|| plan.base_snapshot_oid.clone())
        .unwrap_or_else(|| "HEAD".to_string());

    // Collect approved tips in DAG order. We rely on PlanState's existing
    // topological accessor; if none, fall back to a Kahn walk over `tasks`.
    let approved_in_topo_order: Vec<&crate::plan::PlanTaskEntry> = plan
        .topo_ordered_tasks()
        .into_iter()
        .filter(|e| matches!(e.status, PlanTaskStatus::Approved { .. }))
        .filter(|e| e.worker_branch.is_some() && e.dispatched_base_oid.is_some())
        .collect();

    let manager = WorktreeManager::new(repo_root.to_path_buf());

    // Use a throwaway worktree so the brain's current checkout is not perturbed.
    let staging_id = uuid::Uuid::new_v4().simple().to_string();
    let staging_path = repo_root.join(".spur/worktrees/staging").join(&staging_id);
    if let Some(parent) = staging_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    manager
        .create_worktree_at(&staging_path, &branch_name, &base_ref)
        .await?;

    let mut merged = Vec::new();
    let mut conflict = None;
    for entry in approved_in_topo_order {
        let base_oid = entry.dispatched_base_oid.as_ref().unwrap().clone();
        let tip_branch = entry.worker_branch.as_ref().unwrap().clone();
        let tip_oid = crate::server::run_git_capture(
            repo_root,
            None,
            &["rev-parse", "--verify", tip_branch.as_str()],
        )
        .await?;
        let overlay = vec![(entry.spec.task_id.clone(), base_oid, tip_oid)];
        match manager.apply_overlays(&staging_path, &overlay).await {
            Ok(()) => merged.push(entry.spec.task_id.clone()),
            Err(WorktreeError::OverlayConflict { source_task_id, files }) => {
                conflict = Some(crate::tool_schemas::StagingConflict {
                    dep_task_id: source_task_id,
                    files,
                });
                break;
            }
            Err(other) => {
                let _ = manager.remove_worktree_at(&staging_path).await;
                let _ = manager.delete_branch(&branch_name).await;
                return Err(anyhow::anyhow!("staging cherry-pick failed: {other}"));
            }
        }
    }

    // Tear down the staging worktree but KEEP the branch — the new plan
    // will use it as base.
    let _ = manager.remove_worktree_at(&staging_path).await;

    Ok(StagingBuild {
        branch: branch_name,
        merged_task_ids: merged,
        conflict,
    })
}

/// Given the original plan and the staging build outcome, produce the task
/// list for the new plan. Tasks already approved are excluded; remaining
/// tasks (Pending, Ready, Dispatched, AwaitingReview, BlockedOnSetupConflict)
/// are carried forward with a freshly minted issue_id-less spec rooted at
/// the staging branch's PlanTask shape.
///
/// Returns `(new_tasks, superseded_task_ids)`.
pub fn shape_new_plan(plan: &PlanState) -> (Vec<PlanTask>, Vec<String>) {
    let mut new_tasks = Vec::new();
    let mut superseded = Vec::new();
    for entry in &plan.tasks {
        match entry.status {
            PlanTaskStatus::Approved { .. }
            | PlanTaskStatus::Failed { .. }
            | PlanTaskStatus::Cancelled { .. }
            | PlanTaskStatus::Superseded { .. } => continue,
            _ => {
                superseded.push(entry.spec.task_id.clone());
                new_tasks.push(entry.spec.clone());
            }
        }
    }
    (new_tasks, superseded)
}
```

- [ ] **Step 3B.2: Register the module**

In `crates/spur-mcp/src/plan/mod.rs:8-22` add `pub mod staging;` (alphabetical).

- [ ] **Step 3B.3: Verify `topo_ordered_tasks` exists on `PlanState`**

Run: `cargo build -p spur-mcp`
If the build fails because `topo_ordered_tasks` is not defined, add it to `PlanState` near the existing accessors (`approved_dep_closure`, etc.) in `crates/spur-mcp/src/plan/mod.rs`:

```rust
impl PlanState {
    /// Returns plan tasks in topological (dependency) order. Equal-rank
    /// tasks are ordered by task_id for determinism.
    pub fn topo_ordered_tasks(&self) -> Vec<&PlanTaskEntry> {
        let id_to_idx: std::collections::HashMap<&str, usize> = self
            .tasks
            .iter()
            .enumerate()
            .map(|(i, e)| (e.spec.task_id.as_str(), i))
            .collect();
        let mut in_degree: Vec<usize> = self
            .tasks
            .iter()
            .map(|e| e.spec.depends_on.len())
            .collect();
        let mut queue: std::collections::BTreeSet<&str> = in_degree
            .iter()
            .enumerate()
            .filter(|(_, &d)| d == 0)
            .map(|(i, _)| self.tasks[i].spec.task_id.as_str())
            .collect();
        let mut out = Vec::new();
        while let Some(&id) = queue.iter().next() {
            queue.remove(id);
            let idx = id_to_idx[id];
            out.push(&self.tasks[idx]);
            for entry in &self.tasks {
                if entry.spec.depends_on.iter().any(|d| d == id) {
                    let i = id_to_idx[entry.spec.task_id.as_str()];
                    in_degree[i] -= 1;
                    if in_degree[i] == 0 {
                        queue.insert(entry.spec.task_id.as_str());
                    }
                }
            }
        }
        out
    }
}
```

- [ ] **Step 3B.4: Write unit tests for `shape_new_plan` and `build_staging_branch`**

Append to `crates/spur-mcp/src/plan/staging.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{PlanTaskEntry, PlanTaskStatus};
    use spur_acp::{BrainSessionId, SessionId};

    fn entry_for(task_id: &str, deps: &[&str], status: PlanTaskStatus) -> PlanTaskEntry {
        PlanTaskEntry {
            spec: PlanTask {
                task_id: task_id.into(),
                agent: "test-agent".into(),
                task: "test".into(),
                depends_on: deps.iter().map(|s| s.to_string()).collect(),
                issue_id: None,
                context_files: vec![],
            },
            status,
            result: None,
            worker_branch: None,
            attempt: 1,
            history: vec![],
            last_delegation_id: None,
            dispatched_base_oid: None,
        }
    }

    fn plan_with(entries: Vec<PlanTaskEntry>) -> PlanState {
        PlanState {
            plan_id: "test-plan".into(),
            tasks: entries,
            brain_session_id: BrainSessionId::from(SessionId("test".into())),
            base_snapshot_branch: Some("main".into()),
            base_snapshot_oid: None,
            merge_state: crate::plan::PlanMergeState::NotStarted,
            epic_id: None,
        }
    }

    #[test]
    fn shape_new_plan_excludes_approved_includes_pending_and_blocked() {
        let plan = plan_with(vec![
            entry_for("A", &[], PlanTaskStatus::Approved { summary: None }),
            entry_for("B", &["A"], PlanTaskStatus::BlockedOnSetupConflict {
                dep_task_id: "C".into(),
                files: vec!["x.rs".into()],
            }),
            entry_for("C", &["A"], PlanTaskStatus::Approved { summary: None }),
            entry_for("D", &["B"], PlanTaskStatus::Pending),
        ]);
        let (new_tasks, superseded) = shape_new_plan(&plan);
        let new_ids: Vec<&str> = new_tasks.iter().map(|t| t.task_id.as_str()).collect();
        assert_eq!(new_ids, vec!["B", "D"]);
        assert_eq!(superseded, vec!["B".to_string(), "D".to_string()]);
    }
}
```

- [ ] **Step 3B.5: Run unit tests**

Run: `cargo test -p spur-mcp --lib staging::tests`
Expected: Pass.

- [ ] **Step 3B.6: Commit**

```bash
git add crates/spur-mcp/src/plan/staging.rs crates/spur-mcp/src/plan/mod.rs
git commit -m "feat(spur-mcp): add plan staging-branch builder + shape_new_plan (br-xh7 task 3B)"
```

### Sub-task 3C: handler implementation

- [ ] **Step 3C.1: Replace the stub handler with real implementation**

In `crates/spur-mcp/src/server.rs` replace the `handle_plan_truncate_and_restart` stub from 3A.6 with:

```rust
async fn handle_plan_truncate_and_restart(&self, id: Value, args: Value) -> JsonRpcResponse {
    let input: crate::tool_schemas::PlanTruncateAndRestartInput =
        match serde_json::from_value(args) {
            Ok(input) => input,
            Err(error) => return JsonRpcResponse::invalid_params(id, error.to_string()),
        };

    let repo_root = match self.repo_root.as_ref() {
        Some(r) => r.clone(),
        None => return JsonRpcResponse::internal_error(id, "Repository root not configured".into()),
    };

    let plan_arc = match self.load_or_project_plan(&input.plan_id).await {
        Ok(arc) => arc,
        Err(e) => return JsonRpcResponse::invalid_params(id, e),
    };

    // Snapshot the plan so we can build the staging branch and shape the new
    // plan without holding the lock across `await`s on git.
    let (snapshot, brain_session_id) = {
        let state = plan_arc.lock().await;
        (
            crate::plan::PlanState {
                plan_id: state.plan_id.clone(),
                tasks: state.tasks.clone(),
                brain_session_id: state.brain_session_id.clone(),
                base_snapshot_branch: state.base_snapshot_branch.clone(),
                base_snapshot_oid: state.base_snapshot_oid.clone(),
                merge_state: state.merge_state.clone(),
                epic_id: state.epic_id.clone(),
            },
            state.brain_session_id.clone(),
        )
    };

    // Sanity: the blocked task must exist.
    if !snapshot.tasks.iter().any(|e| e.spec.task_id == input.blocked_task_id) {
        return JsonRpcResponse::invalid_params(
            id,
            format!("Unknown blocked_task_id '{}' in plan '{}'", input.blocked_task_id, input.plan_id),
        );
    }

    let build = match crate::plan::staging::build_staging_branch(&snapshot, &repo_root).await {
        Ok(b) => b,
        Err(e) => return JsonRpcResponse::internal_error(id, e.to_string()),
    };

    let (new_tasks, superseded_task_ids) = crate::plan::staging::shape_new_plan(&snapshot);

    // Mark superseded in the original plan.
    {
        let mut state = plan_arc.lock().await;
        let mutation_id = uuid::Uuid::new_v4().to_string();
        for entry in state.tasks.iter_mut() {
            if superseded_task_ids.contains(&entry.spec.task_id) {
                entry.status = crate::plan::PlanTaskStatus::Superseded {
                    mutation_id: mutation_id.clone(),
                    by: vec![],
                };
            }
        }
    }

    // Submit a new plan with BaseSpec::Branch(build.branch.clone()) per task.
    // The cleanest path: invoke the same submit_plan internals used by the
    // MCP tool. If a `submit_plan_internal` helper does not exist, build the
    // PlanState directly here, mirroring server.rs:5008-5066.
    let new_plan_id = match self
        .submit_plan_internal(
            new_tasks,
            Some(build.branch.clone()),
            brain_session_id,
        )
        .await
    {
        Ok(plan_id) => plan_id,
        Err(e) => return JsonRpcResponse::internal_error(id, format!("new plan submission failed: {e}")),
    };

    let output = crate::tool_schemas::PlanTruncateAndRestartOutput {
        staging_branch: build.branch,
        superseded_task_ids,
        new_plan_id,
        conflict: build.conflict,
    };

    match serde_json::to_string_pretty(&output) {
        Ok(text) => JsonRpcResponse::success(
            id,
            json!({ "content": [{ "type": "text", "text": text }] }),
        ),
        Err(error) => JsonRpcResponse::internal_error(
            id,
            format!("failed to serialize plan_truncate_and_restart response: {error}"),
        ),
    }
}
```

- [ ] **Step 3C.2: Add `submit_plan_internal` helper**

In `crates/spur-mcp/src/server.rs` near the existing `submit_plan` handler, factor out the persistence logic into:

```rust
/// Internal entry point used by `handle_submit_plan` and other tools (e.g.
/// `handle_plan_truncate_and_restart`) that need to submit a fresh plan
/// without going through JSON-RPC argument parsing.
async fn submit_plan_internal(
    &self,
    mut tasks: Vec<crate::plan::PlanTask>,
    base_branch_override: Option<String>,
    brain_session_id: BrainSessionId,
) -> Result<String, String> {
    let _auto_serialized = crate::plan::submit_plan_normalize_tasks(&mut tasks)?;
    let plan_id = uuid::Uuid::new_v4().to_string();
    let entries: Vec<crate::plan::PlanTaskEntry> = build_entries_with_task_map(tasks, None);
    // Construct the base snapshot. When `base_branch_override` is provided
    // (the truncate-and-restart case), use it directly without invoking
    // `snapshot_plan_base`. Otherwise call `snapshot_plan_base` for the
    // default path. NOTE: the snapshot value type is whatever
    // `snapshot_plan_base` returns — at the time of writing this plan, that
    // type's exact name was not verified. The executing engineer should
    // either (a) use the actual snapshot type with `branch: Some(branch),
    // oid: None`, or (b) skip the snapshot type and assign the two
    // PlanState fields directly:
    let (base_snapshot_branch, base_snapshot_oid) = match base_branch_override {
        Some(branch) => (Some(branch), None),
        None => {
            let snap = snapshot_plan_base(self.repo_root.as_ref())
                .await
                .map_err(|e| e.to_string())?;
            (snap.branch, snap.oid)
        }
    };
    let state = crate::plan::PlanState {
        plan_id: plan_id.clone(),
        tasks: entries,
        brain_session_id,
        base_snapshot_branch,
        base_snapshot_oid,
        merge_state: crate::plan::PlanMergeState::NotStarted,
        epic_id: None,
    };
    let state = Arc::new(tokio::sync::Mutex::new(state));
    self.active_plans
        .lock()
        .await
        .insert(plan_id.clone(), Arc::clone(&state));
    self.spawn_ephemeral_plan_runner(state);
    Ok(plan_id)
}
```

If the existing `handle_submit_plan` body has additional logic (epic creation, audit emission), refactor `handle_submit_plan` to call `submit_plan_internal` for the persistence portion and keep epic-specific logic above the call. For br-xh7, `submit_plan_internal` only needs the ephemeral path; epic creation is out of scope for the recovery flow.

- [ ] **Step 3C.3: Build to verify**

Run: `cargo build -p spur-mcp`
Expected: Clean build.

- [ ] **Step 3C.4: Write integration test for the handler**

In `crates/spur-mcp/src/plan/staging.rs` `mod tests`, add:

```rust
#[tokio::test]
async fn handle_plan_truncate_and_restart_happy_path() {
    // Set up a temp git repo with two approved tasks (clean cherry-picks)
    // and one blocked task. Invoke the handler; assert:
    //   1. A `spur/plan-staging/<plan_id>` branch exists with both tips applied.
    //   2. The original plan's blocked + pending tasks are Superseded.
    //   3. A new plan was registered in active_plans.
    //   4. The new plan's base_snapshot_branch matches the staging branch.
    //
    // Use the existing temp-repo helpers in this crate's test suite (search
    // for `fn make_test_repo` or similar).
    // ... follow the established harness pattern.
}

#[tokio::test]
async fn handle_plan_truncate_and_restart_returns_conflict_when_cherry_pick_fails() {
    // Two approved tasks whose tips conflict. Assert the returned `conflict`
    // is Some and the staging branch contains only the first tip.
}
```

- [ ] **Step 3C.5: Run integration tests**

Run: `cargo test -p spur-mcp handle_plan_truncate_and_restart`
Expected: Both tests pass.

- [ ] **Step 3C.6: Run the full crate test suite**

Run: `cargo test -p spur-mcp`
Expected: All tests pass.

- [ ] **Step 3C.7: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/src/plan/staging.rs
git commit -m "feat(spur-mcp): implement plan_truncate_and_restart handler (br-xh7 task 3C)"
```

### Sub-task 3D: docs + workspace lint

- [ ] **Step 3D.1: Run workspace lints**

Run: `cargo clippy -p spur-mcp --all-targets -- -D warnings`
Expected: No warnings. Fix any that surface.

- [ ] **Step 3D.2: Run workspace fmt**

Run: `cargo fmt -p spur-mcp --check`
Expected: No diff. If diff, run `cargo fmt -p spur-mcp` and commit:

```bash
git add -u
git commit -m "style(spur-mcp): apply rustfmt (br-xh7 task 3D)"
```

- [ ] **Step 3D.3: Update AGENTS.md if it lists MCP tools**

Run: `grep -n preview_task_base /Volumes/Projects/spur/AGENTS.md /Volumes/Projects/spur/docs/superpowers/skills/spurpower-spur-way/SKILL.md 2>/dev/null` (or equivalent — look in repo docs).
If found, add `plan_truncate_and_restart` to the same section with a one-line description matching the tool's `description` field.

- [ ] **Step 3D.4: Commit doc update if any**

```bash
git add docs/ AGENTS.md 2>/dev/null
git commit -m "docs: register plan_truncate_and_restart in tool index (br-xh7 task 3D)" || echo "no doc updates needed"
```

---

## Acceptance Criteria

A reviewer can verify the plan is complete by running:

```bash
cargo test -p spur-mcp
cargo clippy -p spur-mcp --all-targets -- -D warnings
```

and confirming:

1. **Task 1 (auto-serialize-siblings):**
   - `find_sibling_overlaps`, `apply_sibling_overlaps`, `submit_plan_normalize_tasks` exist in `crate::plan` with passing tests.
   - `submit_plan` JSON response includes a (possibly empty) `auto_serialized` array.
   - The `br_77i_diamond_dag_orchestrator_rs_serializes_three_siblings` regression test passes.
2. **Task 2 (predispatch-dry-run):**
   - `crate::plan::preview::preview_overlay` exists and is called by both `preview_task_base_impl` and `tick_once`.
   - `tick_once_predicts_overlay_conflict_and_blocks_without_dispatch` passes.
   - `tick_once_with_clean_preview_dispatches_normally` passes.
   - No regression in existing `preview_task_base` tests.
3. **Task 3 (truncate-and-restart-tool):**
   - `plan_truncate_and_restart` is registered in `tools_list()` and dispatched by name in `tools/call`.
   - `crate::plan::staging::build_staging_branch` and `shape_new_plan` exist with passing unit tests.
   - `handle_plan_truncate_and_restart_happy_path` and `handle_plan_truncate_and_restart_returns_conflict_when_cherry_pick_fails` pass.
4. **Cross-cutting:** No clippy warnings; rustfmt clean.

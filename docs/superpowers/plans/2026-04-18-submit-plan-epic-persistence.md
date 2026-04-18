# submit_plan Epic Persistence + Brain Orchestration Guide

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the existing `submit_plan` MCP tool with optional beads-epic persistence and ship a brain-prompt guidance doc that teaches brains when and how to use the submit_plan → review_task pipeline.

**Architecture:** One MCP tool gains three optional fields (`persist_as_epic`, `epic_title`, `epic_body`). When set, the handler composes `PmService::create_issue` calls to build an epic + children + deps subgraph, labels each child with `spur.plan_id=<plan_id>`, and returns `epic_id` + `task_map` alongside the existing `plan_id`. The orchestrator dispatch engine is unchanged. A new markdown guide under `docs/spur/` describes the author-dispatch-review rhythm for brains.

**Tech Stack:** Rust (async/tokio), `serde_json`, `anyhow`. Test harness uses built-in `cargo test`. No new crate dependencies.

**Source spec:** Iceberg-refined proposal (12-round MCTS). Key decisions:
- Reject standalone `ingest_plan` tool (tool-catalog contraction ethos from T1).
- Flag-on-existing-tool (`submit_plan.persist_as_epic`) collapses durable + ephemeral execution into one API.
- Guidance is a markdown doc, not a `.claude/skills/` file (brain sessions have no Skill tool).

---

## File map

| File | Responsibility | Role in plan |
|---|---|---|
| `crates/spur-mcp/src/tools.rs` | `submit_plan_def` schema | Modified by Task 1 |
| `crates/spur-mcp/src/server.rs` | `handle_submit_plan` handler + epic-build helper | Modified by Tasks 3, 5, 6, 7 |
| `crates/spur-mcp/src/plan.rs` | `PlanState` epic correlation field | Modified by Task 4 |
| `crates/spur-mcp/tests/submit_plan_schema.rs` (new) | Schema shape + negative-input tests | Created by Task 2 |
| `crates/spur-mcp/tests/submit_plan_persist.rs` (new) | End-to-end persistence test via fake PmService | Created by Task 8 |
| `docs/spur/brain-orchestration-guide.md` (new) | Brain guidance doc | Created by Task 9 |
| `docs/superpowers/specs/2026-04-18-submit-plan-epic-persistence.md` (new) | Design record | Created by Task 10 |

No changes to `spur-pm` — existing `PmService::create_issue` + `IssueCreate.parent` + `IssueCreate.depends_on` + `IssueCreate.labels` are sufficient. No new PmService method needed.

---

## Dependencies across tasks

- Task 1 (schema) and Task 2 (schema tests) must land together — catalog stays green.
- Task 3 (backend-gate + no-op persist branch) is a prerequisite for Task 5 (epic build) which is a prerequisite for Task 6 (correlation labels).
- Task 4 (PlanState.epic_id field) is independent of the persist logic but consumed by Task 7's response.
- Task 8 (integration test) requires Tasks 3–7 complete.
- Task 9 (guidance doc) is independent of all code tasks; can run in parallel.
- Task 10 (spec commit) is last.

---

## Task 1: Extend `submit_plan_def` schema

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs:576-627` (`submit_plan_def`)

- [ ] **Step 1: Add the three optional fields to the schema**

In `crates/spur-mcp/src/tools.rs::submit_plan_def()`, locate the `json!({ ... })` block. The current shape ends with:

```rust
                "delegation_plan": {
                    "type": "object",
                    "description": "Structured reasoning for the overall plan."
                }
            },
            "required": ["tasks"]
        }),
```

Replace with:

```rust
                "delegation_plan": {
                    "type": "object",
                    "description": "Structured reasoning for the overall plan."
                },
                "persist_as_epic": {
                    "type": "boolean",
                    "description": "When true, mirror the plan into beads as an epic with child issues + dependency edges. Each child is labeled `spur.plan_id=<plan_id>` so review_task(approve) can auto-close the matching beads issue. Requires `epic_title` and a beads PM backend. Defaults to false (ephemeral in-memory plan only)."
                },
                "epic_title": {
                    "type": "string",
                    "description": "Epic title. Required when `persist_as_epic` is true. Ignored otherwise."
                },
                "epic_body": {
                    "type": "string",
                    "description": "Epic description / rationale. Optional when `persist_as_epic` is true. Ignored otherwise."
                }
            },
            "required": ["tasks"]
        }),
```

- [ ] **Step 2: Verify the tool-catalog snapshot test still passes**

Run: `CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo test -p spur-mcp --test tool_catalog`
Expected: PASS — tool name `submit_plan` unchanged; EXPECTED list unaffected.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/src/tools.rs
git commit -m "feat(spur-mcp): add persist_as_epic fields to submit_plan schema

Adds three optional fields: persist_as_epic (bool),
epic_title (string), epic_body (string). Handler support
lands in follow-up commits. Tool name and catalog
snapshot unchanged; INV-1 respected."
```

---

## Task 2: Schema shape tests

Verify the new fields are advertised. No negative-input tests here — those live in Task 8 (integration) because they need the full handler.

**Files:**
- Create: `crates/spur-mcp/tests/submit_plan_schema.rs`

- [ ] **Step 1: Write the schema tests**

Create `crates/spur-mcp/tests/submit_plan_schema.rs`:

```rust
//! submit_plan schema shape tests.
//!
//! Guards that the new persist_as_epic fields are advertised with the
//! right types and descriptions. Negative-input behavior is tested in
//! tests/submit_plan_persist.rs.

use spur_mcp::tools_list;

fn submit_plan_def() -> serde_json::Value {
    tools_list()
        .into_iter()
        .find(|t| t.name == "submit_plan")
        .expect("submit_plan must be in tool catalog")
        .input_schema
}

#[test]
fn schema_advertises_persist_as_epic() {
    let schema = submit_plan_def();
    let prop = schema
        .get("properties")
        .and_then(|p| p.get("persist_as_epic"))
        .expect("persist_as_epic must be advertised");
    assert_eq!(
        prop.get("type").and_then(|v| v.as_str()),
        Some("boolean"),
        "persist_as_epic must be boolean"
    );
}

#[test]
fn schema_advertises_epic_title_as_string() {
    let schema = submit_plan_def();
    let prop = schema
        .get("properties")
        .and_then(|p| p.get("epic_title"))
        .expect("epic_title must be advertised");
    assert_eq!(
        prop.get("type").and_then(|v| v.as_str()),
        Some("string"),
        "epic_title must be string"
    );
}

#[test]
fn schema_advertises_epic_body_as_string() {
    let schema = submit_plan_def();
    let prop = schema
        .get("properties")
        .and_then(|p| p.get("epic_body"))
        .expect("epic_body must be advertised");
    assert_eq!(
        prop.get("type").and_then(|v| v.as_str()),
        Some("string"),
        "epic_body must be string"
    );
}

#[test]
fn persist_fields_are_not_required() {
    let schema = submit_plan_def();
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(
        !required.contains(&"persist_as_epic"),
        "persist_as_epic must remain optional"
    );
    assert!(
        !required.contains(&"epic_title"),
        "epic_title is only required when persist_as_epic is true (enforced in handler)"
    );
}
```

- [ ] **Step 2: Run tests**

Run: `CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo test -p spur-mcp --test submit_plan_schema`
Expected: PASS — all four cases green.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/tests/submit_plan_schema.rs
git commit -m "test(spur-mcp): schema shape tests for submit_plan persist fields"
```

---

## Task 3: Handler backend-gate for persist_as_epic

Add the `persist_as_epic` extraction + backend-availability check to `handle_submit_plan`. Epic creation is deferred to Task 5; this task only wires the gate and returns a typed error when persist is requested without a beads backend.

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:1271-1337` (`handle_submit_plan`)

- [ ] **Step 1: Write the failing test**

Append to `crates/spur-mcp/tests/submit_plan_schema.rs` (same file as Task 2):

```rust
#[test]
fn persist_as_epic_without_title_is_documented_as_handler_error() {
    // Schema-level test only: epic_title is optional at schema level.
    // Handler-level rejection lives in submit_plan_persist.rs once the
    // handler branch is implemented (Task 6).
    let schema = submit_plan_def();
    // Documented via description text containing "Required when".
    let desc = schema
        .get("properties")
        .and_then(|p| p.get("epic_title"))
        .and_then(|p| p.get("description"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        desc.to_lowercase().contains("required when"),
        "epic_title description must document its conditional-required semantics; got: {desc}",
    );
}
```

- [ ] **Step 2: Run the test to verify it passes (description already written in Task 1)**

Run: `CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo test -p spur-mcp --test submit_plan_schema persist_as_epic_without_title`
Expected: PASS — Task 1's `epic_title` description includes "Required when `persist_as_epic` is true."

- [ ] **Step 3: Extract persist fields in the handler**

In `crates/spur-mcp/src/server.rs::handle_submit_plan`, locate the body (starts at line 1271 with `let tasks_val = match args.get("tasks")...`). Immediately after the `if let Err(e) = crate::plan::validate_plan(&tasks) { ... }` block (around line 1290), INSERT:

```rust
        // ─── Persist-as-epic extraction (T2.1) ─────────────────────────
        let persist_as_epic = args
            .get("persist_as_epic")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let epic_title = args
            .get("epic_title")
            .and_then(|v| v.as_str())
            .map(String::from);
        let epic_body = args
            .get("epic_body")
            .and_then(|v| v.as_str())
            .map(String::from);

        if persist_as_epic {
            if epic_title.as_deref().map(str::trim).unwrap_or("").is_empty() {
                return JsonRpcResponse::invalid_params(
                    id,
                    "submit_plan: epic_title is required when persist_as_epic is true",
                );
            }
            let pm_source = self.pm_service.as_deref().map(|p| p.source_str());
            if pm_source != Some("beads") {
                return JsonRpcResponse::error(
                    id,
                    -32000,
                    format!(
                        "submit_plan: persist_as_epic requires a beads PM backend (configured backend: {})",
                        pm_source.unwrap_or("none"),
                    ),
                );
            }
        }
```

Note: `PmService::source_str()` is already `pub` (verified in spur-pm/src/service.rs:155). `self.pm_service` is `Option<Arc<PmService>>` (server.rs field).

- [ ] **Step 4: Run existing tests to verify no regression**

Run: `CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo test -p spur-mcp`
Expected: PASS — no existing test exercises the new branch; extraction compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/tests/submit_plan_schema.rs
git commit -m "feat(spur-mcp): submit_plan persist_as_epic extraction + backend gate

Reads persist_as_epic / epic_title / epic_body from args. Rejects
empty or missing epic_title when persist_as_epic is true. Rejects
non-beads backends with JSON-RPC -32000. Epic creation itself is
a no-op in this commit; wired in a follow-up."
```

---

## Task 4: Add `epic_id` field to `PlanState`

Lets subsequent tasks store the beads epic_id alongside the in-memory plan state so it can be echoed in the response and queried via `get_plan_status` later.

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs` (PlanState struct)

- [ ] **Step 1: Extend `PlanState`**

At `crates/spur-mcp/src/plan.rs`, locate the `PlanState` struct (around line 92):

```rust
#[derive(Debug)]
pub struct PlanState {
    pub plan_id: String,
    pub tasks: Vec<PlanTaskEntry>,
    pub brain_session_id: SessionId,
}
```

Replace with:

```rust
#[derive(Debug)]
pub struct PlanState {
    pub plan_id: String,
    pub tasks: Vec<PlanTaskEntry>,
    pub brain_session_id: SessionId,
    /// beads epic ID when the plan was submitted with `persist_as_epic=true`.
    /// None for ephemeral plans. Used by review_task's auto-close path to
    /// resolve the child beads issue from the task's `spur.plan_id` label.
    pub epic_id: Option<String>,
}
```

- [ ] **Step 2: Update the construction site in `handle_submit_plan`**

In `crates/spur-mcp/src/server.rs::handle_submit_plan`, find the `PlanState` construction (around line 1306):

```rust
        let state = crate::plan::PlanState {
            plan_id: plan_id.clone(),
            tasks: entries,
            brain_session_id: self.brain_session_id.clone(),
        };
```

Replace with:

```rust
        let state = crate::plan::PlanState {
            plan_id: plan_id.clone(),
            tasks: entries,
            brain_session_id: self.brain_session_id.clone(),
            epic_id: None, // populated by Task 7 when persist_as_epic=true
        };
```

- [ ] **Step 3: Compile + run tests**

Run: `CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo test -p spur-mcp`
Expected: PASS — any other `PlanState { ... }` construction sites must also be updated. Use `cargo check -p spur-mcp 2>&1 | grep "missing field"` to locate them; fix each with `epic_id: None`.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/plan.rs crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): add PlanState.epic_id field

Stores the beads epic ID for plans submitted with
persist_as_epic=true. None for ephemeral plans. Populated
in a follow-up commit; this commit just threads the field
through existing construction sites."
```

---

## Task 5: Epic-build helper `build_epic_subgraph`

Pure-async helper that takes the validated `PlanTask` vec + epic fields + plan_id and performs the epic → children → deps creation against `PmService`. Returns `Result<EpicSubgraph, String>`. Isolated from the handler so it can be unit-tested against a fake PmService in Task 8.

**Files:**
- Modify: `crates/spur-mcp/src/server.rs` (add free function or impl method near `handle_submit_plan`)

- [ ] **Step 1: Add the helper function**

In `crates/spur-mcp/src/server.rs`, ABOVE `impl McpCallbackServer { ... }` (near the existing `parse_parallel_tasks` helper around line 199), INSERT:

```rust
/// Result of building a beads epic subgraph for a persisted plan.
#[derive(Debug, Clone)]
pub struct EpicSubgraph {
    pub epic_id: String,
    /// Maps each `PlanTask.task_id` → beads child issue ID.
    pub task_map: std::collections::HashMap<String, String>,
}

/// Compose a beads epic + child issues + dependency edges from a
/// validated plan. Labels each child with `spur.plan_id=<plan_id>` so
/// review_task can correlate approvals back to beads.
///
/// Creates issues in topological order (deps-first) so each child's
/// `depends_on` references beads IDs that already exist. Callers must
/// ensure the plan is validated (no cycles) before invoking.
///
/// On failure mid-creation: partial state lands in beads (epic +
/// whatever children succeeded). Caller should surface the error and
/// leave cleanup to the brain / human. Transactional rollback is out
/// of scope for v1 — beads CLI doesn't expose txn primitives.
pub async fn build_epic_subgraph(
    pm: &spur_pm::PmService,
    plan_id: &str,
    epic_title: &str,
    epic_body: Option<&str>,
    tasks: &[crate::plan::PlanTask],
) -> Result<EpicSubgraph, String> {
    // 1. Create the epic itself.
    let epic_create = spur_pm::types::IssueCreate {
        title: epic_title.to_string(),
        description: epic_body.map(String::from),
        issue_type: Some("epic".to_string()),
        labels: vec![format!("spur.plan_id={}", plan_id)],
        ..Default::default()
    };
    let epic_id = pm
        .create_issue(epic_create)
        .await
        .map_err(|e| format!("failed to create beads epic: {e}"))?;

    // 2. Topological order so each child can reference already-created deps.
    let order = topological_order(tasks).map_err(|e| {
        format!("plan dependency order (should have been caught by validate_plan): {e}")
    })?;

    let mut task_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for idx in order {
        let task = &tasks[idx];
        let depends_on_beads: Vec<String> = task
            .depends_on
            .iter()
            .map(|dep_key| {
                task_map.get(dep_key).cloned().ok_or_else(|| {
                    format!(
                        "task '{}' depends on '{}' which was not yet created (topological order bug)",
                        task.task_id, dep_key,
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut labels = vec![
            format!("spur.plan_id={}", plan_id),
            format!("spur.plan_task_id={}", task.task_id),
            format!("spur.agent={}", task.agent),
        ];
        if let Some(existing_issue_id) = &task.issue_id {
            labels.push(format!("spur.source_issue={}", existing_issue_id));
        }

        let child_create = spur_pm::types::IssueCreate {
            title: format!("{}: {}", task.task_id, truncate_for_title(&task.task)),
            description: Some(task.task.clone()),
            issue_type: Some("task".to_string()),
            labels,
            parent: Some(epic_id.clone()),
            depends_on: depends_on_beads,
            ..Default::default()
        };

        let child_id = pm
            .create_issue(child_create)
            .await
            .map_err(|e| format!("failed to create child issue for task '{}': {e}", task.task_id))?;

        task_map.insert(task.task_id.clone(), child_id);
    }

    Ok(EpicSubgraph { epic_id, task_map })
}

/// Truncate a task description to a reasonable issue-title length.
/// Beads has no hard limit but overly long titles are unwieldy in UIs.
fn truncate_for_title(s: &str) -> String {
    const MAX_TITLE_LEN: usize = 80;
    let first_line = s.lines().next().unwrap_or("").trim();
    if first_line.chars().count() <= MAX_TITLE_LEN {
        first_line.to_string()
    } else {
        let truncated: String = first_line.chars().take(MAX_TITLE_LEN - 3).collect();
        format!("{truncated}...")
    }
}

/// Return task indices in a valid topological order. Callers must have
/// already validated that the plan is acyclic via `plan::validate_plan`.
fn topological_order(tasks: &[crate::plan::PlanTask]) -> Result<Vec<usize>, String> {
    use std::collections::HashMap;
    let key_to_idx: HashMap<&str, usize> = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.task_id.as_str(), i))
        .collect();

    let mut in_degree: Vec<usize> = tasks.iter().map(|t| t.depends_on.len()).collect();
    let mut ready: std::collections::VecDeque<usize> = in_degree
        .iter()
        .enumerate()
        .filter_map(|(i, &d)| if d == 0 { Some(i) } else { None })
        .collect();

    let mut out = Vec::with_capacity(tasks.len());
    while let Some(i) = ready.pop_front() {
        out.push(i);
        for (j, t) in tasks.iter().enumerate() {
            if t.depends_on.iter().any(|dep| {
                key_to_idx.get(dep.as_str()).copied() == Some(i)
            }) {
                in_degree[j] -= 1;
                if in_degree[j] == 0 {
                    ready.push_back(j);
                }
            }
        }
    }

    if out.len() != tasks.len() {
        return Err(format!(
            "topological order incomplete: {} of {} tasks reachable (cycle?)",
            out.len(),
            tasks.len()
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod topo_tests {
    use super::topological_order;
    use crate::plan::PlanTask;

    fn t(id: &str, deps: &[&str]) -> PlanTask {
        PlanTask {
            task_id: id.to_string(),
            agent: "x".to_string(),
            task: "body".to_string(),
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            issue_id: None,
            context_files: Vec::new(),
        }
    }

    #[test]
    fn linear_chain_is_ordered() {
        let tasks = vec![t("a", &[]), t("b", &["a"]), t("c", &["b"])];
        let order = topological_order(&tasks).unwrap();
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn diamond_respects_all_parents() {
        // a → b, a → c, b+c → d
        let tasks = vec![
            t("a", &[]),
            t("b", &["a"]),
            t("c", &["a"]),
            t("d", &["b", "c"]),
        ];
        let order = topological_order(&tasks).unwrap();
        let pos_a = order.iter().position(|&i| i == 0).unwrap();
        let pos_b = order.iter().position(|&i| i == 1).unwrap();
        let pos_c = order.iter().position(|&i| i == 2).unwrap();
        let pos_d = order.iter().position(|&i| i == 3).unwrap();
        assert!(pos_a < pos_b && pos_a < pos_c);
        assert!(pos_b < pos_d && pos_c < pos_d);
    }

    #[test]
    fn cycle_is_detected() {
        let tasks = vec![t("a", &["b"]), t("b", &["a"])];
        let err = topological_order(&tasks).unwrap_err();
        assert!(err.contains("incomplete") || err.contains("cycle"));
    }
}
```

- [ ] **Step 2: Run the topo tests**

Run: `CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo test -p spur-mcp topo_tests`
Expected: PASS — three cases green.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): add build_epic_subgraph helper + topological_order

Pure helper that composes PmService::create_issue calls to build an
epic + children + deps subgraph. Labels each child with
spur.plan_id=<plan_id>, spur.plan_task_id=<task_id>, and
spur.agent=<agent>. Uses topological order so each child's
depends_on references already-created beads IDs.

Partial state on failure is accepted for v1 — beads CLI has no
transaction primitive. Caller surfaces the error."
```

---

## Task 6: Wire `build_epic_subgraph` into `handle_submit_plan`

Connect Task 3's gate to Task 5's helper. Execute epic creation BEFORE spawning the plan executor so any beads error is surfaced synchronously to the brain.

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:handle_submit_plan`

- [ ] **Step 1: Insert the epic-build call**

In `handle_submit_plan`, AFTER the Task 3 gate block (ending with the `return JsonRpcResponse::error(id, -32000, ...)` for non-beads backends) and BEFORE the `let plan_id = uuid::Uuid::new_v4().to_string();` line, INSERT:

```rust
        let plan_id = uuid::Uuid::new_v4().to_string();

        // Build the beads epic subgraph before spawning the executor so
        // any creation error is surfaced synchronously.
        let epic_subgraph: Option<EpicSubgraph> = if persist_as_epic {
            let pm = self.pm_service.as_deref().expect("gate ensures pm is beads");
            let title = epic_title.as_deref().expect("gate ensures non-empty title");
            match build_epic_subgraph(pm, &plan_id, title, epic_body.as_deref(), &tasks).await {
                Ok(sg) => {
                    info!(
                        plan_id = %plan_id,
                        epic_id = %sg.epic_id,
                        children = sg.task_map.len(),
                        "submit_plan: beads epic subgraph created"
                    );
                    Some(sg)
                }
                Err(e) => {
                    error!(plan_id = %plan_id, "build_epic_subgraph failed: {e}");
                    return JsonRpcResponse::error(
                        id,
                        -32000,
                        format!("submit_plan: failed to persist plan as beads epic: {e}"),
                    );
                }
            }
        } else {
            None
        };
```

You need to REMOVE the prior `let plan_id = uuid::Uuid::new_v4().to_string();` statement (it's now inside this block's preamble).

- [ ] **Step 2: Verify compile**

Run: `CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo check -p spur-mcp`
Expected: compiles — `epic_subgraph` is defined but unused until Task 7 wires it into the response. The `#[allow(unused_variables)]` pattern is not needed because the bind goes into the PlanState in the next step.

If warnings appear, that's fine — Task 7 wires up the consumption.

- [ ] **Step 3: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): call build_epic_subgraph before spawning plan executor

Synchronously creates the beads epic + children + deps when
persist_as_epic is true. Errors surface as JSON-RPC -32000 so
the brain learns immediately. Plan execution proceeds
unchanged; epic_id consumption lands in the next commit."
```

---

## Task 7: Populate `PlanState.epic_id` + extend response

Store the created epic_id on PlanState and echo `epic_id` + `task_map` in the success response body.

**Files:**
- Modify: `crates/spur-mcp/src/server.rs:handle_submit_plan`

- [ ] **Step 1: Populate PlanState.epic_id**

Find the `PlanState` construction block:

```rust
        let state = crate::plan::PlanState {
            plan_id: plan_id.clone(),
            tasks: entries,
            brain_session_id: self.brain_session_id.clone(),
            epic_id: None, // populated by Task 7 when persist_as_epic=true
        };
```

Replace `epic_id: None,` with:

```rust
            epic_id: epic_subgraph.as_ref().map(|sg| sg.epic_id.clone()),
```

- [ ] **Step 2: Extend the response body**

Find the final `JsonRpcResponse::success(id, json!({ ... }))` at the tail of the handler:

```rust
        JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": format!(
                        "Plan submitted: {task_count} tasks. plan_id: {plan_id}\n\
                         Poll with get_plan_status to monitor progress."
                    )
                }]
            }),
        )
    }
```

Replace the entire call with:

```rust
        let response_text = if let Some(sg) = &epic_subgraph {
            let task_map_json = serde_json::to_string(&sg.task_map)
                .unwrap_or_else(|_| "{}".to_string());
            format!(
                "Plan submitted: {task_count} tasks.\n\
                 plan_id: {plan_id}\n\
                 epic_id: {epic_id} (beads)\n\
                 task_map: {task_map_json}\n\
                 Poll with get_plan_status to monitor progress.",
                epic_id = sg.epic_id,
            )
        } else {
            format!(
                "Plan submitted: {task_count} tasks. plan_id: {plan_id}\n\
                 Poll with get_plan_status to monitor progress."
            )
        };

        JsonRpcResponse::success(
            id,
            json!({
                "content": [{
                    "type": "text",
                    "text": response_text
                }]
            }),
        )
    }
```

- [ ] **Step 3: Run full crate tests**

Run: `CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo test -p spur-mcp`
Expected: PASS — existing tests unaffected; new persist branch is exercised by Task 8.

- [ ] **Step 4: Commit**

```bash
git add crates/spur-mcp/src/server.rs
git commit -m "feat(spur-mcp): populate PlanState.epic_id + return epic_id in response

PlanState now carries the beads epic ID so review_task's
auto-close path can resolve child issues by plan_id.
Response body echoes epic_id + task_map for the brain to
reference beads issues directly."
```

---

## Task 8: Integration test against a fake PmService

Exercises the full `handle_submit_plan` persist branch with a test-double PmService. Verifies: epic gets created; children created in topo order; labels applied; response carries epic_id + task_map.

Because PmService is a concrete struct (not a trait) in today's codebase, we can't directly inject a mock. Instead, we test `build_epic_subgraph` + `topological_order` at the pure-function level using a **fake trait-object** introduced locally in the test file.

**Files:**
- Create: `crates/spur-mcp/tests/submit_plan_persist.rs`

- [ ] **Step 1: Identify what we can test without a live beads binary**

The `build_epic_subgraph` function calls `pm.create_issue(IssueCreate) -> Result<String>`. We cannot stub PmService without refactoring it to a trait. Two options:

a) **Refactor PmService to `dyn IssueTracker`** — too invasive for this plan.
b) **Test topological_order + build_epic_subgraph input shape only** — via unit tests that assert on the `IssueCreate` values that WOULD be passed to PmService if called. Achievable by extracting a helper `plan_epic_issue_creates(plan_id, epic_title, epic_body, tasks) -> (IssueCreate_for_epic, Vec<IssueCreate_for_children_in_order>)` — a pure function — and making `build_epic_subgraph` call it then execute the creates.

Option (b) is the clean split. We refactor now.

- [ ] **Step 2: Extract `plan_epic_issue_creates` pure helper**

In `crates/spur-mcp/src/server.rs`, REPLACE the body of `build_epic_subgraph` with:

```rust
pub async fn build_epic_subgraph(
    pm: &spur_pm::PmService,
    plan_id: &str,
    epic_title: &str,
    epic_body: Option<&str>,
    tasks: &[crate::plan::PlanTask],
) -> Result<EpicSubgraph, String> {
    let (epic_create, child_specs) =
        plan_epic_issue_creates(plan_id, epic_title, epic_body, tasks)?;

    let epic_id = pm
        .create_issue(epic_create)
        .await
        .map_err(|e| format!("failed to create beads epic: {e}"))?;

    let mut task_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for (task_id, mut child_create) in child_specs {
        // Rewrite `depends_on` from task_id keys → created beads IDs.
        child_create.depends_on = child_create
            .depends_on
            .iter()
            .map(|dep_key| {
                task_map.get(dep_key).cloned().ok_or_else(|| {
                    format!(
                        "task '{task_id}' depends on '{dep_key}' which was not yet created",
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        child_create.parent = Some(epic_id.clone());

        let child_id = pm
            .create_issue(child_create)
            .await
            .map_err(|e| format!("failed to create child for task '{task_id}': {e}"))?;
        task_map.insert(task_id, child_id);
    }

    Ok(EpicSubgraph { epic_id, task_map })
}

/// Pure helper: compute the IssueCreate values that build_epic_subgraph
/// would dispatch to PmService. Returns the epic's IssueCreate plus a
/// Vec of (task_id, IssueCreate) for each child in topological order.
/// Child IssueCreate.depends_on carries task_id keys, NOT beads IDs —
/// the caller rewrites them as children are created.
pub fn plan_epic_issue_creates(
    plan_id: &str,
    epic_title: &str,
    epic_body: Option<&str>,
    tasks: &[crate::plan::PlanTask],
) -> Result<(spur_pm::types::IssueCreate, Vec<(String, spur_pm::types::IssueCreate)>), String> {
    let epic_create = spur_pm::types::IssueCreate {
        title: epic_title.to_string(),
        description: epic_body.map(String::from),
        issue_type: Some("epic".to_string()),
        labels: vec![format!("spur.plan_id={}", plan_id)],
        ..Default::default()
    };

    let order = topological_order(tasks)?;
    let mut child_specs = Vec::with_capacity(tasks.len());
    for idx in order {
        let task = &tasks[idx];
        let mut labels = vec![
            format!("spur.plan_id={}", plan_id),
            format!("spur.plan_task_id={}", task.task_id),
            format!("spur.agent={}", task.agent),
        ];
        if let Some(existing) = &task.issue_id {
            labels.push(format!("spur.source_issue={}", existing));
        }
        let child_create = spur_pm::types::IssueCreate {
            title: format!("{}: {}", task.task_id, truncate_for_title(&task.task)),
            description: Some(task.task.clone()),
            issue_type: Some("task".to_string()),
            labels,
            // depends_on carries task_id keys; rewritten by build_epic_subgraph.
            depends_on: task.depends_on.clone(),
            // parent set by build_epic_subgraph once epic_id is known.
            parent: None,
            ..Default::default()
        };
        child_specs.push((task.task_id.clone(), child_create));
    }
    Ok((epic_create, child_specs))
}
```

Export `plan_epic_issue_creates` + `topological_order` + `EpicSubgraph` from `crates/spur-mcp/src/lib.rs`. Current pub use line (from T1's Task 10):

```rust
pub use server::{build_worker_info, parse_parallel_tasks, validate_parallel_args, McpCallbackServer, WorkerInfo};
```

Extend to:

```rust
pub use server::{
    build_epic_subgraph, build_worker_info, parse_parallel_tasks, plan_epic_issue_creates,
    validate_parallel_args, EpicSubgraph, McpCallbackServer, WorkerInfo,
};
```

Also expose `topological_order` if the test needs it; otherwise keep it private.

- [ ] **Step 3: Create the integration test**

Create `crates/spur-mcp/tests/submit_plan_persist.rs`:

```rust
//! submit_plan persist_as_epic — unit + integration tests over pure helpers.
//!
//! Because PmService is a concrete struct (not a trait) today, live-beads
//! integration is covered at the CLI level elsewhere. Here we test the
//! pure helper that decides WHAT IssueCreate values the handler would
//! dispatch given a plan + epic fields.

use serde_json::json;
use spur_mcp::{plan_epic_issue_creates, tools_list};
use std::collections::HashMap;

// ─── Pure helper tests ───────────────────────────────────────────────

/// Build a minimal PlanTask list for tests. task_id "a" has no deps;
/// "b" depends on "a"; optional "c" depends on both.
fn sample_tasks(with_c: bool) -> Vec<spur_mcp::tools::PlanTask> {
    // NOTE: PlanTask is re-exported indirectly; adjust the path if the
    // actual module export differs (lib.rs re-export). If compile fails,
    // use `spur_mcp::plan::PlanTask` via `spur-mcp::plan` module re-export.
    vec![
        spur_mcp::tools::PlanTask {
            task_id: "a".into(),
            agent: "claude-code-acp".into(),
            task: "Do A.".into(),
            depends_on: Vec::new(),
            issue_id: None,
            context_files: Vec::new(),
        },
        spur_mcp::tools::PlanTask {
            task_id: "b".into(),
            agent: "claude-code-acp".into(),
            task: "Do B.".into(),
            depends_on: vec!["a".into()],
            issue_id: Some("bd-42".into()),
            context_files: Vec::new(),
        },
    ]
    .into_iter()
    .chain(if with_c {
        vec![spur_mcp::tools::PlanTask {
            task_id: "c".into(),
            agent: "codex".into(),
            task: "Do C.".into(),
            depends_on: vec!["a".into(), "b".into()],
            issue_id: None,
            context_files: Vec::new(),
        }]
    } else {
        vec![]
    })
    .collect()
}

#[test]
fn epic_create_carries_plan_id_label_and_epic_type() {
    let tasks = sample_tasks(false);
    let (epic, _children) =
        plan_epic_issue_creates("plan-xyz", "Refactor foo", Some("Body"), &tasks).expect("ok");
    assert_eq!(epic.title, "Refactor foo");
    assert_eq!(epic.issue_type.as_deref(), Some("epic"));
    assert_eq!(epic.description.as_deref(), Some("Body"));
    assert!(
        epic.labels.iter().any(|l| l == "spur.plan_id=plan-xyz"),
        "epic must carry spur.plan_id label; got {:?}",
        epic.labels,
    );
}

#[test]
fn children_are_in_topological_order() {
    let tasks = sample_tasks(true);
    let (_epic, children) =
        plan_epic_issue_creates("plan-xyz", "Refactor foo", None, &tasks).expect("ok");
    let order: Vec<&str> = children.iter().map(|(k, _)| k.as_str()).collect();
    // "a" must precede both "b" and "c"; "b" must precede "c".
    let pos_a = order.iter().position(|&k| k == "a").unwrap();
    let pos_b = order.iter().position(|&k| k == "b").unwrap();
    let pos_c = order.iter().position(|&k| k == "c").unwrap();
    assert!(pos_a < pos_b);
    assert!(pos_a < pos_c);
    assert!(pos_b < pos_c);
}

#[test]
fn children_carry_spur_plan_id_plan_task_id_and_agent_labels() {
    let tasks = sample_tasks(false);
    let (_epic, children) =
        plan_epic_issue_creates("plan-xyz", "Title", None, &tasks).expect("ok");
    let (_, child_b) = children
        .iter()
        .find(|(k, _)| k == "b")
        .expect("child b present");
    let labels: &Vec<String> = &child_b.labels;
    assert!(labels.iter().any(|l| l == "spur.plan_id=plan-xyz"));
    assert!(labels.iter().any(|l| l == "spur.plan_task_id=b"));
    assert!(labels.iter().any(|l| l == "spur.agent=claude-code-acp"));
    assert!(
        labels.iter().any(|l| l == "spur.source_issue=bd-42"),
        "child b sourced from bd-42 must carry spur.source_issue label"
    );
}

#[test]
fn children_depends_on_carries_task_id_keys_not_beads_ids() {
    // plan_epic_issue_creates returns task_id keys; build_epic_subgraph
    // rewrites them as beads IDs are created. This separation is
    // intentional — keeps the helper pure.
    let tasks = sample_tasks(false);
    let (_epic, children) =
        plan_epic_issue_creates("plan-xyz", "T", None, &tasks).expect("ok");
    let (_, child_b) = children.iter().find(|(k, _)| k == "b").unwrap();
    assert_eq!(child_b.depends_on, vec!["a".to_string()]);
}

#[test]
fn children_parent_field_is_unset_before_epic_creation() {
    // plan_epic_issue_creates cannot know the epic's beads ID; parent
    // is populated by build_epic_subgraph.
    let tasks = sample_tasks(false);
    let (_epic, children) =
        plan_epic_issue_creates("plan-xyz", "T", None, &tasks).expect("ok");
    for (_, c) in &children {
        assert!(c.parent.is_none(), "parent must be None at this stage");
    }
}

#[test]
fn cycle_produces_error() {
    let tasks = vec![
        spur_mcp::tools::PlanTask {
            task_id: "a".into(),
            agent: "x".into(),
            task: "A".into(),
            depends_on: vec!["b".into()],
            issue_id: None,
            context_files: Vec::new(),
        },
        spur_mcp::tools::PlanTask {
            task_id: "b".into(),
            agent: "x".into(),
            task: "B".into(),
            depends_on: vec!["a".into()],
            issue_id: None,
            context_files: Vec::new(),
        },
    ];
    let err = plan_epic_issue_creates("p", "t", None, &tasks).unwrap_err();
    assert!(
        err.contains("incomplete") || err.contains("cycle"),
        "cycle error text should mention incomplete or cycle; got: {err}"
    );
}

// ─── Schema round-trip sanity ────────────────────────────────────────

#[test]
fn submit_plan_schema_still_advertises_tasks_as_required() {
    // Guard: persist-field additions must not accidentally drop `tasks`
    // from required.
    let schema = tools_list()
        .into_iter()
        .find(|t| t.name == "submit_plan")
        .unwrap()
        .input_schema;
    let required: Vec<&str> = schema
        .get("required")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(required.contains(&"tasks"));
}

// ─── Silence unused-import warning if json! isn't exercised above ────

#[allow(dead_code)]
fn _unused() -> serde_json::Value {
    json!({})
}

#[allow(dead_code)]
fn _unused_hashmap() -> HashMap<String, String> {
    HashMap::new()
}
```

**Note about `spur_mcp::tools::PlanTask`:** `PlanTask` lives in `crates/spur-mcp/src/plan.rs` (per Task 4's grep). Adjust import to `use spur_mcp::plan::PlanTask;` and update `crates/spur-mcp/src/lib.rs` to re-export `pub use plan::PlanTask;` if it isn't already. Verify via `cargo check -p spur-mcp --tests` before declaring done.

- [ ] **Step 4: Run the integration test**

Run: `CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo test -p spur-mcp --test submit_plan_persist`
Expected: PASS — seven cases green.

Run full crate tests:
`CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo test -p spur-mcp`
Expected: PASS — no regressions.

- [ ] **Step 5: Commit**

```bash
git add crates/spur-mcp/src/server.rs crates/spur-mcp/src/lib.rs crates/spur-mcp/tests/submit_plan_persist.rs
git commit -m "test(spur-mcp): integration tests for submit_plan persist_as_epic

Extracts plan_epic_issue_creates as a pure helper so we can assert
on the exact IssueCreate shape the handler would dispatch. Covers:
epic labels/type, child topological order, label population,
depends_on key preservation, parent-unset-until-epic-created
invariant, and cycle detection."
```

---

## Task 9: Brain orchestration guide

Write the prose that teaches brains (and human operators) when to use submit_plan vs direct delegate_*, how to phrase the DAG, and how to run the review loop. Lives in `docs/spur/` so it's committed with the repo and visible to anyone building brain-adapter configs.

**Files:**
- Create: `docs/spur/brain-orchestration-guide.md`

- [ ] **Step 1: Draft the guide**

Create `docs/spur/brain-orchestration-guide.md` with the following content:

```markdown
# Brain Orchestration Guide

Practical guidance for agents acting as the *brain* in a Spur session — whether that's `claude-code-acp`, `gpt-5-acp`, `kiro`, or another MCP-speaking orchestrator. Describes the three delegation patterns and when to use each.

## TL;DR decision tree

1. **One task, no dependencies.** → `delegate_to_worker(agent, task, context_files?, delegation_plan)`
2. **Several independent tasks you want to run in parallel.** → `delegate_parallel(tasks[], delegation_plan?)`
3. **Multi-task DAG with dependencies (2+ tasks + edges).** → `submit_plan(tasks[], delegation_plan)`
4. **DAG that must survive the session OR be visible to other humans/agents in beads.** → `submit_plan(..., persist_as_epic=true, epic_title=...)`

If unsure between #2 and #3: use #3. The orchestrator auto-runs independent tasks in parallel once their deps are satisfied.

## The three patterns in detail

### Pattern A — Single task

```json
{
  "name": "delegate_to_worker",
  "arguments": {
    "agent": "claude-code-acp",
    "task": "CONTEXT: ...\nGOAL: ...\nCONSTRAINTS: ...\nEXPECTED_OUTPUT: ...",
    "context_files": ["src/a.rs", "src/b.rs"],
    "delegation_plan": {
      "chosen": "claude-code-acp",
      "rationale": "Multi-file refactor; generalist fits."
    }
  }
}
```

Worker runs, returns a diff. You inspect. No review loop (single shot). Use for small edits, one-off bug fixes, or prototyping.

### Pattern B — Parallel independent tasks

```json
{
  "name": "delegate_parallel",
  "arguments": {
    "tasks": [
      {
        "agent": "claude-code-acp",
        "task": "...",
        "context_files": ["src/a.rs"],
        "issue_id": "bd-101",
        "delegation_plan": {"chosen": "claude-code-acp", "rationale": "..."}
      },
      {
        "agent": "codex",
        "task": "...",
        "context_files": ["src/b.rs"],
        "issue_id": "bd-102",
        "delegation_plan": {"chosen": "codex", "rationale": "..."}
      }
    ]
  }
}
```

Tasks must be **truly independent** — no shared state, no file overlaps. Each runs in its own worktree so parallel edits don't corrupt the repo, but merge conflicts at PR time are still your problem.

### Pattern C — Plan with dependencies

```json
{
  "name": "submit_plan",
  "arguments": {
    "tasks": [
      {"task_id": "setup", "agent": "claude-code-acp", "task": "...", "depends_on": []},
      {"task_id": "impl_a", "agent": "claude-code-acp", "task": "...", "depends_on": ["setup"]},
      {"task_id": "impl_b", "agent": "codex", "task": "...", "depends_on": ["setup"]},
      {"task_id": "wire",   "agent": "claude-code-acp", "task": "...", "depends_on": ["impl_a", "impl_b"]}
    ],
    "delegation_plan": {"chosen": "mixed", "rationale": "Diamond DAG; parallel middle."}
  }
}
```

The orchestrator:
1. Dispatches `setup` immediately.
2. Dispatches `impl_a` + `impl_b` in parallel once `setup` approves.
3. Dispatches `wire` once both `impl_a` and `impl_b` approve.

Response includes `plan_id`. Poll via `get_plan_status(plan_id)`.

### Pattern D — Persisted plan (Pattern C + beads epic)

```json
{
  "name": "submit_plan",
  "arguments": {
    "tasks": [ ... same as Pattern C ... ],
    "delegation_plan": { ... },
    "persist_as_epic": true,
    "epic_title": "Refactor auth flow — Q2",
    "epic_body": "Full design in docs/superpowers/plans/2026-04-18-auth-refactor.md"
  }
}
```

Creates a beads epic with child issues for each task, linked by `depends_on` edges and labeled `spur.plan_id=<plan_id>`, `spur.plan_task_id=<task_id>`, `spur.agent=<agent>`. Response adds `epic_id` + `task_map` so you can cross-reference.

**Use this when:**
- The plan spans multiple sessions (restart safety).
- Humans outside Spur should see progress (beads UI / CLI / dashboard).
- You want `review_task(approve)` to auto-close the corresponding beads child.

**Do NOT use this when:**
- The plan is ephemeral (session-local work).
- You're prototyping and don't want extra state to clean up.

**Requirement:** the session's PmService must be a beads backend. Non-beads backends reject `persist_as_epic=true` with `-32000`.

## The review loop

After `submit_plan` (or `execute_epic`) returns, the orchestrator takes over dispatch. Your job becomes **reviewer**:

```
loop {
  status = get_plan_status(plan_id)
  if status.has_task_in("awaiting_review"):
    for task in status.tasks where status == "awaiting_review":
      diff = get_task_diff(plan_id, task.task_id)
      decision = your_review(diff)
      review_task(plan_id, task.task_id, decision, feedback?)
  if status.all_tasks_approved():
    break
  sleep 2s  (or use status.ready_to_merge as your exit signal)
}
```

Decisions:
- `approve` → task marked done, dependents auto-dispatched. If `persist_as_epic=true`, the beads child closes too.
- `reject` → task terminal. Pending/ready dependents cascade-fail. Use for work that's fundamentally misconceived.
- `request_changes` → re-dispatch the worker with `feedback` verbatim. Max 3 attempts per task. Use for fixable issues.

When all tasks are approved and `ready_to_merge` is true:

```json
{"name": "create_pr", "arguments": {"title": "...", "body": "...", "branch": "main"}}
```

## Picking the right agent per task

Call `list_available_workers` if you're unsure. Summary heuristics (see `defaults.toml` for authoritative descriptors):

| Task shape | Preferred agent |
|---|---|
| Multi-file refactor; writing new modules from spec | `claude-code-acp` |
| Single-file mechanical edits; language idiom translation | `codex` |
| Spec-driven workflows (Kiro `/spec-*` commands) | `kiro` |
| Multi-modal (images, diagrams) | `gemini` |

Always set `delegation_plan.chosen` + `delegation_plan.rationale` on each task. The reviewer uses these to detect agent-routing mismatches.

## Error recovery playbook

| Symptom | Action |
|---|---|
| `get_plan_status` shows a task in `failed` state after 3 attempts | Inspect the last attempt's diff via `get_task_diff(..., attempt=N)`. If salvageable, `reject` + re-plan; else `reject` + tell the user. |
| `review_task(request_changes)` returns `remaining_attempts=0` | You've exhausted retries. `approve` (if partial work is usable) or `reject`. |
| Dependent tasks cascade-failed after you `reject`ed a predecessor | Expected behavior. Re-plan the affected subgraph as a new `submit_plan` with `depends_on_external` references to the approved predecessors. |
| `submit_plan` returns `-32000 "persist_as_epic requires a beads PM backend"` | You tried `persist_as_epic=true` on a GitHub/Linear/Plane backend. Either drop the flag (ephemeral plan) or add beads to the repo. |
| `cancel_delegation` returns `-32601 "Internal operation not yet wired"` | Cancellation is not implemented yet. The worker keeps running; tell the user. |

## Interaction with writing-plans-shaped markdown

Spur's writing-plans skill (main Claude Code session) produces `docs/superpowers/plans/YYYY-MM-DD-<slug>.md`. When a brain sees such a file (e.g., in the user's request), it can:

1. Read the file.
2. Compose a `submit_plan` payload with one task per `## Task N` section.
3. Set `epic_body` = link to the plan file (so the beads epic has a back-reference).
4. Set each child task's `task` field to the CONTEXT/GOAL/CONSTRAINTS/EXPECTED_OUTPUT extracted from that task's steps.

This pattern keeps the plan.md as the human-readable artifact and beads as the execution tracking layer. Drift between the two is acceptable as long as the plan.md path + commit SHA are recorded on the epic.

## Escape hatches

- Need one off-plan quick task mid-execution? → `delegate_to_worker` directly. Orchestrator plan state is unaffected.
- Need to abort an in-flight plan? → reject all pending tasks (cascades). Then start a fresh `submit_plan`.
- Need to hand a plan off to a different brain? → plan state is keyed on `plan_id`; both brains polling `get_plan_status` with that ID see the same state (if they share the MCP server).
```

- [ ] **Step 2: Verify the file renders**

Run: `head -30 docs/spur/brain-orchestration-guide.md` — eyeball that markdown headers render and no merge-conflict artifacts.

- [ ] **Step 3: Commit**

```bash
git add docs/spur/brain-orchestration-guide.md
git commit -m "docs(spur): brain orchestration guide

Describes the three delegation patterns (delegate_to_worker,
delegate_parallel, submit_plan), the new persist_as_epic option,
the review loop rhythm, agent selection heuristics, and error
recovery playbook. Primary audience: brain agents and human
operators authoring brain system prompts."
```

---

## Task 10: Design spec record

Commit a design-record spec alongside the plan so the architecture decision lives in git.

**Files:**
- Create: `docs/superpowers/specs/2026-04-18-submit-plan-epic-persistence.md`

- [ ] **Step 1: Create the spec**

```markdown
# submit_plan Epic Persistence — Design Spec

**Status:** In implementation (see plans/2026-04-18-submit-plan-epic-persistence.md)
**Author:** Brain-as-orchestrator MCTS rounds (iceberg-refined)
**Supersedes:** "ingest_plan standalone tool" proposal (rejected)

## Problem

After T1 restored truthfulness to the MCP surface, the remaining friction for brain-as-orchestrator was: how does a brain run a multi-task DAG with optional durability, without re-authoring the plan against beads via N+M primitive calls?

Pre-spec options:
1. Add a standalone `ingest_plan` MCP tool. Rejected: tool-catalog accretion against T1's contraction ethos.
2. Add a `.claude/skills/...` file. Rejected: brains aren't Claude Code main-session users; skills are invisible at the MCP layer.
3. Extend `submit_plan` with optional persistence. **Accepted.**

## Decision

Add three optional fields to `submit_plan`:
- `persist_as_epic: bool` (default `false`).
- `epic_title: String` (required when persist is true).
- `epic_body: String?` (optional free-form description).

When `persist_as_epic=true`:
- Handler verifies beads backend is configured; rejects other backends with `-32000`.
- Handler composes `PmService::create_issue` calls to build an epic + children + deps subgraph.
- Each child gets labels: `spur.plan_id=<plan_id>`, `spur.plan_task_id=<task_id>`, `spur.agent=<agent>` (+ `spur.source_issue=<id>` when `PlanTask.issue_id` is set).
- `PlanState.epic_id` records the beads epic ID; response echoes `epic_id` + `task_map`.

Atomicity is **best-effort**: beads CLI has no transaction primitive. Partial state on mid-creation failure is accepted for v1. The handler surfaces the error; cleanup is the brain/human's responsibility.

## Invariants

- **INV-9** (`submit_plan` non-destructive to catalog): adding persistence fields does not change the tool name or remove any existing field. Snapshot test in `tool_catalog.rs` stays unchanged.
- **INV-10** (persist requires beads): when `persist_as_epic=true` the handler rejects non-beads backends before any issue is created. No partial persistence on wrong backend.
- **INV-11** (topological ordering): children are created in a valid topological order so each child's `depends_on` references only already-created beads IDs.
- **INV-12** (label correlation): every child created via persist carries `spur.plan_id=<plan_id>`. review_task auto-close relies on this label.

## Non-invariants (explicit deferrals)

- **Atomic rollback on mid-creation failure.** Beads CLI composition only. Real transactionality would require a sqlite-backed adapter refactor — tracked separately.
- **Plan.md ↔ beads drift detection.** `epic_body` can link to the plan file; no runtime check that the file hasn't been edited.
- **Auto-sync back from beads to `submit_plan` state.** If a human edits a child issue's body in beads mid-execution, the orchestrator does not re-read. Deferred.
- **Standalone `ingest_plan` (author without dispatch).** Accepted non-goal per the iceberg-refined proposal. Revisit if human-gated authoring workflows become a demand.

## Testing

- Schema shape tests in `tests/submit_plan_schema.rs`.
- Pure-helper tests in `tests/submit_plan_persist.rs` exercising `plan_epic_issue_creates` against the expected `IssueCreate` shapes. Topological-order + cycle-detection guards.
- Existing `tool_catalog.rs` snapshot stays green (name unchanged).

## Out-of-scope for v1

- Brain-prompt surface auto-injection. The guide lives in `docs/spur/brain-orchestration-guide.md` as a human-readable reference. Automatic inclusion in each brain's system prompt is deferred pending the T1.6-T0 investigation of brain-prompt locations.
- TUI visualization of persisted epics. Beads CLI is the canonical UI for now.
- Multi-backend `ingest_plan` (Linear, Plane). Beads-only per existing `add_dependency` pattern.
```

- [ ] **Step 2: Commit**

```bash
git add docs/superpowers/specs/2026-04-18-submit-plan-epic-persistence.md
git commit -m "docs(spur): design spec for submit_plan persist_as_epic

Records the iceberg-refined decision to extend an existing tool
rather than adding a standalone ingest_plan. Lists invariants
INV-9 through INV-12 and explicit deferrals (atomic rollback,
drift detection, plan.md auto-sync, ingest_plan revisit criteria)."
```

---

## Task 11: Full workspace verification

**Files:** none (verification only)

- [ ] **Step 1: Run clippy on touched crates**

```bash
CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo clippy -p spur-mcp --all-targets -- -D warnings
```
Expected: PASS. (spur-pm was not modified; skip its clippy.)

Note: pre-existing `spur-core` clippy errors (documented in T1 PR) are out of scope.

- [ ] **Step 2: Run full workspace test suite**

```bash
CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo test --workspace
```
Expected: PASS across all suites. Regression check against the 39+ suites that passed post-T1.

- [ ] **Step 3: Snapshot EXPECTED catalog still matches**

```bash
CARGO_TARGET_DIR=/Volumes/Projects/spur/target cargo test -p spur-mcp --test tool_catalog
```
Expected: PASS — tool names unchanged.

- [ ] **Step 4: Review commit chain**

```bash
git log --oneline main..HEAD
```
Expected: ~9 commits (Tasks 1, 2, 3, 4, 5, 6, 7, 8, 9, 10). Each message follows conventional-commits shape.

- [ ] **Step 5: No commit for this task**

Verification only. If any step fails, backfill under the appropriate task number.

---

## Self-review

Checked against the source proposal (iceberg-refined):

**Scope coverage:**
- Extend `submit_plan` with `persist_as_epic` + `epic_title` + `epic_body` → Tasks 1, 3, 5, 6, 7. ✓
- Atomic (best-effort) beads epic+children+deps creation → Task 5. ✓
- Labels for correlation (`spur.plan_id`, `spur.plan_task_id`, `spur.agent`) → Task 5. ✓
- Response carries `epic_id` + `task_map` → Task 7. ✓
- Backend-gate (beads-only) → Task 3. ✓
- Brain guidance doc → Task 9. ✓
- Design spec commit → Task 10. ✓

**Invariants:**
- INV-9 (catalog stable) → Task 1 + catalog-test verification in Tasks 1, 11.
- INV-10 (persist requires beads) → Task 3.
- INV-11 (topological order) → Task 5 (`topological_order` + topo tests).
- INV-12 (label correlation) → Task 5 label emission + Task 8 assertion.

**Placeholder scan:**
- No TBD / TODO / "handle edge cases" / "similar to Task N" patterns.
- One deferred pattern: Task 8 notes `PlanTask` import path may need adjustment between `spur_mcp::tools::PlanTask` and `spur_mcp::plan::PlanTask`. The note is explicit and gives the engineer a `cargo check` command to verify. Acceptable.

**Type consistency:**
- `EpicSubgraph { epic_id: String, task_map: HashMap<String, String> }` — defined in Task 5, consumed in Tasks 6, 7, 8.
- `plan_epic_issue_creates(&str, &str, Option<&str>, &[PlanTask]) -> Result<(IssueCreate, Vec<(String, IssueCreate)>), String>` — defined in Task 8 (Step 2 refactor), consumed by Task 5's build_epic_subgraph + Task 8's tests.
- `topological_order(&[PlanTask]) -> Result<Vec<usize>, String>` — defined in Task 5, used by both `plan_epic_issue_creates` and `topo_tests`.
- `build_epic_subgraph(&PmService, &str, &str, Option<&str>, &[PlanTask]) -> Result<EpicSubgraph, String>` — defined in Task 5, refactored in Task 8, consumed in Task 6.

No fixes required.

---

## Out-of-scope / parked items

- **True atomic rollback** on mid-creation failure (requires beads sqlite-txn primitive or adapter refactor).
- **`ingest_plan` standalone** (deferred per iceberg proposal; revisit if human-gated authoring workflows emerge).
- **Main-session `.claude/skills/spur-orchestrator/`** skill (deferred per iceberg proposal).
- **Brain-prompt auto-injection** of the guide (requires T1.6-T0 investigation).
- **Multi-backend persist** (Linear, Plane, GitHub). Beads-only for v1.
- **Plan.md ↔ beads drift detection** tooling.
- **TUI visualization** of persisted epics.

---

## Dependency on other in-flight work

This plan is **independent** of T1.5 / T1.6 / T1.7. It touches different code paths (submit_plan vs delegate_*). Can land in parallel with any of them.

If T1.5 (typed args) lands first: this plan's Task 3/6/7 code can be refactored to use `SubmitPlanArgs` typed struct in a follow-up. Not a blocker.

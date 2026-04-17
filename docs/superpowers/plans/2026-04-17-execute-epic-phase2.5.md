# Execute Epic Implementation Plan (Phase 2.5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship `execute_epic(epic_id)` MCP tool — a strictly-scoped hydration tool that derives `PlanTask[]` from a beads epic subgraph and hands off to the existing `submit_plan` engine unchanged.

**Architecture:** Read beads subgraph via `PmService` → validate (agent routing, deps, nesting) → build `Vec<PlanTask>` → invoke the same internal submit-plan code path used today. Idempotency via a `PlanRegistry` keyed by `epic_id`. Zero changes to the scheduling kernel, orchestrator, events, or TUI.

**Tech Stack:** Rust / tokio / serde / MCP / beads (spur-pm). All changes live in `crates/spur-mcp/`.

**Design spec:** `docs/superpowers/specs/2026-04-17-execute-epic-phase2.5-design.md`

---

## File Structure

- **Modify**: `crates/spur-mcp/src/plan.rs` — add `PlanRegistry`, `execute_epic()`, helpers, unit tests
- **Modify**: `crates/spur-mcp/src/tools.rs` — add `execute_epic_def()`, register in `tools_list()`
- **Modify**: `crates/spur-mcp/src/server.rs` — add `handle_execute_epic()`, wire registry, route in `handle_call`
- **Modify**: `crates/spur-mcp/src/lib.rs` — re-export `PlanRegistry` if needed for tests
- **Create**: `crates/spur-mcp/tests/execute_epic_integration.rs` (optional — see Task 3)

---

## Task 1: Core hydration logic + PlanRegistry + unit tests

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs` (add new items below `build_plan_status` / `review_task`)

**Context:** This is the heart of the feature. It reads a beads epic subgraph via `PmService`, validates every invariant the spec calls out, and produces a `Vec<PlanTask>` ready for the existing `submit_plan` engine. The engine's public entry is `PlanState::new(...)` + `run_plan(...)` — Task 2 wires the hand-off. For this task, produce a pure function that returns `Result<Vec<PlanTask>, String>` plus the validation scaffolding and the `PlanRegistry`.

Known details from the codebase:
- `spur_pm::PmService::get_issue(id)` → `anyhow::Result<Issue>`
- `spur_pm::PmService::list_issues(filter)` → `anyhow::Result<Vec<IssueSummary>>`
- `Issue { id, title, body, status, labels, parent: Option<String>, blocked_by: Vec<String>, issue_type: Option<String>, ... }`
- `IssueFilter { labels, status, issue_type, ... }` — NO `parent` field today. To collect children of an epic, either (a) add `parent` to `IssueFilter` (out of scope) or (b) `list_issues` broadly then filter client-side by `parent == Some(epic_id)`. Use (b).

- [ ] **Step 1.1: Add `PlanRegistry` type at top of `plan.rs` under `PlanState`**

```rust
/// Tracks the active plan for each epic so re-calling `execute_epic(epic_id)`
/// while a plan is running returns the existing plan_id. Lazy cleanup — a
/// registry entry is cleared on the next `execute_epic` call for the same
/// epic if its plan has reached a terminal overall status.
#[derive(Debug, Default)]
pub struct PlanRegistry {
    /// epic_id → plan_id (for the currently-active plan, if any).
    pub by_epic: std::collections::HashMap<String, String>,
}
```

- [ ] **Step 1.2: Add label-parsing helpers near `display_name`**

```rust
/// Find the value of a label of the form `prefix=value` on a label list.
/// Returns the first match, trimmed. Returns `None` if no label has the
/// given prefix.
fn label_value<'a>(labels: &'a [String], prefix: &str) -> Option<&'a str> {
    labels
        .iter()
        .filter_map(|l| l.strip_prefix(prefix).map(str::trim))
        .next()
}

/// Strip `spur.*` machine-routing labels from a user-visible label list.
/// Used when surfacing labels back to the worker's context.
fn strip_spur_labels(labels: &[String]) -> Vec<String> {
    labels
        .iter()
        .filter(|l| !l.starts_with("spur."))
        .cloned()
        .collect()
}
```

- [ ] **Step 1.3: Write the failing test for agent resolution order**

```rust
#[test]
fn label_value_finds_prefix() {
    let labels = vec![
        "spur.agent=codex".to_string(),
        "priority=high".to_string(),
        "spur.task_text=custom".to_string(),
    ];
    assert_eq!(super::label_value(&labels, "spur.agent="), Some("codex"));
    assert_eq!(super::label_value(&labels, "spur.task_text="), Some("custom"));
    assert_eq!(super::label_value(&labels, "missing="), None);
}

#[test]
fn strip_spur_labels_drops_machine_prefix() {
    let labels = vec![
        "spur.agent=codex".to_string(),
        "area:auth".to_string(),
        "spur.task_text=x".to_string(),
        "bug".to_string(),
    ];
    let kept = super::strip_spur_labels(&labels);
    assert_eq!(kept, vec!["area:auth".to_string(), "bug".to_string()]);
}
```

- [ ] **Step 1.4: Run the tests to verify they pass**

Run: `cargo test -p spur-mcp label_value_finds_prefix strip_spur_labels_drops_machine_prefix`
Expected: 2 passed.

- [ ] **Step 1.5: Define the `DerivePlan` output struct and write failing happy-path test**

Add above the pure derivation function:

```rust
/// Result of deriving a `Vec<PlanTask>` from a beads epic subgraph.
/// Warnings are non-fatal (external-dep-already-done, missing-agent-used-default).
#[derive(Debug)]
pub struct DerivedEpicPlan {
    pub plan_tasks: Vec<PlanTask>,
    pub warnings: Vec<String>,
    /// Summary counts for the response payload.
    pub agent_counts: std::collections::HashMap<String, usize>,
    pub edge_count: usize,
}
```

Test (add under `mod tests`):

```rust
#[tokio::test]
async fn derive_epic_plan_resolves_agents_and_deps() {
    // Stub PmService with an in-memory epic + 2 children.
    let pm = test_pm_with_epic_and_children();
    let derived =
        super::derive_epic_plan(&pm, "bd-100", Some("codex")).await.unwrap();
    assert_eq!(derived.plan_tasks.len(), 2);
    // child A has no deps; child B depends on A
    let a = derived.plan_tasks.iter().find(|t| t.task_id == "bd-101").unwrap();
    let b = derived.plan_tasks.iter().find(|t| t.task_id == "bd-102").unwrap();
    assert!(a.depends_on.is_empty());
    assert_eq!(b.depends_on, vec!["bd-101".to_string()]);
    assert_eq!(a.agent, "codex");
    assert_eq!(b.agent, "codex");
    assert_eq!(derived.edge_count, 1);
}
```

NOTE: The test helper `test_pm_with_epic_and_children` will need a stub/mock `PmService`. Look at existing spur-mcp tests (`mod tests` in `plan.rs`) to see whether a test backend already exists. If not, consider introducing a minimal `PmBackend` trait mock OR use `spur_pm::PmService` with a test-only constructor that wraps an in-memory adapter. If that doesn't exist yet, refactor `PmService` with a `mockall` or a hand-rolled trait IS out of scope — in that case, SKIP the derivation integration test at the PmService level and instead test `derive_epic_plan` with a callback-style input (pass pre-fetched `Issue` + `Vec<Issue>` for children). Use this fallback shape:

```rust
pub async fn derive_epic_plan_from_issues(
    epic: &spur_pm::Issue,
    children: &[spur_pm::Issue],
    default_agent: Option<&str>,
    known_agents: &[&str],
) -> Result<DerivedEpicPlan, String>
```

This keeps `derive_epic_plan` (the PmService-fetching version) a thin wrapper. Tests drive the pure `_from_issues` variant with hand-built `Issue` structs.

- [ ] **Step 1.6: Implement `derive_epic_plan_from_issues` — minimal logic to pass Step 1.5 test**

Algorithm:
1. Validate `epic.issue_type.as_deref() == Some("epic")` — else error `"issue '{id}' is not an epic (type={t})"`.
2. Validate `children.is_empty()` → error `"epic '{id}' has no children; create at least one child task first"`.
3. Build `subgraph_ids: HashSet<&str>` from `children.iter().map(|c| c.id.as_str())`.
4. For each child, reject if `child.issue_type.as_deref() == Some("epic")` → error with child id.
5. Resolve agent per child:
   - `label_value(&child.labels, "spur.agent=")`
   - else `label_value(&epic.labels, "spur.agent=")` (inherited)
   - else `default_agent`
   - else error with known-agent list.
   - Validate resolved agent is in `known_agents` → else error.
   - If fallback to `default_agent` was used, push warning.
6. Resolve task text: `label_value(&child.labels, "spur.task_text=")` → else `child.body.clone()`.
7. Map `child.blocked_by`:
   - For each `b` in `child.blocked_by`: if `b` in `subgraph_ids`, keep as internal dep. Else: this is an external dep — check passed-in state? Complication: we don't have the external issue here. ALTER spec-vs-plan: require the caller (`derive_epic_plan`, the PmService wrapper) to resolve external deps via `pm.get_issue(b)`. In `_from_issues`, accept a pre-fetched `external_deps: &HashMap<String, String>` mapping external id → status. Error if any external in blocked_by is not in the map OR its status != "done". Push warning for each done external dep included.
8. Build `PlanTask { task_id: child.id, agent, task: text, depends_on, issue_id: Some(child.id), context_files: vec![] }`.
9. Call `validate_plan(&plan_tasks)` — reuse existing cycle detection.
10. Return `DerivedEpicPlan { plan_tasks, warnings, agent_counts, edge_count }`.

Revised signature:

```rust
pub fn derive_epic_plan_from_issues(
    epic: &spur_pm::Issue,
    children: &[spur_pm::Issue],
    external_dep_statuses: &std::collections::HashMap<String, String>,
    default_agent: Option<&str>,
    known_agents: &[&str],
) -> Result<DerivedEpicPlan, String>
```

Make it synchronous — no async needed in the pure derivation.

- [ ] **Step 1.7: Run test to verify it passes**

Update `derive_epic_plan_resolves_agents_and_deps` to use `_from_issues` instead of async version. Build `Issue` structs inline.

Run: `cargo test -p spur-mcp derive_epic_plan_resolves_agents_and_deps`
Expected: PASS.

- [ ] **Step 1.8: Add failing tests for each error branch**

Add these tests (each drives `derive_epic_plan_from_issues` with crafted input that hits one error branch):

- `derive_rejects_non_epic_issue` — `epic.issue_type = Some("task")` → error contains `"not an epic"`.
- `derive_rejects_nested_epic_child` — one child with `issue_type = Some("epic")` → error contains `"nested epic child"`.
- `derive_rejects_empty_children` — empty children slice → error contains `"no children"`.
- `derive_rejects_unsatisfied_external_dep` — child `blocked_by = ["bd-ext"]`, `external_dep_statuses["bd-ext"] = "open"` → error contains `"not done"`.
- `derive_allows_done_external_dep` — same but status `"done"` → `warnings` contains `"bd-ext"`, that id NOT in `depends_on`.
- `derive_inherits_agent_from_epic_label` — child has no `spur.agent=`, epic has `spur.agent=claude-code` → resolved to `"claude-code"`.
- `derive_falls_back_to_default_agent` — neither label set, `default_agent = Some("codex")` → resolved to `codex`, warning pushed.
- `derive_rejects_missing_agent` — neither label, no default → error contains `"no agent for task"` and lists known agents.
- `derive_rejects_unknown_agent` — label `spur.agent=ghost`, `known_agents = ["codex"]` → error contains `"not configured"` and `"codex"`.
- `derive_uses_spur_task_text_override` — label `spur.task_text=OVERRIDE\ntext` → `PlanTask.task == "OVERRIDE\ntext"`.
- `derive_cycle_rejected` — two children, `bd-101 blocked_by [bd-102]`, `bd-102 blocked_by [bd-101]` → error contains `"Cycle"`.

One test per error, each ~15–25 lines. Show full source for each; no placeholders.

- [ ] **Step 1.9: Run all new tests to verify they fail with the expected messages**

Run: `cargo test -p spur-mcp derive_`
Expected: all fail with "not yet implemented" or missing-branch messages.

- [ ] **Step 1.10: Implement each error branch incrementally until all `derive_*` tests pass**

No placeholders. For each branch, read the test expectation and code to satisfy it. Run tests after each branch:

Run: `cargo test -p spur-mcp derive_`
Expected: all PASS.

- [ ] **Step 1.11: Add `PlanRegistry` tests**

```rust
#[test]
fn plan_registry_empty_has_no_entries() {
    let r = super::PlanRegistry::default();
    assert!(r.by_epic.is_empty());
}

#[test]
fn plan_registry_insert_and_lookup() {
    let mut r = super::PlanRegistry::default();
    r.by_epic.insert("bd-100".into(), "plan-abc".into());
    assert_eq!(r.by_epic.get("bd-100"), Some(&"plan-abc".to_string()));
}
```

Run: `cargo test -p spur-mcp plan_registry`
Expected: 2 PASS.

- [ ] **Step 1.12: Commit Task 1**

```bash
git add crates/spur-mcp/src/plan.rs
git commit -m "$(cat <<'EOF'
feat(plan): execute_epic core — derive_epic_plan_from_issues + PlanRegistry

Pure derivation: reads an epic Issue + children Issues, validates epic
type / nesting / empty children / agent routing / external deps / cycles,
and produces a Vec<PlanTask> ready for the existing submit_plan engine.

Agent resolution precedence: child `spur.agent=<name>` label → epic label
→ default_agent → error. Task text: `spur.task_text=<x>` label override →
issue.body. External blocked_by refs must be done or error early.

PlanRegistry keyed by epic_id for idempotency (active plan returns existing
plan_id). Lazy cleanup on next execute_epic call.

Zero wire-up yet; Task 2 adds the MCP surface + PmService fetch wrapper.

Part of docs/superpowers/specs/2026-04-17-execute-epic-phase2.5-design.md.
EOF
)"
```

---

## Task 2: MCP surface — tool definition, server handler, PmService wrapper

**Files:**
- Modify: `crates/spur-mcp/src/tools.rs` — add `execute_epic_def()` and register it in `tools_list()`
- Modify: `crates/spur-mcp/src/server.rs` — add `handle_execute_epic`, add `plan_registry` field to `McpCallbackServer`, route `execute_epic` in the JSON-RPC dispatcher
- Modify: `crates/spur-mcp/src/plan.rs` — add `derive_epic_plan` async wrapper that fetches via `PmService`
- Modify: `crates/spur-mcp/src/lib.rs` — re-export `PlanRegistry` if tests need it externally (probably not; skip unless compile fails)

**Context:** Task 1 produced a pure function. Task 2 wires it to the MCP surface. The handler fetches the epic + children via `PmService`, calls `derive_epic_plan_from_issues`, builds a `PlanState`, inserts into `active_plans`, updates `PlanRegistry`, and kicks off `run_plan` — mirroring how `handle_submit_plan` does it. Reuse the existing scheduling kernel; do NOT duplicate dispatch logic.

Read `crates/spur-mcp/src/server.rs:1205` (`handle_submit_plan`) end-to-end before starting — your handler is a near-clone with the subgraph-fetch prefix.

- [ ] **Step 2.1: Add `execute_epic_def()` to `tools.rs`**

Place directly below `submit_plan_def()`:

```rust
fn execute_epic_def() -> ToolDefinition {
    ToolDefinition {
        name: "execute_epic".into(),
        description: "Execute a beads epic: hydrate a plan from the epic's \
            children subgraph and dispatch in dependency order. Agent routing \
            comes from the `spur.agent=<name>` label on each child issue \
            (inherited from the epic if unset, or from default_agent). Task \
            text comes from issue.body (override via `spur.task_text=<text>` \
            label). Rejects nested sub-epic children. External blocked_by \
            references must already be `done`. After dispatch, the plan runs \
            under the normal review engine — use get_plan_status / \
            get_task_diff / review_task. Re-calling while a plan is active \
            for the same epic returns the existing plan_id (idempotent). \
            After terminal state, a new call starts a fresh plan.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "epic_id": {
                    "type": "string",
                    "description": "The beads ID of an issue with type=epic"
                },
                "default_agent": {
                    "type": "string",
                    "description": "Fallback agent when a child has no `spur.agent=<name>` label and the epic has no inherited label"
                }
            },
            "required": ["epic_id"]
        }),
    }
}
```

Register in `tools_list()`:

```rust
pub fn tools_list() -> Vec<ToolDefinition> {
    vec![
        // ...existing entries...
        submit_plan_def(),
        execute_epic_def(),  // ← add after submit_plan_def
        get_plan_status_def(),
        // ...
    ]
}
```

Run: `cargo build -p spur-mcp`
Expected: clean build.

- [ ] **Step 2.2: Add `derive_epic_plan` async wrapper in `plan.rs`**

```rust
/// Async wrapper: fetches the epic + children + external dep statuses via
/// PmService, then calls the pure `derive_epic_plan_from_issues`. Returns
/// the same `DerivedEpicPlan`.
pub async fn derive_epic_plan(
    pm: &spur_pm::PmService,
    epic_id: &str,
    default_agent: Option<&str>,
    known_agents: &[&str],
) -> Result<DerivedEpicPlan, String> {
    let epic = pm
        .get_issue(epic_id)
        .await
        .map_err(|e| format!("epic '{epic_id}' not found in beads: {e}"))?;

    // Gather candidate children. IssueFilter has no `parent` field, so list
    // broadly and filter client-side. Scope the list to status in {open,
    // in_progress, done, ...} — use empty filter (list all) if no narrower
    // option exists.
    let all = pm
        .list_issues(spur_pm::IssueFilter::default())
        .await
        .map_err(|e| format!("failed to list issues: {e}"))?;

    // Fetch full Issue for each child (list_issues returns IssueSummary which
    // may lack blocked_by). Re-fetch via get_issue.
    let mut children: Vec<spur_pm::Issue> = Vec::new();
    for s in all {
        // Skip issues without parent or with a different parent.
        // IssueSummary doesn't currently expose `parent`; fetch to check.
        let full = match pm.get_issue(&s.id).await {
            Ok(i) => i,
            Err(_) => continue,
        };
        if full.parent.as_deref() == Some(epic_id) {
            children.push(full);
        }
    }

    // Collect external dep statuses.
    let mut external_dep_statuses = std::collections::HashMap::new();
    let subgraph: std::collections::HashSet<&str> =
        children.iter().map(|c| c.id.as_str()).collect();
    for c in &children {
        for b in &c.blocked_by {
            if !subgraph.contains(b.as_str())
                && !external_dep_statuses.contains_key(b)
            {
                match pm.get_issue(b).await {
                    Ok(ext) => {
                        external_dep_statuses.insert(b.clone(), ext.status);
                    }
                    Err(_) => {
                        external_dep_statuses
                            .insert(b.clone(), "unknown".to_string());
                    }
                }
            }
        }
    }

    derive_epic_plan_from_issues(
        &epic,
        &children,
        &external_dep_statuses,
        default_agent,
        known_agents,
    )
}
```

NOTE: If iterating all issues is too slow for large projects, a later optimization can add `parent` to `IssueFilter`. For Phase 2.5, O(N) is acceptable.

- [ ] **Step 2.3: Add `plan_registry` field to `McpCallbackServer`**

In `server.rs` around line 139–160 where the struct is defined:

```rust
pub struct McpCallbackServer {
    // ... existing fields ...
    active_plans: Arc<Mutex<HashMap<String, Arc<Mutex<crate::plan::PlanState>>>>>,

    /// Phase 2.5: tracks the active plan_id for each epic so that a second
    /// execute_epic call on a running epic returns the same plan_id.
    plan_registry: Arc<Mutex<crate::plan::PlanRegistry>>,
}
```

Initialize in the constructor:

```rust
plan_registry: Arc::new(Mutex::new(crate::plan::PlanRegistry::default())),
```

Run: `cargo build -p spur-mcp`
Expected: clean build.

- [ ] **Step 2.4: Implement `handle_execute_epic` in `server.rs`**

Place immediately below `handle_submit_plan` (search for `fn handle_submit_plan`). Structure:

```rust
async fn handle_execute_epic(&self, id: Value, args: Value) -> JsonRpcResponse {
    let epic_id = match args.get("epic_id").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => return json_rpc_error(id, -32602, "missing required field: epic_id"),
    };
    let default_agent = args
        .get("default_agent")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Idempotency: if an active plan exists for this epic, return it.
    {
        let registry = self.plan_registry.lock().await;
        if let Some(existing_plan_id) = registry.by_epic.get(&epic_id) {
            let plans = self.active_plans.lock().await;
            if let Some(plan_arc) = plans.get(existing_plan_id) {
                let plan_state = plan_arc.lock().await;
                let status = crate::plan::build_plan_status(
                    existing_plan_id,
                    &plan_state,
                );
                // If the plan is still active (not terminal), return it as-is.
                let is_terminal = matches!(
                    status.get("status").and_then(|v| v.as_str()),
                    Some("approved") | Some("failed")
                      | Some("has_failures") | Some("has_rejections")
                );
                if !is_terminal {
                    return json_rpc_ok(id, status);
                }
                // Terminal — fall through to create a new plan. Clear the
                // registry entry and the active_plans entry.
            }
        }
    }

    // Clear any stale terminal registry entry.
    {
        let mut registry = self.plan_registry.lock().await;
        registry.by_epic.remove(&epic_id);
    }

    // Fetch PmService.
    let pm = match &self.pm_service {
        Some(p) => p.clone(),
        None => {
            return json_rpc_error(
                id,
                -32000,
                "beads (PmService) is not configured — cannot execute epic",
            );
        }
    };

    // Known agents from config. Look up via existing helper — search for
    // `agent_configs` or `AgentConfigs` in server.rs to find the known-agents
    // vector. For this plan, assume `self.agent_configs.names()` returns
    // `Vec<String>`; adapt to the actual API name if different.
    let known: Vec<String> = self.agent_configs.names();
    let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();

    let derived = match crate::plan::derive_epic_plan(
        &pm,
        &epic_id,
        default_agent.as_deref(),
        &known_refs,
    )
    .await
    {
        Ok(d) => d,
        Err(e) => return json_rpc_error(id, -32000, &e),
    };

    // Build PlanState and hand off to the existing submit_plan path. Look at
    // handle_submit_plan for the exact construction — replicate it here.
    let plan_id = uuid::Uuid::new_v4().to_string();
    let plan_state = crate::plan::PlanState {
        plan_id: plan_id.clone(),
        tasks: derived
            .plan_tasks
            .iter()
            .map(|t| crate::plan::PlanTaskEntry {
                spec: t.clone(),
                status: crate::plan::PlanTaskStatus::Pending,
                result: None,
                worker_branch: None,
                attempt: 1,
                history: Vec::new(),
            })
            .collect(),
        brain_session_id: /* look up from session state — see handle_submit_plan */,
    };
    let plan_arc = Arc::new(Mutex::new(plan_state));

    // Insert into active_plans + registry BEFORE spawning so concurrent
    // execute_epic callers see the new entry.
    {
        let mut plans = self.active_plans.lock().await;
        plans.insert(plan_id.clone(), plan_arc.clone());
    }
    {
        let mut registry = self.plan_registry.lock().await;
        registry.by_epic.insert(epic_id.clone(), plan_id.clone());
    }

    // Kick off run_plan exactly as handle_submit_plan does (copy the spawn
    // block verbatim and adapt plan_arc / plan_id).

    // Build response: reuse build_plan_status + add `epic_id`, `derived` block.
    let mut resp = {
        let s = plan_arc.lock().await;
        crate::plan::build_plan_status(&plan_id, &s)
    };
    if let serde_json::Value::Object(ref mut m) = resp {
        m.insert("epic_id".into(), json!(epic_id));
        m.insert(
            "derived".into(),
            json!({
                "task_count": derived.plan_tasks.len(),
                "edge_count": derived.edge_count,
                "agents": derived.agent_counts,
                "warnings": derived.warnings,
            }),
        );
    }

    json_rpc_ok(id, resp)
}
```

IMPORTANT: Some details above (`self.agent_configs.names()`, the `brain_session_id` source, the exact `run_plan` spawn block) depend on existing patterns in `handle_submit_plan`. Read that function carefully and mirror its structure — do NOT invent new patterns. If `handle_submit_plan` uses helpers like `Self::spawn_plan_runner(...)`, reuse them.

- [ ] **Step 2.5: Route `execute_epic` in the JSON-RPC dispatcher**

Find where `submit_plan` is routed (search for `"submit_plan"` in `server.rs`). Add an arm:

```rust
"execute_epic" => self.handle_execute_epic(id, args).await,
```

Immediately below the `"submit_plan"` arm.

- [ ] **Step 2.6: Build and fix any compile errors**

Run: `cargo build -p spur-mcp`
Expected: clean build. If any imports are missing (e.g. `json_rpc_error`, `json_rpc_ok`, `Value`), add them.

- [ ] **Step 2.7: Run all existing tests to confirm no regression**

Run: `cargo test --workspace`
Expected: 494 tests pass (Phase 2 count) + any new tests from Task 1. Zero failures.

- [ ] **Step 2.8: Commit Task 2**

```bash
git add crates/spur-mcp/src/plan.rs crates/spur-mcp/src/tools.rs crates/spur-mcp/src/server.rs
git commit -m "$(cat <<'EOF'
feat(mcp): execute_epic tool wire-up — JSON-RPC handler + PmService fetch

Adds the MCP surface for execute_epic: tool definition in tools.rs, async
derive_epic_plan wrapper that fetches epic + children + external dep
statuses via PmService, and handle_execute_epic in server.rs that mirrors
handle_submit_plan's dispatch path after derivation.

PlanRegistry on McpCallbackServer tracks active_plan_id per epic for
idempotent re-calls. Terminal plans clear the entry lazily on next call.

Zero changes to the scheduling kernel, orchestrator, or event plumbing —
after derivation, the plan runs under the existing Phase 2 engine with its
review loop (get_task_diff, review_task, MAX_ATTEMPTS=3) intact.

Part of docs/superpowers/specs/2026-04-17-execute-epic-phase2.5-design.md.
EOF
)"
```

---

## Task 3: Idempotency + end-to-end smoke test (optional, recommended)

**Files:**
- Modify: `crates/spur-mcp/src/plan.rs` (tests at the bottom) — add idempotency unit test using the plain `PlanRegistry`
- Create: `crates/spur-mcp/tests/execute_epic_integration.rs` — end-to-end smoke test (only if a stubbable `PmService` is feasible; skip if not)

**Context:** This task hardens the behavior that Tasks 1–2 deliver. Idempotency and fresh-after-terminal are tested at the registry level (cheap) and at the handler level (expensive — only if PmService can be stubbed).

- [ ] **Step 3.1: Add idempotency unit tests in `plan.rs`**

```rust
#[test]
fn registry_tracks_active_plan_per_epic() {
    let mut r = super::PlanRegistry::default();
    r.by_epic.insert("bd-100".into(), "plan-1".into());
    r.by_epic.insert("bd-200".into(), "plan-2".into());
    assert_eq!(r.by_epic.get("bd-100"), Some(&"plan-1".into()));
    assert_eq!(r.by_epic.get("bd-200"), Some(&"plan-2".into()));
    assert_eq!(r.by_epic.get("bd-999"), None);
}

#[test]
fn registry_entry_replaced_on_reinsert() {
    let mut r = super::PlanRegistry::default();
    r.by_epic.insert("bd-100".into(), "plan-old".into());
    r.by_epic.insert("bd-100".into(), "plan-new".into());
    assert_eq!(r.by_epic.get("bd-100"), Some(&"plan-new".into()));
}
```

Run: `cargo test -p spur-mcp registry_`
Expected: 2 PASS.

- [ ] **Step 3.2: Assess feasibility of an integration test**

Look at existing tests in `crates/spur-mcp/tests/`. If there is already a pattern for stubbing `PmService` (a mock backend, a test-only `PmService::new_in_memory`, etc.), add an integration test `execute_epic_integration.rs` that:

1. Creates a stub PmService with one epic (type=epic) + two children (type=task; one blocked_by the other; both labeled `spur.agent=echo`).
2. Stands up `McpCallbackServer` with the stub.
3. Calls `execute_epic(epic_id="bd-100")` → asserts response has `plan_id`, `derived.task_count == 2`, `derived.edge_count == 1`.
4. Calls `execute_epic(epic_id="bd-100")` again while active → asserts same plan_id returned.

If no such stubbing infrastructure exists, SKIP this step. Document in the commit message that end-to-end coverage is deferred until PmService has a test double.

- [ ] **Step 3.3: Run full test suite**

Run: `cargo test --workspace 2>&1 | grep -E "^test result"`
Expected: all passes, total count = Phase 2 count (494) + Task 1 tests + Task 3 tests.

- [ ] **Step 3.4: Check for warnings**

Run: `cargo build 2>&1 | grep -E "warning"`
Expected: no output (zero warnings).

- [ ] **Step 3.5: Commit Task 3**

```bash
git add crates/spur-mcp/src/plan.rs crates/spur-mcp/tests/ 2>/dev/null
git commit -m "$(cat <<'EOF'
test(plan): registry + idempotency coverage for execute_epic

Unit tests lock in the PlanRegistry semantics: per-epic tracking, replacement
on reinsert, miss on unknown epic. Integration smoke test (if PmService can
be stubbed) walks the end-to-end flow: derive → dispatch → return plan_id →
repeat call returns same plan_id.

Part of docs/superpowers/specs/2026-04-17-execute-epic-phase2.5-design.md.
EOF
)"
```

---

## Success Criteria

After all three tasks:

- `cargo build` — zero errors, zero warnings.
- `cargo test --workspace` — all tests pass, count ≥ 494 + new tests (target: 510+).
- `execute_epic` is listed in `tools_list()` output and callable via MCP.
- Calling `execute_epic(epic_id)` twice while active returns the same `plan_id`.
- Nested sub-epic children / missing agents / unsatisfied external deps return actionable error messages.
- No change to `crates/spur-core/orchestrator.rs`, `crates/spur-acp/*`, `crates/spur-tui/*`, or `crates/spur-pm/*`.

## Open Questions (Document, Do Not Plan)

The design spec (`docs/superpowers/specs/2026-04-17-execute-epic-phase2.5-design.md`) lists non-goals explicitly — restart recovery, continuous projection, daemon mode, attempt carry-over, nested epics. Do NOT address them in this plan. If you hit blockers that seem to require any of them, STOP and escalate; do not expand scope.
